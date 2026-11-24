//! The real binary: CLI subcommands, the MCP stdio server, restart persistence
//! and `--peer` bootstrapping between separate processes.
mod common;
use common::tempdir;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sametra");

fn cmd(data: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("SAMETRA_KEY").env_remove("SAMETRA_API").env_remove("SAMETRA_TOKEN").env("SAMETRA_DATA", data);
    c
}
fn out(o: Output) -> (i32, String, String) {
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Proc {
    /// Graceful stop (SIGINT, like Ctrl-C) so the node flushes its log.
    fn interrupt(mut self) {
        let _ = Command::new("kill").args(["-INT", &self.0.id().to_string()]).status();
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if self.0.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("node did not stop on SIGINT");
    }
}

/// Start a node and wait until it listens. `port` 0 lets the OS pick a free port, read
/// back from the node's start-up line (no race with other tests).
fn run_node(data: &Path, port: u16, extra: &[&str]) -> (Proc, u16) {
    let mut p = cmd(data)
        .args(["run", "--listen", &format!("127.0.0.1:{port}")])
        .args(extra)
        // IPFS only where a test is about it (it would join the public network)
        .args(if extra.iter().any(|a| a.starts_with("--ipfs")) { &[][..] } else { &["--no-ipfs"][..] })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let err = p.stderr.take().unwrap();
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            if !sent && let Some(addr) = line.trim().strip_prefix("listening ") {
                let port: u16 = addr.split(' ').next().and_then(|a| a.rsplit(':').next()).and_then(|x| x.parse().ok()).unwrap_or(0);
                let _ = tx.send(Ok(port));
                sent = true;
            } else if !sent && line.starts_with("error:") {
                let _ = tx.send(Err(line));
                sent = true;
            }
        }
    });
    let port = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(port)) => port,
        Ok(Err(e)) => panic!("node did not start: {e}"),
        Err(_) => panic!("node did not start within 10 s"),
    };
    let end = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < end, "node did not accept connections");
        std::thread::sleep(Duration::from_millis(20));
    }
    (Proc(p), port)
}

fn call(data: &Path, port: u16, tool: &str, args: Value) -> Result<Value, String> {
    let o = cmd(data).args(["call", "--api", &format!("http://127.0.0.1:{port}"), tool, &args.to_string()]).output().unwrap();
    let (code, stdout, stderr) = out(o);
    if code == 0 { Ok(serde_json::from_str(&stdout).unwrap()) } else { Err(stderr) }
}

