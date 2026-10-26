//! The [`SearchProvider`] trait, the typed [`SearchResult`] it returns, and the
//! retained [`SearchCatalog`] app providers answer from, which keeps its
//! records in a [`tilo_search::SearchIndex`].

use super::body::{BodyIndex, SearchBody};
use super::ordinals;
use super::query::{Facet, SearchQuery};
use super::rank::FieldWeight;
use super::row::SearchRow;
use crate::icons::Icon;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tilo_search::{Document, Page, SearchIndex};

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
/// Default maximum number of hits an indexed provider materialises for one
/// interactive query page. The index holds the complete corpus; this is a
/// query-time latency budget, not a data-loss limit.
pub const DEFAULT_SEARCH_CANDIDATE_LIMIT: usize = 256;
/// Maximum Unicode scalar values accepted from one interactive query.
///
/// This is an input-work ceiling, independent of the page budgets. A pasted
/// multi-megabyte string must not become one gram phrase per scalar on the
/// UI thread. The limit is far above a meaningful palette query; callers
/// receive an empty page for longer input and can refine it normally.
pub const MAX_SEARCH_QUERY_SCALARS: usize = 1_024;
use std::borrow::Borrow;
use tilo_actions::ActionAddress;
use tilo_lock::RwLockRecover as _;

/// Fold text the way every index does: case, compatibility forms and
/// alphabetic diacritics (see [`tilo_search::fold()`]).
#[must_use]
pub fn normalize_search_text(text: &str) -> String {
    tilo_search::fold(text)
}

/// Whether an interactive query is short enough to search at all.
fn query_is_bounded(query: &str) -> bool {
    query.chars().nth(MAX_SEARCH_QUERY_SCALARS).is_none()
}

/// Opaque continuation for a bounded [`SearchCatalog`] query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchCursor {
    after_resource_id: Option<String>,
    after_browse_ordinal: Option<u64>,
    generation: u64,
    normalized_query: String,
    /// Hits of the query already returned.
    offset: usize,
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
            offset: 0,
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

/// One bounded page of a provider's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    pub results: Vec<SearchResult>,
    /// Pass this back to `search_page` to continue. `None` means exhausted.
    pub next: Option<SearchCursor>,
    /// Hits materialised for this page.
    pub inspected: usize,
    /// More results for this query are on their way from a slow source (a
    /// mail server, a video service, a catalogue): the palette keeps a
    /// loading row and asks [`SearchProvider::poll_page`] with this token
    /// until they arrive, then appends them to the same list.
    pub pending: Option<PendingToken>,
}

impl SearchPage {
    /// A page with no results and no continuation.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            results: Vec::new(),
            next: None,
            inspected: 0,
            pending: None,
        }
    }

    /// A finished page: these results, continuing at `next`.
    #[must_use]
    pub fn of(results: Vec<SearchResult>, next: Option<SearchCursor>) -> Self {
        Self {
            inspected: results.len(),
            results,
            next,
            pending: None,
        }
    }

    /// A page whose results are still coming: show `results` now and poll
    /// `token` for the rest.
    #[must_use]
    pub fn pending(results: Vec<SearchResult>, token: PendingToken) -> Self {
        Self {
            inspected: results.len(),
            results,
            next: None,
            pending: Some(token),
        }
    }
}

/// A provider's handle on one outstanding request to a slow source. Opaque
/// to the palette, which only hands it back to [`SearchProvider::poll_page`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PendingToken(u64);

impl PendingToken {
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn id(&self) -> u64 {
        self.0
    }
}

/// The answer to [`SearchProvider::poll_page`].
#[derive(Clone, Debug, PartialEq)]
pub enum PagePoll {
    /// Still waiting; ask again on a later frame.
    Pending,
    /// The awaited page. It may carry its own `next` or another `pending`.
    Ready(SearchPage),
    /// The whole query answered again, now that the source has what it
    /// lacked (the app has taken the query in): it replaces every row the
    /// provider gave for the query before, rather than adding to them, so a
    /// hit for what was typed earlier does not stand beside it.
    Answered(SearchPage),
}

/// Cheap diagnostics used by contract and performance tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchCatalogStats {
    pub records: usize,
    /// Records carrying a [`SearchBody`].
    pub body_records: usize,
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

/// One resource offered to a [`SearchCatalog`]: its canonical hit, the short
/// weighted fields it is found and ranked by, and optionally a long
/// [`SearchBody`] it is found by without the catalogue copying the prose.
#[derive(Clone, Debug)]
pub struct SearchRecord {
    resource_id: String,
    result: SearchResult,
    fields: Vec<(String, FieldWeight)>,
    body: Option<SearchBody>,
}

impl SearchRecord {
    /// A record for `resource_id`, activated through `result`, whose
    /// canonical action must address that same resource.
    #[must_use]
    pub fn new(resource_id: impl Into<String>, result: SearchResult) -> Self {
        Self {
            resource_id: resource_id.into(),
            result,
            fields: Vec::new(),
            body: None,
        }
    }

    /// Add a short searchable field. A page of hits is ordered by the best
    /// weighted field match.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: FieldWeight) -> Self {
        self.fields.push((text.into(), weight));
        self
    }

    /// Attach a long body. A match inside it follows every field match of
    /// the same query and reports the body source's location for the hit.
    #[must_use]
    pub fn body(mut self, body: SearchBody) -> Self {
        self.body = Some(body);
        self
    }

    fn legacy(resource_id: String, searchable_text: &str, result: SearchResult) -> Self {
        Self::new(resource_id, result).field(searchable_text, FieldWeight::UNRANKED)
    }
}

/// What the catalogue keeps per resource besides its indexed fields.
#[derive(Clone, Debug)]
struct CatalogEntry {
    result: SearchResult,
    body: Option<SearchBody>,
}

#[derive(Debug, Default)]
struct SearchCatalogInner {
    /// Keyed by browse ordinal, so equally relevant hits and empty-query
    /// browsing both follow insertion order.
    index: SearchIndex,
    entries: BTreeMap<u64, CatalogEntry>,
    ordinals: HashMap<Arc<str>, u64>,
    body_records: usize,
    generation: u64,
    next_browse_ordinal: u64,
}

impl SearchCatalogInner {
    fn same_content(&self, ordinal: u64, record: &SearchRecord) -> bool {
        self.entries
            .get(&ordinal)
            .is_some_and(|entry| entry.result == record.result && entry.body == record.body)
            && self
                .index
                .get(ordinal)
                .is_some_and(|document| document.fields() == record.fields.as_slice())
    }

