//! Maps' resources: the shared map — the places it knows, the searches it
//! made, how directions travel and the plans it accepted — and the areas this
//! device downloaded for offline use, as the app that draws them and an agent
//! that reads them both see them.
//!
//! The basemap, tiles, network and painting are the app's; only what a
//! workspace shares and what a device downloads travel here.

pub mod model;
pub mod offline_areas;
pub mod workspace;
