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
        low_trust: author_rep < LOW_TRUST || score < LOW_TRUST,
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
        relevance: round2(rep.relevance(&e.id)),
        relevance_ratings: rep.relevance.get(&e.id).map(|x| x.1 as u32).unwrap_or(0),
        cited_by: st.cited_by(&e.id) as u32,
        popularity: round2(st.popularity(&e.id)),
        needs: e.tags_named("need").map(String::from).collect(),
        text_match: 0.0,
    }
}

/// Below this (author reputation or message score) content is flagged low trust.
pub const LOW_TRUST: f64 = -0.3;

/// Feed and reply order: trust and relevance of the message, author standing
/// (each worth up to 10 points at full strength), discussion and citations,
/// minus a point per day of age.
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

fn tree(st: &Store, rep: &RepTable, e: &Ev, depth: usize, budget: &mut usize, me: &H32) -> Item {
    let mut v = view(st, rep, e, true, me);
    let mut kids = st.children(&e.id);
    kids.sort_by(|a, b| rank(rep, b, 0, st.cited_by(&b.id)).total_cmp(&rank(rep, a, 0, st.cited_by(&a.id))));
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
            (if r.sort == "new" { e.created_at as f64 } else { rank(&rep, e, n, v.cited_by as usize) }, v)
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
    node.store.lock().unwrap().bump(&id, 1.0); // viewed
    let rep = node.reputation(e.community().unwrap_or(""));
    let st = node.store.lock().unwrap();
    Ok(EventResponse { item: Some(view(&st, &rep, &e, true, &node.keys.pubkey)), raw_json: serde_json::to_string(&*e).unwrap() })
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
    let mut st = node.store.lock().unwrap();
    let mut items: Vec<(f64, Item)> = hits
        .iter()
        .map(|(e, m)| {
            let rep = &reps[e.community().unwrap_or("")];
            let mut v = view(&st, rep, e, false, &node.keys.pubkey);
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
    for n in r.needs.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).take(10) {
        tags.push(t("need", n.chars().take(100).collect::<String>()));
    }
    let e = node.publish(kind, tags, r.content)?;
    Ok(ack(Some(&e.id)))
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
        other => return Err(format!("unknown aspect '{other}': use trust, relevance or capability")),
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
        rep.user.insert(k.pubkey, -0.4);
        assert!(view(&st, &rep, &e, false, &n.keys.pubkey).low_trust);
        let mut rep = RepTable::default();
        rep.event.insert(e.id, (-0.5, 3));
        let v = view(&st, &rep, &e, false, &n.keys.pubkey);
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