    fn insert(&mut self, ordinal: u64, resource_id: Arc<str>, record: SearchRecord) {
        let SearchRecord {
            result,
            fields,
            body,
            ..
        } = record;
        let mut document = Document::new(ordinal);
        for (text, weight) in fields {
            document = document.field(text, weight);
        }
        if let Some(body) = &body {
            document = document.body(Arc::new(BodyIndex(Arc::clone(body.source()))));
            self.body_records = self.body_records.saturating_add(1);
        }
        self.index.upsert(document);
        self.ordinals.insert(resource_id, ordinal);
        self.entries.insert(ordinal, CatalogEntry { result, body });
    }

    fn take(&mut self, resource_id: &str) -> Option<u64> {
        let ordinal = self.ordinals.remove(resource_id)?;
        if let Some(entry) = self.entries.remove(&ordinal)
            && entry.body.is_some()
        {
            self.body_records = self.body_records.saturating_sub(1);
        }
        self.index.remove(ordinal);
        Some(ordinal)
    }

    fn commit(&mut self) {
        if let Err(error) = self.index.commit() {
            tracing::error!(%error, "could not index search records");
        }
    }

    /// Make `records`, validated and unique, the whole catalogue in their
    /// order, touching only what differs. Returns how many records it
    /// indexed again or dropped.
    fn sync(&mut self, records: Vec<SearchRecord>) -> Result<usize, SearchCatalogError> {
        let held: Vec<Option<u64>> = records
            .iter()
            .map(|record| self.ordinals.get(record.resource_id.as_str()).copied())
            .collect();
        let planned = ordinals::plan(&held).ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
        let wanted: HashSet<&str> = records
            .iter()
            .map(|record| record.resource_id.as_str())
            .collect();
        let removed: Vec<Arc<str>> = self
            .ordinals
            .keys()
            .filter(|id| !wanted.contains(id.as_ref()))
            .cloned()
            .collect();
        // Placed records (new, moved, or changed in place) by position.
        let placed: Vec<usize> = (0..records.len())
            .filter(|&at| {
                let (Some(record), Some(ordinal)) = (records.get(at), planned.get(at)) else {
                    return false;
                };
                held.get(at).copied().flatten() != Some(*ordinal)
                    || !self.same_content(*ordinal, record)
            })
            .collect();
        drop(wanted);
        if removed.is_empty() && placed.is_empty() {
            return Ok(0);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        let last = planned.last().copied().unwrap_or(0);
        let next_browse_ordinal = self.next_browse_ordinal.max(
            last.checked_add(1)
                .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?,
        );
        // Free every ordinal that changes hands before taking any: a placed
        // record may take one a moved record held.
        for id in &removed {
            self.take(id);
        }
        for &at in &placed {
            if let Some(record) = records.get(at) {
                self.take(&record.resource_id);
            }
        }
        let mut records: Vec<Option<SearchRecord>> = records.into_iter().map(Some).collect();
        for &at in &placed {
            let (Some(record), Some(&ordinal)) =
                (records.get_mut(at).and_then(Option::take), planned.get(at))
            else {
                continue;
            };
            let resource_id: Arc<str> = record.resource_id.as_str().into();
            self.insert(ordinal, resource_id, record);
        }
        self.generation = generation;
        self.next_browse_ordinal = next_browse_ordinal;
        Ok(removed.len().saturating_add(placed.len()))
    }
}

/// A complete, incremental search catalogue keyed by canonical resource id.
///
/// Cloning a catalogue is O(1), so a widget can retain one instance and return
/// a cheap provider handle whenever Ctrl+K opens. Mutations are recorded
/// immediately and indexed in one batch before the next query. Queries are
/// paged by both result count and work budget; result objects are cloned
/// only for the hits in the requested page.
///
/// Fields are found by folded substring and ranked by the best weighted
/// field match (see [`tilo_search`]). Bodies ([`SearchRecord::body`]) are
/// found after every field hit, and a body source that names locations
/// reports the segment a match is in. A unique item is found directly in a
/// corpus of any size; broad queries continue through `SearchCursor` pages.
#[derive(Clone, Debug)]
pub struct SearchCatalog {
    name: Arc<str>,
    inner: Arc<RwLock<SearchCatalogInner>>,
}

fn validate(record: &SearchRecord) -> Result<(), SearchCatalogError> {
    if record.resource_id.is_empty() {
        return Err(SearchCatalogError::EmptyResourceId);
    }
    let Some(address) = record.result.action_address() else {
        return Err(SearchCatalogError::NonCanonicalResult);
    };
    // A record names its resource, or one action of it when a provider
    // lists each action of a resource as its own row.
    if address.resource != record.resource_id && address.encode() != record.resource_id {
        return Err(SearchCatalogError::ResourceAddressMismatch);
    }
    Ok(())
}

impl SearchCatalog {
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            inner: Arc::new(RwLock::new(SearchCatalogInner::default())),
        }
    }

    /// An independent copy of this catalogue at its current generation. A
    /// session can extend the copy without the published catalogue — or any
    /// other session sharing it — observing the change.
    #[must_use]
    pub fn detached(&self) -> Self {
        let inner = self.inner.read_recover();
        let copy = SearchCatalogInner {
            index: inner.index.detached(),
            entries: inner.entries.clone(),
            ordinals: inner.ordinals.clone(),
            body_records: inner.body_records,
            generation: inner.generation,
            next_browse_ordinal: inner.next_browse_ordinal,
        };
        drop(inner);
        Self {
            name: Arc::clone(&self.name),
            inner: Arc::new(RwLock::new(copy)),
        }
    }

    /// Replace an immutable snapshot in one generation. This is the bulk
    /// counterpart to [`Self::upsert`]: the whole snapshot is indexed in one
    /// build, while later incremental updates retain their existing
    /// semantics.
    pub fn replace_all(
        &self,
        rows: impl IntoIterator<Item = (String, String, SearchResult)>,
    ) -> Result<(), SearchCatalogError> {
        self.replace_all_records(
            rows.into_iter().map(|(resource_id, text, result)| {
                SearchRecord::legacy(resource_id, &text, result)
            }),
        )
    }

    /// [`Self::replace_all`] for weighted records with optional bodies.
    ///
    /// # Errors
    ///
    /// As [`Self::replace_all`].
    pub fn replace_all_records(
        &self,
        records: impl IntoIterator<Item = SearchRecord>,
    ) -> Result<(), SearchCatalogError> {
        // A fresh index indexes everything in one bulk build on commit.
        let mut next = SearchCatalogInner {
            next_browse_ordinal: ordinals::GAP,
            ..SearchCatalogInner::default()
        };
        for record in records {
            validate(&record)?;
            let resource_id: Arc<str> = record.resource_id.as_str().into();
            if next.ordinals.contains_key(&resource_id) {
                return Err(SearchCatalogError::DuplicateResourceId);
            }
            // A gap apart, so a later sync can place records between.
            let ordinal = next.next_browse_ordinal;
            next.next_browse_ordinal = ordinal
                .checked_add(ordinals::GAP)
                .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
            next.insert(ordinal, resource_id, record);
        }
        let mut inner = self.inner.write_recover();
        next.generation = inner
            .generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        *inner = next;
        drop(inner);
        Ok(())
    }

    /// [`Self::sync_records`] for legacy `(resource id, searchable text,
    /// result)` rows, as [`Self::replace_all`] takes them.
    ///
    /// # Errors
    ///
    /// As [`Self::sync_records`].
    pub fn sync(
        &self,
        rows: impl IntoIterator<Item = (String, String, SearchResult)>,
    ) -> Result<bool, SearchCatalogError> {
        self.sync_records(
            rows.into_iter().map(|(resource_id, text, result)| {
                SearchRecord::legacy(resource_id, &text, result)
            }),
        )
    }

    /// Make `records`, in their order, the whole catalogue, as
    /// [`Self::replace_all_records`] would, indexing only the records that
    /// are new, changed or moved, and dropping those absent. Hits and
    /// browsing come in the same order a bulk replacement gives. The
    /// generation advances once if anything changed and not at all
    /// otherwise, so an unchanged snapshot keeps every cursor current.
    ///
    /// The catalogue's write lock is held only to compare and stage the
    /// records; indexing them waits for the next query or
    /// [`Self::index_bytes`], incrementally when few changed.
    ///
    /// # Errors
    ///
    /// As [`Self::replace_all_records`]; the catalogue is then unchanged.
    pub fn sync_records(
        &self,
        records: impl IntoIterator<Item = SearchRecord>,
    ) -> Result<bool, SearchCatalogError> {
        let records: Vec<SearchRecord> = records.into_iter().collect();
        let mut seen = HashSet::with_capacity(records.len());
        for record in &records {
            validate(record)?;
            if !seen.insert(record.resource_id.as_str()) {
                return Err(SearchCatalogError::DuplicateResourceId);
            }
        }
        drop(seen);
        let mut inner = self.inner.write_recover();
        let touched = inner.sync(records);
        drop(inner);
        touched.map(|touched| touched > 0)
    }

    /// Whether a canonical resource is currently retained by this catalogue.
    #[must_use]
    pub fn contains(&self, resource_id: &str) -> bool {
        self.inner.read_recover().ordinals.contains_key(resource_id)
    }

    /// Insert or replace one durable resource. `searchable_text` may combine
    /// title, metadata and body; `result` is materialised only when it is a
    /// hit on a requested page.
    pub fn upsert(
        &self,
        resource_id: impl Into<String>,
        searchable_text: impl AsRef<str>,
        result: SearchResult,
    ) -> Result<(), SearchCatalogError> {
        self.upsert_record(SearchRecord::legacy(
            resource_id.into(),
            searchable_text.as_ref(),
            result,
        ))
    }

    /// [`Self::upsert`] for a weighted record with an optional body.
    ///
    /// # Errors
    ///
    /// As [`Self::upsert`].
    pub fn upsert_record(&self, record: SearchRecord) -> Result<(), SearchCatalogError> {
        validate(&record)?;
        let mut inner = self.inner.write_recover();
        let existing = inner.ordinals.get(record.resource_id.as_str()).copied();
        if existing.is_some_and(|ordinal| inner.same_content(ordinal, &record)) {
            return Ok(());
        }
        let next_generation = inner
            .generation
            .checked_add(1)
            .ok_or(SearchCatalogError::GenerationExhausted)?;
        let ordinal = if let Some(ordinal) = existing {
            ordinal
        } else {
            let ordinal = inner.next_browse_ordinal;
            inner.next_browse_ordinal = ordinal
                .checked_add(1)
                .ok_or(SearchCatalogError::BrowseOrdinalExhausted)?;
            ordinal
        };
        let resource_id: Arc<str> = record.resource_id.as_str().into();
        inner.take(&resource_id);
        inner.insert(ordinal, resource_id, record);
        inner.generation = next_generation;
        drop(inner);
        Ok(())
    }

    /// Remove one resource. Returns whether it existed.
    pub fn remove(&self, resource_id: &str) -> bool {
        let mut inner = self.inner.write_recover();
        // Returning `false` is the existing fail-closed contract for a remove
        // that was not performed. Do not mutate at MAX: keeping the row is
        // safer than making a previously-issued cursor current again.
        if inner.generation == u64::MAX || inner.take(resource_id).is_none() {
            return false;
        }
        inner.generation = inner.generation.saturating_add(1);
        drop(inner);
        true
    }

    #[must_use]
    pub fn stats(&self) -> SearchCatalogStats {
        let inner = self.inner.read_recover();
        let stats = SearchCatalogStats {
            records: inner.entries.len(),
            body_records: inner.body_records,
            generation: inner.generation,
        };
        drop(inner);
        stats
    }

    /// Bytes the catalogue's index occupies, once every mutation is indexed.
    #[must_use]
    pub fn index_bytes(&self) -> u64 {
        self.commit_pending();
        self.inner.read_recover().index.index_bytes()
    }

    /// Whether a prior cursor belongs to this exact retained-index
    /// generation. The pull directory must turn a mismatch into a stale
    /// cursor response, never reinterpret it as an exhausted result set.
    #[must_use]
    pub fn cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.stats().generation == cursor.generation
    }

    fn commit_pending(&self) {
        let dirty = self.inner.read_recover().index.is_dirty();
        if dirty {
            self.inner.write_recover().commit();
        }
    }

    /// Query one deterministic page. Both budgets are hard ceilings. Continue
    /// with the returned cursor when `next` is set.
    #[must_use]
    pub fn query_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        // Providers and remote callers may request a smaller page, but no
        // caller can enlarge one turn beyond the interactive latency and
        // materialisation ceilings. Complete discovery remains available
        // through the returned cursor.
        let limit = work_budget
            .min(DEFAULT_SEARCH_CANDIDATE_LIMIT)
            .min(result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        if limit == 0 || !query_is_bounded(query) {
            return SearchPage::empty();
        }
        let normalized_query = tilo_search::fold(query);
        self.commit_pending();
        let inner = self.inner.read_recover();
        let generation = inner.generation;
        // A catalogue edit between pages invalidates the order. Restarting
        // may repeat rows but can never skip a new or edited resource.
        let offset = cursor
            .filter(|cursor| {
                cursor.generation == generation && cursor.normalized_query == normalized_query
            })
            .map_or(0, |cursor| cursor.offset);
        let found = inner.index.search(query, Page { offset, limit });
        let results: Vec<SearchResult> = found
            .hits
            .iter()
            .filter_map(|hit| {
                let entry = inner.entries.get(&hit.key)?;
                match hit.segment {
                    Some(segment) => entry.body.as_ref()?.source().location(segment),
                    None => Some(entry.result.clone()),
                }
            })
            .collect();
        drop(inner);
        let inspected = found.hits.len();
        SearchPage {
            results,
            next: found.more.then(|| SearchCursor {
                after_resource_id: None,
                after_browse_ordinal: None,
                generation,
                normalized_query,
                offset: offset.saturating_add(inspected),
            }),
            inspected,
            pending: None,
        }
    }
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
    /// What the shared result surface shows of the selected or hovered row,
    /// in the preview band under the list, without changing row geometry.
    pub preview: Option<super::SearchPreview>,
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
    /// What the hit is (a message, a video, a match in a file) and that
    /// row's slots. `None` draws the generic row.
    pub row: Option<SearchRow>,
    /// Provider-supplied relevance bias — a nudge, not an override, since the
    /// registry adds a query-match bonus on top. `0` ranks by query match
    /// alone; raise it to float pinned/recent/favourite hits.
    pub score: i32,
    /// Where the hit lies in the app's anchor path, for scope checks, when
    /// that is not what its action addresses: a contact addressed by its flat
    /// id `contact:42` that lies in `group:family`. `None` scopes the hit by
    /// its action's resource (see [`anchor`](Self::anchor)).
    pub scope_anchor: Option<String>,
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
            row: None,
            score: 0,
            scope_anchor: None,
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
    pub fn with_preview(mut self, preview: impl Into<super::SearchPreview>) -> Self {
        let preview = preview.into();
        self.preview = (!preview.is_empty()).then_some(preview);
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

    /// Draw this hit as `row` (a message, a video, a file hit) instead of
    /// the generic row.
    #[must_use]
    pub fn with_row(mut self, row: SearchRow) -> Self {
        self.row = Some(row);
        self
    }

    /// Place the hit at `anchor` in the app's anchor path for scope checks,
    /// whatever its action addresses: a hit addressed by a flat id
    /// (`sheets.cell:B4`) lies in the scope its path names (`sheet:2`), so it
    /// is narrowed without renaming the resource.
    #[must_use]
    pub fn within(mut self, anchor: impl Into<String>) -> Self {
        self.scope_anchor = Some(anchor.into());
        self
    }

    /// The anchor the hit lies at, for scope checks: its
    /// [`within`](Self::within) anchor, else the resource its action
    /// addresses, or the opaque navigation id.
    #[must_use]
    pub fn anchor(&self) -> String {
        if let Some(anchor) = &self.scope_anchor {
            return anchor.clone();
        }
        self.action_address()
            .map_or_else(|| self.action_id.clone(), |address| address.resource)
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
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let mut page = self
            .catalog
            .query_page(query.text(), cursor, work_budget, result_limit);
        page.results.retain(|hit| query.in_scope(&hit.anchor()));
        page
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
/// providers by relevance. Return everything you consider a match (a
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

    /// Versions this actual provider's complete lazy invocation domain, including
    /// hidden targets and argument/handler semantics omitted from search rows.
    fn action_directory_invocation_domain(&self) -> crate::widget::ActionDirectoryInvocationDomain {
        crate::widget::ActionDirectoryInvocationDomain::Unsupported
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
    /// `query` carries the free text, the facet filters the platform parsed
    /// out of it or the user picked as chips, and the resource scope the
    /// search is narrowed to. A provider that cannot apply a filter or the
    /// scope ignores it; the palette still drops hits outside the scope.
    ///
    /// Providers backed by a durable namespace should override this method so
    /// callers can eventually reach every exact match without scanning the
    /// whole corpus on the UI thread. Tiny providers remain a single page: a
    /// non-empty cursor therefore means they are exhausted.
    ///
    /// A provider whose source answers slowly (a mail server, a web service)
    /// returns what it has now with [`SearchPage::pending`] and answers the
    /// rest from [`poll_page`](Self::poll_page). It never blocks here.
    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        if cursor.is_some() || work_budget == 0 || result_limit == 0 {
            return SearchPage::empty();
        }
        let limit = work_budget.min(result_limit);
        SearchPage::of(self.search_limited(query.text(), limit), None)
    }

    /// The page a [`SearchPage::pending`] token stands for, once its source
    /// has answered. Called on later frames, never in a loop; return
    /// [`PagePoll::Pending`] until the answer is in. A token the provider no
    /// longer knows (the query moved on) answers an empty ready page.
    fn poll_page(&self, _token: &PendingToken) -> PagePoll {
        PagePoll::Ready(SearchPage::empty())
    }

    /// The dimensions this provider filters by. The palette offers their
    /// values as chips and parses typed `key:value` tokens for them into
    /// [`SearchQuery::filters`].
    fn facets(&self) -> Vec<Facet> {
        Vec::new()
    }

    /// The parts of the app this provider's hits can be narrowed to (see
    /// [`ScopeDecl`](super::ScopeDecl)). An unscoped palette offers each as a
    /// "Search in" row that narrows the search to it.
    fn search_scopes(&self) -> Vec<super::ScopeDecl> {
        Vec::new()
    }

    /// Lazily resolve a canonical search address that is intentionally absent
    /// from the small open-resource snapshot.
    fn resolve_action_resource(&self, _address: &ActionAddress) -> Option<tilo_actions::Resource> {
        None
    }

    /// The action that opens one of this provider's hits that names no
    /// action of its own: invoked with the hit's anchor (its `action_id`) as
    /// its one argument, it shows the hit in the app, as picking it in the
    /// app's own search does and as an agent's `open` does. A host that
    /// searches the app from outside it (a desktop's search over the app's
    /// window) opens such hits through it; `None` where hits are opened only
    /// inside the app.
    fn open_action(&self) -> Option<ActionAddress> {
        None
    }
}

/// A [`SearchProvider`] answered entirely from one retained [`SearchCatalog`].
///
/// Implementing this instead of [`SearchProvider`] gets the whole budgeted
/// query surface from one blanket implementation: the action-directory
/// generation and cursor checks read the catalogue's generation, `search` and
/// `search_limited` stop at the caller's result limit, and `search_page`
/// continues through [`SearchCatalog::query_page`] under both hard budgets.
/// Nothing here scans or materialises past a caller's budget, which is the
/// invariant every app provider owes the shell's search service.
pub trait CatalogSearchProvider {
    /// The largest page `search` and `search_limited` return. Providers whose
    /// palette section shows fewer rows lower it.
    const RESULT_LIMIT: usize = DEFAULT_SEARCH_RESULT_LIMIT;

    /// Group label and diagnostic name, as [`SearchProvider::name`].
    fn provider_name(&self) -> &str;

    /// The catalogue answering queries, or `None` while none is published (a
    /// provider with no catalogue reports no generation and empty pages).
    ///
    /// Cloning a [`SearchCatalog`] is O(1), so providers that keep theirs
    /// behind a lock return a clone taken under it.
    fn catalog(&self) -> Option<SearchCatalog>;

    /// Candidate-inspection budget for a one-shot `search_limited(limit)`
    /// call. The default inspects up to [`DEFAULT_SEARCH_CANDIDATE_LIMIT`]
    /// candidates to fill `limit` results; a provider may instead spend no
    /// more inspections than results it asked for. `query_page` caps either
    /// at the shared ceiling.
    fn limited_work_budget(&self, _limit: usize) -> usize {
        DEFAULT_SEARCH_CANDIDATE_LIMIT
    }

    /// Rewrite the caller's query into the form the catalogue was indexed
    /// with, for providers that fold their own text before indexing.
    fn catalog_query<'q>(&self, query: &'q str) -> std::borrow::Cow<'q, str> {
        std::borrow::Cow::Borrowed(query)
    }

    /// As [`SearchProvider::resolve_action_resource`].
    fn resolve_action_address(&self, _address: &ActionAddress) -> Option<tilo_actions::Resource> {
        None
    }

    /// As [`SearchProvider::facets`].
    fn facets(&self) -> Vec<Facet> {
        Vec::new()
    }

    /// As [`SearchProvider::search_scopes`]: the parts of the app this
    /// catalogue's hits can be narrowed to.
    fn search_scopes(&self) -> Vec<super::ScopeDecl> {
        Vec::new()
    }

    /// Whether `hit` passes `query`'s facet filters. The catalogue matches
    /// text only; a provider with facets checks the hit's values here
    /// (`query.admits("language", &[..])`). The default admits every hit.
    fn admits(&self, _hit: &SearchResult, _query: &SearchQuery) -> bool {
        true
    }
}

