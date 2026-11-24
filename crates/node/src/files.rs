//! Files: links to IPFS content in posts, and NIP-94 file metadata events
//! (kind 1063) that tie each file to the identity that added it. Always built;
//! talking to an IPFS daemon is `crate::ipfs` (feature `ipfs`).

use crate::event::{Event, K_FILE};

/// `CID`, `ipfs://CID/path` or `/ipfs/CID/path` -> (`CID`, `/ipfs/CID/path`).
pub fn parse_path(s: &str) -> Result<(String, String), String> {
    let s = s.trim();
    let rest = s.strip_prefix("ipfs://").or_else(|| s.strip_prefix("/ipfs/")).unwrap_or(s);
    let (cid, sub) = rest.split_once('/').map(|(c, p)| (c, format!("/{p}"))).unwrap_or((rest, String::new()));
    if !is_cid(cid) || sub.contains("..") || sub.chars().any(|c| c.is_control() || c == '?' || c == '#') {
        return Err(format!("not an IPFS path: '{s}'"));
    }
    Ok((cid.to_string(), format!("/ipfs/{cid}{sub}")))
}

/// CIDv0 (`Qm…`, base58) or CIDv1 in base32 (`b…`), the forms Kubo prints.
pub fn is_cid(s: &str) -> bool {
    const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    (s.len() == 46 && s.starts_with("Qm") && s.chars().all(|c| B58.contains(c)))
        || (s.len() >= 50 && s.len() <= 120 && s.starts_with('b') && s.chars().all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)))
}

/// The CIDs that a text links to as `ipfs://CID` (or a `/ipfs/CID` gateway path).
pub fn linked_cids(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for marker in ["ipfs://", "/ipfs/"] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            let c = &rest[..end];
            if is_cid(c) && !out.iter().any(|x| x == c) && out.len() < 16 {
                out.push(c.to_string());
            }
        }
    }
    out
}

/// A content type for a file, from its name, else its first bytes. Never HTML:
/// files are other people's content served from the node's own origin.
pub fn content_type(name: &str, head: &[u8]) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let by_ext = match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "pdf" => Some("application/pdf"),
        "json" => Some("application/json"),
        "csv" => Some("text/csv; charset=utf-8"),
        "md" | "markdown" => Some("text/markdown; charset=utf-8"),
        "txt" | "html" | "htm" | "tex" | "lean" | "py" | "rs" | "js" | "ts" => Some("text/plain; charset=utf-8"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mp3" => Some("audio/mpeg"),
        "wasm" => Some("application/wasm"),
        _ => None,
    };
    by_ext.unwrap_or(if head.starts_with(b"\x89PNG") {
        "image/png"
    } else if head.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if head.starts_with(b"GIF8") {
        "image/gif"
    } else if head.starts_with(b"%PDF") {
        "application/pdf"
    } else if head.len() > 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        "image/webp"
    } else if std::str::from_utf8(&head[..head.len().min(512)]).is_ok() || head.len() >= 512 && std::str::from_utf8(&head[..508]).is_ok() {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    })
}

/// A NIP-94 file metadata event's tags, for a file added as `ipfs://cid`.
pub fn nip94_tags(cid: &str, name: &str, media_type: &str, sha256_hex: &str, size: u64, fallback: Option<&str>) -> Vec<Vec<String>> {
    let t = |k: &str, v: &str| vec![k.to_string(), v.to_string()];
    let mut tags = vec![t("url", &format!("ipfs://{cid}")), t("m", media_type), t("x", sha256_hex), t("size", &size.to_string())];
    if !name.is_empty() {
        tags.push(t("alt", name));
    }
    if let Some(f) = fallback {
        tags.push(t("fallback", &format!("{}/ipfs/{cid}", f.trim_end_matches('/'))));
    }
    tags
}

