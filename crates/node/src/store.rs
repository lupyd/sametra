//! In-memory indexed event cache backed by an append-only log.
//!
//! Retention model (see README): an event lives on this node while
//!  * it was authored by this node's identity, or
//!  * it is pinned here, or
//!  * it is small metadata (profiles, community definitions, ratings, capabilities), or
//!  * its author has been seen on the network within `author_ttl`, extended for
//!    popular content (cited, viewed, fetched, found by searches) by up to 10x
//!
//! and the cache is under `max_events`; when it is over, the least popular and
//! oldest unprotected events go first. Content whose author vanished therefore
//! fades unless it is pinned somewhere; popular content just fades more slowly.

use crate::event::*;
use crate::index::{Hit, Index};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

pub type Ev = Arc<Event>;

pub enum Insert {
    Stored(Ev),
    Duplicate(Ev),
    Rejected(&'static str),
}

pub struct Store {
    ev: HashMap<H32, Ev>,
    by_time: BTreeSet<(u64, H32)>,
    by_comm: HashMap<String, BTreeSet<(u64, H32)>>,
    refs: HashMap<H32, Vec<H32>>, // referenced id -> events with e/q tag to it
    repl: HashMap<(H32, u32, String), H32>,
    pub pinned: HashSet<H32>,
    pub last_seen: HashMap<H32, u64>,
    dir: PathBuf,
    log: Option<BufWriter<File>>,
    log_lines: usize,
    pub index: Index,
    /// local usage counter per event: (score at `t`, t); halves every POP_HALF_LIFE
    pop: HashMap<H32, (f32, u64)>,
}

fn is_metadata(kind: u32) -> bool {
    matches!(kind, K_PROFILE | K_COMMUNITY | K_RATING | K_CAPABILITY | 3)
}

/// Popularity halves every week without new use.
pub const POP_HALF_LIFE: u64 = 7 * 86400;

fn decayed(score: f32, at: u64, now: u64) -> f64 {
    score as f64 * 0.5f64.powf(now.saturating_sub(at) as f64 / POP_HALF_LIFE as f64)
}

impl Store {
    pub fn open(dir: PathBuf, pins: HashSet<H32>) -> std::io::Result<Store> {
        fs::create_dir_all(&dir)?;
        let mut s = Store {
            ev: HashMap::new(),
            by_time: BTreeSet::new(),
            by_comm: HashMap::new(),
            refs: HashMap::new(),
            repl: HashMap::new(),
            pinned: pins,
            last_seen: HashMap::new(),
            dir,
            log: None,
            log_lines: 0,
            index: Index::default(),
            pop: HashMap::new(),
        };
        if let Ok(f) = File::open(s.dir.join("events.pb")) {
            // Our own log: trusted, skip signature verification for fast start-up.
            let (mut r, mut buf) = (BufReader::with_capacity(256 * 1024, f), Vec::new());
            while let Ok(Some(())) = crate::pb::read_delimited(&mut r, &mut buf) {
                s.log_lines += 1;
                let Ok(p) = crate::pb::decode::<crate::pb::Event>(&buf) else { continue };
                if p.pubkey.is_empty() {
                    // tombstone written by `evict`
                    if let Ok(id) = <[u8; 32]>::try_from(p.id.as_slice()) {
                        s.remove(&H32(id));
                    }
                } else if let Some(e) = Event::from_pb(p) {
                    s.insert(e);
                }
            }
            // a torn final record (crash mid-write) is simply dropped by the next compaction
        }
        s.compact_if_needed(true)?;
        Ok(s)
    }

