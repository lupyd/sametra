//! Tool inputs are protobuf messages.
//!
//! A provider declares its tool's input as `.proto` source and the name of the
//! input message. The node compiles it (protox, pure Rust) into a
//! FileDescriptorSet that travels in the capability announcement as bytes.
//! Requests carry the input encoded as that message, and the requester's node
//! checks it against the descriptor before sending (prost-reflect). At the edges
//! (CLI, MCP, the web app) the input may be given as protobuf JSON, which the node
//! converts with the same descriptor; on the network it is always binary.

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};
use std::path::Path;

const NAME: &str = "input.proto";
const MAX_SOURCE: usize = 16 * 1024;

/// Resolves the provider's source as `input.proto`, and the well-known types.
struct Source(String);
impl FileResolver for Source {
    fn resolve_path(&self, path: &Path) -> Option<String> {
        (path == Path::new(NAME)).then(|| NAME.to_string())
    }
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        if name == NAME { File::from_source(NAME, &self.0) } else { Err(protox::Error::file_not_found(name)) }
    }
}

/// Compile `.proto` source; the encoded FileDescriptorSet (with imports) if it
/// declares `message`.
pub fn compile(source: &str, message: &str) -> Result<Vec<u8>, String> {
    if source.len() > MAX_SOURCE {
        return Err(format!("input_proto is over {MAX_SOURCE} bytes"));
    }
    let mut chain = ChainFileResolver::new();
    chain.add(Source(source.to_string()));
    chain.add(GoogleFileResolver::new());
    let mut c = protox::Compiler::with_file_resolver(chain);
    c.include_imports(true);
    c.open_file(NAME).map_err(|e| format!("input_proto: {e}"))?;
    let bytes = c.encode_file_descriptor_set();
    descriptor(&bytes, message)?;
    Ok(bytes)
}

/// The input message's descriptor from a capability's FileDescriptorSet.
pub fn descriptor(set: &[u8], message: &str) -> Result<MessageDescriptor, String> {
    let pool = DescriptorPool::decode(set).map_err(|e| format!("bad input descriptor: {e}"))?;
    pool.get_message_by_name(message.trim_start_matches('.')).ok_or_else(|| format!("input_proto declares no message '{message}'"))
}

/// Is `input` a valid encoding of the message?
pub fn check(set: &[u8], message: &str, input: &[u8]) -> Result<(), String> {
    DynamicMessage::decode(descriptor(set, message)?, input).map(|_| ()).map_err(|e| format!("input is not a valid {message}: {e}"))
}

/// Edge conversion: protobuf JSON -> the encoded message.
pub fn from_json(set: &[u8], message: &str, json: &str) -> Result<Vec<u8>, String> {
    let d = descriptor(set, message)?;
    let mut de = serde_json::Deserializer::from_str(json);
    let m = DynamicMessage::deserialize(d, &mut de).map_err(|e| format!("input_json is not a valid {message}: {e}"))?;
    de.end().map_err(|e| format!("input_json: {e}"))?;
    Ok(m.encode_to_vec())
}

/// For reading: the encoded message as protobuf JSON.
pub fn to_json(set: &[u8], message: &str, bytes: &[u8]) -> Result<String, String> {
    let m = DynamicMessage::decode(descriptor(set, message)?, bytes).map_err(|e| e.to_string())?;
    serde_json::to_string(&m).map_err(|e| e.to_string())
}

/// The media type naming a message, for event content.
pub fn media_type(message: &str) -> String {
    format!("application/x-protobuf; message={}", message.trim_start_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
        syntax = "proto3";
        package lab;
        import "google/protobuf/timestamp.proto";
        message Sample { string id = 1; double mass_mg = 2; repeated string assays = 3; google.protobuf.Timestamp taken = 4; }
    "#;

    #[test]
    fn compile_check_and_convert() {
        let set = compile(SRC, "lab.Sample").unwrap();
        let bytes = from_json(&set, "lab.Sample", r#"{"id": "s1", "massMg": 2.5, "assays": ["ms"], "taken": "2026-11-24T10:00:00Z"}"#).unwrap();
        assert!(check(&set, "lab.Sample", &bytes).is_ok());
        let back: serde_json::Value = serde_json::from_str(&to_json(&set, "lab.Sample", &bytes).unwrap()).unwrap();
        assert_eq!(back["massMg"], 2.5);
        assert_eq!(back["taken"], "2026-11-24T10:00:00Z");
        // wrong shapes are refused, at the edge and on the wire
        assert!(from_json(&set, "lab.Sample", r#"{"massMg": "heavy"}"#).unwrap_err().contains("not a valid lab.Sample"));
        assert!(from_json(&set, "lab.Sample", r#"{"nope": 1}"#).is_err());
        assert!(check(&set, "lab.Sample", &[0x12, 0x05]).is_err(), "truncated field");
        assert_eq!(media_type(".lab.Sample"), "application/x-protobuf; message=lab.Sample");
    }

    #[test]
    fn bad_sources_say_why() {
        assert!(compile("syntax = \"proto3\"; message A { strin x = 1; }", "A").unwrap_err().starts_with("input_proto:"));
        assert!(compile(SRC, "lab.Missing").unwrap_err().contains("no message 'lab.Missing'"));
        assert!(compile("syntax = \"proto3\"; import \"other.proto\";", "A").is_err());
        assert!(compile(&"/".repeat(MAX_SOURCE + 1), "A").is_err());
    }
}
