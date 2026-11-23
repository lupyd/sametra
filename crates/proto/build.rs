//! Generates quick-protobuf types from the workspace schema (proto/sametra.proto).
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dir = PathBuf::from("../../proto");
    println!("cargo:rerun-if-changed=../../proto/sametra.proto");
    let raw = out.join("sametra_raw.rs");
    let cfg = pb_rs::ConfigBuilder::new(&[dir.join("sametra.proto")], Some(&raw), None, &[dir])
        .unwrap()
        .single_module(true)
        .dont_use_cow(true)
        .headers(false)
        .custom_struct_derive(vec!["serde::Serialize".into(), "serde::Deserialize".into()])
        .build();
    pb_rs::types::FileDescriptor::run(&cfg).unwrap();
    // `include!` can't take inner attributes; the JSON bridge wants missing fields = defaults.
    let src = std::fs::read_to_string(&raw).unwrap();
    let mut fixed = String::new();
    for line in src.lines() {
        if line.trim_start().starts_with("#!") {
            continue;
        }
        fixed.push_str(line);
        fixed.push('\n');
        if line.trim_start().starts_with("#[derive(") && line.contains("serde::Deserialize") {
            fixed.push_str("#[serde(default)]\n");
        }
    }
    std::fs::write(out.join("sametra.rs"), fixed).unwrap();
}
