//! Sametra node: relay, gossip, store, reputation, the protobuf tool API and the
//! embedded web app. The `sametra` binary (src/main.rs) is a thin CLI over it.
//!
//! Crates: `sametra-proto` (schema, events), `sametra-transport` (ws/wss, TLS,
//! wire codec), `sametra-mcp` (JSON bridge), and this one.

pub use sametra_proto::{b64, event, pb};
pub use sametra_transport as transport;

pub mod api;
pub mod body;
pub mod embed;
pub mod http;
pub mod files;
pub mod index;
#[cfg(feature = "ipfs")]
pub mod ipfs;
pub mod node;
pub mod p2p;
pub mod rep;
pub mod schema;
pub mod store;
