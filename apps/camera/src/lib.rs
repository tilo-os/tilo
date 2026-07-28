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

pub mod app;
pub mod doc;
pub mod frame;
pub mod library;
pub mod model;
pub mod paint;
pub mod scan;
pub mod video;

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
