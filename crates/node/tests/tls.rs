//! TLS: one port serving plain and encrypted connections, wss:// gossip between
//! nodes, certificate trust, and the JSON bridge over https://.
#![cfg(feature = "tls")]
mod common;
use common::*;
use futures_util::{SinkExt, StreamExt};
use sametra_node::event::{Filter, Keys};
use sametra_node::node::{Config, Node};
use sametra_node::pb;
use sametra_node::transport::{Dialer, Message};
use std::path::PathBuf;
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

/// A self-signed certificate for localhost / 127.0.0.1, written as PEM files.
struct Cert {
    dir: TempDir,
}
impl Cert {
    fn new() -> Cert {
        let dir = tempdir("cert");
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        std::fs::write(dir.0.join("cert.pem"), ck.cert.pem()).unwrap();
        std::fs::write(dir.0.join("key.pem"), ck.signing_key.serialize_pem()).unwrap();
        Cert { dir }
    }
    fn cert(&self) -> PathBuf {
        self.dir.0.join("cert.pem")
    }
    fn key(&self) -> PathBuf {
        self.dir.0.join("key.pem")
    }
}

async fn tls_node(c: &Cert) -> TestNode {
    let (cert, key) = (c.cert(), c.key());
    spawn(move |cfg| {
        cfg.tls_cert = Some(cert);
        cfg.tls_key = Some(key);
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_nodes_talk_to_each_other_over_quic() {
    // HTTPS is for browsers and clients; node to node traffic is libp2p over
    // QUIC (TLS 1.3 with the nodes' own keys), whatever the HTTP side does
    let cert = Cert::new();
    let a = tls_node(&cert).await;
    let b = spawn(|_| {}).await;
    b.connect(&a).await;
    let from_a: pb::Ack = a.ok("post", &post("over tls", "x")).await;
    assert!(eventually(T, || b.has(&from_a.id)).await);
    let from_b: pb::Ack = b.ok("post", &post("back over tls", "x")).await;
    assert!(eventually(T, || a.has(&from_b.id)).await);
    let peers: pb::PeersResponse = b.ok("peers", &pb::Empty {}).await;
    assert!(peers.peers[0].addr.starts_with("/ip4/127.0.0.1/udp/") && peers.peers[0].addr.contains("/quic-v1/p2p/"), "{}", peers.peers[0].addr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_port_serves_plain_and_tls() {
    let cert = Cert::new();
    let n = tls_node(&cert).await;
    let port = n.addr.port();
    // plain HTTP still works on the same port (local tools, reverse proxies)
    let plain = n.get("/").await;
    assert_eq!(plain.status, 200);
    // HTTPS: same content
    let r = n.raw_tls(&cert.cert(), format!("GET /c/math HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").as_bytes()).await.unwrap();
    assert_eq!((r.status, r.text()), (200, plain.text()));
    // the API over HTTPS, with the browser's https:// origin
    let body = pb::encode(&post("via https", "x"));
    let req = |origin: &str| {
        let mut v = format!("POST /api/post HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: {origin}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(&body);
        v
    };
    let ok = n.raw_tls(&cert.cert(), &req(&format!("https://localhost:{port}"))).await.unwrap();
    assert_eq!(ok.status, 200);
    let a: pb::Ack = pb::decode(&ok.body).unwrap();
    assert!(n.has(&a.id));
    let refused = n.raw_tls(&cert.cert(), &req(&format!("http://localhost:{port}"))).await.unwrap();
    assert_eq!(refused.status, 403, "an http:// page cannot write to the https:// origin");
    // a relay client over wss://: binary frames
    let req = |sid: &str, f: Filter| Message::binary(pb::encode(&pb::Frame { type_pb: pb::F_REQ, sub_id: sid.into(), filters: vec![f.to_pb()], ..Default::default() }));
    let mut ws = Dialer::new(&[cert.cert()]).unwrap().connect(&format!("wss://localhost:{port}")).await.unwrap();
    ws.send(req("s", Filter { kinds: Some(vec![sametra_node::event::K_POST]), ..Default::default() })).await.unwrap();
    let mut events = 0;
    loop {
        let m = tokio::time::timeout(T, ws.next()).await.unwrap().unwrap().unwrap();
        let Message::Binary(b) = m else { continue }; // keepalive pings
        let f: pb::Frame = pb::decode(&b).unwrap();
        if f.type_pb == pb::F_EOSE {
            break;
        }
        events += 1;
    }
    assert_eq!(events, 1);
    // and plain ws:// on the same port
    let mut plain_ws = Dialer::new(&[]).unwrap().connect(&n.ws).await.unwrap();
    plain_ws.send(req("s", Filter { kinds: Some(vec![1]), ..Default::default() })).await.unwrap();
    let f = loop {
        let m = tokio::time::timeout(T, plain_ws.next()).await.unwrap().unwrap().unwrap();
        if let Message::Binary(b) = m {
            break pb::decode::<pb::Frame>(&b).unwrap();
        }
    };
    assert_eq!(f.type_pb, pb::F_EOSE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn untrusted_certificates_are_refused() {
    let cert = Cert::new();
    let a = tls_node(&cert).await;
    let url = format!("wss://localhost:{}", a.addr.port());
    let e = Dialer::new(&[]).unwrap().connect(&url).await.err().unwrap();
    assert!(e.contains("TLS") && e.to_lowercase().contains("certificate"), "{e}");
    // the wrong name is refused too, even with the CA trusted
    let other = Cert::new();
    let e = Dialer::new(&[other.cert()]).unwrap().connect(&url).await.err().unwrap();
    assert!(e.contains("TLS"), "{e}");
}

#[test]
fn tls_configuration_errors() {
    let cert = Cert::new();
    let dir = tempdir("tlscfg");
    let mk = |f: &dyn Fn(&mut Config)| {
        let mut c = Config { data: dir.0.clone(), ..Default::default() };
        f(&mut c);
        Node::new(c, Keys::generate()).err().map(|e| e.to_string())
    };
    assert!(mk(&|c| c.tls_cert = Some(cert.cert())).unwrap().contains("both"));
    assert!(mk(&|c| {
        c.tls_cert = Some(cert.key());
        c.tls_key = Some(cert.key());
    })
    .is_some(), "a key is not a certificate");
    assert!(mk(&|c| {
        c.tls_cert = Some(cert.cert());
        c.tls_key = Some(cert.key());
    })
    .is_none());
}
