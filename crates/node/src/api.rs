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
use crate::node::CapScores;
use crate::rep::{self, Aspect, RepTable};
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
    let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Profile {
        name: s("name").chars().take(64).collect(),
        about: s("about"),
        agent: flag("agent") || flag("bot"), // NIP-24 `bot` from other nostr clients
        model: s("model").chars().take(100).collect(),
    }
}

/// Per-request context for building items: who is asking, and humanity scores.
pub struct Cx {
    pub me: H32,
    pub hum: Arc<RepTable>,
}
impl Cx {
    /// Build before locking the store (computing humanity locks it).
    fn of(node: &Arc<Node>) -> Cx {
        Cx { me: node.keys.pubkey, hum: node.humanity() }
    }
}

/// At or above this humanity a participant counts as a verified human; at or below
/// minus this, their claim is disputed.
pub const HUMAN_VERIFIED: f64 = 0.3;

fn human_status(st: &Store, hum: &RepTable, pk: &H32) -> &'static str {
    if profile_of(st, pk).agent {
        return "agent";
    }
    let h = hum.user(pk);
    if h >= HUMAN_VERIFIED {
        "verified"
    } else if h <= -HUMAN_VERIFIED {
        "disputed"
    } else {
        "unverified"
    }
}
fn name_of(st: &Store, pk: &H32) -> String {
    let n = profile_of(st, pk).name;
    if n.is_empty() { npub(pk)[..14].to_string() } else { n }
}

fn view(st: &Store, rep: &RepTable, e: &Event, full: bool, cx: &Cx) -> Item {
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
        author_model: profile_of(st, &e.pubkey).model,
        author_human: human_status(st, &cx.hum, &e.pubkey).into(),
        author_rep: round2(author_rep),
        score: round2(score),
        ratings: rep.event.get(&e.id).map(|x| x.1 as u32).unwrap_or(0),
        low_trust: author_rep < LOW_TRUST || score < LOW_TRUST,
        community: e.community().unwrap_or("").into(),
        title: e.tag("title").unwrap_or("").into(),
        content,
        created_at: e.created_at,
        parent: e.parent().filter(|_| e.kind == K_REPLY).map(|p| p.hex()).unwrap_or_default(),
        refs: e.tags_named("q").map(String::from).collect(),
        pinned: st.pinned.contains(&e.id),
        mine: e.pubkey == cx.me,
        reply_count: 0,
        replies: vec![],
        relevance: round2(rep.relevance(&e.id)),
        relevance_ratings: rep.relevance.get(&e.id).map(|x| x.1 as u32).unwrap_or(0),
        cited_by: st.cited_by(&e.id) as u32,
        popularity: round2(st.popularity(&e.id)),
        needs: e.tags_named("need").map(String::from).collect(),
        format: e.tag("m").unwrap_or(crate::body::MARKDOWN).to_string(),
        accept: e.tags_named("accept").map(String::from).collect(),
        blocks: if full && crate::body::is_markdown(e.tag("m").unwrap_or("")) { crate::body::blocks(&e.content) } else { vec![] },
        topic: e.topic_parent().filter(|_| e.kind == K_POST).map(|p| p.hex()).unwrap_or_default(),
        subtopics: if e.kind == K_POST { subtopics_of(st, &e.id).len() as u32 } else { 0 },
        root: e.root().filter(|_| e.kind == K_REPLY).map(|p| p.hex()).unwrap_or_default(),
        mentions: e
            .tags
            .iter()
            .filter(|t| t.len() >= 4 && t[0] == "p" && t[3] == "mention")
            .filter_map(|t| H32::from_hex(&t[1]))
            .map(|pk| Mention { pubkey: pk.hex(), npub: npub(&pk), name: name_of(st, &pk) })
            .collect(),
        text_match: 0.0,
    }
}

/// Below this (author reputation or message score) content is flagged low trust.
pub const LOW_TRUST: f64 = -0.3;

/// Feed and reply order: trust and relevance of the message, author standing
/// (each worth up to 10 points at full strength), discussion and citations,
/// minus a point per day of age.
/// Topics opened under `id`, oldest first.
fn subtopics_of(st: &Store, id: &H32) -> Vec<Ev> {
    let mut v: Vec<Ev> = st.referencing(id).into_iter().filter(|e| e.kind == K_POST && e.topic_parent() == Some(*id)).collect();
    v.sort_by_key(|e| (e.created_at, e.id));
    v.dedup_by_key(|e| e.id);
    v
}

fn rank(rep: &RepTable, e: &Event, replies: usize, cites: usize) -> f64 {
    let age_h = now().saturating_sub(e.created_at) as f64 / 3600.0;
    10.0 * (rep.event(&e.id) + rep.relevance(&e.id) + 0.5 * rep.user(&e.pubkey)) + (1.0 + replies as f64).ln() + 0.5 * (1.0 + cites as f64).ln() - age_h / 24.0
}

/// A score as it goes into a `score` tag: at most three decimals, no trailing zeros.
fn score_tag(s: f64) -> String {
    let t = format!("{s:.3}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.to_string() }
}
fn check_score(s: f64) -> R<()> {
    if s.is_finite() && (-1.0..=1.0).contains(&s) { Ok(()) } else { Err("score must be in -1..1".into()) }
}

fn tree(st: &Store, rep: &RepTable, e: &Ev, depth: usize, budget: &mut usize, cx: &Cx) -> Item {
    let mut v = view(st, rep, e, true, cx);
    let mut kids = st.children(&e.id);
    kids.sort_by(|a, b| rank(rep, b, 0, st.cited_by(&b.id)).total_cmp(&rank(rep, a, 0, st.cited_by(&a.id))));
    for k in kids {
        if *budget == 0 || depth > 64 {
            break;
        }
        *budget -= 1;
        v.replies.push(tree(st, rep, &k, depth + 1, budget, cx));
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
        unread: addressed_to_me(&st, &node.keys.pubkey, node.notifications_read.load(std::sync::atomic::Ordering::Relaxed), 1000).len() as u32,
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
    drop(st);
    let mut v: Vec<Community> = m
        .into_values()
        .map(|mut x| {
            x.joined = joined.contains(&x.slug);
            x.members = node.members(&x.slug) as u32;
            x
        })
        .collect();
    v.sort_by(|a, b| b.joined.cmp(&a.joined).then(b.events.cmp(&a.events)));
    Ok(CommunitiesResponse { communities: v })
}

/// Find communities by name or description, ranked by match, members and activity.
async fn find_communities(node: &Arc<Node>, r: FindCommunitiesRequest) -> R<CommunitiesResponse> {
    let q = r.query.trim().to_string();
    let limit = if r.limit == 0 { 30 } else { r.limit.min(200) } as usize;
    if r.network && !q.is_empty() {
        let f = Filter { kinds: Some(vec![K_COMMUNITY]), search: Some(q.clone()), limit: Some(50), ..Default::default() };
        node.remote_search(vec![f], 1).await;
    }
    let all = communities(node, Empty {}).await?.communities;
    let qt: Vec<String> = tokens(&q).collect();
    let matches: std::collections::HashMap<String, f64> = {
        let st = node.store.lock().unwrap();
        st.search(&q, 1000)
            .into_iter()
            .filter_map(|h| st.get(&h.id).filter(|e| e.kind == K_COMMUNITY).map(|e| (e.d_tag().to_string(), h.score * if h.all { 1.0 } else { 0.5 })))
            .collect()
    };
    let activity = |c: &Community| (1.0 + c.members as f64 + c.events as f64 / 10.0).ln();
    let mut v: Vec<(f64, Community)> = all
        .into_iter()
        .filter_map(|c| {
            if qt.is_empty() {
                return Some((activity(&c), c));
            }
            // slugs of communities nobody defined are matched by name only
            let slug_hit = qt.iter().any(|t| c.slug.contains(t.as_str())) as u8 as f64;
            let m = matches.get(&c.slug).copied().unwrap_or(0.0) + slug_hit;
            (m > 0.0).then(|| (m * (1.0 + 0.3 * activity(&c)), c))
        })
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.slug.cmp(&b.1.slug)));
    Ok(CommunitiesResponse { communities: v.into_iter().take(limit).map(|x| x.1).collect() })
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
    let c = match opt_slug(&r.community) {
        Some(c) => Some(slug(c)?),
        None => None,
    };
    let limit = if r.limit == 0 { 30 } else { r.limit.min(500) } as usize;
    let rep = node.reputation(c.as_deref().unwrap_or(""));
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let f = match &c {
        Some(c) => c_filter(Some(vec![K_POST]), c),
        None => Filter { kinds: Some(vec![K_POST]), ..Default::default() },
    };
    let mut items: Vec<(f64, Item)> = st
        .query(&f, 5000)
        .iter()
        .filter(|e| e.topic_parent().is_none()) // subtopics are listed under their topic
        .map(|e| {
            let n = st.descendant_count(&e.id, 10_000);
            let mut v = view(&st, &rep, e, false, &cx);
            v.reply_count = n as u32;
            (if r.sort == "new" { e.created_at as f64 } else { rank(&rep, e, n + v.subtopics as usize, v.cited_by as usize) }, v)
        })
        .collect();
    items.sort_by(|x, y| y.0.total_cmp(&x.0));
    Ok(Items { items: items.into_iter().take(limit).map(|x| x.1).collect() })
}

