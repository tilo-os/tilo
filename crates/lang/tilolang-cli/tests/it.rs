//! The command's integration tests as one binary: each `tests/*.rs` named here linked the whole
//! command, the toolkit and the UI host as a binary of its own. The files stay where they were, as
//! modules, and nextest runs each test in a process of its own, so nothing one test does reaches
//! another.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::path::Path;

mod android_platform;
mod build;
mod check;
mod command;
mod dev;
mod json_output;
mod native_types;
mod outside_repo;
mod package_platform;
mod push;
mod repl;
mod support;

/// This package's directory.
fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}
