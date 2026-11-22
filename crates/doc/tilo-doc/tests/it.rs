#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "blob_reference.rs"]
mod blob_reference;
#[path = "concurrent_block_text.rs"]
mod concurrent_block_text;
#[path = "independent_documents.rs"]
mod independent_documents;
#[path = "read_in_order.rs"]
mod read_in_order;
