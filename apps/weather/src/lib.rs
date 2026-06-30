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
pub mod model;
pub mod net;
pub mod sample;
pub mod units;
pub mod wxcode;

pub use app::WeatherApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree. Single integration point
/// used by both the live event loop and the `--shots` offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(WeatherApp::new())
}
