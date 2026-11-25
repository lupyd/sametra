//! Full-text index over this node's cache: posts (at any depth), profiles,
//! capability announcements and file metadata.
//!
//! [tantivy](https://github.com/quickwit-oss/tantivy) in memory, rebuilt from the
//! store at start: BM25 ranking over two fields, `title` (titles, names, topics,
//! needs, file names; weighted double) and `body`; English stemming and
//! stopwords. Documents that match every query word rank above partial matches
//! (one AND query, then one OR query for the rest). Adds and removals are
//! committed in batches: when the store flushes, or before a search that would
//! otherwise miss them.

use crate::event::*;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions, Value};
use tantivy::tokenizer::{Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer, TokenStream};
use tantivy::{IndexReader, IndexWriter, TantivyDocument, Term};

/// Kinds worth searching.
pub fn indexed(kind: u32) -> bool {
    matches!(kind, K_POST | K_CAPABILITY | K_PROFILE | K_FILE)
}

/// Text indexed per event, at most (bytes; bounds memory for huge posts).
const MAX_TEXT: usize = 16 * 1024;
const TOKENIZER: &str = "sametra";
/// The indexing thread's memory budget (tantivy's minimum).
const WRITER_BYTES: usize = 15_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: H32,
    pub score: f64,
    /// every query word matched
    pub all: bool,
}

pub struct Index {
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    any: QueryParser,
    every: QueryParser,
    f_id: Field,
    analyzer: Mutex<TextAnalyzer>,
    ids: HashSet<H32>,
    dirty: AtomicBool,
}

impl Default for Index {
    fn default() -> Index {
        Index::new()
    }
}