    fn open_log(&mut self) -> std::io::Result<()> {
        let f = OpenOptions::new().create(true).append(true).open(self.dir.join("events.pb"))?;
        self.log = Some(BufWriter::with_capacity(64 * 1024, f));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.ev.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ev.is_empty()
    }
    pub fn get(&self, id: &H32) -> Option<&Ev> {
        self.ev.get(id)
    }
    #[allow(dead_code)]
    pub fn contains(&self, id: &H32) -> bool {
        self.ev.contains_key(id)
    }
    pub fn communities(&self) -> impl Iterator<Item = (&String, usize)> {
        self.by_comm.iter().map(|(k, v)| (k, v.len()))
    }
    pub fn replaceable(&self, pk: &H32, kind: u32, d: &str) -> Option<&Ev> {
        self.repl.get(&(*pk, kind, d.to_string())).and_then(|id| self.ev.get(id))
    }
    pub fn referencing(&self, id: &H32) -> Vec<Ev> {
        self.refs.get(id).map(|v| v.iter().filter_map(|i| self.ev.get(i).cloned()).collect()).unwrap_or_default()
    }
    /// Direct replies of `id` (events whose parent is `id`).
    /// Direct replies only: ratings, deletions and job results also carry `e`
    /// tags pointing here, but they are not part of the conversation.
    pub fn children(&self, id: &H32) -> Vec<Ev> {
        let mut v: Vec<Ev> = self.referencing(id).into_iter().filter(|e| e.kind == K_REPLY && e.parent() == Some(*id)).collect();
        v.dedup_by_key(|e| e.id);
        v
    }
    pub fn descendant_count(&self, id: &H32, cap: usize) -> usize {
        let (mut n, mut stack) = (0, vec![*id]);
        while let Some(x) = stack.pop() {
            for c in self.children(&x) {
                n += 1;
                if n >= cap {
                    return n;
                }
                stack.push(c.id);
            }
        }
        n
    }
    pub fn iter(&self) -> impl Iterator<Item = &Ev> {
        self.by_time.iter().rev().filter_map(|(_, id)| self.ev.get(id))
    }

    pub fn insert(&mut self, e: Event) -> Insert {
        if let Some(x) = self.ev.get(&e.id) {
            return Insert::Duplicate(x.clone());
        }
        if e.is_ephemeral() {
            return Insert::Rejected("ephemeral");
        }
        if self.referencing(&e.id).iter().any(|d| d.kind == K_DELETE && d.pubkey == e.pubkey) {
            return Insert::Rejected("blocked: deleted by author");
        }
        let rkey = if e.is_replaceable() {
            Some((e.pubkey, e.kind, String::new()))
        } else if e.is_param_replaceable() {
            Some((e.pubkey, e.kind, e.d_tag().to_string()))
        } else {
            None
        };
        if let Some(k) = &rkey
            && let Some(old) = self.repl.get(k).and_then(|id| self.ev.get(id)).cloned() {
                if (old.created_at, old.id) >= (e.created_at, e.id) {
                    return Insert::Rejected("duplicate: have newer version");
                }
                self.remove(&old.id);
            }
        if e.kind == K_DELETE {
            for id in e.tags_named("e").filter_map(H32::from_hex).collect::<Vec<_>>() {
                if self.ev.get(&id).is_some_and(|t| t.pubkey == e.pubkey) {
                    self.remove(&id);
                    self.pinned.remove(&id);
                }
            }
        }
        let e = Arc::new(e);
        let ls = self.last_seen.entry(e.pubkey).or_insert(0);
        *ls = (*ls).max(e.created_at);
        self.by_time.insert((e.created_at, e.id));
        if let Some(c) = e.community() {
            self.by_comm.entry(c.to_string()).or_default().insert((e.created_at, e.id));
        }
        for t in &e.tags {
            if t.len() >= 2 && (t[0] == "e" || t[0] == "q")
                && let Some(r) = H32::from_hex(&t[1]) {
                    self.refs.entry(r).or_default().push(e.id);
                }
        }
        if let Some(k) = rkey {
            self.repl.insert(k, e.id);
        }
        self.index.add(&e);
        if let Some(w) = self.log.as_mut() {
            let _ = crate::pb::write_delimited(w, &e.to_pb());
            self.log_lines += 1;
        }
        self.ev.insert(e.id, e.clone());
        Insert::Stored(e)
    }

    pub fn remove(&mut self, id: &H32) -> Option<Ev> {
        let e = self.ev.remove(id)?;
        self.index.remove(id);
        self.pop.remove(id);
        self.by_time.remove(&(e.created_at, e.id));
        if let Some(c) = e.community()
            && let Some(s) = self.by_comm.get_mut(c) {
                s.remove(&(e.created_at, e.id));
                if s.is_empty() {
                    self.by_comm.remove(c);
                }
            }
        for t in &e.tags {
            if t.len() >= 2 && (t[0] == "e" || t[0] == "q")
                && let Some(r) = H32::from_hex(&t[1])
                    && let Some(v) = self.refs.get_mut(&r) {
                        v.retain(|x| x != id);
                        if v.is_empty() {
                            self.refs.remove(&r);
                        }
                    }
        }
        if e.is_replaceable() || e.is_param_replaceable() {
            let d = if e.is_replaceable() { String::new() } else { e.d_tag().to_string() };
            self.repl.remove(&(e.pubkey, e.kind, d));
        }
        Some(e)
    }

