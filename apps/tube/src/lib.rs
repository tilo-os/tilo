//! Tube — a privacy-respecting `YouTube` client for Tilo.
//!
//! No ads, no tracking, no Google player. Video search + metadata come from
//! public Invidious instances; `SponsorBlock` marks and auto-skips sponsor /
//! intro / outro segments; `DeArrow` swaps clickbait titles and thumbnails
//! for community-sourced factual ones. Everything is built on the CRDT-
//! native `ui` toolkit. Likes, the watch queue, subscriptions, and comment
//! likes and replies are the shared library, changed by actions; fetched
//! catalogues, navigation and playback stay with the window, and the watch
//! history and playback preferences follow the person to their own devices
//! without reaching collaborators.
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod action;
pub mod actions;
pub mod api;
pub mod app;
pub mod backend;
pub mod history;
pub mod i18n;
pub mod images;
pub mod model;
pub mod recommend;
pub mod resource;
mod rows;
pub mod sample;
pub mod screens;
pub mod search_type;
pub mod sync;
pub mod util;
pub mod widgets;

pub use app::TubeApp;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<TubeShell>()
}

pub use resource::{Library, TubeShell};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_app_starts_without_a_catalogue() {
        // An app without the network must not show the bundled sample
        // catalogue (invented channels, subscriber and view counts) as
        // though it were real.
        {
            let app = TubeApp::empty();
            let m = app.model_for_test();
            assert!(
                m.videos.is_empty(),
                "an empty app must not show {} invented videos",
                m.videos.len()
            );
            assert!(
                m.channels.is_empty(),
                "an empty app must not show {} invented channels",
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
