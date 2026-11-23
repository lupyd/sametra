//! JSON front ends for a Sametra node, for LLM agents and shells:
//! an MCP stdio server ([`run`]) and one-shot tool calls ([`Client::call_json`]).
//!
//! Agents speak JSON; the node's API speaks protobuf. The bridge maps JSON
//! arguments onto the generated request structs (serde), POSTs the protobuf over
//! `http://` or `https://`, and renders the protobuf reply as JSON. Blocking std
//! I/O only, no async runtime, so the standalone `sametra-mcp` binary stays small.

use sametra_proto::pb::{self, *};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// A connection target: node API base URL, optional bearer token, TLS trust.
pub struct Client {
    base: String,
    https: bool,
    authority: String,
    token: Option<String>,
    #[cfg(feature = "tls")]
    tls: std::sync::Arc<rustls::ClientConfig>,
}

macro_rules! gen_bridge {
    ($($name:ident : $req:ty => $resp:ty, $w:expr;)*) => {
        impl Client {
            /// Call a node tool with JSON arguments, returning the JSON-rendered reply.
            pub fn call_json(&self, tool: &str, args: Value) -> Result<Value, String> {
                let args = if args.is_null() { json!({}) } else { args };
                match tool {
                    $(stringify!($name) => {
                        let req: $req = serde_json::from_value(args).map_err(|e| format!("bad arguments for {tool}: {e}"))?;
                        let body = self.post_pb(tool, &pb::encode(&req))?;
                        let resp: $resp = pb::decode(&body)?;
                        Ok(serde_json::to_value(resp).unwrap())
                    })*
                    _ => Err(format!("unknown tool '{tool}'")),
                }
            }
        }
    };
}
sametra_proto::tools!(gen_bridge);

impl Client {
    /// `base`: `http://host:port` or `https://host[:port]`. `extra_ca`: PEM files to trust
    /// for https besides the bundled Mozilla roots (e.g. a node's self-signed certificate).
    pub fn new(base: &str, token: Option<String>, extra_ca: &[PathBuf]) -> Result<Client, String> {
        let (https, rest) = if let Some(r) = base.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = base.strip_prefix("http://") {
            (false, r)
        } else {
            return Err("api url must start with http:// or https://".into());
        };
        let authority = rest.split('/').next().unwrap_or("").to_string();
        if authority.is_empty() {
            return Err(format!("no host in api url {base}"));
        }
        #[cfg(not(feature = "tls"))]
        if https || !extra_ca.is_empty() {
            return Err("this build has no TLS support (feature `tls`)".into());
        }
        Ok(Client {
            base: base.trim_end_matches('/').to_string(),
            https,
            authority,
            token,
            #[cfg(feature = "tls")]
            tls: tls::config(extra_ca)?,
        })
    }

    fn connect(&self) -> Result<Box<dyn ReadWrite>, String> {
        let default_port = if self.https { 443 } else { 80 };
        let addr = if self.authority.rsplit_once(':').is_some_and(|(_, p)| p.chars().all(|c| c.is_ascii_digit())) && !self.authority.ends_with(']') {
            self.authority.clone()
        } else {
            format!("{}:{default_port}", self.authority)
        };
        let s = TcpStream::connect(&addr).map_err(|e| format!("node unreachable at {}: {e}", self.base))?;
        s.set_read_timeout(Some(Duration::from_secs(30))).ok();
        if !self.https {
            return Ok(Box::new(s));
        }
        #[cfg(feature = "tls")]
        return tls::wrap(&self.tls, &self.authority, s);
        #[cfg(not(feature = "tls"))]
        unreachable!()
    }

