//! Events, filters and keys. Nostr's model (secp256k1 keys, kinds, tags, BIP-340
//! signatures, NIP-19 npub/nsec), with a binary event format of our own: the id is
//! a hash of the canonical protobuf encoding and content is bytes with a media type.

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

// ---- kinds -------------------------------------------------------------
pub const K_PROFILE: u32 = 0;
pub const K_DELETE: u32 = 5;
pub const K_FILE: u32 = 1063; // file metadata (NIP-94): an IPFS file and who added it
/// A post. Every post may have a parent; a reply is a subtopic like any other.
pub const K_POST: u32 = 11;
pub const K_HEARTBEAT: u32 = 20100; // ephemeral presence + peer address
pub const K_RATING: u32 = 31985; // parameterized replaceable rating
pub const K_CAPABILITY: u32 = 31990; // capability announcement (NIP-89 handler information)
pub const K_JOB: u32 = 5910; // capability request (NIP-90 job request)
pub const K_JOB_RESULT: u32 = 6910; // job result (NIP-90: request kind + 1000)
pub const K_JOB_FEEDBACK: u32 = 7000; // job status: processing / error (NIP-90)

pub const MAX_CONTENT: usize = 64 * 1024;

/// Media types of the payloads Sametra defines. `""` is UTF-8 text (markdown).
pub const TYPE_PROFILE: &str = "application/x-protobuf; message=sametra.Profile";
pub const TYPE_CAPABILITY: &str = "application/x-protobuf; message=sametra.CapabilitySpec";
/// Domain separation for event ids.
const ID_DOMAIN: &[u8] = b"sametra/event/1\0";
pub const MAX_TAGS: usize = 256;
/// A post carries at most this many ancestors besides its root (`path` markers):
/// enough that subtree queries are one tag filter for any realistic depth.
pub const MAX_PATH: usize = 30;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- fixed-size hex byte arrays ----------------------------------------
macro_rules! hexbytes {
    ($name:ident, $n:expr) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub [u8; $n]);
        impl $name {
            pub fn hex(&self) -> String {
                hex::encode(self.0)
            }
            pub fn from_hex(s: &str) -> Option<Self> {
                let mut b = [0u8; $n];
                hex::decode_to_slice(s, &mut b).ok()?;
                Some(Self(b))
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&self.hex())
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.hex())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad hex"))
            }
        }
    };
}
hexbytes!(H32, 32);
hexbytes!(H64, 64);

// ---- event ---------------------------------------------------------------
/// An event in memory. Tags are kept as strings for convenience: 32-byte values
/// (ids, pubkeys) as 64 lowercase hex characters, which the binary form carries raw.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: H32,
    pub pubkey: H32,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: Vec<u8>,
    /// `""`: UTF-8 text (markdown); otherwise a media type.
    pub content_type: String,
    pub sig: H64,
}

