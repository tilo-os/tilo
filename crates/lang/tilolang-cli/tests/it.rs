//! The command's integration tests as one binary: each `tests/*.rs` named here linked the whole
//! command, the toolkit and the UI host as a binary of its own. The files stay where they were, as
//! modules, and nextest runs each test in a process of its own, so nothing one test does reaches
//! another.

use std::path::Path;

mod build;
mod check;
mod command;
mod dev;
mod layout_check;
mod native_types;
mod repl;
mod support;

/// This package's directory.
fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}
