//! The [`SearchProvider`] trait, the typed [`SearchResult`] it returns, and the
//! shared [`fuzzy_score`] ranking primitive the registry uses to merge results
//! from many providers on an even footing.

use crate::icons::Icon;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::sync::{Arc, RwLock};

/// Maximum number of rows a provider should return from the legacy, unbudgeted
/// [`SearchProvider::search`] entry point. Interactive callers use
/// `search_limited`; this ceiling keeps tests and secondary consumers from
/// accidentally materialising an entire library.
pub const DEFAULT_SEARCH_RESULT_LIMIT: usize = 256;

/// Legacy snapshot-size constant.
///
/// This is retained for source compatibility only. A durable namespace must
/// never use it to truncate discovery. Prefer [`SearchCatalog`], which keeps a
/// complete incrementally-maintained index and bounds work per query page.
pub const DEFAULT_SEARCH_INDEX_LIMIT: usize = 4_096;
/// Default maximum number of full records an indexed provider should verify
/// for one interactive query. The index may contain the complete corpus; this
/// is a query-time latency budget, not a data-loss limit.
pub const DEFAULT_SEARCH_CANDIDATE_LIMIT: usize = 256;
/// Maximum Unicode scalar values accepted from one interactive query.
///
/// This is an input-work ceiling, independent of the candidate and result
/// budgets below.  A pasted multi-megabyte string must not allocate a matching
/// normalised copy or generate one posting lookup per scalar on the UI thread.
/// The limit is deliberately far above a meaningful palette query; callers
/// receive an empty page for longer input and can refine it normally.
pub const MAX_SEARCH_QUERY_SCALARS: usize = 1_024;
const SEARCH_GRAM_WIDTH: usize = 4;
use std::borrow::Borrow;
use tilo_actions::ActionAddress;

/// A compact, reusable exact substring index for app search providers.
///
/// The complete supplied corpus is retained. Normalisation and a sorted
/// suffix-reference array are built once without copying suffix text.
/// Providers keep
/// ownership of their domain records and use the returned stable snapshot
/// offsets to materialise typed [`SearchResult`]s.
///
/// Posting lists narrow the candidates; complete-text verification rejects false
/// gram combinations. Each page bounds verification work and supplies a cursor
/// when candidates remain. Embedded NULs and all Unicode scalar values retain
/// their ordinary string semantics.
#[derive(Clone, Debug, Default)]
pub struct SearchTextIndex {
    texts: Vec<String>,
    suffixes: Vec<SuffixRef>,
    suffix_records: WaveletIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SuffixRef {
    record_id: usize,
    byte_offset: usize,
}

/// Succinct range-successor index over the record ids in suffix-array order.
///
/// One bit-vector per record-id bit uses `O(suffixes * log(records))` bits,
/// which is linear in the machine-word representation of the suffix refs. It
/// lets a query enumerate distinct matching record ids in snapshot order
/// without walking every suffix occurrence or any false n-gram candidate.
#[derive(Clone, Debug, Default)]
struct WaveletIndex {
    levels: Vec<WaveletLevel>,
    value_count: usize,
}

#[derive(Clone, Debug, Default)]
struct WaveletLevel {
    bits: Vec<u64>,
    prefix_ones: Vec<usize>,
    zeros: usize,
}

impl WaveletLevel {
    fn rank_ones(&self, end: usize) -> usize {
        let end = end.min(self.prefix_ones.len().saturating_sub(1).saturating_mul(64));
        let word = end / 64;
        let remainder = end % 64;
        let complete = self.prefix_ones.get(word).copied().unwrap_or_default();
        let partial = self.bits.get(word).map_or(0, |bits| {
            if remainder == 0 {
                0
            } else {
                let shift = u32::try_from(remainder).unwrap_or_default();
                let mask = 1u64
                    .checked_shl(shift)
                    .unwrap_or_default()
                    .saturating_sub(1);
                (bits & mask).count_ones() as usize
            }
        });
        complete.saturating_add(partial)
    }
}

impl WaveletIndex {
    fn new(values: &[usize], value_count: usize) -> Self {
        if values.is_empty() || value_count == 0 {
            return Self::default();
        }
        let bit_count = usize::BITS.saturating_sub(value_count.saturating_sub(1).leading_zeros());
        let mut ordered = values.to_vec();
        let mut levels = Vec::with_capacity(bit_count as usize);
        for bit in (0..bit_count).rev() {
            let mut bits = vec![0u64; ordered.len().div_ceil(64)];
            let mut zeros = Vec::with_capacity(ordered.len());
            let mut ones = Vec::with_capacity(ordered.len());
            for (position, value) in ordered.iter().copied().enumerate() {
                if value & (1usize << bit) == 0 {
                    zeros.push(value);
                } else {
                    if let Some(word) = bits.get_mut(position / 64) {
                        *word |= 1u64 << (position % 64);
                    }
                    ones.push(value);
                }
            }
            let zero_count = zeros.len();
            zeros.extend(ones);
            ordered = zeros;
            let mut prefix_ones = Vec::with_capacity(bits.len().saturating_add(1));
            prefix_ones.push(0usize);
            for word in &bits {
                let count = prefix_ones
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(word.count_ones() as usize);
                prefix_ones.push(count);
            }
            levels.push(WaveletLevel {
                bits,
                prefix_ones,
                zeros: zero_count,
            });
        }
        Self {
            levels,
            value_count,
        }
    }

    fn range_successor(&self, range: Range<usize>, lower: usize) -> Option<usize> {
        if range.start >= range.end || lower >= self.value_count {
            return None;
        }
        let before = self.range_count_less_than(range.clone(), lower);
        (before < range.len()).then(|| self.range_kth(range, before))
    }

    fn range_count_less_than(&self, mut range: Range<usize>, value: usize) -> usize {
        if value >= self.value_count {
            return range.len();
        }
        let mut count = 0usize;
        let bit_count = self.levels.len();
        for (level_index, level) in self.levels.iter().enumerate() {
            let ones_before = level.rank_ones(range.start);
            let ones_end = level.rank_ones(range.end);
            let zeros_before = range.start.saturating_sub(ones_before);
            let zeros_end = range.end.saturating_sub(ones_end);
            let bit = bit_count.saturating_sub(level_index).saturating_sub(1);
            if value & (1usize << bit) == 0 {
                range = zeros_before..zeros_end;
            } else {
                count = count.saturating_add(zeros_end.saturating_sub(zeros_before));
                range =
                    level.zeros.saturating_add(ones_before)..level.zeros.saturating_add(ones_end);
            }
        }
        count
    }

    fn range_kth(&self, mut range: Range<usize>, mut rank: usize) -> usize {
        let mut value = 0usize;
        let bit_count = self.levels.len();
        for (level_index, level) in self.levels.iter().enumerate() {
            let ones_before = level.rank_ones(range.start);
            let ones_end = level.rank_ones(range.end);
            let zeros_before = range.start.saturating_sub(ones_before);
            let zeros_end = range.end.saturating_sub(ones_end);
            let zeros_in_range = zeros_end.saturating_sub(zeros_before);
            if rank < zeros_in_range {
                range = zeros_before..zeros_end;
            } else {
                rank = rank.saturating_sub(zeros_in_range);
                let bit = bit_count.saturating_sub(level_index).saturating_sub(1);
                value |= 1usize << bit;
                range =
                    level.zeros.saturating_add(ones_before)..level.zeros.saturating_add(ones_end);
            }
        }
        value
    }
}

/// A bounded page of snapshot offsets from [`SearchTextIndex`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchTextPage {
    pub ids: Vec<usize>,
    /// Resume strictly after this snapshot offset. `None` means exhausted.
    pub next_after: Option<usize>,
    /// Number of distinct exact-match records materialised for this page.
    /// Suffix-range comparisons are bounded by query length and do not scan
    /// complete false-candidate records.
    pub inspected: usize,
}

