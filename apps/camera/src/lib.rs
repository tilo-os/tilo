//! Camera — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A real camera app: a live viewfinder (sample frames when headless), a
//! photo/video shutter, a camera selector, one recent-capture handoff, and a
//! QR / barcode scanner that decodes codes for real (via [`rqrr`]) and
//! offers contextual actions (open URL, copy, connect Wi-Fi, add contact).
//! Settings and capture metadata live in the Automerge CRDT.
//!
//! [`CameraShell`] is the app: the live window, `--shots` and every host
//! that mounts the camera ([`definition`]) run it through the framework.

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
pub mod video;
pub mod worker;

#[cfg(feature = "live-capture")]
pub mod capture;

pub use app::CameraApp;
pub use resource::{Camera, CameraShell, CameraState};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<CameraShell>()
}
