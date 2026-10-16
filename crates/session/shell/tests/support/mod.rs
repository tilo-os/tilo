//! Shared support modules for shell integration tests, pulled into
//! individual test binaries via `mod support;`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

pub mod desktop;
pub mod host;
pub mod peer_stream;
pub mod snapshot;
pub mod wayland;

/// Bounds a wait that ends as soon as its condition holds; only a hang
/// reaches it, however loaded the test machine is.
pub const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
