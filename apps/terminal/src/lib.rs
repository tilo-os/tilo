//! Library target for `quick-terminal`. The binary in `main.rs` delegates
//! widget construction to this module so the nirios shell can embed a
//! `TerminalWidget` as an `AppWidget` inside a tile.
//!
//! See [`actions`] for the resource + action-model finding: the terminal's only
//! shared-document state is the emulator's live-screen projection of an external
//! process, so it exports no actions and publishes no Ctrl+K resource tree —
//! proven on the production path.

pub mod actions;

pub use crate::inner::{
    Attr, CellColor, GridPos, Selection, TermModes, TerminalBackend, TerminalPerformer,
    TerminalWidget, resolve_login_shell,
};

/// Re-exported so hosts can build a command for [`TerminalWidget::spawn_command`]
/// without taking a direct dependency on `portable-pty`.
pub use portable_pty::CommandBuilder;

// The existing `main.rs` content becomes `inner.rs` in Task 5. For this task
// we only need the re-export scaffolding. Until Task 5, we temporarily
// `include!` the binary source — replaced by a proper module split next.
mod inner {
    include!("main_body.rs");
}

mod app_widget_impl;

#[cfg(test)]
pub(crate) mod tests;
