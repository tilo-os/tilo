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
    Category, DownloadedRegion, LatLon, Maneuver, MapModel, Panel, Place, Poi, Route, RouteStep,
    TransitJourney, TransitLeg, TransitMode, TravelMode,
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

impl Basemap {
    /// True when there is no vector cartography to draw — the live app's
    /// first-run state, where a reference grid substrate stands in until real
    /// raster imagery arrives.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.roads.is_empty() && self.river.is_empty()
    }
}

/// Tiny deterministic PRNG so the generated map is identical every run
/// (stable screenshots).
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
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

/// Minutes past midnight the sample journeys depart from: the time every
/// capture shows (`ui::clock::CAPTURE_LOCAL_TIME`, 10:42), so the departures
/// lie just ahead of "now" in a screenshot.
pub const DEMO_CLOCK_BASE_MIN: i64 =
    ui::clock::CAPTURE_LOCAL_TIME.3 as i64 * 60 + ui::clock::CAPTURE_LOCAL_TIME.4 as i64;

/// Walk the integer grid lines strictly between `a` and `b` in the travel
/// direction, then land exactly on `b`. Empty when `a == b`. The building block
/// for [`grid_path`].
fn axis_steps(a: f64, b: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if (b - a).abs() < 1e-9 {
        return out;
    }
    // Bounded by the grid span (+2) rather than a float `while`, so a
    // pathological input can't spin forever on rounding.
    let bound = (b - a).abs().ceil() as u32 + 2;
    if b > a {
        let mut n = a.floor() + 1.0;
        for _ in 0..bound {
            if n >= b - 1e-9 {
                break;
            }
            out.push(n);
            n += 1.0;
        }
    } else {
        let mut n = a.ceil() - 1.0;
        for _ in 0..bound {
            if n <= b + 1e-9 {
                break;
            }
            out.push(n);
            n -= 1.0;
        }
    }
    out.push(b);
    out
}

/// A polyline between two grid-cell coordinates that follows the street grid —
/// horizontally along the start row, then vertically along the destination
/// column — instead of cutting a diagonal across the blocks. This keeps the
/// sample transit legs sitting on the drawn roads (the only routing graph the
/// offline basemap has), mirroring how [`sample_route`] walks the grid. Live
/// data supplies real `legGeometry`; this is only the offline stand-in.
fn grid_path(from: (f64, f64), to: (f64, f64)) -> Vec<LatLon> {
    let (x0, y0) = from;
    let (x1, y1) = to;
    let mut pts = vec![cell(x0, y0)];
    for x in axis_steps(x0, x1) {
        pts.push(cell(x, y0));
    }
    for y in axis_steps(y0, y1) {
        pts.push(cell(x1, y));
    }
    pts
}

