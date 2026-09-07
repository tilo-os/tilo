//! The [`SearchProvider`] trait, the typed [`SearchResult`] it returns, and the
//! shared [`fuzzy_score`] ranking primitive the registry uses to merge results
//! from many providers on an even footing.

use crate::icons::Icon;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use qos_actions::ActionAddress;
use std::borrow::Borrow;

/// A compact, reusable exact substring index for app search providers.
///
/// The complete supplied corpus is retained. Normalisation and Unicode
/// 1/2/3-gram posting lists are built once. Unlike a suffix array, this stores
/// only a constant number of postings per input scalar rather than retaining
/// and comparing every suffix.
/// Providers keep
/// ownership of their domain records and use the returned stable snapshot
/// offsets to materialise typed [`SearchResult`]s.
///
/// Each suffix is represented by a record id and UTF-8 boundary, so storage is
/// linear in the amount of normalised searchable text. Embedded NULs and all
/// Unicode scalar values retain their ordinary string semantics.
#[derive(Clone, Debug, Default)]
pub struct SearchTextIndex {
    texts: Vec<String>,
    postings: HashMap<String, BTreeSet<usize>>,
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
        let mut postings: HashMap<String, BTreeSet<usize>> = HashMap::new();
        for (record_id, text) in texts.iter().enumerate() {
            for gram in index_grams(text) {
                postings.entry(gram).or_default().insert(record_id);
            }
        }

        Self { texts, postings }
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
        if budget == 0 {
            return Vec::new();
        }
        let query = normalize_search_text(query);
        if query.is_empty() {
            return (0..self.texts.len()).take(budget).collect();
        }

        let grams = search_grams(&query);
        let Some(candidates) = rarest_posting(&self.postings, &grams) else {
            return Vec::new();
        };
        candidates
            .iter()
            .copied()
            .filter(|record_id| {
                grams.iter().all(|gram| {
                    self.postings
                        .get(gram)
                        .is_some_and(|posting| posting.contains(record_id))
                }) && self
                    .texts
                    .get(*record_id)
                    .is_some_and(|text| text.contains(&query))
            })
            .take(budget)
            .collect()
    }

    /// The normalised text stored for a snapshot offset, for provider-specific
    /// ranking without repeating normalisation on every keystroke.
    #[must_use]
    pub fn normalized_text(&self, record_id: usize) -> Option<&str> {
        self.texts.get(record_id).map(String::as_str)
    }
}

fn search_grams(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let width = chars.len().min(3);
    let mut grams = BTreeSet::new();
    for window in chars.windows(width) {
        grams.insert(window.iter().collect());
    }
    grams.into_iter().collect()
}

fn index_grams(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut grams = BTreeSet::new();
    for width in 1..=chars.len().min(3) {
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

/// Opaque continuation for a bounded [`SearchCatalog`] query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchCursor {
    after_resource_id: Option<String>,
    after_browse_ordinal: Option<u64>,
    generation: u64,
    normalized_query: String,
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
/// The index uses Unicode 1/2/3-gram and complete-term postings plus exact verification. A unique
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
        if work_budget == 0 || result_limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let query = normalize_search_text(query);
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
/// to contain qos-actions' wire separator.
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
    fn resolve_action_resource(&self, _address: &ActionAddress) -> Option<qos_actions::Resource> {
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
        let mut texts = vec!["abcXbcd"; DEFAULT_SEARCH_CANDIDATE_LIMIT + 40];
        texts.push("the exact abcd resource");
        let index = SearchTextIndex::new(texts);

        assert_eq!(
            index.matching_ids("abcd", DEFAULT_SEARCH_CANDIDATE_LIMIT),
            vec![DEFAULT_SEARCH_CANDIDATE_LIMIT + 40]
        );
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
                .upsert(&id, "abcXbcd", catalog_hit(&id, "False candidate"))
                .unwrap();
        }
        catalog
            .upsert(
                "document:z-target",
                "the exact abcd resource",
                catalog_hit("document:z-target", "Target"),
            )
            .unwrap();

        let mut cursor = None;
        let mut found = Vec::new();
        let mut pages = 0;
        loop {
            let page = catalog.query_page("abcd", cursor.as_ref(), 31, 4);
            assert!(page.inspected <= 31);
            pages += 1;
            found.extend(page.results);
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert!(pages > 1);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Target");
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
