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

use tilo_automerge::heap;

/// Counts heap per thread, for the tests comparing what documents hold (see `heap`).
#[global_allocator]
static HEAP: heap::ThreadCounting = heap::ThreadCounting;

#[path = "binary_inputs.rs"]
mod binary_inputs;
#[path = "codec_scaling.rs"]
mod codec_scaling;
#[path = "convergence.rs"]
mod convergence;
#[path = "docx_header_footer.rs"]
mod docx_header_footer;
#[path = "import_memory.rs"]
mod import_memory;
#[path = "office_entities.rs"]
mod office_entities;
#[path = "office_fidelity.rs"]
mod office_fidelity;
#[path = "office_roundtrip.rs"]
mod office_roundtrip;
#[path = "sheet_roundtrip.rs"]
mod sheet_roundtrip;
#[path = "support/synth.rs"]
mod synth;
