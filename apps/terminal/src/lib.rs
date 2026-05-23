//! Library target for `quick-terminal`. The binary in `main.rs` delegates
//! widget construction to this module so the nirios shell can embed a
//! `TerminalWidget` as an `AppWidget` inside a tile.

pub use crate::inner::{Attr, TerminalBackend, TerminalPerformer, TerminalWidget};

// The existing `main.rs` content becomes `inner.rs` in Task 5. For this task
// we only need the re-export scaffolding. Until Task 5, we temporarily
// `include!` the binary source — replaced by a proper module split next.
mod inner {
    include!("main_body.rs");
}

mod app_widget_impl;

#[cfg(test)]
pub(crate) mod tests;
