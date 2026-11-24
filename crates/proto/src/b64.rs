//! Bytes in JSON as base64, as protobuf's JSON mapping writes them. Only the
//! JSON edge (CLI, MCP) uses this; the network carries the bytes themselves.
//! Decoding also accepts the URL-safe alphabet and missing padding.

use serde::{Deserialize, Deserializer, Serializer};

const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    };
    let s = s.trim().trim_end_matches('=');
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(s.len() * 3 / 4), 0u32, 0u32);
    for c in s.bytes() {
        acc = acc << 6 | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (bits < 6).then_some(out)
}

pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&encode(b))
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    decode(&s).ok_or_else(|| serde::de::Error::custom("bytes must be base64"))
}

/// `repeated bytes`.
pub mod many {
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeSeq};

    pub fn serialize<S: Serializer>(v: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(v.len()))?;
        for b in v {
            seq.serialize_element(&super::encode(b))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        Vec::<String>::deserialize(d)?.iter().map(|s| super::decode(s).ok_or_else(|| serde::de::Error::custom("bytes must be base64"))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (raw, b64) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(encode(raw.as_bytes()), b64);
            assert_eq!(decode(b64).unwrap(), raw.as_bytes());
        }
        assert_eq!(decode("-_8").unwrap(), [0xfb, 0xff], "url-safe, unpadded");
        assert!(decode("Zm9v!").is_none() && decode("Z").is_none());
    }

    #[test]
    fn json_uses_base64() {
        let e = crate::pb::IpfsAddRequest { data: vec![0, 1, 2, 255], ..Default::default() };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["data"], "AAEC/w==");
        let back: crate::pb::IpfsAddRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.data, [0, 1, 2, 255]);
        let f: crate::pb::Filter = serde_json::from_value(serde_json::json!({"ids": ["q83v"]})).unwrap();
        assert_eq!(f.ids, [vec![0xab, 0xcd, 0xef]]);
    }
}
