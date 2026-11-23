//! TLS for `wss://` peers and for serving HTTPS/WSS on the node's port.
//! rustls on the `ring` backend; server roots are the bundled Mozilla set.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpStream;
pub use tokio_rustls::{TlsAcceptor, TlsConnector};
pub use {rustls, tokio_rustls};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs: Vec<_> = CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no PEM certificates", path.display()));
    }
    Ok(certs)
}

/// Client config trusting the Mozilla roots plus `extra_ca` PEM files.
pub fn client_config(extra_ca: &[PathBuf]) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for p in extra_ca {
        for c in read_certs(p)? {
            roots.add(c).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let mut c = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(c))
}

/// Server side: certificate chain and private key, both PEM.
pub fn acceptor(cert: &Path, key: &Path) -> Result<TlsAcceptor, String> {
    let certs = read_certs(cert)?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{}: {e}", key.display()))?;
    let mut c = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS certificate/key: {e}"))?;
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(c)))
}

pub async fn connect(cfg: &Arc<ClientConfig>, host: &str, tcp: TcpStream) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| format!("{host}: {e}"))?;
    TlsConnector::from(cfg.clone()).connect(name, tcp).await.map_err(|e| format!("TLS with {host}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_files_are_reported() {
        let missing = PathBuf::from("/nonexistent/ca.pem");
        assert!(client_config(std::slice::from_ref(&missing)).unwrap_err().contains("ca.pem"));
        assert!(acceptor(&missing, &missing).is_err());
        let dir = std::env::temp_dir().join(format!("sametra-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "not a certificate").unwrap();
        assert!(client_config(std::slice::from_ref(&empty)).unwrap_err().contains("no PEM"));
        assert!(client_config(&[]).is_ok(), "bundled roots alone");
    }
}
