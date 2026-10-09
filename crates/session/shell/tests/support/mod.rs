//! Shared support modules for shell integration tests, pulled into
//! individual test binaries via `mod support;`. `offscreen.rs` lives here
//! (rather than under `crates/sdk/ui-core/tests/support/`) so the shell's e2e harness can
//! reach a headless renderer without depending on a helper `quick-ui`
//! doesn't export.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

pub mod offscreen;
pub mod snapshot;
pub mod wayland;

/// Bounds a wait that ends as soon as its condition holds; only a hang
/// reaches it, however loaded the test machine is.
pub const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
