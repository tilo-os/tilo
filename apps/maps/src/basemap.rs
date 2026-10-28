//! The vector basemap the map draws under its tiles (roads, areas, the
//! river), and the route drawn when no router answers.

use crate::geo::{haversine_m, polyline_length_m};
use crate::model::{LatLon, Maneuver, Route, RouteStep, TravelMode};

/// Road importance class — drives stroke width + colour ramp index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoadClass {
    Primary,
    Secondary,
    Residential,
}

#[derive(Clone, Debug)]
pub struct Road {
    pub class: RoadClass,
    pub pts: Vec<LatLon>,
}

/// Polygonal fill kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaKind {
    Water,
    Park,
    Building,
}

#[derive(Clone, Debug)]
pub struct Area {
    pub kind: AreaKind,
    pub ring: Vec<LatLon>,
}

/// A static vector basemap for the sample city.
#[derive(Clone, Debug, Default)]
pub struct Basemap {
    pub areas: Vec<Area>,
    pub roads: Vec<Road>,
    /// The river centre-line, drawn as a thick water stroke.
    pub river: Vec<LatLon>,
}

impl Basemap {
    /// True when there is no vector cartography to draw — the live app's
    /// first-run state, where a reference grid substrate stands in until real
    /// raster imagery arrives.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.roads.is_empty() && self.river.is_empty()
    }
}

/// Recompute a grid-following route for a different mode/destination using
/// the same shape but mode-dependent timing. Used by the live fallback when
/// the network is unavailable.
pub fn fallback_route(from: LatLon, to: LatLon, mode: TravelMode) -> Route {
    // Manhattan-ish path: go along lon to target lon, then along lat.
    let corner = LatLon::new(from.lat, to.lon);
    let geometry = vec![from, corner, to];
    let distance_m = polyline_length_m(&geometry);
    let duration_s = distance_m / (mode.speed_kmh() * 1000.0 / 3600.0);
    let bearing_east = to.lon >= from.lon;
    let steps = vec![
        RouteStep {
            instruction: format!(
                "Head {} to start",
                if bearing_east { "east" } else { "west" }
            ),
            distance_m: haversine_m(from, corner),
            maneuver: Maneuver::Depart,
        },
        RouteStep {
            instruction: format!(
                "Turn {} toward destination",
                if to.lat >= from.lat { "left" } else { "right" }
            ),
            distance_m: haversine_m(corner, to),
            maneuver: if to.lat >= from.lat {
                Maneuver::Left
            } else {
                Maneuver::Right
            },
        },
        RouteStep {
            instruction: "Arrive at destination".to_string(),
            distance_m: 0.0,
            maneuver: Maneuver::Arrive,
        },
    ];
    Route {
        mode,
        distance_m,
        duration_s,
        geometry,
        steps,
        from,
        to,
    }
}