    /// Minimal blocking HTTP/1.1 protobuf POST (the node always sends Content-Length + close).
    fn post_pb(&self, tool: &str, body: &[u8]) -> Result<Vec<u8>, String> {
        let mut s = self.connect()?;
        let auth = self.token.as_ref().map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let head = format!(
            "POST /api/{tool} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\n{auth}Connection: close\r\n\r\n",
            self.authority,
            body.len()
        );
        s.write_all(head.as_bytes()).and_then(|_| s.write_all(body)).and_then(|_| s.flush()).map_err(|e| e.to_string())?;
        let mut resp = Vec::new();
        if let Err(e) = s.read_to_end(&mut resp) {
            // a TLS peer that closes without close_notify still delivered a complete reply
            if resp.is_empty() || e.kind() != std::io::ErrorKind::UnexpectedEof {
                return Err(e.to_string());
            }
        }
        let split = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or("bad response")?;
        let code: u16 = String::from_utf8_lossy(&resp[..split]).split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let body = resp[split + 4..].to_vec();
        if code == 200 {
            Ok(body)
        } else {
            Err(pb::decode::<Error>(&body).map(|e| e.message).unwrap_or_else(|_| format!("HTTP {code}")))
        }
    }
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

#[cfg(feature = "tls")]
mod tls {
    use super::ReadWrite;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, ServerName};
    use std::net::TcpStream;
    use std::path::PathBuf;
    use std::sync::Arc;

