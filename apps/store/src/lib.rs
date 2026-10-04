//! Library root for the Tilo `store` — the app store, itself a Tilo app.
//!
//! The store surfaces two kinds of installable software side-by-side:
//!
//!   * **Tilo apps** listed by any registry the user has configured. Each
//!     entry carries an ed25519 publisher signature and a BLAKE3 root
//!     hash of the `.tapp`, so the client can verify identity + integrity
//!     before running `tilo-install`.
//!   * **Legacy Flatpak apps** fetched from Flathub's public JSON API.
//!     Actions cross the capability-scoped app-host service.
//!
//! The catalogue groups apps by the decision they invite (an update, already
//! on the device, new) in a scrolling grid of cards, and a details page opens
//! from a card. People search through the platform search, which the Store
//! feeds with its catalogue and with source, category and status facets.
//! Filters live in the window; moderation drafts live in the shared document
//! so they are snapshot-restorable (undo/redo) like the other native apps.
//!
//! Module map:
//!
//!   * [`model`]           — [`StoreItem`], `Source`, [`ActionKind`]
//!     and the filter enum shared across the app.
//!   * [`registry_client`] — HTTP client for the Tilo registry protocol.
//!   * [`flathub_client`]  — HTTP client for the Flathub JSON API.
//!   * `installed`       — compatibility parser for legacy install registries.
//!   * [`config`]          — read/write user config for configured registries.
//!   * [`sources`]         — merges the two feeds + installed state into
//!     one [`StoreItem`] stream.
//!   * [`actions`]         — dispatches Add / Install / Remove via app-host.
//!   * [`app`]             — the root `StoreApp` widget.
//!   * [`shots`]           — the fixture catalogue and the capture scenarios.
//!   * [`widgets`]         — the catalogue card and the app icon.

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
pub mod browse_actions;
pub mod config;
pub mod doc_actions;
pub mod flathub_client;
pub mod flatpakref;
pub mod images;
pub mod live;
pub mod model;
pub mod moderation_client;
pub mod registry_client;
pub mod resource;
pub mod search_provider;
pub mod shots;
pub mod sources;
pub mod widgets;
pub mod worker;

pub use app::StoreApp;
pub use flatpakref::FlatpakRef;
pub use model::{
    ActionKind, InstalledState, SourceFilter, StoreItem, StoreItemVersion, StoreSource, StoreTrust,
};

/// Build the standard worker-backed Store root used by the desktop and SDK
/// harnesses. The binary's deep-link route calls the same live host with its
/// additional launch context.
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    live::app_root()
}
pub use resource::{Store, StoreShell};