fn wait(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn help_and_unknown_commands() {
    let d = tempdir("help");
    let (code, stdout, _) = out(cmd(&d.0).arg("--help").output().unwrap());
    assert_eq!(code, 0);
    for s in ["sametra [run]", "sametra mcp", "sametra call", "--peer", "--api-token", "--no-relay-all"] {
        assert!(stdout.contains(s), "{s}");
    }
    let (code, _, stderr) = out(cmd(&d.0).arg("frobnicate").output().unwrap());
    assert_eq!(code, 2);
    assert!(stderr.contains("USAGE"));
    let (code, _, stderr) = out(cmd(&d.0).args(["run", "--max-peers", "lots"]).output().unwrap());
    assert_eq!(code, 2);
    assert!(stderr.contains("--max-peers"));
}

#[test]
fn identity_is_created_once_and_private() {
    let d = tempdir("id");
    let (code, first, stderr) = out(cmd(&d.0).arg("id").output().unwrap());
    assert_eq!(code, 0);
    assert!(stderr.contains("generated new identity"));
    assert!(first.contains("npub:   npub1") && first.contains("pubkey: ") && !first.contains("nsec"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.0.join("key")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let (_, again, stderr) = out(cmd(&d.0).arg("id").output().unwrap());
    assert_eq!(first, again, "stable across runs");
    assert!(!stderr.contains("generated"));
    let (_, secret, _) = out(cmd(&d.0).args(["id", "--show-secret"]).output().unwrap());
    let nsec = secret.lines().find_map(|l| l.strip_prefix("nsec:   ")).unwrap().to_string();
    // the same identity can be supplied by environment (e.g. a microVM secret)
    let other = tempdir("id-env");
    let (_, via_env, _) = out(cmd(&other.0).arg("id").env("SAMETRA_KEY", &nsec).output().unwrap());
    assert_eq!(via_env, first);
    assert!(!other.0.join("key").exists());
    let (code, _, stderr) = out(cmd(&other.0).arg("id").env("SAMETRA_KEY", "garbage").output().unwrap());
    assert_eq!(code, 2);
    assert!(stderr.contains("SAMETRA_KEY"));
}

#[test]
fn tools_lists_schemas() {
    let d = tempdir("tools");
    let (code, stdout, _) = out(cmd(&d.0).arg("tools").output().unwrap());
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), sametra_mcp::tool_defs().as_array().unwrap().len());
    assert!(v.as_array().unwrap().iter().any(|t| t["name"] == "rate" && t["inputSchema"]["required"] == json!(["score"])));
}

#[test]
fn call_subcommand_against_a_running_node() {
    let d = tempdir("call");
    let (_node, port) = run_node(&d.0, 0, &["--join", "math"]);
    let info = call(&d.0, port, "info", json!({})).unwrap();
    assert_eq!(info["joined"], json!(["math"]));
    let a = call(&d.0, port, "post", json!({"community": "math", "title": "Q", "content": "Why?"})).unwrap();
    let id = a["id"].as_str().unwrap().to_string();
    call(&d.0, port, "post", json!({"parent": id, "content": "Because."})).unwrap();
    let f = call(&d.0, port, "feed", json!({"community": "math"})).unwrap();
    assert_eq!(f["items"][0]["title"], "Q");
    assert_eq!(f["items"][0]["reply_count"], 1);
    // default arguments: `call info` with no JSON
    let o = cmd(&d.0).args(["call", "--api", &format!("http://127.0.0.1:{port}"), "peers"]).output().unwrap();
    assert_eq!(out(o).0, 0);
    // SAMETRA_API env works like --api
    let o = cmd(&d.0).args(["call", "info"]).env("SAMETRA_API", format!("http://127.0.0.1:{port}")).output().unwrap();
    assert_eq!(out(o).0, 0);
    // errors: node-side, bad JSON, bad arguments, unknown tool, missing tool
    assert!(call(&d.0, port, "post", json!({"community": "math", "content": " "})).unwrap_err().contains("content"));
    assert!(call(&d.0, port, "rate", json!({"score": "ten"})).unwrap_err().contains("bad arguments"));
    assert!(call(&d.0, port, "nope", json!({})).unwrap_err().contains("unknown tool"));
    let (code, _, stderr) = out(cmd(&d.0).args(["call", "--api", &format!("http://127.0.0.1:{port}"), "info", "{oops"]).output().unwrap());
    assert!(code == 2 && stderr.contains("bad JSON"));
    let (code, _, stderr) = out(cmd(&d.0).arg("call").output().unwrap());
    assert!(code == 2 && stderr.contains("usage"));
    let dead = free_port();
    assert!(call(&d.0, dead, "info", json!({})).unwrap_err().contains("unreachable"));
}

#[test]
fn mcp_stdio_server() {
    let d = tempdir("mcp");
    let (_node, port) = run_node(&d.0, 0, &[]);
    let mut p = cmd(&d.0).args(["mcp", "--api", &format!("http://127.0.0.1:{port}")]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = p.stdin.take().unwrap();
    let mut stdout = BufReader::new(p.stdout.take().unwrap());
    let mut send = move |msg: Value| {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || -> Value {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    // incomplete initialize requests are refused, as the MCP spec requires
    send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}));
    assert_eq!(recv()["error"]["code"], -32602);
    send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    let init = recv();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "sametra");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["instructions"].as_str().unwrap().contains("public"));
    // notifications get no reply; junk gets an "invalid request" error; then the ping
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!("not an object"));
    assert_eq!(recv()["error"]["code"], -32600);
    send(json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}));
    assert_eq!(recv(), json!({"jsonrpc": "2.0", "id": "p", "result": {}}));

    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let list = recv();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), sametra_mcp::tool_defs().as_array().unwrap().len() + 1, "node tools and `policy`");

    let tool = |id: u32, name: &str, args: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}});
    let text = |v: &Value| -> Value { serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap_or(Value::Null) };
    send(tool(3, "set_profile", json!({"name": "Agent Smith", "agent": true})));
    assert_eq!(text(&recv())["ok"], true);
    send(tool(4, "create_community", json!({"slug": "oncology"})));
    assert_eq!(text(&recv())["community"], "oncology");
    send(tool(5, "post", json!({"community": "oncology", "title": "Hypothesis", "content": "..."})));
    let id = text(&recv())["id"].as_str().unwrap().to_string();
    send(tool(6, "thread", json!({"id": id})));
    let t = text(&recv());
    assert_eq!(t["root"]["author_is_agent"], true);
    assert_eq!(t["root"]["author_name"], "Agent Smith");
    send(tool(7, "memory_set", json!({"key": "todo", "value": "follow up"})));
    recv();
    send(tool(8, "memory_get", json!({})));
    assert_eq!(text(&recv())["entries"][0]["value"], "follow up");
    // tool errors are results with isError, not JSON-RPC errors
    send(tool(9, "rate", json!({"score": 5, "target": "x"})));
    let e = recv();
    assert_eq!(e["result"]["isError"], true);
    assert!(e["result"]["content"][0]["text"].as_str().unwrap().contains("-1..1"));
    send(tool(10, "nope", json!({})));
    assert_eq!(recv()["result"]["isError"], true);
    // unknown methods are JSON-RPC errors
    send(json!({"jsonrpc": "2.0", "id": 11, "method": "resources/list"}));
    assert_eq!(recv()["error"]["code"], -32601);
    drop(send); // closes stdin
    assert!(p.wait().unwrap().success(), "exits cleanly on EOF");
}