fn is_hex32(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A tag value as it travels: 32-byte values raw, the rest as text.
pub fn value_to_pb(s: &str) -> crate::pb::TagValue {
    if is_hex32(s) { crate::pb::TagValue { raw: hex::decode(s).unwrap(), ..Default::default() } } else { crate::pb::TagValue { text: s.to_string(), ..Default::default() } }
}
pub fn value_from_pb(v: &crate::pb::TagValue) -> Option<String> {
    match (v.raw.len(), v.text.is_empty()) {
        (0, _) => Some(v.text.clone()),
        (32, true) => Some(hex::encode(&v.raw)),
        _ => None, // raw values are 32 bytes, and never text as well
    }
}
fn tags_to_pb(tags: &[Vec<String>]) -> Vec<crate::pb::Tag> {
    tags.iter().map(|t| crate::pb::Tag { v: t.iter().map(|x| value_to_pb(x)).collect() }).collect()
}

/// Is this media type text (UTF-8, readable)? `""` (markdown), text/*, and the
/// textual application types (JSON, XML).
pub fn is_text(content_type: &str) -> bool {
    let t = content_type.split(';').next().unwrap_or("").trim();
    t.is_empty() || t.starts_with("text/") || t == "application/json" || t.ends_with("+json") || t == "application/xml" || t.ends_with("+xml")
}

impl Event {
    pub fn to_pb(&self) -> crate::pb::Event {
        crate::pb::Event {
            id: self.id.0.to_vec(),
            pubkey: self.pubkey.0.to_vec(),
            created_at: self.created_at,
            kind: self.kind,
            tags: tags_to_pb(&self.tags),
            content: self.content.clone(),
            content_type: self.content_type.clone(),
            sig: self.sig.0.to_vec(),
        }
    }
    /// Structural conversion only; call `validate()` before trusting it.
    pub fn from_pb(p: crate::pb::Event) -> Option<Event> {
        Some(Event {
            id: H32(p.id.try_into().ok()?),
            pubkey: H32(p.pubkey.try_into().ok()?),
            created_at: p.created_at,
            kind: p.kind,
            tags: p.tags.iter().map(|t| t.v.iter().map(value_from_pb).collect::<Option<Vec<_>>>()).collect::<Option<Vec<_>>>()?,
            content: p.content,
            content_type: p.content_type,
            sig: H64(p.sig.try_into().ok()?),
        })
    }

    /// sha256 of the domain tag and the canonical encoding of everything but id and sig.
    pub fn compute_id(pk: &H32, created_at: u64, kind: u32, tags: &[Vec<String>], content: &[u8], content_type: &str) -> H32 {
        let body = crate::pb::EventBody { pubkey: pk.0.to_vec(), created_at, kind, tags: tags_to_pb(tags), content: content.to_vec(), content_type: content_type.to_string() };
        let mut h = Sha256::new();
        h.update(ID_DOMAIN);
        h.update(crate::pb::encode(&body));
        H32(h.finalize().into())
    }

    /// The content as text: for text media types, valid UTF-8; else empty.
    pub fn text(&self) -> &str {
        if is_text(&self.content_type) { std::str::from_utf8(&self.content).unwrap_or("") } else { "" }
    }

    /// Full validation: size limits, text is UTF-8, id hash, schnorr signature.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.content.len() > MAX_CONTENT {
            return Err("invalid: content too large");
        }
        if self.tags.len() > MAX_TAGS {
            return Err("invalid: too many tags");
        }
        if self.content_type.len() > 200 || self.content_type.chars().any(|c| c.is_control()) {
            return Err("invalid: bad content type");
        }
        if is_text(&self.content_type) && std::str::from_utf8(&self.content).is_err() {
            return Err("invalid: text content is not UTF-8");
        }
        if Self::compute_id(&self.pubkey, self.created_at, self.kind, &self.tags, &self.content, &self.content_type) != self.id {
            return Err("invalid: bad id");
        }
        let vk = VerifyingKey::from_slice(&self.pubkey.0).map_err(|_| "invalid: bad pubkey")?;
        let sig = Signature::from_slice(&self.sig.0).map_err(|_| "invalid: bad sig")?;
        vk.verify_raw(&self.id.0, &sig).map_err(|_| "invalid: bad signature")
    }

    pub fn tag<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.tags_named(name).next()
    }
    pub fn tags_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.tags
            .iter()
            .filter(move |t| t.len() >= 2 && t[0] == name)
            .map(|t| t[1].as_str())
    }
    pub fn d_tag(&self) -> &str {
        self.tag("d").unwrap_or("")
    }
    pub fn is_ephemeral(&self) -> bool {
        (20000..30000).contains(&self.kind)
    }
    pub fn is_replaceable(&self) -> bool {
        self.kind == 0 || self.kind == 3 || (10000..20000).contains(&self.kind)
    }
    pub fn is_param_replaceable(&self) -> bool {
        (30000..40000).contains(&self.kind)
    }
    /// The parent post (`e` tag marked "reply"), if this is not a top-level topic.
    pub fn parent(&self) -> Option<H32> {
        self.marked("reply").next()
    }
    /// The top-level topic of this post's tree (`e` tag marked "root").
    pub fn root(&self) -> Option<H32> {
        self.marked("root").next().or_else(|| self.parent())
    }
    /// Every ancestor this post names: root first, then the path, then the parent.
    pub fn ancestors(&self) -> Vec<H32> {
        let mut v: Vec<H32> = self.marked("root").chain(self.marked("path")).chain(self.marked("reply")).collect();
        let mut seen = std::collections::HashSet::new();
        v.retain(|x| seen.insert(*x));
        v
    }
    fn marked<'a>(&'a self, marker: &'a str) -> impl Iterator<Item = H32> + 'a {
        self.tags.iter().filter(move |t| t.len() >= 4 && t[0] == "e" && t[3] == marker).filter_map(|t| H32::from_hex(&t[1]))
    }
}

/// Tags that place a post under `parent`, whose own ancestors are `parent_ancestors`
/// (root first): the root, up to `MAX_PATH` nearest other ancestors, and the parent.
pub fn ancestry_tags(parent: &H32, parent_ancestors: &[H32]) -> Vec<Vec<String>> {
    let e = |id: &H32, m: &str| vec!["e".to_string(), id.hex(), String::new(), m.to_string()];
    let root = parent_ancestors.first().unwrap_or(parent);
    let mut tags = vec![e(root, "root")];
    let inner: Vec<&H32> = parent_ancestors.iter().skip(1).collect();
    for a in inner.iter().skip(inner.len().saturating_sub(MAX_PATH)) {
        tags.push(e(a, "path"));
    }
    tags.push(e(parent, "reply"));
    tags
}

