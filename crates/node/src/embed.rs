//! How close two posts are in meaning, from text embeddings.
//!
//! Contextual standing (see [`crate::rep`]) needs to know how close two posts
//! are. Without embeddings the node only has the post tree: same tree close,
//! everything else far. With an embeddings service (`--embed-api`, anything
//! that speaks the OpenAI-style `POST /v1/embeddings`: Ollama, llama.cpp's
//! server, Hugging Face TEI, vLLM, a hosted API) the node embeds each post it
//! keeps, so expertise carries across trees that discuss the same thing, and
//! new or niche fields need no names: they are just regions of the space.
//!
//! - Each node computes its own vectors from the text it holds; vectors never
//!   travel on the network, so nobody can claim their post sits somewhere it
//!   does not.
//! - Vectors are normalized and quantized to one signed byte per dimension
//!   (768 bytes for a 768-dimension model), kept in memory and saved to
//!   `DATA/vectors.pb` with the model name. A different model starts over.
//! - Closeness is cosine similarity rescaled against the baseline (the mean
//!   similarity of random pairs this node holds), so it works with any model
//!   without tuning: unrelated posts land near 0, near-duplicates near 1.
//! - Posts are embedded in small batches in the background. If the service is
//!   down the node keeps working on tree closeness and retries.

use crate::event::H32;
use std::collections::HashMap;
use std::sync::RwLock;

/// Text sent to the model per post, at most (bytes).
pub const MAX_TEXT: usize = 2000;
/// Posts per request to the service.
pub const BATCH: usize = 32;
/// Closeness of posts in the same tree, at least (they work on one problem).
pub const SAME_TREE: f64 = 0.5;
/// Closeness of unrelated posts: standing elsewhere counts for almost nothing.
pub const FLOOR: f64 = 0.02;
/// Baseline similarity until there are enough vectors to measure it. (With
/// nomic-embed-text, unrelated research posts sit around 0.35 to 0.45 and
/// posts on one subject around 0.65; other models shift both.)
const DEFAULT_BASE: f64 = 0.35;
/// Vectors needed before the baseline is measured instead of assumed.
const CALIBRATE_FROM: usize = 8;

/// One post's vector: unit length, scaled by 127 and rounded.
pub struct Vector {
    q: Box<[i8]>,
    norm: f32,
}

impl Vector {
    pub fn quantize(v: &[f32]) -> Option<Vector> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !(n.is_finite() && n > 0.0) {
            return None;
        }
        let q: Box<[i8]> = v.iter().map(|x| (x / n * 127.0).round().clamp(-127.0, 127.0) as i8).collect();
        Some(Vector::from_q(q))
    }
    fn from_q(q: Box<[i8]>) -> Vector {
        let norm = (q.iter().map(|&x| (x as i32 * x as i32) as f32).sum::<f32>()).sqrt();
        Vector { q, norm }
    }
    pub fn cosine(&self, o: &Vector) -> f64 {
        if self.q.len() != o.q.len() || self.norm == 0.0 || o.norm == 0.0 {
            return 0.0;
        }
        let dot: i32 = self.q.iter().zip(o.q.iter()).map(|(&a, &b)| a as i32 * b as i32).sum();
        dot as f64 / (self.norm as f64 * o.norm as f64)
    }
}

/// The vectors this node holds, for one model.
#[derive(Default)]
pub struct Vectors {
    pub model: String,
    map: HashMap<H32, Vector>,
    /// mean similarity of random pairs: what "unrelated" looks like for this model
    base: f64,
    pub dirty: bool,
}

