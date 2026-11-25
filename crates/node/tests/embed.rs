//! Standing through embeddings: a fake OpenAI-compatible service, a node that
//! embeds what it holds, and expertise that carries across trees by meaning.
#![cfg(feature = "embed")]
mod common;
use common::*;
use sametra_node::embed::{Config, Embedder};
use sametra_node::event::*;
use sametra_node::pb;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DIMS: usize = 64;

/// Bag of words hashed into DIMS dimensions: texts sharing words are close.
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIMS];
    for w in text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2) {
        // a few synonyms, so meaning can differ from wording
        let w = w.to_lowercase();
        let w = match w.as_str() {
            "car" => "automobile",
            "motor" => "engine",
            "breakdown" => "failure",
            other => other,
        };
        let h = w.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
        v[h as usize % DIMS] += 1.0;
    }
    v
}

/// `POST /v1/embeddings` over plain HTTP/1.1, one request per connection.
/// Answers 503 while `down` is set. Returns the base URL and a request counter.
async fn fake_service(down: Arc<std::sync::atomic::AtomicBool>) -> (String, Arc<AtomicUsize>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let (h, down) = (h.clone(), down.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let body = loop {
                    let n = s.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        assert!(head.starts_with("post /v1/embeddings "), "{head}");
                        let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break buf[i + 4..i + 4 + len].to_vec();
                        }
                    }
                };
                h.fetch_add(1, Ordering::SeqCst);
                let (status, out) = if down.load(Ordering::SeqCst) {
                    ("503 Service Unavailable", serde_json::json!({"error": {"message": "loading model"}}))
                } else {
                    let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(req["model"], "bag-of-words");
                    let data: Vec<_> = req["input"].as_array().unwrap().iter().enumerate().map(|(i, t)| serde_json::json!({"index": i, "embedding": embed(t.as_str().unwrap())})).collect();
                    ("200 OK", serde_json::json!({"object": "list", "data": data, "model": "bag-of-words"}))
                };
                let out = serde_json::to_vec(&out).unwrap();
                let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&out).await;
            });
        }
    });
    (url, hits)
}

fn start(n: &TestNode, url: &str) -> Arc<Embedder> {
    let emb = Arc::new(Embedder::new(Config { api: url.into(), model: "bag-of-words".into(), auth: None }).unwrap());
    let _ = n.node.embedder.set(emb.clone());
    sametra_node::embed::spawn(n.node.clone(), emb.clone());
    emb
}

