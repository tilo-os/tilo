//! Camera — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A real camera app: a live viewfinder (sample frames when headless), a
//! photo/video shutter, a camera selector, one recent-capture handoff, and a
//! QR / barcode scanner that decodes codes for real (via [`rqrr`]) and
//! offers contextual actions (open URL, copy, connect Wi-Fi, add contact).
//! Settings and capture metadata live in the Automerge CRDT.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
pub mod doc;
pub mod document_scan;
mod files;
pub mod frame;
pub mod library;
pub mod media;
pub mod model;
pub mod paint;
pub mod resource;
pub mod scan;
pub mod search;
pub mod video;
pub mod worker;

#[cfg(feature = "live-capture")]
pub mod capture;

pub use app::CameraApp;

use ui::widget::Widget;

/// Build the live app: captures may be projected to disk and the viewfinder
/// needs a real camera.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CameraApp::disk())
}

/// The app as every host mounts it: the live camera and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.camera", app_root)
        .component_actions(actions::register)
        .as_definition("Camera")
}

/// Build the hermetic app used by `--shots` and integration harnesses:
/// self-contained sample content, no disk or device access.
pub fn shots_root() -> Box<dyn Widget> {
    Box::new(CameraApp::sample())
}
pub use resource::{Camera, CameraShell, CameraState, launch_spec, standalone_launch};