impl SearchTextIndex {
    /// Build an index over every supplied searchable record.
    pub fn new<S>(texts: impl IntoIterator<Item = S>) -> Self
    where
        S: AsRef<str>,
    {
        let texts: Vec<String> = texts
            .into_iter()
            .map(|text| normalize_search_text(text.as_ref()))
            .collect();
        let mut suffixes = Vec::new();
        for (record_id, text) in texts.iter().enumerate() {
            for (byte_offset, _) in text.char_indices() {
                suffixes.push(SuffixRef {
                    record_id,
                    byte_offset,
                });
            }
        }
        suffixes.sort_unstable_by(|left, right| {
            suffix_text(&texts, *left)
                .cmp(&suffix_text(&texts, *right))
                .then_with(|| left.record_id.cmp(&right.record_id))
                .then_with(|| left.byte_offset.cmp(&right.byte_offset))
        });
        let suffix_records = WaveletIndex::new(
            &suffixes
                .iter()
                .map(|suffix| suffix.record_id)
                .collect::<Vec<_>>(),
            texts.len(),
        );

        Self {
            texts,
            suffixes,
            suffix_records,
        }
    }

    /// Number of complete records represented by this snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.texts.len()
    }

    /// Whether the snapshot contains no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// Return matching snapshot offsets while inspecting at most `budget`
    /// complete texts. For broad queries the earliest matching records in the
    /// provider's deliberate snapshot order form the bounded result window.
    #[must_use]
    pub fn matching_ids(&self, query: &str, budget: usize) -> Vec<usize> {
        self.matching_page(query, None, budget, budget).ids
    }

    /// Return a deterministic bounded page of matching snapshot offsets.
    ///
    /// The cursor is a record offset rather than a byte position, so Unicode
    /// and embedded NULs cannot split or corrupt it. A binary-searched suffix
    /// range excludes false candidates; the range-successor index then pages
    /// distinct matches in snapshot order without rescanning earlier records.
    #[must_use]
    pub fn matching_page(
        &self,
        query: &str,
        after: Option<usize>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchTextPage {
        // Treat budgets as caller-requested ceilings, not authority to make a
        // single UI-thread page arbitrarily large. Untrusted control-socket
        // and collaboration callers can continue with `next_after`; they may
        // never turn one request into a full-corpus materialisation.
        let work_budget = work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT);
        let result_limit = result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if work_budget == 0 || result_limit == 0 {
            return SearchTextPage::default();
        }
        let Some(query) = normalize_bounded_search_query(query) else {
            return SearchTextPage::default();
        };
        if query.is_empty() {
            return self.empty_query_page(after, work_budget, result_limit);
        }
        let suffix_range = self.exact_suffix_range(&query);
        let mut ids = Vec::with_capacity(work_budget.min(result_limit));
        let mut last = after;
        while ids.len() < work_budget && ids.len() < result_limit {
            let lower = last.map_or(0, |record_id| record_id.saturating_add(1));
            let Some(record_id) = self
                .suffix_records
                .range_successor(suffix_range.clone(), lower)
            else {
                break;
            };
            ids.push(record_id);
            last = Some(record_id);
        }
        let has_more = last.is_some_and(|record_id| {
            self.suffix_records
                .range_successor(suffix_range, record_id.saturating_add(1))
                .is_some()
        });
        let inspected = ids.len();
        SearchTextPage {
            ids,
            next_after: has_more.then_some(last).flatten(),
            inspected,
        }
    }

    fn empty_query_page(
        &self,
        after: Option<usize>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchTextPage {
        let start = after.map_or(0, |offset| offset.saturating_add(1));
        let take = work_budget.min(result_limit);
        let ids: Vec<usize> = (start..self.texts.len()).take(take).collect();
        let inspected = ids.len();
        let next_after = ids
            .last()
            .copied()
            .filter(|last| last.saturating_add(1) < self.texts.len());
        SearchTextPage {
            ids,
            next_after,
            inspected,
        }
    }

    fn exact_suffix_range(&self, query: &str) -> Range<usize> {
        let lower = self.suffixes.partition_point(|suffix| {
            suffix_text(&self.texts, *suffix).is_some_and(|text| text < query)
        });
        let upper = self.suffixes.partition_point(|suffix| {
            suffix_text(&self.texts, *suffix)
                .is_some_and(|text| text <= query || text.starts_with(query))
        });
        lower..upper
    }

    /// The normalised text stored for a snapshot offset, for provider-specific
    /// ranking without repeating normalisation on every keystroke.
    #[must_use]
    pub fn normalized_text(&self, record_id: usize) -> Option<&str> {
        self.texts.get(record_id).map(String::as_str)
    }
}

fn suffix_text(texts: &[String], suffix: SuffixRef) -> Option<&str> {
    texts
        .get(suffix.record_id)
        .and_then(|text| text.get(suffix.byte_offset..))
}

fn search_grams(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let width = chars.len().min(SEARCH_GRAM_WIDTH);
    let mut grams = BTreeSet::new();
    for window in chars.windows(width) {
        grams.insert(window.iter().collect());
    }
    grams.into_iter().collect()
}

fn index_grams(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut grams = BTreeSet::new();
    for width in 1..=chars.len().min(SEARCH_GRAM_WIDTH) {
        for window in chars.windows(width) {
            grams.insert(window.iter().collect());
        }
    }
    grams.into_iter().collect()
}

fn rarest_posting<'a, K: Ord>(
    postings: &'a HashMap<String, BTreeSet<K>>,
    grams: &[String],
) -> Option<&'a BTreeSet<K>> {
    let mut rarest: Option<&BTreeSet<K>> = None;
    for gram in grams {
        let posting = postings.get(gram)?;
        if rarest.is_none_or(|current| posting.len() < current.len()) {
            rarest = Some(posting);
        }
    }
    rarest
}

/// Apply the shared case-folding convention used by [`SearchTextIndex`].
#[must_use]
pub fn normalize_search_text(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Normalise an interactive query while bounding both input traversal and the
/// normalised allocation.  Index construction intentionally continues to use
/// [`normalize_search_text`]: searchable resource content is retained in full
/// so a unique term late in a large document remains discoverable.  Only
/// untrusted, per-keystroke query input is constrained here.
fn normalize_bounded_search_query(text: &str) -> Option<String> {
    let mut normalized = String::with_capacity(text.len().min(MAX_SEARCH_QUERY_SCALARS));
    let mut scalars = 0usize;
    for scalar in text.chars() {
        scalars = scalars.checked_add(1)?;
        if scalars > MAX_SEARCH_QUERY_SCALARS {
            return None;
        }
        normalized.extend(scalar.to_lowercase());
    }
    Some(normalized)
}

/// Opaque continuation for a bounded [`SearchCatalog`] query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchCursor {
    after_resource_id: Option<String>,
    after_browse_ordinal: Option<u64>,
    generation: u64,
    normalized_query: String,
}

impl SearchCursor {
    /// Construct an opaque cursor for a provider which keeps its own compact
    /// index rather than a [`SearchCatalog`].  The payload is intentionally
    /// only meaningful to that provider; hosts must pass it back unchanged.
    #[must_use]
    pub fn opaque(
        generation: u64,
        normalized_query: impl Into<String>,
        after_resource_id: Option<String>,
        after_browse_ordinal: Option<u64>,
    ) -> Self {
        Self {
            after_resource_id,
            after_browse_ordinal,
            generation,
            normalized_query: normalized_query.into(),
        }
    }

