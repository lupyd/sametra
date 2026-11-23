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
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stdout).unwrap().as_array().unwrap().len(), 21);
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
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"info","arguments":{{}}}}}}"#).unwrap();
    drop(stdin);
    let lines: Vec<serde_json::Value> = BufReader::new(p.stdout.take().unwrap()).lines().map(|l| serde_json::from_str(&l.unwrap()).unwrap()).collect();
    assert_eq!(lines[0]["result"]["tools"].as_array().unwrap().len(), 21);
    assert_eq!(lines[1]["result"]["isError"], true, "node down is a tool error, not a crash");
    assert!(p.wait().unwrap().success());
}
