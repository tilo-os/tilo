//! [`SearchIndex`]: a retained set of records, searchable through the
//! engine ([`crate::engine`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use crate::direct::{DIRECT_LIMIT, Direct};
use crate::engine::{self, Engine, IndexError, MakeEngine, Records, ScoreFn};
use crate::query::{Matching, Used};
use crate::typo::Typos;
use crate::weight::{FieldWeight, MatchTier, Ranks};
use tilo_lock::RwLockRecover as _;

/// The segment value of a record's own document.
pub(crate) const RECORD_SEGMENT: u64 = u64::MAX;

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
    pub(crate) key: u64,
    pub(crate) fields: Vec<(String, FieldWeight)>,
    pub(crate) body: Option<Arc<dyn BodySource>>,
    pub(crate) signals: (i32, u32),
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("key", &self.key)
            .field("fields", &self.fields)
            .field("body", &self.body.is_some())
            .field("signals", &self.signals)
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
            signals: (0, 0),
        }
    }

    /// Per-record numbers a custom score reads ([`Signals`]): a boost, and
    /// a length such as the name's.
    #[must_use]
    pub const fn signals(mut self, boost: i32, length: u32) -> Self {
        self.signals = (boost, length);
        self
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

    /// The record's [`Self::signals`]: its boost and length.
    pub(crate) const fn signals_pair(&self) -> (i32, u32) {
        self.signals
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
    /// The ordering score: the rank, or what a custom score made of it.
    pub score: i32,
}

/// What a custom score ([`SearchIndex::search_scored`]) sees of one hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signals {
    /// As [`Hit::rank`].
    pub rank: Option<i32>,
    /// The record's [`Document::signals`].
    pub boost: i32,
    pub length: u32,
}

/// One page of matches, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hits {
    pub hits: Vec<Hit>,
    /// Whether more matches follow this page.
    pub more: bool,
}

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

/// What a commit changes, and the engine that indexes it: the part of a
/// [`SearchIndex`] a search may still have to commit (see
/// [`SearchIndex::search_scored`]).
#[derive(Default)]
pub(crate) struct State {
    pub(crate) pending: BTreeSet<u64>,
    /// Pending keys the committed index has never held, so their records
    /// need no delete. Emptied by a failed commit, which may have applied
    /// some of its records: every pending key then gets its delete.
    pub(crate) fresh: BTreeSet<u64>,
    pub(crate) rebuild: bool,
    /// A commit was asked for before the engine's code was in: the first
    /// search after it arrives makes it.
    pub(crate) deferred: bool,
    /// Made by the first commit.
    pub(crate) engine: Option<Box<dyn Engine>>,
}

impl State {
    fn is_dirty(&self) -> bool {
        self.rebuild || !self.pending.is_empty()
    }
}

/// A retained, in-memory search index over [`Document`]s.
///
/// Records are kept by key; the engine (tantivy, with its postings in a
/// RAM directory) indexes them. Mutations are cheap and deferred:
/// [`commit`](Self::commit) applies them in one batch, as an incremental
/// commit or, when much changed, a bulk rebuild. Searches read the last
/// commit.
///
/// Every field is found by substring, folded (see [`fold()`](crate::fold())):
/// the query as a phrase of grams. The best field and tier rank a hit
/// (see [`MatchTier`](crate::MatchTier)); hits of equal rank come in key
/// order, and body hits follow by BM25.
///
/// A small index without body text is answered by reading its records
/// (`Direct`), ranked and ordered as the engine would.
///
/// Where the engine's code is fetched on demand (the `lazy-engine`
/// feature), such an index never needs it. A larger index's commit before
/// the engine is in waits, and the first search once it is in makes that
/// commit; searches before then find nothing.
pub struct SearchIndex {
    matching: Matching,
    ranks: Ranks,
    used: Used,
    /// The words of the records' titles and keywords, which typos are
    /// matched against; kept only when [`Matching::typos`] is on.
    typos: Typos,
    /// The committed records read directly, while they are few and have no
    /// body ([`Direct`]); searches then skip the engine.
    direct: Option<Direct>,
    /// Most records [`Self::direct`] holds; zero answers through the engine.
    direct_limit: usize,
    documents: BTreeMap<u64, Document>,
    state: RwLock<State>,
}

