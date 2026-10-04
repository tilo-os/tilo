#![allow(
    clippy::manual_clamp,
    clippy::manual_midpoint,
    clippy::needless_pass_by_value,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Maps — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! An OpenStreetMap-style maps app with search (Nominatim), turn-by-turn
//! routing (OSRM) across car/bike/foot, and public-transport journey
//! planning (Transitous/MOTIS). The map itself is a custom Vello-drawn
//! canvas: a stylised vector basemap, a drawn route polyline, and place
//! markers, all coloured from the OS `Theme`.
//!
//! [`MapsShell`] runs it on the app framework, live and for `--shots`. The
//! live app fetches real data on background threads; a capture (no network)
//! stages the bundled sample city, route and transit options through the
//! framework's scenario hooks so every screenshot is fully populated offline.
//! Finding places is the platform search ([`place_search`]).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod canvas;
pub mod doc;
pub mod draw;
mod endpoint_editor;
pub mod geo;
mod map_controls;
mod map_info_sheet;
pub mod model;
pub mod net;
mod offline_panel;
pub mod place_search;
pub mod rank;
pub mod resource;
pub mod sample;
pub mod tiles;
pub mod traffic;

pub use app::MapsApp;
pub use model::{Category, LatLon, MapModel, Panel, Place, Route, TransitJourney, TravelMode};

use ui::widget::Widget;

/// Build and return the app's root widget tree (live: spawns the background
/// tile worker so OSM raster tiles fetch + decode off the UI thread).
pub fn app_root() -> Box<dyn Widget> {
    Box::new(MapsApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.maps", app_root)
        .component_actions(actions::register)
        .as_definition("Maps")
}

pub use resource::{Maps, MapsShell};
