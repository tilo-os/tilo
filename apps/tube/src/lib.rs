//! Tube — a privacy-respecting YouTube client for qOS.
//!
//! No ads, no tracking, no Google player. Video search + metadata come from
//! public Invidious instances; SponsorBlock marks and auto-skips sponsor /
//! intro / outro segments; DeArrow swaps clickbait titles and thumbnails
//! for community-sourced factual ones. Everything is built on the CRDT-
//! native `ui` toolkit, so subscriptions, settings and watch history live
//! in the Automerge document and replicate / undo for free.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer. The whole UI is built
//! from the offline [`sample`] catalogue so a cold screenshot is fully
//! populated; live fetch (`api`) is gated behind a worker thread that only
//! spawns on real interaction.

pub mod action;
pub mod api;
pub mod app;
pub mod backend;
pub mod contrast;
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
