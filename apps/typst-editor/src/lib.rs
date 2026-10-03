//! Typst — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A split-view Typst editor: a monospace, syntax-highlighted source pane on
//! the left, a live rendered preview of the compiled document on the right,
//! a clickable diagnostics ("Problems") panel, page thumbnails, zoom, and a
//! background compile worker. The document source lives in an Automerge `Text`
//! object so editing is collaborative and undoable for free.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.
//!
//! The source text is the app's *only* collaborative document state, so there is
//! nothing structural to migrate onto the resource + action model — see
//! [`actions`] for the finding and its action wiring.

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
pub mod compile;
pub mod diagnostics;
pub mod editor;
pub mod preview;
pub mod resource;
pub mod samples;
pub mod worker;

use ui::widget::Widget;

/// Build the live app root: it reads the user's real document library from
/// disk and opens an empty untitled document when the library is empty.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::TypstApp::new())
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.typst-editor", app_root)
        .history(&framework_history::HISTORY)
        .as_definition("Typst Editor")
}

/// Build a hermetic root seeded with the bundled demo documents. Used by the
/// offscreen `--shots` renderer and tests so captures are deterministic and do
/// not depend on whatever `.typ` files happen to be on disk.
pub fn demo_root() -> Box<dyn Widget> {
    Box::new(app::TypstApp::with_demo_documents())
}

/// Like [`demo_root`] but pre-configured into a named state so `--shots` can
/// capture views that would otherwise need interaction (compact preview, the
/// overflow menu, the Problems panel). Screenshot-only.
pub fn demo_root_scenario(scenario: &str) -> Box<dyn Widget> {
    let mut app = app::TypstApp::with_demo_documents();
    app.apply_demo_scenario(scenario);
    Box::new(app)
}

pub use resource::{TypstEditor, TypstEditorShell, launch_spec as framework_launch_spec};

pub mod framework_history;

#[cfg(test)]
mod framework_history_tests;
