//! Tilo's search layer: tantivy with Tilo's folding and ranking.
//!
//! The records, matching and ranking live in `tilo-search-model`, the engine
//! in `tilo-search-tantivy`; this crate is the index every surface uses.
//!
//! Every surface that finds things by typed text (the palette, the
//! launcher, an app's own search) indexes its records in a
//! [`SearchIndex`] and asks it for ranked pages:
//!
//! - text is folded ([`fold`]) so case, diacritics and compatibility forms
//!   never hide a match;
//! - fields are found by substring and prefix as the person types, through
//!   gram phrases (`GramTokenizer`);
//! - hits rank by the best [`MatchTier`] × [`FieldWeight`] of their fields,
//!   one `DisjunctionMaxQuery` of constant scores, collected by `TopDocs`;
//!   an index may rank tiers its own way ([`Ranks`]) and order hits by
//!   per-record fast fields ([`SearchIndex::search_scored`]);
//! - body text follows every field match, ordered by BM25 over its
//!   ICU-segmented words (`WordTokenizer`);
//! - surfaces that search loosely remembered names opt into typos in title
//!   and keyword words and words spread over fields ([`Matching`]);
//! - a small index without body text is answered by reading its records
//!   instead of through tantivy, ranked and ordered alike.
//!
//! ```
//! use tilo_search::{Document, FieldWeight, Page, SearchIndex};
//!
//! let mut index = SearchIndex::default();
//! index.upsert(Document::new(1).field("Atlas Press catalogue", FieldWeight::TITLE));
//! index.upsert(Document::new(2).field("Atlas", FieldWeight::TITLE));
//! index.commit().unwrap();
//!
//! let hits = index.search("atlas", Page::first(10)).hits;
//! assert_eq!(hits.iter().map(|hit| hit.key).collect::<Vec<_>>(), [2, 1]);
//! assert_eq!(hits[0].rank, Some(100));
//! ```

mod direct;
mod engine;
mod highlight;
mod index;
mod list;
mod usage;
mod words;

/// The engine's error, for callers opening a [`DiskIndex`].
#[cfg(not(feature = "lazy-engine"))]
pub use tilo_search_tantivy::TantivyError;
#[cfg(all(feature = "disk", not(feature = "lazy-engine")))]
pub use tilo_search_tantivy::{DiskHit, DiskIndex, DiskWriter};
/// The engine and its tokenizers, where it is linked: with `lazy-engine` the
/// search layer names no engine code (see [`prepare_engine`]).
#[cfg(not(feature = "lazy-engine"))]
pub use tilo_search_tantivy::{GramTokenizer, WordTokenizer, tantivy_engine};

pub use engine::{ENGINE_ENTRY, ENGINE_LIBRARY, engine_generation, engine_ready, prepare_engine};
#[cfg(feature = "lazy-engine")]
pub use engine::{defer_engine, engine_load_failed, install_engine};
pub use highlight::locate;
pub use index::SearchIndex;
pub use list::ListIndex;
pub use tilo_search_model::engine::{
    Engine, EngineEntry, IndexError, MakeEngine, Records, ScoreFn,
};
pub use tilo_search_model::fold;
pub use tilo_search_model::fold::{fold, fold_char};
pub use tilo_search_model::query::{MIN_INFIX_SCALARS, Matching, matches_inside_words};
pub use tilo_search_model::record::{BodyPiece, BodySource, Document, Hit, Hits, Page, Signals};
pub use tilo_search_model::tokenizer::GRAM_WIDTH;
pub use tilo_search_model::weight::{
    FUZZY_RANK, FieldWeight, INDEXED_WEIGHTS, KEYWORD_SLOT, MAX_FIELD_RANK, MatchTier, Ranks,
    SPANNING_RANK,
};
pub use usage::Frecency;
pub use words::initials;

#[cfg(test)]
mod tests;
