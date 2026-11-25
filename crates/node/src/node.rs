//! Node state, gossip, and peer connections.
//!
//! Every connection is symmetric: we act as a nostr relay for whatever the
//! remote asks (REQ/EVENT/CLOSE) and as a client towards relays we dialed
//! (subscribe + push). New events flood to neighbours once, de-duplicated by
//! id, so the mesh converges without any coordinator.

use crate::event::*;
use crate::rep::{self, Model, RepTable};
use crate::store::{Ev, Insert, Store};
use futures_util::{SinkExt, StreamExt};
use sametra_transport::codec::{Msg, Wire, parse_frame};
use sametra_transport::{Dialer, Message, WebSocketStream};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use crate::pb;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

pub struct Config {
    pub listen: String,
    pub data: PathBuf,
    pub peers: Vec<String>,
    pub public_addr: Option<String>,
    /// Posts whose subtrees a scoped node keeps (`--follow`).
    pub follow: Vec<H32>,
    pub relay_all: bool,
    pub max_events: usize,
    pub max_peers: usize,
    /// What to forget first when the cache is full.
    pub keep: Keep,
    /// Forget unprotected events older than this (seconds); None: only when full.
    pub max_age: Option<u64>,
    pub api_token: Option<String>,
    /// Serve HTTPS/WSS on the same port as plain HTTP/WS (PEM chain and key).
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Extra CA certificates (PEM) trusted when dialing wss:// peers.
    pub tls_ca: Vec<PathBuf>,
    /// Networks treated like localhost for write tools (e.g. the Docker bridge
    /// gateway the host's requests come from). Host and Origin checks still apply.
    pub trusted: Vec<(std::net::IpAddr, u8)>,
}

impl Config {
    /// Is a request from `ip` as good as one from this machine?
    pub fn trusts(&self, ip: std::net::IpAddr) -> bool {
        ip.is_loopback() || self.trusted.iter().any(|(net, bits)| in_net(ip, *net, *bits))
    }
}

fn in_net(ip: std::net::IpAddr, net: std::net::IpAddr, bits: u8) -> bool {
    use std::net::IpAddr::*;
    match (ip, net) {
        (V4(a), V4(n)) => bits == 0 || u32::from(a) >> (32 - bits as u32) == u32::from(n) >> (32 - bits as u32),
        (V6(a), V6(n)) => bits == 0 || u128::from(a) >> (128 - bits as u32) == u128::from(n) >> (128 - bits as u32),
        _ => false,
    }
}

/// `10.42.0.1/32`, `fd00::/8` or a bare address.
pub fn parse_cidr(s: &str) -> Result<(std::net::IpAddr, u8), String> {
    let (a, b) = s.trim().split_once('/').unwrap_or((s.trim(), ""));
    let ip: std::net::IpAddr = a.parse().map_err(|_| format!("not an address: '{s}'"))?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let bits = if b.is_empty() { max } else { b.parse().ok().filter(|n| *n <= max).ok_or_else(|| format!("bad prefix length in '{s}'"))? };
    Ok((ip, bits))
}

impl Default for Config {
    /// The CLI defaults (see `sametra --help`).
    fn default() -> Config {
        Config {
            listen: "0.0.0.0:42424".into(),
            data: PathBuf::from("."),
            peers: vec![],
            public_addr: None,
            follow: vec![],
            relay_all: true,
            max_events: 200_000,
            max_peers: 8,
            keep: Keep::Balanced,
            max_age: None,
            api_token: None,
            tls_cert: None,
            tls_key: None,
            tls_ca: vec![],
            trusted: vec![],
        }
    }
}

/// What a node forgets first when its cache is full: its own choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// standing (author reputation and post score) and use (citations, views, searches), minus age
    Balanced,
    /// standing first; use only breaks ties
    Reputation,
    /// use and citations, minus age
    Popularity,
    /// newest first
    Recent,
}

impl std::str::FromStr for Keep {
    type Err = String;
    fn from_str(s: &str) -> Result<Keep, String> {
        Ok(match s {
            "balanced" => Keep::Balanced,
            "reputation" => Keep::Reputation,
            "popularity" => Keep::Popularity,
            "recent" => Keep::Recent,
            _ => return Err(format!("--keep: '{s}' (use balanced, reputation, popularity or recent)")),
        })
    }
}

/// Commands other tasks can send down a connection.
pub enum Out {
    /// subscription id, filters, hops a search may still travel
    Req(String, Vec<Filter>, u32),
    Close(String),
    /// results of a forwarded search for a subscription, followed by EOSE
    Events(String, Vec<Ev>),
}

/// How long to wait for peers answering a search that may travel `hops` more hops.
/// Each hop waits less than the one before it, so answers flow back in time.
pub fn search_timeout(hops: u32) -> Duration {
    Duration::from_millis(600 * (hops as u64 + 1) + 400)
}
/// Peers asked per lookup (bounds fan-out).
const FANOUT: usize = 16;

pub struct PeerHandle {
    pub tx: mpsc::Sender<Out>,
    pub outbound: bool,
    /// remote speaks sametra binary frames (always true for peers we dialed)
    pub sametra: Arc<AtomicBool>,
    pub addr: String,
    pub since: u64,
}

#[derive(Default)]
struct AddrInfo {
    fails: u32,
    next_try: u64,
}

