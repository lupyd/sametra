//! The tool surface shared by the HTTP API, the web UI and the MCP bridge.
//!
//! Every tool is `POST /api/<tool>` with a protobuf `<Req>` body and a protobuf
//! `<Resp>` reply (see proto/sametra.proto). The `tools!` table in sametra-proto
//! is the single source of truth: it generates this dispatcher and the
//! JSON<->protobuf bridge in sametra-mcp.

use crate::event::*;
use crate::event::{Event, Filter};
use crate::node::Node;
use crate::pb::{self, *};
use crate::rep::RepTable;
use crate::store::{Ev, Store};
use std::sync::Arc;
use std::time::Duration;

type R<T> = Result<T, String>;

macro_rules! gen_server {
    ($($name:ident : $req:ty => $resp:ty, $w:expr;)*) => {
        /// Tools that sign with this node's key or touch private state.
        pub fn is_write(tool: &str) -> bool {
            match tool { $(stringify!($name) => $w,)* _ => false }
        }
        /// Decode request, run tool, encode response.
        pub async fn dispatch(node: &Arc<Node>, tool: &str, body: &[u8]) -> Option<R<Vec<u8>>> {
            Some(match tool {
                $(stringify!($name) => match pb::decode::<$req>(body) {
                    Ok(req) => $name(node, req).await.map(|r: $resp| pb::encode(&r)),
                    Err(e) => Err(e),
                },)*
                _ => return None,
            })
        }
    };
}
sametra_proto::tools!(gen_server);

// ---- helpers ---------------------------------------------------------------

