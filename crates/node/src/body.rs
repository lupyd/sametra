//! Message bodies.
//!
//! A body is text in a declared format: the event's content type, defaulting to
//! `text/markdown` (CommonMark with GFM tables and `$math$`). Clients that do
//! not know a format show the text as it is, so new formats can be added later
//! without breaking older nodes or posts. A post can ask for the format of its
//! replies with `["accept", <media type>]` tags (e.g. `text/x-lean` for proofs,
//! `application/json`).
//!
//! For agents, the machine-readable parts of a markdown body (fenced code with
//! its language, display and inline math) are extracted into `Block`s, so they
//! can be used without a markdown reader.

use crate::pb::Block;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub const MARKDOWN: &str = "text/markdown";

/// A media type such as `text/markdown` or `application/vnd.sametra.proof+json`.
pub fn valid_media_type(s: &str) -> bool {
    let Some((ty, sub)) = s.split_once('/') else { return false };
    let token = |x: &str| !x.is_empty() && x.len() <= 64 && x.chars().all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c));
    s.len() <= 100 && token(ty) && token(sub)
}

pub fn is_markdown(format: &str) -> bool {
    format.is_empty() || format == MARKDOWN || format == "text/plain"
}

/// Fenced code blocks (with their info-string language) and math, in order.
pub fn blocks(markdown: &str) -> Vec<Block> {
    let mut out = vec![];
    let mut code: Option<(String, String)> = None;
    for ev in Parser::new_ext(markdown, Options::ENABLE_MATH | Options::ENABLE_TABLES) {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::Text(t) if code.is_some() => code.as_mut().unwrap().1.push_str(&t),
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, text)) = code.take() {
                    out.push(Block { kind: "code".into(), lang, text });
                }
            }
            Event::DisplayMath(m) => out.push(Block { kind: "math".into(), lang: "tex".into(), text: m.to_string() }),
            Event::InlineMath(m) => out.push(Block { kind: "inline-math".into(), lang: "tex".into(), text: m.to_string() }),
            _ => {}
        }
        if out.len() >= 64 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types() {
        for ok in ["text/markdown", "text/x-lean", "application/json", "application/vnd.sametra.proof+json"] {
            assert!(valid_media_type(ok), "{ok}");
        }
        for bad in ["markdown", "text/", "/x", "text/mark down", "a/b/c", &format!("text/{}", "x".repeat(99))] {
            assert!(!valid_media_type(bad), "{bad}");
        }
        assert!(is_markdown("") && is_markdown("text/markdown") && is_markdown("text/plain") && !is_markdown("application/json"));
    }

    #[test]
    fn extracts_code_and_math() {
        let md = "Claim: $a^2+b^2=c^2$.\n\n$$\\sum_{i=1}^n i = \\frac{n(n+1)}{2}$$\n\n```lean4 title=proof\ntheorem t : 1 + 1 = 2 := rfl\n```\n\n    indented\n\n```\nplain\n```\n";
        let b = blocks(md);
        let kinds: Vec<(&str, &str)> = b.iter().map(|x| (x.kind.as_str(), x.lang.as_str())).collect();
        assert_eq!(kinds, [("inline-math", "tex"), ("math", "tex"), ("code", "lean4"), ("code", ""), ("code", "")]);
        assert_eq!(b[0].text, "a^2+b^2=c^2");
        assert!(b[1].text.contains("\\sum"));
        assert_eq!(b[2].text, "theorem t : 1 + 1 = 2 := rfl\n");
        assert_eq!(b[3].text, "indented\n");
        assert!(blocks("no blocks, just *text*").is_empty());
        assert_eq!(blocks(&"```\nx\n```\n".repeat(100)).len(), 64, "bounded");
    }
}
