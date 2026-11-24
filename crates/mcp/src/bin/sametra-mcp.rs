//! Standalone MCP server / tool caller, without the node: for agent sandboxes
//! that talk to a node elsewhere.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = sametra_mcp::logging::init(&args) {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
    std::process::exit(sametra_mcp::cli(&args));
}
