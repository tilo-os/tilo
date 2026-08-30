// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "association.rs"]
mod association;
#[path = "diag_focus.rs"]
mod diag_focus;
