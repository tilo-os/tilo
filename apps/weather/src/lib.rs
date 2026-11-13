//! Weather: current conditions, a 24-hour strip and a 7-day forecast for the
//! saved places, from the keyless [Open-Meteo](https://open-meteo.com) API.

// An app runs as a sandboxed process, so a wrapped integer here costs one app,
// not the session. The lints that crash (indexing, string slices, unwrap, panic)
// stay denied: `shell`, `ui` and `crates/` keep the full set.
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
pub(crate) mod freshness;
pub mod i18n;
pub mod icon;
pub mod interaction;
pub mod locales;
pub mod model;
pub mod net;
pub mod panes;
mod presentation;
pub mod region;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;

pub mod units;
pub mod wxcode;

pub use app::WeatherApp;
pub use resource::{Weather, WeatherShell};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(WeatherShell)
}