/// Build the stylised basemap.
#[allow(
    clippy::too_many_lines,
    reason = "one deterministic generation pass per layer (river, parks, buildings, roads); \
              splitting it would only relocate the code, not shrink what a reader must trace"
)]
#[allow(
    clippy::similar_names,
    reason = "`nx`/`ny` (the grid cell under test) and `px`/`py` (a park centre from the \
              lookup table) name genuinely different things; the shared axis-letter suffix \
              is the point, not an accident"
)]
pub fn basemap() -> Basemap {
    let mut rng = Rng(0x51ED_2026_C0FF_EE01);
    let mut areas: Vec<Area> = Vec::new();
    let mut roads: Vec<Road> = Vec::new();

    // --- River: a wavy north-south centre-line, slightly east of centre.
    let mut river = Vec::new();
    let base_x = 8.5;
    for i in -NY..=NY {
        let t = f64::from(i);
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
            let cx = f64::from(ix) + 0.5;
            let cy = f64::from(iy) + 0.5;
            if near_park(cx, cy) || near_river(cx, cy) {
                continue;
            }
            if rng.f() > 0.62 {
                continue;
            }
            let inset = rng.range(0.16, 0.30);
            let jx = rng.range(-0.05, 0.05);
            let jy = rng.range(-0.05, 0.05);
            let x0 = f64::from(ix) + inset + jx;
            let x1 = (f64::from(ix) + 1.0) - inset + jx;
            let y0 = f64::from(iy) + inset + jy;
            let y1 = (f64::from(iy) + 1.0) - inset + jy;
            areas.push(Area {
                kind: AreaKind::Building,
                ring: vec![cell(x0, y0), cell(x1, y0), cell(x1, y1), cell(x0, y1)],
            });
        }
    }

    // --- Street grid: residential roads on every grid line.
    for ix in -NX..=NX {
        let x = f64::from(ix);
        let mut pts = Vec::new();
        for iy in -NY..=NY {
            let j = rng.range(-0.04, 0.04);
            pts.push(cell(x + j, f64::from(iy)));
        }
        roads.push(Road {
            class: RoadClass::Residential,
            pts,
        });
    }
    for iy in -NY..=NY {
        let y = f64::from(iy);
        let mut pts = Vec::new();
        for ix in -NX..=NX {
            let j = rng.range(-0.04, 0.04);
            pts.push(cell(f64::from(ix), y + j));
        }
        roads.push(Road {
            class: RoadClass::Residential,
            pts,
        });
    }

    // --- A few primary avenues (thicker) crossing the grid.
    roads.push(Road {
        class: RoadClass::Primary,
        pts: (-NX..=NX).map(|ix| cell(f64::from(ix), 0.0)).collect(),
    });
    roads.push(Road {
        class: RoadClass::Primary,
        pts: (-NY..=NY).map(|iy| cell(0.0, f64::from(iy))).collect(),
    });
    // A diagonal secondary boulevard.
    roads.push(Road {
        class: RoadClass::Secondary,
        pts: (-NY..=NY)
            .map(|iy| cell(f64::from(iy) * 0.8 - 3.0, f64::from(iy)))
            .collect(),
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
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let a = (i as f64) / (n as f64) * std::f64::consts::TAU;
        let jitter = rng.range(0.78, 1.18);
        let dx = a.cos() * rx * jitter;
        let dy = a.sin() * ry * jitter;
        pts.push(LatLon::new(c.lat + dy * DLAT, c.lon + dx * DLON));
    }
    pts
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
        ),
        Place::new(
            "cafe-glueck",
            "Café Glück",
            Category::Cafe,
            "Theatinerstraße 18",
            cell(2.6, 2.1),
        ),
        Place::new(
            "stadtmuseum",
            "Stadtmuseum",
            Category::Museum,
            "St.-Jakobs-Platz 1",
            cell(-2.1, -1.2),
        ),
        Place::new(
            "isar-biergarten",
            "Isar Biergarten",
            Category::Bar,
            "Am Kanal 4",
            cell(7.4, -2.6),
        ),
        Place::new(
            "hofgarten",
            "Hofgarten",
            Category::Park,
            "Hofgartenstraße",
            cell(1.5, 5.4),
        ),
        Place::new(
            "hauptbahnhof",
            "Hauptbahnhof",
            Category::Transit,
            "Bahnhofplatz 2",
            cell(-9.0, 1.1),
        ),
        Place::new(
            "hotel-vier",
            "Hotel Vier Türme",
            Category::Hotel,
            "Sendlinger Str. 44",
            cell(3.8, -4.2),
        ),
        Place::new(
            "viktualienmarkt",
            "Viktualienmarkt",
            Category::Shop,
            "Viktualienmarkt 3",
            cell(0.3, -2.1),
        ),
    ]
}

/// Origin point for directions ("your location").
pub fn origin() -> LatLon {
    cell(-6.5, -6.0)
}

/// A couple of bundled "downloaded areas" so the offline-maps overlay's
/// managed-region list renders in `--shots`/tests without any real download.
/// The live app builds these from real viewport downloads instead.
pub fn regions() -> Vec<DownloadedRegion> {
    vec![
        DownloadedRegion {
            id: "region-demo-1".to_string(),
            name: "München Altstadt".to_string(),
            min: cell(-6.0, -6.0),
            max: cell(6.0, 6.0),
            z0: 13,
            z1: 16,
            tiles: 1148,
        },
        DownloadedRegion {
            id: "region-demo-2".to_string(),
            name: "Hauptbahnhof".to_string(),
            min: cell(-11.0, -2.0),
            max: cell(-6.0, 3.0),
            z0: 14,
            z1: 17,
            tiles: 412,
        },
    ]
}

