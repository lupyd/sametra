//! A stand-in for Kubo's RPC API (`/api/v0/...`), enough for the node's use of it:
//! id, add, cat, block/get, pin/add|rm|ls, swarm/connect|peers, files/stat, refs.
//!
//! Daemons made from one [`Swarm`] can fetch each other's blocks, but only after
//! `swarm/connect` to that peer, like Bitswap needs a connection. `offline=true`
//! answers only from what the daemon holds, as Kubo does. Files are single raw
//! blocks with CIDv1 = base32(0x01 0x55 0x12 0x20 || sha256), as Kubo makes for
//! small files with `cid-version=1`.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

type Blocks = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Daemons that can reach each other.
#[derive(Clone, Default)]
pub struct Swarm(Arc<Mutex<HashMap<String, Blocks>>>);

pub struct Kubo {
    pub port: u16,
    pub peer_id: String,
    pub blocks: Blocks,
    pub pins: Arc<Mutex<HashSet<String>>>,
    pub connected: Arc<Mutex<Vec<String>>>,
    /// every RPC command called, in order
    pub calls: Arc<Mutex<Vec<String>>>,
}

pub fn cid_of(data: &[u8]) -> String {
    let mut bytes = vec![0x01, 0x55, 0x12, 0x20];
    bytes.extend_from_slice(&Sha256::digest(data));
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut out, mut buf, mut bits) = (String::from("b"), 0u32, 0u32);
    for b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(A[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn query(target: &str) -> HashMap<String, String> {
    let q = target.split_once('?').map(|x| x.1).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            let mut out = vec![];
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            (k.to_string(), String::from_utf8(out).unwrap())
        })
        .collect()
}

impl Kubo {
    pub fn start(swarm: &Swarm) -> Kubo {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let peer_id = format!("12D3KooWFake{port:05}{}", "x".repeat(36));
        let k = Kubo { port, peer_id: peer_id.clone(), blocks: Blocks::default(), pins: Default::default(), connected: Default::default(), calls: Default::default() };
        swarm.0.lock().unwrap().insert(peer_id.clone(), k.blocks.clone());
        let (swarm, blocks, pins, connected, calls) = (swarm.clone(), k.blocks.clone(), k.pins.clone(), k.connected.clone(), k.calls.clone());
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (swarm, blocks, pins, connected, calls, peer_id) = (swarm.clone(), blocks.clone(), pins.clone(), connected.clone(), calls.clone(), peer_id.clone());
                std::thread::spawn(move || serve(s, port, &peer_id, &swarm, &blocks, &pins, &connected, &calls));
            }
        });
        k
    }

    pub fn api(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn has(&self, cid: &str) -> bool {
        self.blocks.lock().unwrap().contains_key(cid)
    }
}

fn reply(s: &mut TcpStream, code: u16, ct: &str, body: &[u8]) {
    let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(body);
}

fn error(s: &mut TcpStream, msg: &str) {
    reply(s, 500, "application/json", serde_json::json!({"Message": msg, "Code": 0, "Type": "error"}).to_string().as_bytes());
}

