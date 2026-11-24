//! The single-port HTTP front end: SPA serving, API transport rules, security headers, NIP-11.
mod common;
use common::*;
use sametra_node::pb;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PB: &str = "Content-Type: application/x-protobuf\r\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spa_routes_and_assets() {
    let n = spawn(|_| {}).await;
    let root = n.get("/").await;
    assert_eq!(root.status, 200);
    assert_eq!(root.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(root.header("cache-control"), Some("no-cache"));
    assert_eq!(root.header("x-content-type-options"), Some("nosniff"));
    let csp = root.header("content-security-policy").expect("CSP on pages");
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'") && csp.contains("object-src 'none'"));
    let html = root.text();
    assert!(html.to_lowercase().contains("<!doctype html>"));

    // client-side routes deep-link to the same document
    for p in ["/c/math", "/c/math/reputation", "/t/abc", "/u/npub1x", "/me", "/search?q=x%20y", "/index.html"] {
        let r = n.get(p).await;
        assert_eq!((r.status, r.text()), (200, html.clone()), "{p}");
    }
    // missing files are real 404s, not the app
    assert_eq!(n.get("/missing.js").await.status, 404);
    assert_eq!(n.get("/_astro/nope.css").await.status, 404);
    assert_eq!(n.raw(format!("POST /c/x HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n\r\n", n.host()).as_bytes()).await.status, 405);

    // hashed assets referenced by the built page are served immutable with the right type
    let assets: Vec<&str> = html.split('"').filter(|s| s.starts_with("/_astro/")).collect();
    if assets.is_empty() {
        assert!(html.contains("Web UI not built"), "no web/dist: placeholder page explains how to build it");
    }
    for a in assets {
        let r = n.get(a).await;
        assert_eq!(r.status, 200, "{a}");
        assert!(r.header("cache-control").unwrap().contains("immutable"));
        let ct = r.header("content-type").unwrap();
        if a.ends_with(".js") {
            assert_eq!(ct, "text/javascript");
        } else if a.ends_with(".css") {
            assert_eq!(ct, "text/css");
        }
        assert!(!r.body.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_transport_rules() {
    let n = spawn(|_| {}).await;
    // method
    let r = n.get("/api/info").await;
    assert_eq!(r.status, 405);
    assert!(pb::decode::<pb::Error>(&r.body).unwrap().message.contains("POST"));
    // content type: JSON and form posts are refused (CSRF), with a protobuf Error
    for ct in ["", "Content-Type: application/json\r\n", "Content-Type: application/x-www-form-urlencoded\r\n", "Content-Type: text/plain\r\n"] {
        let r = n.post_raw("/api/info", ct, b"").await;
        assert_eq!(r.status, 415, "{ct:?}");
        assert!(pb::decode::<pb::Error>(&r.body).unwrap().message.contains("application/x-protobuf"));
    }
    assert_eq!(n.post_raw("/api/info", "Content-Type: application/x-protobuf; charset=binary\r\n", b"").await.status, 200);
    // unknown tool, trailing slash, bad body
    let r = n.post_raw("/api/nope", PB, b"").await;
    assert_eq!(r.status, 404);
    assert!(pb::decode::<pb::Error>(&r.body).unwrap().message.contains("nope"));
    assert_eq!(n.post_raw("/api/info/", PB, b"").await.status, 200);
    assert_eq!(n.post_raw("/api/feed", PB, &[0xff, 0xff, 0xff]).await.status, 400);
    // size limits
    let r = n.raw(format!("POST /api/post HTTP/1.1\r\nHost: {}\r\n{PB}Content-Length: 2000000\r\n\r\n", n.host()).as_bytes()).await;
    assert_eq!(r.status, 413);
    let huge = format!("GET / HTTP/1.1\r\nHost: x\r\nX-Pad: {}\r\n\r\n", "a".repeat(20_000));
    assert_eq!(n.raw(huge.as_bytes()).await.status, 431);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_split_across_packets() {
    let n = spawn(|_| {}).await;
    let body = pb::encode(&post("m", "slow", "client"));
    let head = format!("POST /api/post HTTP/1.1\r\nHost: {}\r\n{PB}Content-Length: {}\r\n\r\n", n.host(), body.len());
    let mut s = tokio::net::TcpStream::connect(n.addr).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    for chunk in body.chunks(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.write_all(chunk).await.unwrap();
    }
    let mut buf = vec![];
    s.read_to_end(&mut buf).await.unwrap();
    let r = parse_resp(&buf);
    assert_eq!(r.status, 200);
    let a: pb::Ack = pb::decode(&r.body).unwrap();
    assert!(n.has(&a.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn csrf_and_dns_rebinding_are_refused() {
    let n = spawn(|_| {}).await;
    let body = pb::encode(&post("m", "t", "x"));
    let send = |host: String, origin: Option<&str>, tool: &str| {
        let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        format!("POST /api/{tool} HTTP/1.1\r\nHost: {host}\r\n{origin}{PB}Content-Length: {}\r\n\r\n", body.len()).into_bytes()
    };
    let with_body = |mut h: Vec<u8>| {
        h.extend_from_slice(&body);
        h
    };
    let r = n.raw(&with_body(send(n.host(), Some("http://evil.example"), "post"))).await;
    assert_eq!(r.status, 403);
    assert!(pb::decode::<pb::Error>(&r.body).unwrap().message.contains("cross-origin"));
    let r = n.raw(&with_body(send("evil.example".into(), None, "post"))).await;
    assert_eq!(r.status, 403, "rebinding: attacker hostname resolving to 127.0.0.1");
    let r = n.raw(&with_body(send(n.host(), Some(&format!("http://{}", n.host())), "post"))).await;
    assert_eq!(r.status, 200, "same-origin UI");
    let r = n.raw(&with_body(send(format!("localhost:{}", n.addr.port()), None, "post"))).await;
    assert_eq!(r.status, 200);
    // private state is a write tool too
    let r = n.raw(&with_body(send("evil.example".into(), None, "memory_get"))).await;
    assert_eq!(r.status, 403);
    // public reads stay open to anyone (everything on the network is public)
    let r = n.raw(&with_body(send("evil.example".into(), Some("http://evil.example"), "info"))).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("access-control-allow-origin"), None, "no CORS grants on the API");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_token_admits_other_hosts() {
    let n = spawn(|c| c.api_token = Some("tok".into())).await;
    let body = pb::encode(&post("m", "t", "x"));
    let req = |auth: &str| {
        let mut v = format!("POST /api/post HTTP/1.1\r\nHost: node.example\r\n{auth}{PB}Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(&body);
        v
    };
    assert_eq!(n.raw(&req("Authorization: Bearer tok\r\n")).await.status, 200);
    assert_eq!(n.raw(&req("Authorization: Bearer nope\r\n")).await.status, 403);
    assert_eq!(n.raw(&req("")).await.status, 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nip11_relay_information() {
    let n = spawn(|_| {}).await;
    let r = n.raw(format!("GET / HTTP/1.1\r\nHost: {}\r\nAccept: application/nostr+json\r\n\r\n", n.host()).as_bytes()).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/nostr+json"));
    assert_eq!(r.header("access-control-allow-origin"), Some("*"));
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["pubkey"], n.node.keys.pubkey.hex());
    assert_eq!(v["supported_nips"], serde_json::json!([1, 9, 11, 19]));
    assert_eq!(v["limitation"]["max_subscriptions"], 32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_upgrade_requires_key() {
    let n = spawn(|_| {}).await;
    let r = n.raw(format!("GET / HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n", n.host()).as_bytes()).await;
    assert_eq!(r.status, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_clients() {
    let n = std::sync::Arc::new(spawn(|_| {}).await);
    let tasks: Vec<_> = (0..64)
        .map(|i| {
            let n = n.clone();
            tokio::spawn(async move {
                if i % 2 == 0 {
                    n.ok::<_, pb::Ack>("post", &post("load", &format!("t{i}"), "x")).await;
                } else {
                    assert_eq!(n.get("/").await.status, 200);
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let f: pb::Items = n.ok("feed", &pb::FeedRequest { community: "load".into(), limit: 100, ..Default::default() }).await;
    assert_eq!(f.items.len(), 32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_connections_time_out() {
    let n = spawn(|_| {}).await;
    let mut s = tokio::net::TcpStream::connect(n.addr).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\n").await.unwrap(); // never finish the head
    let mut buf = vec![];
    let r = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut buf)).await;
    assert!(r.is_ok(), "server closes slowloris connections");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docs_are_static_pages_with_working_links() {
    let n = spawn(|_| {}).await;
    let app = n.get("/").await.text();
    if app.contains("Web UI not built") {
        eprintln!("SKIP: web/dist not built");
        return;
    }
    // /docs/x is served from docs/x/index.html, with or without a trailing slash
    for p in ["/docs", "/docs/", "/docs/architecture", "/docs/architecture/"] {
        let r = n.get(p).await;
        assert_eq!(r.status, 200, "{p}");
        assert!(r.text().contains("Sametra docs</title>"), "{p} is a docs page, not the app");
        assert!(r.header("content-security-policy").is_some());
    }
    // crawl: every internal link from the docs resolves, and every #fragment exists on its page
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec!["/docs".to_string()];
    while let Some(page) = queue.pop() {
        if !seen.insert(page.clone()) {
            continue;
        }
        let html = n.get(&page).await.text();
        assert!(!html.contains("<script src") && !html.contains("type=\"module\""), "{page}: docs load no script files (only the inline theme switch)");
        for href in html.split("href=\"").skip(1).filter_map(|s| s.split('"').next()) {
            if !href.starts_with('/') || href.starts_with("/_astro/") {
                continue;
            }
            let (path, frag) = href.split_once('#').unwrap_or((href, ""));
            let path = if path.is_empty() { page.as_str() } else { path };
            let target = n.get(path).await;
            assert_eq!(target.status, 200, "{page} links to {href}");
            if path.starts_with("/docs") {
                assert!(target.text().contains("Sametra docs</title>"), "{page} links to missing doc {href}");
                queue.push(path.trim_end_matches('/').to_string());
            }
            if !frag.is_empty() {
                assert!(target.text().contains(&format!("id=\"{frag}\"")), "{page}: anchor {href} does not exist");
            }
        }
    }
    for p in ["/docs/start", "/docs/using", "/docs/protocol", "/docs/reputation", "/docs/search", "/docs/capabilities", "/docs/retention", "/docs/agents", "/docs/api", "/docs/security"] {
        assert!(seen.contains(p), "{p} is reachable from the docs navigation");
    }
    // unknown docs paths fall back to the app, which shows its own not-found view
    assert_eq!(n.get("/docs/nope").await.text(), app);
}