impl Vectors {
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn has(&self, id: &H32) -> bool {
        self.map.contains_key(id)
    }
    pub fn get(&self, id: &H32) -> Option<&Vector> {
        self.map.get(id)
    }
    pub fn insert(&mut self, id: H32, v: Vector) {
        if let Some(d) = self.map.values().next().map(|x| x.q.len())
            && d != v.q.len()
        {
            // the service changed models under the same name
            tracing::warn!(old = d, new = v.q.len(), "embedding dimensions changed; starting over");
            self.map.clear();
        }
        self.map.insert(id, v);
        self.dirty = true;
    }
    /// Start over if the vectors were made by another model.
    pub fn use_model(&mut self, model: &str) {
        if self.model != model {
            if !self.map.is_empty() {
                tracing::info!(old = %self.model, new = %model, dropped = self.map.len(), "embedding model changed; re-embedding");
            }
            self.map.clear();
            self.model = model.to_string();
            self.dirty = true;
        }
    }
    /// Forget vectors of posts the node no longer holds.
    pub fn retain(&mut self, keep: impl Fn(&H32) -> bool) {
        let n = self.map.len();
        self.map.retain(|id, _| keep(id));
        self.dirty |= self.map.len() != n;
    }
    /// The baseline similarity, from up to 256 pairs spread over what is held.
    pub fn calibrate(&mut self) {
        let vs: Vec<&Vector> = self.map.values().collect();
        if vs.len() < CALIBRATE_FROM {
            self.base = DEFAULT_BASE;
            return;
        }
        let (mut sum, mut n) = (0.0, 0);
        let step = (vs.len() / 17).max(1);
        for i in 0..16usize {
            for j in (i + 1)..17usize {
                let (a, b) = ((i * step * 7) % vs.len(), (j * step * 13 + 1) % vs.len());
                if a != b {
                    sum += vs[a].cosine(vs[b]);
                    n += 1;
                }
            }
        }
        self.base = if n > 0 { (sum / n as f64).clamp(-0.5, 0.9) } else { DEFAULT_BASE };
    }
    /// How close two posts are in meaning, 0..1; None if either has no vector.
    pub fn closeness(&self, a: &H32, b: &H32) -> Option<f64> {
        let (x, y) = (self.map.get(a)?, self.map.get(b)?);
        let base = if self.map.len() < CALIBRATE_FROM { DEFAULT_BASE } else { self.base };
        Some(((x.cosine(y) - base) / (1.0 - base)).clamp(0.0, 1.0))
    }

    pub fn load(path: &std::path::Path) -> Vectors {
        let Ok(b) = std::fs::read(path) else { return Vectors { base: DEFAULT_BASE, ..Default::default() } };
        let Ok(pb) = crate::pb::decode::<crate::pb::Vectors>(&b) else {
            tracing::warn!(path = %path.display(), "unreadable vectors file; re-embedding");
            return Vectors { base: DEFAULT_BASE, ..Default::default() };
        };
        let map = pb
            .items
            .into_iter()
            .filter_map(|v| {
                let id = H32(v.id.as_slice().try_into().ok()?);
                Some((id, Vector::from_q(v.q.into_iter().map(|b| b as i8).collect())))
            })
            .collect();
        let mut v = Vectors { model: pb.model, map, base: DEFAULT_BASE, dirty: false };
        v.calibrate();
        v
    }
    pub fn to_pb(&self) -> crate::pb::Vectors {
        crate::pb::Vectors {
            model: self.model.clone(),
            items: self.map.iter().map(|(id, v)| crate::pb::Vector { id: id.0.to_vec(), q: v.q.iter().map(|&x| x as u8).collect() }).collect(),
        }
    }
}

/// Closeness for contextual standing: meaning where both posts have vectors,
/// the post tree otherwise.
pub struct Closeness {
    pub vectors: std::sync::Arc<RwLock<Vectors>>,
    pub root_of: HashMap<H32, H32>,
}

impl crate::rep::Closeness for Closeness {
    fn closeness(&self, a: &H32, b: &H32) -> f64 {
        if a == b {
            return 1.0;
        }
        let r = |x: &H32| self.root_of.get(x).copied().unwrap_or(*x);
        let same_tree = r(a) == r(b);
        match self.vectors.read().unwrap().closeness(a, b) {
            Some(c) => c.max(if same_tree { SAME_TREE } else { FLOOR }),
            None if same_tree => 1.0,
            None => crate::node::FAR,
        }
    }
}

// ---- semantic search ----------------------------------------------------------

