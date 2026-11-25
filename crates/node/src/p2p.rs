//! Node to node networking, on libp2p.
//!
//! - **Transport**: QUIC on UDP, on the same port number as the HTTP/WebSocket
//!   listener (`--listen 0.0.0.0:42424` also listens on `udp/42424`). The libp2p
//!   identity is the node's own secp256k1 key, so its peer id and its npub are the
//!   same key.
//! - **Discovery**: Kademlia (`/sametra/kad/1.0.0`) over the peers we know, fed
//!   by identify; the node keeps dialing until it has `max_peers` connections.
//! - **Propagation**: GossipSub. Every event goes to `sametra/1/all` (relay-all
//!   nodes subscribe), small metadata and mentions to `sametra/1/meta` (everyone
//!   subscribes), and posts to `sametra/1/tree/<root>` (nodes that follow or take
//!   part in that tree subscribe). A node republishes what it stores on every
//!   topic the event belongs to, which bridges the topics; GossipSub drops the
//!   repeats (the message id is topic + event id).
//! - **Lookups**: request-response (`/sametra/req/1`): a `PeerRequest` of filters,
//!   answered from the peer's store; searches may be forwarded `hops` further.
//!
//! The WebSocket relay on the HTTP port stays for browsers, agents and other
//! light clients; nodes do not use it to talk to each other.

use crate::event::*;
use crate::node::{Node, ORIGIN_P2P, ORIGIN_SYNC};
use crate::pb;
use crate::store::Ev;
use libp2p::futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::gossipsub::{self, IdentTopic, MessageAcceptance, MessageId};
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport, ResponseChannel};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent, dial_opts::DialOpts};
pub use libp2p::{Multiaddr, PeerId};
use libp2p::{StreamProtocol, identify, identity, kad, multiaddr::Protocol, ping};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub const META: &str = "sametra/1/meta";
pub const ALL: &str = "sametra/1/all";
pub fn tree_topic(root: &H32) -> String {
    format!("sametra/1/tree/{}", root.hex())
}

const REQ_PROTOCOL: StreamProtocol = StreamProtocol::new("/sametra/req/1");
const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/sametra/kad/1.0.0");
const ID_PROTOCOL: &str = "/sametra/id/1";
/// Largest request a peer may send us, and response we accept.
const MAX_REQUEST: u64 = 1 << 20;
const MAX_RESPONSE: u64 = 32 << 20;
/// Largest gossip message: one event (64 KiB content, plus tags and signature).
const MAX_GOSSIP: usize = 256 << 10;

/// `host:port` (QUIC on that UDP port), `/ip4/…/udp/…/quic-v1[/p2p/…]` or any
/// other multiaddr.
pub fn parse_addr(s: &str) -> Result<Multiaddr, String> {
    let s = s.trim();
    if s.starts_with('/') {
        return s.parse().map_err(|e| format!("bad multiaddr '{s}': {e}"));
    }
    if s.contains("://") {
        return Err(format!("'{s}': nodes connect over libp2p now; give host:port (QUIC, UDP) or a multiaddr"));
    }
    let (host, port) = s.rsplit_once(':').ok_or_else(|| format!("'{s}': expected host:port or a multiaddr"))?;
    let port: u16 = port.parse().map_err(|_| format!("'{s}': bad port"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let base = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => Multiaddr::empty().with(Protocol::Ip4(ip)),
        Ok(std::net::IpAddr::V6(ip)) => Multiaddr::empty().with(Protocol::Ip6(ip)),
        Err(_) => Multiaddr::empty().with(Protocol::Dns(host.to_string().into())),
    };
    Ok(base.with(Protocol::Udp(port)).with(Protocol::QuicV1))
}

/// The libp2p identity for the node's key.
pub fn identity(keys: &Keys) -> identity::Keypair {
    let sk = identity::secp256k1::SecretKey::try_from_bytes(keys.secret_bytes()).expect("a valid secp256k1 key");
    identity::secp256k1::Keypair::from(sk).into()
}

// ---- wire codec: one protobuf message per stream -----------------------------

#[derive(Clone, Default)]
pub struct Codec;

impl request_response::Codec for Codec {
    type Protocol = StreamProtocol;
    type Request = pb::PeerRequest;
    type Response = pb::PeerResponse;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<pb::PeerRequest>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        let mut b = Vec::new();
        io.take(MAX_REQUEST).read_to_end(&mut b).await?;
        pb::decode(&b).map_err(std::io::Error::other)
    }
    async fn read_response<T>(&mut self, _: &StreamProtocol, io: &mut T) -> std::io::Result<pb::PeerResponse>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        let mut b = Vec::new();
        io.take(MAX_RESPONSE).read_to_end(&mut b).await?;
        pb::decode(&b).map_err(std::io::Error::other)
    }
    async fn write_request<T>(&mut self, _: &StreamProtocol, io: &mut T, r: pb::PeerRequest) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&pb::encode(&r)).await
    }
    async fn write_response<T>(&mut self, _: &StreamProtocol, io: &mut T, r: pb::PeerResponse) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        io.write_all(&pb::encode(&r)).await
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    gossip: gossipsub::Behaviour,
    req: request_response::Behaviour<Codec>,
}

