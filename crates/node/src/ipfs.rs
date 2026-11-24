//! A built-in IPFS node (rust-ipfs: libp2p, Bitswap, Kademlia, UnixFS).
//!
//! Every Sametra node is also an IPFS node. Files added here (`ipfs_add`, the web
//! app's attach button) get a CID that posts link to as `ipfs://CID`. Nodes find
//! each other's IPFS side through their heartbeats and swap blocks over Bitswap;
//! with `public` they also join the public IPFS network (DHT), so any IPFS client
//! can fetch from them, and fall back to trustless HTTP gateways for content no
//! peer has. Files that posts link to are fetched and kept while there is room
//! (`cache_mb`), so popular files end up on many nodes. Pinned files stay.
//!
//! Over HTTP the node serves `/ipfs/CID[/path]`: files for browsers, and raw
//! blocks (`?format=raw`, or `Accept: application/vnd.ipld.raw`) for IPFS clients
//! that verify what they fetch (a trustless gateway).

use bytes::Bytes;
use futures_util::StreamExt;
use ipld_core::cid::Cid;
use rust_ipfs::builder::DefaultIpfsBuilder;
use rust_ipfs::repo::{GCConfig, GCTrigger};
use rust_ipfs::unixfs::{AddOpt, UnixfsStatus};
use rust_ipfs::{IpfsPath, Keypair, Multiaddr, PeerId};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// TCP and UDP (QUIC) port for libp2p; 0 picks one.
    pub port: u16,
    /// Join the public IPFS network (DHT bootstrap) and use public HTTP gateways.
    pub public: bool,
    /// Room for unpinned (cached) blocks, in MiB; older blocks are collected beyond it.
    pub cache_mb: usize,
    /// Fetch at most this many bytes when caching a file a post links to.
    pub max_file: usize,
    /// Host to announce our address with (from the node's public address), if any.
    pub announce_host: Option<String>,
    /// Fetch from the network for anyone asking /ipfs/ over HTTP, not only the
    /// node's operator (others get what this node already has).
    pub open_gateway: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config { port: 7449, public: true, cache_mb: 1024, max_file: 32 << 20, announce_host: None, open_gateway: false }
    }
}

pub struct Ipfs {
    inner: rust_ipfs::Ipfs,
    pub peer_id: PeerId,
    pub cfg: Config,
    /// IPFS peers learned from Sametra heartbeats
    known: Mutex<HashSet<PeerId>>,
    /// files being fetched for the cache
    fetching: Mutex<HashSet<Cid>>,
    fetch_slots: tokio::sync::Semaphore,
}

pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `CID`, `ipfs://CID/path`, `/ipfs/CID/path`.
pub fn parse_path(s: &str) -> Result<IpfsPath, String> {
    let s = s.trim();
    let rest = s.strip_prefix("ipfs://").or_else(|| s.strip_prefix("/ipfs/")).unwrap_or(s);
    format!("/ipfs/{rest}").parse::<IpfsPath>().map_err(|e| format!("not an IPFS path '{s}': {e}"))
}

/// The CIDs that a text links to as `ipfs://CID` (or a `/ipfs/CID` gateway path).
pub fn linked_cids(text: &str) -> Vec<Cid> {
    let mut out = vec![];
    for marker in ["ipfs://", "/ipfs/"] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            if let Ok(c) = rest[..end].parse::<Cid>()
                && !out.contains(&c)
                && out.len() < 16
            {
                out.push(c);
            }
        }
    }
    out
}