// ---- keys ----------------------------------------------------------------
pub struct Keys {
    sk: SigningKey,
    pub pubkey: H32,
}

impl Keys {
    pub fn generate() -> Keys {
        loop {
            let mut b = [0u8; 32];
            getrandom::getrandom(&mut b).expect("rng");
            if let Ok(k) = Self::from_secret(&b) {
                return k;
            }
        }
    }
    pub fn from_secret(b: &[u8]) -> Result<Keys, String> {
        let sk = SigningKey::from_slice(b).map_err(|e| e.to_string())?;
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&sk.verifying_key().to_bytes());
        Ok(Keys { sk, pubkey: H32(pk) })
    }
    /// Accepts hex or nsec.
    pub fn parse(s: &str) -> Result<Keys, String> {
        let s = s.trim().to_lowercase(); // bech32 may be all-uppercase (QR codes)
        if s.starts_with("nsec1") {
            let (hrp, data) = bech32::decode(&s).ok_or("bad nsec")?;
            if hrp != "nsec" {
                return Err("bad nsec".into());
            }
            Self::from_secret(&data)
        } else {
            Self::from_secret(&hex::decode(&s).map_err(|e| e.to_string())?)
        }
    }
    /// The raw secret key (the node's libp2p identity is derived from it).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.sk.to_bytes().into()
    }
    pub fn secret_hex(&self) -> String {
        hex::encode(self.sk.to_bytes())
    }
    pub fn nsec(&self) -> String {
        bech32::encode("nsec", &self.sk.to_bytes())
    }
    pub fn npub(&self) -> String {
        bech32::encode("npub", &self.pubkey.0)
    }
    /// Sign a text event (markdown) now.
    pub fn sign(&self, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        self.sign_at(kind, tags, content, now())
    }
    pub fn sign_at(&self, kind: u32, tags: Vec<Vec<String>>, content: String, created_at: u64) -> Event {
        self.sign_data(kind, tags, content.into_bytes(), "", created_at)
    }
    /// Sign an event with any content: bytes of the given media type.
    pub fn sign_data(&self, kind: u32, tags: Vec<Vec<String>>, content: Vec<u8>, content_type: &str, created_at: u64) -> Event {
        let id = Event::compute_id(&self.pubkey, created_at, kind, &tags, &content, content_type);
        let mut aux = [0u8; 32];
        getrandom::getrandom(&mut aux).expect("rng");
        let sig = self.sk.sign_raw(&id.0, &aux).expect("sign");
        let mut sb = [0u8; 64];
        sb.copy_from_slice(&sig.to_bytes());
        Event { id, pubkey: self.pubkey, created_at, kind, tags, content, content_type: content_type.to_string(), sig: H64(sb) }
    }
}

/// Parse a pubkey given as hex or npub.
pub fn parse_pubkey(s: &str) -> Option<H32> {
    let s = s.trim().to_lowercase();
    if s.starts_with("npub1") {
        let (hrp, d) = bech32::decode(&s)?;
        if hrp != "npub" || d.len() != 32 {
            return None;
        }
        let mut b = [0u8; 32];
        b.copy_from_slice(&d);
        Some(H32(b))
    } else {
        H32::from_hex(&s)
    }
}

pub fn npub(pk: &H32) -> String {
    bech32::encode("npub", &pk.0)
}

// ---- filters -------------------------------------------------------------
#[derive(Default, Clone, Debug)]
pub struct Filter {
    pub ids: Option<Vec<String>>,
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u32>>,
    pub tags: Vec<(String, Vec<String>)>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<usize>,
    /// NIP-50 full-text query: every query token must occur in the event's text.
    pub search: Option<String>,
}

impl Filter {
    pub fn from_pb(p: &crate::pb::Filter) -> Filter {
        let hex32 = |v: &Vec<Vec<u8>>| v.iter().filter(|b| b.len() == 32).map(hex::encode).collect::<Vec<_>>();
        Filter {
            ids: (!p.ids.is_empty()).then(|| hex32(&p.ids)),
            authors: (!p.authors.is_empty()).then(|| hex32(&p.authors)),
            kinds: (!p.kinds.is_empty()).then(|| p.kinds.clone()),
            tags: p.tags.iter().filter(|t| t.name.len() == 1).map(|t| (t.name.clone(), t.values.iter().filter_map(value_from_pb).collect())).collect(),
            since: (p.since > 0).then_some(p.since),
            until: (p.until > 0).then_some(p.until),
            limit: (p.limit > 0).then_some(p.limit as usize),
            search: Some(p.search.trim()).filter(|s| !s.is_empty()).map(String::from),
        }
    }
    pub fn to_pb(&self) -> crate::pb::Filter {
        let raw = |v: &Option<Vec<String>>| v.iter().flatten().filter_map(|h| H32::from_hex(h)).map(|h| h.0.to_vec()).collect::<Vec<_>>();
        crate::pb::Filter {
            ids: raw(&self.ids),
            authors: raw(&self.authors),
            kinds: self.kinds.clone().unwrap_or_default(),
            tags: self.tags.iter().map(|(n, v)| crate::pb::TagFilter { name: n.clone(), values: v.iter().map(|x| value_to_pb(x)).collect() }).collect(),
            since: self.since.unwrap_or(0),
            until: self.until.unwrap_or(0),
            limit: self.limit.unwrap_or(0) as u32,
            search: self.search.clone().unwrap_or_default(),
        }
    }