    /// Return the opaque fields for the provider that created this cursor.
    #[must_use]
    pub fn opaque_parts(&self) -> (u64, &str, Option<&str>, Option<u64>) {
        (
            self.generation,
            &self.normalized_query,
            self.after_resource_id.as_deref(),
            self.after_browse_ordinal,
        )
    }
}

/// One bounded page returned by [`SearchCatalog::query_page`].
#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    pub results: Vec<SearchResult>,
    /// Pass this back to `query_page` to continue. `None` means exhausted.
    pub next: Option<SearchCursor>,
    /// Number of candidate records whose complete text was inspected.
    pub inspected: usize,
}

/// Cheap diagnostics used by contract and performance tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCatalogStats {
    pub records: usize,
    pub normalized_scalars: usize,
    pub postings: usize,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchCatalogError {
    EmptyResourceId,
    NonCanonicalResult,
    ResourceAddressMismatch,
    /// A retained catalogue has consumed every generation value. Mutating it
    /// again would make an old continuation cursor appear current.
    GenerationExhausted,
    /// A retained catalogue has assigned every browse ordinal. Reusing one
    /// would overwrite another resource in empty-query browsing.
    BrowseOrdinalExhausted,
    /// A bulk snapshot must contain one row per canonical resource.
    DuplicateResourceId,
    /// An incremental mutation expected a retained resource that is absent.
    MissingResource,
}

impl std::fmt::Display for SearchCatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyResourceId => formatter.write_str("search resource id is empty"),
            Self::NonCanonicalResult => {
                formatter.write_str("durable search results require a canonical action address")
            }
            Self::ResourceAddressMismatch => formatter.write_str(
                "search result action must address the same stable resource id as the index row",
            ),
            Self::GenerationExhausted => formatter.write_str(
                "search catalogue generation exhausted; rebuild the catalogue before mutating it",
            ),
            Self::BrowseOrdinalExhausted => formatter.write_str(
                "search catalogue browse identity exhausted; rebuild the catalogue before inserting",
            ),
            Self::DuplicateResourceId => formatter.write_str(
                "bulk search catalogue contains the same canonical resource more than once",
            ),
            Self::MissingResource => {
                formatter.write_str("incremental search mutation targets a missing resource")
            }
        }
    }
}

impl std::error::Error for SearchCatalogError {}

#[derive(Clone, Debug)]
struct CatalogEntry {
    normalized: String,
    grams: Vec<String>,
    tokens: Vec<String>,
    result: SearchResult,
    browse_ordinal: u64,
}

#[derive(Debug, Default)]
struct SearchCatalogInner {
    entries: BTreeMap<Arc<str>, CatalogEntry>,
    browse_order: BTreeMap<u64, Arc<str>>,
    postings: HashMap<String, BTreeSet<Arc<str>>>,
    /// Complete whitespace-delimited terms complement the compact n-gram
    /// index.  An exact long identifier (mail id, filename, video id) can
    /// then find a late record without walking every shared short trigram.
    tokens: HashMap<String, BTreeSet<Arc<str>>>,
    normalized_scalars: usize,
    posting_count: usize,
    generation: u64,
    next_browse_ordinal: u64,
}

/// A complete, incremental search catalogue keyed by canonical resource id.
///
/// Cloning a catalogue is O(1), so a widget can retain one instance and return
/// a cheap provider handle whenever Ctrl+K opens. Mutations update only the
/// changed record's posting lists. Queries are explicitly paged by both result
/// count and candidate-inspection budget; result objects are cloned only for
/// exact matches in the requested page.
///
/// The index uses Unicode 1/2/3/4-gram and complete-term postings plus exact verification. A unique
/// item remains directly findable even in a corpus larger than 100,000 rows;
/// broad or adversarial queries can continue through `SearchCursor` pages
/// without blocking one UI turn on the whole corpus.
#[derive(Clone, Debug)]
pub struct SearchCatalog {
    name: Arc<str>,
    inner: Arc<RwLock<SearchCatalogInner>>,
}

