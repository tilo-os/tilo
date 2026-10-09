//! Hex — a hex editor for Tilo, built on the `ui` toolkit.
//!
//! The open file's bytes are the [`HexEditor`] record's: every edit — a
//! keystroke, the palette, an agent, a script — is one of its actions, so
//! it syncs with the record, shows in history and is undone per author.
//! The UI is a classic hex view — offset gutter, hex byte columns, and an
//! ASCII column with a cursor synced across both — plus a data inspector
//! that interprets the bytes at the cursor as every common scalar type,
//! find (a hex pattern or text), and go to offset.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod buffer;
mod file;
pub mod inspector;
pub mod model;
pub mod resource;
pub mod sample;
pub mod search;
pub mod view;

pub use model::HexModel;
pub use resource::{HexEditor, HexEditorShell};
pub use view::HexView;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<HexEditorShell>()
}