#[test]
fn restart_keeps_events_state_and_identity() {
    let d = tempdir("restart");
    let (node, port) = run_node(&d.0, 0, &[]);
    let info = call(&d.0, port, "info", json!({})).unwrap();
    let a = call(&d.0, port, "post", json!({"community": "bio", "title": "Persist me", "content": "x"})).unwrap();
    let id = a["id"].as_str().unwrap().to_string();
    call(&d.0, port, "pin", json!({"id": id})).unwrap();
    call(&d.0, port, "join", json!({"community": "chem"})).unwrap();
    call(&d.0, port, "memory_set", json!({"key": "k", "value": "v"})).unwrap();
    node.interrupt();
    for f in ["key", "state.pb", "memory.pb", "store/events.pb"] {
        assert!(d.0.join(f).exists(), "{f}");
    }
    let (_node, port) = run_node(&d.0, 0, &[]);
    let info2 = call(&d.0, port, "info", json!({})).unwrap();
    assert_eq!(info2["pubkey"], info["pubkey"]);
    assert_eq!(info2["pinned"], 1);
    assert!(info2["joined"].as_array().unwrap().contains(&json!("chem")));
    let t = call(&d.0, port, "thread", json!({"id": id})).unwrap();
    assert_eq!(t["root"]["title"], "Persist me");
    assert_eq!(t["root"]["pinned"], true);
    assert_eq!(call(&d.0, port, "memory_get", json!({"key": "k"})).unwrap()["entries"][0]["value"], "v");
}

#[test]
fn bootstrap_peer_and_gossip_between_processes() {
    let (da, db) = (tempdir("boot-a"), tempdir("boot-b"));
    let (pa, pb) = (free_port(), free_port());
    let (_a, _) = run_node(&da.0, pa, &["--public", &format!("ws://127.0.0.1:{pa}")]);
    let (_b, _) = run_node(&db.0, pb, &["--peer", &format!("ws://127.0.0.1:{pa}"), "--threads", "1"]);
    assert!(wait(Duration::from_secs(20), || call(&db.0, pb, "info", json!({})).unwrap()["peers"] == 1), "b dialed its bootstrap peer");
    let a = call(&da.0, pa, "post", json!({"community": "m", "title": "hello b", "content": "x"})).unwrap();
    let id = a["id"].as_str().unwrap().to_string();
    assert!(wait(Duration::from_secs(10), || call(&db.0, pb, "event", json!({"id": id})).is_ok()));
    let r = call(&db.0, pb, "post", json!({"parent": id, "content": "hello a"})).unwrap();
    let rid = r["id"].as_str().unwrap().to_string();
    assert!(wait(Duration::from_secs(10), || {
        call(&da.0, pa, "thread", json!({"id": id})).unwrap()["root"]["replies"].as_array().is_some_and(|v| v.iter().any(|x| x["id"] == rid.as_str()))
    }));
    let peers = call(&db.0, pb, "peers", json!({})).unwrap();
    assert_eq!(peers["peers"][0]["addr"], format!("ws://127.0.0.1:{pa}"));
    assert_eq!(peers["peers"][0]["outbound"], true);
}

