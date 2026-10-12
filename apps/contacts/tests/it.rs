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

#[path = "agent_api.rs"]
mod agent_api;
#[path = "association.rs"]
mod association;
#[path = "diag_focus.rs"]
mod diag_focus;