/// LSH bands and bits per band: random-hyperplane SimHash (Charikar), each band
/// one DHT key; similar vectors share a band's key with high probability. With
/// 8 bands of 6 bits, two posts at cosine 0.65 (related, for nomic-embed-text)
/// share a key about 3 times in 4; unrelated ones (0.4) less than half the
/// time, and a peer that shares more keys ranks higher.
pub const LSH_BANDS: usize = 8;
pub const LSH_BITS: usize = 6;
/// Rescaled closeness a semantic hit needs (see Vectors::closeness).
pub const SEMANTIC_MIN: f64 = 0.25;

/// The ±1 hyperplanes for a model and dimension: the same on every node, from
/// a hash of the model name, so nodes using one model derive the same keys.
fn planes(model: &str, dims: usize) -> std::sync::Arc<Vec<Vec<i8>>> {
    use sha2::Digest;
    type Planes = std::sync::Arc<Vec<Vec<i8>>>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<(String, usize), Planes>>> = std::sync::OnceLock::new();
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap();
    c.entry((model.to_string(), dims))
        .or_insert_with(|| {
            let planes = (0..LSH_BANDS * LSH_BITS)
                .map(|p| {
                    let mut out = Vec::with_capacity(dims);
                    let mut block = 0u32;
                    while out.len() < dims {
                        let h = sha2::Sha256::digest(format!("sametra/lsh/1\0{model}\0{p}\0{block}").as_bytes());
                        for byte in h {
                            for bit in 0..8 {
                                if out.len() < dims {
                                    out.push(if byte >> bit & 1 == 1 { 1 } else { -1 });
                                }
                            }
                        }
                        block += 1;
                    }
                    out
                })
                .collect();
            std::sync::Arc::new(planes)
        })
        .clone()
}

/// The DHT keys of a vector, one per band, for `model`.
pub fn lsh_keys(model: &str, v: &Vector) -> Vec<H32> {
    use sha2::Digest;
    let planes = planes(model, v.q.len());
    (0..LSH_BANDS)
        .map(|band| {
            let mut code = 0u32;
            for bit in 0..LSH_BITS {
                let p = &planes[band * LSH_BITS + bit];
                let dot: i32 = p.iter().zip(v.q.iter()).map(|(&a, &b)| a as i32 * b as i32).sum();
                code = code << 1 | (dot >= 0) as u32;
            }
            H32(sha2::Sha256::digest(format!("sametra/lsh/1\0{model}\0{band}\0{code}").as_bytes()).into())
        })
        .collect()
}

impl Vectors {
    /// The posts closest in meaning to `q`, best first: (id, rescaled closeness
    /// at least `min`), at most `k`. A scan of the byte vectors; fine for the
    /// hundred thousand or so posts a node keeps.
    pub fn nearest(&self, q: &Vector, k: usize, min: f64) -> Vec<(H32, f64)> {
        let base = if self.map.len() < CALIBRATE_FROM { DEFAULT_BASE } else { self.base };
        let mut hits: Vec<(H32, f64)> = self.map.iter().map(|(id, v)| (*id, ((q.cosine(v) - base) / (1.0 - base)).clamp(0.0, 1.0))).filter(|x| x.1 >= min).collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        hits.truncate(k);
        hits
    }
}

