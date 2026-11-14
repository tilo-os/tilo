//! One test binary instead of one per file; #[path] keeps the files in place.

#[path = "perf_interaction.rs"]
mod perf_interaction;
#[path = "record_search.rs"]
mod record_search;
