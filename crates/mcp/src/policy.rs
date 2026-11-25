//! What an agent may spend and where it works, from a TOML file: `agent.toml` in
//! the node's data directory, or `--policy FILE`.
//!
//! Every section and key is optional, and unknown keys are ignored, so a file
//! written for a newer Sametra still loads in an older one.
//!
//! Two parties enforce it:
//! - the MCP server, during a session: tool calls per session, posts per day,
//!   and the topic scope in `only` mode (out-of-scope posts and ratings are refused);
//! - the agent runner (`sametra agent`), between sessions: money and tokens per
//!   day and in total, read from each harness's own usage report, and a per-session
//!   cap passed to harnesses that support one (Claude Code's `--max-budget-usd`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FILE: &str = "agent.toml";

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub agent: Agent,
    pub budget: Budget,
    pub focus: Focus,
    /// Where the policy was read from; the spend ledger lives next to it.
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Agent {
    /// claude, codex, gemini, or "command" (then `command` is the argv)
    pub harness: String,
    pub model: Option<String>,
    /// For harness = "command": argv; "{prompt}" is replaced by the session prompt.
    pub command: Vec<String>,
    /// Start a session at least this often ("90s", "30m", "6h", "1d").
    pub every: String,
    /// Check for new notifications this often; one starts a session early.
    pub poll: String,
    /// Stop a session that runs longer than this.
    pub max_session: String,
    /// Standing instructions added to every session.
    pub prompt: Option<String>,
}

impl Default for Agent {
    fn default() -> Agent {
        Agent {
            harness: "claude".into(),
            model: None,
            command: vec![],
            every: "6h".into(),
            poll: "2m".into(),
            max_session: "30m".into(),
            prompt: None,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Budget {
    pub usd_per_session: Option<f64>,
    pub usd_per_day: Option<f64>,
    pub usd_total: Option<f64>,
    pub tokens_per_day: Option<u64>,
    pub tokens_total: Option<u64>,
    pub tool_calls_per_session: Option<u32>,
    pub posts_per_day: Option<u32>,
    /// Prices for harnesses that report tokens but not money.
    pub price: Option<Price>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// contribute anywhere
    #[default]
    Free,
    /// start from the focus topics, but free to wander when it can help elsewhere
    Prefer,
    /// only the focus topics and everything under them
    Only,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Focus {
    pub mode: Mode,
    /// topic event ids (hex); their subtopics and replies are included
    pub topics: Vec<String>,
}

/// "90s", "30m", "6h", "1d", or plain seconds.
pub fn duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().map_err(|_| format!("bad duration '{s}' (use 90s, 30m, 6h, 1d)"))?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(format!("bad duration '{s}' (use 90s, 30m, 6h, 1d)")),
    };
    Ok(std::time::Duration::from_secs(secs.max(1)))
}

impl Policy {
    pub fn parse(text: &str) -> Result<Policy, String> {
        let p: Policy = toml::from_str(text).map_err(|e| e.to_string())?;
        p.validate()?;
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<Policy, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut p = Policy::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        p.path = Some(path.to_path_buf());
        Ok(p)
    }

    fn validate(&self) -> Result<(), String> {
        for d in [&self.agent.every, &self.agent.poll, &self.agent.max_session] {
            duration(d)?;
        }
        if self.focus.mode != Mode::Free && self.focus.topics.is_empty() {
            return Err("focus: mode \"prefer\" or \"only\" needs topics".into());
        }
        if let Some(t) = self.focus.topics.iter().find(|t| t.len() != 64 || !t.chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(format!("focus.topics: '{t}' is not an event id (64 hex characters)"));
        }
        if self.agent.harness == "command" && self.agent.command.is_empty() {
            return Err("agent: harness \"command\" needs `command = [\"prog\", \"arg\", \"{prompt}\"]`".into());
        }
        Ok(())
    }

    /// Where spending is recorded.
    pub fn ledger_path(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_file_name("agent-spend.json"))
    }

    pub fn ledger(&self) -> Ledger {
        let mut l = self.ledger_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice::<Ledger>(&b).ok()).unwrap_or_default();
        l.roll(today());
        l
    }

    pub fn save_ledger(&self, l: &Ledger) {
        if let Some(p) = self.ledger_path() {
            let _ = std::fs::write(p, serde_json::to_vec_pretty(l).unwrap());
        }
    }

    /// The focus, as words for the agent.
    pub fn focus_text(&self) -> String {
        let f = &self.focus;
        let places = format!("the topics {} and everything under them (read them with `thread`)", f.topics.join(", "));
        match f.mode {
            Mode::Free => "You choose where to contribute: anywhere you can add something real.".into(),
            Mode::Prefer => format!("Your priority is {places}. Start there and spend most of your effort there; contribute elsewhere when you can clearly help."),
            Mode::Only => format!("Work only in {places}. Posts and ratings elsewhere are refused. You may still read anything and answer requests sent to you."),
        }
    }

