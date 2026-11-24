//! Files on IPFS, through a Kubo daemon (the reference IPFS implementation, in Go).
//!
//! IPFS runs as its own server next to the node: `ipfs daemon` on the same
//! machine, or the `kubo` service in `docker-compose.yml`. The node talks to it
//! over Kubo's RPC API (`--ipfs-api`, default `http://127.0.0.1:5001`) and adds
//! what makes files part of Sametra:
//!
//! - every file added through the node is announced as a NIP-94 file metadata
//!   event (kind 1063) signed with the node's identity, so files belong to
//!   people the way posts do, and anyone can list them;
//! - nodes announce their Kubo peer in heartbeats (`["ipfs", peer id, addr…]`)
//!   and connect their Kubos to each other, so files move directly;
//! - files that posts link to (`ipfs://CID`) are fetched into the local Kubo,
//!   which then serves them to the network;
//! - `/ipfs/CID` on the node's HTTP port serves files to browsers (sandboxed) and
//!   raw blocks to IPFS clients (`?format=raw`).
//!
//! If no daemon answers, the node runs without files and keeps checking.

use http_body_util::{BodyExt, BodyStream, Full};
use hyper::body::{Bytes, Incoming};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use crate::files::parse_path;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// Kubo's RPC API, `http://host:port`.
    pub api: String,
    /// `Authorization` header for a Kubo whose RPC requires it (API.Authorizations).
    pub auth: Option<String>,
    /// Fetch at most this many bytes when keeping a file a post links to.
    pub max_file: u64,
    /// Fetch from the network for anyone asking /ipfs/ over HTTP, not only the
    /// node's operator (others get what the local Kubo already has).
    pub open_gateway: bool,
    /// Announce only public addresses (the node has a public address).
    pub public: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config { api: "http://127.0.0.1:5001".into(), auth: None, max_file: 32 << 20, open_gateway: false, public: false }
    }
}

/// What the daemon says about itself.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub peer_id: String,
    pub addrs: Vec<String>,
    pub agent: String,
}

pub struct Ipfs {
    http: Client<HttpConnector, Full<Bytes>>,
    pub cfg: Config,
    /// the daemon's identity, while it answers
    pub identity: Mutex<Option<Identity>>,
    /// Kubo peers learned from Sametra heartbeats
    known: Mutex<HashSet<String>>,
    /// files being fetched for the cache
    fetching: Mutex<HashSet<String>>,
    fetch_slots: tokio::sync::Semaphore,
}