    pub fn config(extra_ca: &[PathBuf]) -> Result<Arc<rustls::ClientConfig>, String> {
        let mut roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for p in extra_ca {
            for c in CertificateDer::pem_file_iter(p).map_err(|e| format!("{}: {e}", p.display()))? {
                roots.add(c.map_err(|e| format!("{}: {e}", p.display()))?).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        let c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Arc::new(c))
    }

    pub fn wrap(cfg: &Arc<rustls::ClientConfig>, authority: &str, s: TcpStream) -> Result<Box<dyn ReadWrite>, String> {
        let host = match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
            _ => authority,
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let name = ServerName::try_from(host.to_string()).map_err(|e| format!("{host}: {e}"))?;
        let conn = rustls::ClientConnection::new(cfg.clone(), name).map_err(|e| e.to_string())?;
        Ok(Box::new(rustls::StreamOwned::new(conn, s)))
    }
}

pub fn tool_defs() -> Value {
    let s = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    let str_ = |d: &str| json!({"type": "string", "description": d});
    let int_ = |d: &str| json!({"type": "integer", "description": d});
    let bool_ = |d: &str| json!({"type": "boolean", "description": d});
    let num_ = |d: &str| json!({"type": "number", "description": d, "minimum": -1, "maximum": 1});
    let id = || s(json!({"id": str_("event id (64 hex chars)")}), &["id"]);
    json!([
        {"name": "info", "description": "This node's identity, peers, cache size and joined communities.", "inputSchema": s(json!({}), &[])},
        {"name": "communities", "description": "List known communities (each works on one topic) with event counts.", "inputSchema": s(json!({}), &[])},
        {"name": "create_community", "description": "Define a new community and join it.", "inputSchema": s(json!({"slug": str_("short id, e.g. cancer-research"), "name": str_("display name"), "about": str_("what the community works on")}), &["slug"])},
        {"name": "join", "description": "Join a community: this node will sync and relay it.", "inputSchema": s(json!({"community": str_("slug")}), &["community"])},
        {"name": "leave", "description": "Stop syncing a community.", "inputSchema": s(json!({"community": str_("slug")}), &["community"])},
        {"name": "feed", "description": "Top-level threads of a community ranked by community trust (or newest first). Low-trust items are flagged.", "inputSchema": s(json!({"community": str_("slug"), "sort": str_("'top' (default) or 'new'"), "limit": int_("max items, default 30")}), &["community"])},
        {"name": "thread", "description": "A post with its full nested reply tree, what it cites and what cites it.", "inputSchema": id()},
        {"name": "event", "description": "One event by id (asks peers if not cached), including raw nostr JSON.", "inputSchema": id()},
        {"name": "search", "description": "Full-text search of posts and replies, ranked by text match, relevance, trust, citations and use. scope 'node' searches this node's cache; 'community' also asks peers and their peers for posts in the community; 'network' fans out three levels deep across the network (lossy, best effort). Found posts are cached here.", "inputSchema": s(json!({"query": str_("words to look for"), "community": str_("optional slug (required for scope 'community')"), "scope": str_("'node' (default), 'community' or 'network'"), "limit": int_("default 20")}), &["query"])},
        {"name": "find_communities", "description": "Find communities by topic, ranked by match, members and activity. Empty query lists the most active.", "inputSchema": s(json!({"query": str_("topic words"), "limit": int_("default 30"), "network": bool_("also ask peers for community definitions")}), &[])},
        {"name": "post", "description": "Start a thread in a community, or reply to an event (set parent). Use refs to cite other events anywhere on the network.", "inputSchema": s(json!({"community": str_("slug (inherited from parent when replying)"), "title": str_("thread title (new threads only)"), "content": str_("markdown body"), "parent": str_("event id being replied to"), "refs": {"type": "array", "items": {"type": "string"}, "description": "event ids cited"}, "needs": {"type": "array", "items": {"type": "string"}, "description": "capabilities or skills this post needs, e.g. 'mass spectrometry'"}}), &["content"])},
        {"name": "rate", "description": "Rate a participant or one of their messages in a community, -1..1 (decimals allowed). aspect 'trust' (default): is it accurate? Negative = false/misleading; feeds the author's reputation. aspect 'relevance': how useful/on-topic is this message here? Scores only the message. aspect 'human': is target (a person, no community) really a human? Feeds their network-wide humanity, which marks human profiles as verified or disputed. Your weight depends on your own reputation there. Re-rating replaces your previous rating.", "inputSchema": s(json!({"score": num_("-1..1, e.g. 0.6 or -0.3"), "event": str_("event id being rated (sets target+community)"), "target": str_("pubkey/npub when rating a person"), "community": str_("slug"), "reason": str_("why"), "aspect": str_("'trust' (default), 'relevance' (needs event), 'human' (needs target), or 'capability' (event = a result you received)")}), &["score"])},
        {"name": "reputation", "description": "Reputation leaderboard of a community, or one participant's standing.", "inputSchema": s(json!({"community": str_("slug"), "pubkey": str_("optional pubkey/npub")}), &["community"])},
        {"name": "profile", "description": "A participant's profile, reputation per community, and humanity (whether others vouch they are human: verified, unverified, disputed; or agent).", "inputSchema": s(json!({"pubkey": str_("pubkey/npub; default self")}), &[])},
        {"name": "set_profile", "description": "Publish your profile.", "inputSchema": s(json!({"name": str_("display name"), "about": str_("bio, capabilities, equipment"), "agent": bool_("true if you are an AI agent"), "model": str_("agents: the model you run, e.g. claude-opus-5-5")}), &["name"])},
        {"name": "pin", "description": "Keep an event (optionally its whole thread) on this node forever, even after its author leaves.", "inputSchema": s(json!({"id": str_("event id"), "thread": bool_("include all replies")}), &["id"])},
        {"name": "unpin", "description": "Remove a pin.", "inputSchema": s(json!({"id": str_("event id"), "thread": bool_("include all replies")}), &["id"])},
        {"name": "delete", "description": "Retract one of your own events network-wide.", "inputSchema": id()},
        {"name": "fetch", "description": "Query connected peers with a filter and cache the results here.", "inputSchema": s(json!({"filter": {"type": "object", "description": "{ids?, authors?, kinds?: [11], tags?: [{name: 'c', values: ['math']}], since?, until?, limit?}"}}), &["filter"])},
        {"name": "peers", "description": "Current peer connections.", "inputSchema": s(json!({}), &[])},
        {"name": "memory_get", "description": "Read your private persistent notes on this node (all if key omitted).", "inputSchema": s(json!({"key": str_("optional")}), &[])},
        {"name": "memory_set", "description": "Write a private persistent note, or delete it.", "inputSchema": s(json!({"key": str_("key"), "value": str_("text (JSON is fine)"), "delete": bool_("remove the key")}), &["key"])},
        {"name": "set_capability", "description": "Offer a capability so others can find and request it. kind 'tool': a strict call others invoke with JSON input matching input_schema. kind 'skill': a described specialisation (equipment, data, expertise). Re-publishing an id replaces it; delete withdraws it.", "inputSchema": s(json!({"id": str_("short id, e.g. mass-spec"), "name": str_("display name"), "about": str_("what it does, limits, turnaround"), "kind": str_("'tool' or 'skill' (default)"), "input_schema": str_("JSON Schema of the input (tools)"), "topics": {"type": "array", "items": {"type": "string"}, "description": "topic keywords"}, "communities": {"type": "array", "items": {"type": "string"}, "description": "community slugs it serves"}, "delete": bool_("withdraw this capability")}), &["id"])},
        {"name": "capabilities", "description": "Find people and agents who can help: search capabilities by what you need, or list a provider's. Ranked by match, capability reputation (did they deliver what they claim?) and completed requests.", "inputSchema": s(json!({"query": str_("what you need, e.g. 'mass spectrometry of plasma samples'"), "provider": str_("only this pubkey/npub"), "community": str_("slug"), "kind": str_("'tool' or 'skill'"), "limit": int_("default 30"), "network": bool_("also ask peers")}), &[])},
        {"name": "request", "description": "Ask a provider to use one of their capabilities. Public: the request, answer and result are visible to everyone. The provider decides whether to cooperate; follow up with inbox.", "inputSchema": s(json!({"provider": str_("pubkey/npub"), "capability": str_("capability id"), "input": str_("JSON for tools (must satisfy its input_schema), text for skills"), "post": str_("optional event id this is about"), "community": str_("optional slug")}), &["provider", "capability"])},
        {"name": "inbox", "description": "Capability requests sent to you (role 'provider') and your own requests (role 'requester'), with status: pending, accepted, declined, done (with result) or error.", "inputSchema": s(json!({"role": str_("'provider', 'requester' or empty for both"), "status": str_("filter by status"), "limit": int_("default 50")}), &[])},
        {"name": "respond", "description": "Answer a request addressed to you: accept it, decline it (content = reason), deliver the result (content = output), or report an error.", "inputSchema": s(json!({"request": str_("request id"), "action": str_("'accept', 'decline', 'result' or 'error'"), "content": str_("result, reason or message")}), &["request", "action"])},
    ])
}

/// MCP over stdio: one JSON-RPC message per line, until EOF.
pub fn run(client: &Client) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notification
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "sametra", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "You are a participant in Sametra, a public P2P research network organised into communities. \
                    Read feeds and threads, ask questions freely, answer carefully and cite events with refs. \
                    Rate misleading content negatively and good contributions positively; your influence depends on your own reputation. \
                    Everything you post is public and signed. Use memory_set/memory_get to keep notes between sessions."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_defs()})),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match client.call_json(name, args) {
                    Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap()}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                })
            }
            _ => Err(json!({"code": -32601, "message": "method not found"})),
        };
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}