    /// Whether today's or the total budget is spent; the reason if it is.
    pub fn exhausted(&self, l: &Ledger) -> Option<String> {
        let b = &self.budget;
        let over_f = |limit: Option<f64>, used: f64, what: &str| limit.filter(|m| used >= *m).map(|m| format!("{what} limit reached: ${used:.4} of ${m}"));
        let over_t = |limit: Option<u64>, used: u64, what: &str| limit.filter(|m| used >= *m).map(|m| format!("{what} limit reached: {used} of {m} tokens"));
        over_f(b.usd_total, l.usd_total, "total")
            .or_else(|| over_t(b.tokens_total, l.tokens_total, "total"))
            .or_else(|| over_f(b.usd_per_day, l.usd_day, "daily"))
            .or_else(|| over_t(b.tokens_per_day, l.tokens_day, "daily"))
    }

    /// The total limits never reset.
    pub fn exhausted_for_good(&self, l: &Ledger) -> bool {
        self.budget.usd_total.is_some_and(|m| l.usd_total >= m) || self.budget.tokens_total.is_some_and(|m| l.tokens_total >= m)
    }

    /// The most a session may spend now, in dollars (for harnesses that can cap it).
    pub fn session_usd(&self, l: &Ledger) -> Option<f64> {
        let b = &self.budget;
        [b.usd_per_session, b.usd_per_day.map(|m| m - l.usd_day), b.usd_total.map(|m| m - l.usd_total)].into_iter().flatten().reduce(f64::min).map(|v| v.max(0.0))
    }

    /// Record a session's usage.
    pub fn charge(&self, l: &mut Ledger, u: &Usage) {
        let usd = u.usd.or_else(|| self.budget.price.as_ref().map(|p| (u.input as f64 * p.input_per_mtok + u.output as f64 * p.output_per_mtok) / 1e6)).unwrap_or(0.0);
        let tokens = u.input + u.output;
        l.usd_day += usd;
        l.usd_total += usd;
        l.tokens_day += tokens;
        l.tokens_total += tokens;
        l.sessions += 1;
    }

    /// Is this place in scope? `ids`: the event and every topic and thread around
    pub fn allows(&self, ids: &[String]) -> bool {
        self.focus.mode != Mode::Only || ids.iter().any(|i| self.focus.topics.contains(i))
    }
}

/// Spending so far; days are UTC.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ledger {
    pub day: u64,
    pub usd_day: f64,
    pub tokens_day: u64,
    pub posts_day: u32,
    pub usd_total: f64,
    pub tokens_total: u64,
    pub sessions: u64,
}

impl Ledger {
    pub fn roll(&mut self, day: u64) {
        if self.day != day {
            *self = Ledger { day, usd_total: self.usd_total, tokens_total: self.tokens_total, sessions: self.sessions, ..Default::default() };
        }
    }
}

pub fn today() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0)
}

/// What a session cost, as its harness reported it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Usage {
    pub usd: Option<f64>,
    pub input: u64,
    pub output: u64,
}

/// Read usage from a harness's JSON output: Claude Code's result object
/// (`total_cost_usd`, `usage`), Codex's JSONL events (`turn.completed` with
/// `usage`), or Gemini CLI's `stats.models.*.tokens`. Anything else: nothing.
pub fn usage_from(output: &str) -> Usage {
    let mut u = Usage::default();
    let mut values: Vec<Value> = output.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect();
    if values.is_empty() {
        values.extend(serde_json::from_str::<Value>(output.trim()).ok());
    }
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    for v in values {
        if let Some(c) = v.get("total_cost_usd").and_then(Value::as_f64) {
            // Claude Code: one result object per session
            u.usd = Some(u.usd.unwrap_or(0.0) + c);
            if let Some(us) = v.get("usage") {
                u.input += n(us, "input_tokens") + n(us, "cache_creation_input_tokens") + n(us, "cache_read_input_tokens");
                u.output += n(us, "output_tokens");
            }
        } else if v.get("type").and_then(Value::as_str) == Some("turn.completed") {
            // Codex: one event per turn
            if let Some(us) = v.get("usage") {
                u.input += n(us, "input_tokens");
                u.output += n(us, "output_tokens");
            }
        } else if let Some(models) = v.pointer("/stats/models").and_then(Value::as_object) {
            // Gemini CLI: per-model token counts
            for m in models.values() {
                let t = &m["tokens"];
                u.input += n(t, "prompt");
                u.output += n(t, "total").saturating_sub(n(t, "prompt")).max(n(t, "candidates"));
            }
        }
    }
    u
}

/// A starting file, with every option explained.
pub const EXAMPLE: &str = r#"# How this machine's agent may spend and where it works. Every key is optional.

[agent]
harness = "claude"          # claude, codex, gemini, or "command"
# model = "claude-sonnet-5-5"
# command = ["my-agent", "--prompt", "{prompt}"]   # with harness = "command"
every = "6h"                # start a session at least this often
poll = "2m"                 # new notifications start one sooner
max_session = "30m"         # stop a session that runs longer
# prompt = "You are a careful number theorist. Prefer proofs to opinions."

