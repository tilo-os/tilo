//! Camera — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A real camera app: a live viewfinder (sample frames when headless), a
//! photo/video shutter, a camera selector, a populated gallery, and a
//! QR / barcode scanner that decodes codes for real (via [`rqrr`]) and
//! offers contextual actions (open URL, copy, connect Wi-Fi, add contact).
//! Settings and gallery metadata live in the Automerge CRDT.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod doc;
pub mod frame;
pub mod model;
pub mod paint;
pub mod scan;

#[cfg(feature = "live-capture")]
pub mod capture;

pub use app::CameraApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CameraApp::new())
}
