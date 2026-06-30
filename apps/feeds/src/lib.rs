//! Feeds — a CRDT-native RSS/Atom/Gemini reader for qOS.
//!
//! Architecture (mirrors the notes app):
//!   * [`model`]  — pure data + the in-memory [`model::Store`].
//!   * [`doc`]    — Automerge mapping; converging state lives here.
//!   * [`sample`] — bundled offline fixture used by `--shots` + first run.
//!   * [`fetch`]  — off-thread HTTP (reqwest + feed-rs) and gemini (TLS).
//!   * [`html`] / [`gemtext`] — body → renderable [`model::ContentBlock`]s.
//!   * [`opml`]   — subscription import/export.
//!   * [`app`]    — the three-pane root [`app::FeedsApp`] widget.
//!
//! `app_root()` is the single integration point: it returns the root widget
//! used by both the live `ui::App` event loop and the `--shots` offscreen
//! renderer.

pub mod app;
pub mod doc;
pub mod fetch;
pub mod gemtext;
pub mod html;
pub mod icons;
pub mod model;
pub mod opml;
pub mod sample;

pub use app::FeedsApp;
pub use model::{Article, Feed, FeedKind, Filter, Folder, Store};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(FeedsApp::new())
}
