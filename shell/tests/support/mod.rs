//! Shared support modules for shell integration tests.
//!
//! Pulled into individual test binaries via `mod support;`. Keeping
//! `offscreen.rs` here (rather than under `quick-ui/tests/support/`)
//! means the shell's e2e harness can live in `quick-shell/tests/` and
//! still reach a headless renderer, without depending on a helper that
//! the `quick-ui` crate doesn't export.

pub mod offscreen;
pub mod snapshot;
