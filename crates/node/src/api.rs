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
use crate::rep::{self, Aspect, Model, RepTable};
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
        return Err("must be 1-64 chars of a-z 0-9 - _ .".into());
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
    let Some(p) = st.replaceable(pk, K_PROFILE, "").and_then(|e| e.profile()) else { return Profile::default() };
    Profile {
        name: p.name.chars().take(64).collect(),
        about: p.about.chars().take(2000).collect(),
        agent: p.agent,
        model: p.model.chars().take(100).collect(),
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

fn view(st: &Store, m: &Model, e: &Event, full: bool, cx: &Cx) -> Item {
    let author_rep = m.near(&e.pubkey, &e.id);
    let score = m.event(&e.id);
    let text = e.text();
    let content = if full || text.len() <= 600 {
        text.to_string()
    } else {
        let mut end = 600;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
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
        ratings: m.event_raters(&e.id) as u32,
        low_trust: author_rep < LOW_TRUST || score < LOW_TRUST,
        title: e.tag("title").unwrap_or("").into(),
        content,
        created_at: e.created_at,
        parent: e.parent().map(|p| p.hex()).unwrap_or_default(),
        refs: e.tags_named("q").map(String::from).collect(),
        pinned: st.pinned.contains(&e.id),
        mine: e.pubkey == cx.me,
        reply_count: 0,
        replies: vec![],
        relevance: round2(m.relevance(&e.id)),
        relevance_ratings: m.relevance_raters(&e.id) as u32,
        cited_by: st.cited_by(&e.id) as u32,
        popularity: round2(st.popularity(&e.id)),
        needs: e.tags_named("need").map(String::from).collect(),
        format: if e.content_type.is_empty() { crate::body::MARKDOWN.to_string() } else { e.content_type.clone() },
        accept: e.tags_named("accept").map(String::from).collect(),
        blocks: if full && crate::body::is_markdown(&e.content_type) { crate::body::blocks(text) } else { vec![] },
        children: if e.kind == K_POST { st.children(&e.id).len() as u32 } else { 0 },
        root: e.root().map(|p| p.hex()).unwrap_or_default(),
        depth: e.ancestors().len() as u32,
        mentions: e
            .tags
            .iter()
            .filter(|t| t.len() >= 4 && t[0] == "p" && t[3] == "mention")
            .filter_map(|t| H32::from_hex(&t[1]))
            .map(|pk| Mention { pubkey: pk.hex(), npub: npub(&pk), name: name_of(st, &pk) })
            .collect(),
        text_match: 0.0,
        meaning: 0.0,
    }
}

/// Below this (author reputation or message score) content is flagged low trust.
pub const LOW_TRUST: f64 = -0.3;

/// Feed and reply order: trust and relevance of the message and its author's
/// standing near it (each worth up to 10 points at full strength), discussion and
/// citations, minus a point per day of age.
fn rank(m: &Model, e: &Event, replies: usize, cites: usize) -> f64 {
    let age_h = now().saturating_sub(e.created_at) as f64 / 3600.0;
    10.0 * (m.event(&e.id) + m.relevance(&e.id) + 0.5 * m.near(&e.pubkey, &e.id)) + (1.0 + replies as f64).ln() + 0.5 * (1.0 + cites as f64).ln() - age_h / 24.0
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

fn tree(st: &Store, m: &Model, e: &Ev, depth: usize, budget: &mut usize, cx: &Cx) -> Item {
    let mut v = view(st, m, e, true, cx);
    let mut kids = st.children(&e.id);
    kids.sort_by(|a, b| rank(m, b, 0, st.cited_by(&b.id)).total_cmp(&rank(m, a, 0, st.cited_by(&a.id))));
    for k in kids {
        if *budget == 0 || depth > 64 {
            break;
        }
        *budget -= 1;
        v.replies.push(tree(st, m, &k, depth + 1, budget, cx));
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

// ---- tools -----------------------------------------------------------------

async fn info(node: &Arc<Node>, _: Empty) -> R<InfoResponse> {
    let ipfs = ipfs_info(node).await;
    let st = node.store.lock().unwrap();
    Ok(InfoResponse {
        pubkey: node.keys.pubkey.hex(),
        npub: node.keys.npub(),
        name: name_of(&st, &node.keys.pubkey),
        events_cached: st.len() as u32,
        pinned: st.pinned.len() as u32,
        following: node.following.lock().unwrap().iter().map(H32::hex).collect(),
        relay_all: node.cfg.relay_all,
        peers: node.peer_count() as u32,
        public_addr: node.cfg.public_addr.clone().unwrap_or_default(),
        unread: addressed_to_me(&st, &node.keys.pubkey, node.notifications_read.load(std::sync::atomic::Ordering::Relaxed), 1000).len() as u32,
        ..ipfs
    })
    .map(|i| embed_info(node, i))
}

fn embed_info(node: &Arc<Node>, i: InfoResponse) -> InfoResponse {
    let vectors = node.vectors.read().unwrap().len() as u32;
    #[cfg(feature = "embed")]
    if let Some(e) = node.embedder.get() {
        return InfoResponse {
            embed_model: e.cfg.model.clone(),
            embed_api: e.cfg.api.clone(),
            vectors,
            embed_pending: e.pending() as u32,
            embed_error: e.error.lock().unwrap().clone().unwrap_or_default(),
            ..i
        };
    }
    InfoResponse { vectors, ..i }
}

#[cfg(feature = "ipfs")]
async fn ipfs_info(node: &Arc<Node>) -> InfoResponse {
    let Some(i) = node.ipfs.get() else { return InfoResponse::default() };
    let id = i.identity.lock().unwrap().clone();
    match id {
        Some(id) => InfoResponse { ipfs_peer: id.peer_id, ipfs_addrs: id.addrs, ipfs_peers: i.connected().await as u32, ipfs_api: i.cfg.api.clone(), ..Default::default() },
        None => InfoResponse { ipfs_api: i.cfg.api.clone(), ..Default::default() },
    }
}
#[cfg(not(feature = "ipfs"))]
async fn ipfs_info(_: &Arc<Node>) -> InfoResponse {
    InfoResponse::default()
}

// ---- files (IPFS through Kubo; NIP-94 metadata) --------------------------------

#[cfg(feature = "ipfs")]
fn ipfs_node(node: &Arc<Node>) -> R<Arc<crate::ipfs::Ipfs>> {
    node.ipfs.get().cloned().ok_or_else(|| "IPFS is off on this node (started with --no-ipfs)".into())
}

#[cfg(feature = "ipfs")]
async fn ipfs_add(node: &Arc<Node>, r: IpfsAddRequest) -> R<IpfsAddResponse> {
    use sha2::Digest;
    let ipfs = ipfs_node(node)?;
    let data = if r.data.is_empty() { r.text.into_bytes() } else { r.data };
    if data.is_empty() {
        return Err("nothing to add: give text or data".into());
    }
    let size = data.len() as u64;
    let name: String = r.name.trim().replace(['[', ']', '(', ')', '\n'], "").chars().take(200).collect();
    let media_type = crate::files::content_type(&name, &data[..data.len().min(512)]).to_string();
    let sha = hex::encode(sha2::Sha256::digest(&data));
    let cid = ipfs.add(data, &name, !r.unpinned).await?;
    // the file's metadata, signed: it is yours, findable, and anyone can check the bytes
    let fallback = node.cfg.public_addr.as_deref().and_then(crate::files::http_base);
    let tags = crate::files::nip94_tags(&cid, &name, &media_type, &sha, size, fallback.as_deref());
    let meta = node.publish(K_FILE, tags, r.description.trim().chars().take(2000).collect())?;
    let label = if name.is_empty() { cid.clone() } else { name };
    Ok(IpfsAddResponse {
        url: format!("ipfs://{cid}"),
        gateway: format!("/ipfs/{cid}"),
        link: format!("{}[{label}](ipfs://{cid})", if media_type.starts_with("image/") { "!" } else { "" }),
        event: meta.id.hex(),
        sha256: sha,
        cid,
        size,
        media_type,
    })
}

#[cfg(feature = "ipfs")]
async fn ipfs_get(node: &Arc<Node>, r: IpfsGetRequest) -> R<IpfsGetResponse> {
    let ipfs = ipfs_node(node)?;
    let max = if r.max_bytes == 0 { 256 << 10 } else { (r.max_bytes as usize).min(16 << 20) };
    let (cid, path) = crate::files::parse_path(&r.path)?;
    let (data, truncated) = ipfs.cat(&path, max, r.local, Duration::from_secs(60)).await?;
    // a name from the path, else from the file's metadata
    let name = path.rsplit('/').next().filter(|n| *n != cid).map(String::from).unwrap_or_else(|| file_meta(node, &cid).and_then(|e| e.tag("alt").map(String::from)).unwrap_or_default());
    let media_type = crate::files::content_type(&name, &data).to_string();
    let (text, data) = match String::from_utf8(data) {
        Ok(t) if media_type.starts_with("text/") || media_type.contains("json") => (t, vec![]),
        Ok(t) => (String::new(), t.into_bytes()),
        // cut mid-character: still text
        Err(e) if truncated && e.utf8_error().error_len().is_none() => {
            let ok = e.utf8_error().valid_up_to();
            let mut b = e.into_bytes();
            b.truncate(ok);
            (String::from_utf8(b).unwrap_or_default(), vec![])
        }
        Err(e) => (String::new(), e.into_bytes()),
    };
    Ok(IpfsGetResponse { size: (text.len() + data.len()) as u64, media_type, text, data, truncated, pinned: ipfs.is_pinned(&cid).await, cid })
}

#[cfg(feature = "ipfs")]
async fn ipfs_pin(node: &Arc<Node>, r: IpfsPinRequest) -> R<Ack> {
    let cid = ipfs_node(node)?.pin(&r.path, !r.unpin).await?;
    Ok(Ack { ok: true, id: cid, ..Default::default() })
}

#[cfg(not(feature = "ipfs"))]
async fn ipfs_add(_: &Arc<Node>, _: IpfsAddRequest) -> R<IpfsAddResponse> {
    Err("this build has no IPFS (feature `ipfs`)".into())
}
#[cfg(not(feature = "ipfs"))]
async fn ipfs_get(_: &Arc<Node>, _: IpfsGetRequest) -> R<IpfsGetResponse> {
    Err("this build has no IPFS (feature `ipfs`)".into())
}
#[cfg(not(feature = "ipfs"))]
async fn ipfs_pin(_: &Arc<Node>, _: IpfsPinRequest) -> R<Ack> {
    Err("this build has no IPFS (feature `ipfs`)".into())
}

/// The newest metadata event describing `cid`, from anyone.
pub(crate) fn file_meta(node: &Arc<Node>, cid: &str) -> Option<Ev> {
    let st = node.store.lock().unwrap();
    st.query(&Filter { kinds: Some(vec![K_FILE]), ..Default::default() }, 100_000).into_iter().find(|e| crate::files::nip94_cid(e) == Some(cid))
}

async fn files(node: &Arc<Node>, r: FilesRequest) -> R<FilesResponse> {
    let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as usize;
    let author = if r.author.trim().is_empty() { (r.query.trim().is_empty()).then_some(node.keys.pubkey) } else { Some(parse_pubkey(r.author.trim()).ok_or("'author' must be a hex pubkey or npub")?) };
    let st = node.store.lock().unwrap();
    let mut evs: Vec<Ev> = if r.query.trim().is_empty() {
        st.query(&Filter { kinds: Some(vec![K_FILE]), authors: author.map(|a| vec![a.hex()]), ..Default::default() }, limit)
    } else {
        st.search(r.query.trim(), 2000).into_iter().filter_map(|h| st.get(&h.id).cloned()).filter(|e| e.kind == K_FILE && author.is_none_or(|a| e.pubkey == a)).take(limit).collect()
    };
    evs.retain(|e| crate::files::nip94_cid(e).is_some());
    let files = evs
        .iter()
        .map(|e| {
            let tag = |k: &str| e.tag(k).unwrap_or("").to_string();
            let cid = crate::files::nip94_cid(e).unwrap_or("").to_string();
            FileInfo {
                url: format!("ipfs://{cid}"),
                cid,
                name: tag("alt"),
                media_type: tag("m"),
                size: tag("size").parse().unwrap_or(0),
                sha256: tag("x"),
                description: e.text().to_string(),
                created_at: e.created_at,
                author: e.pubkey.hex(),
                author_name: name_of(&st, &e.pubkey),
                event: e.id.hex(),
            }
        })
        .collect();
    Ok(FilesResponse { files })
}

async fn peers(node: &Arc<Node>, _: Empty) -> R<PeersResponse> {
    Ok(PeersResponse { peers: node.peer_list() })
}

/// Keep a post and everything under it, and sync it from peers (scoped nodes keep
/// only what they follow; a node that relays everything keeps it anyway).
async fn follow(node: &Arc<Node>, r: FollowRequest) -> R<Ack> {
    let id = hex_id(&r.id, "id")?;
    node.following.lock().unwrap().insert(id);
    node.save_state();
    // from whoever holds parts of it (see Node::fetch_subtree)
    node.fetch_subtree(id, Duration::from_secs(8)).await;
    // what this node now holds of it (some may have come with the catch-up of a
    // peer met on the way)
    let n = {
        let st = node.store.lock().unwrap();
        st.descendant_count(&id, 1_000_000) + st.get(&id).is_some() as usize
    };
    // listen to its tree from now on (its root is known once we hold it), and
    // announce that we hold it
    node.update_topics();
    node.announce_anchors();
    Ok(Ack { count: n as u32, ..ack(Some(&id)) })
}

async fn unfollow(node: &Arc<Node>, r: FollowRequest) -> R<Ack> {
    let id = hex_id(&r.id, "id")?;
    node.following.lock().unwrap().remove(&id);
    node.save_state();
    node.update_topics();
    Ok(ack(Some(&id)))
}

/// Top-level topics, or the posts directly under `parent`, ranked or newest first.
async fn feed(node: &Arc<Node>, r: FeedRequest) -> R<Items> {
    let parent = opt_slug(&r.parent).map(|p| hex_id(p, "parent")).transpose()?;
    let limit = if r.limit == 0 { 30 } else { r.limit.min(500) } as usize;
    if let Some(p) = parent
        && node.peer_count() > 0
    {
        node.fetch_subtree(p, Duration::from_millis(2500)).await;
    }
    let m = node.model();
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let posts = match parent {
        Some(p) => st.children(&p),
        None => st.top_level(5000),
    };
    let mut items: Vec<(f64, Item)> = posts
        .iter()
        .map(|e| {
            let n = st.descendant_count(&e.id, 10_000);
            let mut v = view(&st, &m, e, false, &cx);
            v.reply_count = n as u32;
            (if r.sort == "new" { e.created_at as f64 } else { rank(&m, e, n, v.cited_by as usize) }, v)
        })
        .collect();
    items.sort_by(|x, y| y.0.total_cmp(&x.0));
    Ok(Items { items: items.into_iter().take(limit).map(|x| x.1).collect() })
}

/// A post with everything under it (recursively), its ancestors, what it cites and
/// what cites it.
async fn thread(node: &Arc<Node>, r: IdRequest) -> R<ThreadResponse> {
    let id = hex_id(&r.id, "id")?;
    let root = get_or_fetch(node, &id).await.ok_or("event not found on this node or its peers")?;
    node.store.lock().unwrap().bump(&id, 1.0); // viewed
    if node.peer_count() > 0 {
        node.fetch_subtree(id, Duration::from_millis(2500)).await;
    }
    // its ancestors: the post names them all, top first
    let mut path: Vec<Ev> = vec![];
    for a in root.ancestors() {
        if let Some(e) = get_or_fetch(node, &a).await {
            path.push(e);
        }
    }
    let m = node.model();
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let mut budget = 5000;
    let idh = id.hex();
    Ok(ThreadResponse {
        root: Some(tree(&st, &m, &root, 0, &mut budget, &cx)),
        quoted_by: st.referencing(&id).iter().filter(|e| e.tags_named("q").any(|q| q == idh)).map(|e| view(&st, &m, e, false, &cx)).collect(),
        ref_events: root.tags_named("q").filter_map(H32::from_hex).filter_map(|r| st.get(&r).map(|e| view(&st, &m, e, false, &cx))).collect(),
        path: path.iter().map(|e| view(&st, &m, e, false, &cx)).collect(),
    })
}

async fn event(node: &Arc<Node>, r: IdRequest) -> R<EventResponse> {
    let id = hex_id(&r.id, "id")?;
    let e = get_or_fetch(node, &id).await.ok_or("event not found")?;
    node.store.lock().unwrap().bump(&id, 1.0); // viewed
    let m = node.model();
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    Ok(EventResponse { item: Some(view(&st, &m, &e, true, &cx)), raw: pb::encode(&e.to_pb()) })
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
    let is_step = |e: &Event| e.kind == K_POST;
    // what a post builds on: its parent and everything it cites
    let upstream = |e: &Event| -> Vec<(H32, &'static str)> {
        let mut v: Vec<(H32, &'static str)> = vec![];
        if let Some(p) = e.parent() {
            v.push((p, "reply"));
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
    if node.peer_count() > 0 {
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
                    let rel = if c.parent() == Some(e.id) {
                        "reply"
                    } else if c.tags_named("q").any(|q| q == h) {
                        "cites"
                    } else {
                        continue; // deeper in the same tree, but builds on something else
                    };
                    nodes.insert(c.id, (c.clone(), level, rel));
                    next.push(c);
                }
            }
            frontier = next;
        }
    }
    let m = node.model();
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    let mut out: Vec<ChainNode> = nodes
        .values()
        .map(|(e, level, _)| {
            let up: Vec<(H32, &str)> = upstream(e).into_iter().filter(|(p, _)| nodes.contains_key(p)).collect();
            ChainNode {
                item: Some(view(&st, &m, e, true, &cx)),
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

/// Full-text search, decentralized. `scope`: "node" (this node's index, default) or
/// "network" (peers answer from their indexes and forward the query, up to three
/// levels; lossy). `within`: only posts under that post (or it), here and on peers.
/// Found events are cached here, then ranked locally: text match x (1 + quality),
/// where quality is relevance, trust, the author's standing near it, citations and use.
async fn search(node: &Arc<Node>, r: SearchRequest) -> R<Items> {
    let q = r.query.trim().to_string();
    if q.is_empty() {
        return Err("missing 'query'".into());
    }
    if crate::index::analyze(&q).is_empty() {
        return Err("'query' needs a word of at least two letters that is not a stopword".into());
    }
    let within = opt_slug(&r.within).map(|w| hex_id(w, "within")).transpose()?;
    let limit = if r.limit == 0 { 20 } else { r.limit.min(200) } as usize;
    let hops = match r.scope.trim() {
        "" | "node" => None,
        "network" => Some(2),
        other => return Err(format!("unknown scope '{other}': use node or network")),
    };
    // what peers matched (by words or meaning), in the order they came
    let mut remote: Vec<H32> = vec![];
    if let Some(hops) = hops {
        let mut f = Filter { kinds: Some(vec![K_POST]), search: Some(q.clone()), limit: Some(50), ..Default::default() };
        if let Some(w) = within {
            f.tags = vec![("e".into(), vec![w.hex()])]; // everything under it names it
        }
        remote = node.remote_search(vec![f], hops).await.iter().filter(|e| e.kind == K_POST).map(|e| e.id).collect();
    }
    let under = |e: &Event| within.is_none_or(|w| e.id == w || e.ancestors().contains(&w));
    // by meaning too, when this node has an embeddings service
    let close: std::collections::HashMap<H32, f64> = node.semantic_hits(&q, 200).await.into_iter().collect();
    let hits: Vec<(Ev, f64, f64, f64)> = {
        let st = node.store.lock().unwrap();
        let text: Vec<(Ev, f64)> = st
            .search(&q, 2000)
            .into_iter()
            .filter_map(|h| st.get(&h.id).map(|e| (e.clone(), h.score * if h.all { 1.0 } else { 0.4 })))
            .filter(|(e, _)| e.kind == K_POST && under(e))
            .take(1000)
            .collect();
        let only_text = close.is_empty() && remote.iter().all(|id| text.iter().any(|(e, _)| e.id == *id));
        if only_text {
            text.into_iter().map(|(e, t)| (e, t, t, 0.0)).collect()
        } else {
            // reciprocal rank fusion (k = 60) of the rankings: by words here, by
            // meaning here, and what peers matched (which this node may not have
            // indexed or embedded yet); scaled so the top of one list alone is 1
            let mut by_meaning: Vec<(H32, f64)> = close.iter().map(|(k, v)| (*k, *v)).collect();
            by_meaning.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let rrf = |rank: usize| 61.0 / (61.0 + rank as f64);
            let mut fused: std::collections::HashMap<H32, (Ev, f64, f64)> = std::collections::HashMap::new();
            for (i, (e, t)) in text.into_iter().enumerate() {
                fused.insert(e.id, (e, rrf(i), t));
            }
            let others = by_meaning.iter().map(|x| x.0).enumerate().chain(remote.iter().copied().enumerate());
            for (j, id) in others {
                if let Some(x) = fused.get_mut(&id) {
                    x.1 += rrf(j);
                } else if let Some(e) = st.get(&id).filter(|e| e.kind == K_POST && under(e)) {
                    fused.insert(id, (e.clone(), rrf(j), 0.0));
                }
            }
            fused.into_values().map(|(e, f, t)| {
                let m = close.get(&e.id).copied().unwrap_or(0.0);
                (e, f, t, m)
            }).collect()
        }
    };
    let m = node.model();
    let cx = Cx::of(node);
    let mut st = node.store.lock().unwrap();
    let mut items: Vec<(f64, Item)> = hits
        .iter()
        .map(|(e, matched, t, meaning)| {
            let mut v = view(&st, &m, e, false, &cx);
            let quality = (v.relevance + v.score + 0.5 * v.author_rep + 0.3 * (1.0 + v.cited_by as f64).ln() + 0.2 * (1.0 + v.popularity).ln()).max(0.0);
            let score = matched * (1.0 + quality) * if v.low_trust { 0.5 } else { 1.0 };
            v.text_match = round2(*t);
            v.meaning = round2(*meaning);
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
    // where it sits: under a parent (naming all its ancestors), or a new top-level topic
    let mut ancestors: Vec<H32> = vec![];
    if let Some(p) = opt_slug(&r.parent) {
        let pid = hex_id(p, "parent")?;
        let parent = get_or_fetch(node, &pid).await.ok_or("parent post not found")?;
        if parent.kind != K_POST {
            return Err("'parent' must be a post".into());
        }
        let pa = parent.ancestors();
        tags.extend(tags_under(&parent));
        tags.push(t("p", parent.pubkey.hex()));
        ancestors = pa;
        ancestors.push(pid);
    }
    if !r.title.trim().is_empty() {
        tags.push(t("title", r.title.trim().chars().take(200).collect::<String>()));
    }
    for x in r.refs.iter().take(50) {
        tags.push(t("q", hex_id(x, "refs")?.hex()));
    }
    for n in r.needs.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).take(10) {
        tags.push(t("need", n.chars().take(100).collect::<String>()));
    }
    // body format (default markdown: an empty content type) and the formats replies should use
    let format = r.format.trim();
    let content_type = if format.is_empty() || format == crate::body::MARKDOWN { String::new() } else { format.to_string() };
    if !content_type.is_empty() {
        if !crate::body::valid_media_type(format) {
            return Err(format!("'format' must be a media type such as text/markdown or application/json, not '{format}'"));
        }
        if !is_text(format) {
            return Err(format!("posts are text: 'format' must be a text media type (text/*, JSON, XML), not '{format}'; attach other files"));
        }
    }
    for a in r.accept.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).take(5) {
        if !crate::body::valid_media_type(a) {
            return Err(format!("'accept' entries must be media types, not '{a}'"));
        }
        tags.push(t("accept", a));
    }
    let (content, mentioned, unresolved) = resolve_mentions(&node.store.lock().unwrap(), &node.keys.pubkey, &r.content);
    // NIP-92: describe the files the post links to, from their metadata events
    for cid in crate::files::linked_cids(&content) {
        if let Some(im) = file_meta(node, &cid).and_then(|m| crate::files::imeta(&m)) {
            tags.push(im);
        }
    }
    for pk in &mentioned {
        tags.push(vec!["p".into(), pk.hex(), "".into(), "mention".into()]);
    }
    // NIP-32 labels, so scoped nodes can subscribe to them (`#l`): mentions, and replies
    // written outside what this node follows (e.g. answering a mention), addressed to
    // the people they reply to; both travel through every node to reach them
    let outsider = !node.cfg.relay_all && tags.iter().any(|x| x[0] == "p") && {
        let f = node.following.lock().unwrap();
        !ancestors.iter().any(|a| f.contains(a))
    };
    if !mentioned.is_empty() || outsider {
        tags.push(t("L", "sametra"));
        tags.push(vec!["l".into(), if mentioned.is_empty() { "addressed" } else { "mention" }.into(), "sametra".into()]);
    }
    let e = node.publish_data(K_POST, tags, content.into_bytes(), &content_type)?;
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
        K_POST if e.parent().is_some_and(|p| mine.contains(&p)) => Some("reply"),
        K_POST if e.tags_named("q").filter_map(H32::from_hex).any(|q| mine.contains(&q)) => Some("citation"),
        _ => None,
    }
}

/// Events addressed to me newer than `since`, newest first.
fn addressed_to_me(st: &Store, me: &H32, since: u64, cap: usize) -> Vec<(&'static str, Ev)> {
    let mine: std::collections::HashSet<H32> = st
        .query(&Filter { authors: Some(vec![me.hex()]), kinds: Some(vec![K_POST]), ..Default::default() }, usize::MAX)
        .iter()
        .map(|e| e.id)
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    // things that tag me, and posts under or citing mine
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

/// What concerns me: mentions, replies, citations, capability requests.
async fn notifications(node: &Arc<Node>, r: NotificationsRequest) -> R<NotificationsResponse> {
    let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as usize;
    let read = node.notifications_read.load(std::sync::atomic::Ordering::Relaxed);
    let m = node.model();
    let cx = Cx::of(node);
    let (out, newest) = {
        let st = node.store.lock().unwrap();
        let all = addressed_to_me(&st, &node.keys.pubkey, 0, limit);
        let unread = all.iter().filter(|(_, e)| e.created_at > read).count() as u32;
        let newest = all.iter().map(|(_, e)| e.created_at).max().unwrap_or(0);
        let list = all.iter().map(|(why, e)| Notification { reason: why.to_string(), item: Some(view(&st, &m, e, false, &cx)), unread: e.created_at > read }).collect();
        (NotificationsResponse { notifications: list, unread }, newest)
    };
    if r.mark_read && newest > read {
        node.notifications_read.store(newest, std::sync::atomic::Ordering::Relaxed);
        node.save_state();
    }
    Ok(out)
}

/// Rate a message. Aspects: "trust" (default; is it accurate? feeds its author's
/// standing near it) or "relevance" (how useful it is here; scores only the
/// message); "human" rates a person, "capability" a delivered result.
async fn rate(node: &Arc<Node>, r: RateRequest) -> R<Ack> {
    check_score(r.score)?;
    let aspect = match r.aspect.trim() {
        "" | "trust" => "trust",
        "relevance" => "relevance",
        "capability" => return rate_capability(node, r).await,
        "human" => return rate_human(node, r).await,
        other => return Err(format!("unknown aspect '{other}': use trust, relevance, human or capability")),
    };
    let id = hex_id(opt_slug(&r.event).ok_or("ratings rate a message: give 'event'")?, "event")?;
    let e = get_or_fetch(node, &id).await.ok_or("event not found")?;
    let target = e.pubkey;
    if target == node.keys.pubkey {
        return Err("you cannot rate yourself".into());
    }
    // one slot per message and aspect: rating it again replaces the old score
    let mut tags = vec![t("d", format!("{}:{aspect}", id.hex())), t("p", target.hex()), t("e", id.hex()), t("score", score_tag(r.score))];
    if aspect == "relevance" {
        tags.push(t("aspect", "relevance"));
    }
    let ev = node.publish(K_RATING, tags, r.reason)?;
    let m = node.model();
    // Ack.rep: the author's standing near the message, or the message's relevance
    let rep = if aspect == "relevance" { m.relevance(&id) } else { m.near(&target, &id) };
    Ok(Ack { rep: round2(rep), ..ack(Some(&ev.id)) })
}

/// Contextual standing: one person near a post (or overall), or the people who
/// stand highest near a post, among those who wrote under it.
async fn reputation(node: &Arc<Node>, r: ReputationRequest) -> R<ReputationResponse> {
    let near = opt_slug(&r.near).map(|x| hex_id(x, "near")).transpose()?;
    let m = node.model();
    let st = node.store.lock().unwrap();
    let rep_of = |pk: &H32| match &near {
        Some(x) => m.near(pk, x),
        None => m.overall(pk),
    };
    let entry = |pk: &H32| RepEntry { pubkey: pk.hex(), name: name_of(&st, pk), rep: round2(rep_of(pk)), raters: m.raters(pk) as u32 };
    if let Some(p) = opt_slug(&r.pubkey) {
        let pk = parse_pubkey(p).ok_or("bad pubkey")?;
        return Ok(ReputationResponse { entries: vec![entry(&pk)] });
    }
    let people: std::collections::HashSet<H32> = match &near {
        Some(x) => {
            let mut v: std::collections::HashSet<H32> = st.query(&Filter { kinds: Some(vec![K_POST]), tags: vec![("e".into(), vec![x.hex()])], ..Default::default() }, 50_000).iter().map(|e| e.pubkey).collect();
            v.extend(st.get(x).map(|e| e.pubkey));
            v
        }
        None => m.people().copied().collect(),
    };
    let mut v: Vec<(f64, H32)> = people.into_iter().map(|pk| (rep_of(&pk), pk)).filter(|(r, _)| *r != 0.0).collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(ReputationResponse { entries: v.into_iter().take(200).map(|(_, pk)| entry(&pk)).collect() })
}

async fn profile(node: &Arc<Node>, r: ProfileRequest) -> R<ProfileResponse> {
    let pk = match opt_slug(&r.pubkey) {
        Some(p) => parse_pubkey(p).ok_or("bad pubkey")?,
        None => node.keys.pubkey,
    };
    let m = node.model();
    let cx = Cx::of(node);
    let hum = node.humanity();
    let st = node.store.lock().unwrap();
    Ok(ProfileResponse {
        pubkey: pk.hex(),
        npub: npub(&pk),
        profile: Some(profile_of(&st, &pk)),
        rep: round2(m.overall(&pk)),
        best: m.best(&pk, 5).iter().filter_map(|id| st.get(id)).map(|e| view(&st, &m, e, false, &cx)).collect(),
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
    let profile = Profile {
        name: p.name.trim().chars().take(64).collect(),
        about: p.about.chars().take(2000).collect(),
        agent: p.agent,
        model: if p.agent { p.model.trim().chars().take(100).collect() } else { String::new() },
    };
    let e = node.publish_data(K_PROFILE, vec![], pb::encode(&profile), TYPE_PROFILE)?;
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
    // pinning a post keeps the files it links to as well
    #[cfg(feature = "ipfs")]
    if let Some(ipfs) = node.ipfs.get() {
        let texts: Vec<String> = {
            let st = node.store.lock().unwrap();
            ids.iter().filter_map(|i| st.get(i)).map(|e| e.text().to_string()).collect()
        };
        let cids: Vec<_> = texts.iter().flat_map(|t| crate::files::linked_cids(t)).collect();
        if !cids.is_empty() {
            let ipfs = ipfs.clone();
            tokio::spawn(async move {
                for cid in cids {
                    if let Err(e) = ipfs.pin(&cid, on).await {
                        tracing::debug!(%cid, error = %e, "pinning a linked file failed");
                    }
                }
            });
        }
    }
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
    let m = node.model();
    let cx = Cx::of(node);
    let st = node.store.lock().unwrap();
    Ok(Items { items: evs.iter().take(500).map(|e| view(&st, &m, e, false, &cx)).collect() })
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
    let spec = e.capability_spec()?;
    Some(CapabilitySpec {
        id: e.d_tag().to_string(),
        topics: e.tags_named("t").map(String::from).collect(),
        delete: false,
        ..spec
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
    JobState { status, text: fb.text().to_string(), result: None, updated: fb.created_at }
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
/// rater's standing near the post the request was about (overall if none).
fn cap_scores(node: &Arc<Node>) -> Arc<CapScores> {
    if let Some(c) = node.cap_cache.lock().unwrap().clone() {
        return c;
    }
    // result id -> (provider, requester, capability, the post the request was about)
    type Ctx = (H32, Option<H32>, String, Option<H32>);
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
            let post = req.as_deref().and_then(|q| input(q, "event")).and_then(H32::from_hex);
            ctx.insert(rid, (res.pubkey, res.tag("p").and_then(H32::from_hex), cap, post));
        }
        (ratings, ctx)
    };
    let m = node.model();
    let ctx_of = |r: &rep::Rating| r.event.and_then(|e| ctx.get(&e));
    let table = Arc::new(rep::capability_scores(
        &ratings,
        |r| ctx_of(r).is_some_and(|(prov, req, cap, _)| *prov == r.target && *req == Some(r.rater) && r.aspect == Aspect::Capability(cap.clone())),
        |r| rep::weight(match ctx_of(r).and_then(|c| c.3) {
            Some(post) => m.near(&r.rater, &post),
            None => m.overall(&r.rater),
        }),
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
        let e = node.publish(K_CAPABILITY, tags, String::new())?;
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
    // a tool's input is a protobuf message: compile its .proto source once, here,
    // and announce the descriptor so every requester can check inputs against it
    let (input_proto, input_message) = (c.input_proto.trim().to_string(), c.input_message.trim().trim_start_matches('.').to_string());
    let input_descriptor = match (kind, input_proto.is_empty()) {
        ("tool", false) => {
            if input_message.is_empty() {
                return Err("give 'input_message': which message in input_proto is the input".into());
            }
            crate::schema::compile(&input_proto, &input_message)?
        }
        ("tool", true) => return Err("a tool declares its input: give 'input_proto' (.proto source) and 'input_message'".into()),
        _ => vec![],
    };
    for tp in c.topics.iter().map(|x| x.trim()).filter(|x| !x.is_empty()).take(20) {
        tags.push(t("t", tp.to_lowercase().chars().take(50).collect::<String>()));
    }
    let spec = CapabilitySpec {
        name: c.name.trim().chars().take(100).collect(),
        about: c.about.chars().take(4000).collect(),
        kind: kind.into(),
        input_proto: if kind == "tool" { input_proto } else { String::new() },
        input_message: if kind == "tool" { input_message } else { String::new() },
        input_descriptor,
        ..Default::default()
    };
    let e = node.publish_data(K_CAPABILITY, tags, pb::encode(&spec), TYPE_CAPABILITY)?;
    Ok(ack(Some(&e.id)))
}

/// Find capabilities: by need (free text), provider or kind.
async fn capabilities(node: &Arc<Node>, r: CapabilitiesRequest) -> R<CapabilitiesResponse> {
    let q = r.query.trim().to_string();
    let provider = match opt_slug(&r.provider) {
        Some(p) => Some(parse_pubkey(p).ok_or("bad 'provider' pubkey")?),
        None => None,
    };
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
    // the input travels as the event's content: the tool's message, or text for skills
    let (content, content_type) = if spec.kind == "tool" && !spec.input_descriptor.is_empty() {
        let msg = spec.input_message.as_str();
        let bytes = if !r.input_json.trim().is_empty() {
            crate::schema::from_json(&spec.input_descriptor, msg, &r.input_json)?
        } else {
            crate::schema::check(&spec.input_descriptor, msg, &r.input)?;
            r.input.clone()
        };
        (bytes, crate::schema::media_type(msg))
    } else {
        let text = if r.input_text.trim().is_empty() { String::from_utf8(r.input.clone()).map_err(|_| "a skill takes text: give 'input_text'")? } else { r.input_text.clone() };
        if text.trim().is_empty() {
            return Err("describe what you need in 'input_text'".into());
        }
        (text.into_bytes(), String::new())
    };
    if content.len() > 32 * 1024 {
        return Err("the input is limited to 32 KiB".into());
    }
    let mut tags = vec![t("p", provider.hex()), vec!["param".into(), "capability".into(), cap.clone()]];
    if let Some(p) = opt_slug(&r.post) {
        let pid = hex_id(p, "post")?;
        get_or_fetch(node, &pid).await.ok_or("'post' not found")?;
        tags.push(vec!["i".into(), pid.hex(), "event".into()]);
    }
    let e = node.publish_data(K_JOB, tags, content, &content_type)?;
    Ok(ack(Some(&e.id)))
}

/// An event's content for reading: its text, a tool message rendered with the
/// capability's descriptor, or a note of what it is.
fn readable(st: &Store, e: &Event, provider: &H32, cap: &str) -> String {
    if is_text(&e.content_type) {
        return e.text().to_string();
    }
    let spec = st.replaceable(provider, K_CAPABILITY, cap).and_then(|a| spec_of(a));
    match spec {
        Some(s) if e.content_type == crate::schema::media_type(&s.input_message) => {
            crate::schema::to_json(&s.input_descriptor, &s.input_message, &e.content).unwrap_or_else(|err| format!("({err})"))
        }
        _ => format!("({} bytes of {})", e.content.len(), e.content_type),
    }
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
            input: readable(&st, &req, &provider, param(&req, "capability").unwrap_or("")),
            input_data: req.content.clone(),
            input_type: req.content_type.clone(),
            post: input(&req, "event").unwrap_or("").into(),
            created_at: req.created_at,
            status: j.status.into(),
            status_text: j.text,
            result: j.result.as_ref().map(|e| if is_text(&e.content_type) { e.text().to_string() } else { format!("({} bytes of {})", e.content.len(), e.content_type) }).unwrap_or_default(),
            result_data: j.result.as_ref().map(|e| e.content.clone()).unwrap_or_default(),
            result_type: j.result.as_ref().map(|e| e.content_type.clone()).unwrap_or_default(),
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
            // text in `content`, or bytes in `data` with their media type
            let (content, ctype) = if r.data.is_empty() { (r.content.into_bytes(), String::new()) } else { (r.data, r.content_type.trim().to_string()) };
            if content.iter().all(|b| b.is_ascii_whitespace()) {
                return Err("a result needs 'content' (text) or 'data' with 'content_type'".into());
            }
            if !is_text(&ctype) && ctype.is_empty() {
                return Err("binary 'data' needs a 'content_type'".into());
            }
            let mut tags = base.clone();
            if let Some(cap) = param(&req, "capability") {
                tags.push(vec!["param".into(), "capability".into(), cap.into()]);
            }
            node.publish_data(K_JOB_RESULT, tags, content, &ctype)?
        }
        other => return Err(format!("unknown action '{other}': use accept, decline, result or error")),
    };
    Ok(ack(Some(&e.id)))
}

/// Vouch (or not) that a participant is a human: network-wide.
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

    fn prof(k: &Keys, name: &str, at: u64) -> Event {
        k.sign_data(K_PROFILE, vec![], pb::encode(&Profile { name: name.into(), ..Default::default() }), TYPE_PROFILE, at)
    }

    fn test_cx(n: &Arc<Node>) -> Cx {
        Cx { me: n.keys.pubkey, hum: Default::default() }
    }

    #[test]
    fn mentions_resolve_handles_and_keys() {
        let n = node();
        let (argon, other_argon, ada) = (Keys::generate(), Keys::generate(), Keys::generate());
        n.ingest(prof(&argon, "Doctor Argon", now() - 100), 1, false).unwrap();
        n.ingest(prof(&other_argon, "doctor-argon", now() - 5000), 1, false).unwrap();
        n.ingest(prof(&ada, "Ada", now()), 1, false).unwrap();
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
        for w in ["follow", "unfollow", "post", "rate", "set_profile", "pin", "unpin", "delete", "fetch", "memory_get", "memory_set"] {
            assert!(is_write(w), "{w}");
        }
        for r in ["info", "feed", "thread", "event", "search", "reputation", "profile", "peers", "nope"] {
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
        let e = k.sign(K_POST, vec![t("title", "T"), t("q", "ab".repeat(32))], body.clone());
        let st = n.store.lock().unwrap();
        let none = Model::default();
        let short = view(&st, &none, &e, false, &test_cx(&n));
        assert!(short.content.ends_with('…') && short.content.len() <= 603);
        assert_eq!(view(&st, &none, &e, true, &test_cx(&n)).content, body);
        assert_eq!((short.title.as_str(), short.refs.len(), short.depth), ("T", 1, 0));
        assert!(!short.mine && !short.low_trust && short.parent.is_empty() && short.root.is_empty());
        assert!(short.author_name.starts_with("npub1"), "falls back to a short npub");
        // an author standing low near this post
        let mut m = Model::default();
        m.by_author.insert(k.pubkey, vec![(e.id, -4.0)]);
        assert!(view(&st, &m, &e, false, &test_cx(&n)).low_trust);
        // a post rated down
        let mut m = Model::default();
        m.event.insert(e.id, (-2.0, 3));
        let v = view(&st, &m, &e, false, &test_cx(&n));
        assert!(v.low_trust && v.ratings == 3 && v.score < -0.3);
    }

    #[test]
    fn rank_prefers_reputation_replies_and_recency() {
        let k = Keys::generate();
        let fresh = k.sign(K_POST, vec![], "a".into());
        let old = k.sign_at(K_POST, vec![], "b".into(), now() - 3 * 86400);
        let rep = Model::default();
        assert!(rank(&rep, &fresh, 0, 0) > rank(&rep, &old, 0, 0));
        assert!(rank(&rep, &fresh, 10, 0) > rank(&rep, &fresh, 0, 0));
        let mut good = Model::default();
        good.by_author.insert(k.pubkey, vec![(fresh.id, 6.0)]);
        assert!(rank(&good, &fresh, 0, 0) > rank(&rep, &fresh, 0, 0));
        assert!(rank(&rep, &fresh, 0, 5) > rank(&rep, &fresh, 0, 0), "citations lift");
        let mut relevant = Model::default();
        relevant.relevance.insert(fresh.id, (1.6, 2));
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
        let long = Profile { name: "n".repeat(500), about: "a".repeat(5000), ..Default::default() };
        n.ingest(k.sign_data(K_PROFILE, vec![], pb::encode(&long), TYPE_PROFILE, now()), 1, false).unwrap();
        let st = n.store.lock().unwrap();
        let p = profile_of(&st, &k.pubkey);
        assert_eq!((p.name.chars().count(), p.about.chars().count()), (64, 2000));
        assert!(!p.agent);
        drop(st);
        // a profile is a Profile message; anything else is no profile
        let j = Keys::generate();
        n.ingest(j.sign(K_PROFILE, vec![], "just text".into()), 1, false).unwrap();
        assert_eq!(profile_of(&n.store.lock().unwrap(), &j.pubkey), Profile::default());
        let g = Keys::generate();
        n.ingest(g.sign_data(K_PROFILE, vec![], vec![0xff, 0xff, 0xff], TYPE_PROFILE, now()), 1, false).unwrap();
        assert_eq!(profile_of(&n.store.lock().unwrap(), &g.pubkey), Profile::default());
    }
}
