//! The relay wire codec: binary protobuf `Frame`s, one per WebSocket message.
//! There is no text encoding; a text message on a link is a protocol error.

use sametra_proto::event::{Event, Filter, H32};
use sametra_proto::pb;
use tokio_tungstenite::tungstenite::Message;

/// Searches travel at most this many hops beyond the first peer.
pub const MAX_HOPS: u32 = 3;

/// A protocol message.
pub enum Msg {
    Publish(Event),
    /// subscription id, filters, and for searches how many more hops it may travel
    Req(String, Vec<Filter>, u32),
    Close(String),
    SubEvent(String, Event),
    Eose(String),
    Ignore,
    Bad,
}

pub fn parse_frame(b: &[u8]) -> Msg {
    let Ok(f) = pb::decode::<pb::Frame>(b) else { return Msg::Bad };
    let ev = f.event.and_then(Event::from_pb);
    match f.type_pb {
        pb::F_PUBLISH => ev.map(Msg::Publish).unwrap_or(Msg::Bad),
        pb::F_EVENT => ev.map(|e| Msg::SubEvent(f.sub_id, e)).unwrap_or(Msg::Bad),
        pb::F_REQ => Msg::Req(f.sub_id, f.filters.iter().take(16).map(Filter::from_pb).collect(), f.hops.min(MAX_HOPS)),
        pb::F_CLOSE => Msg::Close(f.sub_id),
        pb::F_EOSE => Msg::Eose(f.sub_id),
        _ => Msg::Ignore,
    }
}

/// Encodes outgoing frames.
#[derive(Clone, Copy, Default)]
pub struct Wire;

impl Wire {
    fn frame(f: pb::Frame) -> Message {
        Message::binary(pb::encode(&f))
    }
    pub fn event(&self, sid: &str, e: &Event) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_EVENT, sub_id: sid.into(), event: Some(e.to_pb()), ..Default::default() })
    }
    pub fn publish(&self, e: &Event) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_PUBLISH, event: Some(e.to_pb()), ..Default::default() })
    }
    pub fn req(&self, sid: &str, fs: &[Filter], hops: u32) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_REQ, sub_id: sid.into(), filters: fs.iter().map(Filter::to_pb).collect(), hops, ..Default::default() })
    }
    pub fn close(&self, sid: &str) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_CLOSE, sub_id: sid.into(), ..Default::default() })
    }
    pub fn eose(&self, sid: &str) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_EOSE, sub_id: sid.into(), ..Default::default() })
    }
    pub fn ok(&self, id: &H32, ok: bool, msg: &str) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_OK, event_id: id.0.to_vec(), ok, message: msg.into(), ..Default::default() })
    }
    pub fn closed(&self, sid: &str, msg: &str) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_CLOSED, sub_id: sid.into(), message: msg.into(), ..Default::default() })
    }
    pub fn notice(&self, msg: &str) -> Message {
        Self::frame(pb::Frame { type_pb: pb::F_NOTICE, message: msg.into(), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sametra_proto::event::{K_POST, Keys};

    fn c(slug: &str) -> Vec<Vec<String>> {
        vec![vec!["c".into(), slug.into()]]
    }

    #[test]
    fn parse_frames() {
        let e = Keys::generate().sign(K_POST, c("m"), "x".into());
        let f = |fr: pb::Frame| parse_frame(&pb::encode(&fr));
        let ev = Some(e.to_pb());
        assert!(matches!(f(pb::Frame { type_pb: pb::F_PUBLISH, event: ev.clone(), ..Default::default() }), Msg::Publish(x) if x.id == e.id));
        assert!(matches!(f(pb::Frame { type_pb: pb::F_EVENT, sub_id: "s".into(), event: ev, ..Default::default() }), Msg::SubEvent(s, _) if s == "s"));
        assert!(matches!(f(pb::Frame { type_pb: pb::F_PUBLISH, ..Default::default() }), Msg::Bad), "publish without event");
        let bad_ev = Some(pb::Event { id: vec![1; 3], ..Default::default() });
        assert!(matches!(f(pb::Frame { type_pb: pb::F_EVENT, event: bad_ev, ..Default::default() }), Msg::Bad));
        let filters = vec![pb::Filter::default(); 20];
        assert!(matches!(f(pb::Frame { type_pb: pb::F_REQ, sub_id: "s".into(), filters, ..Default::default() }), Msg::Req(_, fs, _) if fs.len() == 16));
        assert!(matches!(f(pb::Frame { type_pb: pb::F_CLOSE, sub_id: "s".into(), ..Default::default() }), Msg::Close(_)));
        let deep = pb::Frame { type_pb: pb::F_REQ, sub_id: "q".into(), filters: vec![pb::Filter { search: "x".into(), ..Default::default() }], hops: 99, ..Default::default() };
        assert!(matches!(f(deep), Msg::Req(_, fs, MAX_HOPS) if fs[0].search.as_deref() == Some("x")), "hops are clamped");
        assert!(matches!(f(pb::Frame { type_pb: pb::F_EOSE, sub_id: "s".into(), ..Default::default() }), Msg::Eose(_)));
        for t in [pb::F_OK, pb::F_NOTICE, pb::F_CLOSED, 99] {
            assert!(matches!(f(pb::Frame { type_pb: t, ..Default::default() }), Msg::Ignore));
        }
        assert!(matches!(parse_frame(&[0xff, 0xff]), Msg::Bad));
    }

    #[test]
    fn frames_roundtrip() {
        let w = Wire;
        let e = Keys::generate().sign(K_POST, c("m"), "x".into());
        let back = |m: Message| match m {
            Message::Binary(b) => parse_frame(&b),
            _ => Msg::Bad,
        };
        assert!(matches!(back(w.event("s", &e)), Msg::SubEvent(s, x) if s == "s" && x.id == e.id && x.validate().is_ok()));
        assert!(matches!(back(w.publish(&e)), Msg::Publish(x) if x.id == e.id));
        assert!(matches!(back(w.eose("s")), Msg::Eose(s) if s == "s"));
        let fs = [Filter { kinds: Some(vec![11]), tags: vec![("c".into(), vec!["m".into()])], ..Default::default() }];
        assert!(matches!(back(w.req("s", &fs, 2)), Msg::Req(s, f, 2) if s == "s" && f[0].matches(&e)));
        assert!(matches!(back(w.close("s")), Msg::Close(_)));
        let ok: pb::Frame = pb::decode(&w.ok(&e.id, true, "").into_data()).unwrap();
        assert_eq!((ok.type_pb, ok.ok, ok.event_id), (pb::F_OK, true, e.id.0.to_vec()));
        let n: pb::Frame = pb::decode(&w.notice("hi").into_data()).unwrap();
        assert_eq!((n.type_pb, n.message.as_str()), (pb::F_NOTICE, "hi"));
        let cl: pb::Frame = pb::decode(&w.closed("s", "m").into_data()).unwrap();
        assert_eq!(cl.type_pb, pb::F_CLOSED);
    }
}