// ---- the handle the rest of the node uses ------------------------------------

/// A connected peer.
#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub addr: Multiaddr,
    pub outbound: bool,
    pub since: u64,
    /// gossip topics it subscribes to
    pub topics: HashSet<String>,
}

#[derive(Default)]
pub struct State {
    pub peers: HashMap<PeerId, PeerInfo>,
    /// where this node listens (with /p2p/<id>)
    pub listen: Vec<Multiaddr>,
    /// topics this node subscribes to
    pub topics: BTreeSet<String>,
}

enum Cmd {
    Dial(Multiaddr),
    Publish(Vec<String>, Vec<u8>),
    Subscribe(String),
    Unsubscribe(String),
    Request(PeerId, pb::PeerRequest, oneshot::Sender<Result<pb::PeerResponse, String>>),
    Respond(ResponseChannel<pb::PeerResponse>, pb::PeerResponse),
}

pub struct P2p {
    pub peer_id: PeerId,
    cmd: mpsc::UnboundedSender<Cmd>,
    pub state: Mutex<State>,
}

impl P2p {
    pub fn dial(&self, addr: Multiaddr) {
        let _ = self.cmd.send(Cmd::Dial(addr));
    }
    pub fn publish(&self, topics: Vec<String>, data: Vec<u8>) {
        let _ = self.cmd.send(Cmd::Publish(topics, data));
    }
    /// Subscribe to exactly these topics.
    pub fn set_topics(&self, want: BTreeSet<String>) {
        let mut st = self.state.lock().unwrap();
        for t in st.topics.difference(&want) {
            let _ = self.cmd.send(Cmd::Unsubscribe(t.clone()));
        }
        for t in want.difference(&st.topics) {
            let _ = self.cmd.send(Cmd::Subscribe(t.clone()));
        }
        st.topics = want;
    }
    pub async fn request(&self, peer: PeerId, req: pb::PeerRequest) -> Result<pb::PeerResponse, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Cmd::Request(peer, req, tx)).map_err(|_| "network stopped".to_string())?;
        rx.await.map_err(|_| "network stopped".to_string())?
    }
    pub fn peers(&self) -> Vec<(PeerId, PeerInfo)> {
        self.state.lock().unwrap().peers.iter().map(|(p, i)| (*p, i.clone())).collect()
    }
    pub fn peer_count(&self) -> usize {
        self.state.lock().unwrap().peers.len()
    }
    pub fn listen_addrs(&self) -> Vec<Multiaddr> {
        self.state.lock().unwrap().listen.clone()
    }
}

