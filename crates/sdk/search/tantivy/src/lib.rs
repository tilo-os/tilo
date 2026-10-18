//! The engine of Tilo's search layer: tantivy, with its postings in a RAM
//! directory, the tokenizers that index fields as marked grams and words,
//! and one compiled query per search that finds and ranks
//! ([`tantivy_engine`]); and [`DiskIndex`], a persistent keyed full-text
//! index for corpora too large for memory.
//!
//! A `SearchIndex` (`tilo-search`) reaches the engine only through a
//! `MakeEngine`, so a program can leave this crate out until someone
//! searches a large index: the shared native runtime opens it from
//! `libtilo_search_engine.so`, the browser desktop fetches it as a module.

#[cfg(feature = "disk")]
mod disk;
mod engine;
mod query;
mod tokenizer;

#[cfg(feature = "disk")]
pub use disk::{DiskHit, DiskIndex, DiskWriter};
pub use engine::tantivy_engine;
#[doc(hidden)]
pub use engine::{Fields, REBUILD_FRACTION, TantivyEngine, WRITER_HEAP_BYTES};
/// tantivy itself, for the search layer's tests that look inside an index.
#[doc(hidden)]
pub use tantivy;
/// The engine's error, for callers opening a [`DiskIndex`].
pub use tantivy::TantivyError;
pub use tokenizer::{GramTokenizer, WordTokenizer};
