//! [`SearchIndex`]: a retained set of records, searchable through tantivy.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::columnar::ColumnValues;
use tantivy::query::{BooleanQuery, ConstScoreQuery, EnableScoring, Occur, Query};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::{
    DocAddress, DocId, DocSet, Index, IndexReader, IndexWriter, ReloadPolicy, Score, Searcher,
    SegmentReader, SingleSegmentIndexWriter, TERMINATED, TantivyDocument, Term,
};

use crate::query::{BoxQuery, Matching, Used, plan};
use crate::tokenizer::{GramTokenizer, WordTokenizer};
use crate::weight::{FieldWeight, INDEXED_WEIGHTS};

const KEY_FIELD: &str = "key";
const SEGMENT_FIELD: &str = "segment";
const GRAMS: &str = "tilo_grams";
const WORDS: &str = "tilo_words";

/// The segment value of a record's own document.
pub(crate) const RECORD_SEGMENT: u64 = u64::MAX;

/// Heap for one bulk build. The build is a single segment, so this only
/// sizes the term table it starts with.
const BUILD_HEAP_BYTES: usize = 3_000_000;
/// Heap for an incremental commit: tantivy's minimum per indexing thread.
const WRITER_HEAP_BYTES: usize = 15_000_000;
/// A commit touching more than one in this many records rebuilds instead:
/// a bulk build is cheaper than deleting and re-adding most of the index.
const REBUILD_FRACTION: usize = 8;

/// One contiguous run of body text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    pub space_before: bool,
}

/// Long text a record is found by, owned by the caller: the index reads it
/// while indexing and never copies it into the retained record.
///
/// A body is a sequence of *segments* (a paragraph, a block, a line), each
/// a sequence of [`BodyPiece`]s. The source must not change while indexed.
pub trait BodySource: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<BodyPiece<'_>>;

    /// Whether a match in `segment` is its own hit ([`Hit::segment`]).
    /// Other segments are found as the record itself.
    fn locates(&self, _segment: usize) -> bool {
        false
    }
}

/// One record: a caller-chosen key, weighted fields, and optionally a body.
#[derive(Clone)]
pub struct Document {
    key: u64,
    fields: Vec<(String, FieldWeight)>,
    body: Option<Arc<dyn BodySource>>,
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("key", &self.key)
            .field("fields", &self.fields)
            .field("body", &self.body.is_some())
            .finish()
    }
}

impl Document {
    /// A record with no fields yet. Keys order hits of equal relevance.
    #[must_use]
    pub const fn new(key: u64) -> Self {
        Self {
            key,
            fields: Vec::new(),
            body: None,
        }
    }

    /// Add a short field, found and ranked by its weight. Values never
    /// match across each other.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: FieldWeight) -> Self {
        self.fields.push((text.into(), weight));
        self
    }

    /// Attach a body. A body match follows every field match.
    #[must_use]
    pub fn body(mut self, body: Arc<dyn BodySource>) -> Self {
        self.body = Some(body);
        self
    }

    #[must_use]
    pub const fn key(&self) -> u64 {
        self.key
    }

    #[must_use]
    pub fn fields(&self) -> &[(String, FieldWeight)] {
        &self.fields
    }

    #[must_use]
    pub const fn body_source(&self) -> Option<&Arc<dyn BodySource>> {
        self.body.as_ref()
    }
}

/// One match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub key: u64,
    /// The body segment a located match is in; `None` for the record itself.
    pub segment: Option<usize>,
    /// The field rank (`0..=`[`MAX_FIELD_RANK`](crate::MAX_FIELD_RANK)) of a
    /// field match; `None` for a match in body text alone.
    pub rank: Option<i32>,
}

/// One page of matches, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    pub hits: Vec<Hit>,
    /// Whether more matches follow this page.
    pub more: bool,
}

/// Which page of matches to return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    /// Matches to skip.
    pub offset: usize,
    pub limit: usize,
}

