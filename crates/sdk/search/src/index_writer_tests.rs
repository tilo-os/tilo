//! The kept incremental writer: results equal the writer-per-commit path
//! it replaced and a fresh build, failures recover, and a lock nothing
//! releases does not wedge the index.

use std::path::Path;
use std::sync::Arc;

use tantivy::directory::{Directory as _, INDEX_WRITER_LOCK};

use super::*;
use crate::engine::{REBUILD_FRACTION, TantivyEngine, WRITER_HEAP_BYTES, tantivy_engine};
use tantivy::{IndexWriter, Term};

/// The tantivy engine of `index`, made by its first commit.
fn tantivy(index: &mut SearchIndex) -> &mut TantivyEngine {
    index
        .state_mut()
        .engine
        .as_mut()
        .expect("committed")
        .as_any_mut()
        .downcast_mut::<TantivyEngine>()
        .expect("the tantivy engine")
}

/// The commit this index made before it kept a writer, verbatim: a writer
/// opened per commit, every pending record deleted and re-added through
/// its indexing thread, then tantivy's own merges waited for.
fn commit_with_a_writer_per_commit(index: &mut SearchIndex) {
    if !index.is_dirty() {
        return;
    }
    let SearchIndex {
        documents,
        used,
        matching,
        ranks,
        typos,
        state,
        ..
    } = index;
    let state = state.get_mut().unwrap();
    let incremental = state.engine.is_some()
        && !state.rebuild
        && state.pending.len().saturating_mul(REBUILD_FRACTION) < documents.len();
    if !incremental {
        commit(
            state,
            tantivy_engine,
            documents,
            *used,
            *matching,
            *ranks,
            typos,
        )
        .unwrap();
        return;
    }
    let records = Records {
        documents,
        pending: &state.pending,
        fresh: &state.fresh,
        rebuild: state.rebuild,
        used: *used,
        matching: *matching,
        ranks: *ranks,
        typos,
    };
    let engine = state
        .engine
        .as_mut()
        .unwrap()
        .as_any_mut()
        .downcast_mut::<TantivyEngine>()
        .unwrap();
    let mut writer: IndexWriter = engine
        .index
        .writer_with_num_threads(1, WRITER_HEAP_BYTES)
        .unwrap();
    for key in records.pending {
        writer.delete_term(Term::from_field_u64(engine.fields.key, *key));
        let Some(document) = documents.get(key) else {
            continue;
        };
        engine.tantivy_documents(records, document, &mut |built| {
            writer.add_document(built).unwrap();
        });
    }
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    engine.reader.as_ref().unwrap().reload().unwrap();
    state.pending.clear();
    state.fresh.clear();
    state.rebuild = false;
}

struct Paragraphs(Vec<String>);

impl BodySource for Paragraphs {
    fn segment_count(&self) -> usize {
        self.0.len()
    }
    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.0.len())
    }
    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
        (piece == 0).then(|| BodyPiece {
            text: self.0.get(segment).map_or("", String::as_str),
            space_before: segment > 0,
        })
    }
    fn locates(&self, segment: usize) -> bool {
        segment > 0
    }
}

const WORDS: [&str; 12] = [
    "atlas", "budget", "crème", "draft", "estuary", "fjord", "granite", "harbour", "invoice",
    "juniper", "kestrel", "lagoon",
];

/// A small deterministic generator, so a failure replays exactly.
struct Steps(u64);

impl Steps {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33).checked_rem(below).unwrap()
    }

    fn word(&mut self) -> &'static str {
        WORDS[usize::try_from(self.next(WORDS.len() as u64)).unwrap()]
    }

    /// A record with title words, a subtitle, signals and, for some, a body
    /// whose later paragraphs locate their own hits.
    fn document(&mut self, key: u64, version: u64) -> Document {
        let title = format!("{} {} {key}", self.word(), self.word());
        let mut document = Document::new(key)
            .field(title, FieldWeight::TITLE)
            .field(format!("{} v{version}", self.word()), FieldWeight::SUBTITLE)
            .signals(i32::try_from(self.next(50)).unwrap(), 10);
        if self.next(3) == 0 {
            let paragraphs = (0..=self.next(3))
                .map(|_| format!("{} {} {}", self.word(), self.word(), self.word()))
                .collect();
            document = document.body(Arc::new(Paragraphs(paragraphs)));
        }
        document
    }
}