/// Start the swarm for `node`, listening on `listen` (the node's `host:port`).
pub fn start(node: &Arc<Node>) -> Result<Arc<P2p>, String> {
    let key = identity(&node.keys);
    let peer_id = key.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_quic()
        .with_dns()
        .map_err(|e| format!("p2p dns: {e}"))?
        .with_behaviour(|key| {
            let gossip_cfg = gossipsub::ConfigBuilder::default()
                .max_transmit_size(MAX_GOSSIP)
                .validation_mode(gossipsub::ValidationMode::Anonymous)
                .validate_messages()
                // the same event on the same topic is the same message, whoever publishes it
                .message_id_fn(|m: &gossipsub::Message| {
                    let id = pb::decode::<pb::Event>(&m.data).map(|e| e.id).unwrap_or_default();
                    MessageId::from([m.topic.as_str().as_bytes(), b"\0", &id].concat())
                })
                .build()
                .map_err(|e| e.to_string())?;
            let gossip = gossipsub::Behaviour::new(gossipsub::MessageAuthenticity::Anonymous, gossip_cfg)?;
            let mut kcfg = kad::Config::new(KAD_PROTOCOL);
            kcfg.set_query_timeout(Duration::from_secs(30));
            let kad = kad::Behaviour::with_config(key.public().to_peer_id(), kad::store::MemoryStore::new(key.public().to_peer_id()), kcfg);
            let req = request_response::Behaviour::with_codec(
                Codec,
                [(REQ_PROTOCOL, ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(Duration::from_secs(20)).with_max_concurrent_streams(64),
            );
            Ok(Behaviour {
                identify: identify::Behaviour::new(identify::Config::new(ID_PROTOCOL.into(), key.public()).with_agent_version(format!("sametra/{}", env!("CARGO_PKG_VERSION")))),
                ping: ping::Behaviour::default(),
                kad,
                gossip,
                req,
            })
        })
        .map_err(|e| format!("p2p: {e}"))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();

    let listen = listen_addr(&node.cfg.listen)?;
    swarm.listen_on(listen.clone()).map_err(|e| format!("p2p listen {listen}: {e}"))?;
    if let Some(p) = &node.cfg.public_addr {
        let a = parse_addr(p)?;
        swarm.add_external_address(a);
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let p2p = Arc::new(P2p { peer_id, cmd: tx, state: Mutex::default() });
    tokio::spawn(run(swarm, rx, Arc::downgrade(node), p2p.clone()));
    Ok(p2p)
}

/// QUIC on the UDP port with the same number as the node's TCP listener.
fn listen_addr(listen: &str) -> Result<Multiaddr, String> {
    let sa: std::net::SocketAddr = listen.parse().map_err(|_| format!("--listen '{listen}': expected ip:port"))?;
    let ip = match sa.ip() {
        std::net::IpAddr::V4(ip) => Protocol::Ip4(ip),
        std::net::IpAddr::V6(ip) => Protocol::Ip6(ip),
    };
    Ok(Multiaddr::empty().with(ip).with(Protocol::Udp(sa.port())).with(Protocol::QuicV1))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

async fn run(mut swarm: libp2p::Swarm<Behaviour>, mut rx: mpsc::UnboundedReceiver<Cmd>, node: Weak<Node>, p2p: Arc<P2p>) {
    let mut pending: HashMap<OutboundRequestId, oneshot::Sender<Result<pb::PeerResponse, String>>> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    // the address book: where peers were reached, and when to try again
    let mut book: HashMap<Multiaddr, (u32, u64)> = HashMap::new();
    let mut dialing: HashMap<libp2p::swarm::ConnectionId, Multiaddr> = HashMap::new();
    let hello = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(n) = node.upgrade() {
        for a in n.cfg.peers.iter().chain(n.saved_peers().iter()) {
            match parse_addr(a) {
                Ok(a) => {
                    book.insert(a, (0, 0));
                }
                Err(e) => tracing::warn!(peer = %a, error = %e, "skipping peer address"),
            }
        }
    }
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                let b = swarm.behaviour_mut();
                match cmd {
                    Cmd::Dial(a) => {
                        book.entry(a.clone()).or_insert((0, 0));
                        let opts = DialOpts::unknown_peer_id().address(a.clone()).build();
                        let id = opts.connection_id();
                        dialing.insert(id, a.clone());
                        if let Err(e) = swarm.dial(opts) {
                            tracing::debug!(addr = %a, error = %e, "dial failed");
                        }
                    }
                    Cmd::Publish(topics, data) => {
                        for t in topics {
                            match b.gossip.publish(IdentTopic::new(&t), data.clone()) {
                                Ok(_) | Err(gossipsub::PublishError::Duplicate) => {}
                                Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => tracing::trace!(topic = %t, "nobody listens on this topic yet"),
                                Err(e) => tracing::debug!(topic = %t, error = %e, "publish failed"),
                            }
                        }
                    }
                    Cmd::Subscribe(t) => { let _ = b.gossip.subscribe(&IdentTopic::new(t)); }
                    Cmd::Unsubscribe(t) => { b.gossip.unsubscribe(&IdentTopic::new(t)); }
                    Cmd::Request(peer, r, back) => {
                        let id = b.req.send_request(&peer, r);
                        pending.insert(id, back);
                    }
                    Cmd::Respond(ch, r) => { let _ = b.req.send_response(ch, r); }
                }
            }
            ev = swarm.select_next_some() => {
                let Some(n) = node.upgrade() else { break };
                match ev {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        tracing::info!(addr = %address, "p2p listening");
                        p2p.state.lock().unwrap().listen.push(address.with_p2p(p2p.peer_id).unwrap_or_else(|a| a));
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, connection_id, .. } => {
                        let outbound = endpoint.is_dialer();
                        let addr = endpoint.get_remote_address().clone();
                        if let Some(a) = dialing.remove(&connection_id) {
                            book.insert(a, (0, 0));
                        }
                        if num_established.get() == 1 {
                            tracing::info!(peer = %peer_id, %addr, outbound, "peer connected");
                            p2p.state.lock().unwrap().peers.insert(peer_id, PeerInfo { addr: addr.clone(), outbound, since: now(), topics: HashSet::new() });
                            if outbound {
                                swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                            }
                            let (n, p) = (n.clone(), p2p.clone());
                            tokio::spawn(async move { catch_up(&n, &p, peer_id).await });
                        }
                    }
                    SwarmEvent::ConnectionClosed { peer_id, num_established: 0, .. } => {
                        tracing::info!(peer = %peer_id, "peer disconnected");
                        p2p.state.lock().unwrap().peers.remove(&peer_id);
                    }
                    SwarmEvent::OutgoingConnectionError { connection_id, error, .. } => {
                        if let Some(a) = dialing.remove(&connection_id) {
                            let e = book.entry(a.clone()).or_insert((0, 0));
                            e.0 += 1;
                            e.1 = now() + (15u64 << e.0.min(8)).min(3600);
                            if e.0 == 1 {
                                tracing::warn!(addr = %a, %error, "dial failed");
                            } else {
                                tracing::debug!(addr = %a, %error, fails = e.0, "dial failed again");
                            }
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. })) => {
                        if info.protocols.contains(&KAD_PROTOCOL) {
                            for a in info.listen_addrs {
                                swarm.behaviour_mut().kad.add_address(&peer_id, a);
                            }
                        }
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Gossip(ev)) => match ev {
                        gossipsub::Event::Message { propagation_source, message_id, message } => {
                            let acc = match pb::decode::<pb::Event>(&message.data).ok().and_then(Event::from_pb) {
                                None => MessageAcceptance::Reject,
                                Some(e) => match n.ingest(e, ORIGIN_P2P, false) {
                                    Ok((e, new)) => {
                                        // bridge: new to us, so pass it on to its other topics
                                        if new {
                                            n.gossip(&e, Some(message.topic.as_str()));
                                        }
                                        MessageAcceptance::Accept
                                    }
                                    Err(m) if m.starts_with("invalid") => MessageAcceptance::Reject,
                                    Err(_) => MessageAcceptance::Ignore,
                                },
                            };
                            swarm.behaviour_mut().gossip.report_message_validation_result(&message_id, &propagation_source, acc);
                        }
                        gossipsub::Event::Subscribed { peer_id, topic } => {
                            let topic = topic.into_string();
                            // a new listener for metadata: introduce ourselves (presence, IPFS peer)
                            // (peers joining together get one heartbeat, a moment later)
                            if topic == META && !hello.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                let (n, hello) = (n.clone(), hello.clone());
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_millis(300)).await;
                                    hello.store(false, std::sync::atomic::Ordering::Relaxed);
                                    n.heartbeat();
                                });
                            }
                            if let Some(p) = p2p.state.lock().unwrap().peers.get_mut(&peer_id) {
                                p.topics.insert(topic);
                            }
                        }
                        gossipsub::Event::Unsubscribed { peer_id, topic } => {
                            if let Some(p) = p2p.state.lock().unwrap().peers.get_mut(&peer_id) {
                                p.topics.remove(&topic.into_string());
                            }
                        }
                        _ => {}
                    },
                    SwarmEvent::Behaviour(BehaviourEvent::Req(ev)) => match ev {
                        request_response::Event::Message { peer, message: request_response::Message::Request { request, channel, .. }, .. } => {
                            let (n, tx) = (n.clone(), p2p.cmd.clone());
                            tokio::spawn(async move {
                                let r = answer(&n, peer, request).await;
                                let _ = tx.send(Cmd::Respond(channel, r));
                            });
                        }
                        request_response::Event::Message { message: request_response::Message::Response { request_id, response }, .. } => {
                            if let Some(back) = pending.remove(&request_id) {
                                let _ = back.send(Ok(response));
                            }
                        }
                        request_response::Event::OutboundFailure { request_id, error, .. } => {
                            if let Some(back) = pending.remove(&request_id) {
                                let _ = back.send(Err(error.to_string()));
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            _ = tick.tick() => {
                let Some(n) = node.upgrade() else { break };
                // keep up to max_peers outbound connections: the address book first,
                // then peers Kademlia knows about
                let outbound = p2p.state.lock().unwrap().peers.values().filter(|p| p.outbound).count();
                if outbound < n.cfg.max_peers {
                    let connected: HashSet<PeerId> = swarm.connected_peers().copied().collect();
                    let t = now();
                    let mut want = n.cfg.max_peers - outbound;
                    let addrs: Vec<Multiaddr> = book.iter().filter(|(a, (_, next))| *next <= t && !dialing.values().any(|d| d == *a) && !matches!(a.iter().last(), Some(Protocol::P2p(p)) if connected.contains(&p))).map(|(a, _)| a.clone()).take(want).collect();
                    for a in addrs {
                        let opts = DialOpts::unknown_peer_id().address(a.clone()).build();
                        dialing.insert(opts.connection_id(), a.clone());
                        book.entry(a).or_default().1 = t + 30;
                        let _ = swarm.dial(opts);
                        want = want.saturating_sub(1);
                    }
                    let found: Vec<PeerId> = swarm.behaviour_mut().kad.kbuckets().flat_map(|b| b.iter().map(|e| *e.node.key.preimage()).collect::<Vec<_>>()).filter(|p| !connected.contains(p) && *p != p2p.peer_id).take(want).collect();
                    for p in found {
                        let _ = swarm.dial(DialOpts::peer_id(p).build());
                    }
                }
                if !swarm.behaviour_mut().kad.kbuckets().all(|b| b.num_entries() == 0) {
                    let _ = swarm.behaviour_mut().kad.bootstrap();
                }
                n.remember_peers(book.iter().filter(|(_, (fails, _))| *fails < 8).map(|(a, _)| a.to_string()).collect());
            }
        }
    }
    tracing::debug!("p2p loop stopped");
}