pub struct Node {
    pub cfg: Config,
    pub keys: Keys,
    pub store: Mutex<Store>,
    /// Posts whose subtrees this node keeps when it does not relay everything.
    pub following: Mutex<BTreeSet<H32>>,
    pub peers: Mutex<HashMap<u64, PeerHandle>>,
    book: Mutex<HashMap<String, AddrInfo>>,
    pub pending: Mutex<HashMap<String, mpsc::Sender<Option<Ev>>>>,
    seen: Mutex<HashMap<H32, u64>>, // recent ephemeral ids
    /// message scores and contextual standing, rebuilt when ratings change
    model_cache: Mutex<Option<(Arc<Model>, u64)>>,
    pub memory: Mutex<BTreeMap<String, String>>,
    /// notifications up to this time have been seen
    pub notifications_read: AtomicU64,
    /// recently seen search subscription ids (loop guard for forwarded searches)
    searches: Mutex<HashMap<String, u64>>,
    /// concurrent outgoing forwarded searches
    search_slots: Arc<tokio::sync::Semaphore>,
    /// capability reputation table, rebuilt on demand (see api.rs)
    pub cap_cache: Mutex<Option<Arc<CapScores>>>,
    /// humanity table (network-wide "human" ratings), rebuilt on demand
    human_cache: Mutex<Option<Arc<RepTable>>>,
    tx: broadcast::Sender<(Ev, u64)>,
    next_conn: AtomicU64,
    dialer: Dialer,
    /// Present when the node serves TLS (`tls_cert` + `tls_key`).
    #[cfg(feature = "tls")]
    pub tls: Option<sametra_transport::tls::TlsAcceptor>,
    /// The IPFS daemon (Kubo) this node uses, if configured.
    #[cfg(feature = "ipfs")]
    pub ipfs: std::sync::OnceLock<Arc<crate::ipfs::Ipfs>>,
}

/// (provider, capability id) -> (reputation, raters)
pub type CapScores = HashMap<(H32, String), (f64, usize)>;

pub const ORIGIN_LOCAL: u64 = 0;
/// Relayed by every node, whatever it follows: small metadata, and capability
/// jobs (so requests and results find their way through scoped nodes).
const GLOBAL_KINDS: [u32; 8] = [K_PROFILE, K_RATING, K_DELETE, K_CAPABILITY, K_JOB, K_JOB_RESULT, K_JOB_FEEDBACK, K_FILE];

/// Closeness from the post tree: posts in the same tree are close, the rest far.
/// (The fallback when no embeddings are available.)
pub struct TreeCloseness {
    root_of: HashMap<H32, H32>,
}
/// How much standing in one tree counts in another.
pub const FAR: f64 = 0.1;
impl rep::Closeness for TreeCloseness {
    fn closeness(&self, a: &H32, b: &H32) -> f64 {
        let r = |x: &H32| self.root_of.get(x).copied().unwrap_or(*x);
        if a == b || r(a) == r(b) { 1.0 } else { FAR }
    }
}

impl Node {
    pub fn new(cfg: Config, keys: Keys) -> std::io::Result<Arc<Node>> {
        let dialer = Dialer::new(&cfg.tls_ca).map_err(std::io::Error::other)?;
        #[cfg(feature = "tls")]
        let tls = match (&cfg.tls_cert, &cfg.tls_key) {
            (Some(c), Some(k)) => Some(sametra_transport::tls::acceptor(c, k).map_err(std::io::Error::other)?),
            (None, None) => None,
            _ => return Err(std::io::Error::other("TLS needs both a certificate and a key")),
        };
        #[cfg(not(feature = "tls"))]
        if cfg.tls_cert.is_some() || cfg.tls_key.is_some() {
            return Err(std::io::Error::other("this build has no TLS support (feature `tls`)"));
        }
        std::fs::create_dir_all(&cfg.data)?;
        let state: pb::NodeState = std::fs::read(cfg.data.join("state.pb")).ok().and_then(|b| pb::decode(&b).ok()).unwrap_or_default();
        let pins: HashSet<H32> = state.pins.iter().filter_map(|p| Some(H32(p.as_slice().try_into().ok()?))).collect();
        let mut store = Store::open(cfg.data.join("store"), pins)?;
        store.load_popularity(&state.popularity);
        let mut following: BTreeSet<H32> = state.following.iter().filter_map(|b| Some(H32(b.as_slice().try_into().ok()?))).collect();
        following.extend(cfg.follow.iter().copied());
        let mut book: HashMap<String, AddrInfo> = state.peers.into_iter().map(|p| (p, AddrInfo::default())).collect();
        for p in &cfg.peers {
            book.entry(p.clone()).or_default();
        }
        let memory = std::fs::read(cfg.data.join("memory.pb"))
            .ok()
            .and_then(|b| pb::decode::<pb::MemoryResponse>(&b).ok())
            .map(|m| m.entries.into_iter().map(|e| (e.key, e.value)).collect())
            .unwrap_or_default();
        let (tx, _) = broadcast::channel(4096);
        let node = Arc::new(Node {
            cfg,
            keys,
            store: Mutex::new(store),
            following: Mutex::new(following),
            peers: Mutex::new(HashMap::new()),
            book: Mutex::new(book),
            pending: Mutex::new(HashMap::new()),
            seen: Mutex::new(HashMap::new()),
            model_cache: Mutex::new(None),
            memory: Mutex::new(memory),
            notifications_read: AtomicU64::new(state.notifications_read),
            searches: Mutex::new(HashMap::new()),
            search_slots: Arc::new(tokio::sync::Semaphore::new(8)),
            cap_cache: Mutex::new(None),
            human_cache: Mutex::new(None),
            tx,
            next_conn: AtomicU64::new(1),
            dialer,
            #[cfg(feature = "tls")]
            tls,
            #[cfg(feature = "ipfs")]
            ipfs: std::sync::OnceLock::new(),
        });
        node.save_state();
        Ok(node)
    }

    /// Persist followed posts, address book and pins (NodeState protobuf, atomic replace).
    pub fn save_state(&self) {
        let (pins, popularity) = {
            let store = self.store.lock().unwrap();
            (store.pinned.iter().map(|p| p.0.to_vec()).collect(), store.export_popularity(5000))
        };
        let st = pb::NodeState {
            following: self.following.lock().unwrap().iter().map(|h| h.0.to_vec()).collect(),
            peers: self.book.lock().unwrap().iter().filter(|(_, i)| i.fails < 8).map(|(a, _)| a.clone()).collect(),
            pins,
            popularity,
            notifications_read: self.notifications_read.load(Ordering::Relaxed),
        };
        write_atomic(&self.cfg.data.join("state.pb"), &pb::encode(&st));
    }
    pub fn save_memory(&self) {
        let m = pb::MemoryResponse {
            entries: self.memory.lock().unwrap().iter().map(|(k, v)| pb::MemoryEntry { key: k.clone(), value: v.clone() }).collect(),
        };
        write_atomic(&self.cfg.data.join("memory.pb"), &pb::encode(&m));
    }