fn queries() -> Vec<String> {
    let mut queries: Vec<String> = WORDS.iter().map(|word| (*word).to_owned()).collect();
    queries.extend(["creme", "ar", "v1", "atlas budget", "17", "granit", ""].map(str::to_owned));
    queries
}

fn pages(index: &SearchIndex, query: &str) -> Vec<Hits> {
    [
        Page::first(500),
        Page {
            offset: 3,
            limit: 4,
        },
    ]
    .into_iter()
    .map(|page| index.search(query, page))
    .collect()
}

#[test]
fn a_kept_writer_answers_as_a_writer_per_commit_and_a_fresh_build() {
    const RECORDS: u64 = 300;
    let mut steps = Steps(0x7105_eea2);
    let initial: Vec<Document> = (0..RECORDS).map(|key| steps.document(key, 0)).collect();
    let mut kept = SearchIndex::new(Matching::LOOSE);
    let mut per_commit = SearchIndex::new(Matching::LOOSE);
    kept.replace_all(initial.clone());
    per_commit.replace_all(initial);
    kept.commit().unwrap();
    commit_with_a_writer_per_commit(&mut per_commit);
    let mut next_key = RECORDS;
    for step in 0..120_u64 {
        // A burst of changes: replaced records, new ones, removals, and a
        // removed key that comes back in the same batch.
        for _ in 0..=steps.next(6) {
            let document = if steps.next(4) == 0 {
                next_key = next_key.checked_add(1).unwrap();
                steps.document(next_key, step)
            } else {
                let key = steps.next(next_key);
                steps.document(key, step)
            };
            kept.upsert(document.clone());
            per_commit.upsert(document);
        }
        for _ in 0..steps.next(3) {
            let key = steps.next(next_key);
            assert_eq!(kept.remove(key), per_commit.remove(key));
            if steps.next(4) == 0 {
                let document = steps.document(key, step);
                kept.upsert(document.clone());
                per_commit.upsert(document);
            }
        }
        kept.commit().unwrap();
        commit_with_a_writer_per_commit(&mut per_commit);
        assert!(!kept.is_dirty());

        let fresh = {
            let mut fresh = SearchIndex::new(Matching::LOOSE);
            fresh.replace_all(kept.documents().cloned());
            fresh.commit().unwrap();
            fresh
        };
        for query in queries() {
            let found = pages(&kept, &query);
            assert_eq!(
                found,
                pages(&per_commit, &query),
                "step {step}, query {query:?}: the kept writer differs from a writer per commit"
            );
            // A fresh build has no deleted documents in its body
            // statistics, so only field hits are compared with it.
            let fields = |pages: Vec<Hits>| -> Vec<Vec<Hit>> {
                pages
                    .into_iter()
                    .map(|page| {
                        page.hits
                            .into_iter()
                            .filter(|hit| hit.rank.is_some())
                            .collect()
                    })
                    .collect()
            };
            assert_eq!(
                fields(found),
                fields(pages(&fresh, &query)),
                "step {step}, query {query:?}: the kept writer differs from a fresh build"
            );
        }
    }
    assert_eq!(
        tantivy(&mut kept).writers_opened,
        1,
        "one writer served every commit"
    );
}

#[test]
fn one_writer_serves_every_incremental_commit_until_a_rebuild() {
    let mut index = SearchIndex::default();
    index.replace_all((0..100).map(|key| Document::new(key).field("row", FieldWeight::TITLE)));
    index.commit().unwrap();
    assert!(
        tantivy(&mut index).writer.is_none(),
        "a bulk build needs no writer"
    );
    for key in 100..130 {
        index.upsert(Document::new(key).field(format!("new {key}"), FieldWeight::TITLE));
        index.commit().unwrap();
    }
    assert_eq!(tantivy(&mut index).writers_opened, 1);
    assert!(tantivy(&mut index).writer.is_some());
    assert_eq!(index.search("new", Page::first(100)).hits.len(), 30);

    // A rebuild replaces the tantivy index, and the writer bound to it.
    index.replace_all((0..100).map(|key| Document::new(key).field("other", FieldWeight::TITLE)));
    index.commit().unwrap();
    assert!(tantivy(&mut index).writer.is_none());
    index.upsert(Document::new(7).field("needle", FieldWeight::TITLE));
    index.commit().unwrap();
    assert_eq!(tantivy(&mut index).writers_opened, 2);
    assert_eq!(index.search("needle", Page::first(10)).hits.len(), 1);
    assert_eq!(index.search("other", Page::first(200)).hits.len(), 99);
}