/// Pull what we missed from a peer that just connected.
async fn catch_up(node: &Arc<Node>, p2p: &P2p, peer: PeerId) {
    let since = node.store.lock().unwrap().iter().next().map(|e| e.created_at.saturating_sub(3600)).unwrap_or(0);
    let req = pb::PeerRequest { filters: node.sync_filters(since).iter().map(Filter::to_pb).collect(), ..Default::default() };
    match p2p.request(peer, req).await {
        Ok(r) => {
            let (mut new, total) = (0, r.events.len());
            for e in r.events.into_iter().filter_map(Event::from_pb) {
                if matches!(node.ingest(e, ORIGIN_SYNC, false), Ok((_, true))) {
                    new += 1;
                }
            }
            tracing::debug!(peer = %peer, total, new, "caught up");
        }
        Err(e) => tracing::debug!(peer = %peer, error = %e, "catch-up failed"),
    }
}

/// Answer a peer's request from our store; forward searches that may travel on.
async fn answer(node: &Arc<Node>, peer: PeerId, r: pb::PeerRequest) -> pb::PeerResponse {
    let filters: Vec<Filter> = r.filters.iter().take(16).map(Filter::from_pb).collect();
    let search = filters.iter().any(|f| f.search.is_some());
    let hops = r.hops.min(crate::node::MAX_HOPS);
    let mut hits: Vec<Ev> = {
        let st = node.store.lock().unwrap();
        filters.iter().flat_map(|f| st.query(f, if search { 200 } else { 5000 })).collect()
    };
    let mut seen = HashSet::new();
    hits.retain(|e| seen.insert(e.id));
    {
        // popularity: found by a search, or fetched by id
        let by_id = filters.iter().any(|f| f.ids.is_some());
        let mut st = node.store.lock().unwrap();
        for e in hits.iter().take(200) {
            if search {
                st.bump(&e.id, 0.2)
            } else if by_id {
                st.bump(&e.id, 1.0)
            }
        }
    }
    if search && hops > 0 && !r.search_id.is_empty() && r.search_id.len() <= 64 && node.fresh_search(&r.search_id) {
        let more = node.ask_peers(filters, crate::node::search_timeout(hops - 1), hops - 1, Some(peer), Some(r.search_id)).await;
        for e in more {
            if seen.insert(e.id) {
                hits.push(e);
            }
        }
    }
    pb::PeerResponse { events: hits.iter().map(|e| e.to_pb()).collect(), error: String::new() }
}

