//! Bundled offline data so the app is fully populated without a network:
//! a procedurally-generated stylised vector basemap (a believable downtown
//! street grid with a river and parks), sample places, a sample driving
//! route with turn-by-turn steps, and sample public-transport itineraries.
//!
//! `--shots` has no event loop and no network, so this is what the headless
//! screenshot renders. The live app fetches real OSM/OSRM/transit data on
//! top of the same model.

use crate::geo::{haversine_m, polyline_length_m};
use crate::model::{
    Category, LatLon, MapModel, Maneuver, Panel, Place, Route, RouteStep, TransitJourney,
    TransitLeg, TransitMode, TravelMode,
};

/// Road importance class — drives stroke width + colour ramp index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoadClass {
    Primary,
    Secondary,
    Residential,
    Path,
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

/// Tiny deterministic PRNG so the generated map is identical every run
/// (stable screenshots).
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
}

/// Grid origin + spacing. `(nx, ny)` integer grid indices map to lat/lon.
const CENTER: LatLon = LatLon::new(48.1371, 11.5754);
const DLAT: f64 = 0.0024;
const DLON: f64 = 0.0036;
const NX: i32 = 16;
const NY: i32 = 13;

fn cell(nx: f64, ny: f64) -> LatLon {
    LatLon::new(CENTER.lat + ny * DLAT, CENTER.lon + nx * DLON)
}

/// Build the stylised basemap.
pub fn basemap() -> Basemap {
    let mut rng = Rng(0x51ED_2026_C0FF_EE01);
    let mut areas: Vec<Area> = Vec::new();
    let mut roads: Vec<Road> = Vec::new();

    // --- River: a wavy north-south centre-line, slightly east of centre.
    let mut river = Vec::new();
    let base_x = 8.5;
    for i in -NY..=NY {
        let t = i as f64;
        let wobble = (t * 0.55).sin() * 1.4 + (t * 0.21).cos() * 0.6;
        river.push(cell(base_x + wobble, t));
    }

    // --- Parks: a couple of irregular green polygons.
    areas.push(Area {
        kind: AreaKind::Park,
        ring: blob(&mut rng, cell(1.5, 5.5), 2.4, 2.0, 10),
    });
    areas.push(Area {
        kind: AreaKind::Park,
        ring: blob(&mut rng, cell(-6.0, -3.5), 1.6, 1.4, 9),
    });
    areas.push(Area {
        kind: AreaKind::Park,
        ring: blob(&mut rng, cell(5.5, -6.5), 1.3, 1.1, 8),
    });

    // --- A small lake inside the first park.
    areas.push(Area {
        kind: AreaKind::Water,
        ring: blob(&mut rng, cell(1.8, 6.2), 0.7, 0.5, 9),
    });

    // Helper: is a grid cell occupied by a park/water (skip buildings there)?
    let park_centers = [
        (1.5_f64, 5.5_f64, 2.6_f64),
        (-6.0, -3.5, 1.8),
        (5.5, -6.5, 1.5),
    ];
    let near_park = |nx: f64, ny: f64| {
        park_centers
            .iter()
            .any(|&(px, py, r)| ((nx - px).powi(2) + (ny - py).powi(2)).sqrt() < r)
    };
    let near_river = |nx: f64, ny: f64| (nx - (base_x + (ny * 0.55).sin() * 1.4)).abs() < 1.1;

    // --- Building fills: a subset of grid cells get a raised block.
    for ix in -NX..NX {
        for iy in -NY..NY {
            let cx = ix as f64 + 0.5;
            let cy = iy as f64 + 0.5;
            if near_park(cx, cy) || near_river(cx, cy) {
                continue;
            }
            if rng.f() > 0.62 {
                continue;
            }
            let inset = rng.range(0.16, 0.30);
            let jx = rng.range(-0.05, 0.05);
            let jy = rng.range(-0.05, 0.05);
            let x0 = ix as f64 + inset + jx;
            let x1 = (ix as f64 + 1.0) - inset + jx;
            let y0 = iy as f64 + inset + jy;
            let y1 = (iy as f64 + 1.0) - inset + jy;
            areas.push(Area {
                kind: AreaKind::Building,
                ring: vec![
                    cell(x0, y0),
                    cell(x1, y0),
                    cell(x1, y1),
                    cell(x0, y1),
                ],
            });
        }
    }

    // --- Street grid: residential roads on every grid line.
    for ix in -NX..=NX {
        let x = ix as f64;
        let mut pts = Vec::new();
        for iy in -NY..=NY {
            let j = rng.range(-0.04, 0.04);
            pts.push(cell(x + j, iy as f64));
        }
        roads.push(Road {
            class: RoadClass::Residential,
            pts,
        });
    }
    for iy in -NY..=NY {
        let y = iy as f64;
        let mut pts = Vec::new();
        for ix in -NX..=NX {
            let j = rng.range(-0.04, 0.04);
            pts.push(cell(ix as f64, y + j));
        }
        roads.push(Road {
            class: RoadClass::Residential,
            pts,
        });
    }

    // --- A few primary avenues (thicker) crossing the grid.
    roads.push(Road {
        class: RoadClass::Primary,
        pts: (-NX..=NX).map(|ix| cell(ix as f64, 0.0)).collect(),
    });
    roads.push(Road {
        class: RoadClass::Primary,
        pts: (-NY..=NY).map(|iy| cell(0.0, iy as f64)).collect(),
    });
    // A diagonal secondary boulevard.
    roads.push(Road {
        class: RoadClass::Secondary,
        pts: (-NY..=NY).map(|iy| cell(iy as f64 * 0.8 - 3.0, iy as f64)).collect(),
    });
    // A ring-ish secondary.
    roads.push(Road {
        class: RoadClass::Secondary,
        pts: vec![
            cell(-10.0, -8.0),
            cell(-2.0, -10.0),
            cell(6.0, -9.0),
            cell(11.0, -3.0),
            cell(11.0, 6.0),
            cell(4.0, 10.0),
            cell(-5.0, 10.0),
            cell(-11.0, 4.0),
            cell(-11.0, -3.0),
            cell(-10.0, -8.0),
        ],
    });

    Basemap {
        areas,
        roads,
        river,
    }
}

