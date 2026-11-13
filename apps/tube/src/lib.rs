//! Tube — a privacy-respecting `YouTube` client for Tilo. No ads, no tracking, no Google player.

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
pub mod captions;
pub mod history;
pub mod i18n;
pub mod images;
#[cfg(any(not(feature = "live"), test))]
pub mod innertube;
pub mod model;
pub mod recommend;
pub mod resource;
mod rows;
#[cfg(feature = "samples")]
pub mod sample;
pub mod screens;
pub mod search_type;
pub mod suggest;
pub mod sync;
pub mod util;
pub mod views;
pub mod widgets;

pub use app::TubeApp;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TubeShell)
}

pub use resource::{Library, TubeShell};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_app_starts_without_a_catalogue() {
        {
            let app = TubeApp::new();
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