impl fmt::Debug for SearchIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.read();
        formatter
            .debug_struct("SearchIndex")
            .field("records", &self.documents.len())
            .field("pending", &state.pending.len())
            .field("rebuild", &state.rebuild)
            .field("engine", &state.engine.is_some())
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
        Self {
            matching,
            ranks: MatchTier::rank_in,
            used: Used::default(),
            typos: Typos::default(),
            direct: None,
            direct_limit: DIRECT_LIMIT,
            documents: BTreeMap::new(),
            state: RwLock::default(),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read_recover()
    }

    pub(crate) fn state_mut(&mut self) -> &mut State {
        self.state.get_mut().unwrap_or_else(PoisonError::into_inner)
    }

    /// An independent copy of the retained records, indexed on the next
    /// commit.
    #[must_use]
    pub fn detached(&self) -> Self {
        let mut copy = Self::new(self.matching).with_ranks(self.ranks);
        copy.direct_limit = self.direct_limit;
        copy.documents.clone_from(&self.documents);
        copy.used = self.used;
        copy.typos.clone_from(&self.typos);
        copy.state_mut().rebuild = true;
        copy
    }

    /// Rank tiers with `ranks` instead of [`MatchTier::rank_in`].
    #[must_use]
    pub fn with_ranks(mut self, ranks: Ranks) -> Self {
        self.ranks = ranks;
        self
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
        let held = self.documents.contains_key(&document.key);
        let state = self.state_mut();
        // Since the last commit every change is pending: a key neither held
        // nor pending was not committed either.
        if !held && !state.pending.contains(&document.key) {
            state.fresh.insert(document.key);
        }
        state.pending.insert(document.key);
        if self.matching.typos {
            self.typos.add(&document);
        }
        if let Some(replaced) = self.documents.insert(document.key, document)
            && self.matching.typos
        {
            self.typos.remove(&replaced);
        }
    }

    /// Remove a record. Returns whether it existed.
    pub fn remove(&mut self, key: u64) -> bool {
        let removed = self.documents.remove(&key);
        if let Some(removed) = &removed
            && self.matching.typos
        {
            self.typos.remove(removed);
        }
        let existed = removed.is_some();
        let state = self.state_mut();
        if existed && state.fresh.remove(&key) {
            // Never committed: nothing to delete.
            state.pending.remove(&key);
        } else if existed {
            state.pending.insert(key);
        }
        existed
    }

    /// Replace every record.
    pub fn replace_all(&mut self, documents: impl IntoIterator<Item = Document>) {
        self.documents.clear();
        self.used = Used::default();
        self.typos = Typos::default();
        for document in documents {
            self.note_used(&document);
            if self.matching.typos {
                self.typos.add(&document);
            }
            if let Some(replaced) = self.documents.insert(document.key, document)
                && self.matching.typos
            {
                self.typos.remove(&replaced);
            }
        }
        let state = self.state_mut();
        state.pending.clear();
        state.fresh.clear();
        state.rebuild = true;
    }

    /// Whether mutations wait for [`commit`](Self::commit).
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.read().is_dirty()
    }

    /// Make every mutation searchable.
    ///
    /// A few changes commit incrementally through one writer the index
    /// keeps from its first incremental commit on; much change rebuilds.
    /// A small index without body text is then answered from its records.
    /// Where the engine's code is still to come (the `lazy-engine`
    /// feature), such an index does not ask for it; a larger one's commit
    /// waits for the first search after the engine arrived.
    ///
    /// # Errors
    ///
    /// Returns the engine's error when indexing fails; the index then still
    /// answers from its last commit and the mutations stay pending.
    pub fn commit(&mut self) -> Result<(), IndexError> {
        if !self.is_dirty() {
            return Ok(());
        }
        let direct = !self.used.body && self.documents.len() <= self.direct_limit;
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        // A small index is answered from its records: it indexes into an
        // engine only where one is linked or already made, and never asks
        // for one.
        let make = if direct && state.engine.is_none() {
            engine::installed()
        } else {
            engine::make()
        };
        match make {
            Some(make) => commit(
                state,
                make,
                &self.documents,
                self.used,
                self.matching,
                self.ranks,
                &self.typos,
            )?,
            None if direct => {
                // The engine made later indexes every record anyway.
                state.pending.clear();
                state.fresh.clear();
                state.rebuild = false;
                state.deferred = false;
            }
            None => {
                state.deferred = true;
                self.direct = None;
                return Ok(());
            }
        }
        self.direct = direct.then(|| Direct::new(&self.documents, self.matching.typos));
        Ok(())
    }

    /// Answer every search through the engine, however few the records:
    /// what the direct path is tested against.
    #[cfg(test)]
    pub(crate) fn through_tantivy(mut self) -> Self {
        self.direct_limit = 0;
        self
    }

    /// One page of matches for `query`, from the last commit.
    ///
    /// Hits come best rank first, and in key order within a rank; body hits
    /// follow every field hit, by BM25. An empty query browses every record
    /// in key order.
    #[must_use]
    pub fn search(&self, query: &str, page: Page) -> Hits {
        self.search_scored(query, page, |signals| signals.rank.unwrap_or(0))
    }

    /// [`Self::search`], ordering field hits by `score` of their
    /// [`Signals`] instead of their rank. Body hits still follow them.
    #[must_use]
    pub fn search_scored(
        &self,
        query: &str,
        page: Page,
        score: impl Fn(Signals) -> i32 + Clone + Send + Sync + 'static,
    ) -> Hits {
        if let Some(direct) = &self.direct {
            return direct.search(self.matching, self.ranks, query, page, &score);
        }
        let score: ScoreFn = Arc::new(score);
        {
            let state = self.read();
            if !state.deferred {
                return self.search_in(&state, query, page, &score);
            }
        }
        // A commit waits for the engine's code.
        let Some(make) = engine::make() else {
            return Hits::default();
        };
        let mut state = self.state.write_recover();
        if state.deferred
            && let Err(error) = commit(
                &mut state,
                make,
                &self.documents,
                self.used,
                self.matching,
                self.ranks,
                &self.typos,
            )
        {
            tracing::error!(%error, "could not index search records");
        }
        let hits = self.search_in(&state, query, page, &score);
        drop(state);
        hits
    }

    fn search_in(&self, state: &State, query: &str, page: Page, score: &ScoreFn) -> Hits {
        let Some(engine) = &state.engine else {
            // Never committed: nothing to find.
            return Hits::default();
        };
        engine.search(self.records(state), query, page, score)
    }

    fn records<'a>(&'a self, state: &'a State) -> Records<'a> {
        Records {
            documents: &self.documents,
            pending: &state.pending,
            fresh: &state.fresh,
            rebuild: state.rebuild,
            used: self.used,
            matching: self.matching,
            ranks: self.ranks,
            typos: &self.typos,
        }
    }

    /// Bytes the committed index occupies.
    #[must_use]
    pub fn index_bytes(&self) -> u64 {
        self.read()
            .engine
            .as_ref()
            .map_or(0, |engine| engine.index_bytes())
    }
}

/// Commit `state`'s changes to `documents`, making its engine first.
fn commit(
    state: &mut State,
    make: MakeEngine,
    documents: &BTreeMap<u64, Document>,
    used: Used,
    matching: Matching,
    ranks: Ranks,
    typos: &Typos,
) -> Result<(), IndexError> {
    let State {
        pending,
        fresh,
        rebuild,
        deferred,
        engine,
    } = state;
    if engine.is_none() {
        // A new engine holds nothing: it indexes every record.
        *rebuild = true;
    }
    let engine = engine.get_or_insert_with(make);
    let committed = engine.commit(Records {
        documents,
        pending,
        fresh,
        rebuild: *rebuild,
        used,
        matching,
        ranks,
        typos,
    });
    fresh.clear();
    *deferred = false;
    committed?;
    pending.clear();
    *rebuild = false;
    Ok(())
}

#[cfg(test)]
#[path = "index_writer_tests.rs"]
mod writer_tests;