/// An irregular closed polygon ("blob") around `c`, radii in grid units.
fn blob(rng: &mut Rng, c: LatLon, rx: f64, ry: f64, n: usize) -> Vec<LatLon> {
    let mut ring = Vec::with_capacity(n);
    for i in 0..n {
        let a = (i as f64) / (n as f64) * std::f64::consts::TAU;
        let jitter = rng.range(0.78, 1.18);
        let dx = a.cos() * rx * jitter;
        let dy = a.sin() * ry * jitter;
        ring.push(LatLon::new(c.lat + dy * DLAT, c.lon + dx * DLON));
    }
    ring
}

/// The bundled places (POIs).
pub fn places() -> Vec<Place> {
    vec![
        Place::new(
            "marienplatz",
            "Marienplatz",
            Category::Landmark,
            "Marienplatz 1, 80331 München",
            cell(0.0, 0.0),
        )
        .with_rating(4.7),
        Place::new(
            "cafe-glueck",
            "Café Glück",
            Category::Cafe,
            "Theatinerstraße 18",
            cell(2.6, 2.1),
        )
        .with_rating(4.5),
        Place::new(
            "stadtmuseum",
            "Stadtmuseum",
            Category::Museum,
            "St.-Jakobs-Platz 1",
            cell(-2.1, -1.2),
        )
        .with_rating(4.3),
        Place::new(
            "isar-biergarten",
            "Isar Biergarten",
            Category::Bar,
            "Am Kanal 4",
            cell(7.4, -2.6),
        )
        .with_rating(4.6),
        Place::new(
            "hofgarten",
            "Hofgarten",
            Category::Park,
            "Hofgartenstraße",
            cell(1.5, 5.4),
        )
        .with_rating(4.8),
        Place::new(
            "hauptbahnhof",
            "Hauptbahnhof",
            Category::Transit,
            "Bahnhofplatz 2",
            cell(-9.0, 1.1),
        )
        .with_rating(4.0),
        Place::new(
            "hotel-vier",
            "Hotel Vier Türme",
            Category::Hotel,
            "Sendlinger Str. 44",
            cell(3.8, -4.2),
        )
        .with_rating(4.4),
        Place::new(
            "viktualienmarkt",
            "Viktualienmarkt",
            Category::Shop,
            "Viktualienmarkt 3",
            cell(0.3, -2.1),
        )
        .with_rating(4.7),
    ]
}