/// The CID a NIP-94 event describes, if it is an IPFS file.
pub fn nip94_cid(e: &Event) -> Option<&str> {
    (e.kind == K_FILE).then(|| e.tag("url")).flatten().and_then(|u| u.strip_prefix("ipfs://")).map(|c| c.split('/').next().unwrap_or(c)).filter(|c| is_cid(c))
}

/// NIP-92 `imeta` tag for a post that links to a described file.
pub fn imeta(meta: &Event) -> Option<Vec<String>> {
    let cid = nip94_cid(meta)?;
    let mut t = vec!["imeta".to_string(), format!("url ipfs://{cid}")];
    for k in ["m", "x", "size", "alt"] {
        if let Some(v) = meta.tag(k) {
            t.push(format!("{k} {v}"));
        }
    }
    Some(t)
}

/// `ws://host:port` or `wss://host` (a node's public address) -> its HTTP base.
pub fn http_base(public: &str) -> Option<String> {
    public.strip_prefix("wss://").map(|r| format!("https://{}", r.trim_end_matches('/'))).or_else(|| public.strip_prefix("ws://").map(|r| format!("http://{}", r.trim_end_matches('/'))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "bafkreihkavvxs6g6cxquciaqbiih52zd6fom6azpioa6a3fanh5kja6hni";
    const V0: &str = "QmS4ustL54uo8FzR9455qaxZwuMiUhyvMcX9Ba8nUH4uVv";

    #[test]
    fn paths_and_links() {
        assert_eq!(parse_path(CID).unwrap(), (CID.to_string(), format!("/ipfs/{CID}")));
        assert_eq!(parse_path(&format!("ipfs://{CID}/a/b.png")).unwrap().1, format!("/ipfs/{CID}/a/b.png"));
        assert_eq!(parse_path(&format!("/ipfs/{V0}/readme")).unwrap().0, V0);
        for bad in ["ipfs://nope".to_string(), format!("ipfs://{CID}/../x"), format!("{CID}/a?b"), "QmO0000000000000000000000000000000000000000000".to_string()] {
            assert!(parse_path(&bad).is_err(), "{bad}");
        }
        let text = format!("see ![fig](ipfs://{CID}) and [again](/ipfs/{CID}/x) and ipfs://junk and https://x/ipfs/{V0}");
        assert_eq!(linked_cids(&text), [CID, V0]);
    }

    #[test]
    fn content_types_never_html() {
        assert_eq!(content_type("page.html", b"<html><script>"), "text/plain; charset=utf-8");
        assert_eq!(content_type("", b"<html><script>alert(1)</script>"), "text/plain; charset=utf-8");
        assert_eq!(content_type("", b"\x89PNG\r\n"), "image/png");
        assert_eq!(content_type("f.svg", b"<svg"), "image/svg+xml");
        assert_eq!(content_type("", &[0, 159, 146, 150]), "application/octet-stream");
    }
}

#[cfg(test)]
mod nip94 {
    use super::*;
    use crate::event::Keys;

    #[test]
    fn metadata_round_trip() {
        let cid = "bafkreihkavvxs6g6cxquciaqbiih52zd6fom6azpioa6a3fanh5kja6hni";
        let tags = nip94_tags(cid, "fig.png", "image/png", &"ab".repeat(32), 42, http_base("wss://node.example").as_deref());
        assert!(tags.contains(&vec!["fallback".to_string(), format!("https://node.example/ipfs/{cid}")]));
        let e = Keys::generate().sign(K_FILE, tags, String::new());
        assert_eq!(nip94_cid(&e), Some(cid));
        let im = imeta(&e).unwrap();
        assert_eq!(im[..3], ["imeta".to_string(), format!("url ipfs://{cid}"), "m image/png".into()]);
        assert!(im.contains(&"alt fig.png".to_string()) && im.contains(&"size 42".to_string()));
        assert_eq!(http_base("ws://1.2.3.4:42424/").as_deref(), Some("http://1.2.3.4:42424"));
    }
}
