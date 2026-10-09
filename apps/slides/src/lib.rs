#![allow(
    clippy::many_single_char_names,
    clippy::redundant_clone,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Slides — a CRDT-native presentation app for Tilo, built on the `ui`
//! toolkit.
//!
//! Architecture mirrors the other native apps: a pure data [`model`], an
//! Automerge-backed [`doc`] layer, a shared slide [`render`]er, and a root
//! [`app::SlidesApp`] widget that owns the editor, inspector, thumbnail
//! rail and a full-screen present mode. [`SlidesShell`] is the app every
//! host launches, and its screenshot scenarios.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
pub mod app;
pub mod assets;
pub mod deck;
pub mod effects;
mod help_overlay;
pub mod model;
pub mod odp;
pub mod outline;
mod overview_grid;
pub mod pptx;
mod presentation;
pub mod render;
pub mod resource;
pub mod sample;
pub mod search;
pub mod strings;

pub use app::{PresentSurface, SlidesApp};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<SlidesShell>()
}

pub use deck::Deck;
pub use resource::SlidesShell;

#[cfg(test)]
mod deck_operations_tests;
