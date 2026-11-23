//! Embeds the built web app (web/dist) into the node binary.
use std::path::{Path, PathBuf};

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    embed_web(&out);
}

/// Embed web/dist (built by `pnpm build`) as a static file table.
fn embed_web(out: &Path) {
    let dist = Path::new("../../web/dist");
    println!("cargo:rerun-if-changed=../../web/dist");
    let mut files = vec![];
    if dist.is_dir() {
        walk(dist, dist, &mut files);
    }
    files.sort();
    let mut s = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        println!("cargo:rerun-if-changed={}", abs.display());
        s.push_str(&format!("    ({:?}, include_bytes!({:?})),\n", rel, abs.canonicalize().unwrap()));
    }
    s.push_str("];\n");
    std::fs::write(out.join("web_files.rs"), s).unwrap();
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            println!("cargo:rerun-if-changed={}", p.display());
            walk(root, &p, out);
        } else {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((format!("/{rel}"), p));
        }
    }
}
