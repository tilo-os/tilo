//! The records, ranking and matching of Tilo's search layer, and the seam
//! to its engine.
//!
//! `tilo-search` keeps an index's records and answers small indexes itself;
//! a larger one is indexed and searched by an [`engine::Engine`] (tantivy,
//! `tilo-search-tantivy`). Both read the records ([`record`]), cut text the
//! same way ([`fold`], [`tokenizer`]), match the same tiers and typos
//! ([`query`], [`typo`]) and rank by the same weights ([`weight`]), which
//! is what this crate holds. Apps use them through `tilo-search`, which
//! re-exports the public ones.

pub mod engine;
pub mod fold;
#[doc(hidden)]
pub mod query;
pub mod record;
#[doc(hidden)]
pub mod tokenizer;
#[doc(hidden)]
pub mod typo;
pub mod weight;
