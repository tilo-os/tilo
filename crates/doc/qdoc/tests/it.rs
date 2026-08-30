// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "blob_reference.rs"]
mod blob_reference;
#[path = "blob_state.rs"]
mod blob_state;
#[path = "blobstore_fixture.rs"]
mod blobstore_fixture;
#[path = "convergence.rs"]
mod convergence;
#[path = "independent_documents.rs"]
mod independent_documents;
#[path = "office_roundtrip.rs"]
mod office_roundtrip;
#[path = "sheet_roundtrip.rs"]
mod sheet_roundtrip;
