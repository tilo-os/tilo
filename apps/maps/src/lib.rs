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
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer. The live app fetches real data
//! on background threads; `--shots` (no event loop, no network) renders the
//! bundled sample city, route and transit options so the screenshot is fully
//! populated offline.

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
pub mod rank;
pub mod resource;
pub mod sample;
mod search_box;
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

/// Build the root widget for headless `--shots` and tests: no tile worker, no
/// network. The stylised vector basemap stands in for raster imagery so the
/// screenshot is fully populated offline.
pub fn app_root_offline() -> Box<dyn Widget> {
    Box::new(MapsApp::offline())
}
pub use resource::{Maps, MapsShell};