impl<T: CatalogSearchProvider> SearchProvider for T {
    fn name(&self) -> &str {
        self.provider_name()
    }

    fn action_directory_generation(&self) -> Option<u64> {
        self.catalog().map(|catalog| catalog.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog()
            .is_some_and(|catalog| catalog.cursor_is_current(cursor))
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, T::RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_page(
            &SearchQuery::new(query),
            None,
            self.limited_work_budget(limit),
            limit.min(T::RESULT_LIMIT),
        )
        .results
    }

    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let Some(catalog) = self.catalog() else {
            return SearchPage::empty();
        };
        let mut page = catalog.query_page(
            &self.catalog_query(query.text()),
            cursor,
            work_budget,
            result_limit,
        );
        page.results
            .retain(|hit| query.in_scope(&hit.anchor()) && self.admits(hit, query));
        page
    }

    fn facets(&self) -> Vec<Facet> {
        CatalogSearchProvider::facets(self)
    }

    fn search_scopes(&self) -> Vec<super::ScopeDecl> {
        CatalogSearchProvider::search_scopes(self)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<tilo_actions::Resource> {
        self.resolve_action_address(address)
    }
}

impl CatalogSearchProvider for SearchCatalog {
    fn provider_name(&self) -> &str {
        &self.name
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{FieldWeight, SearchBody, SearchBodySource};

    fn catalog_rows(count: usize) -> SearchCatalog {
        let catalog = SearchCatalog::new("Rows");
        catalog
            .replace_all((0..count).map(|index| {
                let id = format!("row:{index}");
                // Odd rows hold every 4-gram of `abcde` but not the word itself,
                // so they are candidates for it that fail verification.
                let text = if index % 2 == 0 {
                    "xabcdex row"
                } else {
                    "abcd bcde row"
                };
                let result = SearchResult::new("Rows", text, &id)
                    .with_action_address(ActionAddress::new(id.as_str(), "open"));
                (id, text.to_owned(), result)
            }))
            .unwrap();
        catalog
    }

    struct Retained {
        catalog: Option<SearchCatalog>,
        results_as_work: bool,
    }

    impl CatalogSearchProvider for Retained {
        const RESULT_LIMIT: usize = 5;

        fn provider_name(&self) -> &str {
            "Retained"
        }

        fn catalog(&self) -> Option<SearchCatalog> {
            self.catalog.clone()
        }

        fn limited_work_budget(&self, limit: usize) -> usize {
            if self.results_as_work {
                limit
            } else {
                DEFAULT_SEARCH_CANDIDATE_LIMIT
            }
        }

        fn catalog_query<'q>(&self, query: &'q str) -> std::borrow::Cow<'q, str> {
            std::borrow::Cow::Owned(query.to_lowercase())
        }
    }

