//! Maps — qOS native app (CRDT-native, built on the `ui` toolkit).
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

pub mod actions;
pub mod app;
pub mod canvas;
pub mod doc;
pub mod draw;
pub mod geo;
pub mod livetest;
pub mod location;
pub mod model;
pub mod net;
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

/// Build the root widget for headless `--shots` and tests: no tile worker, no
/// network. The stylised vector basemap stands in for raster imagery so the
/// screenshot is fully populated offline.
pub fn app_root_offline() -> Box<dyn Widget> {
    Box::new(MapsApp::offline())
}