    fn interested(&self, e: &Event) -> bool {
        self.cfg.relay_all
            || e.pubkey == self.keys.pubkey
            || GLOBAL_KINDS.contains(&e.kind)
            || self.follows(e)
            // capability requests, results and feedback addressed to us
            || e.tags_named("p").any(|p| H32::from_hex(p) == Some(self.keys.pubkey))
            // mentions travel through every node so they reach the people named
            || is_mention(e)
    }

    /// Is the event a followed post, or under one?
    pub fn follows(&self, e: &Event) -> bool {
        let f = self.following.lock().unwrap();
        !f.is_empty() && (f.contains(&e.id) || e.ancestors().iter().any(|a| f.contains(a)))
    }

    /// Validate, store and fan out an event. `force` stores even if outside our interests
    /// (explicit fetches). Returns the stored event and whether it was new.
    pub fn ingest(&self, e: Event, origin: u64, force: bool) -> Result<(Ev, bool), String> {
        if e.created_at > now() + 600 {
            return Err("invalid: created_at too far in future".into());
        }
        if e.is_ephemeral() {
            {
                let mut seen = self.seen.lock().unwrap();
                if seen.contains_key(&e.id) {
                    return Ok((Arc::new(e), false));
                }
                e.validate()?;
                seen.insert(e.id, now());
            }
            if e.kind == K_HEARTBEAT {
                if e.created_at + 900 < now() {
                    return Err("invalid: stale heartbeat".into());
                }
                self.store.lock().unwrap().touch(e.pubkey, e.created_at);
                #[cfg(feature = "ipfs")]
                if e.pubkey != self.keys.pubkey
                    && let Some(ipfs) = self.ipfs.get()
                    && let Some(tag) = e.tags.iter().find(|t| t.first().is_some_and(|n| n == "ipfs"))
                    && let Ok(rt) = tokio::runtime::Handle::try_current()
                {
                    let (ipfs, tag) = (ipfs.clone(), tag.clone());
                    rt.spawn(async move { ipfs.learn(&tag).await });
                }
                if e.pubkey != self.keys.pubkey
                    && let Some(addr) = e.tag("r").filter(|a| a.starts_with("ws://") || a.starts_with("wss://")) {
                        let mut book = self.book.lock().unwrap();
                        if book.len() < 2000 && !book.contains_key(addr) {
                            tracing::debug!(%addr, "learned a peer address");
                            book.insert(addr.to_string(), Default::default());
                        }
                    }
            }
            let e = Arc::new(e);
            let _ = self.tx.send((e.clone(), origin));
            return Ok((e, true));
        }
        if let Some(x) = self.store.lock().unwrap().get(&e.id) {
            return Ok((x.clone(), false));
        }
        if !force && !self.interested(&e) {
            return Err("blocked: not following this topic".into());
        }
        e.validate()?;
        let rating = e.kind == K_RATING;
        if matches!(e.kind, K_RATING | K_JOB_RESULT) {
            *self.cap_cache.lock().unwrap() = None;
        }
        if e.kind == K_RATING && e.tag("aspect") == Some("human") {
            *self.human_cache.lock().unwrap() = None;
        }
        let r = self.store.lock().unwrap().insert(e);
        match r {
            Insert::Stored(e) => {
                if rating {
                    *self.model_cache.lock().unwrap() = None;
                }
                // keep the files posts link to, and help serve them
                #[cfg(feature = "ipfs")]
                if e.kind == K_POST
                    && let Some(ipfs) = self.ipfs.get()
                    && let Ok(rt) = tokio::runtime::Handle::try_current()
                {
                    for cid in crate::files::linked_cids(e.text()) {
                        let ipfs = ipfs.clone();
                        rt.spawn(async move { ipfs.cache(cid).await });
                    }
                }
                let _ = self.tx.send((e.clone(), origin));
                Ok((e, true))
            }
            Insert::Duplicate(e) => Ok((e, false)),
            Insert::Rejected(m) => Err(m.into()),
        }
    }

    /// Sign and publish a text event (markdown).
    pub fn publish(&self, kind: u32, tags: Vec<Vec<String>>, content: String) -> Result<Ev, String> {
        self.publish_data(kind, tags, content.into_bytes(), "")
    }

    /// Sign and publish an event with content of any media type.
    pub fn publish_data(&self, kind: u32, tags: Vec<Vec<String>>, content: Vec<u8>, content_type: &str) -> Result<Ev, String> {
        let mut t = now();
        // A replacement must be strictly newer than what it replaces: same-second
        // ties are broken by lowest id, which would drop quick edits at random.
        let probe = Event { id: H32([0; 32]), pubkey: self.keys.pubkey, created_at: t, kind, tags, content, content_type: content_type.to_string(), sig: H64([0; 64]) };
        if probe.is_replaceable() || probe.is_param_replaceable() {
            let d = if probe.is_replaceable() { "" } else { probe.d_tag() };
            if let Some(old) = self.store.lock().unwrap().replaceable(&self.keys.pubkey, kind, d) {
                t = t.max(old.created_at + 1);
            }
        }
        let e = self.keys.sign_data(kind, probe.tags, probe.content, content_type, t);
        self.ingest(e, ORIGIN_LOCAL, true).map(|(e, _)| e)
    }

