//! Standalone MCP server / tool caller, without the node: for agent sandboxes
//! that talk to a node elsewhere.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sametra_mcp::cli(&args));
}