impl Ipfs {
    /// Start, with the libp2p key and blocks under `dir`.
    pub async fn start(dir: &Path, cfg: Config) -> Result<Ipfs, String> {
        std::fs::create_dir_all(dir).map_err(err)?;
        let key_file = dir.join("key");
        let mut seed = match std::fs::read(&key_file) {
            Ok(b) if b.len() == 32 => b,
            _ => {
                let mut b = vec![0u8; 32];
                getrandom::getrandom(&mut b).map_err(err)?;
                std::fs::write(&key_file, &b).map_err(err)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600));
                }
                b
            }
        };
        let key = Keypair::ed25519_from_bytes(&mut seed).map_err(err)?;
        let peer_id = key.public().to_peer_id();
        let tcp: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", cfg.port).parse().map_err(err)?;
        let quic: Multiaddr = format!("/ip4/0.0.0.0/udp/{}/quic-v1", cfg.port).parse().map_err(err)?;
        let mut b = DefaultIpfsBuilder::with_keypair(key)
            .map_err(err)?
            .with_default()
            .enable_tcp()
            .enable_quic()
            .enable_dns()
            .set_path(dir.join("repo"))
            .set_listening_addrs(vec![tcp, quic])
            .with_gc(GCConfig { duration: Duration::from_secs(600), trigger: GCTrigger::AtStorage });
        if cfg.public {
            b = b.enable_gateway_retrieval(Vec::<String>::new());
        }
        let inner = b.start().await.map_err(err)?;
        inner.repo().set_max_storage_size(cfg.cache_mb << 20);
        // listeners bind in the background: wait for them, so we announce real addresses
        for _ in 0..100 {
            if !inner.listening_addresses().await.unwrap_or_default().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if cfg.public {
            let i = inner.clone();
            tokio::spawn(async move {
                match i.default_bootstrap().await {
                    Ok(_) => match i.bootstrap().await {
                        Ok(()) => tracing::info!("joined the public IPFS network"),
                        Err(e) => tracing::warn!(error = %e, "public IPFS bootstrap failed; Sametra peers still work"),
                    },
                    Err(e) => tracing::warn!(error = %e, "no public IPFS bootstrap peers"),
                }
            });
        }
        Ok(Ipfs { inner, peer_id, cfg, known: Mutex::default(), fetching: Mutex::default(), fetch_slots: tokio::sync::Semaphore::new(4) })
    }

    /// Where other nodes can reach us, for heartbeats: `["ipfs", peer id, addr...]`.
    pub async fn heartbeat_tag(&self) -> Vec<String> {
        let mut tag = vec!["ipfs".to_string(), self.peer_id.to_string()];
        let addrs: Vec<String> = match &self.cfg.announce_host {
            Some(h) => {
                let proto = if h.parse::<std::net::Ipv4Addr>().is_ok() {
                    "ip4"
                } else if h.parse::<std::net::Ipv6Addr>().is_ok() {
                    "ip6"
                } else {
                    "dns"
                };
                let port = self.listen_port().await;
                vec![format!("/{proto}/{h}/tcp/{port}"), format!("/{proto}/{h}/udp/{port}/quic-v1")]
            }
            None => self.inner.listening_addresses().await.unwrap_or_default().iter().map(|a| a.to_string()).filter(|a| !a.contains("/0.0.0.0/")).collect(),
        };
        tag.extend(addrs.into_iter().take(4));
        tag
    }

    async fn listen_port(&self) -> u16 {
        for a in self.inner.listening_addresses().await.unwrap_or_default() {
            for p in a.iter() {
                if let rust_ipfs::Protocol::Tcp(port) = p {
                    return port;
                }
            }
        }
        self.cfg.port
    }

    pub async fn listening(&self) -> Vec<String> {
        self.inner.listening_addresses().await.unwrap_or_default().iter().map(|a| a.to_string()).collect()
    }

    pub async fn connected(&self) -> usize {
        self.inner.connected().await.map(|c| c.len()).unwrap_or(0)
    }

    /// A Sametra peer's IPFS side, from its heartbeat tag.
    pub async fn learn(&self, tag: &[String]) {
        let Some(peer) = tag.get(1).and_then(|p| p.parse::<PeerId>().ok()) else { return };
        if peer == self.peer_id {
            return;
        }
        {
            let mut k = self.known.lock().unwrap();
            if k.len() > 5000 || !k.insert(peer) {
                return;
            }
        }
        let addrs: Vec<Multiaddr> = tag[2..].iter().take(4).filter_map(|a| a.parse().ok()).collect();
        tracing::debug!(%peer, addrs = addrs.len(), "learned a Sametra node's IPFS address");
        for a in addrs {
            let _ = self.inner.add_peer((peer, a)).await;
        }
        if let Err(e) = self.inner.connect(peer).await {
            tracing::debug!(%peer, error = %e, "could not reach its IPFS side");
        }
    }

    /// Add bytes as a UnixFS file; its CID.
    pub async fn add(&self, data: Vec<u8>, pin: bool) -> Result<Cid, String> {
        let chunks = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(data))]).boxed();
        let mut add = self.inner.add_unixfs(AddOpt::Stream(chunks)).pin(pin).provide();
        while let Some(st) = add.next().await {
            match st {
                UnixfsStatus::CompletedStatus { path, .. } => return path.root().cid().copied().ok_or_else(|| "added, but no CID".to_string()),
                UnixfsStatus::FailedStatus { error, .. } => return Err(err(error)),
                UnixfsStatus::ProgressStatus { .. } => {}
            }
        }
        Err("add ended without a result".into())
    }

    /// A file's bytes, at most `max` of them (true if cut short). `local`: only from
    /// this node's blocks, never the network.
    pub async fn cat(&self, path: &str, max: usize, local: bool, timeout: Duration) -> Result<(Vec<u8>, bool), String> {
        let p = parse_path(path)?;
        let mut s = self.inner.cat_unixfs(p).set_local(local).timeout(timeout);
        let mut out = vec![];
        while let Some(chunk) = s.next().await {
            let chunk = chunk.map_err(err)?;
            if out.len() + chunk.len() > max {
                out.extend_from_slice(&chunk[..max - out.len()]);
                return Ok((out, true));
            }
            out.extend_from_slice(&chunk);
        }
        Ok((out, false))
    }

    /// A file as a stream of chunks, for HTTP.
    pub fn cat_stream(&self, path: IpfsPath, local: bool) -> impl futures_util::Stream<Item = Result<Bytes, String>> + Send + use<> {
        self.inner.cat_unixfs(path).set_local(local).timeout(Duration::from_secs(60)).map(|c| c.map_err(err))
    }

    /// One raw block (trustless gateway).
    pub async fn block(&self, cid: &Cid, local: bool) -> Result<Vec<u8>, String> {
        let b = self.inner.get_block(cid).set_local(local).timeout(Duration::from_secs(60)).await.map_err(err)?;
        Ok(b.data().to_vec())
    }

    pub async fn pin(&self, path: &str, pin: bool) -> Result<Cid, String> {
        let p = parse_path(path)?;
        let cid = *p.root().cid().ok_or("not a CID path")?;
        if pin {
            self.inner.insert_pin(cid).recursive().await.map_err(err)?;
            let _ = self.inner.provide(cid).await;
        } else {
            self.inner.remove_pin(cid).recursive().await.map_err(err)?;
        }
        Ok(cid)
    }

    pub async fn is_pinned(&self, cid: &Cid) -> bool {
        self.inner.is_pinned(cid).await.unwrap_or(false)
    }

    /// Fetch and keep (unpinned, collectable) a file a post links to, and offer it
    /// to the network. At most `max_file` bytes; a few at a time; never twice at once.
    pub async fn cache(&self, cid: Cid) {
        if !self.fetching.lock().unwrap().insert(cid) {
            return;
        }
        if let Ok(_slot) = self.fetch_slots.acquire().await {
            let path = format!("/ipfs/{cid}");
            match self.cat(&path, self.cfg.max_file, false, Duration::from_secs(120)).await {
                Ok((data, false)) => {
                    tracing::debug!(%cid, bytes = data.len(), "kept a file a post links to");
                    let _ = self.inner.provide(cid).await;
                }
                Ok((_, true)) => tracing::debug!(%cid, max = self.cfg.max_file, "linked file is too large to keep"),
                Err(e) => tracing::debug!(%cid, error = %e, "could not fetch a linked file"),
            }
        }
        self.fetching.lock().unwrap().remove(&cid);
    }
}

