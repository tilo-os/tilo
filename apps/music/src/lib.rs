//! Music — Tilo native streaming + library app, built on the CRDT-native
//! `ui` toolkit.
//!
//! Architecture:
//! - [`model`] — the collaborative [`Library`](model::Library) record (catalog,
//!   playlists, likes) plus local playback/cache values. It replicates across
//!   devices; audio transport/cache never do.
//! - [`seed`] — the bundled offline sample library (`--shots`, tests).
//! - [`backend`] — the swappable YouTube-Music data source
//!   ([`backend::MusicBackend`]): a hermetic offline sample impl and the
//!   shipping-default rustypipe impl (`live` feature).
//! - [`stream`] — the non-blocking worker driving [`backend`] off the UI thread.
//! - [`audio`] — `GStreamer` `playbin` output + live position query (`audio`
//!   feature).
//! - [`lyrics`] — LRC parsing + a bundled sample library, plus a feature-gated
//!   async lrclib fetch worker (`live` or `lyrics` feature).
//! - [`resource`] — the library's verbs as the framework resource and the
//!   app [`MusicShell`]: canonical shared library state, collaboration,
//!   history, peer replication, screenshot scenarios and the launch. Audio
//!   transport/cache stay device-local.
//! - [`storage`] — device-local playback profile and content-addressed cache.
//! - [`art`] / [`icon`] — procedural cover art + vector chrome glyphs.
//! - [`app`] — the [`MusicApp`] surface that draws the whole UI.

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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod audio;
pub mod backend;
pub mod commands;
mod files;
mod i18n;
mod library_migration;
pub mod local_file;
pub mod lyrics;
pub mod model;
pub mod mpris;
pub mod net;
pub mod recommendations;
pub mod resource;
pub mod search_type;
pub mod seed;
pub mod storage;
pub mod stream;
pub use app::{MusicApp, View};
pub use search_type::{MusicSearchProvider, SearchHit, SearchableType};

/// The searchable result types Music contributes to Tilo's global search. An OS
/// search host registers these descriptors (id, display name, whether hits
/// carry a cover) once, then drives one of the providers per query. Exposing
/// this list keeps the host's integration a one-liner
/// (`for t in music::searchable_types() { registry.register(t) }`) with no
/// reach into the app internals — the same seam `apps/tube` pins for videos.
pub fn searchable_types() -> Vec<SearchableType> {
    search_type::all_searchable_types()
}

pub use resource::MusicShell;

ui::dev_reload_entry!(MusicShell);

#[cfg(test)]
mod sync;
