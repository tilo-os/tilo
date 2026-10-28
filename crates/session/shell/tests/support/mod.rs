//! Shared support modules for shell integration tests, pulled into
//! individual test binaries via `mod support;`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

pub mod desktop;
pub mod headless_wayland;
pub mod host;
pub mod peer_stream;
pub mod snapshot;
pub mod wayland;