impl Page {
    #[must_use]
    pub const fn first(limit: usize) -> Self {
        Self { offset: 0, limit }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Fields {
    pub key: Field,
    pub segment: Field,
    pub grams: [Field; INDEXED_WEIGHTS.len()],
    /// Words of title fields, for typo matching.
    pub title_words: Field,
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
    let gram_fields =
        std::array::from_fn(|slot| builder.add_text_field(&format!("grams{slot}"), grams.clone()));
    let title_words = builder.add_text_field("title_words", words.clone());
    let body_grams = builder.add_text_field("body_grams", grams);
    let body_words = builder.add_text_field("body_words", words);
    (
        builder.build(),
        Fields {
            key,
            segment,
            grams: gram_fields,
            title_words,
            body_grams,
            body_words,
        },
    )
}

fn in_memory(schema: Schema) -> Index {
    let index = Index::create_in_ram(schema);
    index.tokenizers().register(GRAMS, GramTokenizer::default());
    index.tokenizers().register(WORDS, WordTokenizer::default());
    index
}

fn manual_reader(index: &Index) -> tantivy::Result<IndexReader> {
    index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
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

/// A retained, in-memory search index over [`Document`]s.
///
/// Records are kept by key; tantivy holds their postings in a RAM
/// directory. Mutations are cheap and deferred: [`commit`](Self::commit)
/// applies them in one batch, as an incremental tantivy commit or, when
/// much changed, a bulk rebuild. Searches read the last commit.
///
/// Every field is found by substring, folded (see [`fold`](crate::fold)):
/// the query as a phrase of grams. The best field and tier rank a hit
/// (see [`MatchTier`](crate::MatchTier)); hits of equal rank come in key
/// order, and body hits follow by BM25.
pub struct SearchIndex {
    fields: Fields,
    index: Index,
    /// `None` only if tantivy could not open a reader on an index in
    /// memory; searches then find nothing.
    reader: Option<IndexReader>,
    matching: Matching,
    used: Used,
    documents: BTreeMap<u64, Document>,
    pending: BTreeSet<u64>,
    rebuild: bool,
}

impl fmt::Debug for SearchIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchIndex")
            .field("records", &self.documents.len())
            .field("pending", &self.pending.len())
            .field("rebuild", &self.rebuild)
            .field("matching", &self.matching)
            .finish_non_exhaustive()
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new(Matching::default())
    }
}

impl SearchIndex {
    /// An empty index matching as `matching` says.
    #[must_use]
    pub fn new(matching: Matching) -> Self {
        let (schema, fields) = schema();
        let index = in_memory(schema);
        let reader = manual_reader(&index)
            .inspect_err(|error| tracing::error!(%error, "could not open a search index"))
            .ok();
        Self {
            fields,
            index,
            reader,
            matching,
            used: Used::default(),
            documents: BTreeMap::new(),
            pending: BTreeSet::new(),
            rebuild: false,
        }
    }

    /// An independent copy of the retained records, indexed on the next
    /// commit.
    #[must_use]
    pub fn detached(&self) -> Self {
        let mut copy = Self::new(self.matching);
        copy.documents.clone_from(&self.documents);
        copy.used = self.used;
        copy.rebuild = true;
        copy
    }

    /// Number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    #[must_use]
    pub fn get(&self, key: u64) -> Option<&Document> {
        self.documents.get(&key)
    }

    /// Records in key order.
    pub fn documents(&self) -> impl Iterator<Item = &Document> {
        self.documents.values()
    }

    fn note_used(&mut self, document: &Document) {
        for (_, weight) in &document.fields {
            if let Some(used) = self.used.slots.get_mut(weight.slot()) {
                *used = true;
            }
        }
        self.used.body |= document.body.is_some();
    }

    /// Insert or replace the record with `document`'s key.
    pub fn upsert(&mut self, document: Document) {
        self.note_used(&document);
        self.pending.insert(document.key);
        self.documents.insert(document.key, document);
    }

    /// Remove a record. Returns whether it existed.
    pub fn remove(&mut self, key: u64) -> bool {
        let existed = self.documents.remove(&key).is_some();
        if existed {
            self.pending.insert(key);
        }
        existed
    }

    /// Replace every record.
    pub fn replace_all(&mut self, documents: impl IntoIterator<Item = Document>) {
        self.documents.clear();
        self.used = Used::default();
        for document in documents {
            self.note_used(&document);
            self.documents.insert(document.key, document);
        }
        self.pending.clear();
        self.rebuild = true;
    }

