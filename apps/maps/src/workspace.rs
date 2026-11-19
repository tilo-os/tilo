//! The shared map: `maps-model` keeps the record and its verbs; the app's own
//! tests drive it through the window.

pub use crate::model::{LatLon, MapModel, Place, Route, TransitJourney, TransitTime, TravelMode};
pub use maps_model::workspace::*;

#[cfg(test)]
mod tests;