/// Origin point for directions ("your location").
pub fn origin() -> LatLon {
    cell(-6.5, -6.0)
}

/// A sample driving route from the origin to Marienplatz that follows the
/// street grid (east, then north), with believable steps.
pub fn sample_route() -> Route {
    let o = origin();
    // East along the ny ≈ -6 row to nx = 0, then north along nx = 0 col.
    let mut geometry = Vec::new();
    let row_y = -6.0;
    let mut nx = -6.5;
    while nx <= 0.0 {
        geometry.push(cell(nx, row_y));
        nx += 1.0;
    }
    geometry.push(cell(0.0, row_y));
    let mut ny = row_y;
    while ny <= 0.0 {
        geometry.push(cell(0.0, ny));
        ny += 1.0;
    }
    geometry.push(cell(0.0, 0.0));

    let leg_east = polyline_length_m(&geometry[..7.min(geometry.len())]);
    let leg_north = polyline_length_m(&geometry[6.min(geometry.len().saturating_sub(1))..]);
    let total = polyline_length_m(&geometry);
    let _ = (o, leg_east, leg_north);

    let steps = vec![
        RouteStep {
            instruction: "Head east on Lindwurmstraße".to_string(),
            distance_m: 240.0,
            maneuver: Maneuver::Depart,
        },
        RouteStep {
            instruction: "Continue onto Pettenkoferstraße".to_string(),
            distance_m: 1320.0,
            maneuver: Maneuver::Continue,
        },
        RouteStep {
            instruction: "Turn left onto Sendlinger Straße".to_string(),
            distance_m: 80.0,
            maneuver: Maneuver::Left,
        },
        RouteStep {
            instruction: "Continue north toward the Altstadt".to_string(),
            distance_m: 1180.0,
            maneuver: Maneuver::Continue,
        },
        RouteStep {
            instruction: "Slight right into the pedestrian zone".to_string(),
            distance_m: 90.0,
            maneuver: Maneuver::SlightRight,
        },
        RouteStep {
            instruction: "Arrive at Marienplatz".to_string(),
            distance_m: 0.0,
            maneuver: Maneuver::Arrive,
        },
    ];

    let mode = TravelMode::Car;
    let duration_s = total / (mode.speed_kmh() * 1000.0 / 3600.0);
    Route {
        mode,
        distance_m: total,
        duration_s,
        geometry,
        steps,
        from: o,
        to: cell(0.0, 0.0),
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
            instruction: format!("Head {} to start", if bearing_east { "east" } else { "west" }),
            distance_m: haversine_m(from, corner),
            maneuver: Maneuver::Depart,
        },
        RouteStep {
            instruction: format!("Turn {} toward destination", if to.lat >= from.lat { "left" } else { "right" }),
            distance_m: haversine_m(corner, to),
            maneuver: if to.lat >= from.lat { Maneuver::Left } else { Maneuver::Right },
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

/// Sample public-transport itineraries for the Transit panel.
pub fn sample_transit() -> Vec<TransitJourney> {
    let walk = |from: &str, to: &str, dep: &str, arr: &str, min: u32| TransitLeg {
        mode: TransitMode::Walk,
        line: String::new(),
        headsign: String::new(),
        from_name: from.to_string(),
        to_name: to.to_string(),
        dep: dep.to_string(),
        arr: arr.to_string(),
        duration_min: min,
        num_stops: 0,
        geometry: vec![],
    };

    vec![
        TransitJourney {
            dep: "09:12".to_string(),
            arr: "09:26".to_string(),
            duration_min: 14,
            transfers: 0,
            legs: vec![
                walk("Your location", "Goetheplatz", "09:12", "09:16", 4),
                TransitLeg {
                    mode: TransitMode::Subway,
                    line: "U3".to_string(),
                    headsign: "→ Moosach".to_string(),
                    from_name: "Goetheplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: "09:17".to_string(),
                    arr: "09:23".to_string(),
                    duration_min: 6,
                    num_stops: 3,
                    geometry: vec![],
                },
                walk("Marienplatz", "Marienplatz", "09:24", "09:26", 2),
            ],
        },
        TransitJourney {
            dep: "09:08".to_string(),
            arr: "09:27".to_string(),
            duration_min: 19,
            transfers: 1,
            legs: vec![
                walk("Your location", "Sendlinger Tor", "09:08", "09:11", 3),
                TransitLeg {
                    mode: TransitMode::Tram,
                    line: "17".to_string(),
                    headsign: "→ Schwanseestraße".to_string(),
                    from_name: "Sendlinger Tor".to_string(),
                    to_name: "Reichenbachplatz".to_string(),
                    dep: "09:12".to_string(),
                    arr: "09:16".to_string(),
                    duration_min: 4,
                    num_stops: 2,
                    geometry: vec![],
                },
                TransitLeg {
                    mode: TransitMode::Bus,
                    line: "52".to_string(),
                    headsign: "→ Tierpark".to_string(),
                    from_name: "Reichenbachplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: "09:19".to_string(),
                    arr: "09:25".to_string(),
                    duration_min: 6,
                    num_stops: 3,
                    geometry: vec![],
                },
                walk("Marienplatz", "Marienplatz", "09:25", "09:27", 2),
            ],
        },
        TransitJourney {
            dep: "09:05".to_string(),
            arr: "09:29".to_string(),
            duration_min: 24,
            transfers: 1,
            legs: vec![
                walk("Your location", "Poccistraße", "09:05", "09:10", 5),
                TransitLeg {
                    mode: TransitMode::Subway,
                    line: "U3/U6".to_string(),
                    headsign: "→ Garching".to_string(),
                    from_name: "Poccistraße".to_string(),
                    to_name: "Odeonsplatz".to_string(),
                    dep: "09:13".to_string(),
                    arr: "09:21".to_string(),
                    duration_min: 8,
                    num_stops: 4,
                    geometry: vec![],
                },
                TransitLeg {
                    mode: TransitMode::Rail,
                    line: "S1".to_string(),
                    headsign: "→ Ostbahnhof".to_string(),
                    from_name: "Odeonsplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: "09:24".to_string(),
                    arr: "09:27".to_string(),
                    duration_min: 3,
                    num_stops: 1,
                    geometry: vec![],
                },
                walk("Marienplatz", "Marienplatz", "09:27", "09:29", 2),
            ],
        },
    ]
}

/// Assemble the fully-populated default model used by `--shots` and as the
/// initial state of a fresh live session.
pub fn default_model() -> MapModel {
    let mut m = MapModel::default();
    m.places = places();
    // Pre-save a couple of favourites so the Explore list is populated.
    for id in ["hofgarten", "cafe-glueck"] {
        m.toggle_saved(id);
    }
    m.recents = vec![
        "Marienplatz".to_string(),
        "coffee near me".to_string(),
        "Hauptbahnhof".to_string(),
    ];
    m.results = m
        .places
        .iter()
        .filter(|p| {
            matches!(
                p.category,
                Category::Landmark | Category::Cafe | Category::Museum | Category::Shop
            )
        })
        .cloned()
        .collect();
    m.selected = Some("marienplatz".to_string());
    m.origin = Some(origin());
    m.origin_name = "Sendling, München".to_string();
    m.route = Some(sample_route());
    m.transit = sample_transit();
    m.panel = Panel::Directions;
    m.mode = TravelMode::Car;
    m.query = "Marienplatz".to_string();
    m.center = LatLon::new(CENTER.lat - 0.004, CENTER.lon - 0.003);
    m.zoom = 14.6;
    m.status = "© OpenStreetMap contributors · OSRM · transitous".to_string();
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basemap_is_populated() {
        let b = basemap();
        assert!(b.roads.len() > 30, "expected a dense grid, got {}", b.roads.len());
        assert!(b.areas.iter().any(|a| a.kind == AreaKind::Park));
        assert!(b.areas.iter().any(|a| a.kind == AreaKind::Building));
        assert!(b.river.len() > 5);
    }

    #[test]
    fn default_model_has_route_and_transit() {
        let m = default_model();
        assert!(m.route.is_some());
        assert_eq!(m.transit.len(), 3);
        assert!(m.selected_place().is_some());
        assert!(!m.saved_places().is_empty());
    }

    #[test]
    fn sample_route_geometry_monotonic_length() {
        let r = sample_route();
        assert!(r.distance_m > 0.0);
        assert!(r.duration_s > 0.0);
        assert!(r.geometry.len() >= 3);
        assert_eq!(r.steps.last().unwrap().maneuver, Maneuver::Arrive);
    }
}
