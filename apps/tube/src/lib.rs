//! Tube — a privacy-respecting `YouTube` client for qOS.
//!
//! No ads, no tracking, no Google player. Video search + metadata come from
//! public Invidious instances; `SponsorBlock` marks and auto-skips sponsor /
//! intro / outro segments; `DeArrow` swaps clickbait titles and thumbnails
//! for community-sourced factual ones. Everything is built on the CRDT-
//! native `ui` toolkit, so subscriptions, settings and watch history live
//! in the Automerge document and replicate / undo for free.
//!
//! The live (online) app starts with an EMPTY catalogue and populates only
//! from real results (yt-dlp / rustypipe / trending) — it never presents the
//! bundled [`sample`] as real data. That sample exists solely for the
//! hermetic `--shots`/demo review path (and tests), where a cold screenshot
//! needs to be fully populated. Live fetch (`api`) runs on a worker thread
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

pub mod action;
pub mod actions;
pub mod api;
pub mod app;
pub mod backend;
pub mod contrast;
pub mod images;
pub mod model;
pub mod recommend;
pub mod sample;
pub mod screens;
pub mod search_type;
pub mod sync;
pub mod util;
pub mod widgets;

pub use app::TubeApp;

use ui::widget::Widget;

/// Descriptors an OS search host registers so a video or channel shows up in
/// the global palette; activating a hit deep-links back into Tube's watch /
/// channel page via [`search_type::SearchHit`].
pub fn searchable_types() -> Vec<search_type::SearchableType> {
    vec![
        search_type::video_searchable_type(),
        search_type::channel_searchable_type(),
    ]
}

/// The conventional entry point for a host embedding Tube. Starts with an
/// empty catalogue — the bundled `sample` data has to be asked for by name,
/// via [`TubeApp::new`], on a `--shots` path.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(TubeApp::empty())
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
    use crate::search_type::{CHANNEL_TYPE_ID, VIDEO_TYPE_ID};

    #[test]
    fn registers_video_and_channel_searchable_types() {
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
        // Both carry a thumbnail, so an OS palette reserves image space for them.
        assert!(types.iter().all(|t| t.has_thumbnail));
    }

    /// A host embedding Tube the documented way must not be handed the
    /// bundled sample catalogue (invented channels, subscriber and view
    /// counts) as though it were real.
    #[test]
    fn app_root_starts_with_no_catalogue() {
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