fn slug(s: &str) -> R<String> {
    let s = s.trim().to_lowercase();
    if s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err("community slug must be 1-64 chars of a-z 0-9 - _ .".into());
    }
    Ok(s)
}
fn hex_id(s: &str, what: &str) -> R<H32> {
    H32::from_hex(s.trim()).ok_or_else(|| format!("'{what}' must be a 64-char hex event id"))
}
fn t(k: &str, v: impl Into<String>) -> Vec<String> {
    vec![k.to_string(), v.into()]
}
fn ack(id: Option<&H32>) -> Ack {
    Ack { ok: true, id: id.map(|i| i.hex()).unwrap_or_default(), ..Default::default() }
}
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
fn opt_slug(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

fn profile_of(st: &Store, pk: &H32) -> Profile {
    let Some(v) = st.replaceable(pk, K_PROFILE, "").and_then(|e| serde_json::from_str::<serde_json::Value>(&e.content).ok()) else {
        return Profile::default();
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").chars().take(2000).collect::<String>();
    Profile { name: s("name").chars().take(64).collect(), about: s("about"), agent: v.get("agent").and_then(|x| x.as_bool()).unwrap_or(false) }
}
fn name_of(st: &Store, pk: &H32) -> String {
    let n = profile_of(st, pk).name;
    if n.is_empty() { npub(pk)[..14].to_string() } else { n }
}

fn view(st: &Store, rep: &RepTable, e: &Event, full: bool, me: &H32) -> Item {
    let author_rep = rep.user(&e.pubkey);
    let score = rep.event(&e.id);
    let content = if full || e.content.len() <= 600 {
        e.content.clone()
    } else {
        let mut end = 600;
        while !e.content.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &e.content[..end])
    };
    Item {
        id: e.id.hex(),
        kind: e.kind,
        author: e.pubkey.hex(),
        author_name: name_of(st, &e.pubkey),
        author_is_agent: profile_of(st, &e.pubkey).agent,
        author_rep: round2(author_rep),
        score: round2(score),
        ratings: rep.event.get(&e.id).map(|x| x.1 as u32).unwrap_or(0),
        low_trust: author_rep < -3.0 || score < -3.0,
        community: e.community().unwrap_or("").into(),
        title: e.tag("title").unwrap_or("").into(),
        content,
        created_at: e.created_at,
        parent: e.parent().filter(|_| e.kind == K_REPLY).map(|p| p.hex()).unwrap_or_default(),
        refs: e.tags_named("q").map(String::from).collect(),
        pinned: st.pinned.contains(&e.id),
        mine: e.pubkey == *me,
        reply_count: 0,
        replies: vec![],
    }
}

fn rank(rep: &RepTable, e: &Event, replies: usize) -> f64 {
    let age_h = now().saturating_sub(e.created_at) as f64 / 3600.0;
    rep.event(&e.id) + 0.5 * rep.user(&e.pubkey) + (1.0 + replies as f64).ln() - age_h / 24.0
}

fn tree(st: &Store, rep: &RepTable, e: &Ev, depth: usize, budget: &mut usize, me: &H32) -> Item {
    let mut v = view(st, rep, e, true, me);
    let mut kids = st.children(&e.id);
    kids.sort_by(|a, b| rank(rep, b, 0).total_cmp(&rank(rep, a, 0)));
    for k in kids {
        if *budget == 0 || depth > 64 {
            break;
        }
        *budget -= 1;
        v.replies.push(tree(st, rep, &k, depth + 1, budget, me));
    }
    v.reply_count = v.replies.iter().map(|r| 1 + r.reply_count).sum();
    v
}

async fn get_or_fetch(node: &Arc<Node>, id: &H32) -> Option<Ev> {
    if let Some(e) = node.store.lock().unwrap().get(id).cloned() {
        return Some(e);
    }
    let f = Filter { ids: Some(vec![id.hex()]), ..Default::default() };
    node.remote_fetch(vec![f], Duration::from_secs(3)).await.into_iter().find(|e| e.id == *id)
}

fn c_filter(kinds: Option<Vec<u32>>, c: &str) -> Filter {
    Filter { kinds, tags: vec![("c".into(), vec![c.to_string()])], ..Default::default() }
}

// ---- tools -----------------------------------------------------------------

async fn info(node: &Arc<Node>, _: Empty) -> R<InfoResponse> {
    let st = node.store.lock().unwrap();
    Ok(InfoResponse {
        pubkey: node.keys.pubkey.hex(),
        npub: node.keys.npub(),
        name: name_of(&st, &node.keys.pubkey),
        events_cached: st.len() as u32,
        pinned: st.pinned.len() as u32,
        joined: node.joined.lock().unwrap().iter().cloned().collect(),
        relay_all: node.cfg.relay_all,
        peers: node.peers.lock().unwrap().len() as u32,
        public_addr: node.cfg.public_addr.clone().unwrap_or_default(),
    })
}

async fn peers(node: &Arc<Node>, _: Empty) -> R<PeersResponse> {
    Ok(PeersResponse { peers: node.peer_list() })
}

async fn communities(node: &Arc<Node>, _: Empty) -> R<CommunitiesResponse> {
    let joined = node.joined.lock().unwrap().clone();
    let st = node.store.lock().unwrap();
    let mut m: std::collections::BTreeMap<String, Community> = Default::default();
    for e in st.query(&Filter { kinds: Some(vec![K_COMMUNITY]), ..Default::default() }, 10_000) {
        let d = e.d_tag().to_string();
        if d.is_empty() || m.contains_key(&d) {
            continue;
        }
        m.insert(
            d.clone(),
            Community { name: e.tag("name").unwrap_or(&d).into(), about: e.content.clone(), creator: e.pubkey.hex(), slug: d, ..Default::default() },
        );
    }
    let blank = |c: &String| Community { slug: c.clone(), name: c.clone(), ..Default::default() };
    for (c, n) in st.communities() {
        m.entry(c.clone()).or_insert_with(|| blank(c)).events = n as u32;
    }
    for c in &joined {
        m.entry(c.clone()).or_insert_with(|| blank(c));
    }
    let mut v: Vec<Community> = m.into_values().map(|mut x| (x.joined = joined.contains(&x.slug), x).1).collect();
    v.sort_by(|a, b| b.joined.cmp(&a.joined).then(b.events.cmp(&a.events)));
    Ok(CommunitiesResponse { communities: v })
}

async fn create_community(node: &Arc<Node>, r: CreateCommunityRequest) -> R<Ack> {
    let c = slug(&r.slug)?;
    let name = if r.name.trim().is_empty() { c.clone() } else { r.name.trim().chars().take(100).collect() };
    let e = node.publish(K_COMMUNITY, vec![t("d", &c), t("name", name)], r.about)?;
    node.joined.lock().unwrap().insert(c.clone());
    node.save_state();
    Ok(Ack { community: c, ..ack(Some(&e.id)) })
}

async fn join(node: &Arc<Node>, r: CommunityRequest) -> R<Ack> {
    let c = slug(&r.community)?;
    node.joined.lock().unwrap().insert(c.clone());
    node.save_state();
    let mut f = c_filter(None, &c);
    f.limit = Some(2000);
    let n = node.remote_fetch(vec![f], Duration::from_secs(3)).await.len();
    Ok(Ack { count: n as u32, community: c, ..ack(None) })
}

async fn leave(node: &Arc<Node>, r: CommunityRequest) -> R<Ack> {
    let c = slug(&r.community)?;
    node.joined.lock().unwrap().remove(&c);
    node.save_state();
    Ok(Ack { community: c, ..ack(None) })
}

async fn feed(node: &Arc<Node>, r: FeedRequest) -> R<Items> {
    let c = slug(&r.community)?;
    let limit = if r.limit == 0 { 30 } else { r.limit.min(500) } as usize;
    let rep = node.reputation(&c);
    let me = node.keys.pubkey;
    let st = node.store.lock().unwrap();
    let mut items: Vec<(f64, Item)> = st
        .query(&c_filter(Some(vec![K_POST]), &c), 5000)
        .iter()
        .map(|e| {
            let n = st.descendant_count(&e.id, 10_000);
            let mut v = view(&st, &rep, e, false, &me);
            v.reply_count = n as u32;
            (if r.sort == "new" { e.created_at as f64 } else { rank(&rep, e, n) }, v)
        })
        .collect();
    items.sort_by(|x, y| y.0.total_cmp(&x.0));
    Ok(Items { items: items.into_iter().take(limit).map(|x| x.1).collect() })
}

async fn thread(node: &Arc<Node>, r: IdRequest) -> R<ThreadResponse> {
    let id = hex_id(&r.id, "id")?;
    let root = get_or_fetch(node, &id).await.ok_or("event not found on this node or its peers")?;
    if !node.peers.lock().unwrap().is_empty() {
        let f = Filter { tags: vec![("e".into(), vec![id.hex()])], limit: Some(5000), ..Default::default() };
        node.remote_fetch(vec![f], Duration::from_millis(1500)).await;
    }
    let rep = node.reputation(root.community().unwrap_or(""));
    let me = node.keys.pubkey;
    let st = node.store.lock().unwrap();
    let mut budget = 5000;
    let idh = id.hex();
    Ok(ThreadResponse {
        root: Some(tree(&st, &rep, &root, 0, &mut budget, &me)),
        quoted_by: st.referencing(&id).iter().filter(|e| e.tags_named("q").any(|q| q == idh)).map(|e| view(&st, &rep, e, false, &me)).collect(),
        ref_events: root.tags_named("q").filter_map(H32::from_hex).filter_map(|r| st.get(&r).map(|e| view(&st, &rep, e, false, &me))).collect(),
        thread_root: root.root().filter(|r| *r != root.id && root.kind == K_REPLY).map(|r| r.hex()).unwrap_or_default(),
    })
}

async fn event(node: &Arc<Node>, r: IdRequest) -> R<EventResponse> {
    let id = hex_id(&r.id, "id")?;
    let e = get_or_fetch(node, &id).await.ok_or("event not found")?;
    let rep = node.reputation(e.community().unwrap_or(""));
    let st = node.store.lock().unwrap();
    Ok(EventResponse { item: Some(view(&st, &rep, &e, true, &node.keys.pubkey)), raw_json: serde_json::to_string(&*e).unwrap() })
}

async fn search(node: &Arc<Node>, r: SearchRequest) -> R<Items> {
    let q = r.query.trim().to_lowercase();
    if q.is_empty() {
        return Err("missing 'query'".into());
    }
    let c = opt_slug(&r.community);
    let limit = if r.limit == 0 { 20 } else { r.limit.min(200) } as usize;
    let hits: Vec<Ev> = node
        .store
        .lock()
        .unwrap()
        .iter()
        .filter(|e| (e.kind == K_POST || e.kind == K_REPLY) && c.is_none_or(|c| e.community() == Some(c)))
        .filter(|e| e.content.to_lowercase().contains(&q) || e.tag("title").is_some_and(|t| t.to_lowercase().contains(&q)))
        .take(limit)
        .cloned()
        .collect();
    let mut items = vec![];
    for e in hits {
        let rep = node.reputation(e.community().unwrap_or(""));
        items.push(view(&node.store.lock().unwrap(), &rep, &e, false, &node.keys.pubkey));
    }
    Ok(Items { items })
}

async fn post(node: &Arc<Node>, r: PostRequest) -> R<Ack> {
    if r.content.trim().is_empty() {
        return Err("missing 'content'".into());
    }
    let mut tags = vec![];
    let kind;
    if let Some(p) = opt_slug(&r.parent) {
        let pid = hex_id(p, "parent")?;
        let parent = get_or_fetch(node, &pid).await.ok_or("parent event not found")?;
        let c = opt_slug(&r.community).or(parent.community()).ok_or("community required")?;
        let root = if parent.kind == K_POST { pid } else { parent.root().unwrap_or(pid) };
        tags.push(t("c", slug(c)?));
        tags.push(vec!["e".into(), root.hex(), "".into(), "root".into()]);
        tags.push(vec!["e".into(), pid.hex(), "".into(), "reply".into()]);
        tags.push(t("p", parent.pubkey.hex()));
        kind = K_REPLY;
    } else {
        tags.push(t("c", slug(&r.community)?));
        if !r.title.trim().is_empty() {
            tags.push(t("title", r.title.trim().chars().take(200).collect::<String>()));
        }
        kind = K_POST;
    }
    for x in r.refs.iter().take(50) {
        tags.push(t("q", hex_id(x, "refs")?.hex()));
    }
    let e = node.publish(kind, tags, r.content)?;
    Ok(ack(Some(&e.id)))
}

async fn rate(node: &Arc<Node>, r: RateRequest) -> R<Ack> {
    if !(-10..=10).contains(&r.score) {
        return Err("score must be in -10..10".into());
    }
    let (target, ev, comm) = if let Some(eid) = opt_slug(&r.event) {
        let id = hex_id(eid, "event")?;
        let e = get_or_fetch(node, &id).await.ok_or("event not found")?;
        let c = opt_slug(&r.community).or(e.community()).ok_or("community required")?.to_string();
        (e.pubkey, Some(id), c)
    } else {
        let p = parse_pubkey(&r.target).ok_or("give 'event', or 'target' (pubkey/npub) + 'community'")?;
        (p, None, r.community.clone())
    };
    if target == node.keys.pubkey {
        return Err("you cannot rate yourself".into());
    }
    let c = slug(&comm)?;
    let d = format!("{c}:{}:{}", target.hex(), ev.map(|e| e.hex()).unwrap_or_default());
    let mut tags = vec![t("d", d), t("c", &c), t("p", target.hex()), t("score", r.score.to_string())];
    if let Some(e) = ev {
        tags.push(t("e", e.hex()));
    }
    let e = node.publish(K_RATING, tags, r.reason)?;
    Ok(Ack { rep: round2(node.reputation(&c).user(&target)), community: c, ..ack(Some(&e.id)) })
}

async fn reputation(node: &Arc<Node>, r: ReputationRequest) -> R<ReputationResponse> {
    let c = slug(&r.community)?;
    let rep = node.reputation(&c);
    let st = node.store.lock().unwrap();
    let entry = |pk: &H32| RepEntry { pubkey: pk.hex(), name: name_of(&st, pk), rep: round2(rep.user(pk)), raters: rep.raters.get(pk).copied().unwrap_or(0) as u32 };
    if let Some(p) = opt_slug(&r.pubkey) {
        let pk = parse_pubkey(p).ok_or("bad pubkey")?;
        return Ok(ReputationResponse { entries: vec![entry(&pk)] });
    }
    let mut v: Vec<(&H32, &f64)> = rep.user.iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(a.1));
    Ok(ReputationResponse { entries: v.into_iter().take(200).map(|(pk, _)| entry(pk)).collect() })
}