impl Index {
    pub fn new() -> Index {
        let text = TextOptions::default().set_indexing_options(TextFieldIndexing::default().set_tokenizer(TOKENIZER).set_index_option(IndexRecordOption::WithFreqsAndPositions));
        let mut sb = Schema::builder();
        let f_id = sb.add_bytes_field("id", STORED | tantivy::schema::INDEXED);
        let f_title = sb.add_text_field("title", text.clone());
        let f_body = sb.add_text_field("body", text);
        let index = tantivy::Index::create_in_ram(sb.build());
        let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .filter(StopWordFilter::new(Language::English).expect("English stopwords"))
            .filter(Stemmer::new(Language::English))
            .build();
        index.tokenizers().register(TOKENIZER, analyzer.clone());
        let writer = index.writer_with_num_threads(1, WRITER_BYTES).expect("index writer");
        let reader = index.reader().expect("index reader");
        let mut any = QueryParser::for_index(&index, vec![f_title, f_body]);
        any.set_field_boost(f_title, 2.0);
        let mut every = QueryParser::for_index(&index, vec![f_title, f_body]);
        every.set_field_boost(f_title, 2.0);
        every.set_conjunction_by_default();
        Index { writer: Mutex::new(writer), reader, any, every, f_id, analyzer: Mutex::new(analyzer), ids: HashSet::new(), dirty: AtomicBool::new(false) }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn add(&mut self, e: &Event) {
        if !indexed(e.kind) || !self.ids.insert(e.id) {
            return;
        }
        let schema = self.reader.searcher().schema().clone();
        let (f_title, f_body) = (schema.get_field("title").unwrap(), schema.get_field("body").unwrap());
        let titles: Vec<&str> = e.tags.iter().filter(|t| t.len() >= 2 && matches!(t[0].as_str(), "title" | "name" | "d" | "alt" | "t" | "need")).map(|t| t[1].as_str()).collect();
        let mut body = e.searchable_text();
        if body.len() > MAX_TEXT {
            let mut cut = MAX_TEXT;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
        }
        let mut doc = TantivyDocument::default();
        doc.add_bytes(self.f_id, &e.id.0[..]);
        doc.add_text(f_title, titles.join(" "));
        doc.add_text(f_body, body);
        if let Err(err) = self.writer.lock().unwrap().add_document(doc) {
            tracing::warn!(error = %err, "indexing failed");
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn remove(&mut self, id: &H32) {
        if self.ids.remove(id) {
            self.writer.lock().unwrap().delete_term(Term::from_field_bytes(self.f_id, &id.0));
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Make recent adds and removals searchable (cheap when there are none).
    pub fn commit(&self) {
        if self.dirty.swap(false, Ordering::Relaxed) {
            let r = self.writer.lock().unwrap().commit().and_then(|_| self.reader.reload());
            if let Err(err) = r {
                tracing::warn!(error = %err, "committing the search index failed");
            }
        }
    }

    /// Ranked hits for `query`, best first, at most `cap`: those matching every
    /// word first, then partial matches.
    pub fn search(&self, query: &str, cap: usize) -> Vec<Hit> {
        // nothing left after stopwords: no results (rather than everything)
        let has_terms = {
            let mut a = self.analyzer.lock().unwrap();
            let mut ts = a.token_stream(query);
            ts.advance()
        };
        if cap == 0 || !has_terms {
            return vec![];
        }
        self.commit();
        let searcher = self.reader.searcher();
        let run = |p: &QueryParser, n: usize| -> Vec<(f64, H32)> {
            let (q, _) = p.parse_query_lenient(query);
            searcher
                .search(&q, &TopDocs::with_limit(n).order_by_score())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(score, addr)| {
                    let doc: TantivyDocument = searcher.doc(addr).ok()?;
                    let b = doc.get_first(self.f_id)?.as_bytes()?;
                    Some((score as f64, H32(b.try_into().ok()?)))
                })
                .collect()
        };
        let mut seen = HashSet::new();
        let mut hits: Vec<Hit> = run(&self.every, cap).into_iter().filter(|(_, id)| seen.insert(*id)).map(|(score, id)| Hit { id, score, all: true }).collect();
        if hits.len() < cap {
            let rest = run(&self.any, cap);
            hits.extend(rest.into_iter().filter(|(_, id)| seen.insert(*id)).map(|(score, id)| Hit { id, score, all: false }));
            hits.truncate(cap);
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(k: &Keys, title: &str, body: &str) -> Event {
        k.sign(K_POST, vec![vec!["title".into(), title.into()]], body.into())
    }

    #[test]
    fn ranks_full_matches_and_titles_first() {
        let k = Keys::generate();
        let mut ix = Index::new();
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
        assert_eq!(h[0].id, a.id, "a title match outranks a body-only match");
        assert_eq!(h[1].id, b.id);
        assert!(h[2..].iter().all(|x| !x.all) && h.iter().any(|x| x.id == d.id), "partial matches follow");
        assert!(!h.iter().any(|x| x.id == c.id));
        assert!(ix.search("the of", 10).is_empty(), "stopwords only");
        assert!(ix.search("zebrafish", 10).is_empty());
        assert_eq!(ix.search("MARKER", 1).len(), 1);
    }

    #[test]
    fn stems_and_odd_queries() {
        let k = Keys::generate();
        let mut ix = Index::new();
        let a = post(&k, "Measuring proteins", "we measured protein folding rates");
        ix.add(&a);
        assert_eq!(ix.search("measure protein", 5)[0].id, a.id, "stemming: measuring, measured, measure");
        assert!(ix.search("folding)(: \"rates", 5).iter().any(|h| h.id == a.id), "query syntax errors are forgiven");
    }

    #[test]
    fn only_searchable_kinds_and_removal() {
        let k = Keys::generate();
        let mut ix = Index::new();
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
    fn many_documents_and_removals() {
        let k = Keys::generate();
        let mut ix = Index::new();
        let evs: Vec<Event> = (0..3000).map(|i| post(&k, &format!("t{i}"), "common words here")).collect();
        for e in &evs {
            ix.add(e);
        }
        for e in &evs[..2000] {
            ix.remove(&e.id);
        }
        assert_eq!(ix.search("common", 5000).len(), 1000);
    }
}
