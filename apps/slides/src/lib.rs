//! Slides — a CRDT-native presentation app for qOS, built on the `ui`
//! toolkit.
//!
//! Architecture mirrors the other native apps: a pure data [`model`], an
//! Automerge-backed [`doc`] layer, a shared slide [`render`]er, and a root
//! [`app::SlidesApp`] widget that owns the editor, inspector, thumbnail
//! rail and a full-screen present mode. `app_root()` is the single
//! integration point used by both the live event loop and `--shots`.

pub mod actions;
pub mod app;
pub mod assets;
pub mod chrome;
pub mod doc;
pub mod model;
pub mod odp;
pub mod portal;
pub mod render;

pub use app::{PresentSurface, SlidesApp};

use ui::widget::Widget;

/// Build and return the app's root widget tree for a live launch — opens on a
/// blank starter slide; the deck's real state comes from the shared CRDT
/// document.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SlidesApp::new())
}

/// Root pre-loaded with the hermetic sample deck, optionally jumped to a named
/// screen (`overview` / `present` / `presenter` / `help` / `narrow` / …). Used
/// only by the `--shots` capture path so a live launch never shows fabricated
/// content.
pub fn app_root_demo(scenario: Option<&str>) -> Box<dyn Widget> {
    Box::new(SlidesApp::demo(scenario))
}
