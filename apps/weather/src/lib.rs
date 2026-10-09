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
//! - [`resource`] — the [`Weather`] resource and the [`WeatherShell`] app.
//! - [`sample`] — bundled, fully-populated offline data.
//! - [`icon`] / [`chart`] — content drawing (weather glyphs, the hourly
//!   strip), every value read from the theme.
//! - [`interaction`] — direct-manipulation state for the saved-place list:
//!   drag-to-reorder and swipe-to-delete, with swipe⇔menu action parity.
//! - [`core`] — the shared model + the two content panels (city-list overview
//!   and forecast detail) rendered into the shell's rects.
//! - [`panes`] — the thin overview/detail widgets the [`ui::widget::NavShell`]
//!   hosts.
//! - [`app`] — the root [`WeatherApp`] widget composing the window shell.
//!
//! ## Captures vs. live
//! A screenshot cannot await a network fetch, so a capture stages the bundled
//! [`sample`] places through the resource's actions and shows their bundled
//! forecasts, offline and deterministic.
//!
//! A live window shows the person's saved places and fetches their real
//! Open-Meteo observations; a blank record stays empty. History shows
//! recorded saved places; provider observations and device units are not
//! recorded versions.

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

pub mod actions;
pub mod api;
pub mod app;
pub mod chart;
pub mod core;
pub mod icon;
pub mod interaction;
pub mod locales;
pub mod model;
pub mod net;
pub mod panes;
mod presentation;
pub mod region;
pub mod resource;
pub mod sample;
pub mod units;
pub mod wxcode;

pub use app::WeatherApp;
pub use resource::{Weather, WeatherShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<WeatherShell>()
}