async fn profile(node: &Arc<Node>, r: ProfileRequest) -> R<ProfileResponse> {
    let pk = match opt_slug(&r.pubkey) {
        Some(p) => parse_pubkey(p).ok_or("bad pubkey")?,
        None => node.keys.pubkey,
    };
    let comms: Vec<String> = node.store.lock().unwrap().communities().map(|(c, _)| c.clone()).collect();
    let mut reputation = vec![];
    for c in comms {
        let t = node.reputation(&c);
        if t.user.contains_key(&pk) {
            reputation.push(CommunityRep { rep: round2(t.user(&pk)), community: c });
        }
    }
    let st = node.store.lock().unwrap();
    Ok(ProfileResponse {
        pubkey: pk.hex(),
        npub: npub(&pk),
        profile: Some(profile_of(&st, &pk)),
        reputation,
        last_seen: st.last_seen.get(&pk).copied().unwrap_or(0),
    })
}

async fn set_profile(node: &Arc<Node>, p: Profile) -> R<Ack> {
    if p.name.trim().is_empty() {
        return Err("missing 'name'".into());
    }
    // kind 0 content stays NIP-01 JSON so other nostr clients can read it
    let content = serde_json::json!({"name": p.name.trim(), "about": p.about, "agent": p.agent}).to_string();
    let e = node.publish(K_PROFILE, vec![], content)?;
    Ok(ack(Some(&e.id)))
}

