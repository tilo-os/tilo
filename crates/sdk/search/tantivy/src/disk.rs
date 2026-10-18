//! [`DiskIndex`]: a persistent, keyed full-text index on disk.
//!
//! For corpora too large to keep in memory (a home directory's file
//! contents): each document is a key, a version the owner reconciles
//! against (a modification time, say), a title and a body, ranked by BM25
//! over their folded ICU words. The index lives in an mmap'ed directory;
//! the owner writes through one [`DiskWriter`] and searches read the last
//! commit.
//!
//! The reader never polls: tantivy's `OnCommitWithDelay` policy would CRC
//! `meta.json` on a thread of its own every 500 ms for as long as the index
//! is open, which on an idle desktop is two wakeups a second for an index
//! nothing writes. The owner's own [`DiskWriter::commit`] reloads it
//! instead, and [`DiskIndex::reload`] covers a commit made elsewhere.

use std::collections::HashMap;
use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::merge_policy::LogMergePolicy;
use tantivy::query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing,
    TextOptions, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use tilo_search_model::tokenizer::words;

use crate::tokenizer::WordTokenizer;

const WORDS: &str = "tilo_words";

/// A title word counts this many times a body word.
const TITLE_BOOST: f32 = 2.0;

#[derive(Clone, Copy, Debug)]
struct Fields {
    key: Field,
    version: Field,
    title: Field,
    body: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(WORDS)
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let key = builder.add_text_field("key", STRING | STORED);
    let version = builder.add_u64_field("version", INDEXED | STORED | FAST);
    let title = builder.add_text_field("title", words.clone());
    let body = builder.add_text_field("body", words);
    (
        builder.build(),
        Fields {
            key,
            version,
            title,
            body,
        },
    )
}

/// One match: the document's key and its BM25 score.
#[derive(Clone, Debug, PartialEq)]
pub struct DiskHit {
    pub key: String,
    pub score: f32,
}

/// A persistent BM25 index of keyed documents.
pub struct DiskIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

impl DiskIndex {
    /// Open the index in `dir`, creating it (and `dir`) if absent.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the directory cannot be opened or holds an
    /// index of another shape.
    pub fn open_or_create(dir: &Path) -> tantivy::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = schema();
        let index =
            Index::open_in_dir(dir).or_else(|_| Index::create_in_dir(dir, schema.clone()))?;
        if index.schema() != schema {
            return Err(tantivy::TantivyError::SchemaError(
                "the search index has an incompatible schema".into(),
            ));
        }
        index.tokenizers().register(WORDS, WordTokenizer::default());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        // A new reader may start at generation zero and make an existing
        // index look empty.
        reader.reload()?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }

    /// Every document's version, by key, from the last commit.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the stored fields cannot be read.
    pub fn versions(&self) -> tantivy::Result<HashMap<String, u64>> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut out = HashMap::new();
        for segment in searcher.segment_readers() {
            let store = segment.get_store_reader(0)?;
            for doc in store.iter::<TantivyDocument>(segment.alive_bitset()) {
                let Ok(doc) = doc else {
                    continue;
                };
                let key = doc
                    .get_first(self.fields.key)
                    .and_then(|value| value.as_str());
                let version = doc
                    .get_first(self.fields.version)
                    .and_then(|value| value.as_u64());
                if let (Some(key), Some(version)) = (key, version) {
                    out.insert(key.to_owned(), version);
                }
            }
        }
        Ok(out)
    }

    /// A writer with one indexing thread and `heap_bytes` of memory, whose
    /// merges keep segments below `max_docs_per_segment` documents.
    ///
    /// The writer's lock is an advisory lock on `.tantivy-writer.lock`,
    /// which the system releases with the process holding it: the file a
    /// crashed process leaves behind does not refuse the next writer.
    ///
    /// # Errors
    ///
    /// Tantivy's error when another live writer holds the index.
    pub fn writer(
        &self,
        heap_bytes: usize,
        max_docs_per_segment: usize,
    ) -> tantivy::Result<DiskWriter> {
        let writer: IndexWriter = self.index.writer_with_num_threads(1, heap_bytes)?;
        let mut policy = LogMergePolicy::default();
        policy.set_max_docs_before_merge(max_docs_per_segment);
        writer.set_merge_policy(Box::new(policy));
        Ok(DiskWriter {
            writer,
            reader: self.reader.clone(),
            fields: self.fields,
        })
    }

    /// Read the last commit now: for a commit this index's own
    /// [`DiskWriter`] did not make, since the reader does not poll.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the directory cannot be read.
    pub fn reload(&self) -> tantivy::Result<()> {
        self.reader.reload()
    }

    /// Changes with every commit a search can see.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.reader.searcher().generation().generation_id()
    }

    /// The `limit` best documents for `query`'s words, best first.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<DiskHit> {
        let terms: Vec<(Occur, Box<dyn Query>)> = words(query)
            .iter()
            .flat_map(|word| {
                let term = |field| -> Box<dyn Query> {
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, word),
                        IndexRecordOption::WithFreqs,
                    ))
                };
                [
                    (
                        Occur::Should,
                        Box::new(BoostQuery::new(term(self.fields.title), TITLE_BOOST))
                            as Box<dyn Query>,
                    ),
                    (Occur::Should, term(self.fields.body)),
                ]
            })
            .collect();
        if terms.is_empty() || limit == 0 {
            return Vec::new();
        }
        let searcher = self.reader.searcher();
        let Ok(top) = searcher.search(&BooleanQuery::new(terms), &TopDocs::with_limit(limit))
        else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(score, address)| {
                let doc = searcher.doc::<TantivyDocument>(address).ok()?;
                let key = doc.get_first(self.fields.key)?.as_str()?.to_owned();
                Some(DiskHit { key, score })
            })
            .collect()
    }
}

