//! Chat — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A bespoke collaborative chat + calls surface tied to workspaces. The
//! convergent state — workspaces, channels, messages, reactions, edits +
//! edit history, threads, and FUSE-shared attachments — lives in an
//! Automerge document ([`doc`]); the [`app::ChatApp`] widget is a
//! projection of that document plus an input router. Conferencing is a
//! docked huddle / expandable overlay with a real `VideoPlayer` host tile;
//! live MoQ-over-iroh transport is gated behind a TODO (see
//! [`app`]'s `feed_demo_frame`).
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod app;
pub mod doc;
pub mod emoji;
pub mod events;
pub mod model;
pub mod paint;
pub mod render_call;
pub mod render_chat;

pub use app::ChatApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ChatApp::new())
}
