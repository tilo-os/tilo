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

#[path = "e2e_docx.rs"]
mod e2e_docx;
#[path = "e2e_html.rs"]
mod e2e_html;
#[path = "e2e_odt.rs"]
mod e2e_odt;
#[path = "two_sessions_one_document.rs"]
mod two_sessions_one_document;
