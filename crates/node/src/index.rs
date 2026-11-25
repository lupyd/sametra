//! Full-text index over this node's cache: posts (at any depth), profiles
//! and capability announcements.
//!
//! An inverted index from token to compact document numbers. Removal is lazy
//! (a tombstoned slot); the index rebuilds itself once half of it is dead.
//! Scoring: the sum of IDF over matched query tokens, with a bonus for tokens in
//! titles/names, scaled by the share of query tokens matched. Documents that
//! match every token rank above partial matches.

use crate::event::*;
use std::collections::HashMap;

/// Kinds worth searching.
pub fn indexed(kind: u32) -> bool {
    matches!(kind, K_POST | K_CAPABILITY | K_PROFILE | K_FILE)
}

/// Cap on distinct tokens indexed per event (bounds memory for huge posts).
const MAX_TOKENS: usize = 400;

#[derive(Default)]
pub struct Index {
    docs: Vec<Option<H32>>,
    ids: HashMap<H32, u32>,
    post: HashMap<Box<str>, Vec<u32>>,
    title: HashMap<Box<str>, Vec<u32>>,
    dead: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: H32,
    pub score: f64,
    /// every query token matched
    pub all: bool,
}

impl Index {
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn add(&mut self, e: &Event) {
        if !indexed(e.kind) || self.ids.contains_key(&e.id) {
            return;
        }
        let doc = self.docs.len() as u32;
        self.docs.push(Some(e.id));
        self.ids.insert(e.id, doc);
        let mut seen = std::collections::HashSet::new();
        for t in tokens(&e.searchable_text()) {
            if seen.len() >= MAX_TOKENS {
                break;
            }
            if seen.insert(t.clone()) {
                self.post.entry(t.into_boxed_str()).or_default().push(doc);
            }
        }
        let titles = e.tags.iter().filter(|t| t.len() >= 2 && matches!(t[0].as_str(), "title" | "name" | "d")).map(|t| t[1].as_str());
        let mut tseen = std::collections::HashSet::new();
        for t in titles.flat_map(tokens) {
            if tseen.insert(t.clone()) {
                self.title.entry(t.into_boxed_str()).or_default().push(doc);
            }
        }
    }

    pub fn remove(&mut self, id: &H32) {
        if let Some(d) = self.ids.remove(id) {
            self.docs[d as usize] = None;
            self.dead += 1;
        }
    }

    /// True when more than half the slots are dead: the caller should `rebuild`.
    pub fn needs_rebuild(&self) -> bool {
        self.dead > 1000 && self.dead * 2 > self.docs.len()
    }

    pub fn rebuild<'a>(&mut self, events: impl Iterator<Item = &'a Event>) {
        *self = Index::default();
        for e in events {
            self.add(e);
        }
    }

    fn idf(&self, df: usize) -> f64 {
        (1.0 + self.ids.len().max(1) as f64 / df.max(1) as f64).ln()
    }

    /// Ranked hits for `query`, best first, at most `cap`.
    pub fn search(&self, query: &str, cap: usize) -> Vec<Hit> {
        let mut q: Vec<String> = tokens(query).collect();
        q.sort();
        q.dedup();
        if q.is_empty() {
            return vec![];
        }
        let mut acc: HashMap<u32, (f64, usize)> = HashMap::new();
        for t in &q {
            if let Some(p) = self.post.get(t.as_str()) {
                let w = self.idf(p.len());
                for d in p {
                    let a = acc.entry(*d).or_default();
                    a.0 += w;
                    a.1 += 1;
                }
            }
            if let Some(p) = self.title.get(t.as_str()) {
                let w = 0.5 * self.idf(p.len());
                for d in p {
                    if let Some(a) = acc.get_mut(d) {
                        a.0 += w;
                    }
                }
            }
        }
        let n = q.len() as f64;
        let mut hits: Vec<Hit> = acc
            .into_iter()
            .filter_map(|(d, (s, m))| {
                let id = self.docs[d as usize]?;
                let share = m as f64 / n;
                Some(Hit { id, score: s * share * share, all: m == q.len() })
            })
            .collect();
        hits.sort_by(|a, b| b.all.cmp(&a.all).then(b.score.total_cmp(&a.score)).then(a.id.cmp(&b.id)));
        hits.truncate(cap);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(k: &Keys, title: &str, body: &str) -> Event {
        k.sign(K_POST, vec![vec!["c".into(), "m".into()], vec!["title".into(), title.into()]], body.into())
    }

    #[test]
    fn ranks_full_matches_rare_terms_and_titles_first() {
        let k = Keys::generate();
        let mut ix = Index::default();
        let a = post(&k, "Marker X in cohort B", "We measured marker X with mass spectrometry.");
        let b = post(&k, "Cohort notes", "marker levels were normal; spectrometry pending");
        let c = post(&k, "Unrelated", "cohort cohort cohort");
        let d = post(&k, "Spectrometry basics", "an introduction");
        for e in [&a, &b, &c, &d] {
            ix.add(e);
        }
        ix.add(&a); // idempotent
        assert_eq!(ix.len(), 4);
        let h = ix.search("marker spectrometry", 10);
        assert_eq!(h.iter().filter(|x| x.all).count(), 2);
        assert_eq!(h[0].id, a.id, "title match outranks a body-only match");
        assert_eq!(h[1].id, b.id);
        assert!(h[2..].iter().all(|x| !x.all) && h.iter().any(|x| x.id == d.id), "partial matches follow");
        assert!(!h.iter().any(|x| x.id == c.id));
        assert!(ix.search("the of", 10).is_empty(), "stopwords only");
        assert!(ix.search("zebrafish", 10).is_empty());
        assert_eq!(ix.search("MARKER", 1).len(), 1);
    }

    #[test]
    fn only_searchable_kinds_and_removal() {
        let k = Keys::generate();
        let mut ix = Index::default();
        ix.add(&k.sign(K_RATING, vec![vec!["d".into(), "marker".into()]], "marker".into()));
        assert!(ix.is_empty());
        let cap = k.sign(K_CAPABILITY, vec![vec!["d".into(), "ms".into()], vec!["t".into(), "proteomics".into()]], r#"{"name":"Mass spec"}"#.into());
        let file = k.sign(K_FILE, vec![vec!["alt".into(), "tumour biology.csv".into()]], "cohort counts".into());
        ix.add(&cap);
        ix.add(&file);
        assert_eq!(ix.search("proteomics", 5)[0].id, cap.id);
        assert_eq!(ix.search("tumour", 5)[0].id, file.id);
        ix.remove(&cap.id);
        ix.remove(&cap.id);
        assert!(ix.search("proteomics", 5).is_empty());
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn rebuild_after_mass_removal() {
        let k = Keys::generate();
        let mut ix = Index::default();
        let evs: Vec<Event> = (0..3000).map(|i| post(&k, &format!("t{i}"), "common words here")).collect();
        for e in &evs {
            ix.add(e);
        }
        for e in &evs[..2000] {
            ix.remove(&e.id);
        }
        assert!(ix.needs_rebuild());
        ix.rebuild(evs[2000..].iter());
        assert!(!ix.needs_rebuild());
        assert_eq!(ix.search("common", 5000).len(), 1000);
    }

    #[test]
    fn token_cap_bounds_huge_posts() {
        let k = Keys::generate();
        let mut ix = Index::default();
        let body: String = (0..2000).map(|i| format!("w{i} ")).collect();
        ix.add(&post(&k, "big", &body));
        assert!(ix.post.len() <= MAX_TOKENS + 1);
    }
}