const E: pb::Empty = pb::Empty {};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standing_carries_across_trees_by_meaning() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (url, _) = fake_service(down).await;
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    // alice is good on protein folding, in one tree
    let folding = alice.sign(K_POST, tags(&[&["title", "Protein folding kinetics"]]), "Folding rates of small proteins follow contact order.".into());
    // two other trees she never wrote in: one on proteins, one on primes
    let bob = Keys::generate();
    let proteins = bob.sign(K_POST, tags(&[&["title", "Why do proteins misfold"]]), "Folding intermediates and contact order in misfolding proteins.".into());
    let primes = bob.sign(K_POST, tags(&[&["title", "Twin primes below a bound"]]), "Sieve estimates for twin prime counts.".into());
    // posts already held are embedded when the service is configured
    for e in [&folding, &proteins] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    start(&n, &url);
    // and new ones as they arrive
    n.node.ingest(primes.clone(), 9, false).unwrap();
    assert!(eventually(Duration::from_secs(5), || n.node.vectors.read().unwrap().len() == 3).await, "all three embedded");
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!((i.embed_model.as_str(), i.vectors, i.embed_pending, i.embed_error.as_str()), ("bag-of-words", 3, 0, ""));
    assert_eq!(i.embed_api, url);

    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.9, event: folding.id.hex(), ..Default::default() }).await;
    let near = |x: &H32| pb::ReputationRequest { near: x.hex(), pubkey: alice.npub() };
    let at = |r: pb::ReputationResponse| r.entries[0].rep;
    let home = at(n.ok("reputation", &near(&folding.id)).await);
    let close = at(n.ok("reputation", &near(&proteins.id)).await);
    let far = at(n.ok("reputation", &near(&primes.id)).await);
    // with the post tree alone, both other trees would get the same 0.1 share
    assert!(home > close && close > far && far >= 0.0, "home {home}, close subject {close}, unrelated {far}");
    assert!(close > 2.0 * far, "a close subject counts for much more than an unrelated one: {close} vs {far}");

    // vectors are saved, and survive a restart without asking the service again
    n.node.save_vectors();
    let v = sametra_node::embed::Vectors::load(&n.node.cfg.data.join("vectors.pb"));
    assert_eq!((v.model.as_str(), v.len()), ("bag-of-words", 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_that_is_down_is_retried_and_the_tree_stands_in() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (url, hits) = fake_service(down.clone()).await;
    let n = spawn(|_| {}).await;
    let alice = Keys::generate();
    let a = alice.sign(K_POST, tags(&[&["title", "Protein folding"]]), "contact order".into());
    let b = under(&alice, &a, "folding rates");
    for e in [&a, &b] {
        n.node.ingest(e.clone(), 9, false).unwrap();
    }
    start(&n, &url);
    assert!(eventually(Duration::from_secs(5), || hits.load(Ordering::SeqCst) >= 1).await);
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert!(i.embed_error.contains("503") && i.embed_error.contains("loading model"), "{}", i.embed_error);
    assert_eq!((i.vectors, i.embed_pending), (0, 2), "kept for the retry");
    // standing still works, from the tree
    n.ok::<_, pb::Ack>("rate", &pb::RateRequest { score: 0.9, event: a.id.hex(), ..Default::default() }).await;
    let r: pb::ReputationResponse = n.ok("reputation", &pb::ReputationRequest { near: b.id.hex(), pubkey: alice.npub() }).await;
    assert!(r.entries[0].rep > 0.0);
    // the service comes back: the retry (after 5 s) embeds them
    down.store(false, Ordering::SeqCst);
    assert!(eventually(Duration::from_secs(12), || n.node.vectors.read().unwrap().len() == 2).await, "embedded after the retry");
    let i: pb::InfoResponse = n.ok("info", &E).await;
    assert_eq!(i.embed_error, "");
}

#[test]
fn config_is_checked() {
    let e = |api: &str, model: &str| Embedder::new(Config { api: api.into(), model: model.into(), auth: None }).err().unwrap_or_default();
    assert!(e("https://api.example/v1", "m").contains("http://"));
    assert!(e("http://127.0.0.1:11434/v1", "").contains("--embed-model"));
    assert_eq!(e("http://127.0.0.1:11434/v1", "nomic-embed-text"), "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_by_meaning_here_and_across_the_network() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (url, _) = fake_service(down).await;
    let public = |c: &mut sametra_node::node::Config| c.public_addr = Some(c.listen.clone());
    let scoped = |c: &mut sametra_node::node::Config| {
        c.public_addr = Some(c.listen.clone());
        c.relay_all = false;
    };
    // a node with embeddings holds a topic whose words differ from the query's
    let holder = spawn(public).await;
    start(&holder, &url);
    let t: pb::Ack = holder.ok("post", &post("Automobile engine failure", "automobile engine failure")).await;
    holder.ok::<_, pb::Ack>("post", &post("Twin primes", "sieve bounds")).await;
    assert!(eventually(Duration::from_secs(5), || holder.node.vectors.read().unwrap().len() == 2).await);
    let s = |q: &str, scope: &str| pb::SearchRequest { query: q.into(), scope: scope.into(), ..Default::default() };
    // here: no word in common, found by meaning
    let r: pb::Items = holder.ok("search", &s("car motor breakdown", "node")).await;
    assert_eq!(r.items.len(), 1, "{:?}", r.items.iter().map(|i| &i.title).collect::<Vec<_>>());
    assert_eq!(r.items[0].id, t.id);
    assert!(r.items[0].meaning > 0.5 && r.items[0].text_match == 0.0);
    // across the network: a newcomer with the same model, knowing only a bootstrap
    // node, finds the holder through the LSH buckets it announced
    let boot = spawn(scoped).await;
    holder.connect(&boot).await;
    holder.node.announce_anchors();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let newcomer = spawn(scoped).await;
    start(&newcomer, &url);
    newcomer.connect(&boot).await;
    let f = sametra_node::event::Filter { search: Some("car motor breakdown".into()), ..Default::default() };
    let targets = newcomer.node.search_targets(&[f]).await;
    assert!(targets.contains(&holder.node.p2p.get().unwrap().peer_id), "the LSH buckets lead to the holder (no word in common)");
    let r: pb::Items = newcomer.ok("search", &s("car motor breakdown", "network")).await;
    assert_eq!(r.items.first().map(|i| i.id.as_str()), Some(t.id.as_str()));
}