/// Percent-encode a query value.
fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Ipfs {
    pub fn new(cfg: Config) -> Result<Ipfs, String> {
        if !cfg.api.starts_with("http://") {
            return Err(format!("--ipfs-api must be http://host:port (got '{}'); put a TLS proxy in front of a remote Kubo", cfg.api));
        }
        let http = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build_http();
        Ok(Ipfs { http, cfg, identity: Mutex::default(), known: Mutex::default(), fetching: Mutex::default(), fetch_slots: tokio::sync::Semaphore::new(4) })
    }

    /// POST `/api/v0/{cmd}?{query}` to Kubo; the response if it succeeded.
    async fn rpc(&self, cmd: &str, query: &[(&str, &str)], body: Option<(String, Vec<u8>)>, timeout: Duration) -> Result<hyper::Response<Incoming>, String> {
        let q: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect();
        let url = format!("{}/api/v0/{cmd}{}{}", self.cfg.api.trim_end_matches('/'), if q.is_empty() { "" } else { "?" }, q.join("&"));
        let mut req = hyper::Request::post(&url);
        if let Some(a) = &self.cfg.auth {
            req = req.header("Authorization", a);
        }
        let (ct, bytes) = body.unwrap_or_default();
        if !ct.is_empty() {
            req = req.header("Content-Type", ct);
        }
        let req = req.body(Full::new(Bytes::from(bytes))).map_err(err)?;
        let resp = tokio::time::timeout(timeout, self.http.request(req))
            .await
            .map_err(|_| format!("IPFS daemon at {} did not answer {cmd} in time", self.cfg.api))?
            .map_err(|e| format!("no IPFS daemon at {} ({e}); start Kubo with `ipfs daemon`", self.cfg.api))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
        let msg = serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| v["Message"].as_str().map(String::from)).unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
        Err(if msg.is_empty() { format!("IPFS {cmd}: HTTP {status}") } else { msg })
    }

    async fn json(&self, cmd: &str, query: &[(&str, &str)], timeout: Duration) -> Result<serde_json::Value, String> {
        let resp = self.rpc(cmd, query, None, timeout).await?;
        let body = tokio::time::timeout(timeout, resp.into_body().collect()).await.map_err(|_| format!("IPFS {cmd} timed out"))?.map_err(err)?.to_bytes();
        serde_json::from_slice(&body).map_err(|e| format!("IPFS {cmd}: unexpected reply ({e})"))
    }

    /// Ask the daemon who it is; remembers the answer (None while it is down).
    pub async fn refresh(&self) -> Option<Identity> {
        let id = match self.json("id", &[], Duration::from_secs(5)).await {
            Ok(v) => {
                let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
                let mut addrs: Vec<String> = v["Addresses"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).map(String::from).collect();
                if self.cfg.public {
                    addrs.retain(|a| !(a.contains("/127.0.0.1/") || a.contains("/::1/") || a.contains("/10.") || a.contains("/192.168.") || a.contains("/172.")));
                }
                Some(Identity { peer_id: s("ID"), addrs, agent: s("AgentVersion") })
            }
            Err(e) => {
                tracing::debug!(error = %e, "IPFS daemon not answering");
                None
            }
        };
        let was = std::mem::replace(&mut *self.identity.lock().unwrap(), id.clone());
        match (&was, &id) {
            (None, Some(i)) => tracing::info!(peer_id = %i.peer_id, agent = %i.agent, api = %self.cfg.api, "IPFS daemon connected"),
            (Some(_), None) => tracing::warn!(api = %self.cfg.api, "IPFS daemon stopped answering; files are unavailable until it is back"),
            _ => {}
        }
        id
    }

    /// Our heartbeat tag: `["ipfs", peer id, addr…]`, while the daemon is up.
    pub fn heartbeat_tag(&self) -> Option<Vec<String>> {
        let id = self.identity.lock().unwrap().clone()?;
        let mut tag = vec!["ipfs".to_string(), id.peer_id];
        tag.extend(id.addrs.into_iter().take(6));
        Some(tag)
    }

    pub fn peer_id(&self) -> Option<String> {
        self.identity.lock().unwrap().as_ref().map(|i| i.peer_id.clone())
    }

    pub async fn connected(&self) -> usize {
        self.json("swarm/peers", &[], Duration::from_secs(5)).await.ok().and_then(|v| v["Peers"].as_array().map(|p| p.len())).unwrap_or(0)
    }

    /// Connect our daemon to a Sametra node's, from its heartbeat tag.
    pub async fn learn(&self, tag: &[String]) {
        let Some(peer) = tag.get(1).filter(|p| p.len() > 20 && p.chars().all(|c| c.is_ascii_alphanumeric())) else { return };
        if self.peer_id().as_deref() == Some(peer.as_str()) {
            return;
        }
        {
            let mut k = self.known.lock().unwrap();
            if k.len() > 5000 || !k.insert(peer.clone()) {
                return;
            }
        }
        for a in tag[2..].iter().take(6).filter(|a| a.starts_with('/')) {
            let addr = if a.contains("/p2p/") { a.clone() } else { format!("{a}/p2p/{peer}") };
            match self.json("swarm/connect", &[("arg", &addr)], Duration::from_secs(15)).await {
                Ok(_) => {
                    tracing::debug!(%peer, %addr, "connected to a Sametra node's IPFS daemon");
                    return;
                }
                Err(e) => tracing::debug!(%peer, %addr, error = %e, "IPFS swarm connect failed"),
            }
        }
        // try again on a later heartbeat
        self.known.lock().unwrap().remove(peer);
    }

    /// Add bytes as a file (CIDv1, raw leaves); its CID.
    pub async fn add(&self, data: Vec<u8>, name: &str, pin: bool) -> Result<String, String> {
        let boundary = format!("sametra{:016x}", rand_u64());
        let fname: String = name.chars().filter(|c| !matches!(c, '"' | '\r' | '\n' | '/' | '\\')).collect();
        let fname = if fname.is_empty() { "file".to_string() } else { fname };
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{fname}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let ct = format!("multipart/form-data; boundary={boundary}");
        let resp = self.rpc("add", &[("cid-version", "1"), ("pin", if pin { "true" } else { "false" }), ("quieter", "true")], Some((ct, body)), Duration::from_secs(120)).await?;
        let out = resp.into_body().collect().await.map_err(err)?.to_bytes();
        // newline-delimited JSON; the last object with a Hash is the file
        String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["Hash"].as_str().map(String::from))
            .next_back()
            .ok_or_else(|| "IPFS add: no CID in the reply".to_string())
    }

    /// A file's bytes, at most `max` of them (true if cut short). `local`: only
    /// what the daemon already has, never the network.
    pub async fn cat(&self, path: &str, max: usize, local: bool, timeout: Duration) -> Result<(Vec<u8>, bool), String> {
        let (_, p) = parse_path(path)?;
        let mut s = Box::pin(self.cat_stream(&p, local, timeout).await?);
        let mut out = vec![];
        let read = async {
            while let Some(chunk) = futures_util::StreamExt::next(&mut s).await {
                let chunk = chunk?;
                if out.len() + chunk.len() > max {
                    out.extend_from_slice(&chunk[..max - out.len()]);
                    return Ok(true);
                }
                out.extend_from_slice(&chunk);
            }
            Ok::<_, String>(false)
        };
        let cut = tokio::time::timeout(timeout, read).await.map_err(|_| "IPFS: reading the file timed out".to_string())??;
        Ok((out, cut))
    }

    /// A file as a stream of chunks (for HTTP). Kubo reports errors that happen
    /// mid-stream in an `X-Stream-Error` trailer; they end the stream with an error.
    pub async fn cat_stream(&self, path: &str, local: bool, timeout: Duration) -> Result<impl futures_util::Stream<Item = Result<Bytes, String>> + Send + use<>, String> {
        let t = format!("{}s", timeout.as_secs().max(1));
        let resp = self.rpc("cat", &[("arg", path), ("offline", if local { "true" } else { "false" }), ("timeout", &t)], None, timeout).await?;
        Ok(futures_util::StreamExt::filter_map(BodyStream::new(resp.into_body()), |f| async move {
            match f {
                Err(e) => Some(Err(err(e))),
                Ok(f) => match f.into_data() {
                    Ok(d) => Some(Ok(d)),
                    Err(f) => f.trailers_ref().and_then(|t| t.get("X-Stream-Error")).map(|e| Err(String::from_utf8_lossy(e.as_bytes()).to_string())),
                },
            }
        }))
    }

    /// One raw block (trustless gateway).
    pub async fn block(&self, cid: &str, local: bool) -> Result<Vec<u8>, String> {
        let resp = self.rpc("block/get", &[("arg", cid), ("offline", if local { "true" } else { "false" }), ("timeout", "60s")], None, Duration::from_secs(65)).await?;
        Ok(resp.into_body().collect().await.map_err(err)?.to_bytes().to_vec())
    }

    pub async fn pin(&self, path: &str, pin: bool) -> Result<String, String> {
        let (cid, _) = parse_path(path)?;
        let cmd = if pin { "pin/add" } else { "pin/rm" };
        self.json(cmd, &[("arg", &cid)], Duration::from_secs(300)).await?;
        Ok(cid)
    }

    pub async fn is_pinned(&self, cid: &str) -> bool {
        self.json("pin/ls", &[("arg", cid), ("type", "recursive"), ("offline", "true")], Duration::from_secs(10)).await.is_ok()
    }

    /// The size of a file (or directory) in bytes, from its root block.
    pub async fn size(&self, cid: &str) -> Result<u64, String> {
        let v = self.json("files/stat", &[("arg", &format!("/ipfs/{cid}")), ("timeout", "60s")], Duration::from_secs(65)).await?;
        v["CumulativeSize"].as_u64().ok_or_else(|| "IPFS files/stat: no size".into())
    }

    /// Fetch a file a post links to into the local daemon (unpinned: Kubo's GC may
    /// collect it), so it serves it too. Skips files over `max_file`.
    pub async fn cache(&self, cid: String) {
        if self.identity.lock().unwrap().is_none() || !self.fetching.lock().unwrap().insert(cid.clone()) {
            return;
        }
        if let Ok(_slot) = self.fetch_slots.acquire().await {
            match self.size(&cid).await {
                Ok(n) if n > self.cfg.max_file => tracing::debug!(%cid, bytes = n, max = self.cfg.max_file, "linked file is too large to keep"),
                Ok(n) => match self.rpc("refs", &[("arg", &cid), ("recursive", "true"), ("unique", "true"), ("timeout", "120s")], None, Duration::from_secs(125)).await {
                    Ok(resp) => {
                        let _ = resp.into_body().collect().await;
                        tracing::debug!(%cid, bytes = n, "kept a file a post links to");
                    }
                    Err(e) => tracing::debug!(%cid, error = %e, "could not fetch a linked file"),
                },
                Err(e) => tracing::debug!(%cid, error = %e, "could not fetch a linked file"),
            }
        }
        self.fetching.lock().unwrap().remove(&cid);
    }
}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    let _ = getrandom::getrandom(&mut b);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        assert_eq!(enc("/ipfs/a b&c"), "/ipfs/a%20b%26c");
        assert!(Ipfs::new(Config { api: "https://x".into(), ..Default::default() }).is_err());
    }
}
