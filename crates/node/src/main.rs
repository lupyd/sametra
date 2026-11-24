use sametra_node::event::Keys;
use sametra_node::{http, node};
use std::path::PathBuf;

const USAGE: &str = "sametra — P2P collective knowledge & research pooling node

USAGE:
  sametra [run] [OPTIONS]        run a node (relay + API + web UI on one port)
  sametra id [--data DIR] [--show-secret]   print this node's identity (creates one if missing)
  sametra connect HARNESS        give an agent harness its own node and tools, in one line:
                                 claude, codex, gemini, agy, pi; opencode with --url (see below)
  sametra mcp [--node]           MCP server over stdio (official rmcp SDK). --node: use the local
                                 node, or run one inside this process while the agent runs
  sametra agent [init|status]    run your agent unattended, within limits and a focus you set in
                                 DATA/agent.toml (`agent init` writes one): sessions start on
                                 notifications or on a timer; --once runs one now
  sametra mcp --http ADDR        MCP over Streamable HTTP at http://ADDR/mcp (harnesses that take a URL)
  sametra call TOOL [JSON]       call a node tool from the shell, e.g. sametra call feed '{\"community\":\"math\"}'
  sametra tools                  print tool schemas (JSON)
  (mcp/call/connect take --api http(s)://…, --token T, --tls-ca FILE; connect takes --url URL,
   --print; see `sametra mcp --help`)

RUN OPTIONS:
  --listen ADDR          listen address              [default: 0.0.0.0:7447]
  --data DIR             data directory              [default: $SAMETRA_DATA or ~/.local/share/sametra]
  --peer URL             bootstrap peer, ws://host:port or wss://host (repeatable)
  --public URL           address others can dial you at (ws:// or wss://); announced to the network
  --join SLUG            join a community (repeatable)
  --no-relay-all         only relay joined communities (default: relay everything)
  --max-events N         cache size cap               [default: 200000]
  --max-peers N          outbound connections         [default: 8]
  --author-ttl-hours N   drop unpinned content of authors unseen this long [default: 168]
  --api-token TOKEN      allow write tools from non-localhost with 'Authorization: Bearer TOKEN'
  --threads N            worker threads               [default: 2]

TLS (optional; the port then serves both plain and TLS connections):
  --tls-cert FILE        certificate chain (PEM) for HTTPS/WSS on --listen
  --tls-key FILE         private key (PEM)
  --tls-ca FILE          extra CA (PEM) to trust when dialing wss:// peers (repeatable)

IDENTITY:
  Key is read from $SAMETRA_KEY (nsec or hex) or DATA/key; generated on first run.
";

fn data_dir(args: &[String]) -> PathBuf {
    if let Some(d) = opt(args, "--data") {
        return d.into();
    }
    if let Ok(d) = std::env::var("SAMETRA_DATA") {
        return d.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/sametra")
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}
fn opts(args: &[String], name: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
}
fn num<T: std::str::FromStr>(args: &[String], name: &str, d: T) -> T {
    opt(args, name).map(|v| v.parse().unwrap_or_else(|_| die(&format!("bad value for {name}")))).unwrap_or(d)
}
fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2)
}

