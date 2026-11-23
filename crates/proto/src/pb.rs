//! Generated protobuf types (see proto/sametra.proto and build.rs) plus codec helpers.
#![allow(dead_code, non_snake_case, non_camel_case_types, unused_imports, clippy::all)]
include!(concat!(env!("OUT_DIR"), "/sametra.rs"));

/// Encode a message body (no length prefix).
pub fn encode<M: MessageWrite>(m: &M) -> Vec<u8> {
    let mut v = Vec::with_capacity(m.get_size());
    m.write_message(&mut Writer::new(&mut v)).expect("vec write");
    v
}

/// Decode a message body (no length prefix).
pub fn decode<'a, M: MessageRead<'a>>(b: &'a [u8]) -> std::result::Result<M, String> {
    M::from_reader(&mut BytesReader::from_bytes(b), b).map_err(|e| format!("bad protobuf: {e}"))
}

/// Append `m` as a varint-length-delimited record.
pub fn write_delimited<M: MessageWrite>(out: &mut impl std::io::Write, m: &M) -> std::io::Result<()> {
    let body = encode(m);
    let mut n = body.len() as u64;
    let mut hdr = [0u8; 10];
    let mut i = 0;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        hdr[i] = if n == 0 { b } else { b | 0x80 };
        i += 1;
        if n == 0 {
            break;
        }
    }
    out.write_all(&hdr[..i])?;
    out.write_all(&body)
}

/// Read one varint-length-delimited record body; Ok(None) at clean EOF.
pub fn read_delimited(r: &mut impl std::io::Read, buf: &mut Vec<u8>) -> std::io::Result<Option<()>> {
    let (mut len, mut shift) = (0u64, 0u32);
    loop {
        let mut b = [0u8; 1];
        if r.read(&mut b)? == 0 {
            return if shift == 0 { Ok(None) } else { Err(std::io::ErrorKind::UnexpectedEof.into()) };
        }
        len |= ((b[0] & 0x7f) as u64) << shift;
        if b[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 35 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
    if len > 1 << 20 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    buf.resize(len as usize, 0);
    r.read_exact(buf)?;
    Ok(Some(()))
}

// Frame types
pub const F_PUBLISH: u32 = 1;
pub const F_REQ: u32 = 2;
pub const F_CLOSE: u32 = 3;
pub const F_EVENT: u32 = 4;
pub const F_EOSE: u32 = 5;
pub const F_OK: u32 = 6;
pub const F_NOTICE: u32 = 7;
pub const F_CLOSED: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame() -> Frame {
        Frame {
            type_pb: F_REQ,
            sub_id: "s1".into(),
            filters: vec![Filter { kinds: vec![11, 1111], tags: vec![TagFilter { name: "c".into(), values: vec!["math".into()] }], limit: 5, ..Default::default() }],
            event: Some(Event { id: vec![1; 32], pubkey: vec![2; 32], kind: 11, content: "hé".into(), sig: vec![3; 64], ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let f = frame();
        let b = encode(&f);
        assert_eq!(b.len(), f.get_size());
        assert_eq!(decode::<Frame>(&b).unwrap(), f);
    }

    #[test]
    fn defaults_encode_to_nothing() {
        assert!(encode(&Empty {}).is_empty());
        assert!(encode(&Ack::default()).is_empty());
        assert_eq!(decode::<Ack>(&[]).unwrap(), Ack::default());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode::<Frame>(&[0xff, 0xff, 0xff]).is_err());
        // field 2 (string) claiming more bytes than present
        assert!(decode::<Frame>(&[0x12, 0x10, b'a']).is_err());
    }

    #[test]
    fn delimited_records() {
        let mut buf = vec![];
        let big = Event { content: "x".repeat(300), ..Default::default() }; // needs a 2-byte varint
        write_delimited(&mut buf, &frame()).unwrap();
        write_delimited(&mut buf, &big).unwrap();
        write_delimited(&mut buf, &Empty {}).unwrap();
        let (mut r, mut b) = (Cursor::new(buf), vec![]);
        assert!(read_delimited(&mut r, &mut b).unwrap().is_some());
        assert_eq!(decode::<Frame>(&b).unwrap(), frame());
        assert!(read_delimited(&mut r, &mut b).unwrap().is_some());
        assert_eq!(decode::<Event>(&b).unwrap(), big);
        assert!(read_delimited(&mut r, &mut b).unwrap().is_some());
        assert!(b.is_empty());
        assert!(read_delimited(&mut r, &mut b).unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn delimited_errors() {
        let mut b = vec![];
        // torn record: header says 10 bytes, 3 present
        assert!(read_delimited(&mut Cursor::new(vec![10, 1, 2, 3]), &mut b).is_err());
        // EOF inside the varint header
        assert!(read_delimited(&mut Cursor::new(vec![0x80]), &mut b).is_err());
        // header longer than 5 bytes
        assert!(read_delimited(&mut Cursor::new(vec![0x80; 7]), &mut b).is_err());
        // record over the 1 MiB cap
        assert!(read_delimited(&mut Cursor::new(vec![0x80, 0x80, 0x80, 0x01]), &mut b).is_err());
    }

    #[test]
    fn json_bridge_fills_missing_fields() {
        let r: PostRequest = serde_json::from_value(serde_json::json!({"content": "hi"})).unwrap();
        assert_eq!(r.content, "hi");
        assert!(r.community.is_empty() && r.refs.is_empty());
        let r: RateRequest = serde_json::from_value(serde_json::json!({"score": -0.3})).unwrap();
        assert_eq!(r.score, -0.3);
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["score"], -0.3);
        assert!(serde_json::from_value::<RateRequest>(serde_json::json!({"score": "x"})).is_err());
    }
}

/// Golden encodings shared with web/test/api.test.js: both codecs must agree byte for byte.
#[cfg(test)]
mod golden {
    use super::*;

    pub fn vectors() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("RateRequest", encode(&RateRequest { score: -0.3, event: "ab".into(), reason: "é".into(), ..Default::default() })),
            ("FetchRequest", encode(&FetchRequest {
                filter: Some(Filter { kinds: vec![11, 1111], tags: vec![TagFilter { name: "c".into(), values: vec!["m".into()] }], since: 1700000000, limit: 5, ..Default::default() }),
            })),
            ("Ack", encode(&Ack { ok: true, id: "x".into(), count: 3, rep: -1.5, community: "m".into() })),
            ("IdRequest", encode(&IdRequest { id: "i".into(), thread: true })),
        ]
    }

    #[test]
    fn print_and_check_golden_vectors() {
        let want = [
            ("RateRequest", "09333333333333d3bf120261622a02c3a9"),
            ("FetchRequest", "0a151a030bd70822060a016312016d2880e2cfaa063805"),
            ("Ack", "0801120178180321000000000000f8bf2a016d"),
            ("IdRequest", "0a01691001"),
        ];
        for ((name, got), (wname, hexs)) in vectors().into_iter().zip(want) {
            assert_eq!(name, wname);
            println!("{name}: {}", hex::encode(&got));
            assert_eq!(hex::encode(&got), hexs.replace(' ', ""), "{name}");
        }
    }
}
