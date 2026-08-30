//! Shared support modules for shell integration tests, pulled into
//! individual test binaries via `mod support;`. `offscreen.rs` lives here
//! (rather than under `ui/tests/support/`) so the shell's e2e harness can
//! reach a headless renderer without depending on a helper `quick-ui`
//! doesn't export.

pub mod offscreen;
pub mod snapshot;
