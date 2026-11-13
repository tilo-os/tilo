//! Hex: a hex editor whose bytes are the [`HexEditor`] record, so every edit,
//! whoever makes it, syncs, shows in history and is undone per author. The view is
//! the offset gutter, hex columns and ASCII column with one cursor across them,
//! plus a data inspector, find and go to offset.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod buffer;
mod file;
pub mod inspector;
pub mod model;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;

pub mod search;
pub mod view;

pub use model::HexModel;
pub use resource::{HexEditor, HexEditorShell};
pub use view::HexView;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(HexEditorShell)
}