    #[test]
    fn catalog_provider_answers_within_budgets_or_is_empty() {
        // phase: catalog_provider_answers_from_the_retained_catalogue_within_budgets
        {
            let catalog = catalog_rows(40);
            let provider = Retained {
                catalog: Some(catalog.clone()),
                results_as_work: false,
            };
            assert_eq!(SearchProvider::name(&provider), "Retained");
            assert_eq!(
                provider.action_directory_generation(),
                Some(catalog.stats().generation)
            );

            // `search` stops at the provider's result limit, and the query is
            // folded before it reaches the catalogue.
            assert_eq!(provider.search("ROW").len(), 5);
            assert_eq!(provider.search_limited("row", 100).len(), 5);
            assert_eq!(provider.search_limited("row", 3).len(), 3);

            let page = provider.search_page(&"row".into(), None, 4, 10);
            assert_eq!(page.inspected, 4);
            let next = page.next.expect("a broad query continues by cursor");
            assert!(provider.action_directory_cursor_is_current(&next));
            catalog.remove("row:0");
            assert!(!provider.action_directory_cursor_is_current(&next));
        }
        // phase: catalog_provider_can_spend_no_more_inspections_than_results
        {
            let catalog = catalog_rows(40);
            let generous = Retained {
                catalog: Some(catalog.clone()),
                results_as_work: false,
            };
            let frugal = Retained {
                catalog: Some(catalog),
                results_as_work: true,
            };
            // Every hit is an exact match, so a page is as large as the
            // smaller budget: the frugal provider materialises no more hits
            // than it returns.
            assert_eq!(generous.search_limited("abcde", 2).len(), 2);
            assert_eq!(frugal.search_limited("abcde", 2).len(), 2);
            assert_eq!(frugal.search_page(&"row".into(), None, 2, 5).inspected, 2);
        }
        // phase: catalog_provider_without_a_catalogue_is_empty_and_has_no_generation
        {
            let provider = Retained {
                catalog: None,
                results_as_work: false,
            };
            assert_eq!(provider.action_directory_generation(), None);
            assert!(provider.search("row").is_empty());
            let page = provider.search_page(&"row".into(), None, 10, 10);
            assert!(page.results.is_empty() && page.next.is_none() && page.inspected == 0);
        }
    }