    pub fn matches(&self, e: &Event) -> bool {
        if let Some(ids) = &self.ids {
            let h = e.id.hex();
            if !ids.contains(&h) {
                return false;
            }
        }
        if let Some(a) = &self.authors {
            let h = e.pubkey.hex();
            if !a.contains(&h) {
                return false;
            }
        }
        if let Some(k) = &self.kinds
            && !k.contains(&e.kind) {
                return false;
            }
        if self.since.is_some_and(|s| e.created_at < s) || self.until.is_some_and(|u| e.created_at > u) {
            return false;
        }
        if let Some(q) = &self.search {
            let words: std::collections::HashSet<String> = tokens(&e.searchable_text()).collect();
            if !tokens(q).all(|t| words.contains(&t)) {
                return false;
            }
        }
        self.tags
            .iter()
            .all(|(name, vals)| e.tags_named(name).any(|tv| vals.iter().any(|v| v == tv)))
    }
}

// ---- text ----------------------------------------------------------------

const STOP: &[&str] = &["the", "and", "for", "are", "but", "not", "you", "with", "this", "that", "from", "was", "has", "have", "its", "can", "our", "how", "what", "why", "who", "of", "to", "in", "is", "it", "on", "an", "as", "at", "by", "be", "or", "if", "we", "do", "so", "a", "i"];

/// Search tokens: lowercase alphanumeric runs (any script) of 2..=40 chars, minus stopwords.
/// Shared by the index, NIP-50 filter matching and query parsing so they always agree.
pub fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| (2..=40).contains(&w.chars().count()))
        .map(str::to_lowercase)
        .filter(|w| !STOP.contains(&w.as_str()))
}

impl Event {
    /// The text a search looks at: titles, names, topics and content.
    pub fn searchable_text(&self) -> String {
        if let Some(p) = self.profile() {
            return format!("{} {} {}", p.name, p.model, p.about);
        }
        if let Some(c) = self.capability_spec() {
            let mut s = format!("{} {} ", c.name, c.about);
            for t in self.tags_named("t") {
                s.push_str(t);
                s.push(' ');
            }
            return s;
        }
        let mut s = String::new();
        for t in self.tags.iter().filter(|t| t.len() >= 2 && matches!(t[0].as_str(), "title" | "name" | "t" | "need" | "d" | "alt")) {
            s.push_str(&t[1]);
            s.push(' ');
        }
        s.push_str(self.text());
        s
    }

    /// A profile event's payload.
    pub fn profile(&self) -> Option<crate::pb::Profile> {
        (self.kind == K_PROFILE && self.content_type == TYPE_PROFILE).then(|| crate::pb::decode(&self.content).ok()).flatten()
    }
    /// A capability announcement's payload.
    pub fn capability_spec(&self) -> Option<crate::pb::CapabilitySpec> {
        (self.kind == K_CAPABILITY && self.content_type == TYPE_CAPABILITY).then(|| crate::pb::decode(&self.content).ok()).flatten()
    }
}