    /// This node's ranking of what to keep (see [`Keep`]). The model is built
    /// first, so ranking needs no further locks; only when the cache is full or an
    /// age limit is set does it cost anything.
    pub fn ranking(&self) -> impl Fn(&Store, &Event) -> f64 + use<> {
        let (full, keep) = (self.store.lock().unwrap().len() > self.cfg.max_events, self.cfg.keep);
        let model = (full && matches!(keep, Keep::Balanced | Keep::Reputation)).then(|| self.model());
        move |s: &Store, e: &Event| {
            // standing: the post's own score, plus its author's standing near it
            let standing = || model.as_ref().map(|m| m.event(&e.id) + m.near(&e.pubkey, &e.id)).unwrap_or(0.0);
            match keep {
                Keep::Balanced => 2.0 * standing() + s.usage_rank(e),
                Keep::Reputation => 100.0 * standing() + 0.01 * s.usage_rank(e),
                Keep::Popularity => s.usage_rank(e),
                Keep::Recent => e.created_at as f64,
            }
        }
    }

    /// Message scores and contextual standing (see rep::Model), rebuilt when a
    /// rating arrives, and at most once a minute as posts arrive.
    pub fn model(&self) -> Arc<Model> {
        if let Some((m, at)) = self.model_cache.lock().unwrap().clone()
            && at + 60 > now()
        {
            return m;
        }
        let (ratings, authors, roots) = {
            let st = self.store.lock().unwrap();
            let ratings = st.query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, usize::MAX);
            let mut authors: HashMap<H32, H32> = HashMap::new();
            let mut roots: HashMap<H32, H32> = HashMap::new();
            for e in st.query(&Filter { kinds: Some(vec![K_POST]), ..Default::default() }, usize::MAX) {
                authors.insert(e.id, e.pubkey);
                roots.insert(e.id, e.root().unwrap_or(e.id));
            }
            (ratings, authors, roots)
        };
        let close: Arc<dyn rep::Closeness> = self.closeness(roots);
        let m = Arc::new(rep::compute(&ratings, |id| authors.get(id).copied(), close, now()));
        *self.model_cache.lock().unwrap() = Some((m.clone(), now()));
        m
    }

    /// How close two posts are: the post tree (embeddings come next).
    fn closeness(&self, roots: HashMap<H32, H32>) -> Arc<dyn rep::Closeness> {
        Arc::new(TreeCloseness { root_of: roots })
    }

    /// Who the network vouches is human (see rep::humanity), cached until a new
    /// "human" rating arrives.
    pub fn humanity(&self) -> Arc<RepTable> {
        if let Some(t) = self.human_cache.lock().unwrap().clone() {
            return t;
        }
        let ratings: Vec<Ev> = {
            let st = self.store.lock().unwrap();
            st.query(&Filter { kinds: Some(vec![K_RATING]), ..Default::default() }, usize::MAX).into_iter().filter(|e| e.tag("aspect") == Some("human")).collect()
        };
        let t = Arc::new(rep::humanity(&ratings));
        *self.human_cache.lock().unwrap() = Some(t.clone());
        t
    }

    fn heartbeat_event(&self) -> Event {
        let mut tags = vec![];
        if let Some(a) = &self.cfg.public_addr {
            tags.push(vec!["r".to_string(), a.clone()]);
        }
        // our IPFS daemon, so Sametra nodes connect theirs and swap files directly
        #[cfg(feature = "ipfs")]
        tags.extend(self.ipfs.get().and_then(|i| i.heartbeat_tag()));
        self.keys.sign(K_HEARTBEAT, tags, String::new())
    }
    pub fn heartbeat(&self) {
        let _ = self.ingest(self.heartbeat_event(), ORIGIN_LOCAL, true);
    }

    /// What we want from relays we dial.
    fn sync_filters(&self, since: u64) -> Vec<Filter> {
        let hb = Filter { kinds: Some(vec![K_HEARTBEAT]), since: Some(now()), ..Default::default() };
        if self.cfg.relay_all {
            return vec![Filter { since: Some(since), limit: Some(20000), ..Default::default() }, hb];
        }
        let following: Vec<String> = self.following.lock().unwrap().iter().map(H32::hex).collect();
        let mut f = vec![Filter { kinds: Some(GLOBAL_KINDS.to_vec()), since: Some(since), ..Default::default() }, hb];
        f.push(Filter { tags: vec![("p".into(), vec![self.keys.pubkey.hex()])], since: Some(since), ..Default::default() });
        // mentions of anyone: relayed by every node so they reach the people named
        f.push(Filter { tags: vec![("l".into(), vec!["mention".into(), "addressed".into()])], since: Some(since), ..Default::default() });
        // followed posts and everything under them (every post names its ancestors)
        if !following.is_empty() {
            f.push(Filter { ids: Some(following.clone()), ..Default::default() });
            f.push(Filter { tags: vec![("e".into(), following)], since: Some(since), limit: Some(20000), ..Default::default() });
        }
        f
    }

    /// Ask every connected sametra peer; collect results until all answered EOSE or timeout.
    pub async fn remote_fetch(&self, filters: Vec<Filter>, timeout: Duration) -> Vec<Ev> {
        self.ask_peers(filters, timeout, 0, None, None).await
    }

    /// Search the network: peers answer from their index and forward the query
    /// up to `hops` more hops. Lossy by design: whatever arrives in time.
    pub async fn remote_search(&self, filters: Vec<Filter>, hops: u32) -> Vec<Ev> {
        let hops = hops.min(sametra_transport::codec::MAX_HOPS);
        let mut rnd = [0u8; 8];
        getrandom::getrandom(&mut rnd).unwrap();
        let sid = format!("q-{}", hex::encode(rnd));
        self.searches.lock().unwrap().insert(sid.clone(), now());
        self.ask_peers(filters, search_timeout(hops), hops, None, Some(sid)).await
    }

    /// Send `filters` to up to FANOUT sametra peers (except `exclude`) and collect
    /// the events they return until every one sent EOSE or `timeout` passed.
    pub async fn ask_peers(&self, filters: Vec<Filter>, timeout: Duration, hops: u32, exclude: Option<u64>, sid: Option<String>) -> Vec<Ev> {
        let sid = sid.unwrap_or_else(|| {
            let mut rnd = [0u8; 8];
            getrandom::getrandom(&mut rnd).unwrap();
            format!("fetch-{}", hex::encode(rnd))
        });
        let (tx, mut rx) = mpsc::channel(4096);
        self.pending.lock().unwrap().insert(sid.clone(), tx);
        let txs: Vec<_> = self
            .peers
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, p)| p.sametra.load(Ordering::Relaxed) && Some(**id) != exclude)
            .take(FANOUT)
            .map(|(_, p)| p.tx.clone())
            .collect();
        let mut waiting = txs.iter().filter(|t| t.try_send(Out::Req(sid.clone(), filters.clone(), hops)).is_ok()).count();
        let asked = waiting;
        let mut out = Vec::new();
        let started = tokio::time::Instant::now();
        let deadline = started + timeout;
        while waiting > 0 {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(Some(e))) => out.push(e),
                Ok(Some(None)) => waiting -= 1,
                _ => break,
            }
        }
        self.pending.lock().unwrap().remove(&sid);
        for t in txs {
            let _ = t.try_send(Out::Close(sid.clone()));
        }
        let mut seen = HashSet::new();
        out.retain(|e| seen.insert(e.id));
        tracing::debug!(%sid, hops, asked, unanswered = waiting, found = out.len(), ms = started.elapsed().as_millis() as u64, "asked peers");
        out
    }

    pub fn peer_list(&self) -> Vec<pb::Peer> {
        self.peers.lock().unwrap().values().map(|p| pb::Peer { addr: p.addr.clone(), outbound: p.outbound, since: p.since }).collect()
    }
}

