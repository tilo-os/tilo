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
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard; layout tests pin exact computed positions"
    )
)]

pub mod activity;
pub mod crashed_app;
pub mod drill_in;
pub mod history_log;
pub mod i18n;
pub mod navigation;
pub mod notifications;
pub mod person_cursor;
pub mod session;
mod shared_tree;
pub mod surface;

pub use surface::{
    ActivityModel, ActivityPaneCode, AppInbox, AppNotifier, DesktopHandle, DesktopNotifications,
    DesktopSurface, HistorySource, HostAction, HostActionSource, NotificationOutcome, OpenActivity,
    PersonalAsk, TileSource, is_host_offer, open_activity, published_selection, seed_tree,
    tile_app_kind,
};