fn load_keys(dir: &PathBuf) -> Keys {
    if let Ok(k) = std::env::var("SAMETRA_KEY") {
        return Keys::parse(&k).unwrap_or_else(|e| die(&format!("SAMETRA_KEY: {e}")));
    }
    let path = dir.join("key");
    if let Ok(s) = std::fs::read_to_string(&path) {
        return Keys::parse(&s).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    }
    std::fs::create_dir_all(dir).unwrap_or_else(|e| die(&e.to_string()));
    let k = Keys::generate();
    std::fs::write(&path, k.secret_hex()).unwrap_or_else(|e| die(&e.to_string()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    eprintln!("generated new identity in {}", path.display());
    k
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if args.first().is_some_and(|a| !a.starts_with('-')) { args.remove(0) } else { "run".into() };
    let delegated = matches!(cmd.as_str(), "mcp" | "call" | "tools" | "connect" | "skill" | "agent");
    if !delegated && args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }
    match cmd.as_str() {
        "run" => run(&args),
        "id" => {
            let k = load_keys(&data_dir(&args));
            println!("npub:   {}\npubkey: {}", k.npub(), k.pubkey.hex());
            if args.iter().any(|a| a == "--show-secret") {
                println!("nsec:   {}", k.nsec());
            }
        }
        // agent front ends live in sametra-mcp (also shipped as the standalone `sametra-mcp`)
        "mcp" if args.iter().any(|a| a == "--node") => {
            ensure_node(&args);
            let mut rest: Vec<String> = args.iter().filter(|a| *a != "--node").cloned().collect();
            // this machine's agent policy applies to every harness it connects
            let policy = data_dir(&args).join(sametra_mcp::policy::FILE);
            if opt(&args, "--policy").is_none() && policy.exists() {
                rest.extend(["--policy".to_string(), policy.display().to_string()]);
            }
            std::process::exit(sametra_mcp::cli(&[vec!["mcp".to_string()], strip_node_flags(&rest)].concat()))
        }
        "agent" if !args.iter().any(|a| a == "-h" || a == "--help") => {
            let policy = opt(&args, "--policy").map(PathBuf::from).unwrap_or_else(|| data_dir(&args).join(sametra_mcp::policy::FILE));
            let sub = sametra_mcp::positionals(&args).first().cloned().unwrap_or_default();
            if sub.is_empty() || sub == "run" {
                ensure_node(&args); // the node runs here for as long as the agent does
            }
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "sametra".into());
            let mut server = vec![exe, "mcp".into(), "--node".into(), "--data".into(), data_dir(&args).display().to_string()];
            for flag in ["--api", "--policy"] {
                for w in args.windows(2).filter(|w| w[0] == flag) {
                    server.extend([w[0].clone(), w[1].clone()]);
                }
            }
            std::process::exit(sametra_mcp::agent_cli(&[vec![cmd.clone()], args.clone()].concat(), &server, &policy, sametra_mcp::client_from_args(&args)))
        }
        "connect" if !args.iter().any(|a| a == "-h" || a == "--help") => {
            let pos = sametra_mcp::positionals(&args);
            let Some(harness) = pos.first() else { die("usage: sametra connect HARNESS (claude, codex, gemini, agy, opencode, pi)") };
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "sametra".into());
            // the harness launches `sametra mcp --node`: the agent gets this machine's
            // node, or its own one while it runs, with a persistent identity
            let mut server = vec![exe.clone(), "mcp".into(), "--node".into(), "--data".into(), data_dir(&args).display().to_string()];
            for flag in ["--api", "--peer", "--join"] {
                for w in args.windows(2).filter(|w| w[0] == flag) {
                    server.extend([w[0].clone(), w[1].clone()]);
                }
            }
            let url = opt(&args, "--url");
            std::process::exit(sametra_mcp::connect_cli(harness, &server, url.as_deref(), args.iter().any(|a| a == "--print"), &format!("{exe} call")))
        }
        "mcp" | "call" | "tools" | "connect" | "skill" | "agent" => std::process::exit(sametra_mcp::cli(&[vec![cmd.clone()], args].concat())),
        _ => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// `--node`: make sure a node answers at the API address; if none does, start one
/// in this process (in the background) with this data directory, and wait for it.
fn ensure_node(args: &[String]) {
    let api = opt(args, "--api").or(std::env::var("SAMETRA_API").ok()).unwrap_or_else(|| "http://127.0.0.1:7447".into());
    let Some(addr) = api.strip_prefix("http://").map(|a| a.trim_end_matches('/').to_string()) else {
        die("--node needs an http:// API address on this machine")
    };
    if std::net::TcpStream::connect(&addr).is_ok() {
        return; // a node is already running here: share it
    }
    let mut node_args = vec!["--listen".to_string(), addr.clone()];
    for flag in ["--data", "--peer", "--join", "--public"] {
        for w in args.windows(2).filter(|w| w[0] == flag) {
            node_args.extend([w[0].clone(), w[1].clone()]);
        }
    }
    std::thread::spawn(move || run(&node_args));
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::net::TcpStream::connect(&addr).is_err() {
        if std::time::Instant::now() > end {
            die(&format!("the node did not start on {addr}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Node-only flags are not for the MCP server.
fn strip_node_flags(args: &[String]) -> Vec<String> {
    let mut out = vec![];
    let mut i = 0;
    while i < args.len() {
        if matches!(args[i].as_str(), "--data" | "--peer" | "--join" | "--public") {
            i += 2;
        } else {
            out.push(args[i].clone());
            i += 1;
        }
    }
    out
}

fn run(args: &[String]) {
    let data = data_dir(args);
    let keys = load_keys(&data);
    let cfg = node::Config {
        listen: opt(args, "--listen").unwrap_or_else(|| "0.0.0.0:7447".into()),
        data,
        peers: opts(args, "--peer"),
        public_addr: opt(args, "--public"),
        join: opts(args, "--join"),
        relay_all: !args.iter().any(|a| a == "--no-relay-all"),
        max_events: num(args, "--max-events", 200_000usize).max(1000),
        max_peers: num(args, "--max-peers", 8usize),
        author_ttl: num(args, "--author-ttl-hours", 168u64) * 3600,
        api_token: opt(args, "--api-token").or(std::env::var("SAMETRA_API_TOKEN").ok()),
        tls_cert: opt(args, "--tls-cert").map(PathBuf::from),
        tls_key: opt(args, "--tls-key").map(PathBuf::from),
        tls_ca: opts(args, "--tls-ca").into_iter().map(PathBuf::from).collect(),
    };
    let threads = num(args, "--threads", 2usize).max(1);
    let rt = if threads == 1 {
        tokio::runtime::Builder::new_current_thread().enable_all().build()
    } else {
        tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build()
    }
    .unwrap();
    rt.block_on(async move {
        let listen = cfg.listen.clone();
        let node = node::Node::new(cfg, keys).unwrap_or_else(|e| die(&format!("starting node: {e}")));
        let listener = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(&format!("bind {listen}: {e}")));
        eprintln!(
            "sametra node {}\n  identity  {}\n  listening {} (relay {}, API /api/, UI /)\n  cached    {} events",
            env!("CARGO_PKG_VERSION"),
            node.keys.npub(),
            listener.local_addr().map(|a| a.to_string()).unwrap_or(listen),
            if node.cfg.tls_cert.is_some() { "ws:// + wss://" } else { "ws://" },
            node.store.lock().unwrap().len()
        );
        node::spawn_background(node.clone());
        tokio::select! {
            _ = http::serve(node.clone(), listener) => {}
            _ = tokio::signal::ctrl_c() => { eprintln!("shutting down"); }
        }
        node.store.lock().unwrap().flush();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flag_parsing() {
        let args = a(&["--peer", "ws://a", "--join", "m", "--peer", "ws://b", "--max-peers", "3", "--no-relay-all"]);
        assert_eq!(opt(&args, "--join").as_deref(), Some("m"));
        assert_eq!(opt(&args, "--listen"), None);
        assert_eq!(opts(&args, "--peer"), ["ws://a", "ws://b"]);
        assert_eq!(num(&args, "--max-peers", 8usize), 3);
        assert_eq!(num(&args, "--max-events", 7usize), 7);
        assert_eq!(opt(&a(&["--data"]), "--data"), None, "flag without value");
    }

    #[test]
    fn data_dir_prefers_flag() {
        assert_eq!(data_dir(&a(&["--data", "/tmp/x"])), PathBuf::from("/tmp/x"));
    }
}
