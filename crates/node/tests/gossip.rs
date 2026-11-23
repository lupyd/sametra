//! Multi-node behaviour over real WebSockets: binary protobuf gossip between
//! sametra nodes, and NIP-01 JSON for ordinary nostr clients.
mod common;
use common::*;
use futures_util::{SinkExt, StreamExt};
use sametra_node::event::*;
use sametra_node::pb;
use serde_json::{Value, json};
use std::time::Duration;
use sametra_node::transport::{BoxIo, Dialer, Message, WebSocketStream};

const T: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_flow_both_ways_over_one_link() {
    let (a, b) = (spawn(|_| {}).await, spawn(|_| {}).await);
    let before: pb::Ack = a.ok("post", &post("m", "history", "before the link")).await;
    b.connect(&a).await; // b dials a; a cannot dial b (think NAT)
    assert!(eventually(T, || b.has(&before.id)).await, "dialer syncs history on connect");
    let from_a: pb::Ack = a.ok("post", &post("m", "from a", "x")).await;
    assert!(eventually(T, || b.has(&from_a.id)).await, "a -> b via b's subscription");
    let from_b: pb::Ack = b.ok("post", &post("m", "from b", "x")).await;
    assert!(eventually(T, || a.has(&from_b.id)).await, "b -> a pushed over the outbound link");
    let ab: pb::PeersResponse = a.ok("peers", &pb::Empty {}).await;
    assert!(ab.peers.len() == 1 && !ab.peers[0].outbound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hops_and_no_loops() {
    let (a, b, c) = (spawn(|_| {}).await, spawn(|_| {}).await, spawn(|_| {}).await);
    b.connect(&a).await;
    c.connect(&b).await;
    c.connect(&a).await; // a triangle: every event has two paths
    let ea: pb::Ack = a.ok("post", &post("m", "from a", "x")).await;
    let ec: pb::Ack = c.ok("post", &post("m", "from c", "x")).await;
    let r: pb::Ack = b.ok("post", &reply(&ea.id, "b replies")).await;
    for n in [&a, &b, &c] {
        for id in [&ea.id, &ec.id, &r.id] {
            assert!(eventually(T, || n.has(id)).await);
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for n in [&a, &b, &c] {
        let t: pb::ThreadResponse = n.ok("thread", &id(&ea.id)).await;
        assert_eq!(t.root.unwrap().replies.len(), 1, "the reply arrives once and is threaded everywhere");
        assert_eq!(n.node.store.lock().unwrap().len(), 3, "no duplicates from the cycle");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deletes_ratings_and_profiles_propagate() {
    let (a, b) = (spawn(|_| {}).await, spawn(|_| {}).await);
    b.connect(&a).await;
    let p: pb::Ack = a.ok("post", &post("m", "oops", "wrong")).await;
    assert!(eventually(T, || b.has(&p.id)).await);
    a.ok::<_, pb::Ack>("delete", &id(&p.id)).await;
    assert!(eventually(T, || !b.has(&p.id)).await, "retraction reaches peers");

    let good: pb::Ack = a.ok("post", &post("m", "good", "work")).await;
    assert!(eventually(T, || b.has(&good.id)).await);
    b.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Bee".into(), ..Default::default() }).await;
    b.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 7, event: good.id.clone(), ..Default::default() }).await;
    let ka = a.node.keys.pubkey.hex();
    let rep_on = |n: &TestNode| n.node.reputation("m").user(&H32::from_hex(&ka).unwrap());
    assert!(eventually(T, || rep_on(&a) > 0.0).await, "rating reached the author's node");
    assert!((rep_on(&a) - rep_on(&b)).abs() < 1e-9, "both nodes compute the same reputation");
    let pr: pb::ProfileResponse = a.ok("profile", &pb::ProfileRequest { pubkey: b.node.keys.pubkey.hex() }).await;
    assert_eq!(pr.profile.unwrap().name, "Bee");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoped_nodes_only_keep_their_communities() {
    let a = spawn(|_| {}).await;
    let b = spawn(|c| {
        c.relay_all = false;
        c.join = vec!["math".into()];
    })
    .await;
    let old_bio: pb::Ack = a.ok("post", &post("bio", "old", "x")).await;
    let old_math: pb::Ack = a.ok("post", &post("math", "old", "x")).await;
    a.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "A".into(), ..Default::default() }).await;
    b.connect(&a).await;
    assert!(eventually(T, || b.has(&old_math.id)).await);
    let bio: pb::Ack = a.ok("post", &post("bio", "new", "x")).await;
    let math: pb::Ack = a.ok("post", &post("math", "new", "x")).await;
    assert!(eventually(T, || b.has(&math.id)).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b.has(&bio.id) && !b.has(&old_bio.id));
    assert!(b.node.store.lock().unwrap().replaceable(&a.node.keys.pubkey, K_PROFILE, "").is_some(), "global metadata still syncs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_spread_dialable_addresses() {
    // a announces the address it listens on
    let a = spawn(|c| c.public_addr = Some(format!("ws://{}", c.listen))).await;
    let a_ws = a.ws.clone();
    let (b, c) = (spawn(|_| {}).await, spawn(|_| {}).await);
    c.connect(&b).await;
    b.connect(&a).await; // a greets its new subscriber b with a heartbeat; b relays it to c
    let book = |n: &TestNode| {
        n.node.save_state();
        pb::decode::<pb::NodeState>(&std::fs::read(n.dir.0.join("state.pb")).unwrap()).unwrap().peers
    };
    assert!(eventually(T, || book(&c).contains(&a_ws)).await, "c learned a's address two hops away");
    assert!(eventually(T, || book(&b).contains(&a_ws)).await);
    assert!(!book(&a).contains(&a_ws), "never ourselves");
    // and presence: a's author is now 'seen' on c, keeping a's content alive there
    assert!(c.node.store.lock().unwrap().last_seen.contains_key(&a.node.keys.pubkey));
}

// ---- plain nostr clients (NIP-01 JSON) --------------------------------------

type Ws = WebSocketStream<BoxIo>;

async fn client(n: &TestNode) -> Ws {
    Dialer::new(&[]).unwrap().connect(&n.ws).await.unwrap()
}
async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}
async fn recv(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(T, ws.next()).await.expect("timed out waiting for relay").unwrap().unwrap();
        match m {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Binary(_) => panic!("a JSON client must never get binary frames"),
            _ => continue,
        }
    }
}
async fn quiet(ws: &mut Ws, d: Duration) -> bool {
    tokio::time::timeout(d, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return Some(t),
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    })
    .await
    .map(|x| x.is_none())
    .unwrap_or(true)
}
fn ev_json(e: &Event) -> Value {
    serde_json::to_value(e).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nostr_client_publish_and_ok() {
    let n = spawn(|_| {}).await;
    let mut ws = client(&n).await;
    let k = Keys::generate();
    let e = k.sign(K_POST, tags(&[&["c", "m"], &["title", "from a nostr app"]]), "hi".into());
    send(&mut ws, json!(["EVENT", ev_json(&e)])).await;
    assert_eq!(recv(&mut ws).await, json!(["OK", e.id.hex(), true, ""]));
    send(&mut ws, json!(["EVENT", ev_json(&e)])).await;
    assert_eq!(recv(&mut ws).await, json!(["OK", e.id.hex(), true, "duplicate:"]));
    let mut forged = ev_json(&k.sign(K_POST, vec![], "x".into()));
    forged["content"] = json!("changed");
    send(&mut ws, json!(["EVENT", forged])).await;
    let r = recv(&mut ws).await;
    assert_eq!(r[2], false);
    assert!(r[3].as_str().unwrap().starts_with("invalid:"));
    send(&mut ws, json!(["EVENT", ev_json(&k.sign(K_HEARTBEAT, vec![], String::new()))])).await;
    assert_eq!(recv(&mut ws).await[2], true, "ephemeral events are accepted and relayed");
    send(&mut ws, json!({"not": "an array"})).await;
    assert_eq!(recv(&mut ws).await[0], "NOTICE");
    send(&mut ws, json!(["REQ", "s"])).await;
    assert_eq!(recv(&mut ws).await[0], "NOTICE", "REQ needs a filter");
    // the post is visible through the API like any other
    let f: pb::Items = n.ok("feed", &pb::FeedRequest { community: "m".into(), ..Default::default() }).await;
    assert_eq!(f.items[0].title, "from a nostr app");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nostr_client_subscriptions() {
    let n = spawn(|_| {}).await;
    for i in 0..3 {
        n.ok::<_, pb::Ack>("post", &post("m", &format!("old{i}"), "x")).await;
    }
    n.ok::<_, pb::Ack>("post", &post("other", "elsewhere", "x")).await;
    let mut ws = client(&n).await;
    send(&mut ws, json!(["REQ", "feed", {"kinds": [11], "#c": ["m"], "limit": 2}])).await;
    let mut got = vec![];
    loop {
        let m = recv(&mut ws).await;
        if m[0] == "EOSE" {
            assert_eq!(m[1], "feed");
            break;
        }
        assert_eq!(m[0], "EVENT");
        assert_eq!(m[1], "feed");
        let e: Event = serde_json::from_value(m[2].clone()).unwrap();
        assert!(e.validate().is_ok());
        got.push(e);
    }
    assert_eq!(got.len(), 2, "limit honoured");
    assert!(got.iter().all(|e| e.community() == Some("m")));
    assert!(got[0].created_at <= got[1].created_at, "stored events are sent oldest first");

    // live: new matching events stream in, others do not
    let live: pb::Ack = n.ok("post", &post("m", "live", "x")).await;
    let m = recv(&mut ws).await;
    assert_eq!((m[0].as_str(), m[2]["id"].as_str()), (Some("EVENT"), Some(live.id.as_str())));
    n.ok::<_, pb::Ack>("post", &post("other", "not for us", "x")).await;
    assert!(quiet(&mut ws, Duration::from_millis(300)).await);
    send(&mut ws, json!(["CLOSE", "feed"])).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    n.ok::<_, pb::Ack>("post", &post("m", "after close", "x")).await;
    assert!(quiet(&mut ws, Duration::from_millis(300)).await, "closed subscriptions stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nostr_client_limits() {
    let n = spawn(|_| {}).await;
    let mut ws = client(&n).await;
    for i in 0..32 {
        send(&mut ws, json!(["REQ", format!("s{i}"), {"kinds": [1]}])).await;
        assert_eq!(recv(&mut ws).await[0], "EOSE");
    }
    send(&mut ws, json!(["REQ", "s32", {"kinds": [1]}])).await;
    let m = recv(&mut ws).await;
    assert_eq!((m[0].as_str(), m[1].as_str()), (Some("CLOSED"), Some("s32")));
    send(&mut ws, json!(["REQ", "s0", {"kinds": [2]}])).await; // replacing an existing sub is fine
    assert_eq!(recv(&mut ws).await[0], "EOSE");
    send(&mut ws, json!(["REQ", "x".repeat(65), {}])).await;
    assert_eq!(recv(&mut ws).await[0], "CLOSED", "sub ids are bounded");

    // publish flood: at most 200 events per wall-clock second per connection
    // (a burst may straddle one second boundary, so at most 400 get through)
    let mut ws = client(&n).await;
    let k = Keys::generate();
    let evs: Vec<Event> = (0..600).map(|i| k.sign(K_REPLY, vec![], format!("{i}"))).collect();
    for e in &evs {
        ws.feed(Message::text(json!(["EVENT", ev_json(e)]).to_string())).await.unwrap();
    }
    ws.flush().await.unwrap();
    let (mut ok, mut limited) = (0, 0);
    for _ in 0..evs.len() {
        let r = recv(&mut ws).await;
        if r[2] == true {
            ok += 1;
        } else if r[3].as_str().unwrap().starts_with("rate-limited") {
            limited += 1;
        }
    }
    assert!((200..=400).contains(&ok) && ok + limited == 600, "ok={ok} limited={limited}");

    // oversized messages close the connection
    let mut ws = client(&n).await;
    ws.send(Message::text(format!("[\"EVENT\",\"{}\"]", "x".repeat(300 * 1024)))).await.ok();
    let closed = tokio::time::timeout(T, async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.unwrap_or(false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nostr_clients_and_sametra_peers_interoperate() {
    let (a, b) = (spawn(|_| {}).await, spawn(|_| {}).await);
    b.connect(&a).await;
    let mut on_b = client(&b).await;
    send(&mut on_b, json!(["REQ", "s", {"#c": ["m"]}])).await;
    assert_eq!(recv(&mut on_b).await[0], "EOSE");
    // a nostr app publishes to a; it reaches b over binary gossip and b's JSON client sees JSON
    let mut on_a = client(&a).await;
    let e = Keys::generate().sign(K_POST, tags(&[&["c", "m"]]), "bridged".into());
    send(&mut on_a, json!(["EVENT", ev_json(&e)])).await;
    assert_eq!(recv(&mut on_a).await[2], true);
    let m = recv(&mut on_b).await;
    assert_eq!(m[0], "EVENT");
    assert_eq!(m[2]["id"], e.id.hex());
    assert_eq!(m[2]["sig"], e.sig.hex());
    // a JSON client never receives REQs from the node (it is not a relay)
    assert!(quiet(&mut on_a, Duration::from_millis(300)).await);
    // new subscribers get the node's heartbeat if they ask for presence
    let mut hb = client(&a).await;
    send(&mut hb, json!(["REQ", "p", {"kinds": [K_HEARTBEAT]}])).await;
    assert_eq!(recv(&mut hb).await[0], "EOSE");
    let m = recv(&mut hb).await;
    assert_eq!((m[0].as_str(), m[2]["kind"].as_u64(), m[2]["pubkey"].as_str()), (Some("EVENT"), Some(K_HEARTBEAT as u64), Some(a.node.keys.pubkey.hex().as_str())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_reaches_beyond_direct_peers() {
    // a <- b <- c <- e, where b, c and e only relay what they joined: content stays on a
    let a = spawn(|c| {
        c.relay_all = false;
        c.join = vec!["bio".into()];
    })
    .await;
    let b = spawn(|c| c.relay_all = false).await;
    let c = spawn(|c| c.relay_all = false).await;
    let e = spawn(|c| c.relay_all = false).await;
    b.connect(&a).await;
    c.connect(&b).await;
    e.connect(&c).await;
    let found: pb::Ack = a.ok("post", &post("bio", "Zebrafish fin regeneration", "single-cell atlas")).await;
    a.ok::<_, pb::Ack>("post", &post("bio", "Unrelated", "nothing to see")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b.has(&found.id) && !c.has(&found.id), "not gossiped to non-members");
    let s = |scope: &str| pb::SearchRequest { query: "zebrafish regeneration".into(), community: "bio".into(), scope: scope.into(), ..Default::default() };
    let local: pb::Items = e.ok("search", &s("node")).await;
    assert!(local.items.is_empty());
    // community scope: peers and their peers (e -> c -> b): a is three levels away
    let near: pb::Items = e.ok("search", &s("community")).await;
    assert!(near.items.is_empty(), "community scope stops two levels out");
    // network scope: three levels (e -> c -> b -> a)
    let net: pb::Items = e.ok("search", &pb::SearchRequest { query: "zebrafish regeneration".into(), scope: "network".into(), ..Default::default() }).await;
    assert_eq!(net.items.len(), 1, "three levels reach a");
    assert_eq!(net.items[0].id, found.id);
    assert!(e.has(&found.id), "found content is cached where it was searched");
    // from c, a is two levels away: community scope finds it
    let r: pb::Items = c.ok("search", &s("community")).await;
    assert_eq!(r.items.len(), 1);
    // nostr clients get NIP-50 search from the local index (no forwarding)
    let mut ws = client(&a).await;
    send(&mut ws, json!(["REQ", "s", {"search": "zebrafish", "kinds": [11]}])).await;
    let m = recv(&mut ws).await;
    assert_eq!((m[0].as_str(), m[2]["id"].as_str()), (Some("EVENT"), Some(found.id.as_str())));
    assert_eq!(recv(&mut ws).await[0], "EOSE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn communities_found_across_the_network_with_members() {
    let a = spawn(|c| c.join = vec!["glaciology".into()]).await;
    let b = spawn(|_| {}).await;
    a.ok::<_, pb::Ack>("create_community", &pb::CreateCommunityRequest { slug: "glaciology".into(), name: "Glaciology".into(), about: "ice sheets and sea level".into() }).await;
    b.connect(&a).await;
    // definitions are global metadata: b learns them through gossip, and a's heartbeat counts a member
    let f = pb::FindCommunitiesRequest { query: "ice sheets".into(), network: true, ..Default::default() };
    let mut found = None;
    for _ in 0..40 {
        let r: pb::CommunitiesResponse = b.ok("find_communities", &f).await;
        if let Some(c) = r.communities.into_iter().find(|c| c.slug == "glaciology" && c.members >= 1) {
            found = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let c = found.expect("community and its member visible from b");
    assert_eq!(c.name, "Glaciology");
    assert!(!c.joined);
}