#[cfg(feature = "tls")]
#[test]
fn tls_node_and_https_calls() {
    let d = tempdir("cli-tls");
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let (cert, key) = (d.0.join("cert.pem"), d.0.join("key.pem"));
    std::fs::write(&cert, ck.cert.pem()).unwrap();
    std::fs::write(&key, ck.signing_key.serialize_pem()).unwrap();
    // half a TLS configuration is a startup error, not a silent plain-text node
    let (code, _, stderr) = out(cmd(&d.0).args(["run", "--listen", "127.0.0.1:0", "--tls-cert", cert.to_str().unwrap()]).output().unwrap());
    assert_eq!(code, 2);
    assert!(stderr.contains("both"), "{stderr}");

    let (_node, port) = run_node(&d.0, 0, &["--tls-cert", cert.to_str().unwrap(), "--tls-key", key.to_str().unwrap()]);
    let https = format!("https://localhost:{port}");
    let call_tls = |tool: &str, args: Value, ca: bool| {
        let mut c = cmd(&d.0);
        c.args(["call", "--api", &https, tool, &args.to_string()]);
        if ca {
            c.args(["--tls-ca", cert.to_str().unwrap()]);
        }
        out(c.output().unwrap())
    };
    let (code, stdout, stderr) = call_tls("post", json!({"community": "m", "title": "t", "content": "over https"}), true);
    assert_eq!(code, 0, "{stderr}");
    let id = serde_json::from_str::<Value>(&stdout).unwrap()["id"].as_str().unwrap().to_string();
    // the same port still answers plain http
    assert_eq!(call(&d.0, port, "event", json!({"id": id})).unwrap()["item"]["content"], "over https");
    // without trusting the certificate, https fails loudly
    let (code, _, stderr) = call_tls("info", json!({}), false);
    assert_eq!(code, 2);
    assert!(stderr.to_lowercase().contains("certificate"), "{stderr}");
    // SAMETRA_TLS_CA works like --tls-ca, and so does the MCP server
    let o = cmd(&d.0).args(["call", "--api", &https, "info"]).env("SAMETRA_TLS_CA", &cert).output().unwrap();
    assert_eq!(out(o).0, 0);
}

/// One JSON-RPC session over the MCP stdio server of `sametra mcp ARGS...`.
fn mcp_session(data: &Path, args: &[&str], calls: &[(&str, Value)]) -> Vec<Value> {
    let mut p = cmd(data).arg("mcp").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = p.stdin.take().unwrap();
    let mut out = BufReader::new(p.stdout.take().unwrap());
    let mut send = |v: Value| writeln!(stdin, "{v}").unwrap();
    let mut recv = || -> Value {
        let mut l = String::new();
        out.read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    };
    send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}));
    assert_eq!(recv()["result"]["serverInfo"]["name"], "sametra");
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let mut res = vec![];
    for (i, (name, a)) in calls.iter().enumerate() {
        send(json!({"jsonrpc": "2.0", "id": i + 1, "method": "tools/call", "params": {"name": name, "arguments": a}}));
        let r = recv();
        res.push(serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap_or(r));
    }
    drop(stdin); // EOF: the MCP server exits
    let _ = p.wait();
    res
}

#[test]
fn mcp_node_runs_or_shares_a_node() {
    let d = tempdir("mcp-node");
    let api = format!("http://127.0.0.1:{}", free_port());
    // no node at that address: the MCP server starts one, with this data directory's identity
    let own = mcp_session(&d.0, &["--node", "--no-ipfs", "--api", &api, "--data", d.0.to_str().unwrap()], &[("info", json!({})), ("set_profile", json!({"name": "Agent", "agent": true, "model": "x"}))]);
    let (_, ident, _) = out(cmd(&d.0).args(["id", "--data", d.0.to_str().unwrap()]).output().unwrap());
    assert!(ident.contains(own[0]["npub"].as_str().unwrap()), "embedded node uses the data directory's key");
    assert_eq!(own[1]["ok"], true);
    // a node already running there: the MCP server shares it instead
    let other = tempdir("mcp-node-other");
    let (_node, port) = run_node(&other.0, 0, &[]);
    let shared = mcp_session(&d.0, &["--node", "--api", &format!("http://127.0.0.1:{port}"), "--data", d.0.to_str().unwrap()], &[("info", json!({}))]);
    let (_, other_id, _) = out(cmd(&other.0).args(["id", "--data", other.0.to_str().unwrap()]).output().unwrap());
    assert!(other_id.contains(shared[0]["npub"].as_str().unwrap()));
}

