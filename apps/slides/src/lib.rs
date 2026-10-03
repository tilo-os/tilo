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
//! rail and a full-screen present mode. `app_root()` is the single
//! integration point used by both the live event loop and `--shots`.

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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod assets;
pub mod chrome;
pub mod doc;
pub mod framework_history;
mod help_overlay;
pub mod model;
pub mod odp;
pub mod outline;
mod overview_grid;
pub mod pptx;
mod presentation;
pub mod render;
pub mod resource;
pub mod search_provider;
pub mod strings;
mod toolbar_tools;

pub use app::{PresentSurface, SlidesApp};

use ui::widget::Widget;

/// Build and return the app's root widget tree for a live launch — opens on a
/// blank starter slide; the deck's real state comes from the shared CRDT
/// document.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SlidesApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.slides", app_root)
        .history(&framework_history::HISTORY)
        .component_actions(actions::register)
        .as_definition("Slides")
}

/// Root pre-loaded with the hermetic sample deck, optionally jumped to a named
/// screen (`overview` / `present` / `presenter` / `help` / `narrow` / …). Used
/// only by the `--shots` capture path so a live launch never shows fabricated
/// content.
pub fn app_root_demo(scenario: Option<&str>) -> Box<dyn Widget> {
    Box::new(SlidesApp::demo(scenario))
}

pub use actions::SlidesDeck;
pub use resource::SlidesShell;

#[cfg(test)]
mod deck_operations_tests;
#[cfg(test)]
mod framework_history_tests;
