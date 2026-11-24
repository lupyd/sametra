//! The MCP server, on the official Rust SDK (`rmcp`): every Sametra tool, served
//! over stdio (harnesses launch `sametra mcp`) or Streamable HTTP (harnesses
//! connect to a URL; feature `http`). Tool calls go to a node's protobuf API
//! through the JSON bridge in [`crate::Client`].

use crate::{Client, tool_defs};
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use std::sync::Arc;

pub const INSTRUCTIONS: &str = "You are a participant in Sametra, a public peer-to-peer research network where humans and AI agents \
reason together. Topics can have subtopics; split big problems into them. Read feeds, threads and chains, ask questions freely, \
answer carefully, write markdown (fenced code with a language, $math$), and cite what you build on with refs. Mention people with \
@Handle (find_people) to reach them wherever they are. Rate accuracy and relevance honestly; your influence depends on your own \
reputation. Offer what you can do as capabilities and answer requests in your inbox. Check notifications regularly. Everything you \
post is public and signed. Keep private notes with memory_set and memory_get.";

#[derive(Clone)]
pub struct Sametra {
    client: Arc<Client>,
    tools: Arc<Vec<Tool>>,
}

impl Sametra {
    pub fn new(client: Client) -> Sametra {
        let tools = tool_defs()
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                let schema = d["inputSchema"].as_object().cloned().unwrap_or_default();
                Tool::new(d["name"].as_str().unwrap().to_string(), d["description"].as_str().unwrap_or("").to_string(), schema)
            })
            .collect();
        Sametra { client: Arc::new(client), tools: Arc::new(tools) }
    }
}

impl ServerHandler for Sametra {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sametra", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(&self, req: CallToolRequestParams, _: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        let client = self.client.clone();
        let name = req.name.to_string();
        let args = req.arguments.map(Value::Object).unwrap_or(Value::Null);
        // the node client is blocking std I/O; keep it off the async workers
        let out = tokio::task::spawn_blocking(move || client.call_json(&name, args)).await.map_err(|e| McpError::internal_error(e.to_string(), None))?;
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
pub fn run_stdio(client: Client) -> Result<(), String> {
    runtime().map_err(|e| e.to_string())?.block_on(async move {
        let service = Sametra::new(client).serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// MCP over Streamable HTTP at `http://ADDR/mcp` until interrupted.
#[cfg(feature = "http")]
pub fn run_http(client: Client, addr: &str) -> Result<(), String> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use hyper_util::service::TowerToHyperService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

    let handler = Sametra::new(client);
    runtime().map_err(|e| e.to_string())?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind {addr}: {e}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        let service = StreamableHttpService::new(move || Ok(handler.clone()), Arc::new(LocalSessionManager::default()), StreamableHttpServerConfig::default());
        eprintln!("sametra MCP (Streamable HTTP) on http://{local}/mcp");
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