#[allow(clippy::too_many_arguments)]
fn serve(mut s: TcpStream, port: u16, me: &str, swarm: &Swarm, blocks: &Blocks, pins: &Mutex<HashSet<String>>, connected: &Mutex<Vec<String>>, calls: &Mutex<Vec<String>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut first = String::new();
    if r.read_line(&mut first).is_err() || first.is_empty() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    let target = first.split(' ').nth(1).unwrap_or("").to_string();
    let cmd = target.split('?').next().unwrap().trim_start_matches("/api/v0/").to_string();
    calls.lock().unwrap().push(cmd.clone());
    let q = query(&target);
    let arg = q.get("arg").cloned().unwrap_or_default();
    let cid = arg.trim_start_matches("/ipfs/").split('/').next().unwrap_or("").to_string();
    let offline = q.get("offline").is_some_and(|v| v == "true");
    // find a block: here, or (online) from a connected peer, keeping a copy
    let fetch = |cid: &str| -> Option<Vec<u8>> {
        if let Some(b) = blocks.lock().unwrap().get(cid) {
            return Some(b.clone());
        }
        if offline {
            return None;
        }
        let peers = connected.lock().unwrap().clone();
        let sw = swarm.0.lock().unwrap();
        let found = peers.iter().filter_map(|p| sw.get(p)).find_map(|b| b.lock().unwrap().get(cid).cloned())?;
        blocks.lock().unwrap().insert(cid.to_string(), found.clone());
        Some(found)
    };
    let missing = |s: &mut TcpStream| error(s, if offline { "block was not found locally (offline): ipld: could not find node" } else { "context deadline exceeded" });
    match cmd.as_str() {
        "id" => reply(&mut s, 200, "application/json", serde_json::json!({"ID": me, "Addresses": [format!("/ip4/127.0.0.1/tcp/{port}/p2p/{me}")], "AgentVersion": "kubo/0.43.1-fake"}).to_string().as_bytes()),
        "add" => {
            // multipart: the file is between the part's blank line and the closing boundary
            let text = &body;
            let start = text.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let end = text.windows(4).rposition(|w| w == b"\r\n--").unwrap();
            let data = text[start..end].to_vec();
            let cid = cid_of(&data);
            let size = data.len();
            blocks.lock().unwrap().insert(cid.clone(), data);
            if q.get("pin").is_none_or(|p| p == "true") {
                pins.lock().unwrap().insert(cid.clone());
            }
            reply(&mut s, 200, "application/json", format!("{}\n", serde_json::json!({"Name": "f", "Hash": cid, "Size": size.to_string()})).as_bytes());
        }
        "cat" | "block/get" => match fetch(&cid) {
            Some(b) => reply(&mut s, 200, "text/plain", &b),
            None => missing(&mut s),
        },
        "files/stat" => match fetch(&cid) {
            Some(b) => reply(&mut s, 200, "application/json", serde_json::json!({"Hash": cid, "CumulativeSize": b.len(), "Type": "file"}).to_string().as_bytes()),
            None => missing(&mut s),
        },
        "refs" => match fetch(&cid) {
            Some(_) => reply(&mut s, 200, "application/json", b""),
            None => missing(&mut s),
        },
        "pin/add" => match fetch(&cid) {
            Some(_) => {
                pins.lock().unwrap().insert(cid.clone());
                reply(&mut s, 200, "application/json", serde_json::json!({"Pins": [cid]}).to_string().as_bytes())
            }
            None => missing(&mut s),
        },
        "pin/rm" => {
            if pins.lock().unwrap().remove(&cid) {
                reply(&mut s, 200, "application/json", serde_json::json!({"Pins": [cid]}).to_string().as_bytes())
            } else {
                error(&mut s, "not pinned or pinned indirectly")
            }
        }
        "pin/ls" => {
            if pins.lock().unwrap().contains(&cid) {
                reply(&mut s, 200, "application/json", serde_json::json!({"Keys": {cid: {"Type": "recursive"}}}).to_string().as_bytes())
            } else {
                error(&mut s, &format!("path '{cid}' is not pinned"))
            }
        }
        "swarm/connect" => {
            let peer = arg.rsplit("/p2p/").next().unwrap_or("").to_string();
            if swarm.0.lock().unwrap().contains_key(&peer) {
                connected.lock().unwrap().push(peer.clone());
                reply(&mut s, 200, "application/json", serde_json::json!({"Strings": [format!("connect {peer} success")]}).to_string().as_bytes())
            } else {
                error(&mut s, "failure: no route to peer")
            }
        }
        "swarm/peers" => {
            let peers: Vec<_> = connected.lock().unwrap().iter().map(|p| serde_json::json!({"Peer": p})).collect();
            reply(&mut s, 200, "application/json", serde_json::json!({"Peers": peers}).to_string().as_bytes())
        }
        _ => error(&mut s, &format!("unknown command {cmd}")),
    }
}