impl SearchCatalog {
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            inner: Arc::new(RwLock::new(SearchCatalogInner::default())),
        }
    }

    /// Replace an immutable snapshot in one generation. This is the bulk
    /// counterpart to [`Self::upsert`]: callers rebuilding a complete
    /// provider avoid repeatedly rebalancing the same high-fanout posting
    /// trees, while later incremental updates retain their existing semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, duplicate, or non-canonical rows, or
    /// when catalogue generation/browse identities are exhausted.
    pub fn replace_all(
        &self,
        rows: impl IntoIterator<Item = (String, String, SearchResult)>,
    ) -> Result<(), SearchCatalogError> {
        let mut next = SearchCatalogInner::default();
        let mut postings = HashMap::<String, Vec<Arc<str>>>::new();
        let mut tokens = HashMap::<String, Vec<Arc<str>>>::new();
        for (resource_id, searchable_text, result) in rows {
            if resource_id.is_empty() {
                return Err(SearchCatalogError::EmptyResourceId);
            }
            if result.action_address().is_none() {
                return Err(SearchCatalogError::NonCanonicalResult);
            }
            if result
                .action_address()
                .is_some_and(|address| address.resource != resource_id)
            {
                return Err(SearchCatalogError::ResourceAddressMismatch);
            }
            let resource_id: Arc<str> = resource_id.into();
            let normalized = normalize_search_text(&searchable_text);
            let grams = index_grams(&normalized);
            let row_tokens = index_tokens(&normalized);
            let ordinal = next.next_browse_ordinal;
            next.next_browse_ordinal = next
                .next_browse_ordinal
                .checked_add(1)
                .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
            next.normalized_scalars = next
                .normalized_scalars
                .saturating_add(normalized.chars().count());
            next.browse_order.insert(ordinal, Arc::clone(&resource_id));
            for gram in &grams {
                postings
                    .entry(gram.clone())
                    .or_default()
                    .push(Arc::clone(&resource_id));
            }
            for token in &row_tokens {
                tokens
                    .entry(token.clone())
                    .or_default()
                    .push(Arc::clone(&resource_id));
            }
            if next.entries.contains_key(resource_id.as_ref()) {
                return Err(SearchCatalogError::DuplicateResourceId);
            }
            next.entries.insert(
                resource_id,
                CatalogEntry {
                    normalized,
                    grams,
                    tokens: row_tokens,
                    result,
                    browse_ordinal: ordinal,
                },
            );
        }
        next.postings = postings
            .into_iter()
            .map(|(key, mut ids)| {
                ids.sort();
                ids.dedup();
                next.posting_count = next.posting_count.saturating_add(ids.len());
                (key, ids.into_iter().collect())
            })
            .collect();
        next.tokens = tokens
            .into_iter()
            .map(|(key, mut ids)| {
                ids.sort();
                ids.dedup();
                next.posting_count = next.posting_count.saturating_add(ids.len());
                (key, ids.into_iter().collect())
            })
            .collect();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        next.generation = inner
            .generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        *inner = next;
        drop(inner);
        Ok(())
    }

    /// Set this freshly-built catalogue's generation immediately after a
    /// published predecessor. This supports providers that construct a full
    /// replacement off their public snapshot lock, then make one short atomic
    /// swap without allowing a cursor from the predecessor to remain current.
    ///
    /// # Errors
    ///
    /// Returns [`SearchCatalogError::GenerationExhausted`] when advancing the
    /// predecessor generation would overflow.
    pub fn advance_after(&self, predecessor_generation: u64) -> Result<(), SearchCatalogError> {
        let generation = predecessor_generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.generation = generation;
        drop(inner);
        Ok(())
    }

    /// Preflight a bounded batch of incremental mutations without changing the
    /// retained catalogue. Providers can use this before changing companion
    /// lookup metadata, ensuring exhausted generation or browse identities
    /// fail closed rather than publishing a partial delta.
    ///
    /// # Errors
    ///
    /// Returns an exhaustion error when the requested mutations cannot obtain
    /// distinct generation or browse identities.
    pub fn preflight_mutations(
        &self,
        mutations: usize,
        inserts: usize,
    ) -> Result<(), SearchCatalogError> {
        let mutations =
            u64::try_from(mutations).map_err(|_| SearchCatalogError::GenerationExhausted)?;
        let inserts =
            u64::try_from(inserts).map_err(|_| SearchCatalogError::BrowseOrdinalExhausted)?;
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .generation
            .checked_add(mutations)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        inner
            .next_browse_ordinal
            .checked_add(inserts)
            .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
        drop(inner);
        Ok(())
    }

    /// Whether a canonical resource is currently retained by this catalogue.
    #[must_use]
    pub fn contains(&self, resource_id: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .contains_key(resource_id)
    }

    /// Insert or replace one durable resource. `searchable_text` may combine
    /// title, metadata and body; `result` is materialised only after an exact
    /// query match.
    ///
    /// # Errors
    ///
    /// Returns [`SearchCatalogError`] when the stable id is empty, the result
    /// is legacy/opaque, or its canonical action addresses another resource.
    pub fn upsert(
        &self,
        resource_id: impl Into<String>,
        searchable_text: impl AsRef<str>,
        result: SearchResult,
    ) -> Result<(), SearchCatalogError> {
        let resource_id = resource_id.into();
        if resource_id.is_empty() {
            return Err(SearchCatalogError::EmptyResourceId);
        }
        let Some(address) = result.action_address() else {
            return Err(SearchCatalogError::NonCanonicalResult);
        };
        if address.resource != resource_id {
            return Err(SearchCatalogError::ResourceAddressMismatch);
        }
        let resource_id: Arc<str> = resource_id.into();
        let normalized = normalize_search_text(searchable_text.as_ref());
        let grams = index_grams(&normalized);
        let tokens = index_tokens(&normalized);
        let scalar_count = normalized.chars().count();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner
            .entries
            .get(resource_id.as_ref())
            .is_some_and(|entry| entry.normalized == normalized && entry.result == result)
        {
            return Ok(());
        }
        let next_generation = inner
            .generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        let browse_ordinal = if let Some(previous) = inner.entries.remove(resource_id.as_ref()) {
            let ordinal = previous.browse_ordinal;
            remove_postings(&mut inner, resource_id.as_ref(), &previous);
            ordinal
        } else {
            let ordinal = inner.next_browse_ordinal;
            inner.next_browse_ordinal = inner
                .next_browse_ordinal
                .checked_add(1)
                .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
            inner.browse_order.insert(ordinal, Arc::clone(&resource_id));
            ordinal
        };
        for gram in &grams {
            if inner
                .postings
                .entry(gram.clone())
                .or_default()
                .insert(Arc::clone(&resource_id))
            {
                inner.posting_count = inner.posting_count.saturating_add(1);
            }
        }
        for token in &tokens {
            if inner
                .tokens
                .entry(token.clone())
                .or_default()
                .insert(Arc::clone(&resource_id))
            {
                inner.posting_count = inner.posting_count.saturating_add(1);
            }
        }
        inner.normalized_scalars = inner.normalized_scalars.saturating_add(scalar_count);
        inner.entries.insert(
            resource_id,
            CatalogEntry {
                normalized,
                grams,
                tokens,
                result,
                browse_ordinal,
            },
        );
        inner.generation = next_generation;
        drop(inner);
        Ok(())
    }

    /// Remove one resource and all of its postings. Returns whether it existed.
    pub fn remove(&self, resource_id: &str) -> bool {
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Returning `false` is the existing fail-closed contract for a remove
        // that was not performed. Do not mutate at MAX: keeping the row is
        // safer than making a previously-issued cursor current again.
        if inner.generation == u64::MAX {
            return false;
        }
        let Some(previous) = inner.entries.remove(resource_id) else {
            return false;
        };
        inner.browse_order.remove(&previous.browse_ordinal);
        remove_postings(&mut inner, resource_id, &previous);
        inner.generation = inner.generation.saturating_add(1);
        drop(inner);
        true
    }

    #[must_use]
    pub fn stats(&self) -> SearchCatalogStats {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stats = SearchCatalogStats {
            records: inner.entries.len(),
            normalized_scalars: inner.normalized_scalars,
            postings: inner.posting_count,
            generation: inner.generation,
        };
        drop(inner);
        stats
    }

    /// Whether a prior cursor belongs to this exact retained-index
    /// generation. The pull directory must turn a mismatch into a stale
    /// cursor response, never reinterpret it as an exhausted result set.
    #[must_use]
    pub fn cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.stats().generation == cursor.generation
    }

    /// Query one deterministic page. Both budgets are hard ceilings. Continue
    /// with the returned cursor from an idle task/worker when `next` is set.
    #[must_use]
    pub fn query_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        // Providers and remote callers may request a smaller page, but no
        // caller can enlarge one retained-index turn beyond the interactive
        // latency/materialisation ceilings. Complete discovery remains
        // available through the returned cursor.
        let work_budget = work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT);
        let result_limit = result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if work_budget == 0 || result_limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let Some(query) = normalize_bounded_search_query(query) else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        let grams = search_grams(&query);
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = inner.generation;
        let candidates = if query.is_empty() {
            CandidateRows::Browse(&inner.browse_order)
        } else if let Some(posting) = inner.tokens.get(&query) {
            CandidateRows::Posting(posting)
        } else if let Some(posting) = rarest_posting(&inner.postings, &grams) {
            CandidateRows::Posting(posting)
        } else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        // A catalogue edit between pages invalidates ordering/candidate-set
        // assumptions. Restarting may repeat rows but can never skip a newly
        // inserted or edited resource.
        let after = cursor
            .filter(|cursor| cursor.generation == generation && cursor.normalized_query == query)
            .and_then(|cursor| cursor.after_resource_id.as_deref());
        let after_browse_ordinal = cursor
            .filter(|cursor| cursor.generation == generation && cursor.normalized_query == query)
            .and_then(|cursor| cursor.after_browse_ordinal);
        let mut results = Vec::new();
        let mut inspected = 0usize;
        let mut last_id = None;
        let mut last_browse_ordinal = None;
        let mut stopped = false;
        for (browse_ordinal, resource_id) in candidates.ids_after(after, after_browse_ordinal) {
            if inspected == work_budget || results.len() == result_limit {
                stopped = true;
                break;
            }
            inspected = inspected.saturating_add(1);
            last_id = Some(resource_id.to_string());
            last_browse_ordinal = browse_ordinal;
            let Some(entry) = inner.entries.get(resource_id) else {
                continue;
            };
            let exact = query.is_empty()
                || (grams.iter().all(|gram| {
                    inner
                        .postings
                        .get(gram)
                        .is_some_and(|posting| posting.contains(resource_id))
                }) && entry.normalized.contains(&query));
            if exact {
                results.push(entry.result.clone());
            }
        }
        let next = stopped.then_some(SearchCursor {
            after_resource_id: last_id,
            after_browse_ordinal: last_browse_ordinal,
            generation,
            normalized_query: query,
        });
        drop(inner);
        SearchPage {
            results,
            next,
            inspected,
        }
    }
}

