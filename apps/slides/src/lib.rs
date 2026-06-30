//! Slides — a CRDT-native presentation app for qOS, built on the `ui`
//! toolkit.
//!
//! Architecture mirrors the other native apps: a pure data [`model`], an
//! Automerge-backed [`doc`] layer, a shared slide [`render`]er, and a root
//! [`app::SlidesApp`] widget that owns the editor, inspector, thumbnail
//! rail and a full-screen present mode. `app_root()` is the single
//! integration point used by both the live event loop and `--shots`.

pub mod app;
pub mod chrome;
pub mod doc;
pub mod model;
pub mod render;

pub use app::{PresentSurface, SlidesApp};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SlidesApp::new())
}
