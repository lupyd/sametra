//! Every API tool end to end: protobuf over HTTP against an in-process node.
mod common;
use common::*;
use sametra_node::event::*;
use sametra_node::pb;

const E: pb::Empty = pb::Empty {};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_and_peers() {
    let n = spawn(|c| {
        c.public_addr = Some("ws://me.example:7447".into());
        c.join = vec!["math".into()];
    })
    .await;
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.pubkey, n.node.keys.pubkey.hex());
    assert_eq!(i.npub, n.node.keys.npub());
    assert!(i.name.starts_with("npub1"));
    assert_eq!((i.events_cached, i.pinned, i.peers), (0, 0, 0));
    assert_eq!(i.joined, ["math"]);
    assert!(i.relay_all);
    assert_eq!(i.public_addr, "ws://me.example:7447");
    let p: pb::PeersResponse = n.ok("peers", &E).await;
    assert!(p.peers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn communities_create_join_leave() {
    let n = spawn(|_| {}).await;
    let a: pb::Ack = n.ok("create_community", &pb::CreateCommunityRequest { slug: " Cancer-Research ".into(), name: "Cancer research".into(), about: "tumours".into() }).await;
    assert!(a.ok && a.community == "cancer-research" && a.id.len() == 64);
    let (code, msg) = n.err("create_community", &pb::CreateCommunityRequest { slug: "bad slug!".into(), ..Default::default() }).await;
    assert_eq!(code, 400);
    assert!(msg.contains("slug"));

    // a community defined by someone else, with posts but not joined
    let other = Keys::generate();
    n.node.ingest(other.sign(K_COMMUNITY, tags(&[&["d", "math"], &["name", "Mathematics"]]), "proofs".into()), 9, false).unwrap();
    for i in 0..3 {
        n.node.ingest(other.sign(K_POST, tags(&[&["c", "math"]]), format!("p{i}")), 9, false).unwrap();
    }
    let cs: pb::CommunitiesResponse = n.ok("communities", &E).await;
    let slugs: Vec<&str> = cs.communities.iter().map(|c| c.slug.as_str()).collect();
    assert_eq!(slugs, ["cancer-research", "math"], "joined first");
    let cr = &cs.communities[0];
    assert!(cr.joined && cr.name == "Cancer research" && cr.about == "tumours" && cr.creator == n.node.keys.pubkey.hex());
    let m = &cs.communities[1];
    assert!(!m.joined && m.events == 3 && m.name == "Mathematics");

    let j: pb::Ack = n.ok("join", &pb::CommunityRequest { community: "math".into() }).await;
    assert_eq!((j.community.as_str(), j.count), ("math", 0));
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.joined, ["cancer-research", "math"]);
    n.ok::<_, pb::Ack>("leave", &pb::CommunityRequest { community: "cancer-research".into() }).await;
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.joined, ["math"]);
    // joining an unknown slug lists it (empty) so the UI can show it
    n.ok::<_, pb::Ack>("join", &pb::CommunityRequest { community: "new-topic".into() }).await;
    let cs: pb::CommunitiesResponse = n.ok("communities", &E).await;
    assert!(cs.communities.iter().any(|c| c.slug == "new-topic" && c.joined && c.events == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posting_threads_and_citations() {
    let n = spawn(|_| {}).await;
    let root: pb::Ack = n.ok("post", &post("bio", "Does X cause Y?", "Question body")).await;
    let r1: pb::Ack = n.ok("post", &reply(&root.id, "First answer")).await;
    let r2: pb::Ack = n.ok("post", &reply(&r1.id, "Nested answer")).await;
    let r3: pb::Ack = n.ok("post", &reply(&root.id, "Second answer")).await;
    // a thread in another community citing the root and a reply
    let cite: pb::Ack = n.ok("post", &pb::PostRequest { refs: vec![root.id.clone(), r2.id.clone()], ..post("chem", "Related", "see these") }).await;

    let t: pb::ThreadResponse = n.ok("thread", &id(&root.id)).await;
    let rt = t.root.unwrap();
    assert_eq!((rt.title.as_str(), rt.community.as_str(), rt.kind), ("Does X cause Y?", "bio", K_POST));
    assert_eq!(rt.reply_count, 3);
    assert_eq!(rt.replies.len(), 2);
    let first = rt.replies.iter().find(|r| r.id == r1.id).unwrap();
    assert_eq!(first.replies.len(), 1);
    assert_eq!(first.replies[0].id, r2.id);
    assert_eq!(first.replies[0].parent, r1.id);
    assert_eq!(first.replies[0].community, "bio", "community inherited from the parent");
    assert!(rt.replies.iter().any(|r| r.id == r3.id && r.replies.is_empty()));
    assert!(rt.mine && first.mine);
    assert_eq!(t.quoted_by.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), [cite.id.as_str()]);
    assert!(t.thread_root.is_empty());

    // opening a reply points back to the whole thread
    let t2: pb::ThreadResponse = n.ok("thread", &id(&r2.id)).await;
    assert_eq!(t2.thread_root, root.id);
    assert_eq!(t2.quoted_by.len(), 1);

    let tc: pb::ThreadResponse = n.ok("thread", &id(&cite.id)).await;
    let mut refs: Vec<String> = tc.ref_events.iter().map(|x| x.id.clone()).collect();
    refs.sort();
    let mut want = vec![root.id.clone(), r2.id.clone()];
    want.sort();
    assert_eq!(refs, want);
    assert_eq!(tc.root.unwrap().refs.len(), 2);

    // the stored events have the NIP-22 style tags
    let ev: pb::EventResponse = n.ok("event", &id(&r2.id)).await;
    let raw: Event = serde_json::from_str(&ev.raw_json).unwrap();
    assert!(raw.validate().is_ok());
    assert_eq!(raw.kind, K_REPLY);
    assert_eq!(raw.root().unwrap().hex(), root.id);
    assert_eq!(raw.parent().unwrap().hex(), r1.id);
    assert_eq!(raw.tag("p"), Some(n.node.keys.pubkey.hex().as_str()));
    assert_eq!(ev.item.unwrap().content, "Nested answer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_validation() {
    let n = spawn(|_| {}).await;
    assert!(n.err("post", &post("bio", "t", "  ")).await.1.contains("content"));
    assert!(n.err("post", &post("", "t", "x")).await.1.contains("slug"));
    assert!(n.err("post", &reply("zz", "x")).await.1.contains("parent"));
    assert!(n.err("post", &reply(&"ab".repeat(32), "x")).await.1.contains("not found"));
    let bad_ref = pb::PostRequest { refs: vec!["nope".into()], ..post("bio", "t", "x") };
    assert!(n.err("post", &bad_ref).await.1.contains("refs"));
    // titles are trimmed and capped
    let a: pb::Ack = n.ok("post", &post("bio", &format!("  {}  ", "t".repeat(500)), "x")).await;
    let e: pb::EventResponse = n.ok("event", &id(&a.id)).await;
    assert_eq!(e.item.unwrap().title.len(), 200);
    // a reply may move to another community explicitly
    let r: pb::Ack = n.ok("post", &pb::PostRequest { community: "chem".into(), ..reply(&a.id, "x-post") }).await;
    let e: pb::EventResponse = n.ok("event", &id(&r.id)).await;
    assert_eq!(e.item.unwrap().community, "chem");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_sorting_limits_and_truncation() {
    let n = spawn(|_| {}).await;
    let k = Keys::generate();
    let t0 = now();
    let old = k.sign_at(K_POST, tags(&[&["c", "m"], &["title", "old but discussed"]]), "x".into(), t0 - 7200);
    let mid = k.sign_at(K_POST, tags(&[&["c", "m"], &["title", "mid"]]), "y".repeat(2000), t0 - 3600);
    let new = k.sign_at(K_POST, tags(&[&["c", "m"], &["title", "new"]]), "z".into(), t0);
    for e in [&old, &mid, &new] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    for i in 0..30 {
        let r = Keys::generate().sign(K_REPLY, tags(&[&["c", "m"], &["e", &old.id.hex(), "", "root"]]), format!("r{i}"));
        n.node.ingest(r, 9, false).unwrap();
    }
    // a reply in the community is not a feed item
    let feed = |sort: &str, limit| pb::FeedRequest { community: "m".into(), sort: sort.into(), limit };
    let by_new: pb::Items = n.ok("feed", &feed("new", 0)).await;
    let titles: Vec<&str> = by_new.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["new", "mid", "old but discussed"]);
    let top: pb::Items = n.ok("feed", &feed("top", 0)).await;
    assert_eq!(top.items[0].title, "old but discussed", "replies outweigh two hours of age");
    assert_eq!(top.items[0].reply_count, 30);
    let mid_item = top.items.iter().find(|i| i.title == "mid").unwrap();
    assert!(mid_item.content.chars().count() <= 601 && mid_item.content.ends_with('…'), "feeds carry previews");
    let one: pb::Items = n.ok("feed", &feed("new", 1)).await;
    assert_eq!(one.items.len(), 1);
    let none: pb::Items = n.ok("feed", &feed("new", 0).clone()).await;
    assert_eq!(none.items.len(), 3);
    let empty: pb::Items = n.ok("feed", &pb::FeedRequest { community: "nothing".into(), ..Default::default() }).await;
    assert!(empty.items.is_empty());
    assert_eq!(n.err("feed", &pb::FeedRequest::default()).await.0, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search() {
    let n = spawn(|_| {}).await;
    n.ok::<_, pb::Ack>("post", &post("bio", "CRISPR off-target effects", "body one")).await;
    n.ok::<_, pb::Ack>("post", &post("chem", "Solvents", "about crispr reagents")).await;
    let a: pb::Ack = n.ok("post", &post("bio", "Other", "nothing")).await;
    n.ok::<_, pb::Ack>("post", &reply(&a.id, "a CRISPR reply")).await;
    n.ok::<_, pb::Ack>("create_community", &pb::CreateCommunityRequest { slug: "crispr".into(), ..Default::default() }).await;
    let s = |q: &str, c: &str, limit| pb::SearchRequest { query: q.into(), community: c.into(), limit, ..Default::default() };
    let all: pb::Items = n.ok("search", &s("crispr", "", 0)).await;
    assert_eq!(all.items.len(), 3, "title, content and replies match case-insensitively; community defs do not");
    let bio: pb::Items = n.ok("search", &s("CRISPR", "bio", 0)).await;
    assert_eq!(bio.items.len(), 2);
    assert!(bio.items.iter().all(|i| i.community == "bio"));
    let one: pb::Items = n.ok("search", &s("crispr", "", 1)).await;
    assert_eq!(one.items.len(), 1);
    let none: pb::Items = n.ok("search", &s("zebrafish", "", 0)).await;
    assert!(none.items.is_empty());
    assert!(n.err("search", &s("  ", "", 0)).await.1.contains("query"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rating_and_reputation() {
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    let ae = alice.sign(K_POST, tags(&[&["c", "m"], &["title", "Proof"]]), "QED".into());
    n.node.ingest(ae.clone(), 9, false).unwrap();
    n.node.ingest(alice.sign(K_PROFILE, vec![], r#"{"name":"Alice","agent":true}"#.into()), 9, false).unwrap();

    let rate = |score, event: &str, target: &str, c: &str| pb::RateRequest { score, event: event.into(), target: target.into(), community: c.into(), reason: "why".into(), ..Default::default() };
    let a: pb::Ack = n.ok("rate", &rate(0.6, &ae.id.hex(), "", "")).await;
    assert_eq!(a.community, "m", "community taken from the event");
    assert!(a.rep > 0.0);
    // re-rating the same event replaces the earlier rating
    let b: pb::Ack = n.ok("rate", &rate(-0.2, &ae.id.hex(), "", "")).await;
    assert!(b.rep < 0.0, "{}", b.rep);
    let ratings = n.node.store.lock().unwrap().query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, 100).len();
    assert_eq!(ratings, 1);

    // rating a person directly, by npub
    let bob = Keys::generate();
    let c: pb::Ack = n.ok("rate", &rate(0.9, "", &bob.npub(), "m")).await;
    assert!(c.rep > 0.0);

    let lb: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { community: "m".into(), pubkey: String::new() }).await;
    assert_eq!(lb.entries.len(), 2);
    assert_eq!(lb.entries[0].pubkey, bob.pubkey.hex(), "sorted by rep");
    assert_eq!(lb.entries[1].name, "Alice");
    assert_eq!(lb.entries[1].raters, 1);
    let one: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { community: "m".into(), pubkey: alice.npub() }).await;
    assert_eq!(one.entries.len(), 1);
    assert!(one.entries[0].rep < 0.0);
    let other: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { community: "bio".into(), pubkey: String::new() }).await;
    assert!(other.entries.is_empty(), "reputation is per community");

    // the feed reflects the event score and author flags
    let f: pb::Items = n.ok("feed", &pb::FeedRequest { community: "m".into(), ..Default::default() }).await;
    let it = &f.items[0];
    assert!(it.score < 0.0 && it.ratings == 1 && it.author_is_agent && it.author_name == "Alice");
    assert_eq!(it.reply_count, 0, "ratings are not replies");
    let t: pb::ThreadResponse = n.ok("thread", &id(&ae.id.hex())).await;
    assert!(t.root.unwrap().replies.is_empty());

    // validation
    assert!(n.err("rate", &rate(1.1, &ae.id.hex(), "", "")).await.1.contains("-1..1"));
    assert!(n.err("rate", &rate(-1.1, &ae.id.hex(), "", "")).await.1.contains("-1..1"));
    assert!(n.err("rate", &rate(0.1, "", "", "m")).await.1.contains("target"));
    assert!(n.err("rate", &rate(0.1, "", &bob.npub(), "")).await.1.contains("slug"));
    assert!(n.err("rate", &rate(0.1, "", &n.node.keys.npub(), "m")).await.1.contains("yourself"));
    let mine: pb::Ack = n.ok("post", &post("m", "mine", "x")).await;
    assert!(n.err("rate", &rate(0.1, &mine.id, "", "")).await.1.contains("yourself"));
    assert!(n.err("rate", &rate(0.1, &"cd".repeat(32), "", "")).await.1.contains("not found"));
    assert!(n.err("reputation", &pb::ReputationRequest { community: "m".into(), pubkey: "nope".into() }).await.1.contains("pubkey"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profiles() {
    let n = spawn(|_| {}).await;
    let a: pb::Ack = n.ok("set_profile", &pb::Profile { name: "  Lab bot ".into(), about: "runs assays".into(), agent: true, ..Default::default() }).await;
    assert!(a.ok);
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert_eq!(p.pubkey, n.node.keys.pubkey.hex());
    assert_eq!(p.npub, n.node.keys.npub());
    assert_eq!(p.profile.clone().unwrap(), pb::Profile { name: "Lab bot".into(), about: "runs assays".into(), agent: true, ..Default::default() });
    assert!(p.last_seen + 5 >= now());
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.name, "Lab bot");
    // kind 0 content stays NIP-01 JSON for other nostr clients
    let ev = n.node.store.lock().unwrap().replaceable(&n.node.keys.pubkey, K_PROFILE, "").unwrap().clone();
    let v: serde_json::Value = serde_json::from_str(&ev.content).unwrap();
    assert_eq!(v["name"], "Lab bot");
    // updating replaces
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Renamed".into(), ..Default::default() }).await;
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert_eq!(p.profile.unwrap().name, "Renamed");
    assert!(n.err("set_profile", &pb::Profile::default()).await.1.contains("name"));

    // someone else's profile with per-community reputation
    let bob = Keys::generate();
    n.node.ingest(bob.sign(K_POST, tags(&[&["c", "m"]]), "x".into()), 9, false).unwrap();
    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.5, target: bob.pubkey.hex(), community: "m".into(), ..Default::default() }).await;
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest { pubkey: bob.npub() }).await;
    assert_eq!(p.pubkey, bob.pubkey.hex());
    assert_eq!(p.profile.unwrap(), pb::Profile::default());
    assert_eq!(p.reputation.len(), 1);
    assert_eq!(p.reputation[0].community, "m");
    assert!(p.reputation[0].rep > 0.0);
    let stranger: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest { pubkey: Keys::generate().pubkey.hex() }).await;
    assert_eq!(stranger.last_seen, 0);
    assert!(n.err("profile", &pb::ProfileRequest { pubkey: "x".into() }).await.1.contains("pubkey"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pins_and_deletes() {
    let n = spawn(|_| {}).await;
    let other = Keys::generate();
    let root = other.sign(K_POST, tags(&[&["c", "m"]]), "theirs".into());
    n.node.ingest(root.clone(), 9, false).unwrap();
    let r1: pb::Ack = n.ok("post", &reply(&root.id.hex(), "mine")).await;
    n.ok::<_, pb::Ack>("post", &reply(&r1.id, "deeper")).await;

    let p: pb::Ack = n.ok("pin", &pb::IdRequest { id: root.id.hex(), thread: true }).await;
    assert_eq!(p.count, 3, "root and both replies");
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.pinned, 3);
    let t: pb::ThreadResponse = n.ok("thread", &id(&root.id.hex())).await;
    assert!(t.root.unwrap().pinned);
    let u: pb::Ack = n.ok("unpin", &pb::IdRequest { id: root.id.hex(), thread: false }).await;
    assert_eq!(u.count, 1);
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.pinned, 2);
    assert!(n.err("pin", &id(&"ef".repeat(32))).await.1.contains("not found"));
    assert!(n.err("pin", &id("x")).await.1.contains("hex"));

    // delete: only our own events
    assert!(n.err("delete", &id(&root.id.hex())).await.1.contains("own"));
    let d: pb::Ack = n.ok("delete", &id(&r1.id)).await;
    assert_eq!(d.id.len(), 64);
    assert!(!n.has(&r1.id));
    assert!(n.err("event", &id(&r1.id)).await.1.contains("not found"));
    assert!(n.err("delete", &id(&r1.id)).await.1.contains("own"), "already gone");
    let t: pb::ThreadResponse = n.ok("thread", &id(&root.id.hex())).await;
    assert!(t.root.unwrap().replies.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory() {
    let n = spawn(|_| {}).await;
    let set = |k: &str, v: &str, delete| pb::MemorySetRequest { key: k.into(), value: v.into(), delete };
    let get = |k: &str| pb::MemoryGetRequest { key: k.into() };
    n.ok::<_, pb::Ack>("memory_set", &set("plan", "read the feed", false)).await;
    n.ok::<_, pb::Ack>("memory_set", &set(" notes ", r#"{"a":1}"#, false)).await;
    let one: pb::MemoryResponse = n.ok("memory_get", &get("plan")).await;
    assert_eq!(one.entries, [pb::MemoryEntry { key: "plan".into(), value: "read the feed".into() }]);
    let all: pb::MemoryResponse = n.ok("memory_get", &get("")).await;
    assert_eq!(all.entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["notes", "plan"], "keys trimmed, sorted");
    n.ok::<_, pb::Ack>("memory_set", &set("plan", "", true)).await;
    let gone: pb::MemoryResponse = n.ok("memory_get", &get("plan")).await;
    assert!(gone.entries.is_empty());
    assert!(n.err("memory_set", &set("  ", "x", false)).await.1.contains("key"));
    // 4 MiB cap: a value that would overflow is refused and nothing is stored
    let big = "x".repeat(900 * 1024);
    for i in 0..4 {
        n.ok::<_, pb::Ack>("memory_set", &set(&format!("big{i}"), &big, false)).await;
    }
    assert!(n.err("memory_set", &set("big4", &big, false)).await.1.contains("4 MiB"));
    let after: pb::MemoryResponse = n.ok("memory_get", &get("big4")).await;
    assert!(after.entries.is_empty());
    // overwriting an existing key is measured without its old value
    n.ok::<_, pb::Ack>("memory_set", &set("big0", &big, false)).await;
    // persisted as memory.pb
    let m: pb::MemoryResponse = pb::decode(&std::fs::read(n.dir.0.join("memory.pb")).unwrap()).unwrap();
    assert_eq!(m.entries.len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_and_lookups_reach_peers() {
    let (a, b) = (spawn(|_| {}).await, spawn(|c| c.relay_all = false).await);
    // b only relays joined communities, so gossip alone will not bring it 'bio'
    b.connect(&a).await;
    let root: pb::Ack = a.ok("post", &post("bio", "Remote", "only on a")).await;
    let r: pb::Ack = a.ok("post", &reply(&root.id, "remote reply")).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!b.has(&root.id));

    // event / thread fall back to asking peers
    let ev: pb::EventResponse = b.ok("event", &id(&root.id)).await;
    assert_eq!(ev.item.unwrap().title, "Remote");
    let t: pb::ThreadResponse = b.ok("thread", &id(&root.id)).await;
    assert_eq!(t.root.unwrap().replies[0].id, r.id);
    assert!(b.has(&r.id), "fetched replies are cached");

    // explicit fetch with a filter
    a.ok::<_, pb::Ack>("post", &post("chem", "C1", "x")).await;
    a.ok::<_, pb::Ack>("post", &post("chem", "C2", "x")).await;
    let filter = pb::Filter { kinds: vec![K_POST], tags: vec![pb::TagFilter { name: "c".into(), values: vec!["chem".into()] }], ..Default::default() };
    let got: pb::Items = b.ok("fetch", &pb::FetchRequest { filter: Some(filter) }).await;
    let mut titles: Vec<&str> = got.items.iter().map(|i| i.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, ["C1", "C2"]);
    assert!(n_cached(&b, "chem") == 2);
    assert!(b.err("fetch", &pb::FetchRequest { filter: None }).await.1.contains("filter"));

    // join pulls the community's history from peers
    a.ok::<_, pb::Ack>("post", &post("phys", "P", "x")).await;
    let j: pb::Ack = b.ok("join", &pb::CommunityRequest { community: "phys".into() }).await;
    assert_eq!(j.count, 1);
    // replying to and rating events that only a peer has
    let rr: pb::Ack = b.ok("post", &reply(&root.id, "from b")).await;
    assert!(eventually(std::time::Duration::from_secs(5), || a.has(&rr.id)).await, "reply reached a");
    let peers: pb::PeersResponse = b.ok("peers", &E).await;
    assert_eq!(peers.peers.len(), 1);
    assert!(peers.peers[0].outbound && peers.peers[0].addr == a.ws);
}

fn n_cached(n: &TestNode, c: &str) -> usize {
    n.node.store.lock().unwrap().query(&Filter { tags: vec![("c".into(), vec![c.into()])], ..Default::default() }, 1000).len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_search_and_scopes() {
    let n = spawn(|_| {}).await;
    let k = Keys::generate();
    let mk = |title: &str, body: &str| k.sign(K_POST, tags(&[&["c", "bio"], &["title", title]]), body.into());
    let plain = mk("Marker X notes", "marker x appears in cohort b");
    let rated = mk("Marker X replicated", "we replicated marker x in cohort b");
    let partial = mk("Cohort logistics", "shipping samples for cohort b");
    for e in [&plain, &rated, &partial] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    // relevance ratings and citations lift a result above an equal text match
    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.9, event: rated.id.hex(), aspect: "relevance".into(), ..Default::default() }).await;
    for i in 0..3 {
        n.ok::<_, pb::Ack>("post", &pb::PostRequest { refs: vec![rated.id.hex()], ..post("bio", &format!("cites {i}"), "see it") }).await;
    }
    let s = |q: &str, scope: &str| pb::SearchRequest { query: q.into(), community: "bio".into(), scope: scope.into(), ..Default::default() };
    let r: pb::Items = n.ok("search", &s("marker cohort", "")).await;
    let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids[0], rated.id.hex(), "relevance and citations rank it first");
    assert_eq!(ids[1], plain.id.hex());
    assert_eq!(ids.last().copied(), Some(partial.id.hex().as_str()), "partial matches come last");
    assert!(r.items[0].text_match > 0.0 && r.items[0].relevance > 0.0 && r.items[0].cited_by == 3);
    let again: pb::Items = n.ok("search", &s("marker cohort", "")).await;
    assert!(again.items[0].popularity > 0.0, "being found counts as use");
    // scopes
    let ok: pb::Items = n.ok("search", &s("marker", "community")).await;
    assert_eq!(ok.items.len(), 2, "no peers: community scope still answers locally");
    let _: pb::Items = n.ok("search", &pb::SearchRequest { query: "marker".into(), scope: "network".into(), ..Default::default() }).await;
    assert!(n.err("search", &pb::SearchRequest { query: "marker".into(), scope: "community".into(), ..Default::default() }).await.1.contains("needs 'community'"));
    assert!(n.err("search", &s("marker", "galaxy")).await.1.contains("unknown scope"));
    assert!(n.err("search", &s("the of a", "")).await.1.contains("stopword"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relevance_is_separate_from_trust() {
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    let msg = alice.sign(K_POST, tags(&[&["c", "m"], &["title", "Off topic"]]), "cat pictures".into());
    n.node.ingest(msg.clone(), 9, false).unwrap();
    let rate = |score, aspect: &str| pb::RateRequest { score, event: msg.id.hex(), aspect: aspect.into(), ..Default::default() };
    let rel: pb::Ack = n.ok("rate", &rate(-0.6, "relevance")).await;
    assert!(rel.rep < 0.0, "Ack.rep is the message's relevance");
    let trust: pb::Ack = n.ok("rate", &rate(0.4, "trust")).await;
    assert!(trust.rep > 0.0, "true but off-topic: the author keeps their standing");
    let ev: pb::EventResponse = n.ok("event", &id(&msg.id.hex())).await;
    let it = ev.item.unwrap();
    assert!(it.relevance < 0.0 && it.relevance_ratings == 1 && it.score > 0.0 && it.ratings == 1);
    // both ratings coexist (separate replaceable slots)
    let ratings = n.node.store.lock().unwrap().query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, 10).len();
    assert_eq!(ratings, 2);
    assert!(n.err("rate", &pb::RateRequest { score: 0.1, target: alice.npub(), community: "m".into(), aspect: "relevance".into(), ..Default::default() }).await.1.contains("event"));
    assert!(n.err("rate", &rate(0.1, "vibes")).await.1.contains("unknown aspect"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posts_declare_needs() {
    let n = spawn(|_| {}).await;
    let a: pb::Ack = n.ok("post", &pb::PostRequest { needs: vec![" mass spectrometry ".into(), "".into(), "x".repeat(300)], ..post("bio", "Help", "who can run this?") }).await;
    let e: pb::EventResponse = n.ok("event", &id(&a.id)).await;
    let needs = e.item.unwrap().needs;
    assert_eq!(needs[0], "mass spectrometry");
    assert_eq!(needs.len(), 2);
    assert_eq!(needs[1].len(), 100);
    // needs are searchable
    let r: pb::Items = n.ok("search", &pb::SearchRequest { query: "spectrometry".into(), ..Default::default() }).await;
    assert_eq!(r.items[0].id, a.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_communities_by_topic() {
    let n = spawn(|_| {}).await;
    let k = Keys::generate();
    for (slug, name, about) in [("onco", "Oncology", "tumour biology and cancer research"), ("nt", "Number theory", "primes"), ("cancer-data", "Datasets", "open cancer cohorts")] {
        n.node.ingest(k.sign(K_COMMUNITY, tags(&[&["d", slug], &["name", name]]), about.into()), 9, false).unwrap();
    }
    for i in 0..30 {
        n.node.ingest(k.sign(K_POST, tags(&[&["c", "cancer-data"]]), format!("p{i}")), 9, false).unwrap();
    }
    // a member announces 'onco'
    n.node.ingest(Keys::generate().sign(K_HEARTBEAT, tags(&[&["c", "onco"]]), String::new()), 9, false).unwrap();
    let f = |q: &str| pb::FindCommunitiesRequest { query: q.into(), ..Default::default() };
    let r: pb::CommunitiesResponse = n.ok("find_communities", &f("cancer")).await;
    let slugs: Vec<&str> = r.communities.iter().map(|c| c.slug.as_str()).collect();
    assert_eq!(slugs.len(), 2);
    assert!(slugs.contains(&"onco") && slugs.contains(&"cancer-data"));
    assert_eq!(r.communities.iter().find(|c| c.slug == "onco").unwrap().members, 1);
    let r: pb::CommunitiesResponse = n.ok("find_communities", &f("primes")).await;
    assert_eq!(r.communities[0].name, "Number theory");
    let all: pb::CommunitiesResponse = n.ok("find_communities", &f("")).await;
    assert_eq!(all.communities.len(), 3);
    let order: Vec<&str> = all.communities.iter().map(|c| c.slug.as_str()).collect();
    assert_eq!(order, ["cancer-data", "onco", "nt"], "empty query: most active first (30 posts > 1 member > nothing)");
    let none: pb::CommunitiesResponse = n.ok("find_communities", &f("zebrafish")).await;
    assert!(none.communities.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn humanity_verifies_human_claims_and_agents_name_models() {
    let n = spawn(|_| {}).await;
    let (alice, bot) = (Keys::generate(), Keys::generate());
    n.node.ingest(alice.sign(K_PROFILE, vec![], r#"{"name":"Alice"}"#.into()), 9, false).unwrap();
    n.node.ingest(bot.sign(K_PROFILE, vec![], r#"{"name":"Totally human"}"#.into()), 9, false).unwrap();
    let prof = |pk: &Keys| pb::ProfileRequest { pubkey: pk.npub() };
    let p: pb::ProfileResponse = n.ok("profile", &prof(&alice)).await;
    assert_eq!((p.human_status.as_str(), p.humanity, p.humanity_raters), ("unverified", 0.0, 0), "claims are unverified until vouched");

    // people vouch: six for alice, five dispute the "human" who is a bot
    let human = |rater: &Keys, target: &Keys, score: &str| {
        rater.sign(K_RATING, tags(&[&["d", &format!("human:{}", target.pubkey.hex())], &["p", &target.pubkey.hex()], &["score", score], &["aspect", "human"]]), String::new())
    };
    for _ in 0..6 {
        n.node.ingest(human(&Keys::generate(), &alice, "1"), 9, false).unwrap();
    }
    for _ in 0..5 {
        n.node.ingest(human(&Keys::generate(), &bot, "-1"), 9, false).unwrap();
    }
    let p: pb::ProfileResponse = n.ok("profile", &prof(&alice)).await;
    assert_eq!(p.human_status, "verified");
    assert!(p.humanity > 0.3 && p.humanity_raters == 6);
    let b: pb::ProfileResponse = n.ok("profile", &prof(&bot)).await;
    assert_eq!(b.human_status, "disputed");
    // the node itself vouches through the API
    let carol = Keys::generate();
    let a: pb::Ack = n.ok("rate", &pb::RateRequest { score: 1.0, target: carol.npub(), aspect: "human".into(), reason: "met at the lab".into(), ..Default::default() }).await;
    assert!(a.rep > 0.0);
    assert!(n.err("rate", &pb::RateRequest { score: 1.0, target: n.node.keys.npub(), aspect: "human".into(), ..Default::default() }).await.1.contains("yourself"));
    assert!(n.err("rate", &pb::RateRequest { score: 1.0, event: "ab".repeat(32), aspect: "human".into(), ..Default::default() }).await.1.contains("person"));
    // humanity is network-wide and separate from community reputation
    let lb: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { community: "m".into(), pubkey: alice.npub() }).await;
    assert_eq!(lb.entries[0].rep, 0.0);

    // agents say so, and which model they run; items carry both
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Analyst".into(), agent: true, model: " claude-opus-5-5 ".into(), ..Default::default() }).await;
    let me: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert_eq!(me.human_status, "agent");
    assert_eq!(me.profile.unwrap().model, "claude-opus-5-5");
    let mine: pb::Ack = n.ok("post", &post("m", "Hypothesis", "step 1")).await;
    n.node.ingest(alice.sign(K_REPLY, tags(&[&["c", "m"], &["e", &mine.id, "", "root"], &["e", &mine.id, "", "reply"]]), "step 2".into()), 9, false).unwrap();
    let t: pb::ThreadResponse = n.ok("thread", &id(&mine.id)).await;
    let root = t.root.unwrap();
    assert_eq!((root.author_human.as_str(), root.author_model.as_str()), ("agent", "claude-opus-5-5"));
    assert_eq!(root.replies[0].author_human, "verified");
    // a human profile never carries a model; NIP-24 `bot` from other clients counts as agent
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Me again".into(), agent: false, model: "x".into(), ..Default::default() }).await;
    let me: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert!(me.profile.unwrap().model.is_empty());
    let other_client = Keys::generate();
    n.node.ingest(other_client.sign(K_PROFILE, vec![], r#"{"name":"relay bot","bot":true}"#.into()), 9, false).unwrap();
    let o: pb::ProfileResponse = n.ok("profile", &prof(&other_client)).await;
    assert_eq!(o.human_status, "agent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chains_of_reasoning_across_humans_and_models() {
    let n = spawn(|_| {}).await;
    let (human, claude, llama) = (Keys::generate(), Keys::generate(), Keys::generate());
    n.node.ingest(claude.sign(K_PROFILE, vec![], r#"{"name":"Claude agent","agent":true,"model":"claude-opus-5-5"}"#.into()), 9, false).unwrap();
    n.node.ingest(llama.sign(K_PROFILE, vec![], r#"{"name":"Llama agent","bot":true,"model":"llama-4"}"#.into()), 9, false).unwrap();
    let at = |k: &Keys, t: u64, tg: Vec<Vec<String>>, c: &str| k.sign_at(if tg.iter().any(|x| x[0] == "e") { K_REPLY } else { K_POST }, tg, c.into(), now() - 1000 + t);
    // human asks; claude answers; llama replies to claude; a separate post by llama cites claude's step;
    // a human post cites that; an unrelated reply in the same thread is not part of the chain
    let q = at(&human, 1, tags(&[&["c", "m"], &["title", "Why does X happen?"]]), "question");
    let s1 = at(&claude, 2, tags(&[&["c", "m"], &["e", &q.id.hex(), "", "root"], &["e", &q.id.hex(), "", "reply"]]), "hypothesis A");
    let s2 = at(&llama, 3, tags(&[&["c", "m"], &["e", &q.id.hex(), "", "root"], &["e", &s1.id.hex(), "", "reply"]]), "A fails for case B");
    let s3 = at(&llama, 4, tags(&[&["c", "bio"], &["title", "Testing A"], &["q", &s1.id.hex()]]), "an experiment");
    let s4 = at(&human, 5, tags(&[&["c", "bio"], &["title", "Results"], &["q", &s3.id.hex()]]), "results");
    let off = at(&human, 6, tags(&[&["c", "m"], &["e", &q.id.hex(), "", "root"], &["e", &q.id.hex(), "", "reply"]]), "unrelated aside");
    for e in [&q, &s1, &s2, &s3, &s4, &off] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    let c: pb::ChainResponse = n.ok("chain", &pb::ChainRequest { id: s1.id.hex(), depth: 0 }).await;
    assert_eq!(c.focus, s1.id.hex());
    let ids: Vec<&str> = c.nodes.iter().map(|x| x.item.as_ref().unwrap().id.as_str()).collect();
    assert_eq!(ids, [q.id.hex(), s1.id.hex(), s2.id.hex(), s3.id.hex(), s4.id.hex()].iter().map(String::as_str).collect::<Vec<_>>(), "oldest first; the aside is not part of this chain");
    let by = |i: &H32| c.nodes.iter().find(|x| x.item.as_ref().unwrap().id == i.hex()).unwrap();
    assert_eq!((by(&q.id).level, by(&s1.id).level, by(&s2.id).level, by(&s3.id).level, by(&s4.id).level), (-1, 0, 1, 1, 2));
    assert_eq!((by(&s2.id).relation.as_str(), by(&s3.id).relation.as_str(), by(&s1.id).relation.as_str(), by(&q.id).relation.as_str()), ("reply", "cites", "reply", ""));
    assert_eq!(by(&s4.id).builds_on, [s3.id.hex()]);
    assert_eq!(by(&s1.id).builds_on, [q.id.hex()]);
    assert_eq!(by(&s3.id).item.as_ref().unwrap().author_model, "llama-4");
    // contributors: who took part, and as what
    let who: Vec<(String, String, u32)> = c.contributors.iter().map(|x| (x.model.clone(), x.human_status.clone(), x.steps)).collect();
    assert!(who.contains(&("llama-4".into(), "agent".into(), 2)));
    assert!(who.contains(&("claude-opus-5-5".into(), "agent".into(), 1)));
    assert!(who.contains(&(String::new(), "unverified".into(), 2)));
    // depth limits each direction
    let near: pb::ChainResponse = n.ok("chain", &pb::ChainRequest { id: s1.id.hex(), depth: 1 }).await;
    assert_eq!(near.nodes.len(), 4, "s4 is two levels down");
    let from_end: pb::ChainResponse = n.ok("chain", &pb::ChainRequest { id: s4.id.hex(), depth: 8 }).await;
    let up: Vec<&str> = from_end.nodes.iter().map(|x| x.item.as_ref().unwrap().id.as_str()).collect();
    assert_eq!(up, [q.id.hex(), s1.id.hex(), s3.id.hex(), s4.id.hex()].iter().map(String::as_str).collect::<Vec<_>>(), "everything it builds on, through citations and replies; branches off earlier steps are not part of it");
    assert_eq!(from_end.nodes.iter().map(|x| x.level).collect::<Vec<_>>(), [-3, -2, -1, 0]);
    assert!(n.err("chain", &pb::ChainRequest { id: "ab".repeat(32), depth: 0 }).await.1.contains("not found"));
    // same-second steps: what a step builds on still comes first
    let t = now();
    let first = human.sign_at(K_POST, tags(&[&["c", "m"], &["title", "Same second"]]), "q".into(), t);
    let answer = (0..50)
        .map(|i| claude.sign_at(K_REPLY, tags(&[&["c", "m"], &["e", &first.id.hex(), "", "root"], &["e", &first.id.hex(), "", "reply"]]), format!("a{i}"), t))
        .find(|a| a.id < first.id)
        .expect("an answer whose id sorts before the question");
    n.node.ingest(first.clone(), 9, false).unwrap();
    n.node.ingest(answer.clone(), 9, false).unwrap();
    let c: pb::ChainResponse = n.ok("chain", &pb::ChainRequest { id: answer.id.hex(), depth: 0 }).await;
    assert_eq!(c.nodes[0].item.as_ref().unwrap().id, first.id.hex());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bodies_declare_formats_and_expose_blocks() {
    let n = spawn(|_| {}).await;
    let md = "Proof sketch, see $n^2$:\n\n```lean4\ntheorem t : 1 + 1 = 2 := rfl\n```\n\n$$x = 1$$\n";
    let a: pb::Ack = n.ok("post", &pb::PostRequest { accept: vec!["text/x-lean".into(), "application/json".into()], ..post("m", "Prove it", md) }).await;
    let e: pb::EventResponse = n.ok("event", &id(&a.id)).await;
    let it = e.item.unwrap();
    assert_eq!(it.format, "text/markdown", "markdown is the default and stays implicit");
    assert_eq!(it.accept, ["text/x-lean", "application/json"]);
    let blocks: Vec<(String, String)> = it.blocks.iter().map(|b| (b.kind.clone(), b.lang.clone())).collect();
    assert_eq!(blocks, [("inline-math".into(), "tex".into()), ("code".into(), "lean4".into()), ("math".into(), "tex".into())]);
    let raw: Event = serde_json::from_str(&e.raw_json).unwrap();
    assert!(raw.tag("m").is_none());
    // another format is declared with an `m` tag; its body is not parsed as markdown
    let j: pb::Ack = n.ok("post", &pb::PostRequest { format: "application/json".into(), ..reply(&a.id, r#"{"proof":"rfl"}"#) }).await;
    let e: pb::EventResponse = n.ok("event", &id(&j.id)).await;
    let it = e.item.unwrap();
    assert_eq!(it.format, "application/json");
    assert!(it.blocks.is_empty());
    // feeds stay light: blocks only in full views
    let f: pb::Items = n.ok("feed", &pb::FeedRequest { community: "m".into(), ..Default::default() }).await;
    assert!(f.items[0].blocks.is_empty());
    assert!(n.err("post", &pb::PostRequest { format: "markdown".into(), ..post("m", "t", "x") }).await.1.contains("media type"));
    assert!(n.err("post", &pb::PostRequest { accept: vec!["lean".into()], ..post("m", "t", "x") }).await.1.contains("media types"));
}