async fn thread(node: &Arc<Node>, r: IdRequest) -> R<ThreadResponse> {
    let id = hex_id(&r.id, "id")?;
    let root = get_or_fetch(node, &id).await.ok_or("event not found on this node or its peers")?;
    node.store.lock().unwrap().bump(&id, 1.0); // viewed
    if !node.peers.lock().unwrap().is_empty() {
        let f = Filter { tags: vec![("e".into(), vec![id.hex()])], limit: Some(5000), ..Default::default() };
        node.remote_fetch(vec![f], Duration::from_millis(1500)).await;
    }
    // enclosing topics: of this topic, or of the topic a reply belongs to
    let mut path: Vec<Ev> = vec![];
    let mut up = if root.kind == K_REPLY { root.root() } else { root.topic_parent() };
    if root.kind == K_REPLY && let Some(t) = up.and_then(|t| node.store.lock().unwrap().get(&t).cloned()) {
        path.push(t.clone());
        up = t.topic_parent();
    }
    while let Some(t) = up {
        if path.len() >= 12 || path.iter().any(|p| p.id == t) {
            break;
        }
        let Some(e) = get_or_fetch(node, &t).await.filter(|e| e.kind == K_POST) else { break };
        up = e.topic_parent();
        path.push(e);
    }
    path.reverse();
    let rep = node.reputation(root.community().unwrap_or(""));
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let mut budget = 5000;
    let idh = id.hex();
    Ok(ThreadResponse {
        root: Some(tree(&st, &rep, &root, 0, &mut budget, &cx)),
        quoted_by: st.referencing(&id).iter().filter(|e| e.tags_named("q").any(|q| q == idh)).map(|e| view(&st, &rep, e, false, &cx)).collect(),
        ref_events: root.tags_named("q").filter_map(H32::from_hex).filter_map(|r| st.get(&r).map(|e| view(&st, &rep, e, false, &cx))).collect(),
        thread_root: root.root().filter(|r| *r != root.id && root.kind == K_REPLY).map(|r| r.hex()).unwrap_or_default(),
        topic_path: path.iter().map(|e| view(&st, &rep, e, false, &cx)).collect(),
        subtopics: subtopics_of(&st, &root.id).iter().map(|e| view(&st, &rep, e, false, &cx)).collect(),
    })
}

async fn event(node: &Arc<Node>, r: IdRequest) -> R<EventResponse> {
    let id = hex_id(&r.id, "id")?;
    let e = get_or_fetch(node, &id).await.ok_or("event not found")?;
    node.store.lock().unwrap().bump(&id, 1.0); // viewed
    let rep = node.reputation(e.community().unwrap_or(""));
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    Ok(EventResponse { item: Some(view(&st, &rep, &e, true, &cx)), raw_json: serde_json::to_string(&*e).unwrap() })
}