/// Writes to a [`DiskIndex`]; changes become searchable on
/// [`commit`](Self::commit).
pub struct DiskWriter {
    writer: IndexWriter,
    /// The index's reader, reloaded by each commit.
    reader: IndexReader,
    fields: Fields,
}

impl DiskWriter {
    /// Insert or replace the document with `key`.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the indexing thread has failed.
    pub fn upsert(
        &mut self,
        key: &str,
        version: u64,
        title: &str,
        body: &str,
    ) -> tantivy::Result<()> {
        self.remove(key);
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.key, key);
        doc.add_u64(self.fields.version, version);
        doc.add_text(self.fields.title, title);
        doc.add_text(self.fields.body, body);
        self.writer.add_document(doc).map(|_| ())
    }

    /// Remove the document with `key`, if any.
    pub fn remove(&mut self, key: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.key, key));
    }

    /// Make every change searchable: commit, then move the index's reader
    /// onto the new commit, on the writer's thread rather than a search's.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the commit cannot be written or read back.
    pub fn commit(&mut self) -> tantivy::Result<()> {
        self.writer.commit()?;
        self.reader.reload()
    }

    /// Commit, let the merges that commit started finish, and release the
    /// writer. Dropping a writer instead discards what it has not committed
    /// and abandons its merges half done, to be redone by the next writer.
    ///
    /// # Errors
    ///
    /// Tantivy's error when the commit, a merge or the reload fails.
    pub fn close(mut self) -> tantivy::Result<()> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        self.reader.reload()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_incompatible_index_is_refused_without_changing_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = Schema::builder();
        builder.add_text_field("body", STRING | STORED);
        let schema = builder.build();
        Index::create_in_dir(dir.path(), schema.clone()).unwrap();
        let metadata = std::fs::read(dir.path().join("meta.json")).unwrap();

        assert!(matches!(
            DiskIndex::open_or_create(dir.path()),
            Err(tantivy::TantivyError::SchemaError(_))
        ));
        assert_eq!(Index::open_in_dir(dir.path()).unwrap().schema(), schema);
        assert_eq!(
            std::fs::read(dir.path().join("meta.json")).unwrap(),
            metadata
        );
    }

    #[test]
    fn documents_persist_version_and_rank_by_bm25() {
        let dir = tempfile::tempdir().unwrap();
        {
            let index = DiskIndex::open_or_create(dir.path()).unwrap();
            let mut writer = index.writer(15_000_000, 2_000).unwrap();
            writer
                .upsert("/a/shopping.md", 3, "shopping", "apples oranges pears")
                .unwrap();
            writer
                .upsert("/a/notes.md", 5, "notes", "Crème brûlée recipe, apples")
                .unwrap();
            writer.upsert("/a/gone.md", 1, "gone", "retired").unwrap();
            writer.remove("/a/gone.md");
            writer.commit().unwrap();
        }
        let index = DiskIndex::open_or_create(dir.path()).unwrap();
        let versions = index.versions().unwrap();
        assert_eq!(versions.get("/a/notes.md"), Some(&5));
        assert!(!versions.contains_key("/a/gone.md"));
        let hits = index.search("creme", 5);
        assert_eq!(
            hits.first().map(|hit| hit.key.as_str()),
            Some("/a/notes.md")
        );
        let hits = index.search("shopping apples", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].key, "/a/shopping.md", "a title word counts more");
        assert!(index.search("retired", 5).is_empty());
    }

    #[test]
    fn a_commit_is_searchable_without_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let index = DiskIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer(15_000_000, 2_000).unwrap();
        writer.upsert("/a/one.md", 1, "one", "first").unwrap();
        writer.commit().unwrap();
        assert_eq!(index.search("first", 5).len(), 1, "the first commit");
        let seen = index.generation();
        writer.upsert("/a/two.md", 1, "two", "second").unwrap();
        writer.commit().unwrap();
        assert_ne!(index.generation(), seen, "a commit moves the generation");
        assert_eq!(index.search("second", 5).len(), 1, "a later commit");
    }

    #[test]
    fn a_writer_a_crash_left_behind_does_not_lock_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let index = DiskIndex::open_or_create(dir.path()).unwrap();
        // The lock file of a process that died holding the writer.
        std::fs::write(dir.path().join(".tantivy-writer.lock"), b"").unwrap();
        let mut writer = index.writer(15_000_000, 2_000).unwrap();
        writer.upsert("/a/one.md", 1, "one", "first").unwrap();
        assert!(
            index.writer(15_000_000, 2_000).is_err(),
            "a live writer still holds the index"
        );
        // A writer whose thread panicked releases the index as it unwinds.
        std::thread::scope(|scope| {
            let held = scope.spawn(move || {
                let _writer = writer;
                panic!("indexing failed");
            });
            assert!(held.join().is_err());
        });
        let mut writer = index.writer(15_000_000, 2_000).unwrap();
        writer.upsert("/a/two.md", 1, "two", "second").unwrap();
        writer.close().unwrap();
        assert_eq!(index.search("second", 5).len(), 1, "close commits");
        assert!(
            index.search("first", 5).is_empty(),
            "the panicked writer's change was never committed"
        );
        assert!(index.writer(15_000_000, 2_000).is_ok(), "close releases");
    }

    /// tantivy names its `meta.json` poller `thread-tantivy-meta-file-watcher`;
    /// the kernel keeps its first 15 bytes.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_open_index_starts_no_meta_file_poller() {
        let dir = tempfile::tempdir().unwrap();
        let index = DiskIndex::open_or_create(dir.path()).unwrap();
        let mut writer = index.writer(15_000_000, 2_000).unwrap();
        writer.upsert("/a/one.md", 1, "one", "first").unwrap();
        writer.commit().unwrap();
        let pollers: Vec<String> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|task| {
                let comm = task.ok()?.path().join("comm");
                let name = std::fs::read_to_string(comm).ok()?;
                (name.trim() == "thread-tantivy-").then(|| name.trim().to_owned())
            })
            .collect();
        assert!(
            pollers.is_empty(),
            "an open index polls meta.json: {pollers:?}"
        );
    }
}
