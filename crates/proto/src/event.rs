//! Nostr (NIP-01) events, filters and keys.

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;

// ---- kinds -------------------------------------------------------------
pub const K_PROFILE: u32 = 0;
pub const K_DELETE: u32 = 5;
pub const K_POST: u32 = 11; // thread root (NIP-7D style)
pub const K_REPLY: u32 = 1111; // reply / comment
pub const K_HEARTBEAT: u32 = 20100; // ephemeral presence + peer address
pub const K_RATING: u32 = 31985; // parameterized replaceable rating
pub const K_COMMUNITY: u32 = 34550; // community definition (NIP-72)
pub const K_CAPABILITY: u32 = 31990; // capability announcement (NIP-89 handler information)
pub const K_JOB: u32 = 5910; // capability request (NIP-90 job request)
pub const K_JOB_RESULT: u32 = 6910; // job result (NIP-90: request kind + 1000)
pub const K_JOB_FEEDBACK: u32 = 7000; // job status: processing / error (NIP-90)

pub const MAX_CONTENT: usize = 64 * 1024;
pub const MAX_TAGS: usize = 256;

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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: H32,
    pub pubkey: H32,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: H64,
}

impl Event {
    pub fn to_pb(&self) -> crate::pb::Event {
        crate::pb::Event {
            id: self.id.0.to_vec(),
            pubkey: self.pubkey.0.to_vec(),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags.iter().map(|t| crate::pb::Tag { v: t.clone() }).collect(),
            content: self.content.clone(),
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
            tags: p.tags.into_iter().map(|t| t.v).collect(),
            content: p.content,
            sig: H64(p.sig.try_into().ok()?),
        })
    }

    pub fn compute_id(pk: &H32, created_at: u64, kind: u32, tags: &[Vec<String>], content: &str) -> H32 {
        let ser = serde_json::to_string(&(0, pk.hex(), created_at, kind, tags, content)).unwrap();
        H32(Sha256::digest(ser.as_bytes()).into())
    }

    /// Full validation: size limits, id hash, schnorr signature.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.content.len() > MAX_CONTENT {
            return Err("invalid: content too large");
        }
        if self.tags.len() > MAX_TAGS {
            return Err("invalid: too many tags");
        }
        if Self::compute_id(&self.pubkey, self.created_at, self.kind, &self.tags, &self.content) != self.id {
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
    pub fn community(&self) -> Option<&str> {
        self.tag("c")
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
    /// Direct parent of a reply: `e` tag marked "reply", else "root", else last `e`.
    pub fn parent(&self) -> Option<H32> {
        let es: Vec<&Vec<String>> = self.tags.iter().filter(|t| t.len() >= 2 && t[0] == "e").collect();
        let pick = es
            .iter()
            .find(|t| t.get(3).map(|m| m == "reply").unwrap_or(false))
            .or_else(|| es.iter().find(|t| t.get(3).map(|m| m == "root").unwrap_or(false)))
            .or(es.last())?;
        H32::from_hex(&pick[1])
    }
    /// Subtopics: the topic this one was opened under (`e` tag marked "topic").
    pub fn topic_parent(&self) -> Option<H32> {
        self.tags.iter().find(|t| t.len() >= 4 && t[0] == "e" && t[3] == "topic").and_then(|t| H32::from_hex(&t[1]))
    }
    pub fn root(&self) -> Option<H32> {
        self.tags
            .iter()
            .find(|t| t.len() >= 4 && t[0] == "e" && t[3] == "root")
            .and_then(|t| H32::from_hex(&t[1]))
            .or_else(|| self.parent())
    }
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
    pub fn secret_hex(&self) -> String {
        hex::encode(self.sk.to_bytes())
    }
    pub fn nsec(&self) -> String {
        bech32::encode("nsec", &self.sk.to_bytes())
    }
    pub fn npub(&self) -> String {
        bech32::encode("npub", &self.pubkey.0)
    }
    pub fn sign(&self, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        self.sign_at(kind, tags, content, now())
    }
    pub fn sign_at(&self, kind: u32, tags: Vec<Vec<String>>, content: String, created_at: u64) -> Event {
        let id = Event::compute_id(&self.pubkey, created_at, kind, &tags, &content);
        let mut aux = [0u8; 32];
        getrandom::getrandom(&mut aux).expect("rng");
        let sig = self.sk.sign_raw(&id.0, &aux).expect("sign");
        let mut sb = [0u8; 64];
        sb.copy_from_slice(&sig.to_bytes());
        Event { id, pubkey: self.pubkey, created_at, kind, tags, content, sig: H64(sb) }
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

fn strs(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect())
}

impl Filter {
    pub fn from_pb(p: &crate::pb::Filter) -> Filter {
        let lower = |v: &Vec<String>| v.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        Filter {
            ids: (!p.ids.is_empty()).then(|| lower(&p.ids)),
            authors: (!p.authors.is_empty()).then(|| lower(&p.authors)),
            kinds: (!p.kinds.is_empty()).then(|| p.kinds.clone()),
            tags: p.tags.iter().filter(|t| t.name.len() == 1).map(|t| (t.name.clone(), t.values.clone())).collect(),
            since: (p.since > 0).then_some(p.since),
            until: (p.until > 0).then_some(p.until),
            limit: (p.limit > 0).then_some(p.limit as usize),
            search: Some(p.search.trim()).filter(|s| !s.is_empty()).map(String::from),
        }
    }
    pub fn to_pb(&self) -> crate::pb::Filter {
        crate::pb::Filter {
            ids: self.ids.clone().unwrap_or_default(),
            authors: self.authors.clone().unwrap_or_default(),
            kinds: self.kinds.clone().unwrap_or_default(),
            tags: self.tags.iter().map(|(n, v)| crate::pb::TagFilter { name: n.clone(), values: v.clone() }).collect(),
            since: self.since.unwrap_or(0),
            until: self.until.unwrap_or(0),
            limit: self.limit.unwrap_or(0) as u32,
            search: self.search.clone().unwrap_or_default(),
        }
    }

    pub fn from_value(v: &Value) -> Filter {
        let mut f = Filter::default();
        let Some(o) = v.as_object() else { return f };
        for (k, v) in o {
            match k.as_str() {
                "ids" => f.ids = strs(v),
                "authors" => f.authors = strs(v),
                "kinds" => f.kinds = v.as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect()),
                "since" => f.since = v.as_u64(),
                "until" => f.until = v.as_u64(),
                "limit" => f.limit = v.as_u64().map(|n| n as usize),
                "search" => f.search = v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from),
                k if k.len() == 2 && k.starts_with('#') => {
                    if let Some(a) = v.as_array() {
                        let vals = a.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                        f.tags.push((k[1..].to_string(), vals));
                    }
                }
                _ => {}
            }
        }
        f
    }

    pub fn matches(&self, e: &Event) -> bool {
        if let Some(ids) = &self.ids {
            let h = e.id.hex();
            if !ids.iter().any(|p| h.starts_with(p.as_str())) {
                return false;
            }
        }
        if let Some(a) = &self.authors {
            let h = e.pubkey.hex();
            if !a.iter().any(|p| h.starts_with(p.as_str())) {
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
        let mut s = String::new();
        for t in self.tags.iter().filter(|t| t.len() >= 2 && matches!(t[0].as_str(), "title" | "name" | "t" | "need" | "d")) {
            s.push_str(&t[1]);
            s.push(' ');
        }
        s.push_str(&self.content);
        s
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
        let json = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(back.validate().is_ok());
        let mut bad = back.clone();
        bad.content.push('x');
        assert!(bad.validate().is_err());
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
        let e = k.sign(K_REPLY, vec![vec!["c".into(), "x".into()], vec!["e".into(), "ab".repeat(32), "".into(), "root".into()]], "héllo".into());
        let bytes = crate::pb::encode(&e.to_pb());
        let back = Event::from_pb(crate::pb::decode(&bytes).unwrap()).unwrap();
        assert!(back.validate().is_ok());
        assert_eq!(back.tags, e.tags);
        assert!(bytes.len() < serde_json::to_string(&e).unwrap().len());
        let f = Filter::from_value(&serde_json::json!({"kinds":[1111],"#c":["x"],"since":5}));
        let f2 = Filter::from_pb(&crate::pb::decode(&crate::pb::encode(&f.to_pb())).unwrap());
        assert!(f2.matches(&e) && f2.since == Some(5) && f2.limit.is_none());
    }

    #[test]
    fn filter_match() {
        let k = Keys::generate();
        let e = k.sign(K_POST, vec![vec!["c".into(), "bio".into()]], "x".into());
        let f = Filter::from_value(&serde_json::json!({"kinds":[11],"#c":["bio"]}));
        assert!(f.matches(&e));
        let f = Filter::from_value(&serde_json::json!({"#c":["math"]}));
        assert!(!f.matches(&e));
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
        stolen.id = Event::compute_id(&stolen.pubkey, stolen.created_at, stolen.kind, &stolen.tags, &stolen.content);
        assert_eq!(stolen.validate(), Err("invalid: bad signature"));
        let mut retimed = e.clone();
        retimed.created_at += 1;
        assert!(retimed.validate().is_err());
        let mut no_point = e.clone();
        no_point.pubkey = H32([0xff; 32]); // not on the curve
        no_point.id = Event::compute_id(&no_point.pubkey, no_point.created_at, no_point.kind, &no_point.tags, &no_point.content);
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
    fn id_is_nip01_hash() {
        // NIP-01: sha256 of [0,pubkey,created_at,kind,tags,content] with JSON escaping
        let pk = H32([7; 32]);
        let t = tags(&[&["c", "m"]]);
        let id = Event::compute_id(&pk, 1700000000, 11, &t, "a\"b\n");
        let ser = format!(r#"[0,"{}",1700000000,11,[["c","m"]],"a\"b\n"]"#, pk.hex());
        assert_eq!(id.0, <[u8; 32]>::from(Sha256::digest(ser.as_bytes())));
    }

    #[test]
    fn tag_helpers() {
        let k = Keys::generate();
        let e = k.sign(K_REPLY, tags(&[&["c", "bio"], &["d", "x"], &["q", "a"], &["q", "b"], &["lonely"]]), String::new());
        assert_eq!(e.community(), Some("bio"));
        assert_eq!(e.d_tag(), "x");
        assert_eq!(e.tags_named("q").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(e.tag("lonely"), None, "one-element tags have no value");
        assert_eq!(k.sign(K_POST, vec![], String::new()).d_tag(), "");
    }

    #[test]
    fn parent_and_root_markers() {
        let k = Keys::generate();
        let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let h = |s: &str| H32::from_hex(s);
        let marked = k.sign(K_REPLY, tags(&[&["e", &a, "", "root"], &["e", &b, "", "reply"]]), String::new());
        assert_eq!((marked.parent(), marked.root()), (h(&b), h(&a)));
        let root_only = k.sign(K_REPLY, tags(&[&["e", &a, "", "root"]]), String::new());
        assert_eq!((root_only.parent(), root_only.root()), (h(&a), h(&a)));
        let unmarked = k.sign(K_REPLY, tags(&[&["e", &a], &["e", &c]]), String::new());
        assert_eq!((unmarked.parent(), unmarked.root()), (h(&c), h(&c)));
        let none = k.sign(K_POST, tags(&[&["q", &a]]), String::new());
        assert_eq!((none.parent(), none.root()), (None, None));
        let junk = k.sign(K_REPLY, tags(&[&["e", "nothex"]]), String::new());
        assert_eq!(junk.parent(), None);
    }

    #[test]
    fn kind_classes() {
        let k = Keys::generate();
        let ev = |kind| k.sign(kind, vec![], String::new());
        assert!(ev(K_HEARTBEAT).is_ephemeral() && !ev(K_POST).is_ephemeral());
        assert!(ev(K_PROFILE).is_replaceable() && ev(3).is_replaceable() && ev(10002).is_replaceable());
        assert!(!ev(K_REPLY).is_replaceable());
        assert!(ev(K_RATING).is_param_replaceable() && ev(K_COMMUNITY).is_param_replaceable());
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
        let m = |v: serde_json::Value| Filter::from_value(&v).matches(&e);
        let id = e.id.hex();
        let pk = e.pubkey.hex();
        assert!(m(serde_json::json!({})));
        assert!(m(serde_json::json!({"ids": [id.to_uppercase()]})), "hex is case-insensitive");
        assert!(m(serde_json::json!({"ids": [&id[..10]]})), "prefixes match");
        assert!(!m(serde_json::json!({"ids": ["00"]})));
        assert!(m(serde_json::json!({"authors": [&pk[..8], "ff"]})));
        assert!(!m(serde_json::json!({"authors": ["0000"]})) || pk.starts_with("0000"));
        assert!(m(serde_json::json!({"since": e.created_at, "until": e.created_at})));
        assert!(!m(serde_json::json!({"since": e.created_at + 1})));
        assert!(!m(serde_json::json!({"until": e.created_at - 1})));
        assert!(!m(serde_json::json!({"kinds": [1111]})));
        assert!(m(serde_json::json!({"#c": ["math", "bio"]})), "OR within one tag");
        assert!(!m(serde_json::json!({"#c": ["bio"], "#t": ["x"]})), "AND across tags");
        assert!(m(serde_json::json!({"#title": ["nope"], "bogus": 1})), "multi-letter tag filters are ignored");
        assert!(Filter::from_value(&serde_json::json!([1, 2])).matches(&e), "non-object = match all");
        let f = Filter::from_value(&serde_json::json!({"limit": 7}));
        assert_eq!(f.limit, Some(7));
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
        let f = Filter::from_value(&serde_json::json!({"search": " mass ", "kinds": [11]}));
        assert_eq!(f.search.as_deref(), Some("mass"));
        assert_eq!(Filter::from_pb(&f.to_pb()).search.as_deref(), Some("mass"));
        assert!(Filter::from_value(&serde_json::json!({"search": "  "})).search.is_none());
    }

    #[test]
    fn filter_pb_conversion() {
        let p = crate::pb::Filter {
            ids: vec!["AB".into()],
            tags: vec![
                crate::pb::TagFilter { name: "c".into(), values: vec!["x".into()] },
                crate::pb::TagFilter { name: "long".into(), values: vec!["y".into()] },
            ],
            ..Default::default()
        };
        let f = Filter::from_pb(&p);
        assert_eq!(f.ids, Some(vec!["ab".to_string()]));
        assert_eq!(f.tags, vec![("c".to_string(), vec!["x".to_string()])]);
        assert!(f.authors.is_none() && f.kinds.is_none() && f.since.is_none() && f.until.is_none() && f.limit.is_none());
        assert_eq!(Filter::from_pb(&f.to_pb()).ids, f.ids);
    }
}