/// Which gossip topics an event goes to.
pub fn topics_for(e: &Event) -> Vec<String> {
    let mut t = vec![ALL.to_string()];
    if crate::node::GLOBAL_KINDS.contains(&e.kind) || e.kind == K_HEARTBEAT || crate::node::is_mention(e) {
        t.push(META.into());
    }
    if e.kind == K_POST {
        t.push(tree_topic(&e.root().unwrap_or(e.id)));
    }
    t
}

impl Node {
    /// The topics this node listens on: everything if it relays everything; the
    /// trees it follows or takes part in otherwise; metadata always.
    pub fn wanted_topics(&self) -> BTreeSet<String> {
        let mut t = BTreeSet::from([META.to_string()]);
        if self.cfg.relay_all {
            t.insert(ALL.into());
            return t;
        }
        let st = self.store.lock().unwrap();
        for f in self.following.lock().unwrap().iter() {
            let root = st.get(f).and_then(|e| e.root()).unwrap_or(*f);
            t.insert(tree_topic(&root));
        }
        let mine = st.query(&Filter { kinds: Some(vec![K_POST]), authors: Some(vec![self.keys.pubkey.hex()]), ..Default::default() }, 2000);
        for e in mine {
            t.insert(tree_topic(&e.root().unwrap_or(e.id)));
        }
        t
    }
    /// Re-subscribe after what we follow or take part in changed.
    pub fn update_topics(&self) {
        if let Some(p) = self.p2p.get() {
            p.set_topics(self.wanted_topics());
        }
    }
    /// Spread an event we just stored to the topics it belongs to (except the
    /// one it arrived on: GossipSub already carries it there).
    pub(crate) fn gossip(&self, e: &Event, arrived_on: Option<&str>) {
        if let Some(p) = self.p2p.get() {
            let topics: Vec<String> = topics_for(e).into_iter().filter(|t| Some(t.as_str()) != arrived_on).collect();
            if !topics.is_empty() {
                p.publish(topics, pb::encode(&e.to_pb()));
            }
        }
    }
    /// Connected peers, for `peers` and `info`.
    pub fn peer_list(&self) -> Vec<pb::Peer> {
        let Some(p) = self.p2p.get() else { return vec![] };
        p.peers().into_iter().map(|(id, i)| pb::Peer { addr: i.addr.with_p2p(id).unwrap_or_else(|a| a).to_string(), outbound: i.outbound, since: i.since }).collect()
    }
    pub fn peer_count(&self) -> usize {
        self.p2p.get().map(|p| p.peer_count()).unwrap_or(0)
    }

