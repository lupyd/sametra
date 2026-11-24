//! Sametra transport: WebSocket connections over plain TCP (`ws://`) or TLS
//! (`wss://`, with the `tls` feature), server-side TLS for the node's port,
//! and the relay wire [`codec`].

pub mod codec;
#[cfg(feature = "tls")]
pub mod tls;

use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
pub use tokio_tungstenite::WebSocketStream;
pub use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

/// Per-connection limits for every WebSocket (inbound and outbound).
pub const MAX_MESSAGE: usize = 256 * 1024;

pub fn ws_config() -> WebSocketConfig {
    let mut c = WebSocketConfig::default();
    c.max_message_size = Some(MAX_MESSAGE);
    c.max_frame_size = Some(MAX_MESSAGE);
    c
}

/// Any byte stream a WebSocket can run over (plain TCP or TLS).
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type BoxIo = Box<dyn Io>;

/// A parsed `ws://` / `wss://` address.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub secure: bool,
    pub host: String,
    pub port: u16,
}

pub fn parse_url(url: &str) -> Result<Target, String> {
    let uri: tungstenite::http::Uri = url.parse().map_err(|e| format!("bad address {url}: {e}"))?;
    let secure = match uri.scheme_str() {
        Some("ws") => false,
        Some("wss") => true,
        _ => return Err(format!("bad address {url}: use ws:// or wss://")),
    };
    let host = uri.host().filter(|h| !h.is_empty()).ok_or_else(|| format!("bad address {url}: no host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    Ok(Target { secure, host, port: uri.port_u16().unwrap_or(if secure { 443 } else { 80 }) })
}

/// Opens outbound WebSocket connections; holds the TLS client configuration.
#[derive(Clone)]
pub struct Dialer {
    #[cfg(feature = "tls")]
    tls: std::sync::Arc<tls::rustls::ClientConfig>,
}

impl Dialer {
    /// `extra_ca`: PEM files with CA certificates to trust in addition to the
    /// bundled Mozilla roots (private networks, self-signed nodes).
    pub fn new(extra_ca: &[PathBuf]) -> Result<Dialer, String> {
        #[cfg(feature = "tls")]
        return Ok(Dialer { tls: tls::client_config(extra_ca)? });
        #[cfg(not(feature = "tls"))]
        {
            if !extra_ca.is_empty() {
                return Err("this build has no TLS support (feature `tls`)".into());
            }
            Ok(Dialer {})
        }
    }

    pub async fn connect(&self, url: &str) -> Result<WebSocketStream<BoxIo>, String> {
        let t = parse_url(url)?;
        let tcp = TcpStream::connect((t.host.as_str(), t.port)).await.map_err(|e| format!("{url}: {e}"))?;
        let _ = tcp.set_nodelay(true);
        let io: BoxIo = if t.secure { self.secure(&t.host, tcp).await? } else { Box::new(tcp) };
        let (ws, _) = tokio_tungstenite::client_async_with_config(url, io, Some(ws_config())).await.map_err(|e| format!("{url}: {e}"))?;
        Ok(ws)
    }

    #[cfg(feature = "tls")]
    async fn secure(&self, host: &str, tcp: TcpStream) -> Result<BoxIo, String> {
        Ok(Box::new(tls::connect(&self.tls, host, tcp).await?))
    }
    #[cfg(not(feature = "tls"))]
    async fn secure(&self, _: &str, _: TcpStream) -> Result<BoxIo, String> {
        Err("wss:// needs a build with the `tls` feature".into())
    }
}

/// Finish a server-side WebSocket whose HTTP upgrade was already answered;
/// `part` holds any bytes read past the request head.
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(stream: S, part: Vec<u8>) -> WebSocketStream<S> {
    WebSocketStream::from_partially_read(stream, part, Role::Server, Some(ws_config())).await
}

/// Does the connection open with a TLS ClientHello? (Lets one port serve
/// plain HTTP/ws and HTTPS/wss.) Waits for the first byte without consuming it.
pub async fn starts_with_tls(sock: &TcpStream) -> std::io::Result<bool> {
    let mut b = [0u8; 1];
    Ok(sock.peek(&mut b).await? == 1 && b[0] == 0x16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let t = |s: &str| parse_url(s).unwrap();
        assert_eq!(t("ws://a.example:42424"), Target { secure: false, host: "a.example".into(), port: 42424 });
        assert_eq!(t("wss://relay.example"), Target { secure: true, host: "relay.example".into(), port: 443 });
        assert_eq!(t("ws://host/path?x"), Target { secure: false, host: "host".into(), port: 80 });
        assert_eq!(t("wss://[::1]:9"), Target { secure: true, host: "::1".into(), port: 9 });
        for bad in ["http://x", "x:1", "ws://", "", "wss:///p"] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn connect_errors_are_descriptive() {
        let d = Dialer::new(&[]).unwrap();
        let e = d.connect("ws://127.0.0.1:1").await.err().unwrap();
        assert!(e.starts_with("ws://127.0.0.1:1"), "{e}");
        assert!(d.connect("ftp://x").await.err().unwrap().contains("ws://"));
    }
}