enum CandidateRows<'a> {
    Browse(&'a BTreeMap<u64, Arc<str>>),
    Posting(&'a BTreeSet<Arc<str>>),
}

impl CandidateRows<'_> {
    fn ids_after<'a>(
        &'a self,
        after: Option<&'a str>,
        after_browse_ordinal: Option<u64>,
    ) -> Box<dyn Iterator<Item = (Option<u64>, &'a Arc<str>)> + 'a> {
        use std::ops::Bound::{Excluded, Unbounded};
        match self {
            Self::Browse(entries) => match after_browse_ordinal {
                Some(after) => Box::new(
                    entries
                        .range((Excluded(after), Unbounded))
                        .map(|(ordinal, id)| (Some(*ordinal), id)),
                ),
                None => Box::new(entries.iter().map(|(ordinal, id)| (Some(*ordinal), id))),
            },
            Self::Posting(posting) => match after {
                Some(after) => Box::new(
                    posting
                        .range::<str, _>((Excluded(after), Unbounded))
                        .map(|id| (None, id)),
                ),
                None => Box::new(posting.iter().map(|id| (None, id))),
            },
        }
    }
}

fn remove_postings(inner: &mut SearchCatalogInner, resource_id: &str, entry: &CatalogEntry) {
    inner.normalized_scalars = inner
        .normalized_scalars
        .saturating_sub(entry.normalized.chars().count());
    for gram in &entry.grams {
        let mut empty = false;
        if let Some(posting) = inner.postings.get_mut(gram) {
            if posting.remove(resource_id) {
                inner.posting_count = inner.posting_count.saturating_sub(1);
            }
            empty = posting.is_empty();
        }
        if empty {
            inner.postings.remove(gram);
        }
    }
    for token in &entry.tokens {
        let mut empty = false;
        if let Some(posting) = inner.tokens.get_mut(token) {
            if posting.remove(resource_id) {
                inner.posting_count = inner.posting_count.saturating_sub(1);
            }
            empty = posting.is_empty();
        }
        if empty {
            inner.tokens.remove(token);
        }
    }
}

fn index_tokens(normalized: &str) -> Vec<String> {
    normalized
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Marks an `action_id` as a framework-owned canonical activation. The tag
/// keeps arbitrary legacy ids from becoming actions merely because they happen
/// to contain tilo-actions' wire separator.
const ACTION_ADDRESS_PREFIX: &str = "\u{1}action\u{1f}";

/// One typed hit produced by a [`SearchProvider`].
///
/// App-agnostic on purpose: an app fills the display fields (`title`,
/// `subtitle`, `kind`, `icon`) and an activation target. New providers should
/// use [`SearchResult::for_action`] or [`SearchResult::with_action_address`] to
/// name an exported resource action. The palette resolves that address against
/// the current resource tree and sends it through the same operation broker as
/// every other affordance.
///
/// `action_id` remains public and opaque for compatibility with providers that
/// predate the resource/action model. Those legacy values are still handed to
/// the owning widget's navigation callback when they are activated.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    /// Primary line — what the row reads as (e.g. a contact's display name).
    pub title: String,
    /// Optional secondary line rendered dimmer under the title (e.g. an email
    /// address, a file path, a command description).
    pub subtitle: Option<String>,
    /// Plain-text detail rendered by the shared result surface for the
    /// selected or hovered row without changing row geometry.
    pub preview: Option<String>,
    /// Grouping label, e.g. `"Contacts"`, `"Files"`, `"Commands"`. Rows sharing
    /// a `kind` are bucketed together under this label in the results overlay,
    /// so a mixed query reads as sectioned groups rather than one flat list.
    pub kind: String,
    /// Activation token. Prefer the canonical encoded [`ActionAddress`] made
    /// by [`for_action`](Self::for_action) or
    /// [`with_action_address`](Self::with_action_address). Any value that is
    /// not tagged as canonical is treated as a legacy opaque navigation id.
    /// A tagged address that is not currently exported is omitted by the
    /// palette rather than gaining authority through the legacy callback.
    pub action_id: String,
    /// Optional leading glyph drawn at the start of the row.
    pub icon: Option<Icon>,
    /// Provider-supplied relevance bias — a nudge, not an override, since the
    /// registry adds a query-match bonus on top. `0` ranks by query match
    /// alone; raise it to float pinned/recent/favourite hits.
    pub score: i32,
}

impl SearchResult {
    /// A result with the three required fields; chain the builders for the rest.
    pub fn new(
        kind: impl Into<String>,
        title: impl Into<String>,
        action_id: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            preview: None,
            kind: kind.into(),
            action_id: action_id.into(),
            icon: None,
            score: 0,
        }
    }

    /// A result that activates a canonical exported resource action.
    ///
    /// This is the preferred constructor for new providers. The address is
    /// encoded into the existing `action_id` storage so adding canonical
    /// activation does not break providers built against the legacy shape.
    pub fn for_action(
        kind: impl Into<String>,
        title: impl Into<String>,
        address: impl Borrow<ActionAddress>,
    ) -> Self {
        Self::new(
            kind,
            title,
            format!("{ACTION_ADDRESS_PREFIX}{}", address.borrow().encode()),
        )
    }

    /// Replace this hit's legacy activation id with a canonical action
    /// address while retaining its display metadata.
    #[must_use]
    pub fn with_action_address(mut self, address: impl Borrow<ActionAddress>) -> Self {
        self.action_id = format!("{ACTION_ADDRESS_PREFIX}{}", address.borrow().encode());
        self
    }

    /// Decode this hit's canonical action address, if it carries one.
    pub fn action_address(&self) -> Option<ActionAddress> {
        self.action_id
            .strip_prefix(ACTION_ADDRESS_PREFIX)
            .and_then(ActionAddress::parse)
    }

    /// Whether this result opted into canonical action activation.
    pub(crate) fn has_action_address(&self) -> bool {
        self.action_id.starts_with(ACTION_ADDRESS_PREFIX)
    }

    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = Some(preview.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_score(mut self, score: i32) -> Self {
        self.score = score;
        self
    }
}

/// Automatic bounded search over the resources an app currently has open.
///
/// Apps still provide explicit [`SearchProvider`]s for large or closed
/// collections (all mail, every file, every contact). This provider closes the
/// baseline gap: a live resource published by any app is findable by its label,
/// kind, or stable id even when the app has no bespoke content index. One canonical
/// representative action activates the resource; the palette continues to
/// list every action separately from the same fresh authority tree.
#[derive(Clone)]
pub struct OpenResourceSearch {
    catalog: SearchCatalog,
    resources: Arc<BTreeMap<String, tilo_actions::Resource>>,
}

