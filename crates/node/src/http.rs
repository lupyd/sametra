//! One port, three faces: nostr relay (WebSocket), protobuf API, and the web UI (SPA + static docs).
//! A tiny hand-rolled HTTP/1.1 reader keeps the binary small (no hyper/axum).
//! With a certificate configured the same port also speaks TLS: connections that
//! open with a TLS ClientHello get HTTPS/WSS, the rest plain HTTP/WS.

use crate::{api, pb};
use crate::node::{self, Node};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod web {
    include!(concat!(env!("OUT_DIR"), "/web_files.rs"));
}
const NO_UI: &[u8] = b"<!doctype html><meta charset=utf-8><title>Sametra</title><p>Web UI not built. Run <code>pnpm -C web install && pnpm -C web build</code> then rebuild the node. The API is at <code>/api/&lt;tool&gt;</code>.";
const PROTO: &str = "application/x-protobuf";
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 1 << 20;

pub async fn serve(node: Arc<Node>, listener: TcpListener) {
    let sem = Arc::new(tokio::sync::Semaphore::new(1024));
    loop {
        let Ok((sock, peer)) = listener.accept().await else { continue };
        let Ok(permit) = sem.clone().try_acquire_owned() else { continue }; // shed load
        let _ = sock.set_nodelay(true);
        let node = node.clone();
        tokio::spawn(async move {
            let _p = permit;
            let _ = conn(node, sock, peer).await;
        });
    }
}

struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}
impl Req {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

const TIMEOUT: Duration = Duration::from_secs(10);

async fn conn(node: Arc<Node>, sock: TcpStream, peer: SocketAddr) -> std::io::Result<()> {
    #[cfg(feature = "tls")]
    if let Some(acceptor) = node.tls.clone() {
        let timed_out = |_| std::io::Error::from(std::io::ErrorKind::TimedOut);
        if tokio::time::timeout(TIMEOUT, sametra_transport::starts_with_tls(&sock)).await.map_err(timed_out)?? {
            let tls = tokio::time::timeout(TIMEOUT, acceptor.accept(sock)).await.map_err(timed_out)??;
            return handle(node, tls, peer, true).await;
        }
    }
    handle(node, sock, peer, false).await
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin + Send>(node: Arc<Node>, mut sock: S, peer: SocketAddr, secure: bool) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let head_end = loop {
        let mut chunk = [0u8; 2048];
        let n = tokio::time::timeout(TIMEOUT, sock.read(&mut chunk)).await.map_err(|_| std::io::ErrorKind::TimedOut)??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return respond(&mut sock, 431, "text/plain", b"headers too large").await;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/");
    let path = target.split_once('?').map(|x| x.0).unwrap_or(target);
    let r = Req {
        method,
        path: path.to_string(),
        headers: lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect(),
    };

    // --- nostr relay over WebSocket
    if r.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        let Some(key) = r.header("sec-websocket-key") else { return respond(&mut sock, 400, "text/plain", b"missing key").await };
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let resp = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
        sock.write_all(resp.as_bytes()).await?;
        node::serve_ws(node, sock, buf[head_end..].to_vec(), peer.to_string()).await;
        return Ok(());
    }

    // --- NIP-11 relay information
    if r.path == "/" && r.header("accept").is_some_and(|a| a.contains("application/nostr+json")) {
        let info = json!({
            "name": "sametra", "description": "Sametra P2P collective knowledge node",
            "pubkey": node.keys.pubkey, "software": "sametra", "version": env!("CARGO_PKG_VERSION"),
            "supported_nips": [1, 9, 11, 19],
            "limitation": {"max_message_length": 262144, "max_subscriptions": 32, "max_content_length": 65536},
        });
        return respond_cors(&mut sock, 200, "application/nostr+json", info.to_string().as_bytes()).await;
    }

    let Some(tool) = r.path.strip_prefix("/api/") else {
        return static_file(&mut sock, &r).await;
    };
    let tool = tool.trim_end_matches('/').to_string();
    if r.method != "POST" {
        return pb_resp(&mut sock, 405, &pb::Error { message: "use POST with a protobuf body".into() }).await;
    }
    // Requiring the protobuf content type also blocks cross-site form posts (CSRF):
    // browsers must preflight it, and we never grant CORS.
    if !r.header("content-type").is_some_and(|c| c.starts_with(PROTO)) {
        return pb_resp(&mut sock, 415, &pb::Error { message: format!("Content-Type must be {PROTO}") }).await;
    }
    let clen: usize = r.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if clen > MAX_BODY {
        return respond(&mut sock, 413, "text/plain", b"body too large").await;
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < clen {
        let mut chunk = vec![0u8; (clen - body.len()).min(64 * 1024)];
        let n = tokio::time::timeout(TIMEOUT, sock.read(&mut chunk)).await.map_err(|_| std::io::ErrorKind::TimedOut)??;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(clen);

    if api::is_write(&tool) && let Err(e) = authorize(&node, &r, &peer, secure) {
        return pb_resp(&mut sock, 403, &pb::Error { message: e.into() }).await;
    }
    match api::dispatch(&node, &tool, &body).await {
        Some(Ok(bytes)) => respond(&mut sock, 200, PROTO, &bytes).await,
        Some(Err(e)) => pb_resp(&mut sock, 400, &pb::Error { message: e }).await,
        None => pb_resp(&mut sock, 404, &pb::Error { message: format!("unknown tool '{tool}'") }).await,
    }
}

/// Embedded SPA: exact file match, else index.html so client-side routes deep-link.
async fn static_file<S: AsyncWrite + Unpin>(sock: &mut S, r: &Req) -> std::io::Result<()> {
    if r.method != "GET" {
        return respond(sock, 405, "text/plain", b"method not allowed").await;
    }
    let find = |p: &str| web::FILES.iter().find(|(n, _)| *n == p).map(|(_, b)| *b);
    // static pages (the docs) are built as `<dir>/index.html`
    let dir_index = format!("{}/index.html", r.path.trim_end_matches('/'));
    let (path, body) = match find(&r.path) {
        Some(b) => (r.path.as_str(), b),
        None if r.path.rsplit('/').next().is_some_and(|l| l.contains('.')) => return respond(sock, 404, "text/plain", b"not found").await,
        None => match find(&dir_index) {
            Some(b) => (dir_index.as_str(), b),
            None => ("/index.html", find("/index.html").unwrap_or(NO_UI)),
        },
    };
    let ct = match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" | "webmanifest" => "application/json",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    };
    // Astro fingerprints everything under /_astro/, so those can be cached forever.
    let cache = if path.starts_with("/_astro/") { "Cache-Control: public, max-age=31536000, immutable\r\n" } else { "" };
    let csp = "Content-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\n";
    write_resp(sock, 200, ct, body, &format!("{cache}{csp}")).await
}

/// Write tools sign with the node's identity: only the local operator (or a bearer
/// of the API token) may use them. Origin/Host checks stop browsers on other sites
/// (CSRF, DNS rebinding) from driving a node on localhost.
fn authorize(node: &Node, r: &Req, peer: &SocketAddr, secure: bool) -> Result<(), &'static str> {
    if let Some(tok) = &node.cfg.api_token
        && r.header("authorization").and_then(|h| h.strip_prefix("Bearer ")) == Some(tok.as_str()) {
            return Ok(());
        }
    if !peer.ip().is_loopback() {
        return Err("write tools are only available from localhost or with the API token");
    }
    let host = r.header("host").unwrap_or("");
    let hostname = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    if !matches!(hostname, "localhost" | "127.0.0.1" | "[::1]") {
        return Err("bad Host header");
    }
    if let Some(o) = r.header("origin") {
        let ok = o == format!("{}://{host}", if secure { "https" } else { "http" });
        if !ok {
            return Err("cross-origin write refused");
        }
    }
    Ok(())
}

async fn pb_resp<S: AsyncWrite + Unpin, M: sametra_proto::quick_protobuf::MessageWrite>(sock: &mut S, code: u16, m: &M) -> std::io::Result<()> {
    respond(sock, code, PROTO, &pb::encode(m)).await
}

async fn respond_cors<S: AsyncWrite + Unpin>(sock: &mut S, code: u16, ct: &str, body: &[u8]) -> std::io::Result<()> {
    write_resp(sock, code, ct, body, "Access-Control-Allow-Origin: *\r\n").await
}

async fn respond<S: AsyncWrite + Unpin>(sock: &mut S, code: u16, ct: &str, body: &[u8]) -> std::io::Result<()> {
    write_resp(sock, code, ct, body, "").await
}

async fn write_resp<S: AsyncWrite + Unpin>(sock: &mut S, code: u16, ct: &str, body: &[u8], extra: &str) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n{}X-Content-Type-Options: nosniff\r\n{extra}\r\n",
        body.len(),
        if extra.contains("Cache-Control") { "" } else { "Cache-Control: no-cache\r\n" }
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Keys;
    use crate::node::Config;

    fn node(token: Option<&str>) -> Arc<Node> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let data = std::env::temp_dir().join(format!("sametra-http-{}-{n}", std::process::id()));
        Node::new(Config { data, api_token: token.map(String::from), ..Default::default() }, Keys::generate()).unwrap()
    }
    fn req(h: &[(&str, &str)]) -> Req {
        Req { method: "POST".into(), path: "/api/post".into(), headers: h.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }
    const LOCAL: &str = "127.0.0.1:5555";
    const REMOTE: &str = "203.0.113.9:5555";

    #[test]
    fn authorize_rules() {
        let n = node(Some("s3cret"));
        let a = |h: &[(&str, &str)], peer: &str| authorize(&n, &req(h), &peer.parse().unwrap(), false);
        assert!(a(&[("Host", "127.0.0.1:7447")], LOCAL).is_ok());
        assert!(a(&[("host", "localhost:7447"), ("Origin", "http://localhost:7447")], LOCAL).is_ok());
        assert!(a(&[("Host", "[::1]:7447")], "[::1]:9").is_ok());
        assert!(a(&[("Host", "127.0.0.1:7447")], REMOTE).is_err(), "remote without token");
        assert_eq!(a(&[("Host", "evil.example:7447")], LOCAL), Err("bad Host header"), "DNS rebinding");
        assert_eq!(a(&[("Host", "127.0.0.1:7447"), ("Origin", "http://evil.example")], LOCAL), Err("cross-origin write refused"));
        assert!(a(&[("Host", "127.0.0.1:7447"), ("Origin", "http://127.0.0.1:9999")], LOCAL).is_err(), "other local port");
        assert!(a(&[], LOCAL).is_err(), "Host is required");
        // the token admits remote callers regardless of Host
        assert!(a(&[("Authorization", "Bearer s3cret"), ("Host", "node.example")], REMOTE).is_ok());
        assert!(a(&[("Authorization", "Bearer wrong"), ("Host", "node.example")], REMOTE).is_err());
        assert!(a(&[("Authorization", "s3cret")], REMOTE).is_err(), "Bearer scheme required");
        // without a configured token, a Bearer header grants nothing
        let open = node(None);
        assert!(authorize(&open, &req(&[("Authorization", "Bearer "), ("Host", "x")]), &REMOTE.parse().unwrap(), false).is_err());
        // over TLS the page's origin is https://
        let s = |o: &str| authorize(&open, &req(&[("Host", "localhost:7447"), ("Origin", o)]), &LOCAL.parse().unwrap(), true);
        assert!(s("https://localhost:7447").is_ok());
        assert!(s("http://localhost:7447").is_err());
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let r = req(&[("Content-Type", "a"), ("X", "b")]);
        assert_eq!(r.header("content-type"), Some("a"));
        assert_eq!(r.header("CONTENT-TYPE"), Some("a"));
        assert_eq!(r.header("y"), None);
        assert_eq!(find(b"abc\r\n\r\nx", b"\r\n\r\n"), Some(3));
    }
}