/// Bundled viewport POIs with rich tags, standing in for a live Overpass reply
/// so the offline `--shots`/tests can render the themed POI marker layer and an
/// open info card without a network round-trip.
pub fn pois() -> Vec<Poi> {
    let mk = |id: &str, name: &str, cat: Category, nx: f64, ny: f64| Poi {
        id: id.to_string(),
        name: name.to_string(),
        category: cat,
        pos: cell(nx, ny),
        address: String::new(),
        opening_hours: None,
        website: None,
        phone: None,
        kind: None,
    };
    vec![
        Poi {
            address: "Theatinerstraße 12, München".to_string(),
            opening_hours: Some("Mo–Sa 08:00–20:00".to_string()),
            website: Some("https://www.tambosi.de".to_string()),
            phone: Some("+49 89 2900 8600".to_string()),
            kind: Some("coffee".to_string()),
            ..mk("osm-node-201", "Café Tambosi", Category::Cafe, 1.2, 1.6)
        },
        Poi {
            address: "Sparkassenstraße 2, München".to_string(),
            opening_hours: Some("11:00–23:30".to_string()),
            kind: Some("bavarian".to_string()),
            ..mk(
                "osm-node-202",
                "Ratskeller",
                Category::Restaurant,
                -0.8,
                0.6,
            )
        },
        Poi {
            address: "Am Platzl 9, München".to_string(),
            opening_hours: Some("09:00–24:00".to_string()),
            website: Some("https://www.hofbraeuhaus.de".to_string()),
            kind: Some("beer garden".to_string()),
            ..mk("osm-node-203", "Hofbräuhaus", Category::Bar, 1.6, 0.9)
        },
        Poi {
            address: "St.-Jakobs-Platz 1".to_string(),
            opening_hours: Some("Tu–Su 10:00–18:00".to_string()),
            ..mk(
                "osm-way-204",
                "Jüdisches Museum",
                Category::Museum,
                -2.0,
                -1.0,
            )
        },
        Poi {
            opening_hours: Some("24/7".to_string()),
            ..mk(
                "osm-node-205",
                "Marienplatz (U/S)",
                Category::Transit,
                0.2,
                -0.3,
            )
        },
        Poi {
            kind: Some("bakery".to_string()),
            opening_hours: Some("06:00–19:00".to_string()),
            ..mk(
                "osm-node-206",
                "Rischart Backhaus",
                Category::Shop,
                0.6,
                -0.9,
            )
        },
        Poi {
            address: "Neuhauser Str. 2".to_string(),
            ..mk("osm-node-207", "St. Michael", Category::Landmark, -1.6, 0.2)
        },
        Poi {
            opening_hours: Some("08:00–22:00".to_string()),
            ..mk("osm-node-208", "Viktualien Café", Category::Cafe, 0.5, -2.0)
        },
        Poi {
            address: "Sendlinger Str. 44".to_string(),
            phone: Some("+49 89 5525 570".to_string()),
            ..mk(
                "osm-node-209",
                "Hotel Blauer Bock",
                Category::Hotel,
                3.8,
                -4.0,
            )
        },
        Poi {
            kind: Some("park".to_string()),
            ..mk("osm-way-210", "Hofgarten", Category::Park, 1.5, 5.2)
        },
    ]
}

