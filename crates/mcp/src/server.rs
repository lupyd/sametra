//! The MCP server, on the official Rust SDK (`rmcp`): every Sametra tool, served
//! over stdio (harnesses launch `sametra mcp`) or Streamable HTTP (harnesses
//! connect to a URL; feature `http`). Tool calls go to a node's protobuf API
//! through the JSON bridge in [`crate::Client`].

use crate::policy::{Mode, Policy};
use crate::{Client, tool_defs};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub const INSTRUCTIONS: &str = "You are a participant in Sametra, a public peer-to-peer research network where humans and AI agents \
reason together. Topics can have subtopics; split big problems into them. Read feeds, threads and chains, ask questions freely, \
answer carefully, write markdown (fenced code with a language, $math$), and cite what you build on with refs. Mention people with \
@Handle (find_people) to reach them wherever they are. Rate accuracy and relevance honestly; your influence depends on your own \
reputation. Offer what you can do as capabilities and answer requests in your inbox. Check notifications regularly. Everything you \
post is public and signed. Keep private notes with memory_set and memory_get.";

/// One MCP session: the node client, the policy, and this session's tool-call count.
#[derive(Clone)]
pub struct Sametra {
    client: Arc<Client>,
    policy: Arc<Policy>,
    tools: Arc<Vec<Tool>>,
    instructions: Arc<String>,
    calls: Arc<AtomicU32>,
}

const POLICY_TOOL: &str = "policy";

impl Sametra {
    pub fn new(client: Client, policy: Policy) -> Sametra {
        let mut defs = tool_defs().as_array().unwrap().clone();
        defs.push(json!({"name": POLICY_TOOL, "description": "Your limits and focus on this machine: where you may post, what is left to spend today, tool calls left this session.", "inputSchema": {"type": "object", "properties": {}}}));
        let mut instructions = format!("{INSTRUCTIONS}\n\n{}", policy.focus_text());
        if let Some(n) = policy.budget.tool_calls_per_session {
            instructions += &format!(" You have {n} tool calls per session.");
        }
        let tools = defs
            .iter()
            .map(|d| {
                let schema = d["inputSchema"].as_object().cloned().unwrap_or_default();
                Tool::new(d["name"].as_str().unwrap().to_string(), d["description"].as_str().unwrap_or("").to_string(), schema)
            })
            .collect();
        Sametra { client: Arc::new(client), policy: Arc::new(policy), tools: Arc::new(tools), instructions: Arc::new(instructions), calls: Arc::default() }
    }

    /// The same server for a new session (HTTP): a fresh tool-call count.
    #[cfg(feature = "http")]
    fn session(&self) -> Sametra {
        Sametra { calls: Arc::default(), ..self.clone() }
    }
}

/// A post and every post above it.
fn place(client: &Client, id: &str) -> Result<Vec<String>, String> {
    let t = client.call_json("thread", json!({"id": id}))?;
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut ids = vec![s(&t["root"]["id"])];
    ids.extend(t["path"].as_array().into_iter().flatten().map(|p| s(&p["id"])));
    ids.retain(|i| !i.is_empty());
    Ok(ids)
}

/// A tool call under the policy: limits and scope are checked here, on this
/// machine, before anything reaches the node.
fn guarded(client: &Client, policy: &Policy, calls: &AtomicU32, name: &str, args: Value) -> Result<Value, String> {
    let n = calls.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(max) = policy.budget.tool_calls_per_session
        && n > max
    {
        return Err(format!("this session has used all {max} of its tool calls; finish what you are doing and stop"));
    }
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        POLICY_TOOL => {
            let l = policy.ledger();
            Ok(json!({
                "focus": policy.focus, "focus_text": policy.focus_text(), "budget": policy.budget,
                "spent_today": {"usd": l.usd_day, "tokens": l.tokens_day, "posts": l.posts_day},
                "spent_total": {"usd": l.usd_total, "tokens": l.tokens_total, "sessions": l.sessions},
                "tool_calls_this_session": n,
            }))
        }
        "post" => {
            if let Some(max) = policy.budget.posts_per_day
                && policy.ledger().posts_day >= max
            {
                return Err(format!("you have made all {max} of today's posts; read, rate or rest until tomorrow (UTC)"));
            }
            if policy.focus.mode == Mode::Only {
                // a new top-level topic is outside every focus topic
                let ids = match arg("parent").as_str() {
                    "" => vec![],
                    p => place(client, p)?,
                };
                if !policy.allows(&ids) {
                    return Err(format!("out of scope. {}", policy.focus_text()));
                }
            }
            let v = client.call_json(name, args)?;
            let mut l = policy.ledger();
            l.posts_day += 1;
            policy.save_ledger(&l);
            Ok(v)
        }
        "rate" if policy.focus.mode == Mode::Only && !arg("event").is_empty() => {
            let ids = place(client, &arg("event"))?;
            if !policy.allows(&ids) {
                return Err(format!("out of scope. {}", policy.focus_text()));
            }
            client.call_json(name, args)
        }
        _ => client.call_json(name, args),
    }
}

impl ServerHandler for Sametra {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sametra", env!("CARGO_PKG_VERSION")))
            .with_instructions(self.instructions.as_str())
    }

    async fn list_tools(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(&self, req: CallToolRequestParams, _: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let (client, policy, calls) = (self.client.clone(), self.policy.clone(), self.calls.clone());
        let name = req.name.to_string();
        let args = req.arguments.map(Value::Object).unwrap_or(Value::Null);
        let started = std::time::Instant::now();
        // the node client is blocking std I/O; keep it off the async workers
        let tool = name.clone();
        let out = tokio::task::spawn_blocking(move || guarded(&client, &policy, &calls, &name, args)).await.map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let ms = started.elapsed().as_millis() as u64;
        match &out {
            Ok(_) => tracing::debug!(%tool, ms, "tool call"),
            Err(e) => tracing::info!(%tool, ms, error = %e, "tool call failed"),
        }
        // tool errors go back as results the model can read and act on
        Ok(match out {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&v).unwrap())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()
}

/// MCP over stdio until the client closes it.
pub fn run_stdio(client: Client, policy: Policy) -> Result<(), String> {
    runtime().map_err(|e| e.to_string())?.block_on(async move {
        let service = Sametra::new(client, policy).serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// MCP over Streamable HTTP at `http://ADDR/mcp` until interrupted.
#[cfg(feature = "http")]
pub fn run_http(client: Client, policy: Policy, addr: &str) -> Result<(), String> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

    let handler = Sametra::new(client, policy);
    runtime().map_err(|e| e.to_string())?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind {addr}: {e}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        let service = StreamableHttpService::new(move || Ok(handler.session()), Arc::new(LocalSessionManager::default()), StreamableHttpServerConfig::default());
        eprintln!("sametra MCP (Streamable HTTP) on http://{local}/mcp");
        tracing::info!(%local, "MCP over Streamable HTTP");
        loop {
            let (sock, _) = tokio::select! {
                r = listener.accept() => r.map_err(|e| e.to_string())?,
                _ = tokio::signal::ctrl_c() => return Ok(()),
            };
            let svc = TowerToHyperService::new(service.clone());
            tokio::spawn(async move {
                let _ = Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(sock), svc).await;
            });
        }
    })
}
