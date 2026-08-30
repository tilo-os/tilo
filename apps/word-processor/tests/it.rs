// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "e2e_docx.rs"]
mod e2e_docx;
#[path = "e2e_html.rs"]
mod e2e_html;
#[path = "e2e_odt.rs"]
mod e2e_odt;
#[path = "two_sessions_one_document.rs"]
mod two_sessions_one_document;