/// A sample driving route from the origin to Marienplatz that follows the
/// street grid (east, then north), with believable steps.
pub fn sample_route() -> Route {
    let o = origin();
    // East along the ny ≈ -6 row to nx = 0, then north along nx = 0 col.
    let mut geometry = Vec::new();
    let row_y = -6.0;
    // Bounded rather than a float `while`; the grid span here is a small
    // compile-time-known constant, so 64 steps is generous headroom.
    let mut nx = -6.5;
    for _ in 0..64 {
        if nx > 0.0 {
            break;
        }
        geometry.push(cell(nx, row_y));
        nx += 1.0;
    }
    geometry.push(cell(0.0, row_y));
    let mut ny = row_y;
    for _ in 0..64 {
        if ny > 0.0 {
            break;
        }
        geometry.push(cell(0.0, ny));
        ny += 1.0;
    }
    geometry.push(cell(0.0, 0.0));

    let leg_east = polyline_length_m(geometry.get(..7.min(geometry.len())).unwrap_or(&[]));
    let leg_north = polyline_length_m(
        geometry
            .get(6.min(geometry.len().saturating_sub(1))..)
            .unwrap_or(&[]),
    );
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

/// Sample public-transport itineraries for the Transit panel. Each leg carries
/// a plausible on-grid `geometry` polyline so the offline `--shots` map and the
/// hermetic tests exercise the real on-map itinerary rendering (the live app
/// gets these from the planner's `legGeometry`).
#[allow(
    clippy::too_many_lines,
    reason = "one block per bundled sample itinerary; splitting it would only relocate the \
              fixture data, not shrink what a reader must trace for one journey"
)]
pub fn sample_transit() -> Vec<TransitJourney> {
    // All departure/arrival clocks derive from the one shared demo base so the
    // Transit departures stay consistent with the Drive summary's clock.
    let clk = |off: i64| -> String {
        let m = DEMO_CLOCK_BASE_MIN + off;
        format!("{:02}:{:02}", m / 60, m % 60)
    };
    let walk = |from: &str, to: &str, dep: i64, arr: i64, min: u32, geom: Vec<LatLon>| TransitLeg {
        mode: TransitMode::Walk,
        line: String::new(),
        headsign: String::new(),
        from_name: from.to_string(),
        to_name: to.to_string(),
        dep: clk(dep),
        arr: clk(arr),
        duration_min: min,
        num_stops: 0,
        geometry: geom,
        color: None,
        text_color: None,
        agency: String::new(),
        realtime: false,
    };

    vec![
        TransitJourney {
            dep: clk(12),
            arr: clk(26),
            duration_min: 14,
            transfers: 0,
            legs: vec![
                walk(
                    "Your location",
                    "Goetheplatz",
                    12,
                    16,
                    4,
                    grid_path((-6.5, -6.0), (-5.0, -5.0)),
                ),
                TransitLeg {
                    mode: TransitMode::Subway,
                    line: "U3".to_string(),
                    headsign: "Moosach".to_string(),
                    from_name: "Goetheplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: clk(17),
                    arr: clk(23),
                    duration_min: 6,
                    num_stops: 3,
                    geometry: grid_path((-5.0, -5.0), (0.0, 0.0)),
                    color: Some("eb690b".to_string()),
                    text_color: Some("ffffff".to_string()),
                    agency: "MVG".to_string(),
                    realtime: true,
                },
                walk(
                    "Marienplatz",
                    "Marienplatz",
                    24,
                    26,
                    2,
                    grid_path((0.0, 0.0), (0.2, -0.2)),
                ),
            ],
        },
        TransitJourney {
            dep: clk(8),
            arr: clk(27),
            duration_min: 19,
            transfers: 1,
            legs: vec![
                walk(
                    "Your location",
                    "Sendlinger Tor",
                    8,
                    11,
                    3,
                    grid_path((-6.5, -6.0), (-4.0, -4.0)),
                ),
                TransitLeg {
                    mode: TransitMode::Tram,
                    line: "17".to_string(),
                    headsign: "Schwanseestraße".to_string(),
                    from_name: "Sendlinger Tor".to_string(),
                    to_name: "Reichenbachplatz".to_string(),
                    dep: clk(12),
                    arr: clk(16),
                    duration_min: 4,
                    num_stops: 2,
                    geometry: grid_path((-4.0, -4.0), (-2.0, -1.5)),
                    color: Some("e30613".to_string()),
                    text_color: Some("ffffff".to_string()),
                    agency: "MVG".to_string(),
                    realtime: true,
                },
                TransitLeg {
                    mode: TransitMode::Bus,
                    line: "52".to_string(),
                    headsign: "Tierpark".to_string(),
                    from_name: "Reichenbachplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: clk(19),
                    arr: clk(25),
                    duration_min: 6,
                    num_stops: 3,
                    geometry: grid_path((-2.0, -1.5), (0.0, 0.0)),
                    color: Some("005e82".to_string()),
                    text_color: Some("ffffff".to_string()),
                    agency: "MVG".to_string(),
                    realtime: false,
                },
                walk(
                    "Marienplatz",
                    "Marienplatz",
                    25,
                    27,
                    2,
                    grid_path((0.0, 0.0), (0.2, -0.2)),
                ),
            ],
        },
        TransitJourney {
            dep: clk(5),
            arr: clk(29),
            duration_min: 24,
            transfers: 1,
            legs: vec![
                walk(
                    "Your location",
                    "Poccistraße",
                    5,
                    10,
                    5,
                    grid_path((-6.5, -6.0), (-6.0, -4.5)),
                ),
                TransitLeg {
                    mode: TransitMode::Subway,
                    line: "U3/U6".to_string(),
                    headsign: "Garching".to_string(),
                    from_name: "Poccistraße".to_string(),
                    to_name: "Odeonsplatz".to_string(),
                    dep: clk(13),
                    arr: clk(21),
                    duration_min: 8,
                    num_stops: 4,
                    geometry: grid_path((-6.0, -4.5), (1.5, 5.0)),
                    color: Some("0065ae".to_string()),
                    text_color: Some("ffffff".to_string()),
                    agency: "MVG".to_string(),
                    realtime: true,
                },
                TransitLeg {
                    mode: TransitMode::Rail,
                    line: "S1".to_string(),
                    headsign: "Ostbahnhof".to_string(),
                    from_name: "Odeonsplatz".to_string(),
                    to_name: "Marienplatz".to_string(),
                    dep: clk(24),
                    arr: clk(27),
                    duration_min: 3,
                    num_stops: 1,
                    geometry: grid_path((1.5, 5.0), (0.0, 0.0)),
                    color: Some("16bae7".to_string()),
                    text_color: Some("ffffff".to_string()),
                    agency: "DB Regio AG S-Bahn München".to_string(),
                    realtime: true,
                },
                walk(
                    "Marienplatz",
                    "Marienplatz",
                    27,
                    29,
                    2,
                    grid_path((0.0, 0.0), (0.2, -0.2)),
                ),
            ],
        },
    ]
}