#[test]
fn connect_asks_each_harness_cli() {
    let d = tempdir("connect");
    // a stand-in `claude` that records how it was called
    let bin = d.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = d.0.join("argv");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", log.display());
    for h in ["claude", "codex", "gemini", "agy", "opencode"] {
        let f = bin.join(h);
        std::fs::write(&f, &script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let data = d.0.join("agent");
    let connect = |args: &[&str]| out(cmd(&d.0).arg("connect").args(args).env("PATH", &path).env("HOME", &d.0).output().unwrap());
    let argv = || std::fs::read_to_string(&log).unwrap().lines().map(String::from).collect::<Vec<_>>();
    let (code, stdout, _) = connect(&["claude", "--data", data.to_str().unwrap()]);
    assert_eq!(code, 0, "{stdout}");
    let a = argv();
    assert_eq!(a[..5], ["mcp", "add", "--scope", "user", "sametra"]);
    assert_eq!(a[5], "--");
    assert!(a[6].ends_with("sametra"));
    assert_eq!(a[7..], ["mcp", "--node", "--data", data.to_str().unwrap()]);
    let (code, _, _) = connect(&["codex", "--data", data.to_str().unwrap(), "--peer", "ws://boot:42424"]);
    assert_eq!(code, 0);
    assert!(argv().join(" ").ends_with(&format!("mcp --node --data {} --peer ws://boot:42424", data.display())));
    let (code, _, _) = connect(&["opencode", "--url", "http://127.0.0.1:7448/mcp"]);
    assert_eq!(code, 0);
    assert_eq!(argv(), ["mcp", "add", "sametra", "--url", "http://127.0.0.1:7448/mcp"]);
    // --print shows the command without running it
    std::fs::remove_file(&log).unwrap();
    let (code, stdout, _) = connect(&["gemini", "--print"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("gemini mcp add --scope user sametra "));
    assert!(!log.exists());
    // pi has no MCP: it gets an Agent Skill
    let (code, stdout, _) = connect(&["pi"]);
    assert_eq!(code, 0, "{stdout}");
    let skill = std::fs::read_to_string(d.0.join(".pi/agent/skills/sametra/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: sametra\n") && skill.contains(" call notifications"));
    let (code, _, stderr) = connect(&["emacs"]);
    assert!(code == 2 && stderr.contains("unknown harness"));
    let (code, _, stderr) = connect(&["opencode"]);
    assert!(code == 2 && stderr.contains("--url"));
}

#[test]
fn mcp_over_streamable_http() {
    use std::io::Read;
    let d = tempdir("mcp-http");
    let (_node, port) = run_node(&d.0, 0, &[]);
    let mcp_port = free_port();
    let _mcp = Proc(cmd(&d.0).args(["mcp", "--http", &format!("127.0.0.1:{mcp_port}"), "--api", &format!("http://127.0.0.1:{port}")]).stderr(Stdio::null()).spawn().unwrap());
    assert!(wait(Duration::from_secs(10), || std::net::TcpStream::connect(("127.0.0.1", mcp_port)).is_ok()));
    let mut session = String::new();
    let mut post = |body: Value| -> Option<Value> {
        let b = body.to_string();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", mcp_port)).unwrap();
        let sid = if session.is_empty() { String::new() } else { format!("Mcp-Session-Id: {session}\r\n") };
        write!(s, "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{mcp_port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-06-18\r\n{sid}Content-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()).unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        if let Some(h) = resp.lines().find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:")) {
            session = h.split_once(':').unwrap().1.trim().to_string();
        }
        resp.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).rfind(|l| l.starts_with('{')).map(|l| serde_json::from_str(l).unwrap())
    };
    let init = post(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "sametra");
    post(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let info = post(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "info", "arguments": {}}})).unwrap();
    let v: Value = serde_json::from_str(info["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(v["npub"].as_str().unwrap().starts_with("npub1"));
}

/// The error text of a refused tool call, from [`mcp_session`].
fn refusal(v: &Value) -> String {
    assert_eq!(v["result"]["isError"], true, "expected a refusal, got {v}");
    v["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn agent_policy_limits_and_focus() {
    let d = tempdir("policy");
    let (_node, port) = run_node(&d.0, 0, &[]);
    let api = format!("http://127.0.0.1:{port}");
    let data = d.0.to_str().unwrap();
    let topic = call(&d.0, port, "post", json!({"community": "math", "title": "Focus", "content": "here"})).unwrap()["id"].as_str().unwrap().to_string();
    let elsewhere = call(&d.0, port, "post", json!({"community": "bio", "title": "Other", "content": "there"})).unwrap()["id"].as_str().unwrap().to_string();
    let policy = format!("[budget]\nposts_per_day = 3\ntool_calls_per_session = 6\n[focus]\nmode = \"only\"\ntopics = [\"{topic}\"]\n");
    std::fs::write(d.0.join("agent.toml"), policy).unwrap();
    // `mcp --node` picks up DATA/agent.toml
    let r = mcp_session(&d.0, &["--node", "--api", &api, "--data", data], &[
        ("post", json!({"parent": topic, "content": "in scope"})),
        ("post", json!({"topic": topic, "title": "Sub", "content": "a subtopic is in scope"})),
        ("post", json!({"parent": elsewhere, "content": "out of scope"})),
        ("post", json!({"community": "bio", "title": "New", "content": "new topic elsewhere"})),
        ("rate", json!({"event": elsewhere, "score": 0.5})),
        ("policy", json!({})),
        ("info", json!({})),
    ]);
    assert_eq!(r[0]["ok"], true);
    assert_eq!(r[1]["ok"], true);
    // replies inside the subtopic are in scope too
    let sub = r[1]["id"].as_str().unwrap();
    assert!(refusal(&r[2]).contains("out of scope"));
    assert!(refusal(&r[3]).contains("out of scope"));
    assert!(refusal(&r[4]).contains("out of scope"));
    assert_eq!(r[5]["spent_today"]["posts"], 2);
    assert_eq!(r[5]["focus"]["mode"], "only");
    assert!(refusal(&r[6]).contains("all 6 of its tool calls"));
    // posts per day hold across sessions
    let r = mcp_session(&d.0, &["--node", "--api", &api, "--data", data], &[
        ("post", json!({"parent": sub, "content": "third"})),
        ("post", json!({"parent": sub, "content": "fourth"})),
    ]);
    assert_eq!(r[0]["ok"], true);
    assert!(refusal(&r[1]).contains("all 3 of today's posts"));
}

#[test]
fn agent_runs_sessions_within_budget() {
    let d = tempdir("agent");
    let (_node, port) = run_node(&d.0, 0, &[]);
    let api = format!("http://127.0.0.1:{port}");
    let data = d.0.to_str().unwrap();
    let agent = |args: &[&str]| out(cmd(&d.0).arg("agent").args(args).args(["--api", &api, "--data", data]).output().unwrap());
    let (code, _, stderr) = agent(&["--once"]);
    assert!(code == 2 && stderr.contains("agent init"), "{stderr}");
    let (code, stdout, _) = agent(&["init"]);
    assert_eq!(code, 0);
    assert!(stdout.trim().ends_with("agent.toml"));
    assert_eq!(agent(&["init"]).0, 2, "never overwrites a policy");
    // a stand-in harness: records its prompt and reports usage the way Claude Code does
    let fake = d.0.join("harness.sh");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s' \"$1\" > {}\necho '{{\"total_cost_usd\": 0.6, \"usage\": {{\"input_tokens\": 1000, \"output_tokens\": 200}}}}'\n", d.0.join("prompt").display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let policy = format!("[agent]\nharness = \"command\"\ncommand = [\"{}\", \"{{prompt}}\"]\nprompt = \"Be brief.\"\n[budget]\nusd_per_day = 1.0\n[focus]\nmode = \"prefer\"\ncommunities = [\"math\"]\n", fake.display());
    std::fs::write(d.0.join("agent.toml"), policy).unwrap();
    let (code, _, stderr) = agent(&["--print"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = agent(&["--once"]);
    assert_eq!(code, 0, "{stderr}");
    let prompt = std::fs::read_to_string(d.0.join("prompt")).unwrap();
    assert!(prompt.contains("notifications") && prompt.contains("Your priority is the communities math") && prompt.ends_with("Be brief."));
    assert!(stderr.contains("1000 in / 200 out tokens, $0.6000"), "{stderr}");
    assert_eq!(agent(&["--once"]).0, 0, "$0.60 of $1.00: one more session");
    let (code, _, stderr) = agent(&["--once"]);
    assert!(code == 2 && stderr.contains("daily limit reached"), "{stderr}");
    let (_, status, stderr) = agent(&["status"]);
    assert!(!stderr.contains("listening"), "status does not start a node");
    let s: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(s["ledger"]["sessions"], 2);
    assert!((s["ledger"]["usd_day"].as_f64().unwrap() - 1.2).abs() < 1e-9);
}


/// GET over plain HTTP; (status, headers, body).
fn http_get(port: u16, path: &str, host: &str) -> (u16, String, Vec<u8>) {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut resp = vec![];
    s.read_to_end(&mut resp).unwrap();
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&resp[..split]).to_string();
    let mut body = resp[split + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        let (mut out, mut rest) = (vec![], &body[..]);
        loop {
            let nl = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let n = usize::from_str_radix(std::str::from_utf8(&rest[..nl]).unwrap(), 16).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&rest[nl + 2..nl + 2 + n]);
            rest = &rest[nl + 4 + n..];
        }
        body = out;
    }
    (head.split(' ').nth(1).unwrap().parse().unwrap(), head, body)
}

#[test]
fn ipfs_files_move_between_nodes() {
    let (a, b) = (tempdir("ipfs-a"), tempdir("ipfs-b"));
    let ipfs = ["--ipfs-private", "--ipfs-port", "0"];
    let (_na, pa) = run_node(&a.0, 0, &ipfs);
    let (_nb, pb) = run_node(&b.0, 0, &[&ipfs[..], &["--peer", &format!("ws://127.0.0.1:{pa}")]].concat());
    let info = call(&a.0, pa, "info", json!({})).unwrap();
    assert!(info["ipfs_peer"].as_str().unwrap().starts_with("12D3Koo"));
    // the nodes find each other's IPFS side through their heartbeats
    assert!(wait(Duration::from_secs(15), || call(&b.0, pb, "info", json!({})).unwrap()["ipfs_peers"].as_u64() > Some(0)));
    let added = call(&a.0, pa, "ipfs_add", json!({"text": "# Lemma\n\nEvery group of prime order is cyclic.", "name": "lemma.md"})).unwrap();
    let cid = added["cid"].as_str().unwrap().to_string();
    assert_eq!(added["link"], format!("[lemma.md](ipfs://{cid})"));
    // B's gateway fetches it from A for its operator
    let (code, head, body) = http_get(pb, &format!("/ipfs/{cid}"), &format!("127.0.0.1:{pb}"));
    assert_eq!(code, 200, "{head}");
    assert_eq!(body, b"# Lemma\n\nEvery group of prime order is cyclic.");
    assert!(head.contains("Content-Security-Policy: sandbox") && head.contains("immutable"));
    let (code, head, raw) = http_get(pb, &format!("/ipfs/{cid}?format=raw"), "localhost");
    assert!(code == 200 && head.contains("application/vnd.ipld.raw"));
    assert_eq!(raw, body, "small files are one raw block");
    // anyone else only gets what B already has
    let other = call(&a.0, pa, "ipfs_add", json!({"text": "only on A"})).unwrap()["cid"].as_str().unwrap().to_string();
    let (code, _, body) = http_get(pb, &format!("/ipfs/{other}"), "example.org");
    assert_eq!(code, 404);
    assert!(String::from_utf8_lossy(&body).contains("not on this node"));
    assert_eq!(http_get(pb, &format!("/ipfs/{cid}"), "example.org").0, 200);
    // a file a post links to is fetched and kept by the nodes that carry the post
    let data = call(&a.0, pa, "ipfs_add", json!({"text": "x,y\n1,2", "name": "d.csv"})).unwrap()["cid"].as_str().unwrap().to_string();
    call(&a.0, pa, "post", json!({"community": "math", "title": "Data", "content": format!("see [d.csv](ipfs://{data})")})).unwrap();
    assert!(wait(Duration::from_secs(15), || call(&b.0, pb, "ipfs_get", json!({"path": format!("ipfs://{data}"), "local": true})).is_ok_and(|v| v["text"] == "x,y\n1,2")));
    // pin and unpin
    assert_eq!(call(&b.0, pb, "ipfs_pin", json!({"path": data})).unwrap()["ok"], true);
    assert_eq!(call(&b.0, pb, "ipfs_get", json!({"path": data, "local": true})).unwrap()["pinned"], true);
    assert!(call(&a.0, pa, "ipfs_get", json!({"path": "ipfs://nope"})).is_err());
}
