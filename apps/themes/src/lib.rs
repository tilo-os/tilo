//! Themes — Tilo native theme studio (CRDT-native, built on the `ui`
//! toolkit). Browse preset themes, customise the OS token set live, and
//! create / duplicate / export your own.
//!
//! `app_root()` is the single integration point used by both the live
//! winit event loop and the `--shots` offscreen renderer.

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
pub mod customizer;
pub mod doc;
pub mod draw;
pub mod gallery;
pub mod model;
pub mod preview;
pub mod resource;
pub mod store;
pub mod widgets;

pub use app::ThemesApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ThemesApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.themes", app_root)
        .history(&framework_history::HISTORY)
        .component_actions(actions::register)
        .as_definition("Theme Studio")
}

/// Build the root widget tree for a headless `--shots` capture at the given
/// viewport width. The single-frame renderer has no live first frame to settle
/// the responsive layout, so the layout class (docked vs. overlay) is seeded
/// from `width` here and the gallery flap is snapped to its endpoint. A narrow
/// width therefore captures the collapsed gallery floating over its scrim.
pub fn app_root_for_shot(width: u32) -> Box<dyn Widget> {
    // Optional scenario for the capture (sidebar menu, narrow scroll, font
    // change), selected via the `TILO_THEMES_SHOT` env var so a single binary
    // renders every review still. `live` captures the presets-only catalog;
    // every other scenario uses the seeded demo catalog.
    let scenario = std::env::var("TILO_THEMES_SHOT").unwrap_or_default();
    let mut app = if scenario == "live" {
        ThemesApp::new()
    } else {
        ThemesApp::new_demo()
    };
    app.seed_shot(f64::from(width), &scenario);
    Box::new(app)
}

#[cfg(test)]
mod framework_history_tests;

pub mod framework_history;
pub use resource::{Themes, ThemesShell};
