//! Library root for the qOS `store` — the app store, itself a qOS app.
//!
//! The store surfaces two kinds of installable software side-by-side:
//!
//!   * **qOS apps** listed by any registry the user has configured. Each
//!     entry carries an ed25519 publisher signature and a BLAKE3 root
//!     hash of the `.qpkg`, so the client can verify identity + integrity
//!     before running `qos-install`.
//!   * **Legacy Flatpak apps** fetched from Flathub's public JSON API.
//!     Actions shell out to the host `flatpak` CLI.
//!
//! Filter chips (`All | qOS | Flatpak | Installed`), category chips and a
//! search box sit above a scrolling list of cards. A detail sheet slides
//! in from the right when a card is clicked. Selection, filters and the
//! search string all live in the CRDT so the UI is snapshot-restorable
//! (undo/redo) exactly like the other native apps.
//!
//! Module map:
//!
//!   * [`model`]           — [`StoreItem`], [`Source`], [`ActionKind`]
//!                           and the filter enum shared across the app.
//!   * [`registry_client`] — HTTP client for the qOS registry protocol.
//!   * [`flathub_client`]  — HTTP client for the Flathub JSON API.
//!   * [`installed`]       — parser for `$XDG_DATA_HOME/qos-apps/registry.toml`.
//!   * [`config`]          — read/write user config for configured registries.
//!   * [`sources`]         — merges the two feeds + installed state into
//!                           one [`StoreItem`] stream.
//!   * [`actions`]         — dispatches Add / Install / Remove via
//!                           `qos-install` / `flatpak` subprocesses.
//!   * [`app`]             — the root `StoreApp` widget.
//!   * [`shots`]           — headless `--shots` renderer.
//!   * [`widgets`]         — small ui-toolkit compositions shared across
//!                           the surface (cards, filter chips, etc).

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
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod actions;
pub mod app;
pub mod config;
pub mod flathub_client;
pub mod flatpakref;
pub mod installed;
pub mod model;
pub mod registry_client;
pub mod shots;
pub mod sources;
pub mod widgets;
pub mod worker;

pub use app::StoreApp;
pub use flatpakref::FlatpakRef;
pub use model::{
    ActionKind, InstalledState, SourceFilter, StoreItem, StoreItemVersion, StoreSource,
};
