//! The engine behind [`SearchIndex`](crate::SearchIndex): tantivy, with
//! its postings in a RAM directory.
//!
//! A [`SearchIndex`](crate::SearchIndex) keeps its records itself and asks
//! an [`Engine`] to index and search them. The engine is reached only
//! through [`MakeEngine`], so a program that should not carry tantivy until
//! someone searches (the browser desktop's start module) can leave it out:
//! with the `lazy-engine` feature, the program hands over a loader
//! ([`defer_engine`]) that fetches the engine's code and passes back
//! [`tantivy_engine`] ([`install_engine`]). Until then commits wait and
//! searches find nothing; [`engine_ready`] tells a surface to say so, and
//! [`engine_generation`] to search again once the engine is in.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write as _;
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::directory::{Directory as _, RamDirectory, TerminatingWrite as _};
use tantivy::merge_policy::{LogMergePolicy, MergeCandidate, MergePolicy, NoMergePolicy};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::TokenizerManager;
use tantivy::{
    DocId, Index, IndexBuilder, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, Score,
    SegmentMeta, SegmentReader, TantivyDocument, TantivyError, Term,
};

use crate::index::{BodySource, Document, Hit, Hits, Page, RECORD_SEGMENT, Signals};
use crate::query::{FIELD_BASE, Matching, Used, compile};
use crate::tokenizer::{GramTokenizer, WordTokenizer};
use crate::typo::Typos;
use crate::weight::{INDEXED_WEIGHTS, KEYWORD_SLOT, Ranks};

const KEY_FIELD: &str = "key";
const SEGMENT_FIELD: &str = "segment";
const BOOST_FIELD: &str = "boost";
const LENGTH_FIELD: &str = "length";
const GRAMS: &str = "tilo_grams";
const WORDS: &str = "tilo_words";

/// Heap for one bulk build. The build is a single segment, so this only
/// sizes the term table it starts with.
const BUILD_HEAP_BYTES: usize = 3_000_000;
/// Heap of the kept writer's one indexing thread: tantivy's minimum. The
/// thread never indexes (a batch is built as its own segment), so this is
/// never allocated.
pub(crate) const WRITER_HEAP_BYTES: usize = 15_000_000;
/// Heap for one incremental batch's segment. It sizes the term table the
/// segment starts with (128K slots) and grows as a large batch needs.
const BATCH_HEAP_BYTES: usize = 400_000;
/// A commit touching more than one in this many records rebuilds instead:
/// a bulk build is cheaper than deleting and re-adding most of the index.
pub(crate) const REBUILD_FRACTION: usize = 8;

/// Why indexing failed: the engine's error, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexError(String);

impl fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for IndexError {}

impl From<TantivyError> for IndexError {
    fn from(error: TantivyError) -> Self {
        Self(error.to_string())
    }
}

/// A custom order for field hits
/// ([`SearchIndex::search_scored`](crate::SearchIndex::search_scored)).
pub type ScoreFn = Arc<dyn Fn(Signals) -> i32 + Send + Sync>;

/// What an [`Engine`] reads of a [`SearchIndex`](crate::SearchIndex): its
/// records, the changes since the last commit, and how it matches.
#[derive(Clone, Copy)]
pub struct Records<'a> {
    pub(crate) documents: &'a BTreeMap<u64, Document>,
    pub(crate) pending: &'a BTreeSet<u64>,
    pub(crate) fresh: &'a BTreeSet<u64>,
    pub(crate) rebuild: bool,
    pub(crate) used: Used,
    pub(crate) matching: Matching,
    pub(crate) ranks: Ranks,
    /// The words of the records' titles and keywords, for typos.
    pub(crate) typos: &'a Typos,
}

