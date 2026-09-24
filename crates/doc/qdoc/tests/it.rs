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
#[path = "blob_state.rs"]
mod blob_state;
#[path = "blobstore_fixture.rs"]
mod blobstore_fixture;
#[path = "codec_scaling.rs"]
mod codec_scaling;
#[path = "convergence.rs"]
mod convergence;
#[path = "docx_header_footer.rs"]
mod docx_header_footer;
#[path = "independent_documents.rs"]
mod independent_documents;
#[path = "office_entities.rs"]
mod office_entities;
#[path = "office_roundtrip.rs"]
mod office_roundtrip;
#[path = "sheet_roundtrip.rs"]
mod sheet_roundtrip;