    /// Send `filters` to up to FANOUT peers (except `exclude`) and collect what
    /// they return until all answered or `timeout` passed. What comes back is
    /// stored (it was asked for).
    pub async fn ask_peers(&self, filters: Vec<Filter>, timeout: Duration, hops: u32, exclude: Option<PeerId>, sid: Option<String>) -> Vec<Ev> {
        let Some(p2p) = self.p2p.get().cloned() else { return vec![] };
        let peers: Vec<PeerId> = p2p.peers().into_iter().map(|x| x.0).filter(|p| Some(*p) != exclude).take(crate::node::FANOUT).collect();
        let req = pb::PeerRequest { filters: filters.iter().map(Filter::to_pb).collect(), hops, search_id: sid.unwrap_or_default() };
        let started = tokio::time::Instant::now();
        let asks = peers.iter().map(|p| {
            let (p2p, req) = (p2p.clone(), req.clone());
            let p = *p;
            async move { tokio::time::timeout(timeout, p2p.request(p, req)).await }
        });
        let answers = libp2p::futures::future::join_all(asks).await;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut unanswered = 0;
        for a in answers {
            match a {
                Ok(Ok(r)) => {
                    for e in r.events.into_iter().filter_map(Event::from_pb) {
                        if seen.insert(e.id)
                            && let Ok((e, _)) = self.ingest(e, ORIGIN_SYNC, true)
                        {
                            out.push(e);
                        }
                    }
                }
                _ => unanswered += 1,
            }
        }
        tracing::debug!(hops, asked = peers.len(), unanswered, found = out.len(), ms = started.elapsed().as_millis() as u64, "asked peers");
        out
    }