/// Indexes a [`SearchIndex`](crate::SearchIndex)'s records and searches
/// them. One engine serves one index for its life.
pub trait Engine: Send + Sync {
    /// Make every change in `records` searchable.
    ///
    /// # Errors
    ///
    /// When indexing failed; searches then answer from the last commit.
    fn commit(&mut self, records: Records<'_>) -> Result<(), IndexError>;

    /// One page of matches, best first, from the last commit.
    fn search(&self, records: Records<'_>, query: &str, page: Page, score: &ScoreFn) -> Hits;

    /// Bytes the committed index occupies.
    fn index_bytes(&self) -> u64;

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Makes the engine for a new index.
pub type MakeEngine = fn() -> Box<dyn Engine>;

#[must_use]
pub fn tantivy_engine() -> Box<dyn Engine> {
    Box::new(TantivyEngine::new())
}

#[cfg(not(feature = "lazy-engine"))]
mod loading {
    use super::MakeEngine;

    /// Linked in: always there.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-engine build has no engine until it is fetched"
    )]
    pub(crate) fn make() -> Option<MakeEngine> {
        Some(super::tantivy_engine)
    }

    /// Linked in: always there.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-engine build has no engine until it is fetched"
    )]
    pub(crate) fn installed() -> Option<MakeEngine> {
        Some(super::tantivy_engine)
    }

    /// Whether no search waits for the engine: none does, it is linked in.
    #[must_use]
    pub fn engine_ready() -> bool {
        true
    }

    /// Fetch the engine now, ahead of a search. It is linked in.
    pub fn prepare_engine() {}

    /// How many times the engine arrived: never, as it was always there.
    #[must_use]
    pub fn engine_generation() -> u64 {
        0
    }
}

#[cfg(feature = "lazy-engine")]
mod loading {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::MakeEngine;

    static MAKE: OnceLock<MakeEngine> = OnceLock::new();
    static LOAD: OnceLock<fn()> = OnceLock::new();
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    /// The engine, or `None` while its code is still to come (and then
    /// fetched).
    pub(crate) fn make() -> Option<MakeEngine> {
        let make = MAKE.get().copied();
        if make.is_none() {
            prepare_engine();
        }
        make
    }

    /// The engine if its code is in, without fetching it.
    pub(crate) fn installed() -> Option<MakeEngine> {
        MAKE.get().copied()
    }

    /// Whether no search waits for the engine: its code is in, or nothing
    /// asked for it. While it is on its way, searches of indexes too large
    /// to answer from their records find nothing and their commits wait.
    #[must_use]
    pub fn engine_ready() -> bool {
        MAKE.get().is_some() || !REQUESTED.load(Ordering::Acquire)
    }

    /// Fetch the engine now, ahead of a search: a surface about to search
    /// (the palette opening) calls it so that results come with the first
    /// keystroke. Fetches once; again after [`engine_load_failed`].
    pub fn prepare_engine() {
        if MAKE.get().is_some() || REQUESTED.swap(true, Ordering::AcqRel) {
            return;
        }
        match LOAD.get() {
            Some(load) => load(),
            None => REQUESTED.store(false, Ordering::Release),
        }
    }

    /// Bumped when the engine arrives: a surface that searched before
    /// searches again once this moved.
    #[must_use]
    pub fn engine_generation() -> u64 {
        GENERATION.load(Ordering::Acquire)
    }

    /// How the program fetches the engine's code: `load` starts the fetch
    /// and, once the code is in, calls [`install_engine`] (or
    /// [`engine_load_failed`]).
    pub fn defer_engine(load: fn()) {
        let _ = LOAD.set(load);
    }