/// The chain of reasoning around a post: walk up through reply parents and
/// citations (fetching missing links from peers), and down through replies and
/// posts that cite it, `depth` levels each way. Returns a small graph, oldest
/// first, and who contributed (humans and which models).
async fn chain(node: &Arc<Node>, r: ChainRequest) -> R<ChainResponse> {
    let id = hex_id(&r.id, "id")?;
    let depth = if r.depth == 0 { 4 } else { r.depth.min(8) } as i32;
    const MAX_NODES: usize = 300;
    let focus = get_or_fetch(node, &id).await.ok_or("event not found on this node or its peers")?;
    let is_step = |e: &Event| e.kind == K_POST || e.kind == K_REPLY;
    // what a post builds on: its parent (replies) and everything it cites
    let upstream = |e: &Event| -> Vec<(H32, &'static str)> {
        let mut v: Vec<(H32, &'static str)> = vec![];
        if e.kind == K_REPLY && let Some(p) = e.parent() {
            v.push((p, "reply"));
        }
        if e.kind == K_POST && let Some(p) = e.topic_parent() {
            v.push((p, "subtopic"));
        }
        v.extend(e.tags_named("q").filter_map(H32::from_hex).map(|q| (q, "cites")));
        v
    };
    let mut nodes: std::collections::HashMap<H32, (Ev, i32, &'static str)> = Default::default(); // id -> (event, level, relation found while walking)
    nodes.insert(id, (focus.clone(), 0, ""));
    // upstream, fetching links this node does not have (bounded)
    let mut frontier = vec![focus.clone()];
    let mut fetched = 0;
    for level in 1..=depth {
        let mut next = vec![];
        for e in &frontier {
            for (pid, rel) in upstream(e) {
                if nodes.contains_key(&pid) || nodes.len() >= MAX_NODES {
                    continue;
                }
                let local = node.store.lock().unwrap().get(&pid).cloned();
                let got = match local {
                    Some(x) => Some(x),
                    None if fetched < 20 => {
                        fetched += 1;
                        get_or_fetch(node, &pid).await
                    }
                    None => None,
                };
                if let Some(p) = got.filter(|p| is_step(p)) {
                    nodes.insert(pid, (p.clone(), -level, rel));
                    next.push(p);
                }
            }
        }
        frontier = next;
    }
    // downstream: replies and citing posts
    if !node.peers.lock().unwrap().is_empty() {
        let f = Filter { tags: vec![("e".into(), vec![id.hex()])], limit: Some(500), ..Default::default() };
        let q = Filter { tags: vec![("q".into(), vec![id.hex()])], limit: Some(500), ..Default::default() };
        node.remote_fetch(vec![f, q], Duration::from_millis(1500)).await;
    }
    {
        let st = node.store.lock().unwrap();
        let mut frontier = vec![focus.clone()];
        for level in 1..=depth {
            let mut next = vec![];
            for e in &frontier {
                let h = e.id.hex();
                for c in st.referencing(&e.id) {
                    if nodes.contains_key(&c.id) || nodes.len() >= MAX_NODES || !is_step(&c) {
                        continue;
                    }
                    let rel = if c.kind == K_REPLY && c.parent() == Some(e.id) {
                        "reply"
                    } else if c.kind == K_POST && c.topic_parent() == Some(e.id) {
                        "subtopic"
                    } else if c.tags_named("q").any(|q| q == h) {
                        "cites"
                    } else {
                        continue; // shares the thread root but builds on something else
                    };
                    nodes.insert(c.id, (c.clone(), level, rel));
                    next.push(c);
                }
            }
            frontier = next;
        }
    }
    let rep = node.reputation(focus.community().unwrap_or(""));
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let mut out: Vec<ChainNode> = nodes
        .values()
        .map(|(e, level, _)| {
            let up: Vec<(H32, &str)> = upstream(e).into_iter().filter(|(p, _)| nodes.contains_key(p)).collect();
            ChainNode {
                item: Some(view(&st, &rep, e, true, &cx)),
                builds_on: up.iter().map(|(p, _)| p.hex()).collect(),
                level: *level,
                // how this step joins the chain: replying to a step in it, or citing one
                relation: up.first().map(|(_, r)| r.to_string()).unwrap_or_default(),
            }
        })
        .collect();
    // oldest first; within the same second, what a step builds on comes before it
    out.sort_by_key(|n| (n.item.as_ref().map(|i| i.created_at), n.level, n.item.as_ref().map(|i| i.id.clone())));
    let mut contributors: Vec<Contributor> = vec![];
    for n in &out {
        let it = n.item.as_ref().unwrap();
        match contributors.iter_mut().find(|c| c.pubkey == it.author) {
            Some(c) => c.steps += 1,
            None => contributors.push(Contributor { pubkey: it.author.clone(), name: it.author_name.clone(), human_status: it.author_human.clone(), model: it.author_model.clone(), steps: 1 }),
        }
    }
    contributors.sort_by(|a, b| b.steps.cmp(&a.steps).then(a.name.cmp(&b.name)));
    Ok(ChainResponse { focus: id.hex(), nodes: out, contributors })
}

/// Full-text search. `scope`: "node" (this node's cache, default), "community"
/// (also ask peers and their peers, filtered to `community`), "network" (lossy
/// fan-out three levels deep). Found events are cached here, then everything is ranked locally:
/// text match x (1 + quality), where quality is relevance, trust, author
/// standing, citations and use.
async fn search(node: &Arc<Node>, r: SearchRequest) -> R<Items> {
    let q = r.query.trim().to_string();
    if q.is_empty() {
        return Err("missing 'query'".into());
    }
    if tokens(&q).next().is_none() {
        return Err("'query' needs a word of at least two letters that is not a stopword".into());
    }
    let c = match opt_slug(&r.community) {
        Some(c) => Some(slug(c)?),
        None => None,
    };
    let limit = if r.limit == 0 { 20 } else { r.limit.min(200) } as usize;
    let hops = match r.scope.trim() {
        "" | "node" => None,
        "community" if c.is_none() => return Err("scope 'community' needs 'community'".into()),
        "community" => Some(1),
        "network" => Some(2),
        other => return Err(format!("unknown scope '{other}': use node, community or network")),
    };
    if let Some(hops) = hops {
        let mut f = Filter { kinds: Some(vec![K_POST, K_REPLY]), search: Some(q.clone()), limit: Some(50), ..Default::default() };
        if let Some(c) = &c {
            f.tags = vec![("c".into(), vec![c.clone()])];
        }
        node.remote_search(vec![f], hops).await;
    }
    let hits: Vec<(Ev, f64)> = {
        let st = node.store.lock().unwrap();
        st.search(&q, 2000)
            .into_iter()
            .filter_map(|h| st.get(&h.id).map(|e| (e.clone(), h.score * if h.all { 1.0 } else { 0.4 })))
            .filter(|(e, _)| (e.kind == K_POST || e.kind == K_REPLY) && c.as_deref().is_none_or(|c| e.community() == Some(c)))
            .take(1000)
            .collect()
    };
    let mut reps: std::collections::HashMap<String, Arc<RepTable>> = Default::default();
    for (e, _) in &hits {
        let cm = e.community().unwrap_or("").to_string();
        if let std::collections::hash_map::Entry::Vacant(v) = reps.entry(cm) {
            let t = node.reputation(v.key());
            v.insert(t);
        }
    }
    let cx = Cx::of(node);
    let mut st = node.store.lock().unwrap();
    let mut items: Vec<(f64, Item)> = hits
        .iter()
        .map(|(e, m)| {
            let rep = &reps[e.community().unwrap_or("")];
            let mut v = view(&st, rep, e, false, &cx);
            let quality = (v.relevance + v.score + 0.5 * v.author_rep + 0.3 * (1.0 + v.cited_by as f64).ln() + 0.2 * (1.0 + v.popularity).ln()).max(0.0);
            let score = m * (1.0 + quality) * if v.low_trust { 0.5 } else { 1.0 };
            v.text_match = round2(*m);
            (score, v)
        })
        .collect();
    items.sort_by(|a, b| b.0.total_cmp(&a.0));
    items.truncate(limit);
    for (_, it) in &items {
        if let Some(id) = H32::from_hex(&it.id) {
            st.bump(&id, 0.2); // found by a search
        }
    }
    Ok(Items { items: items.into_iter().map(|x| x.1).collect() })
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
        let root = if parent.kind == K_POST { pid } else { parent.root().unwrap_or(pid) };
        // replies stay in their topic's community; independent topics have none
        if let Some(c) = opt_slug(&r.community).or(parent.community()) {
            tags.push(t("c", slug(c)?));
        }
        tags.push(vec!["e".into(), root.hex(), "".into(), "root".into()]);
        tags.push(vec!["e".into(), pid.hex(), "".into(), "reply".into()]);
        tags.push(t("p", parent.pubkey.hex()));
        kind = K_REPLY;
    } else {
        // a topic: under another topic (a subtopic, inheriting its community), in a
        // community, or on its own
        let parent_topic = match opt_slug(&r.topic) {
            Some(tp) => {
                let tid = hex_id(tp, "topic")?;
                let topic = get_or_fetch(node, &tid).await.ok_or("topic not found")?;
                if topic.kind != K_POST {
                    return Err("'topic' must be a topic (a post), not a reply".into());
                }
                if r.title.trim().is_empty() {
                    return Err("a subtopic needs a 'title'".into());
                }
                Some(topic)
            }
            None => None,
        };
        let community = opt_slug(&r.community).map(String::from).or_else(|| parent_topic.as_ref().and_then(|t| t.community().map(String::from)));
        if let Some(c) = community {
            tags.push(t("c", slug(&c)?));
        }
        if !r.title.trim().is_empty() {
            tags.push(t("title", r.title.trim().chars().take(200).collect::<String>()));
        }
        if let Some(tp) = &parent_topic {
            tags.push(vec!["e".into(), tp.id.hex(), "".into(), "topic".into()]);
            tags.push(t("p", tp.pubkey.hex()));
        }
        kind = K_POST;
    }
    for x in r.refs.iter().take(50) {
        tags.push(t("q", hex_id(x, "refs")?.hex()));
    }
    for n in r.needs.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).take(10) {
        tags.push(t("need", n.chars().take(100).collect::<String>()));
    }
    // body format (default markdown, left implicit) and the formats replies should use
    let format = r.format.trim();
    if !format.is_empty() && format != crate::body::MARKDOWN {
        if !crate::body::valid_media_type(format) {
            return Err(format!("'format' must be a media type such as text/markdown or application/json, not '{format}'"));
        }
        tags.push(t("m", format));
    }
    for a in r.accept.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).take(5) {
        if !crate::body::valid_media_type(a) {
            return Err(format!("'accept' entries must be media types, not '{a}'"));
        }
        tags.push(t("accept", a));
    }
    let (content, mentioned, unresolved) = resolve_mentions(&node.store.lock().unwrap(), &node.keys.pubkey, &r.content);
    for pk in &mentioned {
        tags.push(vec!["p".into(), pk.hex(), "".into(), "mention".into()]);
    }
    // NIP-32 labels, so scoped nodes can subscribe to them (`#l`): mentions, and posts
    // written from outside the community (e.g. answering a mention), addressed to the
    // people they reply to; both travel through every node to reach them
    let outsider = !node.cfg.relay_all
        && tags.iter().any(|x| x[0] == "p")
        && tags.iter().find(|x| x[0] == "c").is_none_or(|c| !node.joined.lock().unwrap().contains(&c[1]));
    if !mentioned.is_empty() || outsider {
        tags.push(t("L", "sametra"));
        tags.push(vec!["l".into(), if mentioned.is_empty() { "addressed" } else { "mention" }.into(), "sametra".into()]);
    }
    let e = node.publish(kind, tags, content)?;
    Ok(Ack { mentioned: mentioned.iter().map(npub).collect(), unresolved, ..ack(Some(&e.id)) })
}

