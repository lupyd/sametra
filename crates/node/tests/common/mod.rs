//! Shared harness: in-process nodes on ephemeral ports, a raw HTTP client,
//! protobuf API calls, and polling helpers.
#![allow(dead_code)]

pub mod kubo;

use quick_protobuf::{MessageRead, MessageWrite};
use sametra_node::event::{Event, K_POST, Keys, ancestry_tags};
use sametra_node::node::{self, Config, Node};
use sametra_node::{http, pb};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn tempdir(name: &str) -> TempDir {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!("sametra-it-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

pub struct TestNode {
    pub node: Arc<Node>,
    pub addr: SocketAddr,
    pub ws: String,
    pub dir: TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for TestNode {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Resp {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Start a node (relay + API + UI) on 127.0.0.1:<ephemeral>.
pub async fn spawn(f: impl FnOnce(&mut Config)) -> TestNode {
    spawn_with(Keys::generate(), tempdir("node"), f).await
}

pub async fn spawn_with(keys: Keys, dir: TempDir, f: impl FnOnce(&mut Config)) -> TestNode {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut cfg = Config { listen: addr.to_string(), data: dir.0.clone(), ..Default::default() };
    f(&mut cfg);
    let node = Node::new(cfg, keys).unwrap();
    let server = tokio::spawn(http::serve(node.clone(), listener));
    TestNode { node, addr, ws: format!("ws://{addr}"), dir, server }
}

impl TestNode {
    pub fn host(&self) -> String {
        self.addr.to_string()
    }

    /// Send raw bytes, read the whole response (the server always closes).
    pub async fn raw(&self, req: &[u8]) -> Resp {
        let mut s = TcpStream::connect(self.addr).await.unwrap();
        s.write_all(req).await.unwrap();
        let mut buf = vec![];
        tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut buf)).await.expect("response timeout").unwrap();
        parse_resp(&buf)
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.raw(format!("GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n", self.host()).as_bytes()).await
    }

    pub async fn post_raw(&self, path: &str, extra_headers: &str, body: &[u8]) -> Resp {
        let head = format!("POST {path} HTTP/1.1\r\nHost: {}\r\n{extra_headers}Content-Length: {}\r\n\r\n", self.host(), body.len());
        self.raw(&[head.as_bytes(), body].concat()).await
    }

    /// Protobuf API call; Err((status, message)) on failure.
    pub async fn call<Q: MessageWrite, R: for<'a> MessageRead<'a>>(&self, tool: &str, req: &Q) -> Result<R, (u16, String)> {
        let r = self.post_raw(&format!("/api/{tool}"), "Content-Type: application/x-protobuf\r\n", &pb::encode(req)).await;
        if r.status == 200 {
            Ok(pb::decode(&r.body).expect("response decodes"))
        } else {
            Err((r.status, pb::decode::<pb::Error>(&r.body).map(|e| e.message).unwrap_or_else(|_| r.text())))
        }
    }

    pub async fn ok<Q: MessageWrite, R: for<'a> MessageRead<'a>>(&self, tool: &str, req: &Q) -> R {
        match self.call(tool, req).await {
            Ok(r) => r,
            Err(e) => panic!("{tool} failed: {e:?}"),
        }
    }

    pub async fn err<Q: MessageWrite>(&self, tool: &str, req: &Q) -> (u16, String) {
        match self.call::<Q, pb::Empty>(tool, req).await {
            Ok(_) => panic!("{tool} unexpectedly succeeded"),
            Err(e) => e,
        }
    }

    /// Dial `other` and wait until both ends see the link as a sametra peer.
    pub async fn connect(&self, other: &TestNode) {
        self.connect_url(other, &other.ws.clone()).await
    }

    /// Like `connect`, through an explicit address (e.g. `wss://localhost:port`).
    pub async fn connect_url(&self, other: &TestNode, url: &str) {
        tokio::spawn(node::dial(self.node.clone(), url.to_string()));
        let (me, them) = (self.node.clone(), other.node.clone());
        assert!(
            eventually(Duration::from_secs(10), || {
                me.peers.lock().unwrap().values().any(|p| p.outbound && p.addr == url)
                    && them.peers.lock().unwrap().values().any(|p| !p.outbound && p.sametra.load(Ordering::Relaxed))
            })
            .await,
            "link to {url} did not come up"
        );
    }

    /// Raw HTTPS request (trusting `ca`), whole response.
    #[cfg(feature = "tls")]
    pub async fn raw_tls(&self, ca: &std::path::Path, req: &[u8]) -> Result<Resp, String> {
        use sametra_node::transport::tls;
        let cfg = tls::client_config(&[ca.to_path_buf()])?;
        let tcp = TcpStream::connect(self.addr).await.map_err(|e| e.to_string())?;
        let mut s = tls::connect(&cfg, "localhost", tcp).await?;
        s.write_all(req).await.map_err(|e| e.to_string())?;
        let mut buf = vec![];
        let _ = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut buf)).await.map_err(|_| "timeout")?;
        Ok(parse_resp(&buf))
    }

    pub fn has(&self, id: &str) -> bool {
        sametra_node::event::H32::from_hex(id).is_some_and(|h| self.node.store.lock().unwrap().get(&h).is_some())
    }
}

pub fn parse_resp(buf: &[u8]) -> Resp {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("no header terminator");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    Resp { status, headers, body: buf[split + 4..].to_vec() }
}

/// Poll `f` every 25 ms until true or timeout.
pub async fn eventually(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

/// A new top-level topic.
pub fn post(title: &str, content: &str) -> pb::PostRequest {
    pb::PostRequest { title: title.into(), content: content.into(), ..Default::default() }
}
pub fn reply(parent: &str, content: &str) -> pb::PostRequest {
    pb::PostRequest { parent: parent.into(), content: content.into(), ..Default::default() }
}
/// A signed post by `k` under `parent` (naming all of parent's ancestors).
pub fn under(k: &Keys, parent: &Event, content: &str) -> Event {
    k.sign(K_POST, ancestry_tags(&parent.id, &parent.ancestors()), content.into())
}
pub fn id(i: &str) -> pb::IdRequest {
    pb::IdRequest { id: i.into(), thread: false }
}
pub fn tags(v: &[&[&str]]) -> Vec<Vec<String>> {
    v.iter().map(|t| t.iter().map(|s| s.to_string()).collect()).collect()
}
