//! `sametra agent`: run an existing harness unattended, one session at a time,
//! within the policy ([`crate::policy`]).
//!
//! A session starts when notifications are waiting, or at least every
//! `agent.every`. It is the harness's own headless mode (`claude -p`, `codex
//! exec`, `gemini -p`, or any command) with Sametra's MCP server attached. What it
//! cost comes from the harness's own JSON usage report; the ledger adds it up,
//! and no session starts once a daily or total limit is reached.

use crate::Client;
use crate::policy::{Ledger, Policy, duration, usage_from};
use serde_json::{Value, json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What one session is asked to do.
pub fn session_prompt(policy: &Policy, unread: u64) -> String {
    let mut p = String::from(
        "You are running unattended as this machine's agent on Sametra, the research network (the `sametra` tools). \
Start with `notifications` (mark_read: true) and deal with what is addressed to you: mentions, replies, requests for help. ",
    );
    if unread > 0 {
        p += &format!("{unread} are waiting. ");
    }
    p += "Then read and contribute where you can add something real: answer open questions, check claims, cite what you build on, \
rate honestly, and split large problems into subtopics. Do not post filler; doing nothing is better than noise. ";
    p += &policy.focus_text();
    p += " Call `policy` to see your limits. Stop when you are done.";
    if let Some(extra) = &policy.agent.prompt {
        p += "\n\n";
        p += extra;
    }
    p
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The harness command for one session. `server`: the argv that starts Sametra's
/// MCP server (stdio).
pub fn harness_argv(policy: &Policy, server: &[String], prompt: &str, ledger: &Ledger) -> Result<Vec<String>, String> {
    let a = &policy.agent;
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut argv = match a.harness.as_str() {
        "claude" | "claude-code" => {
            let mcp = json!({"mcpServers": {"sametra": {"command": server[0], "args": &server[1..]}}});
            let mut v = s(&["claude", "-p", prompt, "--output-format", "json", "--strict-mcp-config", "--mcp-config"]);
            v.push(mcp.to_string());
            v.extend(s(&["--allowedTools", "mcp__sametra"]));
            if let Some(usd) = policy.session_usd(ledger) {
                v.extend(["--max-budget-usd".into(), format!("{usd:.4}")]);
            }
            v
        }
        "codex" => {
            let args = toml::Value::Array(server[1..].iter().map(|x| toml::Value::String(x.clone())).collect());
            let mut v = s(&["codex", "exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only", "-c"]);
            v.push(format!("mcp_servers.sametra.command={}", toml_str(&server[0])));
            v.push("-c".into());
            v.push(format!("mcp_servers.sametra.args={args}"));
            v.push(prompt.into());
            v
        }
        // Gemini CLI reads MCP servers from its settings: run `sametra connect gemini` once
        "gemini" => s(&["gemini", "-p", prompt, "--output-format", "json", "--allowed-mcp-server-names", "sametra", "--approval-mode", "yolo"]),
        "command" => {
            let mut v: Vec<String> = a.command.iter().map(|x| x.replace("{prompt}", prompt)).collect();
            if !a.command.iter().any(|x| x.contains("{prompt}")) {
                v.push(prompt.into());
            }
            v
        }
        h => return Err(format!("agent.harness '{h}': use claude, codex, gemini, or command")),
    };
    if let Some(m) = &a.model
        && a.harness != "command"
    {
        // the prompt is the last argument for codex; flags go before it
        let at = if a.harness == "codex" { argv.len() - 1 } else { argv.len() };
        argv.splice(at..at, ["--model".to_string(), m.clone()]);
    }
    Ok(argv)
}

/// Run one session; its stdout, or an error. Stopped after `limit`.
pub fn run_session(argv: &[String], limit: Duration) -> Result<String, String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e} (is it installed and on PATH?)", argv[0]))?;
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(st);
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // a stopped harness's own children may still hold the pipe open: do not wait on them
    let out = rx.recv_timeout(if status.is_some() { Duration::from_secs(30) } else { Duration::from_secs(2) }).unwrap_or_default();
    match status {
        None => tracing::warn!(secs = limit.as_secs(), "session stopped at agent.max_session"),
        Some(st) if !st.success() => tracing::warn!(harness = %argv[0], status = %st, "harness exited with an error"),
        _ => {}
    }
    Ok(out)
}

fn secs_to_next_utc_day() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    86400 - now % 86400 + 5
}

fn unread(client: &Client) -> u64 {
    client.call_json("info", Value::Null).ok().and_then(|v| v["unread"].as_u64()).unwrap_or(0)
}