    /// Newest-first query. Picks the narrowest index available.
    pub fn query(&self, f: &Filter, hard_cap: usize) -> Vec<Ev> {
        let limit = f.limit.unwrap_or(hard_cap).min(hard_cap);
        let mut out = Vec::new();
        if limit == 0 {
            return out;
        }
        if let Some(q) = &f.search {
            // NIP-50: every token must match; results in relevance order, not time order
            return self
                .index
                .search(q, 5000)
                .into_iter()
                .filter(|h| h.all)
                .filter_map(|h| self.ev.get(&h.id))
                .filter(|e| f.matches(e))
                .take(limit)
                .cloned()
                .collect();
        }
        if let Some(ids) = f.ids.as_ref().filter(|ids| ids.iter().all(|i| i.len() == 64)) {
            out = ids.iter().filter_map(|i| H32::from_hex(i)).filter_map(|i| self.ev.get(&i)).filter(|e| f.matches(e)).cloned().collect();
            out.sort_by_key(|e| std::cmp::Reverse(e.created_at));
            out.truncate(limit);
            return out;
        }
        let lo = (f.since.unwrap_or(0), H32([0; 32]));
        let hi = (f.until.unwrap_or(u64::MAX), H32([0xff; 32]));
        let tag = |n: &str| f.tags.iter().find(|(k, _)| k == n).map(|(_, v)| v);
        if let Some(es) = tag("e") {
            let mut seen = HashSet::new();
            for r in es.iter().filter_map(|x| H32::from_hex(x)) {
                for e in self.referencing(&r) {
                    if seen.insert(e.id) && f.matches(&e) {
                        out.push(e);
                    }
                }
            }
            out.sort_by_key(|e| std::cmp::Reverse(e.created_at));
            out.truncate(limit);
            return out;
        }
        if let Some(cs) = tag("c") {
            for c in cs {
                if let Some(set) = self.by_comm.get(c) {
                    for (_, id) in set.range(lo..=hi).rev() {
                        let e = &self.ev[id];
                        if f.matches(e) {
                            out.push(e.clone());
                            if out.len() >= limit * cs.len() {
                                break;
                            }
                        }
                    }
                }
            }
            out.sort_by_key(|e| std::cmp::Reverse(e.created_at));
            out.truncate(limit);
            return out;
        }
        for (_, id) in self.by_time.range(lo..=hi).rev() {
            let e = &self.ev[id];
            if f.matches(e) {
                out.push(e.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    pub fn pin(&mut self, ids: impl IntoIterator<Item = H32>, on: bool) {
        for id in ids {
            if on {
                self.pinned.insert(id);
            } else {
                self.pinned.remove(&id);
            }
        }
    }

    /// Drop content of vanished authors and enforce the cache size cap.
    pub fn evict(&mut self, own: &H32, author_ttl: u64, max_events: usize) -> usize {
        let now = now();
        let protected = |s: &Store, e: &Event| e.pubkey == *own || s.pinned.contains(&e.id) || is_metadata(e.kind);
        // popular content outlives its author for longer: up to 10x the TTL
        let grace = |s: &Store, e: &Event| (1.0 + s.keep_score(&e.id)).min(10.0);
        let dead: Vec<H32> = self
            .ev
            .values()
            .filter(|e| !protected(self, e))
            .filter(|e| {
                let seen = self.last_seen.get(&e.pubkey).copied().unwrap_or(0);
                (seen as f64 + author_ttl as f64 * grace(self, e)) < now as f64
            })
            .map(|e| e.id)
            .collect();
        let mut n = dead.len();
        for id in dead {
            self.remove(&id);
            self.tombstone(&id);
        }
        if self.ev.len() > max_events {
            let target = max_events - max_events / 10;
            // least popular first; among equals, oldest first (a month of age ~ one doubling of use)
            let mut cands: Vec<(f64, H32)> = self
                .ev
                .values()
                .filter(|e| !protected(self, e))
                .map(|e| (self.keep_score(&e.id) - now.saturating_sub(e.created_at) as f64 / (30.0 * 86400.0), e.id))
                .collect();
            cands.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let victims: Vec<H32> = cands.into_iter().take(self.ev.len() - target).map(|x| x.1).collect();
            n += victims.len();
            for id in victims {
                self.remove(&id);
                self.tombstone(&id);
            }
        }
        // forget presence of authors with nothing left here and long gone
        let cutoff = now.saturating_sub(author_ttl);
        let ev = &self.ev;
        if self.last_seen.len() > 4 * ev.len() + 1024 {
            let alive: HashSet<H32> = ev.values().map(|e| e.pubkey).collect();
            self.last_seen.retain(|pk, t| *t >= cutoff || alive.contains(pk));
        }
        self.pop.retain(|_, (s, t)| decayed(*s, *t, now) >= 0.05);
        if self.index.needs_rebuild() {
            let evs: Vec<Ev> = self.ev.values().cloned().collect();
            self.index.rebuild(evs.iter().map(|e| &**e));
        }
        n
    }

    // ---- popularity ----------------------------------------------------------

    /// Record use of an event (viewed, fetched by a peer, returned by a search).
    pub fn bump(&mut self, id: &H32, w: f32) {
        if !self.ev.contains_key(id) {
            return;
        }
        let now = now();
        let p = self.pop.entry(*id).or_insert((0.0, now));
        *p = ((decayed(p.0, p.1, now) as f32) + w, now);
    }
    pub fn popularity(&self, id: &H32) -> f64 {
        self.pop.get(id).map(|(s, t)| decayed(*s, *t, now())).unwrap_or(0.0)
    }
    /// Events citing `id` with a `q` tag.
    pub fn cited_by(&self, id: &H32) -> usize {
        let h = id.hex();
        self.refs.get(id).map(|v| v.iter().filter_map(|i| self.ev.get(i)).filter(|e| e.tags_named("q").any(|q| q == h)).count()).unwrap_or(0)
    }
    /// ln(1 + use + 2·citations): what eviction weighs against age.
    pub fn keep_score(&self, id: &H32) -> f64 {
        (1.0 + self.popularity(id) + 2.0 * self.cited_by(id) as f64).ln()
    }
    pub fn export_popularity(&self, max: usize) -> Vec<crate::pb::Popularity> {
        let mut v: Vec<(&H32, &(f32, u64))> = self.pop.iter().collect();
        v.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        v.into_iter().take(max).map(|(id, (s, t))| crate::pb::Popularity { id: id.0.to_vec(), score: *s, at: *t }).collect()
    }
    pub fn load_popularity(&mut self, entries: &[crate::pb::Popularity]) {
        for p in entries {
            if let Ok(id) = <[u8; 32]>::try_from(p.id.as_slice()) {
                self.pop.insert(H32(id), (p.score, p.at));
            }
        }
    }

    /// Ranked full-text hits that are still cached.
    pub fn search(&self, query: &str, cap: usize) -> Vec<Hit> {
        self.index.search(query, cap).into_iter().filter(|h| self.ev.contains_key(&h.id)).collect()
    }

    /// Record a removal in the log so replay does not resurrect evicted events.
    /// (Replacements and NIP-09 deletions replay correctly on their own.)
    fn tombstone(&mut self, id: &H32) {
        if let Some(w) = self.log.as_mut() {
            let _ = crate::pb::write_delimited(w, &crate::pb::Event { id: id.0.to_vec(), ..Default::default() });
            self.log_lines += 1;
        }
    }

    pub fn touch(&mut self, pk: H32, t: u64) {
        let ls = self.last_seen.entry(pk).or_insert(0);
        *ls = (*ls).max(t);
    }

    pub fn flush(&mut self) {
        if let Some(w) = self.log.as_mut() {
            let _ = w.flush();
        }
    }

    /// Rewrite the log once it is mostly garbage (evicted/replaced events).
    pub fn compact_if_needed(&mut self, force_open: bool) -> std::io::Result<()> {
        if self.log_lines > 2 * self.ev.len() + 10_000 || (force_open && self.log_lines > self.ev.len() * 3 / 2 + 100) {
            self.flush();
            self.log = None;
            let tmp = self.dir.join("events.pb.tmp");
            {
                let mut w = BufWriter::new(File::create(&tmp)?);
                for (_, id) in &self.by_time {
                    crate::pb::write_delimited(&mut w, &self.ev[id].to_pb())?;
                }
                w.flush()?;
            }
            fs::rename(&tmp, self.dir.join("events.pb"))?;
            self.log_lines = self.ev.len();
        }
        if self.log.is_none() {
            self.open_log()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sametra-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn insert_query_replace_evict_persist() {
        let dir = tmpdir("store");
        let me = Keys::generate();
        let other = Keys::generate();
        let mut s = Store::open(dir.clone(), HashSet::new()).unwrap();
        let c = vec!["c".to_string(), "math".to_string()];
        let root = me.sign(K_POST, vec![c.clone()], "root".into());
        let reply = other.sign(
            K_REPLY,
            vec![c.clone(), vec!["e".into(), root.id.hex(), "".into(), "root".into()], vec!["e".into(), root.id.hex(), "".into(), "reply".into()]],
            "reply".into(),
        );
        assert!(matches!(s.insert(root.clone()), Insert::Stored(_)));
        assert!(matches!(s.insert(reply.clone()), Insert::Stored(_)));
        assert!(matches!(s.insert(reply.clone()), Insert::Duplicate(_)));
        assert_eq!(s.children(&root.id).len(), 1);
        assert_eq!(s.query(&Filter::from_value(&serde_json::json!({"#c":["math"]})), 100).len(), 2);

        // profile replaced by newer
        let p1 = other.sign(K_PROFILE, vec![], r#"{"name":"a"}"#.into());
        s.insert(p1);
        let mut p2 = other.sign(K_PROFILE, vec![], r#"{"name":"b"}"#.into());
        p2.created_at += 1; // signature not rechecked by store
        s.insert(p2);
        assert!(s.replaceable(&other.pubkey, K_PROFILE, "").unwrap().content.contains("\"b\""));

        // other's author went silent -> their reply is evicted, our root stays
        s.last_seen.insert(other.pubkey, 1);
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 1);
        assert!(s.contains(&root.id) && !s.contains(&reply.id));

        // pinned content survives
        s.insert(reply.clone());
        s.pin([reply.id], true);
        s.last_seen.insert(other.pubkey, 1);
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 0);

        s.flush();
        drop(s);
        let s2 = Store::open(dir.clone(), HashSet::from([reply.id])).unwrap();
        assert!(s2.contains(&root.id) && s2.contains(&reply.id) && s2.pinned.contains(&reply.id));
        assert!(s2.replaceable(&other.pubkey, K_PROFILE, "").unwrap().content.contains("\"b\""));
        let _ = fs::remove_dir_all(dir);
    }

    fn tg(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|t| t.iter().map(|s| s.to_string()).collect()).collect()
    }
    /// Signed event with a chosen timestamp (the store does not re-verify signatures).
    fn at(k: &Keys, kind: u32, tags: Vec<Vec<String>>, content: &str, t: u64) -> Event {
        let mut e = k.sign(kind, tags, content.into());
        e.created_at = t;
        e.id = Event::compute_id(&e.pubkey, t, kind, &e.tags, &e.content);
        e
    }
    fn reply_to(k: &Keys, root: &Event, parent: &Event, t: u64) -> Event {
        at(k, K_REPLY, tg(&[&["c", "m"], &["e", &root.id.hex(), "", "root"], &["e", &parent.id.hex(), "", "reply"]]), "r", t)
    }
    fn open(name: &str) -> (Store, PathBuf) {
        let d = tmpdir(name);
        (Store::open(d.clone(), HashSet::new()).unwrap(), d)
    }
    fn stored(i: Insert) -> bool {
        matches!(i, Insert::Stored(_))
    }
    fn q(s: &Store, v: serde_json::Value) -> Vec<Ev> {
        s.query(&Filter::from_value(&v), 1000)
    }

    #[test]
    fn deletion_is_author_only_and_sticky() {
        let (mut s, dir) = open("delete");
        let (a, b) = (Keys::generate(), Keys::generate());
        let post = at(&a, K_POST, tg(&[&["c", "m"]]), "p", 100);
        s.insert(post.clone());
        s.pin([post.id], true);
        // someone else cannot delete it
        assert!(stored(s.insert(at(&b, K_DELETE, tg(&[&["e", &post.id.hex()]]), "", 101))));
        assert!(s.contains(&post.id));
        // the author can, which also drops the pin
        assert!(stored(s.insert(at(&a, K_DELETE, tg(&[&["e", &post.id.hex()]]), "", 102))));
        assert!(!s.contains(&post.id) && !s.pinned.contains(&post.id));
        // and it cannot be re-inserted by gossip afterwards
        assert!(matches!(s.insert(post.clone()), Insert::Rejected(m) if m.contains("deleted")));
        // deletion survives a restart (log replay applies it again)
        s.flush();
        drop(s);
        let s = Store::open(dir.clone(), HashSet::new()).unwrap();
        assert!(!s.contains(&post.id));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ephemeral_events_are_not_stored() {
        let (mut s, dir) = open("ephemeral");
        let k = Keys::generate();
        assert!(matches!(s.insert(k.sign(K_HEARTBEAT, vec![], String::new())), Insert::Rejected("ephemeral")));
        assert_eq!(s.len(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parameterized_replaceable_keyed_by_d_tag() {
        let (mut s, dir) = open("prepl");
        let k = Keys::generate();
        let r = |d: &str, score: &str, t| at(&k, K_RATING, tg(&[&["d", d], &["c", "m"], &["score", score]]), "", t);
        assert!(stored(s.insert(r("x", "1", 10))));
        assert!(stored(s.insert(r("y", "2", 10))), "different d tag = different slot");
        assert!(stored(s.insert(r("x", "3", 20))));
        assert!(matches!(s.insert(r("x", "4", 15)), Insert::Rejected(_)), "older version loses");
        assert_eq!(s.len(), 2);
        assert_eq!(s.replaceable(&k.pubkey, K_RATING, "x").unwrap().tag("score"), Some("3"));
        // the replaced version is gone from every index
        assert_eq!(q(&s, serde_json::json!({"#c": ["m"]})).len(), 2);
        assert_eq!(s.iter().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn query_uses_each_index_correctly() {
        let (mut s, dir) = open("query");
        let (a, b) = (Keys::generate(), Keys::generate());
        let root = at(&a, K_POST, tg(&[&["c", "m"]]), "root", 100);
        let quote = at(&b, K_POST, tg(&[&["c", "bio"], &["q", &root.id.hex()]]), "quote", 105);
        s.insert(root.clone());
        s.insert(quote.clone());
        let mut replies = vec![];
        for i in 0..5 {
            let r = reply_to(&b, &root, &root, 110 + i);
            s.insert(r.clone());
            replies.push(r);
        }
        // ids: exact, newest first, limited
        let got = q(&s, serde_json::json!({"ids": [root.id.hex(), replies[4].id.hex()], "limit": 1}));
        assert_eq!(got.iter().map(|e| e.id).collect::<Vec<_>>(), [replies[4].id]);
        // ids by prefix fall back to a scan but still match
        assert_eq!(q(&s, serde_json::json!({"ids": [&root.id.hex()[..12]]}))[0].id, root.id);
        // #e: referencing events (replies and quotes), no duplicates although replies tag root twice
        let refs = q(&s, serde_json::json!({"#e": [root.id.hex()]}));
        assert_eq!(refs.len(), 5);
        assert_eq!(refs[0].id, replies[4].id);
        assert_eq!(q(&s, serde_json::json!({"#q": [root.id.hex()]})).len(), 1);
        // #c with time window and kind
        assert_eq!(q(&s, serde_json::json!({"#c": ["m"], "since": 111, "until": 113})).len(), 3);
        assert_eq!(q(&s, serde_json::json!({"#c": ["m"], "kinds": [11]}))[0].id, root.id);
        assert_eq!(q(&s, serde_json::json!({"#c": ["m", "bio"], "limit": 2})).len(), 2);
        // time index
        let all = q(&s, serde_json::json!({}));
        assert_eq!(all.len(), 7);
        assert!(all.windows(2).all(|w| w[0].created_at >= w[1].created_at));
        assert_eq!(q(&s, serde_json::json!({"authors": [a.pubkey.hex()]})).len(), 1);
        assert!(q(&s, serde_json::json!({"limit": 0})).is_empty());
        assert_eq!(s.query(&Filter::default(), 3).len(), 3, "hard cap");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn thread_structure() {
        let (mut s, dir) = open("tree");
        let k = Keys::generate();
        let root = at(&k, K_POST, tg(&[&["c", "m"]]), "root", 1);
        let r1 = reply_to(&k, &root, &root, 2);
        let r2 = reply_to(&k, &root, &r1, 3);
        let r3 = reply_to(&k, &root, &r2, 4);
        for e in [&root, &r1, &r2, &r3] {
            s.insert(e.clone());
        }
        assert_eq!(s.children(&root.id).iter().map(|e| e.id).collect::<Vec<_>>(), [r1.id], "r2/r3 tag root but are not its children");
        assert_eq!(s.children(&r1.id)[0].id, r2.id);
        assert_eq!(s.descendant_count(&root.id, 100), 3);
        assert_eq!(s.descendant_count(&root.id, 2), 2, "capped");
        assert_eq!(s.descendant_count(&r3.id, 100), 0);
        // ratings and deletions point at a message with `e` tags but are not replies
        let rating = at(&k, K_RATING, tg(&[&["d", "x"], &["e", &root.id.hex()], &["p", &k.pubkey.hex()], &["score", "5"]]), "great", 5);
        s.insert(rating);
        assert_eq!(s.children(&root.id).len(), 1);
        assert_eq!(s.descendant_count(&root.id, 100), 3);
        s.remove(&r2.id);
        assert_eq!(s.descendant_count(&root.id, 100), 1, "orphans are not reachable");
        assert!(s.remove(&r2.id).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn eviction_cap_spares_own_pinned_and_metadata() {
        let (mut s, dir) = open("cap");
        let (me, other) = (Keys::generate(), Keys::generate());
        let mine = at(&me, K_POST, tg(&[&["c", "m"]]), "mine", 1);
        let pinned = at(&other, K_POST, tg(&[&["c", "m"]]), "pinned", 2);
        let profile = at(&other, K_PROFILE, vec![], "{}", 3);
        for e in [&mine, &pinned, &profile] {
            s.insert(e.clone());
        }
        s.pin([pinned.id], true);
        let posts: Vec<Event> = (0..20).map(|i| at(&other, K_POST, tg(&[&["c", "m"]]), &format!("p{i}"), 10 + i)).collect();
        for p in &posts {
            s.insert(p.clone());
        }
        s.touch(other.pubkey, now()); // author is alive: only the cap applies
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 0);
        assert_eq!(s.len(), 23);
        let n = s.evict(&me.pubkey, 3600, 10);
        assert_eq!(s.len(), 9, "trimmed to 90% of the cap");
        assert_eq!(n, 14);
        assert!(s.contains(&mine.id) && s.contains(&pinned.id) && s.contains(&profile.id));
        assert!(s.contains(&posts[19].id) && !s.contains(&posts[0].id), "oldest go first");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn touch_keeps_authors_alive() {
        let (mut s, dir) = open("touch");
        let (me, other) = (Keys::generate(), Keys::generate());
        let old = at(&other, K_POST, tg(&[&["c", "m"]]), "old", 1000);
        s.insert(old.clone());
        assert_eq!(s.last_seen[&other.pubkey], 1000);
        s.touch(other.pubkey, now()); // e.g. a heartbeat
        s.touch(other.pubkey, 5); // never moves backwards
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 0);
        s.last_seen.insert(other.pubkey, now() - 7200);
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn community_index_counts() {
        let (mut s, dir) = open("comms");
        let k = Keys::generate();
        let a = at(&k, K_POST, tg(&[&["c", "m"]]), "a", 1);
        s.insert(a.clone());
        s.insert(at(&k, K_POST, tg(&[&["c", "m"]]), "b", 2));
        s.insert(at(&k, K_POST, tg(&[&["c", "bio"]]), "c", 3));
        let mut c: Vec<(String, usize)> = s.communities().map(|(k, n)| (k.clone(), n)).collect();
        c.sort();
        assert_eq!(c, [("bio".to_string(), 1), ("m".to_string(), 2)]);
        let bio = s.query(&Filter::from_value(&serde_json::json!({"#c": ["bio"]})), 10)[0].id;
        s.remove(&bio);
        assert_eq!(s.communities().count(), 1, "empty community index is dropped");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn compaction_and_torn_tail() {
        let (mut s, dir) = open("compact");
        let (me, other) = (Keys::generate(), Keys::generate());
        let keep: Vec<Event> = (0..20).map(|i| at(&me, K_POST, tg(&[&["c", "m"]]), &format!("k{i}"), 10 + i)).collect();
        for e in &keep {
            s.insert(e.clone());
        }
        for i in 0..400 {
            s.insert(at(&other, K_POST, tg(&[&["c", "m"]]), &format!("x{i}"), 100 + i));
        }
        s.last_seen.insert(other.pubkey, 1);
        assert_eq!(s.evict(&me.pubkey, 3600, 100_000), 400);
        s.flush();
        let log = dir.join("events.pb");
        let before = fs::metadata(&log).unwrap().len();
        drop(s);
        // simulate a crash mid-append
        let mut f = OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(&[50, 1, 2, 3]).unwrap();
        drop(f);
        let s = Store::open(dir.clone(), HashSet::new()).unwrap();
        assert_eq!(s.len(), 20);
        assert!(keep.iter().all(|e| s.contains(&e.id)));
        let after = fs::metadata(&log).unwrap().len();
        assert!(after * 5 < before, "log was compacted on open: {before} -> {after}");
        assert!(!dir.join("events.pb.tmp").exists());
        drop(s);
        // the compacted log reloads to the same state
        let s = Store::open(dir.clone(), HashSet::new()).unwrap();
        assert_eq!(s.len(), 20);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn popularity_decays_persists_and_counts_citations() {
        let (mut s, dir) = open("pop");
        let k = Keys::generate();
        let a = at(&k, K_POST, tg(&[&["c", "m"]]), "a", now());
        s.insert(a.clone());
        s.bump(&a.id, 3.0);
        s.bump(&a.id, 1.0);
        assert!((s.popularity(&a.id) - 4.0).abs() < 1e-3);
        s.bump(&H32([7; 32]), 5.0); // unknown events are ignored
        assert_eq!(s.popularity(&H32([7; 32])), 0.0);
        assert!((decayed(4.0, 0, POP_HALF_LIFE) - 2.0).abs() < 1e-9, "halves every half-life");
        let exported = s.export_popularity(10);
        assert_eq!(exported.len(), 1);
        let (mut s2, dir2) = open("pop2");
        s2.insert(a.clone());
        s2.load_popularity(&exported);
        assert!((s2.popularity(&a.id) - 4.0).abs() < 1e-3);
        // citations: q tags only (replies don't count)
        let cite = at(&k, K_POST, tg(&[&["c", "m"], &["q", &a.id.hex()]]), "cites a", now());
        let reply = reply_to(&k, &a, &a, now());
        s.insert(cite);
        s.insert(reply);
        assert_eq!(s.cited_by(&a.id), 1);
        assert!(s.keep_score(&a.id) > s.keep_score(&H32([1; 32])));
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(dir2);
    }

    #[test]
    fn popular_content_outlives_its_author_longer() {
        let (mut s, dir) = open("grace");
        let (me, gone) = (Keys::generate(), Keys::generate());
        let plain = at(&gone, K_POST, tg(&[&["c", "m"]]), "plain", 100);
        let popular = at(&gone, K_POST, tg(&[&["c", "m"]]), "popular", 101);
        s.insert(plain.clone());
        s.insert(popular.clone());
        s.bump(&popular.id, 20.0);
        // author last seen 2 TTLs ago: plain content is gone, popular content stays
        s.last_seen.insert(gone.pubkey, now() - 7200);
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 1);
        assert!(!s.contains(&plain.id) && s.contains(&popular.id));
        // ...but not forever: grace is capped at 10 TTLs
        s.last_seen.insert(gone.pubkey, now() - 11 * 3600);
        assert_eq!(s.evict(&me.pubkey, 3600, 1000), 1);
        assert!(!s.contains(&popular.id));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_pressure_drops_least_popular_first() {
        let (mut s, dir) = open("pressure");
        let (me, other) = (Keys::generate(), Keys::generate());
        let t = now();
        let old_cited = at(&other, K_POST, tg(&[&["c", "m"]]), "old but cited", t - 86400);
        s.insert(old_cited.clone());
        let posts: Vec<Event> = (0..15).map(|i| at(&other, K_POST, tg(&[&["c", "m"]]), &format!("p{i}"), t - 3600 + i)).collect();
        for p in &posts {
            s.insert(p.clone());
        }
        let hot = posts[0].clone();
        for _ in 0..3 {
            s.insert(at(&other, K_POST, tg(&[&["c", "x"], &["q", &old_cited.id.hex()]]), &format!("cite {}", s.len()), t));
        }
        s.bump(&hot.id, 50.0);
        s.touch(other.pubkey, t);
        s.evict(&me.pubkey, 3600, 10);
        assert_eq!(s.len(), 9);
        assert!(s.contains(&old_cited.id), "cited content survives the cap");
        assert!(s.contains(&hot.id), "used content survives the cap");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nip50_queries_use_the_index() {
        let (mut s, dir) = open("nip50");
        let k = Keys::generate();
        let a = at(&k, K_POST, tg(&[&["c", "bio"], &["title", "CRISPR screen"]]), "off-target effects", 10);
        let b = at(&k, K_REPLY, tg(&[&["c", "bio"]]), "crispr reply", 20);
        let c = at(&k, K_POST, tg(&[&["c", "chem"]]), "crispr reagents", 30);
        for e in [&a, &b, &c] {
            s.insert(e.clone());
        }
        let ids = |v: serde_json::Value| q(&s, v).iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(serde_json::json!({"search": "crispr", "#c": ["bio"], "kinds": [11]})), [a.id]);
        assert_eq!(ids(serde_json::json!({"search": "crispr"})).len(), 3);
        assert_eq!(ids(serde_json::json!({"search": "crispr effects"})), [a.id], "all tokens must match");
        assert_eq!(ids(serde_json::json!({"search": "crispr", "limit": 1})).len(), 1);
        assert_eq!(ids(serde_json::json!({"search": "crispr"}))[0], a.id, "relevance order: title match first");
        s.remove(&a.id);
        assert!(q(&s, serde_json::json!({"search": "effects"})).is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
