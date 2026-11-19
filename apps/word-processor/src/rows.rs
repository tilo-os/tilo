//! The rows Writer publishes for the document's Find: this window's results
//! and each hit in them, which shows its range in the document.

use ui::tilo_resource;

/// The Find results of this window, as one row of the document's tree.
#[derive(tilo_resource::Row)]
#[row(kind = "word_processor.search-results", label = "Search results")]
pub struct SearchResults;

/// One hit of the document's Find, which shows its range.
#[derive(tilo_resource::Row)]
#[row(
    kind = "word_processor.search-hit",
    label = "Search hit",
    effects = crate::effects::WordProcessorEffect
)]
pub struct SearchHit;