// ---- people and mentions ------------------------------------------------------

/// The @handle for a name: letters and digits only, so "Doctor Argon" is @DoctorArgon.
pub fn handle_of(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect()
}

/// Rewrite `@handle`, `@npub1…` and bare `nostr:npub1…` into NIP-27 `nostr:npub1…`
/// references and collect who was mentioned (at most 10, never yourself). A handle
/// matches a profile name ignoring case, spaces and punctuation; if several match,
/// the most recently seen wins (the post's `p` tag records exactly who was meant).
fn resolve_mentions(st: &Store, me: &H32, text: &str) -> (String, Vec<H32>, Vec<String>) {
    let mut people: std::collections::HashMap<String, Vec<(H32, u64)>> = Default::default();
    let mut out = String::with_capacity(text.len());
    let (mut mentioned, mut unresolved): (Vec<H32>, Vec<String>) = (vec![], vec![]);
    let add = |pk: H32, mentioned: &mut Vec<H32>| {
        if pk != *me && !mentioned.contains(&pk) && mentioned.len() < 10 {
            mentioned.push(pk);
        }
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_code = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code; // never rewrite inside code
        }
        let starts_word = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '.');
        if !in_code && c == '@' && starts_word {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') && j - i <= 64 {
                j += 1;
            }
            let handle: String = chars[i + 1..j].iter().collect();
            if !handle.is_empty() {
                if let Some(pk) = handle.starts_with("npub1").then(|| parse_pubkey(&handle)).flatten() {
                    add(pk, &mut mentioned);
                    out.push_str(&format!("nostr:{}", npub(&pk)));
                    i = j;
                    continue;
                }
                if people.is_empty() {
                    for p in st.query(&Filter { kinds: Some(vec![K_PROFILE]), ..Default::default() }, usize::MAX) {
                        let h = handle_of(&profile_of(st, &p.pubkey).name).to_lowercase();
                        if !h.is_empty() {
                            people.entry(h).or_default().push((p.pubkey, st.last_seen.get(&p.pubkey).copied().unwrap_or(0)));
                        }
                    }
                    people.entry(String::new()).or_default(); // loaded, even if nobody has a profile
                }
                match people.get(&handle.to_lowercase()).and_then(|v| v.iter().max_by_key(|x| (x.1, x.0))) {
                    Some((pk, _)) => {
                        add(*pk, &mut mentioned);
                        out.push_str(&format!("nostr:{}", npub(pk)));
                    }
                    None => {
                        if !unresolved.contains(&handle) {
                            unresolved.push(handle.clone());
                        }
                        out.push('@');
                        out.push_str(&handle);
                    }
                }
                i = j;
                continue;
            }
        }
        if !in_code && c == 'n' && chars[i..].iter().take(11).collect::<String>() == "nostr:npub1" {
            let tok: String = chars[i + 6..].iter().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if let Some(pk) = parse_pubkey(&tok) {
                add(pk, &mut mentioned);
            }
        }
        out.push(c);
        i += 1;
    }
    (out, mentioned, unresolved)
}

/// Find people (humans and agents) by name, description or model.
async fn find_people(node: &Arc<Node>, r: FindPeopleRequest) -> R<PeopleResponse> {
    let q = r.query.trim().trim_start_matches('@').to_string();
    let limit = if r.limit == 0 { 20 } else { r.limit.min(200) } as usize;
    let hum = node.humanity();
    let st = node.store.lock().unwrap();
    let matches: std::collections::HashMap<H32, f64> = if q.is_empty() {
        Default::default()
    } else {
        st.search(&q, 2000).into_iter().filter_map(|h| st.get(&h.id).filter(|e| e.kind == K_PROFILE).map(|e| (e.pubkey, h.score * if h.all { 1.0 } else { 0.4 }))).collect()
    };
    let caps: std::collections::HashMap<H32, u32> = st.query(&Filter { kinds: Some(vec![K_CAPABILITY]), ..Default::default() }, usize::MAX).iter().filter(|e| spec_of(e).is_some()).fold(Default::default(), |mut m, e| {
        *m.entry(e.pubkey).or_default() += 1;
        m
    });
    let qh = handle_of(&q).to_lowercase();
    let now = now();
    let mut out: Vec<(f64, Person)> = vec![];
    for e in st.query(&Filter { kinds: Some(vec![K_PROFILE]), ..Default::default() }, usize::MAX) {
        let p = profile_of(&st, &e.pubkey);
        if (r.kind == "human" && p.agent) || (r.kind == "agent" && !p.agent) {
            continue;
        }
        let handle = handle_of(&p.name);
        let exact = !qh.is_empty() && handle.to_lowercase() == qh;
        let prefix = !qh.is_empty() && handle.to_lowercase().starts_with(&qh);
        let m = matches.get(&e.pubkey).copied().unwrap_or(0.0) + if exact { 10.0 } else if prefix { 3.0 } else { 0.0 };
        if !q.is_empty() && m == 0.0 {
            continue;
        }
        let seen = st.last_seen.get(&e.pubkey).copied().unwrap_or(0);
        let recent = if now.saturating_sub(seen) < 86400 { 0.3 } else { 0.0 };
        let c = caps.get(&e.pubkey).copied().unwrap_or(0);
        let quality = 1.0 + hum.user(&e.pubkey).max(0.0) + recent + 0.2 * (1.0 + c as f64).ln();
        out.push((
            if q.is_empty() { seen as f64 } else { m * quality },
            Person {
                pubkey: e.pubkey.hex(),
                npub: npub(&e.pubkey),
                about: p.about.chars().take(280).collect(),
                agent: p.agent,
                model: p.model.clone(),
                human_status: human_status(&st, &hum, &e.pubkey).into(),
                humanity: round2(hum.user(&e.pubkey)),
                last_seen: seen,
                capabilities: c,
                handle,
                name: p.name,
                text_match: round2(m),
            },
        ));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.pubkey.cmp(&b.1.pubkey)));
    Ok(PeopleResponse { people: out.into_iter().take(limit).map(|x| x.1).collect() })
}