/// What a post says, for the model: its title and, if it is text, its body.
pub fn text_of(e: &crate::event::Event) -> String {
    let mut s = e.tag("title").unwrap_or("").to_string();
    if crate::event::is_text(&e.content_type) {
        if !s.is_empty() {
            s.push_str("\n\n");
        }
        s.push_str(e.text());
    }
    if s.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

#[cfg(feature = "embed")]
pub use client::*;

#[cfg(not(feature = "embed"))]
impl crate::node::Node {
    pub async fn semantic_hits(&self, _: &str, _: usize) -> Vec<(H32, f64)> {
        vec![]
    }
    pub async fn semantic_keys(&self, _: &str) -> Vec<H32> {
        vec![]
    }
}

#[cfg(feature = "embed")]
mod client {
    use super::*;
    use crate::event::K_POST;
    use crate::node::Node;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::client::legacy::{Client, connect::HttpConnector};
    use hyper_util::rt::TokioExecutor;
    use std::collections::{BTreeSet, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Clone)]
    pub struct Config {
        /// Base URL of an OpenAI-compatible API, e.g. `http://127.0.0.1:11434/v1`.
        pub api: String,
        pub model: String,
        /// `Authorization` header value, e.g. `Bearer sk-…`.
        pub auth: Option<String>,
    }

    /// Embeds the posts the node keeps, in the background.
    pub struct Embedder {
        http: Client<HttpConnector, Full<Bytes>>,
        pub cfg: Config,
        queue: Mutex<(VecDeque<H32>, BTreeSet<H32>)>,
        wake: tokio::sync::Notify,
        /// the last error from the service, while it fails
        pub error: Mutex<Option<String>>,
    }

    impl Node {
        /// The query as a vector, from this node's embeddings service.
        pub async fn embed_query(&self, q: &str) -> Option<Vector> {
            let emb = self.embedder.get()?.clone();
            let v = emb.embed(&[q.to_string()]).await.ok()?.into_iter().next()?;
            Vector::quantize(&v)
        }
        /// Posts close in meaning to `q`: (id, closeness), best first.
        pub async fn semantic_hits(&self, q: &str, k: usize) -> Vec<(H32, f64)> {
            let Some(v) = self.embed_query(q).await else { return vec![] };
            self.vectors.read().unwrap().nearest(&v, k, SEMANTIC_MIN)
        }
        /// The DHT keys a semantic search looks up: the query's LSH buckets.
        pub async fn semantic_keys(&self, q: &str) -> Vec<H32> {
            let Some(v) = self.embed_query(q).await else { return vec![] };
            let model = self.vectors.read().unwrap().model.clone();
            lsh_keys(&model, &v)
        }
    }

    impl Embedder {
        pub fn new(cfg: Config) -> Result<Embedder, String> {
            if !cfg.api.starts_with("http://") {
                return Err(format!("--embed-api must be http://host:port/v1 (got '{}'); put a TLS proxy in front of a remote service", cfg.api));
            }
            if cfg.model.is_empty() {
                return Err("--embed-model is required with --embed-api".into());
            }
            let http = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build_http();
            Ok(Embedder { http, cfg, queue: Mutex::default(), wake: tokio::sync::Notify::new(), error: Mutex::default() })
        }

        /// Embed this post when the worker gets to it.
        pub fn enqueue(&self, id: H32) {
            let mut q = self.queue.lock().unwrap();
            if q.1.insert(id) {
                q.0.push_back(id);
                drop(q);
                self.wake.notify_one();
            }
        }
        pub fn pending(&self) -> usize {
            self.queue.lock().unwrap().0.len()
        }
        fn take(&self, n: usize) -> Vec<H32> {
            let mut q = self.queue.lock().unwrap();
            let k = n.min(q.0.len());
            let ids: Vec<H32> = q.0.drain(..k).collect();
            for id in &ids {
                q.1.remove(id);
            }
            ids
        }

        /// `POST {api}/embeddings`: one vector per text, in order.
        pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            let url = format!("{}/embeddings", self.cfg.api.trim_end_matches('/'));
            let body = serde_json::json!({ "model": self.cfg.model, "input": texts });
            let mut req = hyper::Request::post(&url).header("Content-Type", "application/json");
            if let Some(a) = &self.cfg.auth {
                req = req.header("Authorization", a);
            }
            let req = req.body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap()))).map_err(|e| e.to_string())?;
            let t = Duration::from_secs(60);
            let resp = tokio::time::timeout(t, self.http.request(req))
                .await
                .map_err(|_| format!("embeddings service at {} did not answer in time", self.cfg.api))?
                .map_err(|e| format!("no embeddings service at {} ({e})", self.cfg.api))?;
            let status = resp.status();
            let bytes = tokio::time::timeout(t, resp.into_body().collect()).await.map_err(|_| "embeddings: reply timed out".to_string())?.map_err(|e| e.to_string())?.to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| format!("embeddings: HTTP {status}, not JSON: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)])))?;
            if !status.is_success() {
                let msg = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("");
                return Err(format!("embeddings: HTTP {status} {msg}"));
            }
            let mut out: Vec<(usize, Vec<f32>)> = v["data"]
                .as_array()
                .ok_or("embeddings: reply has no `data`")?
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let at = d["index"].as_u64().map(|x| x as usize).unwrap_or(i);
                    let e = d["embedding"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default();
                    (at, e)
                })
                .collect();
            out.sort_by_key(|x| x.0);
            if out.len() != texts.len() || out.iter().any(|x| x.1.is_empty()) {
                return Err(format!("embeddings: asked for {}, got {} usable vectors", texts.len(), out.iter().filter(|x| !x.1.is_empty()).count()));
            }
            Ok(out.into_iter().map(|x| x.1).collect())
        }
    }

    /// Start embedding: queue every post that has no vector yet, then keep up
    /// with new ones (the node queues them as they arrive).
    pub fn spawn(node: Arc<Node>, emb: Arc<Embedder>) {
        node.vectors.write().unwrap().use_model(&emb.cfg.model);
        {
            let st = node.store.lock().unwrap();
            let vs = node.vectors.read().unwrap();
            let posts = st.query(&crate::event::Filter { kinds: Some(vec![K_POST]), ..Default::default() }, usize::MAX);
            for e in posts.iter().rev() {
                if !vs.has(&e.id) {
                    emb.enqueue(e.id);
                }
            }
        }
        tokio::spawn(async move {
            let mut backoff = 0u64;
            loop {
                if emb.pending() == 0 {
                    let _ = tokio::time::timeout(Duration::from_secs(30), emb.wake.notified()).await;
                    continue;
                }
                let ids = emb.take(BATCH);
                let posts: Vec<(H32, String)> = {
                    let st = node.store.lock().unwrap();
                    ids.iter().filter_map(|id| st.get(id).map(|e| (*id, text_of(e)))).filter(|x| !x.1.trim().is_empty()).collect()
                };
                if posts.is_empty() {
                    continue;
                }
                let texts: Vec<String> = posts.iter().map(|x| x.1.clone()).collect();
                match emb.embed(&texts).await {
                    Ok(vecs) => {
                        let mut vs = node.vectors.write().unwrap();
                        for ((id, _), v) in posts.iter().zip(vecs) {
                            if let Some(q) = Vector::quantize(&v) {
                                vs.insert(*id, q);
                            }
                        }
                        let n = vs.len();
                        drop(vs);
                        if emb.error.lock().unwrap().take().is_some() {
                            tracing::info!(api = %emb.cfg.api, "embeddings service answering again");
                        }
                        tracing::debug!(embedded = posts.len(), held = n, "embedded posts");
                        backoff = 0;
                    }
                    Err(e) => {
                        for (id, _) in &posts {
                            emb.enqueue(*id);
                        }
                        let first = emb.error.lock().unwrap().replace(e.clone()).is_none();
                        if first {
                            tracing::warn!(error = %e, "embedding failed; standing uses the post tree until the service answers");
                        }
                        backoff = (backoff * 2).clamp(5, 300);
                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> H32 {
        H32([b; 32])
    }

    #[test]
    fn lsh_keys_agree_for_close_vectors() {
        let a: Vec<f32> = (0..64).map(|i| ((i * 7) % 13) as f32 - 6.0).collect();
        let near: Vec<f32> = a.iter().enumerate().map(|(i, x)| x + if i % 9 == 0 { 0.5 } else { 0.0 }).collect();
        let far: Vec<f32> = (0..64).map(|i| ((i * 5) % 11) as f32 - 5.0).collect();
        let (qa, qn, qf) = (Vector::quantize(&a).unwrap(), Vector::quantize(&near).unwrap(), Vector::quantize(&far).unwrap());
        let shared = |x: &Vector, y: &Vector| lsh_keys("m", x).iter().zip(lsh_keys("m", y)).filter(|(p, q)| **p == *q).count();
        assert_eq!(lsh_keys("m", &qa), lsh_keys("m", &qa), "deterministic");
        assert_eq!(lsh_keys("m", &qa).len(), LSH_BANDS);
        assert!(shared(&qa, &qn) > shared(&qa, &qf), "close vectors share more buckets");
        assert_ne!(lsh_keys("m", &qa), lsh_keys("other model", &qa), "keys are per model");
        let mut vs = Vectors::default();
        vs.use_model("m");
        vs.insert(H32([1; 32]), qn);
        vs.insert(H32([2; 32]), qf);
        assert_eq!(vs.nearest(&qa, 5, 0.5).first().map(|x| x.0), Some(H32([1; 32])));
    }

    #[test]
    fn quantized_cosine_is_close_to_exact() {
        let a = [0.3f32, -1.2, 0.5, 2.0, 0.0, 0.7];
        let b = [0.25f32, -1.0, 0.9, 1.7, 0.1, 0.4];
        let exact = {
            let d: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            let n = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
            (d / (n(&a) * n(&b))) as f64
        };
        let (qa, qb) = (Vector::quantize(&a).unwrap(), Vector::quantize(&b).unwrap());
        assert!((qa.cosine(&qb) - exact).abs() < 0.01);
        assert!((qa.cosine(&qa) - 1.0).abs() < 1e-6);
        assert!(Vector::quantize(&[0.0, 0.0]).is_none());
        assert!(Vector::quantize(&[f32::NAN, 1.0]).is_none());
    }

    #[test]
    fn closeness_is_rescaled_from_the_baseline() {
        let mut vs = Vectors { base: DEFAULT_BASE, ..Default::default() };
        vs.use_model("m");
        vs.insert(h(1), Vector::quantize(&[1.0, 0.0, 0.0]).unwrap());
        vs.insert(h(2), Vector::quantize(&[0.95, 0.31, 0.0]).unwrap()); // cos ≈ 0.95
        vs.insert(h(3), Vector::quantize(&[0.0, 0.0, 1.0]).unwrap()); // cos 0
        let near = vs.closeness(&h(1), &h(2)).unwrap();
        assert!(near > 0.9 && near < 1.0, "{near}");
        assert_eq!(vs.closeness(&h(1), &h(3)), Some(0.0));
        assert_eq!(vs.closeness(&h(1), &h(9)), None);
        // with the tree as fallback and floor
        let c = Closeness { vectors: std::sync::Arc::new(RwLock::new(vs)), root_of: [(h(3), h(1))].into() };
        use crate::rep::Closeness as _;
        assert_eq!(c.closeness(&h(1), &h(3)), SAME_TREE, "same tree, different subject");
        assert_eq!(c.closeness(&h(2), &h(3)), FLOOR);
        assert_eq!(c.closeness(&h(1), &h(9)), crate::node::FAR, "no vector: the tree decides");
        assert_eq!(c.closeness(&h(9), &h(9)), 1.0);
    }

    #[test]
    fn model_change_and_dimension_change_start_over() {
        let mut vs = Vectors::default();
        vs.use_model("a");
        vs.insert(h(1), Vector::quantize(&[1.0, 2.0]).unwrap());
        vs.use_model("a");
        assert_eq!(vs.len(), 1);
        vs.insert(h(2), Vector::quantize(&[1.0, 2.0, 3.0]).unwrap());
        assert_eq!(vs.len(), 1, "dimensions changed");
        vs.use_model("b");
        assert!(vs.is_empty());
    }

    #[test]
    fn vectors_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("sametra-vec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("vectors.pb");
        let mut vs = Vectors::default();
        vs.use_model("nomic-embed-text");
        vs.insert(h(1), Vector::quantize(&[0.5, -0.5, 0.1]).unwrap());
        std::fs::write(&p, crate::pb::encode(&vs.to_pb())).unwrap();
        let back = Vectors::load(&p);
        assert_eq!(back.model, "nomic-embed-text");
        assert_eq!(back.get(&h(1)).unwrap().q, vs.get(&h(1)).unwrap().q);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn text_is_title_and_body_cut_at_a_char_boundary() {
        let k = crate::event::Keys::generate();
        let e = k.sign(crate::event::K_POST, vec![vec!["title".into(), "Q".into()]], "é".repeat(MAX_TEXT));
        let t = text_of(&e);
        assert!(t.starts_with("Q\n\n") && t.len() <= MAX_TEXT);
    }
}