#[test]
fn a_failed_commit_keeps_its_mutations_and_the_next_one_recovers() {
    let mut index = SearchIndex::default();
    index.replace_all((0..100).map(|key| Document::new(key).field("old row", FieldWeight::TITLE)));
    index.commit().unwrap();
    // A replaced record and a new one; the commit fails after the first of
    // its two tantivy commits has deleted the old version.
    index.upsert(Document::new(3).field("replaced row", FieldWeight::TITLE));
    index.upsert(Document::new(500).field("new row", FieldWeight::TITLE));
    tantivy(&mut index).fail_between_commits = true;
    assert!(index.commit().is_err());
    assert!(index.is_dirty(), "the mutations stay pending");
    assert!(
        tantivy(&mut index).writer.is_none(),
        "the failed writer was dropped"
    );
    let keys = |index: &SearchIndex, query: &str| -> Vec<u64> {
        index
            .search(query, Page::first(200))
            .hits
            .iter()
            .map(|hit| hit.key)
            .collect()
    };
    assert_eq!(
        keys(&index, "old row").len(),
        100,
        "searches read the last commit"
    );
    assert!(keys(&index, "new row").is_empty());

    index.commit().unwrap();
    assert!(!index.is_dirty());
    assert_eq!(
        tantivy(&mut index).writers_opened,
        2,
        "the next commit opened a new writer"
    );
    assert_eq!(keys(&index, "replaced"), [3]);
    assert_eq!(keys(&index, "new row"), [500]);
    assert_eq!(keys(&index, "old row").len(), 99);
    assert_eq!(keys(&index, "row").len(), 101, "every record once");
}

#[test]
fn a_writer_lock_nothing_releases_falls_back_to_a_rebuild() {
    let mut index = SearchIndex::default();
    index.replace_all((0..100).map(|key| Document::new(key).field("row", FieldWeight::TITLE)));
    index.commit().unwrap();
    // What a writer leaked by a panic leaves behind.
    tantivy(&mut index)
        .index
        .directory()
        .atomic_write(Path::new(&INDEX_WRITER_LOCK.filepath), b"")
        .unwrap();
    index.upsert(Document::new(7).field("needle", FieldWeight::TITLE));
    index.commit().unwrap();
    assert_eq!(index.search("needle", Page::first(10)).hits.len(), 1);
    assert_eq!(tantivy(&mut index).writers_opened, 0);
    // The rebuilt index has no lock: incremental commits work again.
    index.upsert(Document::new(8).field("needle", FieldWeight::TITLE));
    index.commit().unwrap();
    assert_eq!(tantivy(&mut index).writers_opened, 1);
    assert_eq!(index.search("needle", Page::first(10)).hits.len(), 2);
}

#[test]
fn a_commit_waiting_for_the_engine_is_made_by_the_first_search() {
    let mut index = SearchIndex::default();
    index.replace_all((0..10).map(|key| Document::new(key).field("row", FieldWeight::TITLE)));
    // What `commit` leaves while the engine's code is still to come.
    index.state_mut().deferred = true;
    assert!(index.is_dirty(), "the mutations wait");
    assert_eq!(index.search("row", Page::first(20)).hits.len(), 10);
    assert!(!index.is_dirty(), "the search made the commit");
    index.upsert(Document::new(10).field("row", FieldWeight::TITLE));
    assert_eq!(
        index.search("row", Page::first(20)).hits.len(),
        10,
        "later searches read the last commit again"
    );
}

#[test]
fn an_index_with_a_kept_writer_is_shared_across_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<SearchIndex>();
}