    #[test]
    fn open_resource_search_indexes_nested_resources_within_hard_budgets() {
        // phase: open_resource_search_indexes_nested_resources_and_prefers_view_actions
        {
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
            let root = tilo_actions::Resource::new("notes", "notes.collection", "Notes")
                .with_child(nested);
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
        // phase: open_resource_search_pages_with_hard_budgets
        {
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
            let page = provider.search_page(&"".into(), None, 3, 2);
            assert!(page.inspected <= 3);
            assert!(page.results.len() <= 2);
            assert!(page.next.is_some());
        }
    }

    #[test]
    fn results_and_action_addresses_are_built_without_changing_legacy_shape() {
        // phase: canonical_action_address_round_trips_without_changing_legacy_shape
        {
            let address = ActionAddress::new("contacts.contact:ada", "open");
            let hit = SearchResult::for_action("Contacts", "Ada", address.clone());
            assert_eq!(hit.action_address(), Some(address));

            let legacy = SearchResult::new("Contacts", "Ada", "contact:ada");
            assert_eq!(legacy.action_address(), None);
        }
        // phase: result_builders_populate_fields
        {
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
    }

    #[test]
    fn queries_fold_unicode_and_stop_at_the_input_ceiling() {
        // phase: bounded_query_normalization_keeps_unicode_case_matching
        {
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
        // phase: oversized_query_stops_before_index_work_or_materialisation
        {
            // Every record contains the whole query, so any index work would
            // find them; `inspected == 0` proves the rejection came first.
            let query = "x".repeat(MAX_SEARCH_QUERY_SCALARS + 1);
            let catalog = SearchCatalog::new("Documents");
            for index in 0..32 {
                let id = format!("document:{index:06}");
                catalog
                    .upsert(&id, format!("document {query}"), catalog_hit(&id, &id))
                    .unwrap();
            }

            let page = catalog.query_page(&query, None, 1, 1);
            assert!(page.results.is_empty());
            assert!(page.next.is_none());
            assert_eq!(page.inspected, 0, "input rejection must precede scans");
        }
    }

    fn catalog_hit(resource_id: &str, title: impl Into<String>) -> SearchResult {
        SearchResult::for_action("Documents", title, ActionAddress::new(resource_id, "open"))
    }

    #[test]
    fn the_incremental_catalogue_discovers_late_records_in_bounded_pages() {
        // phase: incremental_catalog_finds_unique_resource_beyond_one_hundred_thousand
        {
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
            assert_eq!(page.inspected, 1, "only the match is materialised");
            assert_eq!(catalog.stats().records, 100_001);
        }
        // phase: adversarial_exact_verification_can_continue_in_fixed_work_pages
        {
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
        // phase: catalogue_clamps_hostile_page_budgets_without_truncating_discovery
        {
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
    }

    #[test]
    fn the_incremental_catalogue_refuses_reuse_and_tracks_generations() {
        // phase: catalog_edits_and_deletes_only_change_incremental_generation
        {
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
        // phase: catalogue_rejects_legacy_or_mismatched_activation
        {
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
        // phase: catalogue_refuses_generation_reuse_without_partial_mutation
        {
            let catalog = SearchCatalog::new("Documents");
            catalog
                .upsert("document:a", "old phrase", catalog_hit("document:a", "A"))
                .unwrap();
            catalog.inner.write_recover().generation = u64::MAX;

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
        // phase: catalogue_refuses_browse_ordinal_reuse_without_inserting
        {
            let catalog = SearchCatalog::new("Documents");
            catalog.inner.write_recover().next_browse_ordinal = u64::MAX;

            assert_eq!(
                catalog.upsert("document:a", "phrase", catalog_hit("document:a", "A")),
                Err(SearchCatalogError::BrowseOrdinalExhausted)
            );
            assert_eq!(catalog.stats(), SearchCatalogStats::default());
            assert!(catalog.query_page("", None, 4, 4).results.is_empty());
        }
    }

    fn record(resource_id: &str, title: &str) -> SearchRecord {
        SearchRecord::new(resource_id, catalog_hit(resource_id, title))
            .field(title, FieldWeight::TITLE)
    }

    fn drain(catalog: &SearchCatalog, query: &str, budget: usize) -> (Vec<SearchResult>, usize) {
        let mut results = Vec::new();
        let mut inspected = 0;
        let mut cursor = None;
        loop {
            let page = catalog.query_page(query, cursor.as_ref(), budget, budget);
            assert!(page.inspected <= budget);
            inspected += page.inspected;
            results.extend(page.results);
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        (results, inspected)
    }

    #[test]
    fn weighted_fields_order_a_page_by_the_shared_ranker() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([
                SearchRecord::new("document:a", catalog_hit("document:a", "Travel notes"))
                    .field("Travel notes", FieldWeight::TITLE)
                    .field("an atlas of old roads", FieldWeight::BODY),
                record("document:b", "Atlas Press catalogue"),
                record("document:c", "Atlas"),
            ])
            .unwrap();
        let titles: Vec<_> = catalog
            .query_page("atlas", None, 8, 8)
            .results
            .into_iter()
            .map(|hit| hit.title)
            .collect();
        assert_eq!(titles, ["Atlas", "Atlas Press catalogue", "Travel notes"]);
        // Fields never match across their boundary.
        assert!(
            catalog
                .query_page("notes an", None, 8, 8)
                .results
                .is_empty()
        );
    }

    #[test]
    fn legacy_rows_keep_their_candidate_order() {
        let catalog = SearchCatalog::new("Documents");
        for (id, text) in [("document:1", "xx atlas"), ("document:2", "atlas")] {
            catalog.upsert(id, text, catalog_hit(id, text)).unwrap();
        }
        let ids: Vec<_> = catalog
            .query_page("atlas", None, 8, 8)
            .results
            .into_iter()
            .map(|hit| hit.action_address().unwrap().resource)
            .collect();
        assert_eq!(ids, ["document:1", "document:2"]);
    }

    #[test]
    fn folding_finds_accented_and_compatibility_spellings() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([
                record("document:a", "Crème brûlée"),
                record("document:b", "Ｔｏｋｙｏ"),
            ])
            .unwrap();
        assert_eq!(
            catalog.query_page("creme brulee", None, 8, 8).results.len(),
            1
        );
        assert_eq!(catalog.query_page("CRÈME", None, 8, 8).results.len(), 1);
        assert_eq!(catalog.query_page("tokyo", None, 8, 8).results.len(), 1);
    }

    #[test]
    fn cjk_titles_are_found_by_an_inner_word() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([
                record("document:a", "東京都の地図"),
                record("document:b", "大阪の地図"),
            ])
            .unwrap();
        let hits = catalog.query_page("東京", None, 8, 8).results;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "東京都の地図");
        assert_eq!(catalog.query_page("地図", None, 8, 8).results.len(), 2);
    }

    /// A body source that names the paragraph a match ends in.
    struct Paragraphs {
        resource: String,
        paragraphs: Vec<String>,
    }

    impl SearchBodySource for Paragraphs {
        fn segment_count(&self) -> usize {
            self.paragraphs.len()
        }
        fn piece_count(&self, segment: usize) -> usize {
            usize::from(segment < self.paragraphs.len())
        }
        fn piece(&self, segment: usize, piece: usize) -> Option<crate::search::BodyPiece<'_>> {
            (piece == 0).then(|| crate::search::BodyPiece {
                text: self.paragraphs.get(segment).map_or("", String::as_str),
                space_before: segment > 0,
            })
        }
        fn location(&self, segment: usize) -> Option<SearchResult> {
            let id = format!("{}#{segment}", self.resource);
            Some(SearchResult::for_action(
                "Places",
                format!("Paragraph {segment}"),
                ActionAddress::new(id, "open"),
            ))
        }
    }

    fn body_record(index: usize, paragraphs: Vec<String>) -> SearchRecord {
        let id = format!("document:{index:05}");
        let body = SearchBody::new(Arc::new(Paragraphs {
            resource: id.clone(),
            paragraphs,
        }));
        record(&id, &format!("Document {index}")).body(body)
    }

    #[test]
    fn a_late_body_only_hit_is_reachable_through_the_shared_helper() {
        let (catalog, hit) = crate::search::testing::assert_late_hit_reachable(
            |corpus| {
                let catalog = SearchCatalog::new("Documents");
                catalog
                    .replace_all_records(corpus.map(|row| {
                        body_record(
                            row.index,
                            vec!["An ordinary opening paragraph.".to_string(), row.text],
                        )
                    }))
                    .unwrap();
                catalog
            },
            3_000,
        );
        assert_eq!(hit.title, "Paragraph 1", "the hit names the paragraph");
        let stats = catalog.stats();
        assert_eq!(stats.body_records, 3_000);
    }

    #[test]
    fn body_hits_follow_field_hits_and_skip_records_already_found() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([
                record("document:title", "Harbour lights").body(SearchBody::new(Arc::new(
                    "harbour lights again".to_string(),
                ))),
                body_record(1, vec!["the harbour lights at dusk".to_string()]),
            ])
            .unwrap();
        let (hits, _) = drain(&catalog, "harbour lights", 1);
        let titles: Vec<_> = hits.iter().map(|hit| hit.title.as_str()).collect();
        // The plain body of the title hit does not report it twice; the
        // located body reports its paragraph.
        assert_eq!(titles, ["Harbour lights", "Paragraph 0"]);
    }

    #[test]
    fn a_match_late_in_a_huge_body_is_found() {
        let mut prose = "lorem ipsum ".repeat(50_000);
        prose.push_str("violet-tail");
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([body_record(0, vec![prose.clone()])])
            .unwrap();
        let (hits, inspected) = drain(&catalog, "violet-tail", 4);
        assert_eq!(hits.len(), 1);
        assert_eq!(inspected, 1);
    }

    #[test]
    fn body_words_out_of_order_do_not_match() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records(
                (0..40).map(|index| body_record(index, vec!["cat dog g ca".to_string()])),
            )
            .unwrap();
        let (hits, inspected) = drain(&catalog, "dog cat", 7);
        assert!(hits.is_empty());
        assert_eq!(inspected, 0);
        assert_eq!(drain(&catalog, "dog g ca", 7).0.len(), 40);
    }

    #[test]
    fn body_cursors_go_stale_with_the_catalogue() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records(
                (0..8).map(|index| body_record(index, vec!["shared words".to_string()])),
            )
            .unwrap();
        let first = catalog.query_page("shared", None, 2, 2);
        let cursor = first.next.expect("more bodies to verify");
        assert!(catalog.cursor_is_current(&cursor));
        catalog
            .upsert_record(record("document:new", "Other"))
            .unwrap();
        assert!(!catalog.cursor_is_current(&cursor));
        // A stale continuation restarts rather than skipping anything.
        let (hits, _) = drain(&catalog, "shared", 3);
        assert_eq!(hits.len(), 8);
    }

    #[test]
    fn unchanged_bodies_are_not_mutations() {
        let body = SearchBody::new(Arc::new("the long unchanged chapter".to_string()));
        let first = SearchCatalog::new("Documents");
        first
            .replace_all_records([record("document:a", "A").body(body.clone())])
            .unwrap();
        let rebuilt = SearchCatalog::new("Documents");
        rebuilt
            .replace_all_records([
                record("document:a", "A").body(body.clone()),
                record("document:b", "B"),
            ])
            .unwrap();
        // The first record browses first, whatever its ordinal.
        let reused = rebuilt
            .inner
            .read()
            .unwrap()
            .entries
            .values()
            .next()
            .unwrap()
            .body
            .clone()
            .unwrap();
        assert!(reused.shares_source_with(&body));
        assert_eq!(drain(&rebuilt, "unchanged", 8).0.len(), 1);
        // An identical upsert is not a mutation.
        let generation = rebuilt.stats().generation;
        rebuilt
            .upsert_record(record("document:a", "A").body(body))
            .unwrap();
        assert_eq!(rebuilt.stats().generation, generation);
    }

    #[test]
    fn detached_copies_extend_without_touching_the_original() {
        let catalog = SearchCatalog::new("Actions");
        catalog
            .replace_all_records([record("document:a", "Open")])
            .unwrap();
        let session = catalog.detached();
        let address = ActionAddress::new("document:a", "share");
        session
            .upsert_record(
                SearchRecord::new(
                    address.encode(),
                    SearchResult::for_action("Actions", "Share", address.clone()),
                )
                .field("Share", FieldWeight::TITLE),
            )
            .unwrap();
        assert!(session.contains(&address.encode()));
        assert_eq!(session.query_page("share", None, 8, 8).results.len(), 1);
        assert!(catalog.query_page("share", None, 8, 8).results.is_empty());
        assert_eq!(catalog.stats().records, 1);
    }

    #[test]
    fn a_query_that_is_a_term_elsewhere_still_finds_it_inside_words() {
        let catalog = SearchCatalog::new("Documents");
        for (id, text) in [
            ("document:1", "needle.rs"),
            ("document:2", "contains-needle-here"),
        ] {
            catalog.upsert(id, text, catalog_hit(id, text)).unwrap();
        }
        let page = catalog.query_page("needle", None, 8, 8);
        assert_eq!(
            page.results.len(),
            2,
            "`needle` is a word of one record and inside a word of the other"
        );
    }

    #[test]
    fn interior_query_terms_narrow_the_candidates() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records((0..500).map(|index| {
                let title = if index == 499 {
                    "alpha rare-token omega".to_string()
                } else {
                    format!("alpha rare-token-{index} omega")
                };
                record(&format!("document:{index:03}"), &title)
            }))
            .unwrap();
        let page = catalog.query_page("pha rare-token ome", None, 4, 4);
        assert_eq!(page.results.len(), 1);
        assert_eq!(
            page.inspected, 1,
            "the interior term's posting is the candidate list"
        );
    }

