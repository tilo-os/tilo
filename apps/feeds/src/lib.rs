//! Feeds — a CRDT-native RSS/Atom/Gemini reader for Tilo.
//!
//! Architecture (mirrors the notes app):
//!   * [`model`]  — pure data + the in-memory [`model::Store`].
//!   * [`doc`]    — Automerge mapping; converging state lives here.
//!   * [`sample`] — bundled offline fixture used only by `--shots` and tests.
//!   * [`fetch`]  — HTTP (`tilo_net` + feed-rs) and gemini (TLS) on the app runtime.
//!   * [`html`] / [`gemtext`] — body → renderable [`model::ContentBlock`]s.
//!   * [`opml`]   — subscription import/export.
//!   * [`app`]    — the root [`app::FeedsApp`]: the window shell around the
//!     sidebar of places, the article list and the reader.
//!
//!   * [`resource`] — the resource, its effects and the framework app.
//!
//! [`FeedsShell`] is the single integration point: the live window, `--shots`
//! and every host that mounts the app run it through the framework.

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
pub mod app;
mod chrome;
pub mod doc;
pub mod fetch;
pub mod gemtext;
pub mod html;
mod list_pane;
pub mod modal;
pub mod model;
pub mod opml;
mod reader_pane;
pub mod reader_theme;
pub mod resource;
pub mod sample;
mod sidebar;

pub use app::FeedsApp;
pub use model::{Article, Feed, FeedKind, Filter, Folder, Store};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<FeedsShell>()
}

pub use resource::{Feeds, FeedsEffect, FeedsShell};