impl OpenResourceSearch {
    #[must_use]
    pub fn new(resources: &[tilo_actions::Resource]) -> Self {
        fn collect(
            resource: &tilo_actions::Resource,
            resources: &mut BTreeMap<String, tilo_actions::Resource>,
        ) {
            resources
                .entry(resource.id.as_str().to_string())
                .or_insert_with(|| resource.clone());
            for child in &resource.children {
                collect(child, resources);
            }
        }

        let mut indexed = BTreeMap::new();
        for resource in resources {
            collect(resource, &mut indexed);
        }
        let catalog = SearchCatalog::new("Open resources");
        if let Err(error) = catalog.replace_all(indexed.values().filter_map(|resource| {
            let action = resource
                .actions
                .iter()
                .find(|action| action.kind == tilo_actions::OperationKind::View)
                .or_else(|| resource.actions.first())?;
            let address = ActionAddress::new(resource.id.as_str(), &action.id);
            Some((
                resource.id.as_str().to_string(),
                format!(
                    "{} {} {}",
                    resource.label,
                    resource.kind,
                    resource.id.as_str()
                ),
                SearchResult::for_action("Resources", resource.label.clone(), address)
                    .with_subtitle(format!("{} · {}", action.label, resource.kind)),
            ))
        })) {
            tracing::error!(%error, "could not index open resources");
        }
        Self {
            catalog,
            resources: Arc::new(indexed),
        }
    }

    #[cfg(test)]
    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.resources, &other.resources)
    }
}

impl SearchProvider for OpenResourceSearch {
    fn name(&self) -> &'static str {
        "Open resources"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog
            .query_page(
                query,
                None,
                DEFAULT_SEARCH_CANDIDATE_LIMIT,
                limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
            )
            .results
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<tilo_actions::Resource> {
        self.resources
            .get(&address.resource)
            .filter(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
            .cloned()
    }
}

/// A source of search results — implemented by an app for each searchable
/// *type* it exposes (its contacts, its open files, its commands, …).
///
/// A provider **filters** to plausible matches; the
/// [`SearchRegistry`](crate::search::SearchRegistry) **ranks** across
/// providers via [`fuzzy_score`]. Return everything you consider a match (a
/// contact whose phone number matches even though the title is a name, say) —
/// the registry floats the strongest title matches without discarding
/// your field-aware hits.
pub trait SearchProvider {
    /// Toolkit provenance for a component-owned proxy. Applications cannot
    /// construct a CallbackBinding; its payload remains owned by AppComponent.
    #[doc(hidden)]
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        None
    }
    /// The kind label this provider produces, used for diagnostics and as the
    /// default group header. Individual results may still carry a different
    /// [`SearchResult::kind`].
    fn name(&self) -> &str;

    /// Monotonic retained-index generation for the broker pull directory.
    /// `None` means the provider has not proved that a continuation cursor is
    /// safe across mutation, so remote directory discovery must omit it
    /// rather than silently advertise mutable rows as revision-stable.
    /// Providers backed by [`SearchCatalog`] should return
    /// `Some(catalog.stats().generation)`.
    fn action_directory_generation(&self) -> Option<u64> {
        None
    }

    /// Validate a continuation token before it is used by a remote pull
    /// directory. The conservative default refuses it: only providers that
    /// expose a retained generation may claim continuation safety.
    fn action_directory_cursor_is_current(&self, _cursor: &SearchCursor) -> bool {
        false
    }

    /// Return the matching results for `query`. An empty query conventionally
    /// returns a provider's "browse" set (recents / favourites / all), or an
    /// empty vec if browsing makes no sense for the source.
    fn search(&self, query: &str) -> Vec<SearchResult>;

    /// Query with a materialisation budget. App providers must override this
    /// and stop constructing hits once `limit` is reached. The fallback exists
    /// only for tiny/test providers outside `apps/`; the repository contract
    /// checker rejects app implementations that inherit it.
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let mut hits = self.search(query);
        hits.truncate(limit);
        hits
    }

    /// Return one bounded continuation page for an interactive search.
    ///
    /// Providers backed by a durable namespace should override this method so
    /// callers can eventually reach every exact match without scanning the
    /// whole corpus on the UI thread. Tiny legacy providers remain a single
    /// page: a non-empty cursor therefore means they are exhausted.
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        if cursor.is_some() || work_budget == 0 || result_limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let limit = work_budget.min(result_limit);
        let results = self.search_limited(query, limit);
        SearchPage {
            inspected: results.len(),
            results,
            next: None,
        }
    }

    /// Lazily resolve a canonical search address that is intentionally absent
    /// from the small open-resource snapshot.
    fn resolve_action_resource(&self, _address: &ActionAddress) -> Option<tilo_actions::Resource> {
        None
    }
}

impl SearchProvider for SearchCatalog {
    fn name(&self) -> &str {
        &self.name
    }

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.query_page(
            query,
            None,
            DEFAULT_SEARCH_CANDIDATE_LIMIT,
            limit.min(DEFAULT_SEARCH_RESULT_LIMIT),
        )
        .results
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.query_page(query, cursor, work_budget, result_limit)
    }
}

