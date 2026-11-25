//! Every API tool end to end: protobuf over HTTP against an in-process node.
mod common;
use common::*;
use sametra_node::event::*;
use sametra_node::pb;

const E: pb::Empty = pb::Empty {};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_and_peers() {
    let n = spawn(|c| {
        c.public_addr = Some("me.example:42424".into());
        c.follow = vec![H32([7; 32])];
    })
    .await;
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.pubkey, n.node.keys.pubkey.hex());
    assert_eq!(i.npub, n.node.keys.npub());
    assert!(i.name.starts_with("npub1"));
    assert_eq!((i.events_cached, i.pinned, i.peers), (0, 0, 0));
    assert_eq!(i.following, [H32([7; 32]).hex()]);
    assert!(i.relay_all);
    assert_eq!(i.public_addr, "me.example:42424");
    let p: pb::PeersResponse = n.ok("peers", &E).await;
    assert!(p.peers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_and_unfollow() {
    let n = spawn(|_| {}).await;
    let topic: pb::Ack = n.ok("post", &post("A topic", "x")).await;
    let a: pb::Ack = n.ok("follow", &pb::FollowRequest { id: topic.id.clone() }).await;
    assert_eq!((a.id.as_str(), a.count), (topic.id.as_str(), 1), "count: how much of it this node holds");
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.following, std::slice::from_ref(&topic.id));
    n.ok::<_, pb::Ack>("unfollow", &pb::FollowRequest { id: topic.id.clone() }).await;
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert!(i.following.is_empty());
    assert!(n.err("follow", &pb::FollowRequest { id: "x".into() }).await.1.contains("hex"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posts_form_a_tree() {
    let n = spawn(|_| {}).await;
    let root: pb::Ack = n.ok("post", &post("Does X cause Y?", "Question body")).await;
    let r1: pb::Ack = n.ok("post", &reply(&root.id, "First answer")).await;
    let r2: pb::Ack = n.ok("post", &pb::PostRequest { title: "A part of it".into(), ..reply(&r1.id, "Nested answer, and a topic of its own") }).await;
    let r3: pb::Ack = n.ok("post", &reply(&root.id, "Second answer")).await;
    let deep: pb::Ack = n.ok("post", &reply(&r2.id, "Deeper")).await;
    // a separate topic citing the root and a reply
    let cite: pb::Ack = n.ok("post", &pb::PostRequest { refs: vec![root.id.clone(), r2.id.clone()], ..post("Related", "see these") }).await;

    let t: pb::ThreadResponse = n.ok("thread", &id(&root.id)).await;
    let rt = t.root.unwrap();
    assert_eq!((rt.title.as_str(), rt.kind, rt.depth, rt.children), ("Does X cause Y?", K_POST, 0, 2));
    assert_eq!(rt.reply_count, 4, "everything under it");
    assert_eq!(rt.replies.len(), 2);
    let first = rt.replies.iter().find(|r| r.id == r1.id).unwrap();
    let nested = &first.replies[0];
    assert_eq!((nested.id.as_str(), nested.parent.as_str(), nested.root.as_str(), nested.depth), (r2.id.as_str(), r1.id.as_str(), root.id.as_str(), 2));
    assert_eq!(nested.title, "A part of it", "any post may have a title");
    assert_eq!(nested.replies[0].id, deep.id, "recursively");
    assert!(rt.replies.iter().any(|r| r.id == r3.id && r.replies.is_empty()));
    assert!(rt.mine && first.mine);
    assert!(t.path.is_empty());
    assert_eq!(t.quoted_by.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), [cite.id.as_str()]);

    // any post opens as a topic of its own, with its ancestors
    let t2: pb::ThreadResponse = n.ok("thread", &id(&r2.id)).await;
    assert_eq!(t2.path.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), [root.id.as_str(), r1.id.as_str()], "top-level first");
    assert_eq!(t2.root.as_ref().unwrap().replies[0].id, deep.id);
    assert_eq!(t2.quoted_by.len(), 1);

    let tc: pb::ThreadResponse = n.ok("thread", &id(&cite.id)).await;
    let mut refs: Vec<String> = tc.ref_events.iter().map(|x| x.id.clone()).collect();
    refs.sort();
    let mut want = vec![root.id.clone(), r2.id.clone()];
    want.sort();
    assert_eq!(refs, want);

    // the stored events name all their ancestors
    let ev: pb::EventResponse = n.ok("event", &id(&deep.id)).await;
    let raw = Event::from_pb(pb::decode(&ev.raw).unwrap()).unwrap();
    assert!(raw.validate().is_ok());
    assert_eq!(raw.kind, K_POST);
    assert_eq!(raw.ancestors().iter().map(H32::hex).collect::<Vec<_>>(), [root.id.clone(), r1.id.clone(), r2.id.clone()]);
    assert_eq!(raw.tag("p"), Some(n.node.keys.pubkey.hex().as_str()));
    assert_eq!(ev.item.unwrap().content, "Deeper");

    // feeds: top-level topics, or what is directly under a post
    let tops: pb::Items = n.ok("feed", &pb::FeedRequest { sort: "new".into(), ..Default::default() }).await;
    let mut ids: Vec<&str> = tops.items.iter().map(|x| x.id.as_str()).collect();
    ids.sort();
    let mut want = [root.id.as_str(), cite.id.as_str()];
    want.sort();
    assert_eq!(ids, want);
    let under: pb::Items = n.ok("feed", &pb::FeedRequest { parent: root.id.clone(), sort: "new".into(), ..Default::default() }).await;
    let mut ids: Vec<&str> = under.items.iter().map(|x| x.id.as_str()).collect();
    ids.sort();
    let mut want = [r1.id.as_str(), r3.id.as_str()];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!(under.items.iter().find(|x| x.id == r1.id).unwrap().reply_count, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_validation() {
    let n = spawn(|_| {}).await;
    assert!(n.err("post", &post("t", "  ")).await.1.contains("content"));
    assert!(n.err("post", &reply("zz", "x")).await.1.contains("parent"));
    assert!(n.err("post", &reply(&"ab".repeat(32), "x")).await.1.contains("not found"));
    let bad_ref = pb::PostRequest { refs: vec!["nope".into()], ..post("t", "x") };
    assert!(n.err("post", &bad_ref).await.1.contains("refs"));
    // titles are trimmed and capped
    let a: pb::Ack = n.ok("post", &post(&format!("  {}  ", "t".repeat(500)), "x")).await;
    let e: pb::EventResponse = n.ok("event", &id(&a.id)).await;
    assert_eq!(e.item.unwrap().title.len(), 200);
    // only posts have posts under them
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Me".into(), ..Default::default() }).await;
    let prof = n.node.store.lock().unwrap().replaceable(&n.node.keys.pubkey, K_PROFILE, "").unwrap().id.hex();
    assert!(n.err("post", &reply(&prof, "x")).await.1.contains("must be a post"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn very_deep_trees_still_find_their_subtrees() {
    let n = spawn(|_| {}).await;
    let mut ids = vec![n.ok::<_, pb::Ack>("post", &post("Root", "0")).await.id];
    for i in 1..40 {
        let a: pb::Ack = n.ok("post", &reply(ids.last().unwrap(), &format!("{i}"))).await;
        ids.push(a.id);
    }
    let last = Event::from_pb(pb::decode(&n.ok::<_, pb::EventResponse>("event", &id(ids.last().unwrap())).await.raw).unwrap()).unwrap();
    let anc = last.ancestors();
    assert_eq!(anc.len(), 1 + MAX_PATH + 1, "the root and the nearest ancestors");
    assert_eq!(anc[0].hex(), ids[0]);
    let t: pb::ThreadResponse = n.ok("thread", &id(&ids[0])).await;
    assert_eq!(t.root.unwrap().reply_count, 39, "walking down still finds everything");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_sorting_limits_and_truncation() {
    let n = spawn(|_| {}).await;
    let k = Keys::generate();
    let t0 = now();
    let old = k.sign_at(K_POST, tags(&[&["title", "old but discussed"]]), "x".into(), t0 - 7200);
    let mid = k.sign_at(K_POST, tags(&[&["title", "mid"]]), "y".repeat(2000), t0 - 3600);
    let new = k.sign_at(K_POST, tags(&[&["title", "new"]]), "z".into(), t0);
    for e in [&old, &mid, &new] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    for i in 0..30 {
        n.node.ingest(under(&Keys::generate(), &old, &format!("r{i}")), 9, false).unwrap();
    }
    // posts under a topic are not top-level feed items
    let feed = |sort: &str, limit| pb::FeedRequest { sort: sort.into(), limit, ..Default::default() };
    let by_new: pb::Items = n.ok("feed", &feed("new", 0)).await;
    let titles: Vec<&str> = by_new.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["new", "mid", "old but discussed"]);
    let top: pb::Items = n.ok("feed", &feed("top", 0)).await;
    assert_eq!(top.items[0].title, "old but discussed", "discussion outweighs two hours of age");
    assert_eq!((top.items[0].reply_count, top.items[0].children), (30, 30));
    let mid_item = top.items.iter().find(|i| i.title == "mid").unwrap();
    assert!(mid_item.content.chars().count() <= 601 && mid_item.content.ends_with('…'), "feeds carry previews");
    let one: pb::Items = n.ok("feed", &feed("new", 1)).await;
    assert_eq!(one.items.len(), 1);
    let none: pb::Items = n.ok("feed", &pb::FeedRequest { parent: new.id.hex(), ..Default::default() }).await;
    assert!(none.items.is_empty());
    assert_eq!(n.err("feed", &pb::FeedRequest { parent: "nope".into(), ..Default::default() }).await.0, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_here_and_within_a_subtree() {
    let n = spawn(|_| {}).await;
    let bio: pb::Ack = n.ok("post", &post("CRISPR off-target effects", "body one")).await;
    n.ok::<_, pb::Ack>("post", &post("Solvents", "about crispr reagents")).await;
    let other: pb::Ack = n.ok("post", &post("Other", "nothing")).await;
    let deep: pb::Ack = n.ok("post", &reply(&bio.id, "a CRISPR reply")).await;
    n.ok::<_, pb::Ack>("post", &reply(&other.id, "crispr elsewhere")).await;
    let s = |q: &str, within: &str, limit| pb::SearchRequest { query: q.into(), within: within.into(), limit, ..Default::default() };
    let all: pb::Items = n.ok("search", &s("crispr", "", 0)).await;
    assert_eq!(all.items.len(), 4, "titles and bodies, at any depth, case-insensitively");
    let inside: pb::Items = n.ok("search", &s("CRISPR", &bio.id, 0)).await;
    let mut ids: Vec<&str> = inside.items.iter().map(|i| i.id.as_str()).collect();
    ids.sort();
    let mut want = [bio.id.as_str(), deep.id.as_str()];
    want.sort();
    assert_eq!(ids, want, "within: the post and everything under it");
    let one: pb::Items = n.ok("search", &s("crispr", "", 1)).await;
    assert_eq!(one.items.len(), 1);
    let none: pb::Items = n.ok("search", &s("zebrafish", "", 0)).await;
    assert!(none.items.is_empty());
    assert!(n.err("search", &s("  ", "", 0)).await.1.contains("query"));
    assert!(n.err("search", &s("crispr", "nope", 0)).await.1.contains("within"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ratings_and_contextual_reputation() {
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    n.node.ingest(profile_event(&alice, "Alice", true, ""), 9, false).unwrap();
    // alice writes in two trees: physics (good) and maths (bad)
    let physics = alice.sign(K_POST, tags(&[&["title", "Physics"]]), "on waves".into());
    let maths = alice.sign(K_POST, tags(&[&["title", "Maths"]]), "on primes".into());
    let p2 = under(&alice, &physics, "more on waves");
    for e in [&physics, &maths, &p2] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    let rate = |score, event: &H32| pb::RateRequest { score, event: event.hex(), reason: "why".into(), ..Default::default() };
    let a: pb::Ack = n.ok("rate", &rate(0.8, &physics.id)).await;
    assert!(a.rep > 0.0, "Ack.rep: the author's standing near the message");
    n.ok::<_, pb::Ack>("rate", &rate(0.8, &p2.id)).await;
    let m: pb::Ack = n.ok("rate", &rate(-0.9, &maths.id)).await;
    assert!(m.rep < a.rep, "near maths her standing is lower");
    // re-rating the same message replaces the earlier rating
    n.ok::<_, pb::Ack>("rate", &rate(-0.9, &maths.id)).await;
    let ratings = n.node.store.lock().unwrap().query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, 100).len();
    assert_eq!(ratings, 3);

    // standing is contextual: high near physics, low near maths
    let near = |x: &H32| pb::ReputationRequest { near: x.hex(), pubkey: alice.npub() };
    let ph: pb::ReputationResponse = n.ok("reputation", &near(&physics.id)).await;
    let ma: pb::ReputationResponse = n.ok("reputation", &near(&maths.id)).await;
    assert!(ph.entries[0].rep > 0.0 && ma.entries[0].rep < 0.0, "{} / {}", ph.entries[0].rep, ma.entries[0].rep);
    assert_eq!(ph.entries[0].raters, 1);
    // items carry the author's standing near them
    let t: pb::ThreadResponse = n.ok("thread", &id(&physics.id.hex())).await;
    let it = t.root.unwrap();
    assert!(it.author_rep > 0.0 && it.score > 0.0 && it.ratings == 1 && it.author_is_agent && it.author_name == "Alice");
    assert_eq!(it.reply_count, 1, "ratings are not replies");
    let t: pb::ThreadResponse = n.ok("thread", &id(&maths.id.hex())).await;
    assert!(t.root.unwrap().author_rep < 0.0);
    // a leaderboard near a subject: who wrote under it, by standing there
    let lb: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { near: physics.id.hex(), pubkey: String::new() }).await;
    assert_eq!(lb.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Alice"]);
    // the profile: overall, and where she stands highest
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest { pubkey: alice.npub() }).await;
    let best: Vec<&str> = p.best.iter().map(|i| i.id.as_str()).collect();
    assert!(best.contains(&physics.id.hex().as_str()) && !best.contains(&maths.id.hex().as_str()));

    // validation
    assert!(n.err("rate", &rate(1.1, &physics.id)).await.1.contains("-1..1"));
    assert!(n.err("rate", &rate(-1.1, &physics.id)).await.1.contains("-1..1"));
    assert!(n.err("rate", &pb::RateRequest { score: 0.1, target: alice.npub(), ..Default::default() }).await.1.contains("give 'event'"));
    let mine: pb::Ack = n.ok("post", &post("mine", "x")).await;
    assert!(n.err("rate", &pb::RateRequest { score: 0.1, event: mine.id.clone(), ..Default::default() }).await.1.contains("yourself"));
    assert!(n.err("rate", &rate(0.1, &H32([0xcd; 32]))).await.1.contains("not found"));
    assert!(n.err("reputation", &pb::ReputationRequest { near: String::new(), pubkey: "nope".into() }).await.1.contains("pubkey"));
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
    // the profile travels as a protobuf message, typed by its content type
    let ev = n.node.store.lock().unwrap().replaceable(&n.node.keys.pubkey, K_PROFILE, "").unwrap().clone();
    assert_eq!(ev.content_type, TYPE_PROFILE);
    let stored: pb::Profile = pb::decode(&ev.content).unwrap();
    assert_eq!((stored.name.as_str(), stored.agent), ("Lab bot", true));
    // updating replaces
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Renamed".into(), ..Default::default() }).await;
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert_eq!(p.profile.unwrap().name, "Renamed");
    assert!(n.err("set_profile", &pb::Profile::default()).await.1.contains("name"));

    // someone else's profile: overall standing from their rated posts
    let bob = Keys::generate();
    let bp = bob.sign(K_POST, vec![], "x".into());
    n.node.ingest(bp.clone(), 9, false).unwrap();
    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.5, event: bp.id.hex(), ..Default::default() }).await;
    let p: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest { pubkey: bob.npub() }).await;
    assert_eq!(p.pubkey, bob.pubkey.hex());
    assert_eq!(p.profile.unwrap(), pb::Profile::default());
    assert!(p.rep > 0.0);
    assert_eq!(p.best.len(), 1);
    let stranger: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest { pubkey: Keys::generate().pubkey.hex() }).await;
    assert_eq!((stranger.last_seen, stranger.rep), (0, 0.0));
    assert!(n.err("profile", &pb::ProfileRequest { pubkey: "x".into() }).await.1.contains("pubkey"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pins_and_deletes() {
    let n = spawn(|_| {}).await;
    let other = Keys::generate();
    let root = other.sign(K_POST, vec![], "theirs".into());
    n.node.ingest(root.clone(), 9, false).unwrap();
    let r1: pb::Ack = n.ok("post", &reply(&root.id.hex(), "mine")).await;
    n.ok::<_, pb::Ack>("post", &reply(&r1.id, "deeper")).await;

    let p: pb::Ack = n.ok("pin", &pb::IdRequest { id: root.id.hex(), thread: true }).await;
    assert_eq!(p.count, 3, "the post and everything under it");
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
    // b keeps only what it follows, so gossip alone will not bring it these posts
    b.connect(&a).await;
    let root: pb::Ack = a.ok("post", &post("Remote", "only on a")).await;
    let r: pb::Ack = a.ok("post", &reply(&root.id, "remote reply")).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!b.has(&root.id));

    // event / thread fall back to asking peers
    let ev: pb::EventResponse = b.ok("event", &id(&root.id)).await;
    assert_eq!(ev.item.unwrap().title, "Remote");
    let t: pb::ThreadResponse = b.ok("thread", &id(&root.id)).await;
    assert_eq!(t.root.unwrap().replies[0].id, r.id);
    assert!(b.has(&r.id), "fetched posts are cached");

    // explicit fetch with a filter
    let c1: pb::Ack = a.ok("post", &post("C1", "x")).await;
    let c2: pb::Ack = a.ok("post", &post("C2", "x")).await;
    let filter = Filter { ids: Some(vec![c1.id.clone(), c2.id.clone()]), ..Default::default() }.to_pb();
    let got: pb::Items = b.ok("fetch", &pb::FetchRequest { filter: Some(filter) }).await;
    let mut titles: Vec<&str> = got.items.iter().map(|i| i.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, ["C1", "C2"]);
    assert!(b.has(&c1.id) && b.has(&c2.id));
    assert!(b.err("fetch", &pb::FetchRequest { filter: None }).await.1.contains("filter"));

    // following a post pulls it and everything under it, and keeps it coming
    let p: pb::Ack = a.ok("post", &post("P", "x")).await;
    let pr: pb::Ack = a.ok("post", &reply(&p.id, "under P")).await;
    let f: pb::Ack = b.ok("follow", &pb::FollowRequest { id: p.id.clone() }).await;
    assert_eq!(f.count, 2);
    assert!(b.has(&p.id) && b.has(&pr.id));
    // replying to a post that only a peer has
    let rr: pb::Ack = b.ok("post", &reply(&root.id, "from b")).await;
    assert!(eventually(std::time::Duration::from_secs(5), || a.has(&rr.id)).await, "reply reached a");
    let peers: pb::PeersResponse = b.ok("peers", &E).await;
    assert_eq!(peers.peers.len(), 1);
    assert!(peers.peers[0].outbound && peers.peers[0].addr == a.p2p_addr().await.to_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ranked_search_and_scopes() {
    let n = spawn(|_| {}).await;
    let k = Keys::generate();
    let mk = |title: &str, body: &str| k.sign(K_POST, tags(&[&["title", title]]), body.into());
    let plain = mk("Marker X notes", "marker x appears in cohort b");
    let rated = mk("Marker X replicated", "we replicated marker x in cohort b");
    let partial = mk("Cohort logistics", "shipping samples for cohort b");
    for e in [&plain, &rated, &partial] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    // relevance ratings and citations lift a result above an equal text match
    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.9, event: rated.id.hex(), aspect: "relevance".into(), ..Default::default() }).await;
    for i in 0..3 {
        n.ok::<_, pb::Ack>("post", &pb::PostRequest { refs: vec![rated.id.hex()], ..post(&format!("cites {i}"), "see it") }).await;
    }
    let s = |q: &str, scope: &str| pb::SearchRequest { query: q.into(), scope: scope.into(), ..Default::default() };
    let r: pb::Items = n.ok("search", &s("marker cohort", "")).await;
    let ids: Vec<&str> = r.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids[0], rated.id.hex(), "relevance and citations rank it first");
    assert_eq!(ids[1], plain.id.hex());
    assert_eq!(ids.last().copied(), Some(partial.id.hex().as_str()), "partial matches come last");
    assert!(r.items[0].text_match > 0.0 && r.items[0].relevance > 0.0 && r.items[0].cited_by == 3);
    let again: pb::Items = n.ok("search", &s("marker cohort", "")).await;
    assert!(again.items[0].popularity > 0.0, "being found counts as use");
    // the network scope still answers locally when there are no peers
    let net: pb::Items = n.ok("search", &s("marker", "network")).await;
    assert_eq!(net.items.len(), 2);
    assert!(n.err("search", &s("marker", "galaxy")).await.1.contains("unknown scope"));
    assert!(n.err("search", &s("the of a", "")).await.1.contains("stopword"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relevance_is_separate_from_trust() {
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    let msg = alice.sign(K_POST, tags(&[&["title", "Off topic"]]), "cat pictures".into());
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
    assert!(n.err("rate", &rate(0.1, "vibes")).await.1.contains("unknown aspect"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posts_declare_needs() {
    let n = spawn(|_| {}).await;
    let a: pb::Ack = n.ok("post", &pb::PostRequest { needs: vec![" mass spectrometry ".into(), "".into(), "x".repeat(300)], ..post("Help", "who can run this?") }).await;
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
async fn humanity_verifies_human_claims_and_agents_name_models() {
    let n = spawn(|_| {}).await;
    let (alice, bot) = (Keys::generate(), Keys::generate());
    n.node.ingest(profile_event(&alice, "Alice", false, ""), 9, false).unwrap();
    n.node.ingest(profile_event(&bot, "Totally human", false, ""), 9, false).unwrap();
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
    // humanity is separate from standing
    let lb: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { near: String::new(), pubkey: alice.npub() }).await;
    assert_eq!(lb.entries[0].rep, 0.0);

    // agents say so, and which model they run; items carry both
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Analyst".into(), agent: true, model: " claude-opus-5-5 ".into(), ..Default::default() }).await;
    let me: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert_eq!(me.human_status, "agent");
    assert_eq!(me.profile.unwrap().model, "claude-opus-5-5");
    let mine: pb::Ack = n.ok("post", &post("Hypothesis", "step 1")).await;
    let mine_ev = n.node.store.lock().unwrap().get(&H32::from_hex(&mine.id).unwrap()).unwrap().clone();
    n.node.ingest(under(&alice, &mine_ev, "step 2"), 9, false).unwrap();
    let t: pb::ThreadResponse = n.ok("thread", &id(&mine.id)).await;
    let root = t.root.unwrap();
    assert_eq!((root.author_human.as_str(), root.author_model.as_str()), ("agent", "claude-opus-5-5"));
    assert_eq!(root.replies[0].author_human, "verified");
    // a human profile never carries a model
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Me again".into(), agent: false, model: "x".into(), ..Default::default() }).await;
    let me: pb::ProfileResponse = n.ok("profile", &pb::ProfileRequest::default()).await;
    assert!(me.profile.unwrap().model.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chains_of_reasoning_across_humans_and_models() {
    let n = spawn(|_| {}).await;
    let (human, claude, llama) = (Keys::generate(), Keys::generate(), Keys::generate());
    n.node.ingest(profile_event(&claude, "Claude agent", true, "claude-opus-5-5"), 9, false).unwrap();
    n.node.ingest(profile_event(&llama, "Llama agent", true, "llama-4"), 9, false).unwrap();
    let at = |k: &Keys, t: u64, tg: Vec<Vec<String>>, c: &str| k.sign_at(K_POST, tg, c.into(), now() - 1000 + t);
    let below = |k: &Keys, t: u64, parent: &Event, c: &str| at(k, t, ancestry_tags(&parent.id, &parent.ancestors()), c);
    // human asks; claude answers; llama answers claude; a separate post by llama cites claude's step;
    // a human post cites that; an unrelated answer in the same tree is not part of the chain
    let q = at(&human, 1, tags(&[&["title", "Why does X happen?"]]), "question");
    let s1 = below(&claude, 2, &q, "hypothesis A");
    let s2 = below(&llama, 3, &s1, "A fails for case B");
    let s3 = at(&llama, 4, tags(&[&["title", "Testing A"], &["q", &s1.id.hex()]]), "an experiment");
    let s4 = at(&human, 5, tags(&[&["title", "Results"], &["q", &s3.id.hex()]]), "results");
    let off = below(&human, 6, &q, "unrelated aside");
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
    assert_eq!(up, [q.id.hex(), s1.id.hex(), s3.id.hex(), s4.id.hex()].iter().map(String::as_str).collect::<Vec<_>>(), "everything it builds on; branches off earlier steps are not part of it");
    assert_eq!(from_end.nodes.iter().map(|x| x.level).collect::<Vec<_>>(), [-3, -2, -1, 0]);
    assert!(n.err("chain", &pb::ChainRequest { id: "ab".repeat(32), depth: 0 }).await.1.contains("not found"));
    // same-second steps: what a step builds on still comes first
    let t = now();
    // a question whose id is in the upper half, so an answer that sorts before it is easy to find
    let first = (0..200).map(|i| human.sign_at(K_POST, tags(&[&["title", "Same second"]]), format!("q{i}"), t)).find(|q| q.id.0[0] >= 0x80).unwrap();
    let answer = (0..50)
        .map(|i| claude.sign_at(K_POST, ancestry_tags(&first.id, &[]), format!("a{i}"), t))
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
    let a: pb::Ack = n.ok("post", &pb::PostRequest { accept: vec!["text/x-lean".into(), "application/json".into()], ..post("Prove it", md) }).await;
    let e: pb::EventResponse = n.ok("event", &id(&a.id)).await;
    let it = e.item.unwrap();
    assert_eq!(it.format, "text/markdown", "markdown is the default and stays implicit");
    assert_eq!(it.accept, ["text/x-lean", "application/json"]);
    let blocks: Vec<(String, String)> = it.blocks.iter().map(|b| (b.kind.clone(), b.lang.clone())).collect();
    assert_eq!(blocks, [("inline-math".into(), "tex".into()), ("code".into(), "lean4".into()), ("math".into(), "tex".into())]);
    let raw = Event::from_pb(pb::decode(&e.raw).unwrap()).unwrap();
    assert_eq!(raw.content_type, "", "markdown is the empty content type");
    // another format is the event's content type; its body is not parsed as markdown
    let j: pb::Ack = n.ok("post", &pb::PostRequest { format: "application/json".into(), ..reply(&a.id, r#"{"proof":"rfl"}"#) }).await;
    let e: pb::EventResponse = n.ok("event", &id(&j.id)).await;
    let it = e.item.unwrap();
    assert_eq!(it.format, "application/json");
    assert!(it.blocks.is_empty());
    // feeds stay light: blocks only in full views
    let f: pb::Items = n.ok("feed", &pb::FeedRequest::default()).await;
    assert!(f.items[0].blocks.is_empty());
    assert!(n.err("post", &pb::PostRequest { format: "markdown".into(), ..post("t", "x") }).await.1.contains("media type"));
    assert!(n.err("post", &pb::PostRequest { accept: vec!["lean".into()], ..post("t", "x") }).await.1.contains("media types"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notifications_cover_replies_citations_and_requests() {
    let n = spawn(|_| {}).await;
    let other = Keys::generate();
    let mine: pb::Ack = n.ok("post", &post("My result", "x")).await;
    n.ok::<_, pb::Ack>("post", &reply(&mine.id, "my own reply does not notify me")).await;
    let mine_ev = n.node.store.lock().unwrap().get(&H32::from_hex(&mine.id).unwrap()).unwrap().clone();
    let answer = under(&other, &mine_ev, "an answer");
    let cite = other.sign(K_POST, tags(&[&["title", "Builds on yours"], &["q", &mine.id]]), "see".into());
    let job = other.sign(K_JOB, tags(&[&["p", &n.node.keys.pubkey.hex()], &["param", "capability", "x"]]), String::new());
    for e in [&answer, &cite, &job] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    let r: pb::NotificationsResponse = n.ok("notifications", &pb::NotificationsRequest::default()).await;
    let mut got: Vec<(String, String)> = r.notifications.iter().map(|x| (x.reason.clone(), x.item.as_ref().unwrap().id.clone())).collect();
    got.sort();
    assert_eq!(got, [("citation".to_string(), cite.id.hex()), ("reply".to_string(), answer.id.hex()), ("request".to_string(), job.id.hex())]);
    assert_eq!(r.unread, 3);
    let people: pb::PeopleResponse = n.ok("find_people", &pb::FindPeopleRequest { kind: "agent".into(), ..Default::default() }).await;
    assert!(people.people.is_empty(), "nobody has an agent profile yet");
}

/// A profile event as any node would publish it: a Profile message.
fn profile_event(k: &Keys, name: &str, agent: bool, model: &str) -> Event {
    let p = pb::Profile { name: name.into(), agent, model: model.into(), ..Default::default() };
    k.sign_data(K_PROFILE, vec![], pb::encode(&p), TYPE_PROFILE, now())
}
