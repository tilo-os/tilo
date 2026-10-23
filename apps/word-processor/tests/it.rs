// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "e2e_docx.rs"]
mod e2e_docx;
#[path = "e2e_html.rs"]
mod e2e_html;
#[path = "e2e_odt.rs"]
mod e2e_odt;

mod canonical_component;
#[path = "concurrent_paragraph_edits.rs"]
mod concurrent_paragraph_edits;
#[path = "headless_launch.rs"]
mod headless_launch;
mod support;

#[path = "framework_host.rs"]
mod framework_host;
#[path = "framework_read.rs"]
mod framework_read;
mod window_documents;

#[path = "framework_undo.rs"]
mod framework_undo;

mod scroll_cost;

#[path = "writer_benchmark.rs"]
mod writer_benchmark;