async fn pin_impl(node: &Arc<Node>, r: IdRequest, on: bool) -> R<Ack> {
    let id = hex_id(&r.id, "id")?;
    if on {
        get_or_fetch(node, &id).await.ok_or("event not found")?;
    }
    let mut ids = vec![id];
    if r.thread {
        if on {
            let f = Filter { tags: vec![("e".into(), vec![id.hex()])], ..Default::default() };
            node.remote_fetch(vec![f], Duration::from_millis(1500)).await;
        }
        let st = node.store.lock().unwrap();
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            for c in st.children(&x) {
                ids.push(c.id);
                stack.push(c.id);
            }
        }
    }
    let n = ids.len() as u32;
    node.store.lock().unwrap().pin(ids, on);
    node.save_state();
    Ok(Ack { count: n, ..ack(Some(&id)) })
}
async fn pin(node: &Arc<Node>, r: IdRequest) -> R<Ack> {
    pin_impl(node, r, true).await
}
async fn unpin(node: &Arc<Node>, r: IdRequest) -> R<Ack> {
    pin_impl(node, r, false).await
}

async fn delete(node: &Arc<Node>, r: IdRequest) -> R<Ack> {
    let id = hex_id(&r.id, "id")?;
    if !node.store.lock().unwrap().get(&id).is_some_and(|e| e.pubkey == node.keys.pubkey) {
        return Err("you can only delete your own cached events".into());
    }
    let e = node.publish(K_DELETE, vec![t("e", id.hex())], String::new())?;
    Ok(ack(Some(&e.id)))
}