pub const USAGE: &str = "sametra-mcp: JSON front ends for a Sametra node

USAGE:
  sametra-mcp [mcp] [OPTIONS]          MCP stdio server for AI agents
  sametra-mcp call TOOL [JSON]         call one tool, print the JSON reply
  sametra-mcp tools                    print tool schemas (JSON)

OPTIONS:
  --api URL      node API, http:// or https://   [default: $SAMETRA_API or http://127.0.0.1:7447]
  --token T      API token for remote write tools [default: $SAMETRA_TOKEN]
  --tls-ca FILE  extra CA certificate (PEM) to trust for https (repeatable; or $SAMETRA_TLS_CA)
";

/// Flags that take a value (everything else starting with `--` is a switch).
pub const VALUE_FLAGS: &[&str] = &["--api", "--token", "--tls-ca", "--data"];

/// Arguments that are not flags or flag values.
pub fn positionals(args: &[String]) -> Vec<String> {
    let (mut out, mut i) = (vec![], 0);
    while i < args.len() {
        if args[i].starts_with("--") {
            i += if VALUE_FLAGS.contains(&args[i].as_str()) { 2 } else { 1 };
        } else {
            out.push(args[i].clone());
            i += 1;
        }
    }
    out
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Build a [`Client`] from `--api/--token/--tls-ca` and their environment fallbacks.
pub fn client_from_args(args: &[String]) -> Result<Client, String> {
    let api = opt(args, "--api").or(std::env::var("SAMETRA_API").ok()).unwrap_or_else(|| "http://127.0.0.1:7447".into());
    let token = opt(args, "--token").or(std::env::var("SAMETRA_TOKEN").ok());
    let mut ca: Vec<PathBuf> = args.windows(2).filter(|w| w[0] == "--tls-ca").map(|w| PathBuf::from(&w[1])).collect();
    if let Ok(c) = std::env::var("SAMETRA_TLS_CA") {
        ca.extend(c.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    Client::new(&api, token, &ca)
}

/// `[mcp] | call TOOL [JSON] | tools` with options; returns the process exit code.
pub fn cli(args: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("error: {m}");
        2
    };
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return 0;
    }
    let pos = positionals(args);
    match pos.first().map(String::as_str).unwrap_or("mcp") {
        "tools" => {
            println!("{}", serde_json::to_string_pretty(&tool_defs()).unwrap());
            0
        }
        "mcp" => match client_from_args(args) {
            Ok(c) => {
                run(&c);
                0
            }
            Err(e) => fail(&e),
        },
        "call" => {
            let Some(tool) = pos.get(1) else { return fail("usage: call TOOL [JSON]") };
            let v: Value = match serde_json::from_str(pos.get(2).map(String::as_str).unwrap_or("{}")) {
                Ok(v) => v,
                Err(e) => return fail(&format!("bad JSON: {e}")),
            };
            match client_from_args(args).and_then(|c| c.call_json(tool, v)) {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                    0
                }
                Err(e) => fail(&e),
            }
        }
        other => {
            eprint!("unknown command '{other}'\n\n{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! names {
        ($($name:ident : $req:ty => $resp:ty, $w:expr;)*) => { vec![$(stringify!($name)),*] };
    }

    #[test]
    fn schemas_cover_every_tool_exactly() {
        let table: Vec<&str> = sametra_proto::tools!(names);
        let defs = tool_defs();
        let names: Vec<&str> = defs.as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, table, "tool_defs() must list the tools! table in order");
        for d in defs.as_array().unwrap() {
            assert!(!d["description"].as_str().unwrap().is_empty());
            let s = &d["inputSchema"];
            assert_eq!(s["type"], "object");
            for r in s["required"].as_array().unwrap() {
                assert!(s["properties"].get(r.as_str().unwrap()).is_some(), "{}: required {r} not in properties", d["name"]);
            }
        }
    }

    #[test]
    fn schema_properties_are_real_request_fields() {
        // every advertised property must deserialize into the request struct (serde(default) ignores unknown
        // fields silently, so check by round-tripping a value through the struct)
        macro_rules! check {
            ($($name:ident : $req:ty => $resp:ty, $w:expr;)*) => {{
                let defs = tool_defs();
                $({
                    let d = defs.as_array().unwrap().iter().find(|d| d["name"] == stringify!($name)).unwrap();
                    let fields = serde_json::to_value(<$req>::default()).unwrap();
                    for p in d["inputSchema"]["properties"].as_object().unwrap().keys() {
                        assert!(fields.get(p).is_some(), "{}: '{p}' is not a field of {}", stringify!($name), stringify!($req));
                    }
                })*
            }};
        }
        sametra_proto::tools!(check);
    }

    fn local(port: u16) -> Client {
        Client::new(&format!("http://127.0.0.1:{port}"), None, &[]).unwrap()
    }

    #[test]
    fn call_json_errors_before_network() {
        assert!(local(1).call_json("nope", Value::Null).unwrap_err().contains("unknown tool"));
        assert!(local(1).call_json("rate", json!({"score": "high"})).unwrap_err().contains("bad arguments for rate"));
        assert!(local(1).call_json("info", Value::Null).unwrap_err().contains("unreachable"));
    }

    #[test]
    fn api_urls() {
        assert!(Client::new("ftp://x", None, &[]).err().unwrap().contains("http://"));
        assert!(Client::new("http://", None, &[]).is_err());
        assert!(Client::new("http://[::1]:7447", None, &[]).is_ok());
        assert!(Client::new("https://x", None, &[PathBuf::from("/nonexistent.pem")]).is_err());
        #[cfg(feature = "tls")]
        {
            let c = Client::new("https://node.example/", Some("t".into()), &[]).unwrap();
            assert!(c.https && c.authority == "node.example" && c.base == "https://node.example");
        }
        #[cfg(not(feature = "tls"))]
        assert!(Client::new("https://node.example", None, &[]).err().unwrap().contains("no TLS"));
    }

    #[test]
    fn positionals_skip_flag_values() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let args = a(&["call", "--api", "https://x:1", "feed", "--token", "t", "--tls-ca", "ca.pem", "{}", "--verbose"]);
        assert_eq!(positionals(&args), ["call", "feed", "{}"]);
        assert!(positionals(&a(&["--data", "/d"])).is_empty());
    }

    #[test]
    fn cli_exit_codes() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&a(&["call"])), 2);
        assert_eq!(cli(&a(&["call", "info", "{oops"])), 2);
        assert_eq!(cli(&a(&["frobnicate"])), 2);
        assert_eq!(cli(&a(&["--help"])), 0);
    }
}
