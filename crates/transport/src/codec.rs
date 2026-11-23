//! The relay wire codec: one set of protocol messages, two encodings.
//!
//! Sametra peers exchange binary protobuf `Frame`s; ordinary nostr clients use
//! NIP-01 JSON text. A connection switches to binary on the first binary frame.

use sametra_proto::event::{Event, Filter, H32};
use sametra_proto::pb;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// Searches travel at most this many hops beyond the first peer.
pub const MAX_HOPS: u32 = 3;

/// A protocol message, independent of encoding.
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

pub fn parse_json(txt: &str) -> Msg {
    let Ok(Value::Array(a)) = serde_json::from_str::<Value>(txt) else { return Msg::Bad };
    let s = |i: usize| a.get(i).and_then(|v| v.as_str()).map(String::from);
    let ev = |i: usize| a.get(i).and_then(|v| serde_json::from_value::<Event>(v.clone()).ok());
    match (a.first().and_then(|v| v.as_str()), a.len()) {
        (Some("EVENT"), 2) => ev(1).map(Msg::Publish).unwrap_or(Msg::Bad),
        (Some("EVENT"), 3) => match (s(1), ev(2)) {
            (Some(sid), Some(e)) => Msg::SubEvent(sid, e),
            _ => Msg::Bad,
        },
        (Some("REQ"), n) if n >= 3 => match s(1) {
            Some(sid) => Msg::Req(sid, a[2..].iter().take(16).map(Filter::from_value).collect(), 0),
            None => Msg::Bad,
        },
        (Some("REQ"), _) => Msg::Bad, // NIP-01: at least one filter
        (Some("CLOSE"), _) => s(1).map(Msg::Close).unwrap_or(Msg::Bad),
        (Some("EOSE"), _) => s(1).map(Msg::Eose).unwrap_or(Msg::Bad),
        (Some(_), _) => Msg::Ignore,
        _ => Msg::Bad,
    }
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

/// Encodes outgoing messages for one connection: binary protobuf frames for
/// sametra peers, NIP-01 JSON text for ordinary nostr clients.
#[derive(Clone, Copy)]
pub struct Wire {
    /// binary protobuf frames (Sametra peers) instead of NIP-01 JSON text
    pub binary: bool,
}

impl Wire {
    fn frame(f: pb::Frame) -> Message {
        Message::binary(pb::encode(&f))
    }
    pub fn event(&self, sid: &str, e: &Event) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_EVENT, sub_id: sid.into(), event: Some(e.to_pb()), ..Default::default() })
        } else {
            Message::text(format!("[\"EVENT\",{},{}]", json!(sid), serde_json::to_string(e).unwrap()))
        }
    }
    pub fn publish(&self, e: &Event) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_PUBLISH, event: Some(e.to_pb()), ..Default::default() })
        } else {
            Message::text(format!("[\"EVENT\",{}]", serde_json::to_string(e).unwrap()))
        }
    }
    pub fn req(&self, sid: &str, fs: &[Filter], hops: u32) -> Option<Message> {
        self.binary.then(|| {
            Self::frame(pb::Frame { type_pb: pb::F_REQ, sub_id: sid.into(), filters: fs.iter().map(Filter::to_pb).collect(), hops, ..Default::default() })
        })
    }
    pub fn close(&self, sid: &str) -> Option<Message> {
        self.binary.then(|| Self::frame(pb::Frame { type_pb: pb::F_CLOSE, sub_id: sid.into(), ..Default::default() }))
    }
    pub fn eose(&self, sid: &str) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_EOSE, sub_id: sid.into(), ..Default::default() })
        } else {
            Message::text(json!(["EOSE", sid]).to_string())
        }
    }
    pub fn ok(&self, id: &H32, ok: bool, msg: &str) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_OK, event_id: id.0.to_vec(), ok, message: msg.into(), ..Default::default() })
        } else {
            Message::text(json!(["OK", id.hex(), ok, msg]).to_string())
        }
    }
    pub fn closed(&self, sid: &str, msg: &str) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_CLOSED, sub_id: sid.into(), message: msg.into(), ..Default::default() })
        } else {
            Message::text(json!(["CLOSED", sid, msg]).to_string())
        }
    }
    pub fn notice(&self, msg: &str) -> Message {
        if self.binary {
            Self::frame(pb::Frame { type_pb: pb::F_NOTICE, message: msg.into(), ..Default::default() })
        } else {
            Message::text(json!(["NOTICE", msg]).to_string())
        }
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
    fn parse_json_messages() {
        let e = Keys::generate().sign(K_POST, c("m"), "x".into());
        let ej = serde_json::to_string(&e).unwrap();
        assert!(matches!(parse_json(&format!(r#"["EVENT",{ej}]"#)), Msg::Publish(x) if x.id == e.id));
        assert!(matches!(parse_json(&format!(r#"["EVENT","s",{ej}]"#)), Msg::SubEvent(s, x) if s == "s" && x.id == e.id));
        assert!(matches!(parse_json(r##"["REQ","s",{"kinds":[11]},{"#c":["m"]}]"##), Msg::Req(s, f, 0) if s == "s" && f.len() == 2));
        let many = format!(r#"["REQ","s"{}]"#, ",{}".repeat(40));
        assert!(matches!(parse_json(&many), Msg::Req(_, f, _) if f.len() == 16), "filters per REQ are capped");
        assert!(matches!(parse_json(r#"["CLOSE","s"]"#), Msg::Close(s) if s == "s"));
        assert!(matches!(parse_json(r#"["EOSE","s"]"#), Msg::Eose(s) if s == "s"));
        assert!(matches!(parse_json(r#"["AUTH","x"]"#), Msg::Ignore));
        for bad in [r#"{"a":1}"#, "not json", "[]", "[1]", r#"["EVENT",{"id":"zz"}]"#, r#"["REQ",5,{}]"#, r#"["REQ","s"]"#, r#"["CLOSE"]"#, r#"["EVENT","s",{}]"#] {
            assert!(matches!(parse_json(bad), Msg::Bad), "{bad}");
        }
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
    fn wire_encodings_roundtrip() {
        let e = Keys::generate().sign(K_POST, c("m"), "x".into());
        let (bin, txt) = (Wire { binary: true }, Wire { binary: false });
        let back = |m: Message| match m {
            Message::Binary(b) => parse_frame(&b),
            Message::Text(t) => parse_json(&t),
            _ => Msg::Bad,
        };
        for w in [bin, txt] {
            assert!(matches!(back(w.event("s", &e)), Msg::SubEvent(s, x) if s == "s" && x.id == e.id && x.validate().is_ok()));
            assert!(matches!(back(w.publish(&e)), Msg::Publish(x) if x.id == e.id));
            assert!(matches!(back(w.eose("s")), Msg::Eose(s) if s == "s"));
        }
        let fs = [Filter::from_value(&json!({"kinds": [11], "#c": ["m"]}))];
        assert!(matches!(back(bin.req("s", &fs, 2).unwrap()), Msg::Req(s, f, 2) if s == "s" && f[0].matches(&e)));
        assert!(matches!(back(bin.close("s").unwrap()), Msg::Close(_)));
        assert!(txt.req("s", &fs, 0).is_none() && txt.close("s").is_none(), "we never send REQ to plain nostr clients");
        assert_eq!(txt.ok(&e.id, false, "nope").into_text().unwrap().as_str(), json!(["OK", e.id.hex(), false, "nope"]).to_string());
        assert_eq!(txt.closed("s", "m").into_text().unwrap().as_str(), r#"["CLOSED","s","m"]"#);
        assert_eq!(txt.notice("m").into_text().unwrap().as_str(), r#"["NOTICE","m"]"#);
        let ok: pb::Frame = pb::decode(&bin.ok(&e.id, true, "").into_data()).unwrap();
        assert_eq!((ok.type_pb, ok.ok, ok.event_id), (pb::F_OK, true, e.id.0.to_vec()));
        let n: pb::Frame = pb::decode(&bin.notice("hi").into_data()).unwrap();
        assert_eq!((n.type_pb, n.message.as_str()), (pb::F_NOTICE, "hi"));
        let cl: pb::Frame = pb::decode(&bin.closed("s", "m").into_data()).unwrap();
        assert_eq!(cl.type_pb, pb::F_CLOSED);
        // binary frames are smaller than NIP-01 JSON
        assert!(bin.event("s", &e).len() < txt.event("s", &e).len());
    }
}