/// Is the event addressed to people wherever they are: it @mentions someone
/// (`["p", pubkey, relay, "mention"]`), or is labelled `addressed` (a reply written
/// outside the topics its author's node follows)? Such events travel through every
/// node, labelled (`["l", ...]`) so scoped nodes can subscribe to them.
pub fn is_mention(e: &Event) -> bool {
    e.tags.iter().any(|t| t.len() >= 4 && t[0] == "p" && t[3] == "mention")
        || (e.tags.iter().any(|t| t.len() >= 2 && t[0] == "l" && t[1] == "addressed") && e.tags_named("p").next().is_some())
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)) {
        tracing::warn!(path = %path.display(), error = %e, "saving node state failed");
    }
}

// ---- connection handling ---------------------------------------------------

pub async fn serve_ws<S>(node: Arc<Node>, stream: S, part: Vec<u8>, addr: String)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ws = sametra_transport::accept(stream, part).await;
    tracing::debug!(%addr, "inbound websocket");
    run_conn(node, ws, false, addr.clone()).await;
    tracing::debug!(%addr, "inbound websocket closed");
}

async fn run_conn<S>(node: Arc<Node>, ws: WebSocketStream<S>, outbound: bool, addr: String)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let id = node.next_conn.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Out>(256);
    let own_tx = tx.clone();
    // every link speaks the binary protocol; there are no other kinds of client
    let sametra = Arc::new(AtomicBool::new(true));
    node.peers.lock().unwrap().insert(id, PeerHandle { tx, outbound, sametra: sametra.clone(), addr: addr.clone(), since: now() });
    let mut brx = node.tx.subscribe();
    let mut subs: HashMap<String, Vec<Filter>> = HashMap::new();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    let mut budget = (now(), 0u32); // crude per-second rate limit for published EVENTs
    let wire = Wire;

    if outbound {
        let since = node.store.lock().unwrap().iter().next().map(|e| e.created_at.saturating_sub(3600)).unwrap_or(0);
        let _ = sink.send(wire.req("sync", &node.sync_filters(since), 0)).await;
        node.heartbeat();
    }

    loop {
        tokio::select! {
            m = stream.next() => {
                let msg = match m {
                    // text is not part of the protocol
                    Some(Ok(Message::Text(_))) => {
                        let _ = sink.send(wire.notice("error: this relay speaks binary protobuf frames only")).await;
                        break;
                    }
                    Some(Ok(Message::Binary(b))) => parse_frame(&b),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                };
                match msg {
                    Msg::Publish(e) => {
                        let t = now();
                        if budget.0 != t { budget = (t, 0); }
                        budget.1 += 1;
                        let eid = e.id;
                        if budget.1 == 201 {
                            tracing::warn!(conn = id, %addr, "peer is publishing over 200 events/s; refusing the rest of this second");
                        }
                        let (ok, m) = if budget.1 > 200 {
                            (false, "rate-limited: slow down".to_string())
                        } else {
                            match node.ingest(e, id, false) {
                                Ok((_, true)) => (true, String::new()),
                                Ok((_, false)) => (true, "duplicate:".into()),
                                Err(m) => {
                                    tracing::debug!(conn = id, %addr, event = %eid.hex(), reason = %m, "event refused");
                                    (false, m)
                                }
                            }
                        };
                        // peers flooding to us don't need acks; nostr clients do
                        // peers flooding to us need no acks, refusals do
                        if !ok { let _ = sink.send(wire.ok(&eid, ok, &m)).await; }
                    }
                    Msg::SubEvent(sid, e) => {
                        let pend = node.pending.lock().unwrap().get(&sid).cloned();
                        if let Ok((e, _)) = node.ingest(e, id, pend.is_some())
                            && let Some(p) = pend { let _ = p.try_send(Some(e)); }
                    }
                    Msg::Eose(sid) => {
                        if let Some(p) = node.pending.lock().unwrap().get(&sid) { let _ = p.try_send(None); }
                    }
                    Msg::Req(sid, filters, hops) => {
                        // searches are one-shot (answered, maybe forwarded, then EOSE); others stay live
                        let search = filters.iter().any(|f| f.search.is_some());
                        if sid.len() > 64 || (!search && subs.len() >= 32 && !subs.contains_key(&sid)) {
                            let _ = sink.send(wire.closed(&sid, "error: too many subscriptions")).await;
                            continue;
                        }
                        let mut hits: Vec<Ev> = {
                            let st = node.store.lock().unwrap();
                            filters.iter().flat_map(|f| st.query(f, if search { 200 } else { 20000 })).collect()
                        };
                        if search {
                            let mut seen = HashSet::new();
                            hits.retain(|e| seen.insert(e.id)); // keep relevance order
                        } else {
                            hits.sort_by_key(|e| (e.created_at, e.id));
                            hits.dedup_by_key(|e| e.id);
                        }
                        {
                            // popularity: found by a search, or fetched by id
                            let by_id = filters.iter().any(|f| f.ids.is_some());
                            let mut st = node.store.lock().unwrap();
                            for e in hits.iter().take(200) {
                                if search { st.bump(&e.id, 0.2) } else if by_id { st.bump(&e.id, 1.0) }
                            }
                        }
                        let mut failed = false;
                        for e in &hits {
                            if sink.feed(wire.event(&sid, e)).await.is_err() { failed = true; break; }
                        }
                        if failed { break; }
                        if search {
                            let fresh = {
                                let mut seen = node.searches.lock().unwrap();
                                let t = now();
                                let fresh = seen.get(&sid).is_none_or(|at| at + 60 < t);
                                seen.insert(sid.clone(), t);
                                fresh
                            };
                            let slot = (hops > 0 && fresh).then(|| node.search_slots.clone().try_acquire_owned().ok()).flatten();
                            if let Some(slot) = slot {
                                let (n, back, sid) = (node.clone(), own_tx.clone(), sid.clone());
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    let found = n.ask_peers(filters, search_timeout(hops - 1), hops - 1, Some(id), Some(sid.clone())).await;
                                    let _ = back.send(Out::Events(sid, found)).await;
                                });
                            } else if sink.send(wire.eose(&sid)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        if sink.send(wire.eose(&sid)).await.is_err() { break; }
                        // introduce ourselves to new subscribers so they learn our address now
                        if subs.is_empty() && !outbound {
                            let hb = node.heartbeat_event();
                            if filters.iter().any(|f| f.matches(&hb)) { let _ = sink.send(wire.event(&sid, &hb)).await; }
                        }
                        subs.insert(sid, filters);
                    }
                    Msg::Close(sid) => { subs.remove(&sid); }
                    Msg::Ignore => {}
                    Msg::Bad => { let _ = sink.send(wire.notice("error: bad message")).await; }
                }
            }
            Some(cmd) = rx.recv() => {
                let m = match cmd {
                    Out::Req(sid, fs, hops) => Some(wire.req(&sid, &fs, hops)),
                    Out::Close(sid) => Some(wire.close(&sid)),
                    Out::Events(sid, evs) => {
                        let mut failed = false;
                        for e in &evs {
                            if sink.feed(wire.event(&sid, e)).await.is_err() { failed = true; break; }
                        }
                        if failed { break; }
                        Some(wire.eose(&sid))
                    }
                };
                if let Some(m) = m && sink.send(m).await.is_err() { break; }
            }
            r = brx.recv() => match r {
                Ok((e, origin)) if origin != id => {
                    let mut matched = false;
                    for (sid, fs) in &subs {
                        if fs.iter().any(|f| f.matches(&e)) {
                            matched = true;
                            if sink.feed(wire.event(sid, &e)).await.is_err() { break; }
                        }
                    }
                    // relays we dialed don't subscribe to us: push new events to them
                    if outbound && !matched {
                        let _ = sink.feed(wire.publish(&e)).await;
                    }
                    if sink.flush().await.is_err() { break; }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            _ = ping.tick() => { if sink.send(Message::Ping(Default::default())).await.is_err() { break; } }
        }
    }
    node.peers.lock().unwrap().remove(&id);
}

// ---- background tasks ------------------------------------------------------

pub fn spawn_background(node: Arc<Node>) {
    // dialer
    let n = node.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let connected: Vec<String> = n.peers.lock().unwrap().values().filter(|p| p.outbound).map(|p| p.addr.clone()).collect();
            if connected.len() >= n.cfg.max_peers {
                continue;
            }
            let t = now();
            let mut cands: Vec<String> = n
                .book
                .lock()
                .unwrap()
                .iter()
                .filter(|(a, i)| i.next_try <= t && !connected.contains(a) && Some(*a) != n.cfg.public_addr.as_ref())
                .map(|(a, _)| a.clone())
                .collect();
            cands.truncate(n.cfg.max_peers - connected.len());
            for addr in cands {
                let n = n.clone();
                tokio::spawn(async move { dial(n, addr).await });
            }
        }
    });

    // heartbeat
    let n = node.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(120));
        loop {
            tick.tick().await;
            n.heartbeat();
        }
    });

    // maintenance: flush, evict, compact, forget old ephemeral ids, save address book
    let n = node;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut i = 0u64;
        loop {
            tick.tick().await;
            i += 1;
            n.store.lock().unwrap().flush();
            if i.is_multiple_of(12) {
                let evicted = {
                    let rank = n.ranking();
                    let r = crate::store::Retention { max_events: n.cfg.max_events, max_age: n.cfg.max_age };
                    let mut st = n.store.lock().unwrap();
                    let k = st.evict(&n.keys.pubkey, &r, |s, e| rank(s, e));
                    if let Err(e) = st.compact_if_needed(false) {
                        tracing::warn!(error = %e, "compacting the event log failed");
                    }
                    k
                };
                if evicted > 0 {
                    *n.model_cache.lock().unwrap() = None;
                    tracing::info!(evicted, cached = n.store.lock().unwrap().len(), "evicted events");
                }
                let cut = now().saturating_sub(1200);
                n.seen.lock().unwrap().retain(|_, t| *t > cut);
                n.searches.lock().unwrap().retain(|_, t| *t + 120 > now());
                n.save_state();
            }
        }
    });
}

