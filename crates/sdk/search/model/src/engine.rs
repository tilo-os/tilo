//! The seam between a `SearchIndex` (in `tilo-search`) and the engine that
//! indexes and searches its records.
//!
//! A `SearchIndex` keeps its records itself and asks an [`Engine`] to index
//! and search them. The engine (tantivy, `tilo-search-tantivy`) is reached
//! only through [`MakeEngine`], so a program can leave it out until someone
//! searches: the browser desktop fetches its code as a module of its own,
//! and the shared native runtime opens it from `libtilo_search_engine.so`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::query::{Matching, Used};
use crate::record::{Document, Hits, Page, Signals};
use crate::typo::Typos;
use crate::weight::Ranks;

/// Why indexing failed: the engine's error, as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexError(String);

impl fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for IndexError {}

impl IndexError {
    /// An engine's error, as its text.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A custom order for field hits
/// (`SearchIndex::search_scored`).
pub type ScoreFn = Arc<dyn Fn(Signals) -> i32 + Send + Sync>;

/// What an [`Engine`] reads of a `SearchIndex`: its
/// records, the changes since the last commit, and how it matches.
#[derive(Clone, Copy)]
pub struct Records<'a> {
    pub documents: &'a BTreeMap<u64, Document>,
    /// Keys changed since the last commit.
    pub pending: &'a BTreeSet<u64>,
    /// Pending keys the committed index has never held, so their records
    /// need no delete.
    pub fresh: &'a BTreeSet<u64>,
    /// Every record is to be indexed anew.
    pub rebuild: bool,
    pub used: Used,
    pub matching: Matching,
    pub ranks: Ranks,
    /// The words of the records' titles and keywords, for typos.
    pub typos: &'a Typos,
}

/// Indexes a `SearchIndex`'s records and searches
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

    /// The engine as itself, for tests that look inside one.
    #[doc(hidden)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Makes the engine for a new index.
pub type MakeEngine = fn() -> Box<dyn Engine>;

/// The entry an engine library exports: the engine.
pub type EngineEntry = fn() -> MakeEngine;
