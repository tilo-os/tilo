//! Weather — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A clean weather client: current conditions, a 24h temperature chart,
//! and a 7-day forecast for multiple saved locations, powered by the
//! keyless [Open-Meteo](https://open-meteo.com) API.
//!
//! ## Architecture
//! - [`model`] — pure data (locations, snapshots, units), no IO.
//! - [`wxcode`] — WMO weather-code → condition/icon family mapping.
//! - [`units`] — °C/°F + km/h/mph conversions.
//! - [`region`] — read-only consumer of the shared Settings store, from which
//!   the display units are derived (Settings owns the toggle, not this app).
//! - [`api`] — Open-Meteo URL construction + JSON parsing (pure/testable).
//! - [`net`] — the live background fetches (`tilo_net` on the app runtime).
//! - [`doc`] — the Automerge CRDT projection (saved places only; provider
//!   cache data is device-local).
//! - [`sample`] — bundled, fully-populated offline data.
//! - [`icon`] / [`chart`] — custom Vello vector drawing (weather glyphs,
//!   the temperature curve).
//! - [`interaction`] — direct-manipulation state for the saved-place list:
//!   drag-to-reorder, swipe-to-delete, and the right-click context menu,
//!   with swipe⇔menu action parity.
//! - [`core`] — the shared model + the two content panels (city-list overview
//!   and forecast detail) rendered into the shell's rects.
//! - [`panes`] — the thin overview/detail widgets the [`ui::widget::NavShell`]
//!   hosts.
//! - [`app`] — the root [`WeatherApp`] widget composing the responsive shell.
//!
//! ## `--shots`/tests vs. live
//! The offscreen screenshot path renders one frame with no event loop and no
//! async runtime, so it can't await a network fetch. [`WeatherApp::new`] is
//! therefore seeded with fully-populated [`sample`] snapshots and never
//! touches the network, so a screenshot is honest and offline.
//!
//! The live binary uses [`WeatherApp::live`]: it adopts the user's saved places
//! and fetches their real Open-Meteo observations. A blank record stays empty.
//! History shows recorded saved places through the shared domain painter;
//! provider observations and device units are not recorded versions.

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
pub mod api;
pub mod app;
pub mod chart;
pub mod core;
pub mod doc;
pub mod framework_history;
#[cfg(test)]
mod framework_history_tests;
pub mod icon;
pub mod interaction;
pub mod model;
pub mod net;
pub mod panes;
mod presentation;
pub mod region;
pub mod resource;
pub mod sample;
pub mod search_provider;
pub mod units;
pub mod wxcode;

pub use app::WeatherApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree. Single integration point
/// used by both the live event loop and the `--shots` offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(WeatherApp::live())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.weather", app_root)
        .component_actions(actions::register)
        .as_definition("Weather")
}

/// Like [`app_root`] but seeded with a named interaction scenario for a
/// headless screenshot (e.g. an in-progress drag, an open overlay flap).
/// See [`WeatherApp::seed_shot_scenario`]. An empty/unknown name yields the
/// same default frame as [`app_root`].
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = WeatherApp::new();
    app.seed_shot_scenario(scenario);
    Box::new(app)
}
pub use resource::{Weather, WeatherShell};