/// Connect to a relay and run the link until it drops (also used by tests).
pub async fn dial(node: Arc<Node>, addr: String) {
    let r = tokio::time::timeout(Duration::from_secs(10), node.dialer.connect(&addr)).await;
    let ws = match r {
        Ok(Ok(ws)) => ws,
        failed => {
            let mut book = node.book.lock().unwrap();
            let i = book.entry(addr.clone()).or_default();
            i.fails += 1;
            i.next_try = now() + (15u64 << i.fails.min(8)).min(3600);
            let error = failed.map(|r| r.err().unwrap_or_default()).unwrap_or_else(|_| "timed out".into());
            let retry_in = i.next_try.saturating_sub(now());
            if i.fails == 1 {
                tracing::warn!(%addr, %error, retry_in, "dial failed");
            } else {
                tracing::debug!(%addr, %error, fails = i.fails, retry_in, "dial failed again");
            }
            return;
        }
    };
    tracing::info!(%addr, "connected to peer");
    if let Some(i) = node.book.lock().unwrap().get_mut(&addr) {
        i.fails = 0;
    }
    run_conn(node.clone(), ws, true, addr.clone()).await;
    tracing::info!(%addr, "disconnected from peer");
    if let Some(i) = node.book.lock().unwrap().get_mut(&addr) {
        i.next_try = now() + 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!("sametra-node-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        d
    }
    fn node_with(name: &str, f: impl FnOnce(&mut Config)) -> Arc<Node> {
        let mut cfg = Config { data: tmpdir(name), ..Default::default() };
        f(&mut cfg);
        Node::new(cfg, Keys::generate()).unwrap()
    }
    fn c(slug: &str) -> Vec<Vec<String>> {
        vec![vec!["c".into(), slug.into()]]
    }

    // ---- ingest -------------------------------------------------------------

    #[test]
    fn ingest_validates_and_broadcasts() {
        let n = node_with("ingest", |_| {});
        let mut rx = n.tx.subscribe();
        let k = Keys::generate();
        let e = k.sign(K_POST, c("m"), "x".into());
        let (stored, new) = n.ingest(e.clone(), 7, false).unwrap();
        assert!(new && stored.id == e.id);
        let (b, origin) = rx.try_recv().unwrap();
        assert_eq!((b.id, origin), (e.id, 7));
        assert!(!n.ingest(e.clone(), 8, false).unwrap().1, "duplicate");
        assert!(rx.try_recv().is_err(), "duplicates are not re-broadcast");

        let mut forged = k.sign(K_POST, c("m"), "y".into());
        forged.content = b"z".to_vec();
        assert!(n.ingest(forged, 1, false).unwrap_err().starts_with("invalid"));
        let mut future = k.sign(K_POST, c("m"), "f".into());
        future.created_at = now() + 3600;
        assert!(n.ingest(future, 1, false).unwrap_err().contains("future"));
        assert!(n.ingest(k.sign(K_POST, c("m"), "s".into()), 1, false).is_ok());
        assert_eq!(n.store.lock().unwrap().len(), 2);
    }

    #[test]
    fn interest_scoping_without_relay_all() {
        let k = Keys::generate();
        let topic = k.sign(K_POST, vec![], "a topic".into());
        let n = node_with("interest", |c| {
            c.relay_all = false;
            c.follow = vec![topic.id];
        });
        assert!(n.ingest(topic.clone(), 1, false).is_ok(), "the followed post");
        let child = k.sign(K_POST, ancestry_tags(&topic.id, &[]), "under it".into());
        let grandchild = k.sign(K_POST, ancestry_tags(&child.id, &[topic.id]), "deeper".into());
        assert!(n.ingest(child, 1, false).is_ok());
        assert!(n.ingest(grandchild, 1, false).is_ok(), "everything under it, at any depth");
        assert!(n.ingest(k.sign(K_POST, vec![], "elsewhere".into()), 1, false).unwrap_err().contains("not following"));
        assert!(n.ingest(k.sign(K_POST, vec![], "fetched".into()), 1, true).is_ok(), "explicit fetches are kept");
        assert!(n.ingest(k.sign(K_PROFILE, vec![], "{}".into()), 1, false).is_ok(), "global metadata");
        assert!(n.publish(K_POST, vec![], "mine".into()).is_ok(), "own events");
        assert!(n.ingest(n.keys.sign(K_POST, vec![], "own".into()), 1, false).is_ok());
        let sf = n.sync_filters(5);
        assert!(sf.iter().any(|f| f.tags == vec![("e".to_string(), vec![topic.id.hex()])] && f.since == Some(5)), "everything under it names it");
        assert!(sf.iter().any(|f| f.ids == Some(vec![topic.id.hex()])), "and the post itself");
        assert!(sf.iter().any(|f| f.kinds.as_ref().is_some_and(|k| k.contains(&K_RATING))));
        assert!(sf.iter().any(|f| f.kinds == Some(vec![K_HEARTBEAT])));
    }

    #[test]
    fn sync_filters_relay_all() {
        let n = node_with("syncall", |_| {});
        let sf = n.sync_filters(42);
        assert_eq!(sf.len(), 2);
        assert!(sf[0].kinds.is_none() && sf[0].tags.is_empty() && sf[0].since == Some(42));
    }

    #[test]
    fn heartbeats_feed_presence_and_address_book() {
        let n = node_with("hb", |_| {});
        let k = Keys::generate();
        let hb = |addr: &str| k.sign(K_HEARTBEAT, vec![vec!["r".into(), addr.into()]], String::new());
        let e = hb("ws://10.0.0.1:42424");
        assert!(n.ingest(e.clone(), 1, false).unwrap().1);
        assert!(!n.ingest(e, 1, false).unwrap().1, "seen ephemeral ids are dropped");
        assert!(n.book.lock().unwrap().contains_key("ws://10.0.0.1:42424"));
        assert!(n.store.lock().unwrap().last_seen.get(&k.pubkey).is_some_and(|t| *t + 5 >= now()));
        assert_eq!(n.store.lock().unwrap().len(), 0, "heartbeats are not stored");
        n.ingest(hb("http://evil"), 1, false).unwrap();
        assert!(!n.book.lock().unwrap().contains_key("http://evil"), "only ws(s) addresses");
        let stale = k.sign_at(K_HEARTBEAT, vec![vec!["r".into(), "ws://old:1".into()]], String::new(), now() - 3600);
        assert!(n.ingest(stale, 1, false).unwrap_err().contains("stale"));
        assert!(!n.book.lock().unwrap().contains_key("ws://old:1"));
        // our own heartbeat never adds ourselves to the book
        let me = node_with("hb2", |c| c.public_addr = Some("ws://me:1".into()));
        me.heartbeat();
        assert!(!me.book.lock().unwrap().contains_key("ws://me:1"));
    }

    #[test]
    fn quick_edits_of_replaceable_events_always_win() {
        let n = node_with("edits", |_| {});
        for i in 0..5 {
            n.publish(K_PROFILE, vec![], format!("v{i}")).unwrap();
            let d = vec![vec!["d".into(), "x".into()]];
            n.publish(K_RATING, d, format!("r{i}")).unwrap();
        }
        let st = n.store.lock().unwrap();
        assert_eq!(st.replaceable(&n.keys.pubkey, K_PROFILE, "").unwrap().text(), "v4");
        assert_eq!(st.replaceable(&n.keys.pubkey, K_RATING, "x").unwrap().text(), "r4");
        assert_eq!(st.len(), 2);
    }

    #[test]
    fn popularity_persists() {
        let n = node_with("popular", |_| {});
        // popularity survives a restart through state.pb
        let e = n.publish(K_POST, c("bio"), "x".into()).unwrap();
        n.store.lock().unwrap().bump(&e.id, 5.0);
        n.save_state();
        n.store.lock().unwrap().flush();
        let data = n.cfg.data.clone();
        drop(n);
        let again = Node::new(Config { data, ..Default::default() }, Keys::generate()).unwrap();
        assert!(again.store.lock().unwrap().popularity(&e.id) > 4.9);
    }

    #[tokio::test]
    async fn remote_search_without_peers_returns_immediately() {
        let n = node_with("nosearch", |_| {});
        let t = std::time::Instant::now();
        let f = Filter { search: Some("x".into()), ..Default::default() };
        assert!(n.remote_search(vec![f], 2).await.is_empty());
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn model_is_rebuilt_on_new_ratings() {
        let n = node_with("repcache", |_| {});
        let (a, b) = (Keys::generate(), Keys::generate());
        let post = b.sign(K_POST, vec![], "claim".into());
        let elsewhere = b.sign(K_POST, vec![], "other topic".into());
        n.ingest(post.clone(), 1, false).unwrap();
        n.ingest(elsewhere.clone(), 1, false).unwrap();
        assert_eq!(n.model().near(&b.pubkey, &post.id), 0.0);
        let t = |k: &str, v: String| vec![k.to_string(), v];
        let r = a.sign(K_RATING, vec![t("d", "x".into()), t("p", b.pubkey.hex()), t("e", post.id.hex()), t("score", "1".into())], String::new());
        n.ingest(r, 1, false).unwrap();
        let m = n.model();
        assert!(m.near(&b.pubkey, &post.id) > 0.0);
        assert!(m.near(&b.pubkey, &elsewhere.id) < m.near(&b.pubkey, &post.id), "standing is strongest where it was earned");
        assert!(Arc::ptr_eq(&m, &n.model()), "cached");
    }

    #[test]
    fn state_and_memory_persist() {
        let dir = tmpdir("persist");
        let keys = Keys::generate();
        let secret = keys.secret_hex();
        let cfg = || Config { data: dir.clone(), peers: vec!["ws://boot:1".into()], ..Default::default() };
        let n = Node::new(cfg(), keys).unwrap();
        let e = n.publish(K_POST, c("m"), "x".into()).unwrap();
        n.following.lock().unwrap().insert(e.id);
        n.store.lock().unwrap().pin([e.id], true);
        n.book.lock().unwrap().insert("ws://learned:2".into(), AddrInfo::default());
        n.book.lock().unwrap().insert("ws://dead:3".into(), AddrInfo { fails: 9, next_try: 0 });
        n.memory.lock().unwrap().insert("k".into(), "v".into());
        n.save_state();
        n.save_memory();
        n.store.lock().unwrap().flush();
        drop(n);
        let n = Node::new(Config { peers: vec![], ..cfg() }, Keys::parse(&secret).unwrap()).unwrap();
        assert!(n.following.lock().unwrap().contains(&e.id));
        assert!(n.store.lock().unwrap().pinned.contains(&e.id));
        assert!(n.store.lock().unwrap().get(&e.id).is_some());
        let book = n.book.lock().unwrap();
        assert!(book.contains_key("ws://boot:1") && book.contains_key("ws://learned:2"));
        assert!(!book.contains_key("ws://dead:3"), "repeatedly failing peers are forgotten");
        drop(book);
        assert_eq!(n.memory.lock().unwrap().get("k").map(String::as_str), Some("v"));
        assert!(!dir.join("state.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_state_files_are_ignored() {
        let dir = tmpdir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.pb"), [0xff, 0xff, 0xff]).unwrap();
        std::fs::write(dir.join("memory.pb"), [0x0a, 0x50]).unwrap();
        let n = Node::new(Config { data: dir.clone(), follow: vec![H32([7; 32])], ..Default::default() }, Keys::generate()).unwrap();
        assert!(n.following.lock().unwrap().contains(&H32([7; 32])));
        assert!(n.memory.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn remote_fetch_without_peers_returns_immediately() {
        let n = node_with("nofetch", |_| {});
        let t = std::time::Instant::now();
        assert!(n.remote_fetch(vec![Filter::default()], Duration::from_secs(5)).await.is_empty());
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(n.pending.lock().unwrap().is_empty());
    }
}
