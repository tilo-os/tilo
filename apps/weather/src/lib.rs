//! Weather — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A clean weather client: current conditions, a 24h temperature chart,
//! and a 7-day forecast for multiple saved locations, powered by the
//! keyless [Open-Meteo](https://open-meteo.com) API.
//!
//! ## Architecture
//! - [`model`] — pure data (locations, snapshots, units), no IO.
//! - [`wxcode`] — WMO weather-code → condition/icon family mapping.
//! - [`units`] — °C/°F + km/h/mph preferences and conversions.
//! - [`api`] — Open-Meteo URL construction + JSON parsing (pure/testable).
//! - [`net`] — the live background fetch worker (reqwest + tokio).
//! - [`doc`] — the Automerge CRDT projection (saved places, units, the
//!   last-fetched snapshot per place).
//! - [`sample`] — bundled, fully-populated offline data.
//! - [`icon`] / [`chart`] — custom Vello vector drawing (weather glyphs,
//!   the temperature curve).
//! - [`sidebar`] — the adaptive flap (docked vs. overlay) state machine,
//!   resolved from the widget's allocated width.
//! - [`interaction`] — direct-manipulation state for the saved-place list:
//!   drag-to-reorder, swipe-to-delete, and the right-click context menu,
//!   with swipe⇔menu action parity.
//! - [`app`] — the root [`WeatherApp`] widget that composes it all.
//!
//! ## `--shots` vs. live
//! The offscreen screenshot path renders one frame with no event loop and
//! no async runtime, so it can't await a network fetch. [`WeatherApp`] is
//! therefore seeded with [`sample`] data and spawns its network worker
//! lazily on the first input event — which `--shots` never delivers. The
//! screenshot is thus fully populated and offline; the live binary fetches
//! real data and overwrites the sample snapshots in place.

pub mod api;
pub mod app;
pub mod chart;
pub mod doc;
pub mod icon;
pub mod interaction;
pub mod model;
pub mod net;
pub mod sample;
pub mod sidebar;
pub mod units;
pub mod wxcode;

pub use app::WeatherApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree. Single integration point
/// used by both the live event loop and the `--shots` offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(WeatherApp::new())
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