    /// The engine's code is in: indexes commit and search from now on.
    pub fn install_engine(make: MakeEngine) {
        if MAKE.set(make).is_ok() {
            GENERATION.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The fetch failed: the next search fetches again.
    pub fn engine_load_failed() {
        REQUESTED.store(false, Ordering::Release);
    }
}

#[cfg(feature = "lazy-engine")]
pub use loading::{defer_engine, engine_load_failed, install_engine};
pub use loading::{engine_generation, engine_ready, prepare_engine};
pub(crate) use loading::{installed, make};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Fields {
    pub key: Field,
    pub segment: Field,
    pub boost: Field,
    pub length: Field,
    pub grams: [Field; INDEXED_WEIGHTS.len()],
    pub title_words: Field,
    /// Words of keyword fields, matched by typo below the titles' words.
    pub keyword_words: Field,
    pub body_grams: Field,
    pub body_words: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let grams = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(GRAMS)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(WORDS)
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let key = builder.add_u64_field(KEY_FIELD, INDEXED | FAST);
    let segment = builder.add_u64_field(SEGMENT_FIELD, INDEXED | FAST);
    let boost = builder.add_i64_field(BOOST_FIELD, FAST);
    let length = builder.add_u64_field(LENGTH_FIELD, FAST);
    let gram_fields =
        std::array::from_fn(|slot| builder.add_text_field(&format!("grams{slot}"), grams.clone()));
    let title_words = builder.add_text_field("title_words", words.clone());
    let keyword_words = builder.add_text_field("keyword_words", words.clone());
    let body_grams = builder.add_text_field("body_grams", grams);
    let body_words = builder.add_text_field("body_words", words);
    (
        builder.build(),
        Fields {
            key,
            segment,
            boost,
            length,
            grams: gram_fields,
            title_words,
            keyword_words,
            body_grams,
            body_words,
        },
    )
}

/// An index in memory that never starts a thread of its own: its writes go
/// through a [`SingleSegmentIndexWriter`](tantivy::SingleSegmentIndexWriter)
/// (and, natively, an incremental writer), and no doc store compressor
/// thread runs, since nothing is stored. Threads cannot start in the browser.
fn in_memory(schema: Schema) -> Index {
    in_memory_builder(schema.clone())
        .create_in_ram()
        .unwrap_or_else(|_| {
            let mut index = Index::create_in_ram(schema);
            index.set_tokenizers(tokenizers());
            index
        })
}

/// [`in_memory`]'s settings and tokenizers, for an index or a single
/// segment writer to start from.
fn in_memory_builder(schema: Schema) -> IndexBuilder {
    let settings = IndexSettings {
        docstore_compress_dedicated_thread: false,
        ..IndexSettings::default()
    };
    Index::builder()
        .schema(schema)
        .settings(settings)
        .tokenizers(tokenizers())
}

fn tokenizers() -> TokenizerManager {
    let tokenizers = TokenizerManager::default();
    tokenizers.register(GRAMS, GramTokenizer::default());
    tokenizers.register(WORDS, WordTokenizer::default());
    tokenizers
}

fn manual_reader(index: &Index) -> tantivy::Result<IndexReader> {
    index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
}

/// Merge what tantivy's default policy would merge after a commit, and wait
/// for it, as a writer closed after each commit did: every commit returns a
/// settled index, so the segments a search sees, and with them the body
/// statistics BM25 orders by, do not depend on a merge's timing.
fn settle_merges(index: &Index, writer: &mut IndexWriter) -> tantivy::Result<()> {
    /// Each round merges a level into one segment, so a cascade is a few
    /// rounds; the bound only keeps a policy surprise from spinning.
    const MAX_ROUNDS: usize = 16;
    let policy = LogMergePolicy::default();
    for _ in 0..MAX_ROUNDS {
        let candidates = policy.compute_merge_candidates(&index.searchable_segment_metas()?);
        if candidates.is_empty() {
            break;
        }
        for MergeCandidate(segments) in candidates {
            writer.merge(&segments).wait()?;
        }
    }
    Ok(())
}

fn segment_text(body: &dyn BodySource, segment: usize) -> String {
    let mut text = String::new();
    for piece in 0..body.piece_count(segment) {
        let Some(piece) = body.piece(segment, piece) else {
            continue;
        };
        if piece.space_before && !text.is_empty() {
            text.push(' ');
        }
        text.push_str(piece.text);
    }
    text
}

/// The tantivy index of one [`SearchIndex`](crate::SearchIndex).
pub(crate) struct TantivyEngine {
    pub(crate) fields: Fields,
    pub(crate) index: Index,
    /// `None` only if tantivy could not open a reader on an index in
    /// memory; searches then find nothing.
    pub(crate) reader: Option<IndexReader>,
    /// The incremental writer, opened by the first incremental commit and
    /// kept for every later one (see [`Engine::commit`]).
    pub(crate) writer: Option<IndexWriter>,
    /// Incremental writers opened over this index's life.
    pub(crate) writers_opened: u32,
    /// Fail the next incremental commit between its two commits.
    #[cfg(test)]
    pub(crate) fail_between_commits: bool,
}

impl Engine for TantivyEngine {
    /// A few changes commit incrementally through one tantivy writer the
    /// engine keeps from its first incremental commit on: opening a writer
    /// (its lock, its meta and six threads) costs more than the records a
    /// typical commit carries. Between commits the writer holds nothing
    /// staged and its threads sleep without a timeout, so a kept writer
    /// costs no wakeups and no arena; dropping the index drops it.
    ///
    /// A failed writer is dropped with whatever it had staged, and the next
    /// commit opens a new one.
    fn commit(&mut self, records: Records<'_>) -> Result<(), IndexError> {
        let incremental = !cfg!(target_arch = "wasm32")
            && !records.rebuild
            && records.pending.len().saturating_mul(REBUILD_FRACTION) < records.documents.len();
        let committed = if incremental {
            match self.commit_incremental(records) {
                // A writer lock nothing will release (a writer leaked by a
                // panic, say) would refuse every incremental commit from
                // now on: a rebuild gives the records an index without it.
                Err(TantivyError::LockFailure(..)) => self.rebuild_index(records),
                other => other,
            }
        } else {
            self.rebuild_index(records)
        };
        committed.map_err(IndexError::from)
    }

    /// Hits come best rank first, and in key order within a rank; body hits
    /// follow every field hit, by BM25. An empty query browses every record
    /// in key order.
    fn search(&self, records: Records<'_>, query: &str, page: Page, score: &ScoreFn) -> Hits {
        let score = Arc::clone(score);
        let Some(reader) = &self.reader else {
            return Hits::default();
        };
        let searcher = reader.searcher();
        // Never ask the collector for more slots than there are documents.
        let documents = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
        let limit = page.limit.min(documents.saturating_sub(page.offset));
        if limit == 0 {
            return Hits::default();
        }
        let Some(compiled) = compile(
            &self.fields,
            records.used,
            records.matching,
            records.ranks,
            records.typos,
            query,
        ) else {
            return Hits::default();
        };
        let collector = TopDocs::with_limit(limit.saturating_add(1))
            .and_offset(page.offset)
            .tweak_score(move |segment_reader: &SegmentReader| {
                let fast = segment_reader.fast_fields();
                let u64s = |name: &str| {
                    fast.u64(name)
                        .ok()
                        .map(|column| column.first_or_default_col(0))
                };
                let keys = u64s(KEY_FIELD);
                let segments = u64s(SEGMENT_FIELD);
                let lengths = u64s(LENGTH_FIELD);
                let boosts = fast
                    .i64(BOOST_FIELD)
                    .ok()
                    .map(|column| column.first_or_default_col(0));
                let score = score.clone();
                move |doc: DocId, raw: Score| {
                    let value = |column: &Option<Arc<dyn tantivy::columnar::ColumnValues<u64>>>,
                                 default| {
                        column
                            .as_ref()
                            .map_or(default, |column| column.get_val(doc))
                    };
                    let signals = Signals {
                        rank: (raw >= FIELD_BASE).then(|| rank_of(raw)),
                        boost: boosts.as_ref().map_or(0, |column| {
                            i32::try_from(column.get_val(doc)).unwrap_or_default()
                        }),
                        length: u32::try_from(value(&lengths, 0)).unwrap_or(u32::MAX),
                    };
                    let ordered = signals.rank.map_or(0, |_| score(signals));
                    (
                        signals.rank.is_some(),
                        ordered,
                        raw,
                        Reverse(value(&keys, 0)),
                        Reverse(value(&segments, RECORD_SEGMENT)),
                        signals.rank,
                    )
                }
            });
        let found = match searcher.search(compiled.as_ref(), &collector) {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(%error, "search failed");
                return Hits::default();
            }
        };
        let more = found.len() > limit;
        let hits = found
            .into_iter()
            .take(limit)
            .map(
                |((_, score, _, Reverse(key), Reverse(segment), rank), _)| Hit {
                    key,
                    segment: (segment != RECORD_SEGMENT)
                        .then(|| usize::try_from(segment).unwrap_or(usize::MAX)),
                    rank,
                    score,
                },
            )
            .collect();
        Hits { hits, more }
    }

    fn index_bytes(&self) -> u64 {
        self.reader.as_ref().map_or(0, |reader| {
            reader
                .searcher()
                .space_usage()
                .map_or(0, |usage| usage.total().get_bytes())
        })
    }

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl TantivyEngine {
    fn new() -> Self {
        let (schema, fields) = schema();
        let index = in_memory(schema);
        let reader = manual_reader(&index)
            .inspect_err(|error| tracing::error!(%error, "could not open a search index"))
            .ok();
        Self {
            fields,
            index,
            reader,
            writer: None,
            writers_opened: 0,
            #[cfg(test)]
            fail_between_commits: false,
        }
    }

    pub(crate) fn tantivy_documents(
        &self,
        records: Records<'_>,
        document: &Document,
        out: &mut impl FnMut(TantivyDocument),
    ) {
        let fields = &self.fields;
        let mut record = TantivyDocument::default();
        record.add_u64(fields.key, document.key);
        record.add_u64(fields.segment, RECORD_SEGMENT);
        record.add_i64(fields.boost, i64::from(document.signals.0));
        record.add_u64(fields.length, u64::from(document.signals.1));
        for (text, weight) in &document.fields {
            let slot = weight.slot();
            if let Some(grams) = fields.grams.get(slot) {
                record.add_text(*grams, text);
            }
            // Only title and keyword words are matched by typo.
            if records.matching.typos {
                if slot == 0 {
                    record.add_text(fields.title_words, text);
                } else if slot == KEYWORD_SLOT {
                    record.add_text(fields.keyword_words, text);
                }
            }
        }
        if let Some(body) = &document.body {
            let mut inline = String::new();
            for segment in 0..body.segment_count() {
                let text = segment_text(body.as_ref(), segment);
                if body.locates(segment) {
                    let mut located = TantivyDocument::default();
                    located.add_u64(fields.key, document.key);
                    located.add_u64(fields.segment, segment as u64);
                    located.add_text(fields.body_grams, &text);
                    located.add_text(fields.body_words, &text);
                    out(located);
                } else if !text.is_empty() {
                    if !inline.is_empty() {
                        inline.push(' ');
                    }
                    inline.push_str(&text);
                }
            }
            if !inline.is_empty() {
                record.add_text(fields.body_grams, &inline);
                record.add_text(fields.body_words, &inline);
            }
        }
        out(record);
    }

    /// `documents` indexed as the one segment of a new index in memory.
    fn build<'a>(
        &self,
        records: Records<'_>,
        heap_bytes: usize,
        documents: impl Iterator<Item = &'a Document>,
    ) -> tantivy::Result<Index> {
        let mut writer = in_memory_builder(self.index.schema())
            .single_segment_index_writer::<TantivyDocument>(RamDirectory::create(), heap_bytes)?;
        let mut failed = None;
        for document in documents {
            self.tantivy_documents(records, document, &mut |built| {
                if failed.is_none()
                    && let Err(error) = writer.add_document(built)
                {
                    failed = Some(error);
                }
            });
        }
        if let Some(error) = failed {
            return Err(error);
        }
        writer.finalize()
    }

    fn rebuild_index(&mut self, records: Records<'_>) -> tantivy::Result<()> {
        let index = self.build(records, BUILD_HEAP_BYTES, records.documents.values())?;
        let reader = manual_reader(&index)?;
        // The kept writer writes to the index this one replaces.
        self.writer = None;
        self.index = index;
        self.reader = Some(reader);
        Ok(())
    }

    /// One indexing thread, which never indexes: [`Self::batch_segment`]
    /// does. It never merges on its own either: [`settle_merges`] merges as
    /// each commit ends.
    fn open_writer(&mut self) -> tantivy::Result<IndexWriter> {
        let writer: IndexWriter = self.index.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        self.writers_opened = self.writers_opened.saturating_add(1);
        Ok(writer)
    }

    /// The pending records that still exist, as a segment of this index
    /// that no segment list names yet: built in an index of its own sized to
    /// the batch, then its files moved into this index's directory.
    ///
    /// The writer's own indexing thread would start every segment with the
    /// term table of its 15 MB minimum heap, 4 MB written and read back even
    /// for one record, which costs more than the record.
    fn batch_segment(&self, records: Records<'_>) -> tantivy::Result<Option<SegmentMeta>> {
        let mut documents = records
            .pending
            .iter()
            .filter_map(|key| records.documents.get(key))
            .peekable();
        if documents.peek().is_none() {
            return Ok(None);
        }
        let built = self.build(records, BATCH_HEAP_BYTES, documents)?;
        let Some(segment) = built.searchable_segment_metas()?.into_iter().next() else {
            return Ok(None);
        };
        // A meta of this index first: while it lives, its files are never
        // collected as garbage.
        let meta = self.index.new_segment_meta(segment.id(), segment.max_doc());
        let (from, to) = (built.directory(), self.index.directory());
        // The built index has this one segment: its files are the index's.
        for path in built.load_metas()?.list_segment_files() {
            if !from.exists(&path)? {
                continue;
            }
            let bytes = from.open_read(&path)?.read_bytes()?;
            let mut file = to.open_write(&path)?;
            file.write_all(&bytes)?;
            file.terminate()?;
        }
        Ok(Some(meta))
    }

    /// Delete the pending records from the index, then add their segment,
    /// commit and merge on the kept writer, then move the reader onto the
    /// commit. The writer is taken out for the commit and put back only once
    /// it has succeeded: on an error it drops here, discarding what it
    /// staged and releasing the index's lock.
    fn commit_incremental(&mut self, records: Records<'_>) -> tantivy::Result<()> {
        let mut writer = match self.writer.take() {
            Some(writer) => writer,
            None => self.open_writer()?,
        };
        let mut replaces = false;
        for key in records.pending.difference(records.fresh) {
            writer.delete_term(Term::from_field_u64(self.fields.key, *key));
            replaces |= records.documents.contains_key(key);
        }
        if let Some(segment) = self.batch_segment(records)? {
            // An added segment takes every delete the writer has queued but
            // not yet applied as its own. Deletes of removed records find
            // nothing in it, but a replaced record would lose its new
            // version: apply those to the old segments first. The commit
            // between is never read; the reader moves once the batch is in.
            if replaces {
                writer.commit()?;
                #[cfg(test)]
                if std::mem::take(&mut self.fail_between_commits) {
                    return Err(TantivyError::SystemError("injected".into()));
                }
            }
            writer.add_segment(segment)?;
        }
        writer.commit()?;
        settle_merges(&self.index, &mut writer)?;
        self.writer = Some(writer);
        if let Some(reader) = &self.reader {
            return reader.reload();
        }
        self.reader = Some(manual_reader(&self.index)?);
        Ok(())
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a field score is FIELD_BASE plus a rank of at most a few hundred"
)]
fn rank_of(score: Score) -> i32 {
    (score - FIELD_BASE).floor() as i32
}