    /// Whether mutations wait for [`commit`](Self::commit).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.rebuild || !self.pending.is_empty()
    }

    /// Make every mutation searchable.
    ///
    /// # Errors
    ///
    /// Returns tantivy's error when indexing fails; the index then still
    /// answers from its last commit and the mutations stay pending.
    pub fn commit(&mut self) -> tantivy::Result<()> {
        if !self.is_dirty() {
            return Ok(());
        }
        let incremental = !cfg!(target_arch = "wasm32")
            && !self.rebuild
            && self.pending.len().saturating_mul(REBUILD_FRACTION) < self.documents.len();
        if incremental {
            self.commit_incremental()?;
        } else {
            self.rebuild_index()?;
        }
        self.pending.clear();
        self.rebuild = false;
        Ok(())
    }

    fn tantivy_documents(&self, document: &Document, out: &mut impl FnMut(TantivyDocument)) {
        let fields = &self.fields;
        let mut record = TantivyDocument::default();
        record.add_u64(fields.key, document.key);
        record.add_u64(fields.segment, RECORD_SEGMENT);
        for (text, weight) in &document.fields {
            let slot = weight.slot();
            if let Some(grams) = fields.grams.get(slot) {
                record.add_text(*grams, text);
            }
            // Only title words are matched by typo.
            if slot == 0 && self.matching.typos {
                record.add_text(fields.title_words, text);
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

    fn rebuild_index(&mut self) -> tantivy::Result<()> {
        let index = in_memory(self.index.schema());
        let mut writer = SingleSegmentIndexWriter::<TantivyDocument>::new(index, BUILD_HEAP_BYTES)?;
        let mut failed = None;
        for document in self.documents.values() {
            self.tantivy_documents(document, &mut |built| {
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
        let index = writer.finalize()?;
        let reader = manual_reader(&index)?;
        self.index = index;
        self.reader = Some(reader);
        Ok(())
    }

    fn commit_incremental(&mut self) -> tantivy::Result<()> {
        let mut writer: IndexWriter = self.index.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
        let mut failed = None;
        for key in &self.pending {
            writer.delete_term(Term::from_field_u64(self.fields.key, *key));
            let Some(document) = self.documents.get(key) else {
                continue;
            };
            self.tantivy_documents(document, &mut |built| {
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
        writer.commit()?;
        writer.wait_merging_threads()?;
        if let Some(reader) = &self.reader {
            return reader.reload();
        }
        self.reader = Some(manual_reader(&self.index)?);
        Ok(())
    }

    /// One page of matches for `query`, from the last commit.
    ///
    /// Hits come best rank first, and in index order within a rank; body
    /// hits follow every field hit, by BM25. An empty query browses every
    /// record in index order.
    #[must_use]
    pub fn search(&self, query: &str, page: Page) -> Hits {
        let Some(reader) = &self.reader else {
            return Hits::default();
        };
        if page.limit == 0 {
            return Hits::default();
        }
        let plan = plan(&self.fields, self.used, self.matching, query);
        let searcher = reader.searcher();
        let wanted = page.offset.saturating_add(page.limit).saturating_add(1);
        let mut walk = Walk {
            searcher: &searcher,
            columns: searcher
                .segment_readers()
                .iter()
                .map(|reader| {
                    Some((
                        u64_column(reader, KEY_FIELD)?,
                        u64_column(reader, SEGMENT_FIELD)?,
                    ))
                })
                .collect(),
            wanted,
            hits: Vec::new(),
            seen: HashSet::new(),
        };
        let walked = plan
            .stages
            .iter()
            .try_for_each(|stage| match stage.query() {
                Some(query) => walk.stage(stage.rank, query.as_ref()),
                None => Ok(()),
            })
            .and_then(|()| match plan.body {
                Some(body) => walk.body(body, plan.body_order),
                None => Ok(()),
            });
        if let Err(error) = walked {
            tracing::warn!(%error, "search failed");
            return Hits::default();
        }
        let more = walk.hits.len() > page.offset.saturating_add(page.limit);
        let hits = walk
            .hits
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .collect();
        Hits { hits, more }
    }

    /// Bytes the committed index occupies.
    #[must_use]
    pub fn index_bytes(&self) -> u64 {
        self.reader.as_ref().map_or(0, |reader| {
            reader
                .searcher()
                .space_usage()
                .map_or(0, |usage| usage.total().get_bytes())
        })
    }
}

/// Collects hits stage by stage until a page is full.
struct Walk<'a> {
    searcher: &'a Searcher,
    /// Each segment's key and body-segment columns.
    columns: Vec<Option<(Column, Column)>>,
    wanted: usize,
    hits: Vec<Hit>,
    seen: HashSet<DocAddress>,
}

type Column = Arc<dyn ColumnValues<u64>>;

fn u64_column(reader: &SegmentReader, name: &str) -> Option<Column> {
    reader
        .fast_fields()
        .u64(name)
        .ok()
        .map(|column| column.first_or_default_col(0))
}

impl Walk<'_> {
    fn remaining(&self) -> usize {
        self.wanted.saturating_sub(self.hits.len())
    }

    fn push(&mut self, address: DocAddress, key: u64, segment: u64, rank: Option<i32>) {
        self.seen.insert(address);
        self.hits.push(Hit {
            key,
            segment: (segment != RECORD_SEGMENT)
                .then(|| usize::try_from(segment).unwrap_or(usize::MAX)),
            rank,
        });
    }

    /// Records of one rank, in key order, until the page is full. Each
    /// segment is read only as far as the page needs.
    fn stage(&mut self, rank: i32, query: &dyn Query) -> tantivy::Result<()> {
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(());
        }
        let weight = query.weight(EnableScoring::disabled_from_searcher(self.searcher))?;
        let mut found: Vec<(u64, u64, DocAddress)> = Vec::new();
        for (ordinal, (reader, columns)) in self
            .searcher
            .segment_readers()
            .iter()
            .zip(&self.columns)
            .enumerate()
        {
            let Ok(ordinal) = u32::try_from(ordinal) else {
                break;
            };
            let Some((keys, segments)) = columns else {
                continue;
            };
            let mut scorer = weight.scorer(reader, 1.0)?;
            let mut taken = 0usize;
            let mut doc = scorer.doc();
            while doc != TERMINATED && taken < remaining {
                let address = DocAddress::new(ordinal, doc);
                if !reader.is_deleted(doc) && !self.seen.contains(&address) {
                    found.push((keys.get_val(doc), segments.get_val(doc), address));
                    taken = taken.saturating_add(1);
                }
                doc = scorer.advance();
            }
        }
        found.sort_unstable_by_key(|(key, segment, _)| (*key, *segment));
        for (key, segment, address) in found.into_iter().take(remaining) {
            self.push(address, key, segment, Some(rank));
        }
        Ok(())
    }

    /// Body matches not already found, by BM25.
    fn body(&mut self, body: BoxQuery, order: Option<BoxQuery>) -> tantivy::Result<()> {
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(());
        }
        let found: BoxQuery = Box::new(ConstScoreQuery::new(body, 0.0));
        let query: BoxQuery = match order {
            Some(order) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, found),
                (Occur::Should, order),
            ])),
            None => found,
        };
        // Records already found as field hits can take at most that many
        // places of the top list.
        let collector = TopDocs::with_limit(remaining.saturating_add(self.seen.len())).tweak_score(
            |reader: &SegmentReader| {
                let keys = u64_column(reader, KEY_FIELD);
                let segments = u64_column(reader, SEGMENT_FIELD);
                move |doc: DocId, score: Score| {
                    let key = keys.as_ref().map_or(0, |keys| keys.get_val(doc));
                    let segment = segments
                        .as_ref()
                        .map_or(RECORD_SEGMENT, |segments| segments.get_val(doc));
                    (score, Reverse(key), Reverse(segment))
                }
            },
        );
        let top = self.searcher.search(query.as_ref(), &collector)?;
        for ((_, Reverse(key), Reverse(segment)), address) in top {
            if self.remaining() == 0 {
                break;
            }
            if !self.seen.contains(&address) {
                self.push(address, key, segment, None);
            }
        }
        Ok(())
    }
}
