//! Tube — a privacy-respecting YouTube client for qOS.
//!
//! No ads, no tracking, no Google player. Video search + metadata come from
//! public Invidious instances; SponsorBlock marks and auto-skips sponsor /
//! intro / outro segments; DeArrow swaps clickbait titles and thumbnails
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

pub mod action;
pub mod actions;
pub mod api;
pub mod app;
pub mod backend;
pub mod contrast;
pub mod images;
pub mod model;
pub mod playback;
pub mod recommend;
pub mod sample;
pub mod screens;
pub mod search_type;
pub mod sync;
pub mod util;
pub mod widgets;

pub use app::TubeApp;

use ui::widget::Widget;

/// The searchable result types Tube contributes to qOS's global search. An OS
/// search host registers these descriptors (id, display name, whether hits carry
/// a thumbnail) so a video or a channel shows up in the OS-wide palette and
/// activating it deep-links back into Tube's watch / channel page via
/// [`search_type::SearchHit`]. Exposed here as the single registration point so
/// the host's integration is a one-liner (`for t in tube::searchable_types() {
/// registry.register(t) }`) with no reach into the app internals.
pub fn searchable_types() -> Vec<search_type::SearchableType> {
    vec![
        search_type::video_searchable_type(),
        search_type::channel_searchable_type(),
    ]
}

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(TubeApp::new())
}

/// `--shots` variant for a NARROW viewport: the navigation rail is collapsed
/// to an overlay flap and forced open so the still frame captures the
/// summoned rail + dimming scrim. Pair with a sub-breakpoint `--viewport`
/// width. Identical to [`app_root`] apart from that one shots-only flag.
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
        assert!(ids.contains(&VIDEO_TYPE_ID), "video type registered: {ids:?}");
        assert!(ids.contains(&CHANNEL_TYPE_ID), "channel type registered: {ids:?}");
        // Both carry a thumbnail, so an OS palette reserves image space for them.
        assert!(types.iter().all(|t| t.has_thumbnail));
    }
}
