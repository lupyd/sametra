//! Multi-node behaviour over real WebSockets: binary protobuf frames between
//! nodes, and the relay protocol as any client sees it.
mod common;
use common::*;
use futures_util::{SinkExt, StreamExt};
use sametra_node::event::*;
use sametra_node::pb;
use std::time::Duration;
use sametra_node::transport::{BoxIo, Dialer, Message, WebSocketStream};

const T: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_flow_both_ways_over_one_link() {
    let (a, b) = (spawn(|_| {}).await, spawn(|_| {}).await);
    let before: pb::Ack = a.ok("post", &post("history", "before the link")).await;
    b.connect(&a).await; // b dials a; a cannot dial b (think NAT)
    assert!(eventually(T, || b.has(&before.id)).await, "dialer syncs history on connect");
    let from_a: pb::Ack = a.ok("post", &post("from a", "x")).await;
    assert!(eventually(T, || b.has(&from_a.id)).await, "a -> b via b's subscription");
    let from_b: pb::Ack = b.ok("post", &post("from b", "x")).await;
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
    let ea: pb::Ack = a.ok("post", &post("from a", "x")).await;
    let ec: pb::Ack = c.ok("post", &post("from c", "x")).await;
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
    let p: pb::Ack = a.ok("post", &post("oops", "wrong")).await;
    assert!(eventually(T, || b.has(&p.id)).await);
    a.ok::<_, pb::Ack>("delete", &id(&p.id)).await;
    assert!(eventually(T, || !b.has(&p.id)).await, "retraction reaches peers");

    let good: pb::Ack = a.ok("post", &post("good", "work")).await;
    assert!(eventually(T, || b.has(&good.id)).await);
    b.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Bee".into(), ..Default::default() }).await;
    b.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.7, event: good.id.clone(), ..Default::default() }).await;
    let (ka, gid) = (a.node.keys.pubkey, H32::from_hex(&good.id).unwrap());
    let rep_on = |n: &TestNode| n.node.model().near(&ka, &gid);
    assert!(eventually(T, || rep_on(&a) > 0.0).await, "rating reached the author's node");
    assert!((rep_on(&a) - rep_on(&b)).abs() < 1e-9, "both nodes compute the same reputation");
    let pr: pb::ProfileResponse = a.ok("profile", &pb::ProfileRequest { pubkey: b.node.keys.pubkey.hex() }).await;
    assert_eq!(pr.profile.unwrap().name, "Bee");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoped_nodes_only_keep_what_they_follow() {
    let a = spawn(|_| {}).await;
    let topic: pb::Ack = a.ok("post", &post("Followed", "x")).await;
    let old_inside: pb::Ack = a.ok("post", &reply(&topic.id, "old, under it")).await;
    let old_outside: pb::Ack = a.ok("post", &post("Elsewhere", "x")).await;
    a.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "A".into(), ..Default::default() }).await;
    let tid = H32::from_hex(&topic.id).unwrap();
    let b = spawn(move |c| {
        c.relay_all = false;
        c.follow = vec![tid];
    })
    .await;
    b.connect(&a).await;
    assert!(eventually(T, || b.has(&topic.id) && b.has(&old_inside.id)).await, "the followed post and its history");
    let deep: pb::Ack = a.ok("post", &reply(&old_inside.id, "new, deeper under it")).await;
    let outside: pb::Ack = a.ok("post", &post("new elsewhere", "x")).await;
    assert!(eventually(T, || b.has(&deep.id)).await, "everything under it, at any depth");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b.has(&outside.id) && !b.has(&old_outside.id));
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
async fn send(ws: &mut Ws, f: pb::Frame) {
    ws.send(Message::binary(pb::encode(&f))).await.unwrap();
}
async fn recv(ws: &mut Ws) -> pb::Frame {
    loop {
        let m = tokio::time::timeout(T, ws.next()).await.expect("timed out waiting for relay").unwrap().unwrap();
        match m {
            Message::Binary(b) => return pb::decode(&b).unwrap(),
            Message::Text(_) => panic!("the relay never sends text"),
            _ => continue,
        }
    }
}
/// No frame arrives for `d`.
async fn quiet(ws: &mut Ws, d: Duration) -> bool {
    tokio::time::timeout(d, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(_))) => return,
                Some(Ok(_)) => continue,
                _ => std::future::pending::<()>().await,
            }
        }
    })
    .await
    .is_err()
}
fn publish(e: &Event) -> pb::Frame {
    pb::Frame { type_pb: pb::F_PUBLISH, event: Some(e.to_pb()), ..Default::default() }
}
fn req(sid: &str, f: Filter) -> pb::Frame {
    pb::Frame { type_pb: pb::F_REQ, sub_id: sid.into(), filters: vec![f.to_pb()], ..Default::default() }
}
fn of_kinds(k: &[u32]) -> Filter {
    Filter { kinds: Some(k.to_vec()), ..Default::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_publish_and_refusals() {
    let n = spawn(|_| {}).await;
    let mut ws = client(&n).await;
    let k = Keys::generate();
    let e = k.sign(K_POST, tags(&[&["title", "from a client"]]), "hi".into());
    // accepted events need no answer; refusals get one
    send(&mut ws, publish(&e)).await;
    send(&mut ws, publish(&e)).await;
    assert!(quiet(&mut ws, Duration::from_millis(300)).await, "accepted and duplicate events are not acknowledged");
    let mut forged = k.sign(K_POST, vec![], "x".into());
    forged.content = b"changed".to_vec();
    send(&mut ws, publish(&forged)).await;
    let r = recv(&mut ws).await;
    assert_eq!((r.type_pb, r.ok, r.event_id), (pb::F_OK, false, forged.id.0.to_vec()));
    assert!(r.message.starts_with("invalid:"));
    send(&mut ws, publish(&k.sign(K_HEARTBEAT, vec![], String::new()))).await;
    assert!(quiet(&mut ws, Duration::from_millis(200)).await, "ephemeral events are accepted and relayed");
    ws.send(Message::binary(vec![0xff, 0xff, 0xff])).await.unwrap();
    assert_eq!(recv(&mut ws).await.type_pb, pb::F_NOTICE);
    // the post is visible through the API like any other
    let f: pb::Items = n.ok("feed", &pb::FeedRequest::default()).await;
    assert_eq!(f.items[0].title, "from a client");
    // text is not part of the protocol: the relay says so and hangs up
    ws.send(Message::text(r#"["REQ","s",{}]"#)).await.unwrap();
    let n2 = recv(&mut ws).await;
    assert!(n2.type_pb == pb::F_NOTICE && n2.message.contains("binary"));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_subscriptions() {
    let n = spawn(|_| {}).await;
    let topic: pb::Ack = n.ok("post", &post("Topic", "x")).await;
    for i in 0..3 {
        n.ok::<_, pb::Ack>("post", &reply(&topic.id, &format!("old{i}"))).await;
    }
    n.ok::<_, pb::Ack>("post", &post("elsewhere", "x")).await;
    let under_topic = || Filter { kinds: Some(vec![11]), tags: vec![("e".into(), vec![topic.id.clone()])], limit: Some(2), ..Default::default() };
    let mut ws = client(&n).await;
    send(&mut ws, req("feed", under_topic())).await;
    let mut got = vec![];
    loop {
        let m = recv(&mut ws).await;
        assert_eq!(m.sub_id, "feed");
        if m.type_pb == pb::F_EOSE {
            break;
        }
        assert_eq!(m.type_pb, pb::F_EVENT);
        let e = Event::from_pb(m.event.unwrap()).unwrap();
        assert!(e.validate().is_ok());
        got.push(e);
    }
    assert_eq!(got.len(), 2, "limit honoured");
    assert!(got.iter().all(|e| e.parent().map(|p| p.hex()) == Some(topic.id.clone())));
    assert!(got[0].created_at <= got[1].created_at, "stored events are sent oldest first");

    // live: new matching events stream in, others do not
    let live: pb::Ack = n.ok("post", &reply(&topic.id, "live")).await;
    let m = recv(&mut ws).await;
    assert_eq!((m.type_pb, hex::encode(&m.event.unwrap().id)), (pb::F_EVENT, live.id.clone()));
    n.ok::<_, pb::Ack>("post", &post("not for us", "x")).await;
    assert!(quiet(&mut ws, Duration::from_millis(300)).await);
    send(&mut ws, pb::Frame { type_pb: pb::F_CLOSE, sub_id: "feed".into(), ..Default::default() }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    n.ok::<_, pb::Ack>("post", &reply(&topic.id, "after close")).await;
    assert!(quiet(&mut ws, Duration::from_millis(300)).await, "closed subscriptions stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_limits() {
    let n = spawn(|_| {}).await;
    let mut ws = client(&n).await;
    for i in 0..32 {
        send(&mut ws, req(&format!("s{i}"), of_kinds(&[1]))).await;
        assert_eq!(recv(&mut ws).await.type_pb, pb::F_EOSE);
    }
    send(&mut ws, req("s32", of_kinds(&[1]))).await;
    let m = recv(&mut ws).await;
    assert_eq!((m.type_pb, m.sub_id.as_str()), (pb::F_CLOSED, "s32"));
    send(&mut ws, req("s0", of_kinds(&[2]))).await; // replacing an existing sub is fine
    assert_eq!(recv(&mut ws).await.type_pb, pb::F_EOSE);
    send(&mut ws, req(&"x".repeat(65), Filter::default())).await;
    assert_eq!(recv(&mut ws).await.type_pb, pb::F_CLOSED, "sub ids are bounded");

    // publish flood: at most 200 events per wall-clock second per connection
    // (a burst may straddle one second boundary, so at most 400 get through)
    let mut ws = client(&n).await;
    let k = Keys::generate();
    let evs: Vec<Event> = (0..600).map(|i| k.sign(K_POST, vec![], format!("{i}"))).collect();
    for e in &evs {
        ws.feed(Message::binary(pb::encode(&publish(e)))).await.unwrap();
    }
    ws.flush().await.unwrap();
    // refusals are the only answers: count them until the relay goes quiet
    let mut limited = 0;
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
        if let Message::Binary(b) = m {
            let r: pb::Frame = pb::decode(&b).unwrap();
            if r.type_pb == pb::F_OK && !r.ok && r.message.starts_with("rate-limited") {
                limited += 1;
            }
        }
    }
    let ok = 600 - limited;
    assert!((200..=400).contains(&ok), "ok={ok} limited={limited}");

    // oversized messages close the connection
    let mut ws = client(&n).await;
    ws.send(Message::binary(vec![0u8; 300 * 1024])).await.ok();
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
async fn clients_on_different_nodes_see_each_other() {
    let (a, b) = (spawn(|_| {}).await, spawn(|_| {}).await);
    b.connect(&a).await;
    let mut on_b = client(&b).await;
    send(&mut on_b, req("s", of_kinds(&[K_POST]))).await;
    assert_eq!(recv(&mut on_b).await.type_pb, pb::F_EOSE);
    // a client publishes to a; it reaches b's subscriber unchanged, signature and all
    let mut on_a = client(&a).await;
    let e = Keys::generate().sign(K_POST, vec![], "across".into());
    send(&mut on_a, publish(&e)).await;
    let m = recv(&mut on_b).await;
    assert_eq!(m.type_pb, pb::F_EVENT);
    let got = Event::from_pb(m.event.unwrap()).unwrap();
    assert_eq!(got, e);
    assert!(got.validate().is_ok());
    // subscribers asking for presence get the node's heartbeat
    let mut hb = client(&a).await;
    send(&mut hb, req("p", of_kinds(&[K_HEARTBEAT]))).await;
    assert_eq!(recv(&mut hb).await.type_pb, pb::F_EOSE);
    let m = recv(&mut hb).await;
    let e = Event::from_pb(m.event.unwrap()).unwrap();
    assert_eq!((e.kind, e.pubkey), (K_HEARTBEAT, a.node.keys.pubkey));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_reaches_beyond_direct_peers() {
    // a <- b <- c <- e, where every node keeps only what it follows: content stays on a
    let a = spawn(|c| c.relay_all = false).await;
    let b = spawn(|c| c.relay_all = false).await;
    let c = spawn(|c| c.relay_all = false).await;
    let e = spawn(|c| c.relay_all = false).await;
    b.connect(&a).await;
    c.connect(&b).await;
    e.connect(&c).await;
    let found: pb::Ack = a.ok("post", &post("Zebrafish fin regeneration", "single-cell atlas")).await;
    a.ok::<_, pb::Ack>("post", &post("Unrelated", "nothing to see")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b.has(&found.id) && !c.has(&found.id), "not gossiped to nodes that do not follow it");
    let s = |scope: &str, within: &str| pb::SearchRequest { query: "zebrafish regeneration".into(), scope: scope.into(), within: within.into(), ..Default::default() };
    let local: pb::Items = e.ok("search", &s("node", "")).await;
    assert!(local.items.is_empty());
    // network scope: three levels (e -> c -> b -> a)
    let net: pb::Items = e.ok("search", &s("network", "")).await;
    assert_eq!(net.items.len(), 1, "three levels reach a");
    assert_eq!(net.items[0].id, found.id);
    assert!(e.has(&found.id), "found content is cached where it was searched");
    // within a subtree, across the network: only what is under that post
    let part: pb::Ack = a.ok("post", &reply(&found.id, "regeneration of zebrafish fins, part two")).await;
    a.ok::<_, pb::Ack>("post", &post("Zebrafish regeneration elsewhere", "another tree")).await;
    let inside: pb::Items = c.ok("search", &s("network", &found.id)).await;
    let ids: Vec<&str> = inside.items.iter().map(|i| i.id.as_str()).collect();
    assert!(ids.contains(&part.id.as_str()) && ids.len() <= 2, "{ids:?}");
    // a search REQ without hops is answered from the local index (no forwarding)
    let mut ws = client(&a).await;
    send(&mut ws, req("s", Filter { search: Some("atlas".into()), kinds: Some(vec![11]), ..Default::default() })).await;
    let m = recv(&mut ws).await;
    assert_eq!((m.type_pb, hex::encode(&m.event.unwrap().id)), (pb::F_EVENT, found.id.clone()));
    assert_eq!(recv(&mut ws).await.type_pb, pb::F_EOSE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mentions_reach_people_through_scoped_nodes() {
    // asker <- relay -> argon, nodes that keep only what they follow: only the mention travels
    let asker = spawn(|c| c.relay_all = false).await;
    let relay = spawn(|c| c.relay_all = false).await;
    let argon = spawn(|c| c.relay_all = false).await;
    relay.connect(&asker).await;
    argon.connect(&relay).await;
    argon.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Doctor Argon".into(), about: "noble gas spectroscopy".into(), agent: true, model: "claude-opus-5-5".into() }).await;
    argon.ok::<_, pb::Ack>("set_capability", &pb::CapabilitySpec { id: "spectra".into(), name: "Gas spectra".into(), kind: "skill".into(), ..Default::default() }).await;
    // the profile is global metadata: asker learns it, and can find and mention Argon
    let mut found = vec![];
    for _ in 0..100 {
        let r: pb::PeopleResponse = asker.ok("find_people", &pb::FindPeopleRequest { query: "@doctorargon".into(), ..Default::default() }).await;
        if r.people.first().is_some_and(|p| p.capabilities == 1) {
            found = r.people;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let p = found.first().expect("asker finds Argon by handle");
    assert_eq!((p.handle.as_str(), p.model.as_str(), p.capabilities, p.human_status.as_str()), ("DoctorArgon", "claude-opus-5-5", 1, "agent"));
    let quiet: pb::Ack = asker.ok("post", &post("Unrelated", "nobody is mentioned")).await;
    let a: pb::Ack = asker.ok("post", &post("Odd spectral line", "@DoctorArgon could you look at this? Also @nobody.")).await;
    assert_eq!(a.mentioned, [argon.node.keys.npub()]);
    assert_eq!(a.unresolved, ["nobody"]);
    assert!(eventually(T, || argon.has(&a.id)).await, "the mention reached Argon through a node that does not follow it");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!argon.has(&quiet.id) && !relay.has(&quiet.id), "posts without mentions stay where they are followed");
    let n: pb::NotificationsResponse = argon.ok("notifications", &pb::NotificationsRequest::default()).await;
    assert_eq!(n.unread, 1);
    assert_eq!((n.notifications[0].reason.as_str(), n.notifications[0].unread), ("mention", true));
    let it = n.notifications[0].item.as_ref().unwrap();
    assert!(it.content.starts_with(&format!("nostr:{} could you", argon.node.keys.npub())));
    assert_eq!(it.mentions[0].name, "Doctor Argon");
    let info: pb::InfoResponse = argon.ok("info", &pb::Empty {}).await;
    assert_eq!(info.unread, 1);
    // reading clears the count, and it stays cleared after a restart of the state
    let _: pb::NotificationsResponse = argon.ok("notifications", &pb::NotificationsRequest { mark_read: true, ..Default::default() }).await;
    let n: pb::NotificationsResponse = argon.ok("notifications", &pb::NotificationsRequest::default()).await;
    assert_eq!(n.unread, 0);
    assert!(!n.notifications[0].unread);
    let st: pb::NodeState = pb::decode(&std::fs::read(argon.dir.0.join("state.pb")).unwrap()).unwrap();
    assert!(st.notifications_read > 0);
    // Argon answers, and opens a part of it as a topic of its own: the asker is told about both
    let r: pb::Ack = argon.ok("post", &reply(&a.id, "That is the 696 nm argon line.")).await;
    let s: pb::Ack = argon.ok("post", &pb::PostRequest { title: "Calibrate the spectrometer".into(), ..reply(&a.id, "first") }).await;
    let mut reasons = vec![];
    for _ in 0..100 {
        let n: pb::NotificationsResponse = asker.ok("notifications", &pb::NotificationsRequest::default()).await;
        reasons = n.notifications.iter().map(|x| (x.reason.clone(), x.item.as_ref().unwrap().id.clone())).collect();
        if reasons.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    reasons.sort();
    let mut want = [("reply".to_string(), r.id), ("reply".to_string(), s.id)];
    want.sort();
    assert_eq!(reasons, want);
}
