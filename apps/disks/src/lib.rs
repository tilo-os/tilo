//! Disks — qOS native disk utility (CRDT-native, built on the `ui` toolkit).
//!
//! A drive list on the left, and a detail pane with a proportional
//! partition map, filesystem usage bars and a SMART health summary on
//! the right. Topology is parsed from `lsblk` + `smartctl` in the live
//! binary; `--shots` (and the instant-on seed) use a curated sample so
//! the screenshot is fully populated offline.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod actions;
pub mod app;
pub mod doc;
pub mod model;
pub mod ops;
pub mod probe;

pub use app::DisksApp;
pub use model::{Drive, DriveKind, Partition, SmartInfo, Topology};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(DisksApp::new())
}
