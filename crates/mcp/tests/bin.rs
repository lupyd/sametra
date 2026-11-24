//! The standalone `sametra-mcp` binary (no node inside).
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_sametra-mcp")).args(args).env_remove("SAMETRA_API").output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn help_tools_and_errors() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("--tls-ca") && stdout.contains("https://"));
    let (code, stdout, _) = run(&["tools"]);
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stdout).unwrap().as_array().unwrap().len(), sametra_mcp::tool_defs().as_array().unwrap().len());
    let (code, _, stderr) = run(&["call", "--api", "http://127.0.0.1:1", "info"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unreachable"));
    let (code, _, stderr) = run(&["call", "--api", "ftp://x", "info"]);
    assert!(code == 2 && stderr.contains("http://"));
}

#[test]
fn mcp_server_answers_without_a_node() {
    use std::io::{BufRead, BufReader, Write};
    let mut p = Command::new(env!("CARGO_BIN_EXE_sametra-mcp"))
        .args(["--api", "http://127.0.0.1:1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = p.stdin.take().unwrap();
    // a real MCP handshake (the server is the official rmcp SDK), then two calls
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":0,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"test","version":"1"}}}}}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"info","arguments":{{}}}}}}"#).unwrap();
    let mut out = BufReader::new(p.stdout.take().unwrap()).lines();
    let mut by_id = std::collections::HashMap::new();
    while by_id.len() < 3 {
        let v: serde_json::Value = serde_json::from_str(&out.next().unwrap().unwrap()).unwrap();
        by_id.insert(v["id"].to_string(), v);
    }
    drop(stdin);
    assert_eq!(by_id["0"]["result"]["serverInfo"]["name"], "sametra");
    assert_eq!(by_id["1"]["result"]["tools"].as_array().unwrap().len(), sametra_mcp::tool_defs().as_array().unwrap().len());
    assert_eq!(by_id["2"]["result"]["isError"], true, "node down is a tool error, not a crash");
    assert!(p.wait().unwrap().success(), "exits when the client closes stdin");
}
