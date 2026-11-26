//! [`DiskIndex`] — a persistent, keyed full-text index on disk.
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

use crate::tokenizer::{WordTokenizer, words};

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
        let Ok(top) = searcher.search(
            &BooleanQuery::new(terms),
            &TopDocs::with_limit(limit).order_by_score(),
        ) else {
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

    pub fn remove(&mut self, key: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.key, key));
    }

    /// Make every change searchable: commit, then move the index's reader
    /// onto the new commit, on the writer's thread rather than a search's.
    pub fn commit(&mut self) -> tantivy::Result<()> {
        self.writer.commit()?;
        self.reader.reload()
    }

    /// Commit, let the merges that commit started finish, and release the
    /// writer. Dropping a writer instead discards what it has not committed
    /// and abandons its merges half done, to be redone by the next writer.
    pub fn close(mut self) -> tantivy::Result<()> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        self.reader.reload()
    }
}
