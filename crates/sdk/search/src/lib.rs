//! Tilo's search layer: tantivy with Tilo's folding and ranking.
//!
//! Every surface that finds things by typed text (the palette, the
//! launcher, an app's own search) indexes its records in a
//! [`SearchIndex`] and asks it for ranked pages:
//!
//! - text is folded ([`fold()`]) so case, diacritics and compatibility forms
//!   never hide a match;
//! - fields are found by substring and prefix as the person types, through
//!   gram phrases ([`GramTokenizer`]);
//! - hits rank by the best [`MatchTier`] × [`FieldWeight`] of their fields,
//!   one `DisjunctionMaxQuery` of constant scores, collected by `TopDocs`;
//!   an index may rank tiers its own way ([`Ranks`]) and order hits by
//!   per-record fast fields ([`SearchIndex::search_scored`]);
//! - body text follows every field match, ordered by BM25 over its
//!   ICU-segmented words ([`WordTokenizer`]);
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
#[cfg(feature = "disk")]
mod disk;
mod engine;
pub mod fold;
mod highlight;
mod index;
mod list;
mod query;
mod tokenizer;
mod typo;
mod usage;
mod weight;
mod words;

#[cfg(feature = "disk")]
pub use disk::{DiskHit, DiskIndex, DiskWriter};
pub use engine::{
    Engine, IndexError, MakeEngine, Records, ScoreFn, engine_generation, engine_ready,
    prepare_engine, tantivy_engine,
};
#[cfg(feature = "lazy-engine")]
pub use engine::{defer_engine, engine_load_failed, install_engine};
pub use fold::{fold, fold_char};
pub use highlight::locate;
pub use index::{BodyPiece, BodySource, Document, Hit, Hits, Page, SearchIndex, Signals};
pub use list::ListIndex;
pub use query::{MIN_ALIAS_SCALARS, MIN_INFIX_SCALARS, Matching, matches_inside_words};
/// The engine's error, for callers opening a [`DiskIndex`].
pub use tantivy::TantivyError;
pub use tokenizer::{GRAM_WIDTH, GramTokenizer, WordTokenizer};
pub use usage::Frecency;
pub use weight::{
    FUZZY_RANK, FieldWeight, INDEXED_WEIGHTS, KEYWORD_SLOT, MAX_FIELD_RANK, MatchTier, Ranks,
    SPANNING_RANK,
};
pub use words::initials;

#[cfg(test)]
mod tests;
