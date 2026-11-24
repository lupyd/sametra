//! One logging setup for both binaries (`sametra`, `sametra-mcp`).
//!
//! Our crates emit [`tracing`] events. This installs a `tracing-subscriber`
//! that writes them to stderr (stdout carries MCP and command output), and also
//! collects `log` records from dependencies through `tracing-log`.
//!
//! What gets logged: `--log FILTER`, else `$SAMETRA_LOG`, else `$RUST_LOG`, else
//! `info` for Sametra and `warn` for everything else. Filters use the
//! `EnvFilter` syntax: `debug`, `warn,sametra_node=debug`, `sametra_mcp::agent=trace`.
//! Format: `--log-format compact` (default), `pretty` or `json`.

use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

/// Sametra at `info`, dependencies (libp2p is chatty) at `warn`. connexa logs
/// every aborted inbound handshake as an error; libp2p aborts the losers whenever
/// a peer dials several of our addresses at once, so those are off unless asked for.
pub const QUIET: &str = "connexa::task::swarm=off";
pub const DEFAULT: &str = "warn,sametra=info,sametra_node=info,sametra_mcp=info,sametra_transport=info,sametra_proto=info,connexa::task::swarm=off";

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// The filter to use, and where it came from (for error messages).
fn filter_spec(args: &[String]) -> (String, &'static str) {
    if let Some(f) = opt(args, "--log") {
        return (f, "--log");
    }
    for var in ["SAMETRA_LOG", "RUST_LOG"] {
        if let Ok(f) = std::env::var(var)
            && !f.trim().is_empty()
        {
            return (f, if var == "RUST_LOG" { "$RUST_LOG" } else { "$SAMETRA_LOG" });
        }
    }
    (DEFAULT.to_string(), "default")
}

/// A bare level (`debug`) means that level for Sametra and `warn` for the rest,
/// so `--log debug` does not drown in libp2p internals; anything else is used as is.
fn expand(spec: &str) -> String {
    let s = spec.trim();
    if matches!(s, "trace" | "debug" | "info" | "warn" | "error") {
        let deps = if matches!(s, "error") { "error" } else { "warn" };
        format!("{deps},sametra={s},sametra_node={s},sametra_mcp={s},sametra_transport={s},sametra_proto={s},{QUIET}")
    } else {
        s.to_string()
    }
}

/// Whether logs are JSON (then plain-text banners would only get in the way).
pub fn json(args: &[String]) -> bool {
    opt(args, "--log-format").as_deref() == Some("json")
}

/// Install the subscriber. Safe to call more than once (later calls do nothing).
pub fn init(args: &[String]) -> Result<(), String> {
    let (spec, from) = filter_spec(args);
    let filter = EnvFilter::try_new(expand(&spec)).map_err(|e| format!("{from}: bad log filter '{spec}': {e}"))?;
    let ansi = std::io::stderr().is_terminal();
    let b = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(ansi);
    let r = match opt(args, "--log-format").as_deref().unwrap_or("compact") {
        "compact" => b.compact().try_init(),
        "pretty" => b.pretty().try_init(),
        "json" => b.json().with_current_span(true).try_init(),
        f => return Err(format!("--log-format: '{f}' (use compact, pretty or json)")),
    };
    // already installed (tests, or a node started inside `mcp --node`): keep it
    let _ = r;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        assert_eq!(expand("debug"), "warn,sametra=debug,sametra_node=debug,sametra_mcp=debug,sametra_transport=debug,sametra_proto=debug,connexa::task::swarm=off");
        assert!(expand("error").starts_with("error,sametra=error"));
        assert_eq!(expand("off"), "off");
        assert_eq!(expand("warn,sametra_node=trace"), "warn,sametra_node=trace");
        assert!(EnvFilter::try_new(DEFAULT).is_ok());
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(filter_spec(&a(&["--log", "trace"])).0, "trace");
        assert!(init(&a(&["--log", "sametra=[[["])).is_err());
        assert!(init(&a(&["--log-format", "xml"])).is_err());
    }
}