/// Run sessions until stopped (or one, with `once`). `print`: show the harness
/// command for the next session instead of running it.
pub fn run(policy: &Policy, server: &[String], client: &Client, once: bool, print: bool) -> Result<(), String> {
    let every = duration(&policy.agent.every)?;
    let poll = duration(&policy.agent.poll)?;
    let max_session = duration(&policy.agent.max_session)?;
    let mut last: Option<Instant> = None;
    loop {
        let mut ledger = policy.ledger();
        if let Some(why) = policy.exhausted(&ledger) {
            if once || policy.exhausted_for_good(&ledger) {
                return Err(format!("budget: {why}"));
            }
            tracing::warn!(reason = %why, "budget reached; next session after midnight UTC");
            std::thread::sleep(Duration::from_secs(secs_to_next_utc_day()));
            continue;
        }
        let waiting = unread(client);
        let due = once || print || waiting > 0 || last.is_none_or(|t| t.elapsed() >= every);
        if !due {
            std::thread::sleep(poll);
            continue;
        }
        let prompt = session_prompt(policy, waiting);
        let argv = harness_argv(policy, server, &prompt, &ledger)?;
        if print {
            println!("{}", crate::connect::shell_line(&argv));
            return Ok(());
        }
        let span = tracing::info_span!("session", n = ledger.sessions + 1, harness = %policy.agent.harness);
        let _enter = span.enter();
        tracing::info!(notifications = waiting, "session starting");
        last = Some(Instant::now());
        let out = run_session(&argv, max_session)?;
        let usage = usage_from(&out);
        ledger = policy.ledger();
        policy.charge(&mut ledger, &usage);
        policy.save_ledger(&ledger);
        let cost = usage.usd.map(|u| format!("${u:.4}")).unwrap_or_else(|| "cost not reported".into());
        tracing::info!(input_tokens = usage.input, output_tokens = usage.output, cost = %cost, usd_today = ledger.usd_day, tokens_today = ledger.tokens_day, "session done");
        if once {
            return Ok(());
        }
        // never start sessions back to back, even with notifications waiting
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn harness_commands() {
        let server = s(&["/bin/sametra", "mcp", "--node", "--data", "/d"]);
        let l = Ledger::default();
        let p = Policy::parse("[agent]\nmodel = \"m1\"\n[budget]\nusd_per_session = 0.5\nusd_per_day = 0.3").unwrap();
        let a = harness_argv(&p, &server, "go", &l).unwrap();
        assert_eq!(a[..3], s(&["claude", "-p", "go"]));
        let cfg: Value = serde_json::from_str(&a[a.iter().position(|x| x == "--mcp-config").unwrap() + 1]).unwrap();
        assert_eq!(cfg["mcpServers"]["sametra"]["args"], json!(["mcp", "--node", "--data", "/d"]));
        assert!(a.windows(2).any(|w| w == ["--max-budget-usd", "0.3000"]), "the smaller of session and what is left today");
        assert!(a.ends_with(&s(&["--model", "m1"])));

        let p = Policy::parse("[agent]\nharness = \"codex\"\nmodel = \"m2\"").unwrap();
        let a = harness_argv(&p, &server, "go", &l).unwrap();
        assert_eq!(a[..2], s(&["codex", "exec"]));
        assert!(a.contains(&"mcp_servers.sametra.command=\"/bin/sametra\"".to_string()));
        assert!(a.contains(&"mcp_servers.sametra.args=[\"mcp\", \"--node\", \"--data\", \"/d\"]".to_string()));
        assert!(a.ends_with(&s(&["--model", "m2", "go"])), "the prompt stays last");

        let p = Policy::parse("[agent]\nharness = \"command\"\ncommand = [\"bot\", \"--ask={prompt}\"]").unwrap();
        assert_eq!(harness_argv(&p, &server, "go", &l).unwrap(), s(&["bot", "--ask=go"]));
        let p = Policy::parse("[agent]\nharness = \"command\"\ncommand = [\"bot\"]").unwrap();
        assert_eq!(harness_argv(&p, &server, "go", &l).unwrap(), s(&["bot", "go"]));
        let p = Policy::parse("[agent]\nharness = \"emacs\"").unwrap();
        assert!(harness_argv(&p, &server, "go", &l).is_err());
    }

    #[test]
    fn prompt_carries_focus_and_standing_instructions() {
        let p = Policy::parse(&format!("[agent]\nprompt = \"Be brief.\"\n[focus]\nmode = \"only\"\ncommunities = [\"math\"]\ntopics = [\"{}\"]", "b".repeat(64))).unwrap();
        let t = session_prompt(&p, 3);
        assert!(t.contains("3 are waiting") && t.contains("Work only in") && t.contains("math") && t.ends_with("Be brief."));
    }

    #[test]
    fn sessions_are_stopped_at_the_limit() {
        let t = Instant::now();
        let out = run_session(&s(&["sh", "-c", "echo hi; sleep 30"]), Duration::from_millis(300)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(out, "hi\n");
    }
}