    /// Ask peers for events matching `filters`.
    pub async fn remote_fetch(&self, filters: Vec<Filter>, timeout: Duration) -> Vec<Ev> {
        self.ask_peers(filters, timeout, 0, None, None).await
    }

    /// Search the network: peers answer from their index and forward the query
    /// up to `hops` more hops. Lossy by design: whatever arrives in time.
    pub async fn remote_search(&self, filters: Vec<Filter>, hops: u32) -> Vec<Ev> {
        let hops = hops.min(crate::node::MAX_HOPS);
        let mut rnd = [0u8; 8];
        getrandom::getrandom(&mut rnd).unwrap();
        let sid = format!("q-{}", hex::encode(rnd));
        self.fresh_search(&sid);
        self.ask_peers(filters, crate::node::search_timeout(hops), hops, None, Some(sid)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_addr("10.0.0.1:42424").unwrap().to_string(), "/ip4/10.0.0.1/udp/42424/quic-v1");
        assert_eq!(parse_addr("[::1]:5").unwrap().to_string(), "/ip6/::1/udp/5/quic-v1");
        assert_eq!(parse_addr("node.example:42424").unwrap().to_string(), "/dns/node.example/udp/42424/quic-v1");
        assert!(parse_addr("/ip4/1.2.3.4/udp/1/quic-v1").is_ok());
        assert!(parse_addr("ws://x:1").unwrap_err().contains("libp2p"));
        assert!(parse_addr("nope").is_err());
        assert_eq!(listen_addr("0.0.0.0:42424").unwrap().to_string(), "/ip4/0.0.0.0/udp/42424/quic-v1");
    }

    #[test]
    fn the_peer_id_is_the_node_key() {
        let k = Keys::generate();
        let id = identity(&k);
        let pk = id.public().try_into_secp256k1().unwrap().to_bytes();
        assert_eq!(&pk[1..], &k.pubkey.0, "compressed key: parity byte, then the x coordinate");
    }

    #[test]
    fn topics() {
        let k = Keys::generate();
        let root = k.sign(K_POST, vec![], "q".into());
        let reply = k.sign(K_POST, ancestry_tags(&root.id, &[]), "a".into());
        assert_eq!(topics_for(&root), vec![ALL.to_string(), tree_topic(&root.id)]);
        assert_eq!(topics_for(&reply), vec![ALL.to_string(), tree_topic(&root.id)], "a reply goes to its root's tree");
        let rating = k.sign(K_RATING, vec![], String::new());
        assert_eq!(topics_for(&rating), vec![ALL.to_string(), META.to_string()]);
    }
}