/// Assemble the fully-populated default model used by `--shots` and as the
/// initial state of a fresh live session.
/// Sample geocoded region candidates for the offline download-by-place shot
/// (no network). Real Munich bounding boxes; used only behind `--shots`/demo.
pub fn download_regions() -> Vec<crate::net::RegionHit> {
    use crate::net::RegionHit;
    vec![
        RegionHit {
            name: "Munich".into(),
            context: "Bavaria, Germany".into(),
            pos: LatLon::new(48.1371, 11.5754),
            sw: LatLon::new(48.0616, 11.3608),
            ne: LatLon::new(48.2482, 11.7229),
        },
        RegionHit {
            name: "München Hauptbahnhof".into(),
            context: "Bahnhofplatz, München".into(),
            pos: LatLon::new(48.1402, 11.5586),
            sw: LatLon::new(48.1382, 11.5556),
            ne: LatLon::new(48.1422, 11.5616),
        },
        RegionHit {
            name: "Marienplatz".into(),
            context: "Altstadt-Lehel, München".into(),
            pos: LatLon::new(48.1374, 11.5755),
            sw: LatLon::new(48.1364, 11.5745),
            ne: LatLon::new(48.1384, 11.5765),
        },
    ]
}

pub fn default_model() -> MapModel {
    let mut m = MapModel {
        places: places(),
        ..Default::default()
    };
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
    m.dest = Some(cell(0.0, 0.0));
    m.dest_name = "Marienplatz".to_string();
    m.route = Some(sample_route());
    m.transit = sample_transit();
    m.panel = Panel::Directions;
    m.mode = TravelMode::Car;
    m.query = "Marienplatz".to_string();
    m.center = LatLon::new(CENTER.lat - 0.004, CENTER.lon - 0.003);
    m.zoom = 14.6;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample world: populated basemap, default route and transit, monotonic route
    /// length, grid-following axis-aligned transit legs, and demo-base clocks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sample_world_is_populated_and_geometrically_sound() {
        // Phase `basemap_is_populated`.
        {
            let b = basemap();
            assert!(
                b.roads.len() > 30,
                "expected a dense grid, got {}",
                b.roads.len()
            );
            assert!(b.areas.iter().any(|a| a.kind == AreaKind::Park));
            assert!(b.areas.iter().any(|a| a.kind == AreaKind::Building));
            assert!(b.river.len() > 5);
        }

        // Phase `default_model_has_route_and_transit`.
        {
            let m = default_model();
            assert!(m.route.is_some());
            assert_eq!(m.transit.len(), 3);
            assert!(m.selected_place().is_some());
            assert!(!m.saved_places().is_empty());
        }

        // Phase `sample_route_geometry_monotonic_length`.
        {
            let r = sample_route();
            assert!(r.distance_m > 0.0);
            assert!(r.duration_s > 0.0);
            assert!(r.geometry.len() >= 3);
            assert_eq!(r.steps.last().unwrap().maneuver, Maneuver::Arrive);
        }

        // Phase `transit_legs_follow_grid_not_diagonal`.
        // Every drawn transit leg follows the street grid: each segment runs along
        // a grid line (constant lat OR constant lon), never a crow-flies diagonal
        // across the blocks, and a ride leg bends at least once (so it is provably
        // not a 2-point straight line). This guards the old fake-diagonal route.
        {
            let journeys = sample_transit();
            assert!(!journeys.is_empty());
            let approx = |a: f64, b: f64| (a - b).abs() < 1e-9;
            let mut exercised_ride = false;
            for j in &journeys {
                for leg in &j.legs {
                    let g = &leg.geometry;
                    assert!(g.len() >= 2, "a drawn leg has a real polyline, not a point");
                    for seg in g.windows(2) {
                        let axis_aligned =
                            approx(seg[0].lat, seg[1].lat) || approx(seg[0].lon, seg[1].lon);
                        assert!(
                            axis_aligned,
                            "leg segment cuts a diagonal across blocks: {:?} -> {:?}",
                            seg[0], seg[1]
                        );
                    }
                    if leg.mode != TransitMode::Walk {
                        assert!(
                            g.len() >= 3,
                            "a ride leg follows multiple grid segments, not a straight 2-point line"
                        );
                        exercised_ride = true;
                    }
                }
            }
            assert!(
                exercised_ride,
                "at least one non-walk transit leg was checked"
            );
        }

        // Phase `grid_path_is_axis_aligned_and_keeps_endpoints`.
        // `grid_path` never emits a diagonal segment and keeps the exact endpoints
        // as the first/last vertex (the transfer pucks anchor on them).
        {
            let p = grid_path((-5.0, -5.0), (0.0, 0.0));
            assert_eq!(p.first().copied(), Some(cell(-5.0, -5.0)));
            assert_eq!(p.last().copied(), Some(cell(0.0, 0.0)));
            assert!(p.len() >= 3, "a two-axis path bends through the grid");
            for seg in p.windows(2) {
                let aligned = (seg[0].lat - seg[1].lat).abs() < 1e-9
                    || (seg[0].lon - seg[1].lon).abs() < 1e-9;
                assert!(aligned, "grid_path emitted a diagonal segment");
            }
        }

        // Phase `transit_clocks_lie_ahead_of_the_capture_time`.
        // The sample departures derive from the time a capture shows, so every
        // journey leaves after "now" in a screenshot and arrives after it leaves.
        {
            let now = format!(
                "{:02}:{:02}",
                DEMO_CLOCK_BASE_MIN / 60,
                DEMO_CLOCK_BASE_MIN % 60
            );
            assert_eq!(now, "10:42");
            for j in &sample_transit() {
                assert!(j.dep > now, "journey departs after now: {}", j.dep);
                assert!(j.arr > j.dep, "journey arrives after it leaves");
                for leg in &j.legs {
                    assert!(leg.dep > now, "leg departs after now: {}", leg.dep);
                }
            }
        }
    }
}