/// Why an event concerns me, if it does (and is not mine).
fn notification_reason(me: &H32, mine: &std::collections::HashSet<H32>, e: &Event) -> Option<&'static str> {
    if e.pubkey == *me {
        return None;
    }
    let to_me = |marker: Option<&str>| e.tags.iter().any(|t| t.len() >= 2 && t[0] == "p" && H32::from_hex(&t[1]) == Some(*me) && marker.is_none_or(|m| t.get(3).map(String::as_str) == Some(m)));
    if to_me(Some("mention")) {
        return Some("mention");
    }
    match e.kind {
        K_JOB if to_me(None) => Some("request"),
        K_REPLY if e.parent().is_some_and(|p| mine.contains(&p)) => Some("reply"),
        K_POST if e.topic_parent().is_some_and(|p| mine.contains(&p)) => Some("subtopic"),
        K_POST | K_REPLY if e.tags_named("q").filter_map(H32::from_hex).any(|q| mine.contains(&q)) => Some("citation"),
        _ => None,
    }
}

/// Events addressed to me newer than `since`, newest first.
fn addressed_to_me(st: &Store, me: &H32, since: u64, cap: usize) -> Vec<(&'static str, Ev)> {
    let mine: std::collections::HashSet<H32> = st
        .query(&Filter { authors: Some(vec![me.hex()]), kinds: Some(vec![K_POST, K_REPLY]), ..Default::default() }, usize::MAX)
        .iter()
        .map(|e| e.id)
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    // things that tag me, and things that reply to, cite or extend my posts
    let mut cands: Vec<Ev> = st.query(&Filter { tags: vec![("p".into(), vec![me.hex()])], since: Some(since + 1), ..Default::default() }, usize::MAX);
    for id in &mine {
        cands.extend(st.referencing(id).into_iter().filter(|e| e.created_at > since));
    }
    cands.sort_by_key(|e| std::cmp::Reverse((e.created_at, e.id)));
    for e in cands {
        if !seen.insert(e.id) {
            continue;
        }
        if let Some(why) = notification_reason(me, &mine, &e) {
            out.push((why, e));
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// What concerns me: mentions, replies, subtopics, citations, capability requests.
async fn notifications(node: &Arc<Node>, r: NotificationsRequest) -> R<NotificationsResponse> {
    let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as usize;
    let read = node.notifications_read.load(std::sync::atomic::Ordering::Relaxed);
    let cx = Cx::of(node);
    let (out, newest) = {
        let st = node.store.lock().unwrap();
        let all = addressed_to_me(&st, &node.keys.pubkey, 0, limit);
        let unread = all.iter().filter(|(_, e)| e.created_at > read).count() as u32;
        let newest = all.iter().map(|(_, e)| e.created_at).max().unwrap_or(0);
        let mut reps: std::collections::HashMap<String, Arc<RepTable>> = Default::default();
        let mut list = vec![];
        for (why, e) in &all {
            let c = e.community().unwrap_or("").to_string();
            let rep = reps.entry(c.clone()).or_insert_with(|| Arc::new(RepTable::default())).clone();
            list.push(Notification { reason: why.to_string(), item: Some(view(&st, &rep, e, false, &cx)), unread: e.created_at > read });
        }
        (NotificationsResponse { notifications: list, unread }, newest)
    };
    if r.mark_read && newest > read {
        node.notifications_read.store(newest, std::sync::atomic::Ordering::Relaxed);
        node.save_state();
    }
    Ok(out)
}

/// Rate a person or a message. Aspects: "trust" (default; accuracy, feeds the
/// author's reputation) or "relevance" (how useful this message is here; scores
/// only the message).
async fn rate(node: &Arc<Node>, r: RateRequest) -> R<Ack> {
    check_score(r.score)?;
    let aspect = match r.aspect.trim() {
        "" | "trust" => "trust",
        "relevance" => "relevance",
        "capability" => return rate_capability(node, r).await,
        "human" => return rate_human(node, r).await,
        other => return Err(format!("unknown aspect '{other}': use trust, relevance, human or capability")),
    };
    if aspect == "relevance" && opt_slug(&r.event).is_none() {
        return Err("relevance rates a message: give 'event'".into());
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
    // trust keeps the original slot layout; relevance gets its own slot per message
    let mut d = format!("{c}:{}:{}", target.hex(), ev.map(|e| e.hex()).unwrap_or_default());
    let mut tags = vec![t("c", &c), t("p", target.hex()), t("score", score_tag(r.score))];
    if aspect == "relevance" {
        d.push_str(":relevance");
        tags.push(t("aspect", "relevance"));
    }
    tags.insert(0, t("d", d));
    if let Some(e) = ev {
        tags.push(t("e", e.hex()));
    }
    let e = node.publish(K_RATING, tags, r.reason)?;
    let table = node.reputation(&c);
    // Ack.rep: the person's reputation for trust ratings, the message's relevance otherwise
    let rep = if aspect == "relevance" { table.relevance(&ev.unwrap()) } else { table.user(&target) };
    Ok(Ack { rep: round2(rep), community: c, ..ack(Some(&e.id)) })
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
    let hum = node.humanity();
    let st = node.store.lock().unwrap();
    Ok(ProfileResponse {
        pubkey: pk.hex(),
        npub: npub(&pk),
        profile: Some(profile_of(&st, &pk)),
        reputation,
        last_seen: st.last_seen.get(&pk).copied().unwrap_or(0),
        humanity: round2(hum.user(&pk)),
        humanity_raters: hum.raters.get(&pk).copied().unwrap_or(0) as u32,
        human_status: human_status(&st, &hum, &pk).into(),
    })
}

async fn set_profile(node: &Arc<Node>, p: Profile) -> R<Ack> {
    if p.name.trim().is_empty() {
        return Err("missing 'name'".into());
    }
    // kind 0 content stays NIP-01 JSON so other nostr clients can read it
    let content = serde_json::json!({
        "name": p.name.trim(),
        "about": p.about,
        "agent": p.agent,
        "bot": p.agent, // NIP-24, so other nostr clients see it too
        "model": if p.agent { p.model.trim().chars().take(100).collect::<String>() } else { String::new() },
    })
    .to_string();
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
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let empty = RepTable::default();
    Ok(Items { items: evs.iter().take(500).map(|e| view(&st, &empty, e, false, &cx)).collect() })
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

// ---- capabilities (NIP-89 announcements, NIP-90 jobs) -------------------------

/// A live (not withdrawn) capability announcement.
fn spec_of(e: &Event) -> Option<CapabilitySpec> {
    if e.kind != K_CAPABILITY || e.tag("status") == Some("withdrawn") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&e.content).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(CapabilitySpec {
        id: e.d_tag().to_string(),
        name: s("name"),
        about: s("about"),
        kind: s("kind"),
        input_schema: v.get("input_schema").filter(|x| !x.is_null()).map(|x| x.to_string()).unwrap_or_default(),
        topics: e.tags_named("t").map(String::from).collect(),
        communities: e.tags_named("c").map(String::from).collect(),
        delete: false,
    })
}

/// `["param", name, value]` (NIP-90).
fn param<'a>(e: &'a Event, name: &str) -> Option<&'a str> {
    e.tags.iter().find(|t| t.len() >= 3 && t[0] == "param" && t[1] == name).map(|t| t[2].as_str())
}
/// `["i", data, type]` inputs (NIP-90).
fn input<'a>(e: &'a Event, ty: &str) -> Option<&'a str> {
    e.tags.iter().find(|t| t.len() >= 3 && t[0] == "i" && t[2] == ty).map(|t| t[1].as_str())
}

struct JobState {
    status: &'static str,
    text: String,
    result: Option<Ev>,
    updated: u64,
}