async fn fetch(node: &Arc<Node>, r: FetchRequest) -> R<Items> {
    let f = Filter::from_pb(&r.filter.ok_or("missing 'filter'")?);
    let evs = node.remote_fetch(vec![f], Duration::from_secs(4)).await;
    let st = node.store.lock().unwrap();
    let empty = RepTable::default();
    Ok(Items { items: evs.iter().take(500).map(|e| view(&st, &empty, e, false, &node.keys.pubkey)).collect() })
}

async fn memory_get(node: &Arc<Node>, r: MemoryGetRequest) -> R<MemoryResponse> {
    let m = node.memory.lock().unwrap();
    let entries = match opt_slug(&r.key) {
        Some(k) => m.get(k).map(|v| vec![MemoryEntry { key: k.into(), value: v.clone() }]).unwrap_or_default(),
        None => m.iter().map(|(k, v)| MemoryEntry { key: k.clone(), value: v.clone() }).collect(),
    };
    Ok(MemoryResponse { entries })
}

async fn memory_set(node: &Arc<Node>, r: MemorySetRequest) -> R<Ack> {
    let k = r.key.trim().to_string();
    if k.is_empty() {
        return Err("missing 'key'".into());
    }
    {
        let mut m = node.memory.lock().unwrap();
        if r.delete {
            m.remove(&k);
        } else {
            let total: usize = m.iter().filter(|(x, _)| **x != k).map(|(x, v)| x.len() + v.len()).sum();
            if total + k.len() + r.value.len() > 4 << 20 {
                return Err("memory is limited to 4 MiB".into());
            }
            m.insert(k, r.value);
        }
    }
    node.save_memory();
    Ok(ack(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Config;

    fn node() -> Arc<Node> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let data = std::env::temp_dir().join(format!("sametra-api-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        Node::new(Config { data, ..Default::default() }, Keys::generate()).unwrap()
    }

    #[test]
    fn slug_rules() {
        assert_eq!(slug("  Cancer-Research ").unwrap(), "cancer-research");
        assert_eq!(slug("a.b_c-1").unwrap(), "a.b_c-1");
        for bad in ["", "  ", "has space", "slash/", "ünï", &"x".repeat(65)] {
            assert!(slug(bad).is_err(), "{bad:?}");
        }
        assert!(slug(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn small_helpers() {
        assert!(hex_id("zz", "id").unwrap_err().contains("'id'"));
        assert_eq!(hex_id(&format!(" {} ", "ab".repeat(32)), "id").unwrap(), H32([0xab; 32]));
        assert_eq!(round2(1.23456), 1.23);
        assert_eq!(round2(-0.005), -0.01);
        assert_eq!(opt_slug("  "), None);
        assert_eq!(opt_slug(" x "), Some("x"));
        assert_eq!(ack(None).id, "");
        assert!(ack(Some(&H32([1; 32]))).ok);
    }

    #[test]
    fn write_tools_are_marked() {
        for w in ["create_community", "join", "leave", "post", "rate", "set_profile", "pin", "unpin", "delete", "fetch", "memory_get", "memory_set"] {
            assert!(is_write(w), "{w}");
        }
        for r in ["info", "communities", "feed", "thread", "event", "search", "reputation", "profile", "peers", "nope"] {
            assert!(!is_write(r), "{r}");
        }
    }

    #[tokio::test]
    async fn dispatch_decodes_and_rejects() {
        let n = node();
        assert!(dispatch(&n, "nope", &[]).await.is_none());
        assert!(dispatch(&n, "feed", &[0xff, 0xff]).await.unwrap().unwrap_err().contains("bad protobuf"));
        let out = dispatch(&n, "info", &[]).await.unwrap().unwrap();
        assert_eq!(pb::decode::<InfoResponse>(&out).unwrap().pubkey, n.keys.pubkey.hex());
        let err = dispatch(&n, "post", &pb::encode(&PostRequest::default())).await.unwrap().unwrap_err();
        assert!(err.contains("content"));
    }

    #[test]
    fn view_truncates_on_char_boundary_and_flags_low_trust() {
        let n = node();
        let k = Keys::generate();
        let body = "é".repeat(400); // 800 bytes, 2 per char
        let e = k.sign(K_POST, vec![t("c", "m"), t("title", "T"), t("q", "ab".repeat(32))], body.clone());
        let st = n.store.lock().unwrap();
        let short = view(&st, &RepTable::default(), &e, false, &n.keys.pubkey);
        assert!(short.content.ends_with('…') && short.content.len() <= 603);
        assert_eq!(view(&st, &RepTable::default(), &e, true, &n.keys.pubkey).content, body);
        assert_eq!((short.title.as_str(), short.community.as_str(), short.refs.len()), ("T", "m", 1));
        assert!(!short.mine && !short.low_trust && short.parent.is_empty());
        assert!(short.author_name.starts_with("npub1"), "falls back to a short npub");
        let mut rep = RepTable::default();
        rep.user.insert(k.pubkey, -4.0);
        assert!(view(&st, &rep, &e, false, &n.keys.pubkey).low_trust);
        let mut rep = RepTable::default();
        rep.event.insert(e.id, (-5.0, 3));
        let v = view(&st, &rep, &e, false, &n.keys.pubkey);
        assert!(v.low_trust && v.ratings == 3 && v.score == -5.0);
    }

    #[test]
    fn rank_prefers_reputation_replies_and_recency() {
        let k = Keys::generate();
        let fresh = k.sign(K_POST, vec![], "a".into());
        let old = k.sign_at(K_POST, vec![], "b".into(), now() - 3 * 86400);
        let rep = RepTable::default();
        assert!(rank(&rep, &fresh, 0) > rank(&rep, &old, 0));
        assert!(rank(&rep, &fresh, 10) > rank(&rep, &fresh, 0));
        let mut good = RepTable::default();
        good.user.insert(k.pubkey, 8.0);
        assert!(rank(&good, &fresh, 0) > rank(&rep, &fresh, 0));
    }

    #[test]
    fn profile_of_sanitises() {
        let n = node();
        let k = Keys::generate();
        let content = serde_json::json!({"name": "n".repeat(500), "about": 5, "agent": "yes"}).to_string();
        n.ingest(k.sign(K_PROFILE, vec![], content), 1, false).unwrap();
        let st = n.store.lock().unwrap();
        let p = profile_of(&st, &k.pubkey);
        assert_eq!(p.name.chars().count(), 64);
        assert!(p.about.is_empty() && !p.agent);
        drop(st);
        let j = Keys::generate();
        n.ingest(j.sign(K_PROFILE, vec![], "not json".into()), 1, false).unwrap();
        assert_eq!(profile_of(&n.store.lock().unwrap(), &j.pubkey), Profile::default());
    }
}
