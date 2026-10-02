//! Tube — a privacy-respecting `YouTube` client for Tilo.
//!
//! No ads, no tracking, no Google player. Video search + metadata come from
//! public Invidious instances; `SponsorBlock` marks and auto-skips sponsor /
//! intro / outro segments; `DeArrow` swaps clickbait titles and thumbnails
//! for community-sourced factual ones. Everything is built on the CRDT-
//! native `ui` toolkit. Explicit subscriptions and playback settings are
//! collaborative actions; fetched catalogues, navigation, playback and watch
//! history stay device-local so a remote refresh never rewrites a collaborator's
//! screen or personal viewing trail.
//!
//! The live (online) app starts with an EMPTY catalogue and populates only
//! from live results (yt-dlp / rustypipe / trending). The bundled [`sample`]
//! exists solely for the hermetic `--shots`/demo review path (and tests), where
//! a cold screenshot needs to be fully populated. Live fetch (`api`) runs on a
//! worker thread
//! that spawns on launch (online) or on the first real interaction.

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

pub mod action;
pub mod actions;
pub mod api;
pub mod app;
pub mod backend;
pub mod contrast;
pub mod history_store;
pub mod images;
pub mod model;
pub mod recommend;
pub mod resource;
pub mod sample;
pub mod screens;
pub mod search_type;
pub mod sync;
pub mod util;
pub mod widgets;

pub use app::TubeApp;

use ui::widget::Widget;

/// Descriptors used by hosts that want type-specific search presentation in
/// addition to Tube's standard [`ui::search::SearchProvider`] integration.
pub fn searchable_types() -> Vec<search_type::SearchableType> {
    vec![
        search_type::video_searchable_type(),
        search_type::channel_searchable_type(),
        search_type::playlist_searchable_type(),
        search_type::history_searchable_type(),
    ]
}

/// The conventional entry point for a host embedding Tube. Starts with an
/// empty catalogue — the bundled `sample` data has to be asked for by name,
/// via [`TubeApp::new`], on a `--shots` path.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(TubeApp::empty())
}

/// The app as every host mounts it: the live root, which fetches the
/// catalogue where the network worker can run.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    fn live_root() -> Box<dyn Widget> {
        Box::new(TubeApp::new().online())
    }
    ui::launch::LaunchSpec::new("so.tilo.tube", live_root)
        .component_actions(actions::register)
        .as_definition("Tube")
}

/// `--shots` variant for a narrow viewport: the nav rail is forced open as an
/// overlay flap with its dimming scrim, and starts from [`TubeApp::new`] (the
/// sample catalogue) since a shot of an empty grid says nothing about the
/// rail. Pair with a sub-breakpoint `--viewport` width.
pub fn app_root_narrow_shot() -> Box<dyn Widget> {
    Box::new(TubeApp::new().with_open_sidebar_shot())
}

/// `--shots` variant that starts on a named [`model::Route`], so each screen
/// (home / trending / search / settings / …) can be captured for visual
/// self-inspection — in particular the selected-nav-item and selected-chip
/// contrast. Shots-only; the live app derives its route from sample/persisted
/// state.
pub fn app_root_route_shot(route: model::Route) -> Box<dyn Widget> {
    Box::new(TubeApp::new().with_route_shot(route))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_type::{CHANNEL_TYPE_ID, HISTORY_TYPE_ID, PLAYLIST_TYPE_ID, VIDEO_TYPE_ID};

    #[test]
    fn app_root_registers_search_types_and_starts_empty() {
        // Case `registers_all_tube_searchable_types`.
        {
            let types = searchable_types();
            let ids: Vec<&str> = types.iter().map(|t| t.id).collect();
            assert!(
                ids.contains(&VIDEO_TYPE_ID),
                "video type registered: {ids:?}"
            );
            assert!(
                ids.contains(&CHANNEL_TYPE_ID),
                "channel type registered: {ids:?}"
            );
            assert!(
                ids.contains(&PLAYLIST_TYPE_ID),
                "playlist type registered: {ids:?}"
            );
            assert!(
                ids.contains(&HISTORY_TYPE_ID),
                "history type registered: {ids:?}"
            );
            // Every result can carry a thumbnail, so an OS palette reserves image space.
            assert!(types.iter().all(|t| t.has_thumbnail));
        }
        // Case `app_root_starts_with_no_catalogue`:
        // A host embedding Tube the documented way must not be handed the
        // bundled sample catalogue (invented channels, subscriber and view
        // counts) as though it were real.
        {
            let app = TubeApp::empty();
            let m = app.model_for_test();
            assert!(
                m.videos.is_empty(),
                "app_root must not hand a host {} invented videos",
                m.videos.len()
            );
            assert!(
                m.channels.is_empty(),
                "app_root must not hand a host {} invented channels",
                m.channels.len()
            );
            assert!(
                !m.feed_loading,
                "with nothing fetching, Home must show its empty state rather \
             than a skeleton that never resolves"
            );
        }
    }
}
pub use resource::{Tube, TubeShared, TubeShell, launch_spec, standalone_launch};
