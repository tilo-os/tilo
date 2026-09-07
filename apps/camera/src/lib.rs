//! Camera — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A real camera app: a live viewfinder (sample frames when headless), a
//! photo/video shutter, a camera selector, an on-disk gallery (empty until
//! you capture), and a QR / barcode scanner that decodes codes for real
//! (via [`rqrr`]) and
//! offers contextual actions (open URL, copy, connect Wi-Fi, add contact).
//! Settings and gallery metadata live in the Automerge CRDT.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
pub mod doc;
pub mod frame;
pub mod library;
pub mod media;
pub mod model;
pub mod paint;
pub mod scan;
pub mod search;
pub mod video;
pub mod worker;

#[cfg(feature = "live-capture")]
pub mod capture;

pub use app::CameraApp;

use ui::widget::Widget;

/// Build the live app: the library is backed by real files on disk and the
/// viewfinder needs a real camera.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CameraApp::disk())
}

/// Build the hermetic app used by `--shots` and integration harnesses:
/// self-contained sample content, no disk or device access.
pub fn shots_root() -> Box<dyn Widget> {
    Box::new(CameraApp::sample())
}