/// A content type for a file, from its name, else its first bytes. Never HTML:
/// files are other people's content served from the node's own origin.
pub fn content_type(name: &str, head: &[u8]) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let by_ext = match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "pdf" => Some("application/pdf"),
        "json" => Some("application/json"),
        "csv" => Some("text/csv; charset=utf-8"),
        "md" | "markdown" => Some("text/markdown; charset=utf-8"),
        "txt" | "html" | "htm" | "tex" | "lean" | "py" | "rs" | "js" | "ts" => Some("text/plain; charset=utf-8"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mp3" => Some("audio/mpeg"),
        "wasm" => Some("application/wasm"),
        _ => None,
    };
    by_ext.unwrap_or(if head.starts_with(b"\x89PNG") {
        "image/png"
    } else if head.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if head.starts_with(b"GIF8") {
        "image/gif"
    } else if head.starts_with(b"%PDF") {
        "application/pdf"
    } else if head.len() > 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        "image/webp"
    } else if std::str::from_utf8(&head[..head.len().min(512)]).is_ok() || head.len() >= 512 && std::str::from_utf8(&head[..508]).is_ok() {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "bafkreihkavvxs6g6cxquciaqbiih52zd6fom6azpioa6a3fanh5kja6hni";

    #[test]
    fn paths_and_links() {
        for p in [CID.to_string(), format!("ipfs://{CID}"), format!("/ipfs/{CID}"), format!("ipfs://{CID}/a/b.png")] {
            assert!(parse_path(&p).is_ok(), "{p}");
        }
        assert!(parse_path("ipfs://nope").is_err());
        let text = format!("see ![fig](ipfs://{CID}) and [again](/ipfs/{CID}/x) and ipfs://junk and https://x/ipfs/QmS4ustL54uo8FzR9455qaxZwuMiUhyvMcX9Ba8nUH4uVv");
        let c = linked_cids(&text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].to_string(), CID);
    }

    #[test]
    fn content_types_never_html() {
        assert_eq!(content_type("page.html", b"<html><script>"), "text/plain; charset=utf-8");
        assert_eq!(content_type("", b"<html><script>alert(1)</script>"), "text/plain; charset=utf-8");
        assert_eq!(content_type("", b"\x89PNG\r\n"), "image/png");
        assert_eq!(content_type("f.svg", b"<svg"), "image/svg+xml");
        assert_eq!(content_type("", &[0, 159, 146, 150]), "application/octet-stream");
    }
}
