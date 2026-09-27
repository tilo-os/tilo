//! Tilo's search layer: tantivy with Tilo's folding and ranking.
//!
//! Every surface that finds things by typed text (the palette, the
//! launcher, an app's own search) indexes its records in a
//! [`SearchIndex`] and asks it for ranked pages:
//!
//! - text is folded ([`fold`]) so case, diacritics and compatibility forms
//!   never hide a match;
//! - fields are found by substring and prefix as the person types, through
//!   gram phrases ([`GramTokenizer`]);
//! - hits rank by the best [`MatchTier`] × [`FieldWeight`] of their fields,
//!   and each page walks the ranks best first, reading only as many matches
//!   as it shows;
//! - body text follows every field match, ordered by BM25 over its
//!   ICU-segmented words ([`WordTokenizer`]);
//! - surfaces that search loosely remembered names opt into typo-tolerant
//!   `FuzzyTermQuery` and words spread over fields ([`Matching`]).
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

pub mod fold;
mod highlight;
mod index;
mod query;
mod tokenizer;
mod weight;

pub use fold::{fold, fold_char};
pub use highlight::locate;
pub use index::{BodyPiece, BodySource, Document, Hit, Hits, Page, SearchIndex};
pub use query::Matching;
pub use tokenizer::{GRAM_WIDTH, GramTokenizer, WordTokenizer};
pub use weight::{
    FUZZY_RANK, FieldWeight, INDEXED_WEIGHTS, MAX_FIELD_RANK, MatchTier, SPANNING_RANK,
};

#[cfg(test)]
mod tests;
