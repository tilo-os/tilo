// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "blob_reference.rs"]
mod blob_reference;
#[path = "concurrent_block_text.rs"]
mod concurrent_block_text;
#[path = "independent_documents.rs"]
mod independent_documents;