/// Status of a job request from the provider's feedback and results.
fn job_state(st: &Store, req: &Event) -> JobState {
    let provider = req.tag("p").and_then(H32::from_hex);
    let from_provider: Vec<Ev> = st.referencing(&req.id).into_iter().filter(|e| Some(e.pubkey) == provider).collect();
    let latest = |k: u32| from_provider.iter().filter(|e| e.kind == k).max_by_key(|e| (e.created_at, e.id)).cloned();
    if let Some(res) = latest(K_JOB_RESULT) {
        return JobState { status: "done", text: String::new(), updated: res.created_at, result: Some(res) };
    }
    let Some(fb) = latest(K_JOB_FEEDBACK) else {
        return JobState { status: "pending", text: String::new(), result: None, updated: req.created_at };
    };
    let tag = fb.tags.iter().find(|t| t.len() >= 2 && t[0] == "status");
    let extra = tag.and_then(|t| t.get(2)).map(String::as_str).unwrap_or("");
    let status = match tag.map(|t| t[1].as_str()) {
        Some("processing") | Some("partial") => "accepted",
        Some("error") if extra.starts_with("declined") => "declined",
        Some("error") => "error",
        _ => "pending",
    };
    JobState { status, text: fb.content.clone(), result: None, updated: fb.created_at }
}

#[derive(Default, Clone, Copy)]
struct CapStats {
    requests: u32,
    completed: u32,
    declined: u32,
    response_secs: u64,
}

fn cap_stats(st: &Store) -> std::collections::HashMap<(H32, String), CapStats> {
    let mut out: std::collections::HashMap<(H32, String), CapStats> = Default::default();
    for req in st.query(&Filter { kinds: Some(vec![K_JOB]), ..Default::default() }, usize::MAX) {
        let (Some(p), Some(cap)) = (req.tag("p").and_then(H32::from_hex), param(&req, "capability")) else { continue };
        let s = out.entry((p, cap.to_string())).or_default();
        s.requests += 1;
        let j = job_state(st, &req);
        match j.status {
            "done" => {
                s.completed += 1;
                s.response_secs += j.updated.saturating_sub(req.created_at);
            }
            "declined" => s.declined += 1,
            _ => {}
        }
    }
    out
}

/// Capability reputation for every (provider, capability), cached until a new
/// rating or result arrives. A rating counts only if it cites a result that
/// provider delivered to the rater for that capability; it is weighted by the
/// rater's reputation in the community the request was made in (1 if none).
fn cap_scores(node: &Arc<Node>) -> Arc<CapScores> {
    if let Some(c) = node.cap_cache.lock().unwrap().clone() {
        return c;
    }
    // result id -> (provider, requester, capability, community of the request)
    type Ctx = (H32, Option<H32>, String, Option<String>);
    let (ratings, ctx) = {
        let st = node.store.lock().unwrap();
        let ratings: Vec<Ev> = st
            .query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, usize::MAX)
            .into_iter()
            .filter(|e| e.tag("aspect") == Some("capability"))
            .collect();
        let mut ctx: std::collections::HashMap<H32, Ctx> = Default::default();
        for rt in &ratings {
            let Some(rid) = rt.tag("e").and_then(H32::from_hex) else { continue };
            let Some(res) = st.get(&rid).filter(|e| e.kind == K_JOB_RESULT) else { continue };
            let req = res.tag("e").and_then(H32::from_hex).and_then(|i| st.get(&i).cloned());
            let cap = param(res, "capability").or(req.as_deref().and_then(|q| param(q, "capability"))).unwrap_or("").to_string();
            let comm = req.as_deref().and_then(|q| q.community()).map(String::from);
            ctx.insert(rid, (res.pubkey, res.tag("p").and_then(H32::from_hex), cap, comm));
        }
        (ratings, ctx)
    };
    let mut reps: std::collections::HashMap<String, Arc<RepTable>> = Default::default();
    for (_, _, _, c) in ctx.values() {
        if let Some(c) = c
            && !reps.contains_key(c)
        {
            reps.insert(c.clone(), node.reputation(c));
        }
    }
    let ctx_of = |r: &rep::Rating| r.event.and_then(|e| ctx.get(&e));
    let table = Arc::new(rep::capability_scores(
        &ratings,
        |r| ctx_of(r).is_some_and(|(prov, req, cap, _)| *prov == r.target && *req == Some(r.rater) && r.aspect == Aspect::Capability(cap.clone())),
        |r| ctx_of(r).and_then(|c| c.3.as_ref()).map(|c| rep::weight(reps[c].user(&r.rater))).unwrap_or(1.0),
    ));
    *node.cap_cache.lock().unwrap() = Some(table.clone());
    table
}

/// Publish (or withdraw) a capability: a NIP-89 announcement, replaceable per id.
async fn set_capability(node: &Arc<Node>, c: CapabilitySpec) -> R<Ack> {
    let id = slug(&c.id).map_err(|_| "capability 'id' must be 1-64 chars of a-z 0-9 - _ .".to_string())?;
    let mut tags = vec![t("d", &id), t("k", K_JOB.to_string())];
    if c.delete {
        tags.push(t("status", "withdrawn"));
        let e = node.publish(K_CAPABILITY, tags, "{}".into())?;
        return Ok(ack(Some(&e.id)));
    }
    if c.name.trim().is_empty() {
        return Err("missing 'name'".into());
    }
    let kind = match c.kind.trim() {
        "tool" => "tool",
        "skill" | "" => "skill",
        other => return Err(format!("'kind' must be tool or skill, not '{other}'")),
    };
    let schema = match c.input_schema.trim() {
        "" => serde_json::Value::Null,
        s => {
            let v: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("'input_schema' is not JSON: {e}"))?;
            if !v.is_object() {
                return Err("'input_schema' must be a JSON Schema object".into());
            }
            v
        }
    };
    for tp in c.topics.iter().map(|x| x.trim()).filter(|x| !x.is_empty()).take(20) {
        tags.push(t("t", tp.to_lowercase().chars().take(50).collect::<String>()));
    }
    for cm in c.communities.iter().filter(|x| !x.trim().is_empty()).take(10) {
        tags.push(t("c", slug(cm)?));
    }
    let content = serde_json::json!({
        "name": c.name.trim().chars().take(100).collect::<String>(),
        "about": c.about.chars().take(4000).collect::<String>(),
        "kind": kind,
        "input_schema": schema,
    });
    let e = node.publish(K_CAPABILITY, tags, content.to_string())?;
    Ok(ack(Some(&e.id)))
}