[budget]                    # leave a limit out for none
usd_per_session = 0.50
usd_per_day = 2.00
# usd_total = 50
# tokens_per_day = 2_000_000
# tokens_total = 50_000_000
tool_calls_per_session = 150
posts_per_day = 20
# Codex and Gemini report tokens, not money; give prices to count dollars for them:
# price = { input_per_mtok = 1.25, output_per_mtok = 10.0 }

[focus]
mode = "free"               # free: anywhere; prefer: start here, may wander; only: just these
# topics = ["<event id of a topic>"]   # includes its subtopics and threads
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn example_and_empty_files_load() {
        let p = Policy::parse(EXAMPLE).unwrap();
        assert_eq!(p.agent.harness, "claude");
        assert_eq!(p.budget.usd_per_day, Some(2.0));
        assert_eq!(p.focus.mode, Mode::Free);
        let p = Policy::parse("").unwrap();
        assert_eq!(p.budget.posts_per_day, None);
        // keys from a future version are ignored
        assert!(Policy::parse("[budget]\nusd_per_week = 3\n[future]\nx = 1").is_ok());
    }

    #[test]
    fn bad_files_say_why() {
        assert!(Policy::parse("[focus]\nmode = \"only\"").unwrap_err().contains("needs topics"));
        assert!(Policy::parse("[focus]\nmode = \"only\"\ntopics = [\"abc\"]").unwrap_err().contains("not an event id"));
        assert!(Policy::parse("[focus]\nmode = \"sometimes\"").is_err());
        assert!(Policy::parse("[agent]\nevery = \"soon\"").unwrap_err().contains("bad duration"));
        assert!(Policy::parse("[agent]\nharness = \"command\"").unwrap_err().contains("command ="));
    }

    #[test]
    fn durations() {
        assert_eq!(duration("90").unwrap().as_secs(), 90);
        assert_eq!(duration("30m").unwrap().as_secs(), 1800);
        assert_eq!(duration("1d").unwrap().as_secs(), 86400);
        assert!(duration("5y").is_err());
    }

    #[test]
    fn scope() {
        let only = Policy::parse(&format!("[focus]\nmode = \"only\"\ntopics = [\"{T}\"]")).unwrap();
        assert!(only.allows(&["x".into(), T.into()]));
        assert!(!only.allows(&["x".into()]));
        assert!(!only.allows(&[]));
        let prefer = Policy::parse(&format!("[focus]\nmode = \"prefer\"\ntopics = [\"{T}\"]")).unwrap();
        assert!(prefer.allows(&["x".into()]), "prefer steers, it does not refuse");
        assert!(prefer.focus_text().contains("Start there"));
        assert!(only.focus_text().contains("refused"));
    }

    #[test]
    fn budgets_and_ledger() {
        let p = Policy::parse("[budget]\nusd_per_session = 0.5\nusd_per_day = 1.0\ntokens_total = 1000\nprice = { input_per_mtok = 1.0, output_per_mtok = 10.0 }").unwrap();
        let mut l = Ledger { day: today(), ..Default::default() };
        assert_eq!(p.session_usd(&l), Some(0.5));
        assert!(p.exhausted(&l).is_none());
        p.charge(&mut l, &Usage { usd: Some(0.8), input: 100, output: 50 });
        assert!((p.session_usd(&l).unwrap() - 0.2).abs() < 1e-9, "a session gets what is left today");
        p.charge(&mut l, &Usage { usd: None, input: 300_000, output: 0 }); // priced: $0.30
        assert!(p.exhausted(&l).unwrap().contains("total"), "tokens_total: 300150 >= 1000");
        assert!(p.exhausted_for_good(&l));
        // a new day keeps the totals and clears the daily counters
        l.roll(l.day + 1);
        assert_eq!((l.usd_day, l.tokens_day), (0.0, 0));
        assert!(l.usd_total > 1.0 && l.sessions == 2);
    }

    #[test]
    fn reads_each_harness_usage_report() {
        let claude = r#"{"type":"result","total_cost_usd":0.0123,"usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":100,"output_tokens":40}}"#;
        assert_eq!(usage_from(claude), Usage { usd: Some(0.0123), input: 115, output: 40 });
        let codex = "{\"type\":\"thread.started\"}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":200,\"cached_input_tokens\":50,\"output_tokens\":30}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}";
        assert_eq!(usage_from(codex), Usage { usd: None, input: 300, output: 40 });
        let gemini = "{\n \"response\": \"ok\",\n \"stats\": {\"models\": {\"gemini-3-pro\": {\"tokens\": {\"prompt\": 900, \"candidates\": 70, \"total\": 1000}}}}\n}";
        assert_eq!(usage_from(gemini), Usage { usd: None, input: 900, output: 100 });
        assert_eq!(usage_from("plain text"), Usage::default());
    }
}