// ---- bech32 (NIP-19 npub/nsec) -------------------------------------------
pub mod bech32 {
    const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const GEN: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];

    fn polymod(v: &[u8]) -> u32 {
        let mut chk = 1u32;
        for &x in v {
            let b = chk >> 25;
            chk = ((chk & 0x1ffffff) << 5) ^ x as u32;
            for (i, g) in GEN.iter().enumerate() {
                if (b >> i) & 1 == 1 {
                    chk ^= g;
                }
            }
        }
        chk
    }
    fn expand(hrp: &str) -> Vec<u8> {
        let mut v: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
        v.push(0);
        v.extend(hrp.bytes().map(|b| b & 31));
        v
    }
    fn convert(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
        let (mut acc, mut bits, maxv) = (0u32, 0u32, (1u32 << to) - 1);
        let mut out = Vec::new();
        for &d in data {
            if (d as u32) >> from != 0 {
                return None;
            }
            acc = (acc << from) | d as u32;
            bits += from;
            while bits >= to {
                bits -= to;
                out.push(((acc >> bits) & maxv) as u8);
            }
        }
        if pad {
            if bits > 0 {
                out.push(((acc << (to - bits)) & maxv) as u8);
            }
        } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
            return None;
        }
        Some(out)
    }
    pub fn encode(hrp: &str, data: &[u8]) -> String {
        let d5 = convert(data, 8, 5, true).unwrap();
        let mut v = expand(hrp);
        v.extend(&d5);
        v.extend([0u8; 6]);
        let pm = polymod(&v) ^ 1;
        let mut s = String::from(hrp);
        s.push('1');
        for &x in d5.iter() {
            s.push(CHARSET[x as usize] as char);
        }
        for i in 0..6 {
            s.push(CHARSET[((pm >> (5 * (5 - i))) & 31) as usize] as char);
        }
        s
    }
    pub fn decode(s: &str) -> Option<(String, Vec<u8>)> {
        let s = s.to_lowercase();
        let pos = s.rfind('1')?;
        let (hrp, rest) = (&s[..pos], &s[pos + 1..]);
        if rest.len() < 6 {
            return None;
        }
        let vals: Vec<u8> = rest
            .bytes()
            .map(|c| CHARSET.iter().position(|&x| x == c).map(|p| p as u8))
            .collect::<Option<_>>()?;
        let mut v = expand(hrp);
        v.extend(&vals);
        if polymod(&v) != 1 {
            return None;
        }
        Some((hrp.to_string(), convert(&vals[..vals.len() - 6], 5, 8, false)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![vec!["c".into(), "math".into()]], "hello \"world\"\n".into());
        assert!(e.validate().is_ok());
        assert_eq!(e.text(), "hello \"world\"\n");
        let mut bad = e.clone();
        bad.content.push(b'x');
        assert!(bad.validate().is_err());
        // the media type is signed too
        let mut retyped = e.clone();
        retyped.content_type = "text/plain".into();
        assert_eq!(retyped.validate(), Err("invalid: bad id"));
    }

    #[test]
    fn binary_content() {
        let k = Keys::generate();
        let p = crate::pb::Profile { name: "Ada".into(), agent: true, model: "m".into(), ..Default::default() };
        let e = k.sign_data(K_PROFILE, vec![], crate::pb::encode(&p), TYPE_PROFILE, 100);
        assert!(e.validate().is_ok());
        assert_eq!(e.text(), "", "binary content is not text");
        assert_eq!(e.profile().unwrap().name, "Ada");
        assert!(e.searchable_text().contains("Ada"));
        // text media types must be UTF-8
        let bad = k.sign_data(K_POST, vec![], vec![0xff, 0xfe], "text/plain", 100);
        assert_eq!(bad.validate(), Err("invalid: text content is not UTF-8"));
        assert!(k.sign_data(K_POST, vec![], vec![0xff, 0xfe], "application/octet-stream", 100).validate().is_ok());
    }

    #[test]
    fn bech32_known_vector() {
        // NIP-19 test vector
        let pk = parse_pubkey("npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg").unwrap();
        assert_eq!(pk.hex(), "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e");
        assert_eq!(npub(&pk), "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg");
        let k = Keys::parse("nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5").unwrap();
        assert_eq!(k.secret_hex(), "67dea2ed018072d675f5415ecfaed7d2597555e202d85b3d65ea4e58d2d92ffa");
    }

    #[test]
    fn bip340_vector0() {
        // BIP-340 test vector #0: confirms raw (unhashed) signing as nostr requires.
        let k = Keys::parse("0000000000000000000000000000000000000000000000000000000000000003").unwrap();
        assert_eq!(k.pubkey.hex(), "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9");
        let sig = k.sk.sign_raw(&[0u8; 32], &[0u8; 32]).unwrap();
        assert_eq!(
            hex::encode(sig.to_bytes()),
            "e907831f80848d1069a5371b402410364bdf1c5f8307b0084c55f1ce2dca821525f66a4a85ea8b71e482a74f382d2ce5ebeee8fdb2172f477df4900d310536c0"
        );
    }

    #[test]
    fn protobuf_roundtrip() {
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![vec!["t".into(), "x".into()], vec!["e".into(), "ab".repeat(32), "".into(), "root".into()]], "héllo".into());
        let pb = e.to_pb();
        // ids and pubkeys in tags travel as 32 raw bytes, other values as text
        assert_eq!(pb.tags[1].v[1].raw.len(), 32);
        assert!(pb.tags[1].v[1].text.is_empty() && pb.tags[0].v[1].text == "x");
        let bytes = crate::pb::encode(&pb);
        let back = Event::from_pb(crate::pb::decode(&bytes).unwrap()).unwrap();
        assert!(back.validate().is_ok());
        assert_eq!(back, e);
        // a raw value of the wrong size, or raw and text at once, is not an event
        let mut odd = e.to_pb();
        odd.tags[1].v[1].raw.pop();
        assert!(Event::from_pb(odd).is_none());
        let f = Filter { kinds: Some(vec![11]), tags: vec![("t".into(), vec!["x".into()])], since: Some(5), ..Default::default() };
        let f2 = Filter::from_pb(&crate::pb::decode(&crate::pb::encode(&f.to_pb())).unwrap());
        assert!(f2.matches(&e) && f2.since == Some(5) && f2.limit.is_none());
    }

    #[test]
    fn filter_match() {
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![vec!["c".into(), "bio".into()]], "x".into());
        let f = Filter { kinds: Some(vec![11]), tags: vec![("c".into(), vec!["bio".into()])], ..Default::default() };
        assert!(f.matches(&e));
        let f = Filter { tags: vec![("c".into(), vec!["math".into()])], ..Default::default() };
        assert!(!f.matches(&e));
        // ids travel raw and match exactly
        let f = Filter::from_pb(&Filter { ids: Some(vec![e.id.hex()]), ..Default::default() }.to_pb());
        assert!(f.matches(&e));
        assert_eq!(Filter { ids: Some(vec![e.id.hex()]), ..Default::default() }.to_pb().ids[0].len(), 32);
    }

    fn tags(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter().map(|t| t.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn validate_rejects_each_kind_of_forgery() {
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![], "x".into());
        let mut bad_id = e.clone();
        bad_id.id.0[0] ^= 1;
        assert_eq!(bad_id.validate(), Err("invalid: bad id"));
        let mut bad_sig = e.clone();
        bad_sig.sig.0[5] ^= 1;
        assert!(bad_sig.validate().is_err());
        // someone else's pubkey on our signature: id changes, re-hash it to isolate the sig check
        let mut stolen = e.clone();
        stolen.pubkey = Keys::generate().pubkey;
        stolen.id = Event::compute_id(&stolen.pubkey, stolen.created_at, stolen.kind, &stolen.tags, &stolen.content, &stolen.content_type);
        assert_eq!(stolen.validate(), Err("invalid: bad signature"));
        let mut retimed = e.clone();
        retimed.created_at += 1;
        assert!(retimed.validate().is_err());
        let mut no_point = e.clone();
        no_point.pubkey = H32([0xff; 32]); // not on the curve
        no_point.id = Event::compute_id(&no_point.pubkey, no_point.created_at, no_point.kind, &no_point.tags, &no_point.content, &no_point.content_type);
        assert_eq!(no_point.validate(), Err("invalid: bad pubkey"));
    }

    #[test]
    fn validate_size_limits() {
        let k = Keys::generate();
        assert!(k.sign(K_POST, vec![], "x".repeat(MAX_CONTENT)).validate().is_ok());
        assert_eq!(k.sign(K_POST, vec![], "x".repeat(MAX_CONTENT + 1)).validate(), Err("invalid: content too large"));
        let many = (0..=MAX_TAGS).map(|i| vec!["t".to_string(), i.to_string()]).collect();
        assert_eq!(k.sign(K_POST, many, String::new()).validate(), Err("invalid: too many tags"));
    }

    #[test]
    fn id_is_the_hash_of_the_canonical_encoding() {
        let pk = H32([7; 32]);
        let t = tags(&[&["c", "m"], &["e", &"ab".repeat(32)]]);
        let id = Event::compute_id(&pk, 1700000000, 11, &t, b"a\"b\n", "");
        // a hand-written encoding of EventBody: field 1 pubkey, 2 created_at, 3 kind, 4 tags, 5 content
        let mut body = vec![0x0a, 32];
        body.extend([7u8; 32]);
        body.extend([0x10, 0x80, 0xe2, 0xcf, 0xaa, 0x06]); // varint 1700000000
        body.extend([0x18, 11]);
        body.extend([0x22, 10, 0x0a, 3, 0x0a, 1, b'c', 0x0a, 3, 0x0a, 1, b'm']); // tag ["c","m"]: text values
        body.extend([0x22, 41, 0x0a, 3, 0x0a, 1, b'e', 0x0a, 34, 0x12, 32]); // tag ["e", <raw 32 bytes>]
        body.extend([0xab; 32]);
        body.extend([0x2a, 4, b'a', b'"', b'b', b'\n']);
        let mut h = Sha256::new();
        h.update(b"sametra/event/1\0");
        h.update(&body);
        assert_eq!(id.0, <[u8; 32]>::from(h.finalize()));
        // unset content type and an empty one are the same thing; a set one changes the id
        assert_ne!(id, Event::compute_id(&pk, 1700000000, 11, &t, b"a\"b\n", "text/plain"));
    }

    #[test]
    fn tag_helpers() {
        let k = Keys::generate();
        let e = k.sign(K_POST, tags(&[&["t", "bio"], &["d", "x"], &["q", "a"], &["q", "b"], &["lonely"]]), String::new());
        assert_eq!(e.tag("t"), Some("bio"));
        assert_eq!(e.d_tag(), "x");
        assert_eq!(e.tags_named("q").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(e.tag("lonely"), None, "one-element tags have no value");
        assert_eq!(k.sign(K_POST, vec![], String::new()).d_tag(), "");
    }

    #[test]
    fn ancestry() {
        let k = Keys::generate();
        let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let h = |s: &str| H32::from_hex(s).unwrap();
        let top = k.sign(K_POST, vec![], String::new());
        assert_eq!((top.parent(), top.root(), top.ancestors()), (None, None, vec![]));
        // a child of a top-level topic: the topic is root and parent
        let child = k.sign(K_POST, ancestry_tags(&h(&a), &[]), String::new());
        assert_eq!((child.parent(), child.root(), child.ancestors()), (Some(h(&a)), Some(h(&a)), vec![h(&a)]));
        // deeper: root, path, parent
        let deep = k.sign(K_POST, ancestry_tags(&h(&c), &[h(&a), h(&b)]), String::new());
        assert_eq!((deep.parent(), deep.root()), (Some(h(&c)), Some(h(&a))));
        assert_eq!(deep.ancestors(), [h(&a), h(&b), h(&c)]);
        // very deep: the root and the nearest MAX_PATH ancestors
        let chain: Vec<H32> = (0..100u8).map(|i| H32([i; 32])).collect();
        let t = ancestry_tags(&H32([200; 32]), &chain);
        let e = k.sign(K_POST, t, String::new());
        let anc = e.ancestors();
        assert_eq!(anc.len(), 1 + MAX_PATH + 1);
        assert_eq!((anc[0], anc[1], *anc.last().unwrap()), (chain[0], chain[100 - MAX_PATH], H32([200; 32])));
        let junk = k.sign(K_POST, tags(&[&["e", "nothex", "", "reply"]]), String::new());
        assert_eq!(junk.parent(), None);
    }

    #[test]
    fn kind_classes() {
        let k = Keys::generate();
        let ev = |kind| k.sign(kind, vec![], String::new());
        assert!(ev(K_HEARTBEAT).is_ephemeral() && !ev(K_POST).is_ephemeral());
        assert!(ev(K_PROFILE).is_replaceable() && ev(3).is_replaceable() && ev(10002).is_replaceable());
        assert!(!ev(K_POST).is_replaceable());
        assert!(ev(K_RATING).is_param_replaceable() && ev(K_CAPABILITY).is_param_replaceable());
        assert!(!ev(K_PROFILE).is_param_replaceable() && !ev(K_HEARTBEAT).is_replaceable());
    }

    #[test]
    fn keys_parse_and_encode() {
        let k = Keys::generate();
        assert_eq!(Keys::parse(&k.secret_hex()).unwrap().pubkey, k.pubkey);
        assert_eq!(Keys::parse(&format!("  {}\n", k.nsec())).unwrap().pubkey, k.pubkey);
        assert_eq!(parse_pubkey(&k.npub()), Some(k.pubkey));
        assert_eq!(parse_pubkey(&k.pubkey.hex()), Some(k.pubkey));
        assert_eq!(parse_pubkey(&k.npub().to_uppercase()), Some(k.pubkey), "bech32 is case-insensitive");
        assert!(Keys::parse("zz").is_err());
        assert!(Keys::parse(&"00".repeat(32)).is_err(), "zero is not a valid secret");
        assert_eq!(Keys::parse(&k.nsec().to_uppercase()).unwrap().pubkey, k.pubkey);
        assert!(Keys::parse(&k.npub()).is_err());
        assert_eq!(parse_pubkey(&k.nsec()), None, "secrets are not pubkeys");
        let mut bad = k.npub();
        bad.pop();
        bad.push(if k.npub().ends_with('q') { 'p' } else { 'q' });
        assert_eq!(parse_pubkey(&bad), None, "checksum");
        assert_eq!(parse_pubkey("abcd"), None);
    }

    #[test]
    fn hex_types() {
        assert!(H32::from_hex(&"ab".repeat(31)).is_none());
        assert!(H32::from_hex(&"ab".repeat(33)).is_none());
        assert!(H64::from_hex(&"0g".repeat(64)).is_none());
        let h = H32([0xab; 32]);
        assert_eq!(serde_json::to_string(&h).unwrap(), format!("\"{}\"", "ab".repeat(32)));
        assert!(serde_json::from_str::<H32>("\"abc\"").is_err());
        assert_eq!(format!("{h:?}"), h.hex());
    }

    #[test]
    fn from_pb_rejects_wrong_lengths() {
        let k = Keys::generate();
        let good = k.sign(K_POST, vec![], String::new()).to_pb();
        assert!(Event::from_pb(good.clone()).is_some());
        assert!(Event::from_pb(crate::pb::Event { id: vec![0; 31], ..good.clone() }).is_none());
        assert!(Event::from_pb(crate::pb::Event { pubkey: vec![], ..good.clone() }).is_none());
        assert!(Event::from_pb(crate::pb::Event { sig: vec![0; 32], ..good }).is_none());
    }

    #[test]
    fn filter_fields() {
        let k = Keys::generate();
        let e = k.sign(K_POST, tags(&[&["c", "bio"], &["title", "T"]]), "x".into());
        let m = |f: Filter| f.matches(&e);
        let tag = |n: &str, v: &[&str]| (n.to_string(), v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let id = e.id.hex();
        let pk = e.pubkey.hex();
        assert!(m(Filter::default()));
        assert!(m(Filter { ids: Some(vec![id.clone()]), ..Default::default() }));
        assert!(!m(Filter { ids: Some(vec![id[..10].to_string()]), ..Default::default() }), "ids match exactly");
        assert!(m(Filter { authors: Some(vec![pk, "ff".repeat(32)]), ..Default::default() }));
        assert!(m(Filter { since: Some(e.created_at), until: Some(e.created_at), ..Default::default() }));
        assert!(!m(Filter { since: Some(e.created_at + 1), ..Default::default() }));
        assert!(!m(Filter { until: Some(e.created_at - 1), ..Default::default() }));
        assert!(!m(Filter { kinds: Some(vec![1111]), ..Default::default() }));
        assert!(m(Filter { tags: vec![tag("c", &["math", "bio"])], ..Default::default() }), "OR within one tag");
        assert!(!m(Filter { tags: vec![tag("c", &["bio"]), tag("t", &["x"])], ..Default::default() }), "AND across tags");
        // over the wire: multi-letter tag filters and malformed ids are dropped
        let mut p = Filter { tags: vec![tag("title", &["nope"])], ..Default::default() }.to_pb();
        p.ids.push(vec![1, 2, 3]);
        let f = Filter::from_pb(&p);
        assert!(f.tags.is_empty() && f.ids.as_ref().is_some_and(|i| i.is_empty()));
    }

    #[test]
    fn tokenizer_and_search_filters() {
        let t: Vec<String> = tokens("The CRISPR-Cas9 off-target rate, über 5% (n=40) in β-cells").collect();
        assert_eq!(t, ["crispr", "cas9", "off", "target", "rate", "über", "40", "cells"]);
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![vec!["title".into(), "Mass spectrometry".into()]], "Cohort B shows marker X".into());
        let m = |q: &str| Filter { search: Some(q.into()), ..Default::default() }.matches(&e);
        assert!(m("spectrometry marker") && m("MASS") && m("cohort marker"));
        assert!(m("cohort x"), "one-letter words are not tokens");
        assert!(!m("zebrafish") && !m("marker zebrafish"), "all tokens must match");
        assert!(m("the of"), "a query of only stopwords matches everything");
        let prof = crate::pb::Profile { name: "Doctor Argon".into(), about: "noble gases".into(), ..Default::default() };
        let p = k.sign_data(K_PROFILE, vec![], crate::pb::encode(&prof), TYPE_PROFILE, 1);
        assert!(Filter { search: Some("argon gases".into()), ..Default::default() }.matches(&p));
        let f = Filter::from_pb(&crate::pb::Filter { search: " mass ".into(), kinds: vec![11], ..Default::default() });
        assert_eq!(f.search.as_deref(), Some("mass"));
        assert_eq!(Filter::from_pb(&f.to_pb()).search.as_deref(), Some("mass"));
        assert!(Filter::from_pb(&crate::pb::Filter { search: "  ".into(), ..Default::default() }).search.is_none());
    }

    #[test]
    fn filter_pb_conversion() {
        let p = crate::pb::Filter {
            ids: vec![vec![0xab; 32]],
            tags: vec![
                crate::pb::TagFilter { name: "c".into(), values: vec![value_to_pb("x")] },
                crate::pb::TagFilter { name: "long".into(), values: vec![value_to_pb("y")] },
            ],
            ..Default::default()
        };
        let f = Filter::from_pb(&p);
        assert_eq!(f.ids, Some(vec!["ab".repeat(32)]));
        assert_eq!(f.tags, vec![("c".to_string(), vec!["x".to_string()])]);
        assert!(f.authors.is_none() && f.kinds.is_none() && f.since.is_none() && f.until.is_none() && f.limit.is_none());
        assert_eq!(Filter::from_pb(&f.to_pb()).ids, f.ids);
    }
}