/// Find capabilities: by need (free text), provider, community or kind.
async fn capabilities(node: &Arc<Node>, r: CapabilitiesRequest) -> R<CapabilitiesResponse> {
    let q = r.query.trim().to_string();
    let provider = match opt_slug(&r.provider) {
        Some(p) => Some(parse_pubkey(p).ok_or("bad 'provider' pubkey")?),
        None => None,
    };
    let community = opt_slug(&r.community).map(slug).transpose()?;
    let limit = if r.limit == 0 { 30 } else { r.limit.min(200) } as usize;
    if r.network {
        if !q.is_empty() {
            let f = Filter { kinds: Some(vec![K_CAPABILITY]), search: Some(q.clone()), limit: Some(100), ..Default::default() };
            node.remote_search(vec![f], 1).await;
        } else if let Some(p) = provider {
            let f = Filter { kinds: Some(vec![K_CAPABILITY]), authors: Some(vec![p.hex()]), ..Default::default() };
            node.remote_fetch(vec![f], Duration::from_secs(3)).await;
        }
    }
    let scores = cap_scores(node);
    let st = node.store.lock().unwrap();
    let matches: std::collections::HashMap<H32, f64> = if q.is_empty() {
        Default::default()
    } else {
        st.search(&q, 2000).into_iter().map(|h| (h.id, h.score * if h.all { 1.0 } else { 0.4 })).collect()
    };
    let stats = cap_stats(&st);
    let mut out: Vec<(f64, Capability)> = vec![];
    for e in st.query(&Filter { kinds: Some(vec![K_CAPABILITY]), ..Default::default() }, usize::MAX) {
        let Some(spec) = spec_of(&e) else { continue };
        if provider.is_some_and(|p| p != e.pubkey)
            || community.as_ref().is_some_and(|c| !spec.communities.contains(c))
            || (!r.kind.trim().is_empty() && spec.kind != r.kind.trim())
        {
            continue;
        }
        let m = matches.get(&e.id).copied().unwrap_or(0.0);
        if !q.is_empty() && m == 0.0 {
            continue;
        }
        let key = (e.pubkey, spec.id.clone());
        let (rep, raters) = scores.get(&key).copied().unwrap_or((0.0, 0));
        let s = stats.get(&key).copied().unwrap_or_default();
        let profile = profile_of(&st, &e.pubkey);
        let quality = 1.0 + rep.max(-0.9) + (1.0 + s.completed as f64).ln() / 4.0;
        let rank = if q.is_empty() { quality } else { m * quality };
        out.push((
            rank,
            Capability {
                provider: e.pubkey.hex(),
                provider_name: name_of(&st, &e.pubkey),
                provider_is_agent: profile.agent,
                provider_model: profile.model.clone(),
                spec: Some(spec),
                rep: round2(rep),
                ratings: raters as u32,
                requests: s.requests,
                completed: s.completed,
                declined: s.declined,
                avg_response_secs: if s.completed > 0 { (s.response_secs / s.completed as u64) as u32 } else { 0 },
                updated_at: e.created_at,
                text_match: round2(m),
            },
        ));
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
    Ok(CapabilitiesResponse { capabilities: out.into_iter().take(limit).map(|x| x.1).collect() })
}

/// Ask a provider to use one of their capabilities: a public NIP-90 job request.
async fn request(node: &Arc<Node>, r: JobRequest) -> R<Ack> {
    let provider = parse_pubkey(&r.provider).ok_or("'provider' must be a pubkey or npub")?;
    if provider == node.keys.pubkey {
        return Err("that is your own capability".into());
    }
    let cap = r.capability.trim().to_string();
    let lookup = |node: &Arc<Node>| node.store.lock().unwrap().replaceable(&provider, K_CAPABILITY, &cap).cloned();
    let ann = match lookup(node) {
        Some(a) => Some(a),
        None => {
            let f = Filter { kinds: Some(vec![K_CAPABILITY]), authors: Some(vec![provider.hex()]), tags: vec![("d".into(), vec![cap.clone()])], ..Default::default() };
            node.remote_fetch(vec![f], Duration::from_secs(3)).await;
            lookup(node)
        }
    };
    let spec = ann.as_deref().and_then(spec_of).ok_or_else(|| format!("{} offers no capability '{cap}'", npub(&provider)))?;
    let input_text = r.input.trim();
    if spec.kind == "tool" {
        let v: serde_json::Value = serde_json::from_str(if input_text.is_empty() { "{}" } else { input_text }).map_err(|e| format!("'{cap}' is a tool: 'input' must be JSON ({e})"))?;
        if let Ok(schema) = serde_json::from_str::<serde_json::Value>(&spec.input_schema) {
            for k in schema.get("required").and_then(|x| x.as_array()).into_iter().flatten().filter_map(|k| k.as_str()) {
                if v.get(k).is_none() {
                    return Err(format!("'{cap}' requires input field '{k}'"));
                }
            }
        }
    } else if input_text.is_empty() {
        return Err("describe what you need in 'input'".into());
    }
    if r.input.len() > 32 * 1024 {
        return Err("'input' is limited to 32 KiB".into());
    }
    let mut tags = vec![t("p", provider.hex()), vec!["param".into(), "capability".into(), cap.clone()], vec!["i".into(), r.input.clone(), "text".into()]];
    let mut community = opt_slug(&r.community).map(slug).transpose()?;
    if let Some(p) = opt_slug(&r.post) {
        let pid = hex_id(p, "post")?;
        let post = get_or_fetch(node, &pid).await.ok_or("'post' not found")?;
        if community.is_none() {
            community = post.community().map(String::from);
        }
        tags.push(vec!["i".into(), pid.hex(), "event".into()]);
    }
    if let Some(c) = community {
        tags.push(t("c", c));
    }
    let e = node.publish(K_JOB, tags, String::new())?;
    Ok(ack(Some(&e.id)))
}

/// Requests to me (role "provider"), my requests ("requester"), or both.
async fn inbox(node: &Arc<Node>, r: InboxRequest) -> R<JobsResponse> {
    let me = node.keys.pubkey;
    let role = r.role.trim();
    if !matches!(role, "" | "provider" | "requester") {
        return Err("'role' must be provider or requester".into());
    }
    let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as usize;
    let st = node.store.lock().unwrap();
    let rated: std::collections::HashSet<String> = st
        .query(&Filter { kinds: Some(vec![K_RATING]), authors: Some(vec![me.hex()]), ..Default::default() }, usize::MAX)
        .iter()
        .filter(|e| e.tag("aspect") == Some("capability"))
        .filter_map(|e| e.tag("e").map(String::from))
        .collect();
    let mut jobs = vec![];
    for req in st.query(&Filter { kinds: Some(vec![K_JOB]), ..Default::default() }, usize::MAX) {
        let Some(provider) = req.tag("p").and_then(H32::from_hex) else { continue };
        let mine = req.pubkey == me;
        let to_me = provider == me;
        if !(mine && role != "provider" || to_me && role != "requester") {
            continue;
        }
        let j = job_state(&st, &req);
        if !r.status.trim().is_empty() && j.status != r.status.trim() {
            continue;
        }
        let result_id = j.result.as_ref().map(|e| e.id.hex()).unwrap_or_default();
        jobs.push(Job {
            id: req.id.hex(),
            requester: req.pubkey.hex(),
            requester_name: name_of(&st, &req.pubkey),
            provider: provider.hex(),
            provider_name: name_of(&st, &provider),
            capability: param(&req, "capability").unwrap_or("").into(),
            input: input(&req, "text").unwrap_or("").into(),
            post: input(&req, "event").unwrap_or("").into(),
            community: req.community().unwrap_or("").into(),
            created_at: req.created_at,
            status: j.status.into(),
            status_text: j.text,
            result: j.result.as_ref().map(|e| e.content.clone()).unwrap_or_default(),
            rated: !result_id.is_empty() && rated.contains(&result_id),
            result_id,
            updated_at: j.updated,
            mine,
        });
    }
    jobs.sort_by_key(|j| std::cmp::Reverse(j.updated_at));
    jobs.truncate(limit);
    Ok(JobsResponse { jobs })
}

/// The provider answers a request: accept, decline, deliver a result, or report an error.
async fn respond(node: &Arc<Node>, r: RespondRequest) -> R<Ack> {
    let id = hex_id(&r.request, "request")?;
    let req = get_or_fetch(node, &id).await.ok_or("request not found")?;
    if req.kind != K_JOB || req.tag("p").and_then(H32::from_hex) != Some(node.keys.pubkey) {
        return Err("that is not a request addressed to you".into());
    }
    let base = vec![t("e", id.hex()), t("p", req.pubkey.hex())];
    let status = |s: &str, extra: &str| {
        let mut tags = vec![vec!["status".to_string(), s.to_string(), extra.to_string()]];
        tags.extend(base.clone());
        tags
    };
    let e = match r.action.trim() {
        "accept" => node.publish(K_JOB_FEEDBACK, status("processing", "accepted"), r.content)?,
        "decline" => node.publish(K_JOB_FEEDBACK, status("error", "declined"), r.content)?,
        "error" => node.publish(K_JOB_FEEDBACK, status("error", "failed"), r.content)?,
        "result" => {
            if r.content.trim().is_empty() {
                return Err("a result needs 'content'".into());
            }
            let mut tags = base.clone();
            tags.push(t("request", serde_json::to_string(&*req).unwrap()));
            if let Some(cap) = param(&req, "capability") {
                tags.push(vec!["param".into(), "capability".into(), cap.into()]);
            }
            if let Some(c) = req.community() {
                tags.push(t("c", c));
            }
            node.publish(K_JOB_RESULT, tags, r.content)?
        }
        other => return Err(format!("unknown action '{other}': use accept, decline, result or error")),
    };
    Ok(ack(Some(&e.id)))
}

/// Vouch (or not) that a participant is a human: network-wide, not per community.
async fn rate_human(node: &Arc<Node>, r: RateRequest) -> R<Ack> {
    if opt_slug(&r.event).is_some() {
        return Err("humanity rates a person: give 'target', not 'event'".into());
    }
    let target = parse_pubkey(&r.target).ok_or("give 'target' (pubkey/npub) of the person")?;
    if target == node.keys.pubkey {
        return Err("you cannot rate yourself".into());
    }
    let tags = vec![t("d", format!("human:{}", target.hex())), t("p", target.hex()), t("score", score_tag(r.score)), t("aspect", "human")];
    let e = node.publish(K_RATING, tags, r.reason)?;
    Ok(Ack { rep: round2(node.humanity().user(&target)), ..ack(Some(&e.id)) })
}

/// Rate a capability after receiving its result: does it deliver what it claims?
async fn rate_capability(node: &Arc<Node>, r: RateRequest) -> R<Ack> {
    check_score(r.score)?;
    let rid = hex_id(opt_slug(&r.event).ok_or("a capability rating cites the result you received: give 'event'")?, "event")?;
    let res = get_or_fetch(node, &rid).await.ok_or("result not found")?;
    if res.kind != K_JOB_RESULT {
        return Err("'event' must be a capability result".into());
    }
    if res.tag("p").and_then(H32::from_hex) != Some(node.keys.pubkey) {
        return Err("you can only rate a capability that delivered a result to you".into());
    }
    let cap = param(&res, "capability").map(String::from).or_else(|| opt_slug(&r.capability).map(String::from)).ok_or("unknown capability")?;
    let provider = res.pubkey;
    let tags = vec![
        t("d", format!("cap:{}:{cap}", provider.hex())),
        t("p", provider.hex()),
        t("e", rid.hex()),
        t("score", score_tag(r.score)),
        t("aspect", "capability"),
        t("capability", &cap),
    ];
    let e = node.publish(K_RATING, tags, r.reason)?;
    let rep = cap_scores(node).get(&(provider, cap)).map(|x| x.0).unwrap_or(0.0);
    Ok(Ack { rep: round2(rep), ..ack(Some(&e.id)) })
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

    fn test_cx(n: &Arc<Node>) -> Cx {
        Cx { me: n.keys.pubkey, hum: Default::default() }
    }

    #[test]
    fn mentions_resolve_handles_and_keys() {
        let n = node();
        let (argon, other_argon, ada) = (Keys::generate(), Keys::generate(), Keys::generate());
        n.ingest(argon.sign_at(K_PROFILE, vec![], r#"{"name":"Doctor Argon"}"#.into(), now() - 100), 1, false).unwrap();
        n.ingest(other_argon.sign_at(K_PROFILE, vec![], r#"{"name":"doctor-argon"}"#.into(), now() - 5000), 1, false).unwrap();
        n.ingest(ada.sign(K_PROFILE, vec![], r#"{"name":"Ada"}"#.into()), 1, false).unwrap();
        n.store.lock().unwrap().touch(argon.pubkey, now());
        let me = n.keys.pubkey;
        let st = n.store.lock().unwrap();
        let (text, who, missing) = resolve_mentions(&st, &me, "Ping @DoctorArgon and @ada, not @nobody or mail@ada.org; `@ada` in code stays.");
        assert_eq!(who, [argon.pubkey, ada.pubkey], "most recently seen Argon wins; emails and code are not mentions");
        assert_eq!(missing, ["nobody"]);
        assert!(text.starts_with(&format!("Ping nostr:{} and nostr:{}, not @nobody or mail@ada.org; `@ada`", argon.npub(), ada.npub())));
        let (text, who, _) = resolve_mentions(&st, &me, &format!("cc @{} and nostr:{}", ada.npub(), argon.npub()));
        assert_eq!(who, [ada.pubkey, argon.pubkey]);
        assert_eq!(text, format!("cc nostr:{} and nostr:{}", ada.npub(), argon.npub()));
        let (_, who, _) = resolve_mentions(&st, &me, &format!("@{}", npub(&me)));
        assert!(who.is_empty(), "never yourself");
        let many: String = (0..20).map(|_| format!("@{} ", Keys::generate().npub())).collect();
        assert_eq!(resolve_mentions(&st, &me, &many).1.len(), 10, "at most 10");
        assert_eq!(handle_of("Doctor Argon, PhD"), "DoctorArgonPhD");
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
        let short = view(&st, &RepTable::default(), &e, false, &test_cx(&n));
        assert!(short.content.ends_with('…') && short.content.len() <= 603);
        assert_eq!(view(&st, &RepTable::default(), &e, true, &test_cx(&n)).content, body);
        assert_eq!((short.title.as_str(), short.community.as_str(), short.refs.len()), ("T", "m", 1));
        assert!(!short.mine && !short.low_trust && short.parent.is_empty());
        assert!(short.author_name.starts_with("npub1"), "falls back to a short npub");
        let mut rep = RepTable::default();
        rep.user.insert(k.pubkey, -0.4);
        assert!(view(&st, &rep, &e, false, &test_cx(&n)).low_trust);
        let mut rep = RepTable::default();
        rep.event.insert(e.id, (-0.5, 3));
        let v = view(&st, &rep, &e, false, &test_cx(&n));
        assert!(v.low_trust && v.ratings == 3 && v.score == -0.5);
    }

    #[test]
    fn rank_prefers_reputation_replies_and_recency() {
        let k = Keys::generate();
        let fresh = k.sign(K_POST, vec![], "a".into());
        let old = k.sign_at(K_POST, vec![], "b".into(), now() - 3 * 86400);
        let rep = RepTable::default();
        assert!(rank(&rep, &fresh, 0, 0) > rank(&rep, &old, 0, 0));
        assert!(rank(&rep, &fresh, 10, 0) > rank(&rep, &fresh, 0, 0));
        let mut good = RepTable::default();
        good.user.insert(k.pubkey, 0.8);
        assert!(rank(&good, &fresh, 0, 0) > rank(&rep, &fresh, 0, 0));
        assert!(rank(&rep, &fresh, 0, 5) > rank(&rep, &fresh, 0, 0), "citations lift");
        let mut relevant = RepTable::default();
        relevant.relevance.insert(fresh.id, (0.4, 2));
        assert!(rank(&relevant, &fresh, 0, 0) > rank(&rep, &fresh, 0, 0), "relevance lifts");
    }

    #[test]
    fn scores_are_tagged_compactly_and_bounded() {
        assert_eq!(score_tag(0.5), "0.5");
        assert_eq!(score_tag(-1.0), "-1");
        assert_eq!(score_tag(0.0), "0");
        assert_eq!(score_tag(-0.0001), "0");
        assert_eq!(score_tag(1.0 / 3.0), "0.333");
        assert!(check_score(1.0).is_ok() && check_score(-1.0).is_ok());
        assert!(check_score(1.01).is_err() && check_score(f64::NAN).is_err() && check_score(f64::INFINITY).is_err());
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
