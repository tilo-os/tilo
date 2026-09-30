//! The portable desktop surface shared by every host that can run the
//! portable `tilo-ui` build.
//!
//! [`surface::DesktopSurface`] presents a `tilo-desktop-model` tree with the
//! toolkit's own desktop widgets (`GroupRow`, `WindowGroup`, `TileHost`) and
//! folds gestures back into the tree through the model's shared topology
//! operations. [`navigation`] is the `DesktopNavigationV1` contract a host
//! page consumes. What a tab shows is the host's choice ([`surface::TileSource`]):
//! the browser mounts in-process application components; the native shell
//! keeps compositor surfaces and its own chrome.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod navigation;
pub mod surface;

pub use surface::{DesktopHandle, DesktopSurface, TileSource, published_selection, seed_tree};