/// Case-insensitive subsequence fuzzy score: `Some(score)` when `query` matches
/// `text` as a subsequence, `None` otherwise. Higher is a better match. An
/// empty query matches everything with score `0`.
///
/// The score rewards, in order of weight, matches that are **contiguous**, that
/// land on a **word boundary** (start of string, or right after a space /
/// separator), and that **start** the string; it penalises gaps between matched
/// characters. This is the single ranking primitive the registry uses so a hit
/// from one provider is comparable to a hit from another.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();

    let is_boundary = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        // `i > 0` here, so `i - 1` is always a valid index into `t`; go
        // through `get` anyway so the lookup can never panic.
        t.get(i.saturating_sub(1))
            .is_none_or(|prev| !prev.is_alphanumeric())
    };

    let mut qi = 0usize;
    let mut score = 0i32;
    let mut prev_match: Option<usize> = None;
    let mut streak = 0i32;

    for (ti, ch) in t.iter().enumerate() {
        // `q.get` doubles as the "query exhausted" check the old `qi >=
        // q.len()` guard made explicit.
        let Some(&qc) = q.get(qi) else {
            break;
        };
        if *ch == qc {
            let mut bonus = 1i32;
            match prev_match {
                Some(pm) if ti.checked_sub(1) == Some(pm) => {
                    // Contiguous with the previous match — reward the streak so
                    // "docs" beats a scattered "d…o…c…s".
                    streak = streak.saturating_add(1);
                    bonus = bonus.saturating_add(2).saturating_add(streak);
                }
                Some(pm) => {
                    // A gap: pay for the distance skipped, reset the streak.
                    let gap = ti.saturating_sub(pm).saturating_sub(1).min(3);
                    bonus = bonus.saturating_sub(i32::try_from(gap).unwrap_or(3));
                    streak = 0;
                }
                None => {
                    // First matched char: reward starting at, or near, the front.
                    if ti == 0 {
                        bonus = bonus.saturating_add(4);
                    } else if is_boundary(ti) {
                        bonus = bonus.saturating_add(3);
                    } else {
                        let penalty = i32::try_from(ti.min(3)).unwrap_or(3);
                        bonus = bonus.saturating_sub(penalty);
                    }
                    streak = 0;
                }
            }
            if prev_match.is_some() && is_boundary(ti) {
                bonus = bonus.saturating_add(2);
            }
            score = score.saturating_add(bonus);
            prev_match = Some(ti);
            qi = qi.saturating_add(1);
        }
    }

    if qi < q.len() { None } else { Some(score) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_resource_search_indexes_nested_resources_and_prefers_view_actions() {
        let nested = tilo_actions::Resource::new("note:launch", "notes.note", "Launch plan")
            .with_action(
                tilo_actions::ActionSpec::operation(
                    "delete",
                    "Delete note",
                    tilo_actions::OperationKind::Effect,
                    "test.note",
                    &serde_json::json!({}),
                )
                .unwrap(),
            )
            .with_action(
                tilo_actions::ActionSpec::operation(
                    "open",
                    "Open note",
                    tilo_actions::OperationKind::View,
                    "test.note",
                    &serde_json::json!({}),
                )
                .unwrap(),
            );
        let root =
            tilo_actions::Resource::new("notes", "notes.collection", "Notes").with_child(nested);
        let provider = OpenResourceSearch::new(&[root]);

        let hit = provider
            .search("launch")
            .pop()
            .expect("resource is findable");
        let address = hit.action_address().expect("canonical activation");
        assert_eq!(address, ActionAddress::new("note:launch", "open"));
        assert_eq!(
            provider
                .resolve_action_resource(&address)
                .expect("fresh authority resource")
                .label,
            "Launch plan"
        );
        assert_eq!(provider.action_directory_generation(), None);
    }

    #[test]
    fn open_resource_search_pages_with_hard_budgets() {
        let resources = (0..20)
            .map(|index| {
                tilo_actions::Resource::new(
                    format!("item:{index}"),
                    "test.item",
                    format!("Item {index}"),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "open",
                        "Open",
                        tilo_actions::OperationKind::View,
                        "test.item",
                        &serde_json::json!({}),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let provider = OpenResourceSearch::new(&resources);
        let page = provider.search_page("", None, 3, 2);
        assert!(page.inspected <= 3);
        assert!(page.results.len() <= 2);
        assert!(page.next.is_some());
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert_eq!(fuzzy_score("xyz", "alphabet"), None);
        assert_eq!(fuzzy_score("abz", "abc"), None);
    }

    #[test]
    fn canonical_action_address_round_trips_without_changing_legacy_shape() {
        let address = ActionAddress::new("contacts.contact:ada", "open");
        let hit = SearchResult::for_action("Contacts", "Ada", address.clone());
        assert_eq!(hit.action_address(), Some(address));

        let legacy = SearchResult::new("Contacts", "Ada", "contact:ada");
        assert_eq!(legacy.action_address(), None);
    }

    #[test]
    fn prefix_beats_midword_beats_scattered() {
        // "cal" as a prefix of "Calculator" must outrank the same letters
        // scattered through "Critical Analysis Log".
        let prefix = fuzzy_score("cal", "Calculator").unwrap();
        let scattered = fuzzy_score("cal", "Critical Analysis Log").unwrap();
        assert!(
            prefix > scattered,
            "prefix {prefix} should beat scattered {scattered}"
        );
    }

    #[test]
    fn word_boundary_match_scores_well() {
        // Matching the start of the second word ("Ng") should score positively
        // — a common case for "last name" queries over "First Last".
        let boundary = fuzzy_score("ng", "Alice Ng").unwrap();
        assert!(
            boundary > 0,
            "word-boundary match should be positive, got {boundary}"
        );
    }

    #[test]
    fn contiguous_beats_gappy() {
        let contiguous = fuzzy_score("abc", "abcdef").unwrap();
        let gappy = fuzzy_score("abc", "a_b_c_def").unwrap();
        assert!(
            contiguous > gappy,
            "contiguous {contiguous} should beat gappy {gappy}"
        );
    }

    #[test]
    fn is_case_insensitive() {
        assert!(fuzzy_score("ALICE", "alice").is_some());
        assert_eq!(fuzzy_score("Alice", "alice"), fuzzy_score("alice", "ALICE"));
    }

    #[test]
    fn result_builders_populate_fields() {
        let r = SearchResult::new("Contacts", "Alice Ng", "contact:42")
            .with_subtitle("alice@example.com")
            .with_icon(Icon::Mail)
            .with_score(5);
        assert_eq!(r.kind, "Contacts");
        assert_eq!(r.title, "Alice Ng");
        assert_eq!(r.action_id, "contact:42");
        assert_eq!(r.subtitle.as_deref(), Some("alice@example.com"));
        assert_eq!(r.icon, Some(Icon::Mail));
        assert_eq!(r.score, 5);
    }

    #[test]
    fn shared_text_index_keeps_the_complete_corpus() {
        let texts =
            (0..(DEFAULT_SEARCH_INDEX_LIMIT + 500)).map(|index| format!("Document {index}"));
        let index = SearchTextIndex::new(texts);

        assert_eq!(index.len(), DEFAULT_SEARCH_INDEX_LIMIT + 500);
        assert_eq!(
            index.matching_ids("Document 4595", DEFAULT_SEARCH_CANDIDATE_LIMIT),
            vec![4595]
        );
    }

    #[test]
    fn shared_text_index_is_unicode_aware_and_case_insensitive() {
        let index = SearchTextIndex::new(["Résumé — Αθήνα", "東京 notes"]);

        assert_eq!(index.matching_ids("ΑΘΉ", 8), vec![0]);
        assert_eq!(index.matching_ids("東京", 8), vec![1]);
        assert!(index.matching_ids("resume", 8).is_empty());
    }

    #[test]
    fn shared_text_index_honours_the_query_budget() {
        let index = SearchTextIndex::new((0..1_000).map(|i| format!("common item {i}")));
        assert_eq!(index.matching_ids("common", 7).len(), 7);
        assert!(index.matching_ids("common", 0).is_empty());
    }

    #[test]
    fn shared_text_index_clamps_hostile_page_budgets() {
        let index = SearchTextIndex::new((0..1_000).map(|i| format!("common item {i}")));
        let page = index.matching_page("common", None, usize::MAX, usize::MAX);

        assert_eq!(page.inspected, DEFAULT_SEARCH_CANDIDATE_LIMIT);
        assert_eq!(page.ids.len(), DEFAULT_SEARCH_RESULT_LIMIT);
        assert!(
            page.next_after.is_some(),
            "the bounded page remains continuable"
        );
    }

    #[test]
    fn shared_text_index_pages_a_broad_hundred_thousand_row_posting() {
        let index = SearchTextIndex::new(
            std::iter::once("common common item 0".to_owned())
                .chain((1..100_001).map(|i| format!("common item {i}"))),
        );
        let first = index.matching_page("common", None, 7, 3);
        assert_eq!(first.inspected, 3);
        assert_eq!(first.ids, vec![0, 1, 2]);
        let second = index.matching_page("common", first.next_after, 7, 3);
        assert_eq!(second.inspected, 3);
        assert_eq!(second.ids, vec![3, 4, 5]);
    }

    #[test]
    fn oversized_query_stops_before_index_work_or_materialisation() {
        let catalog = SearchCatalog::new("Documents");
        for index in 0..100_001 {
            let id = format!("document:{index:06}");
            catalog
                .upsert(&id, "common searchable document", catalog_hit(&id, &id))
                .unwrap();
        }
        let query = "x".repeat(MAX_SEARCH_QUERY_SCALARS + 1);

        let page = catalog.query_page(&query, None, 1, 1);
        assert!(page.results.is_empty());
        assert!(page.next.is_none());
        assert_eq!(page.inspected, 0, "input rejection must precede scans");
        assert!(
            SearchTextIndex::new(["common searchable document"])
                .matching_ids(&query, 1)
                .is_empty()
        );
    }

    #[test]
    fn bounded_query_normalization_keeps_unicode_case_matching() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .upsert(
                "document:munich",
                "MÜNCHEN 夏",
                catalog_hit("document:munich", "München"),
            )
            .unwrap();

        let page = catalog.query_page("mÜnChEn 夏", None, 1, 1);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.inspected, 1);
    }

    #[test]
    fn embedded_nul_substrings_remain_distinct() {
        let index = SearchTextIndex::new(["a", "a\0", "a\0\0"]);
        assert_eq!(index.matching_ids("a\0\0", 8), vec![2]);
    }

    #[test]
    fn repeated_occurrences_do_not_consume_the_result_budget_twice() {
        let index = SearchTextIndex::new(["aaaaaa", "aaab"]);
        assert_eq!(index.matching_ids("aaa", 2), vec![0, 1]);
    }

    #[test]
    fn exact_suffix_range_does_not_lose_late_matches_to_false_candidates() {
        let false_candidate = "abcXbcdXcdeXdefXefgXfgh";
        let mut texts = vec![false_candidate; DEFAULT_SEARCH_CANDIDATE_LIMIT + 40];
        texts.push("the exact abcdefgh resource");
        let index = SearchTextIndex::new(texts);

        assert_eq!(
            index.matching_ids("abcdefgh", 1),
            vec![DEFAULT_SEARCH_CANDIDATE_LIMIT + 40]
        );
    }

    #[test]
    fn suffix_pages_match_naive_exact_record_order_across_cursors_and_budgets() {
        let source = ["banana", "bandana", "ananas", "", "BANANA"];
        let index = SearchTextIndex::new(source);
        for query in ["a", "ana", "nana", "band", "missing"] {
            let normalized = normalize_search_text(query);
            let matches: Vec<usize> = source
                .iter()
                .enumerate()
                .filter_map(|(record_id, text)| {
                    normalize_search_text(text)
                        .contains(&normalized)
                        .then_some(record_id)
                })
                .collect();
            for after in [None, Some(0), Some(1), Some(3), Some(usize::MAX)] {
                for budget in 1..=3 {
                    let remaining: Vec<usize> = matches
                        .iter()
                        .copied()
                        .filter(|record_id| after.is_none_or(|after| *record_id > after))
                        .collect();
                    let expected: Vec<usize> = remaining.iter().copied().take(budget).collect();
                    let expected_next = expected
                        .last()
                        .copied()
                        .filter(|_| remaining.len() > expected.len());
                    let page = index.matching_page(query, after, budget, budget);
                    assert_eq!(page.ids, expected, "query={query:?}, after={after:?}");
                    assert_eq!(
                        page.next_after, expected_next,
                        "query={query:?}, after={after:?}"
                    );
                    assert_eq!(page.inspected, page.ids.len());
                }
            }
        }
    }

    fn catalog_hit(resource_id: &str, title: impl Into<String>) -> SearchResult {
        SearchResult::for_action("Documents", title, ActionAddress::new(resource_id, "open"))
    }

    #[test]
    fn incremental_catalog_finds_unique_resource_beyond_one_hundred_thousand() {
        let catalog = SearchCatalog::new("Documents");
        for index in 0..100_001 {
            let id = format!("document:{index:06}");
            let text = if index == 100_000 {
                "row cobalt-orchid-needle"
            } else {
                "row"
            };
            catalog
                .upsert(&id, text, catalog_hit(&id, format!("Document {index}")))
                .unwrap();
        }

        let page = catalog.query_page("cobalt-orchid-needle", None, 8, 8);
        assert_eq!(page.results.len(), 1);
        assert_eq!(
            page.results[0].action_address(),
            Some(ActionAddress::new("document:100000", "open"))
        );
        assert_eq!(page.inspected, 1, "the rare posting avoids a corpus scan");
        let stats = catalog.stats();
        assert_eq!(stats.records, 100_001);
        assert!(
            stats.postings <= stats.normalized_scalars.saturating_mul(4),
            "posting storage stays linear: {stats:?}"
        );
    }

    #[test]
    fn catalog_edits_and_deletes_only_change_incremental_generation() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .upsert("document:a", "old phrase", catalog_hit("document:a", "A"))
            .unwrap();
        let seeded = catalog.stats();
        catalog
            .upsert("document:a", "old phrase", catalog_hit("document:a", "A"))
            .unwrap();
        assert_eq!(
            catalog.stats(),
            seeded,
            "idempotent sync does no index work"
        );
        let provider = catalog.clone();
        catalog
            .upsert("document:a", "new phrase", catalog_hit("document:a", "A"))
            .unwrap();
        assert!(provider.search_limited("old", 4).is_empty());
        assert_eq!(provider.search_limited("new", 4).len(), 1);
        assert_eq!(catalog.stats().generation, seeded.generation + 1);
        assert!(catalog.remove("document:a"));
        assert!(provider.search_limited("new", 4).is_empty());
        assert_eq!(catalog.stats().records, 0);
    }

    #[test]
    fn adversarial_exact_verification_can_continue_in_fixed_work_pages() {
        let catalog = SearchCatalog::new("Documents");
        for index in 0..1_000 {
            let id = format!("document:a{index:04}");
            catalog
                .upsert(&id, "abc c d", catalog_hit(&id, "False candidate"))
                .unwrap();
        }
        catalog
            .upsert(
                "document:z-target",
                "the exact abc d resource",
                catalog_hit("document:z-target", "Target"),
            )
            .unwrap();

        let mut cursor = None;
        let mut found = Vec::new();
        let mut pages = 0usize;
        loop {
            let page = catalog.query_page("abc d", cursor.as_ref(), 31, 4);
            assert!(page.inspected <= 31);
            pages = pages.saturating_add(1);
            found.extend(page.results);
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert!(pages <= 1_001usize.div_ceil(31));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Target");
    }

    #[test]
    fn catalogue_clamps_hostile_page_budgets_without_truncating_discovery() {
        let catalog = SearchCatalog::new("Documents");
        for index in 0..1_000 {
            let id = format!("document:{index:04}");
            assert!(
                catalog
                    .upsert(&id, "common", catalog_hit(&id, format!("Document {index}")))
                    .is_ok()
            );
        }

        let page = catalog.query_page("common", None, usize::MAX, usize::MAX);
        assert_eq!(page.inspected, DEFAULT_SEARCH_CANDIDATE_LIMIT);
        assert_eq!(page.results.len(), DEFAULT_SEARCH_RESULT_LIMIT);
        assert!(page.next.is_some(), "the bounded page remains continuable");
    }

    #[test]
    fn catalogue_rejects_legacy_or_mismatched_activation() {
        let catalog = SearchCatalog::new("Documents");
        assert_eq!(
            catalog.upsert(
                "document:a",
                "A",
                SearchResult::new("Documents", "A", "legacy-open")
            ),
            Err(SearchCatalogError::NonCanonicalResult)
        );
        assert_eq!(
            catalog.upsert("document:a", "A", catalog_hit("document:different", "A")),
            Err(SearchCatalogError::ResourceAddressMismatch)
        );
    }

    #[test]
    fn catalogue_refuses_generation_reuse_without_partial_mutation() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .upsert("document:a", "old phrase", catalog_hit("document:a", "A"))
            .unwrap();
        catalog
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation = u64::MAX;

        assert_eq!(
            catalog.upsert("document:a", "new phrase", catalog_hit("document:a", "A")),
            Err(SearchCatalogError::GenerationExhausted)
        );
        assert_eq!(catalog.search_limited("old", 4).len(), 1);
        assert!(catalog.search_limited("new", 4).is_empty());
        assert!(!catalog.remove("document:a"));
        assert_eq!(catalog.stats().records, 1);
        assert_eq!(catalog.stats().generation, u64::MAX);
    }

    #[test]
    fn catalogue_refuses_browse_ordinal_reuse_without_inserting() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_browse_ordinal = u64::MAX;

        assert_eq!(
            catalog.upsert("document:a", "phrase", catalog_hit("document:a", "A")),
            Err(SearchCatalogError::BrowseOrdinalExhausted)
        );
        assert_eq!(catalog.stats(), SearchCatalogStats::default());
        assert!(catalog.query_page("", None, 4, 4).results.is_empty());
    }
}