    /// A small deterministic generator, so a failure replays exactly.
    struct Steps(u64);

    impl Steps {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(self.0 >> 33).unwrap() % below.max(1)
        }
    }

    const SYNC_WORDS: [&str; 6] = ["harbour", "atlas", "granite", "lagoon", "fjord", "kestrel"];

    fn sync_record(id: usize, version: usize) -> SearchRecord {
        let title = format!(
            "{} {} {id}",
            SYNC_WORDS[id % SYNC_WORDS.len()],
            SYNC_WORDS[version % SYNC_WORDS.len()]
        );
        record(&format!("document:{id}"), &title)
    }

    fn answers(catalog: &SearchCatalog) -> Vec<(Vec<SearchResult>, bool)> {
        ["", "harbour", "atlas granite", "lagoon", "1", "fjord 2"]
            .into_iter()
            .map(|query| {
                let page = catalog.query_page(query, None, 256, 256);
                (page.results, page.next.is_some())
            })
            .collect()
    }

    #[test]
    fn syncing_a_snapshot_answers_as_a_bulk_replacement() {
        let mut steps = Steps(0x5ea2_c4ed);
        // (id, version) in snapshot order.
        let mut snapshot: Vec<(usize, usize)> = (0..200).map(|id| (id, 0)).collect();
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records(
                snapshot
                    .iter()
                    .map(|&(id, version)| sync_record(id, version)),
            )
            .unwrap();
        let mut next_id = 200;
        for step in 1..150 {
            match steps.next(5) {
                // New records at one place, and at the same place again and
                // again later, until its gap is crowded.
                0 => {
                    let at = if step % 3 == 0 {
                        50
                    } else {
                        steps.next(snapshot.len())
                    };
                    for _ in 0..=steps.next(4) {
                        snapshot.insert(at, (next_id, step));
                        next_id += 1;
                    }
                }
                1 => {
                    snapshot.remove(steps.next(snapshot.len()));
                }
                2 => {
                    let at = steps.next(snapshot.len());
                    snapshot[at].1 = step;
                }
                3 => {
                    let moved = snapshot.remove(steps.next(snapshot.len()));
                    snapshot.insert(steps.next(snapshot.len()), moved);
                }
                _ => {}
            }
            catalog
                .sync_records(
                    snapshot
                        .iter()
                        .map(|&(id, version)| sync_record(id, version)),
                )
                .unwrap();
            let bulk = SearchCatalog::new("Documents");
            bulk.replace_all_records(
                snapshot
                    .iter()
                    .map(|&(id, version)| sync_record(id, version)),
            )
            .unwrap();
            assert_eq!(catalog.stats().records, snapshot.len());
            assert_eq!(answers(&catalog), answers(&bulk), "step {step}");
        }
    }

    #[test]
    fn a_sync_indexes_only_what_changed_and_an_unchanged_one_nothing() {
        let catalog = SearchCatalog::new("Documents");
        let snapshot = |changed: Option<usize>| {
            (0..1_000).map(move |id| sync_record(id, usize::from(changed == Some(id))))
        };
        catalog.replace_all_records(snapshot(None)).unwrap();
        let generation = catalog.stats().generation;
        let touched =
            |records: Vec<SearchRecord>| catalog.inner.write().unwrap().sync(records).unwrap();
        assert_eq!(touched(snapshot(None).collect()), 0);
        assert_eq!(
            catalog.stats().generation,
            generation,
            "cursors stay current"
        );
        assert_eq!(touched(snapshot(Some(500)).collect()), 1);
        assert_eq!(catalog.stats().generation, generation + 1);
        let mut records: Vec<SearchRecord> = snapshot(Some(500)).collect();
        records.pop();
        records.insert(0, sync_record(5_000, 0));
        assert_eq!(touched(records), 2);
        let first = catalog.query_page("", None, 1, 1);
        assert_eq!(
            first.results[0].action_address().unwrap().resource,
            "document:5000",
            "the new first record browses first"
        );
    }

    #[test]
    fn a_refused_sync_changes_nothing() {
        let catalog = SearchCatalog::new("Documents");
        catalog
            .replace_all_records([record("document:a", "A"), record("document:b", "B")])
            .unwrap();
        let before = catalog.stats();
        assert_eq!(
            catalog.sync_records([record("document:a", "A"), record("document:a", "again")]),
            Err(SearchCatalogError::DuplicateResourceId)
        );
        assert_eq!(catalog.stats(), before);
        assert!(!catalog.query_page("b", None, 8, 8).results.is_empty());
    }
}
