//! Pure data model for the Maps app.
//!
//! The window's model ([`MapModel`]) holds what a window shows: the shared
//! workspace's places, searches and plans ([`crate::workspace::Workspace`],
//! the document) beside its own presentation state. Camera, panel,
//! selection, drafts and ranked caches remain per-window, and the offline
//! areas are this device's ([`crate::offline_areas`]).

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Model};

/// A WGS-84 geographic coordinate (degrees).
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }
}

/// Point-of-interest category. Drives the marker colour and the place-card
/// icon. Kept deliberately small and OSM-flavoured.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Landmark,
    Cafe,
    Restaurant,
    Bar,
    Museum,
    Park,
    Transit,
    Hotel,
    Shop,
    Hospital,
    #[default]
    Other,
}

impl Category {
    /// The icon a marker and a place row show for the category.
    pub fn icon(self) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self {
            Category::Landmark => Icon::Flag,
            Category::Cafe => Icon::Coffee,
            Category::Restaurant => Icon::Utensils,
            Category::Bar => Icon::Beer,
            Category::Museum => Icon::Landmark,
            Category::Park => Icon::Trees,
            Category::Transit => Icon::TrainFront,
            Category::Hotel => Icon::BedDouble,
            Category::Shop => Icon::ShoppingBag,
            Category::Hospital => Icon::Hospital,
            Category::Other => Icon::MapPin,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Landmark => "Landmark",
            Category::Cafe => "Café",
            Category::Restaurant => "Restaurant",
            Category::Bar => "Bar",
            Category::Museum => "Museum",
            Category::Park => "Park",
            Category::Transit => "Transit stop",
            Category::Hotel => "Hotel",
            Category::Shop => "Shop",
            Category::Hospital => "Hospital",
            Category::Other => "Place",
        }
    }

    /// Best-effort mapping from a Nominatim `class`/`type` pair.
    pub fn from_osm(class: &str, kind: &str) -> Category {
        match (class, kind) {
            ("amenity", "cafe") => Category::Cafe,
            ("amenity", "restaurant" | "fast_food" | "food_court") => Category::Restaurant,
            ("amenity", "bar" | "pub" | "biergarten" | "nightclub") => Category::Bar,
            ("amenity", "hospital" | "clinic" | "doctors") => Category::Hospital,
            ("tourism", "museum" | "gallery" | "artwork") => Category::Museum,
            ("tourism", "hotel" | "hostel" | "guest_house" | "motel") => Category::Hotel,
            ("tourism", "attraction" | "viewpoint" | "monument") | ("historic", _) => {
                Category::Landmark
            }
            ("leisure", "park" | "garden" | "nature_reserve") => Category::Park,
            ("shop", _) => Category::Shop,
            ("railway" | "public_transport", _) | ("highway", "bus_stop") => Category::Transit,
            _ => Category::Other,
        }
    }
}

/// A point of interest. `id` is stable; markers, the place card and the
/// saved-list all reference it.
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Place {
    /// The place's id, as the geocoder or the map names it.
    #[model(readonly)]
    pub id: String,
    /// Name
    #[model(label)]
    pub name: String,
    /// Category
    pub category: Category,
    /// Address
    pub address: String,
    /// Location
    #[model(readonly)]
    pub pos: LatLon,
    /// Saved
    pub saved: bool,
}

impl Identified for Place {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

impl Place {
    pub fn new(id: &str, name: &str, category: Category, address: &str, pos: LatLon) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            category,
            address: address.to_string(),
            pos,
            saved: false,
        }
    }
}

/// A point of interest fetched live from the Overpass API for the current
/// viewport and drawn as our own themed marker layer (never baked into tiles).
/// Ephemeral view data — re-fetched on pan/zoom, not persisted in the CRDT — so
/// it carries the readable OSM tags the info card surfaces rather than the
/// slimmer persisted [`Place`].
#[derive(Clone, Debug, PartialEq)]
pub struct Poi {
    /// Stable OSM identity, e.g. `osm-node-12345` — dedupes across refetches.
    pub id: String,
    pub name: String,
    pub category: Category,
    pub pos: LatLon,
    /// Composed street address (house number + road) when tagged.
    pub address: String,
    pub opening_hours: Option<String>,
    pub website: Option<String>,
    pub phone: Option<String>,
    /// Cuisine / shop-kind subtitle, e.g. "italian", "bakery".
    pub kind: Option<String>,
}

impl Poi {
    /// What the place is, in sentence case: OpenStreetMap's finer kind
    /// ("Beer garden" for `beer_garden`) when tagged, else the category.
    pub fn kind_label(&self) -> String {
        let Some(kind) = self.kind.as_deref().filter(|k| !k.trim().is_empty()) else {
            return self.category.label().to_string();
        };
        ui::format::capitalize_first(&kind.trim().replace(['_', ';'], " "))
    }
}

/// Travel mode for the directions engine. `Car`/`Bike`/`Foot` route on roads
/// (OSRM); `Transit` plans a public-transport journey (MOTIS) instead — it is
/// a mode of Directions, not a separate surface.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TravelMode {
    #[default]
    Car,
    Transit,
    Bike,
    Foot,
}

impl TravelMode {
    pub const ALL: [TravelMode; 4] = [
        TravelMode::Car,
        TravelMode::Transit,
        TravelMode::Bike,
        TravelMode::Foot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TravelMode::Car => "Drive",
            TravelMode::Transit => "Transit",
            TravelMode::Bike => "Cycle",
            TravelMode::Foot => "Walk",
        }
    }

    /// Whether this mode plans a public-transport journey rather than a road
    /// route. Callers route through OSRM only for the road modes.
    pub fn is_transit(self) -> bool {
        matches!(self, TravelMode::Transit)
    }

    /// OSRM public-server profile path component. Only meaningful for the road
    /// modes; `Transit` never routes via OSRM (guarded by `is_transit`).
    pub fn osrm_profile(self) -> &'static str {
        match self {
            // `Transit` never actually routes via OSRM (guarded by
            // `is_transit`); it shares Car's profile string only because the
            // return type demands some value.
            TravelMode::Car | TravelMode::Transit => "driving",
            TravelMode::Bike => "cycling",
            TravelMode::Foot => "foot",
        }
    }

    /// Plausible average speed (km/h) for the offline road-route fallback.
    /// Unused for `Transit` (which has no road-route fallback).
    pub fn speed_kmh(self) -> f64 {
        match self {
            TravelMode::Car => 32.0,
            TravelMode::Bike => 15.0,
            TravelMode::Foot => 4.8,
            TravelMode::Transit => 20.0,
        }
    }
}

/// Turn type for a routing step. Maps to OSRM's `maneuver.modifier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Maneuver {
    Depart,
    Continue,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    Uturn,
    Roundabout,
    Merge,
    Arrive,
}

impl Maneuver {
    /// The icon that shows this manoeuvre in a step row and the
    /// navigation card.
    pub fn icon(self) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self {
            Maneuver::Depart | Maneuver::Continue | Maneuver::Merge => Icon::ArrowUp,
            Maneuver::SlightLeft => Icon::ArrowUpLeft,
            Maneuver::Left => Icon::CornerUpLeft,
            Maneuver::SharpLeft => Icon::ArrowDownLeft,
            Maneuver::SlightRight => Icon::ArrowUpRight,
            Maneuver::Right => Icon::CornerUpRight,
            Maneuver::SharpRight => Icon::ArrowDownRight,
            Maneuver::Uturn => Icon::Undo2,
            Maneuver::Roundabout => Icon::RotateCw,
            Maneuver::Arrive => Icon::Flag,
        }
    }

    pub fn from_osrm(kind: &str, modifier: &str) -> Maneuver {
        match kind {
            "depart" => Maneuver::Depart,
            "arrive" => Maneuver::Arrive,
            "roundabout" | "rotary" | "roundabout turn" => Maneuver::Roundabout,
            "merge" => Maneuver::Merge,
            "uturn" => Maneuver::Uturn,
            _ => match modifier {
                "uturn" => Maneuver::Uturn,
                "sharp left" => Maneuver::SharpLeft,
                "left" => Maneuver::Left,
                "slight left" => Maneuver::SlightLeft,
                "sharp right" => Maneuver::SharpRight,
                "right" => Maneuver::Right,
                "slight right" => Maneuver::SlightRight,
                _ => Maneuver::Continue,
            },
        }
    }
}

/// One turn-by-turn instruction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteStep {
    pub instruction: String,
    pub distance_m: f64,
    pub maneuver: Maneuver,
}

/// A computed road route between two points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Route {
    pub mode: TravelMode,
    pub distance_m: f64,
    pub duration_s: f64,
    /// Decoded polyline, WGS-84.
    pub geometry: Vec<LatLon>,
    pub steps: Vec<RouteStep>,
    pub from: LatLon,
    pub to: LatLon,
}

/// Mode of one public-transport leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TransitMode {
    Walk,
    Bus,
    Tram,
    Subway,
    Rail,
    Ferry,
}

impl TransitMode {
    pub fn label(self) -> &'static str {
        match self {
            TransitMode::Walk => "Walk",
            TransitMode::Bus => "Bus",
            TransitMode::Tram => "Tram",
            TransitMode::Subway => "Metro",
            TransitMode::Rail => "Train",
            TransitMode::Ferry => "Ferry",
        }
    }

    /// The mode a MOTIS leg reports, by its mode name.
    pub fn from_motis(mode: &str) -> TransitMode {
        match mode.to_ascii_uppercase().as_str() {
            "WALK" => TransitMode::Walk,
            "TRAM" | "STREETCAR" | "LIGHT_RAIL" => TransitMode::Tram,
            "SUBWAY" | "METRO" => TransitMode::Subway,
            "RAIL" | "REGIONAL_RAIL" | "REGIONAL_FAST_RAIL" | "LONG_DISTANCE"
            | "HIGHSPEED_RAIL" | "NIGHT_RAIL" => TransitMode::Rail,
            "FERRY" => TransitMode::Ferry,
            // "BUS" | "COACH", and any mode MOTIS doesn't report yet, both land
            // here — the safest default for an unrecognised surface mode.
            _ => TransitMode::Bus,
        }
    }
}

/// One leg of a public-transport itinerary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitLeg {
    pub mode: TransitMode,
    /// Line label, e.g. "U3", "58", "S1". Empty for walking legs.
    pub line: String,
    pub headsign: String,
    pub from_name: String,
    pub to_name: String,
    /// "HH:MM" local clock.
    pub dep: String,
    pub arr: String,
    pub duration_min: u32,
    pub num_stops: u32,
    pub geometry: Vec<LatLon>,
    /// Authentic GTFS line colour (`routeColor`) as a hex string without `#`,
    /// e.g. "78b82c" for the S2. `None` for walk legs / feeds without a colour;
    /// the renderer falls back to a theme-derived per-mode colour.
    #[serde(default)]
    pub color: Option<String>,
    /// GTFS `routeTextColor` hex (label colour on the line pill).
    #[serde(default)]
    pub text_color: Option<String>,
    /// Operating agency, e.g. "DB Regio AG S-Bahn München".
    #[serde(default)]
    pub agency: String,
    /// Whether this leg's times are backed by real-time data (vs. schedule
    /// only). Surfaced as a "live" indicator so freshness is honest.
    #[serde(default)]
    pub realtime: bool,
}

/// A full door-to-door public-transport journey option.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitJourney {
    pub dep: String,
    pub arr: String,
    pub duration_min: u32,
    pub transfers: u32,
    pub legs: Vec<TransitLeg>,
}

/// Whether a transit query is anchored to a departure or an arrival time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TransitTimeMode {
    /// Leave at the chosen time (the default, "leave now").
    Depart,
    /// Arrive by the chosen time.
    Arrive,
}

/// The depart/arrive anchor for the Transit planner. Defaults to "leave now"
/// (`Depart` at a zero offset); the user shifts the anchor in ±15-minute
/// steps and the offset is applied to the wall clock when the query is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransitTime {
    pub mode: TransitTimeMode,
    /// Minutes relative to "now". `0` with `Depart` means "leave now".
    pub offset_min: i32,
}

tilo_resource::json_model!(TransitTime);

impl Default for TransitTime {
    fn default() -> Self {
        Self {
            mode: TransitTimeMode::Depart,
            offset_min: 0,
        }
    }
}

impl TransitTime {
    /// True when the anchor is the plain "leave now" default.
    pub fn is_now(&self) -> bool {
        self.mode == TransitTimeMode::Depart && self.offset_min == 0
    }

    /// True when the query should be anchored to the arrival time.
    pub fn arrive_by(&self) -> bool {
        self.mode == TransitTimeMode::Arrive
    }
}

/// Which view the collapsible side panel is showing. Not a user-facing tab: the
/// panel defaults to `Explore` (search, nearby, saved, recents, the selected
/// place card) and switches to `Directions` only as an action reached from a
/// place/POI card — routing between two points, with the travel mode (including
/// public `Transit`) chosen inside it. A back control returns to `Explore`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Panel {
    Explore,
    Directions,
}

impl Panel {
    pub const ALL: [Panel; 2] = [Panel::Explore, Panel::Directions];

    pub fn label(self) -> &'static str {
        match self {
            Panel::Explore => "Explore",
            Panel::Directions => "Directions",
        }
    }
}

/// A map area the user downloaded for offline use. The tiles themselves live
/// in the on-disk tile cache (`z/x/y.png`); this record is the user-facing
/// handle to that area — it is what the offline panel lists and what a delete
/// action targets. Persisted in the CRDT so saved areas survive a restart and
/// ride undo/redo like the rest of the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadedRegion {
    /// Stable id (a monotonic timestamp) so the manage-list can target a row.
    pub id: String,
    /// Human label — a place inside the area when one is known, else its centre
    /// coordinate.
    pub name: String,
    /// South-west / north-east corners of the downloaded bounding box.
    pub min: LatLon,
    pub max: LatLon,
    /// Inclusive slippy-map zoom range that was fetched.
    pub z0: u8,
    pub z1: u8,
    /// How many tiles the download covered (for the list's size summary).
    pub tiles: u32,
}

/// The Maps window state. [`crate::workspace::Workspace`] holds its explicit
/// collaborative subset rather than serialising this entire struct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapModel {
    /// All places known to the app: sample POIs, search results that were
    /// pinned, and saved favourites. The marker layer renders these.
    pub places: Vec<Place>,
    /// Recently searched query strings, most-recent first.
    pub recents: Vec<String>,
    /// Currently selected place id (drives the place card).
    pub selected: Option<String>,
    /// Active road route, if any.
    pub route: Option<Route>,
    /// The routes the service offered for this trip, the active one first:
    /// a single route when it offered no other, and what "another route"
    /// steps through.
    pub route_alternatives: Vec<Route>,
    /// Public-transport itinerary options.
    pub transit: Vec<TransitJourney>,
    /// Index of the highlighted transit option.
    pub transit_choice: usize,
    pub panel: Panel,
    pub mode: TravelMode,
    /// Current search text.
    pub query: String,
    /// Text typed into the Directions "From" field (a real geocoding input).
    #[serde(default)]
    pub origin_query: String,
    /// Text typed into the Directions "To" field (a real geocoding input).
    #[serde(default)]
    pub dest_query: String,
    /// Depart/arrive anchor for the Transit planner.
    #[serde(default)]
    pub transit_time: TransitTime,
    /// Live search results (subset shown in the Explore panel).
    pub results: Vec<Place>,
    /// Route origin — "your location". Sample data seeds a plausible spot.
    pub origin: Option<LatLon>,
    /// Origin label for the directions panel.
    pub origin_name: String,
    /// Route destination. Held separately from `selected` so the directions
    /// panel can swap the two endpoints without the destination collapsing
    /// back onto the origin (which would route a point to itself).
    #[serde(default)]
    pub dest: Option<LatLon>,
    /// Destination label for the directions/transit panels.
    #[serde(default)]
    pub dest_name: String,
    /// Map view centre + fractional zoom (slippy-map z).
    pub center: LatLon,
    pub zoom: f64,
    /// One-line network/status message for the status bar.
    pub status: String,
    /// Map areas this device saved for offline use, as its device-local
    /// setting holds them. The tiles are persisted in the on-disk cache; this
    /// is the manageable list of what has been downloaded.
    #[serde(default)]
    pub regions: Vec<DownloadedRegion>,
}

impl Default for MapModel {
    fn default() -> Self {
        Self {
            places: Vec::new(),
            recents: Vec::new(),
            selected: None,
            route: None,
            route_alternatives: Vec::new(),
            transit: Vec::new(),
            transit_choice: 0,
            panel: Panel::Explore,
            mode: TravelMode::Car,
            query: String::new(),
            origin_query: String::new(),
            dest_query: String::new(),
            transit_time: TransitTime::default(),
            results: Vec::new(),
            origin: None,
            // Without a real location fix, routes start from the map centre.
            origin_name: "Map centre".to_string(),
            dest: None,
            dest_name: String::new(),
            center: LatLon::new(48.1371, 11.5754),
            zoom: 14.4,
            status: String::new(),
            regions: Vec::new(),
        }
    }
}

impl MapModel {
    pub fn place(&self, id: &str) -> Option<&Place> {
        self.places.iter().find(|p| p.id == id)
    }

    pub fn selected_place(&self) -> Option<&Place> {
        self.selected.as_deref().and_then(|id| self.place(id))
    }

    pub fn saved_places(&self) -> Vec<&Place> {
        self.places.iter().filter(|p| p.saved).collect()
    }

    /// Toggle the saved flag on a place by id, returning the new state.
    pub fn toggle_saved(&mut self, id: &str) -> bool {
        if let Some(p) = self.places.iter_mut().find(|p| p.id == id) {
            p.saved = !p.saved;
            p.saved
        } else {
            false
        }
    }

    /// Insert (or move-to-front) a recent search query, capped at 8.
    pub fn push_recent(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() {
            return;
        }
        self.recents.retain(|r| !r.eq_ignore_ascii_case(q));
        self.recents.insert(0, q.to_string());
        self.recents.truncate(8);
    }

    /// Merge a freshly-fetched place into `places` (dedupe by id) so the
    /// marker layer and the saved list stay coherent.
    pub fn upsert_place(&mut self, p: Place) {
        if let Some(existing) = self.places.iter_mut().find(|e| e.id == p.id) {
            let saved = existing.saved || p.saved;
            *existing = Place { saved, ..p };
        } else {
            self.places.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_and_saved_places_dedupe_cap_and_keep_flags() {
        {
            let mut m = MapModel::default();
            for i in 0..12 {
                m.push_recent(&format!("q{i}"));
            }
            assert_eq!(m.recents.len(), 8);
            assert_eq!(m.recents[0], "q11");
            // Re-pushing an existing term moves it to front, not duplicates.
            m.push_recent("Q7");
            assert_eq!(m.recents[0], "Q7");
            assert_eq!(
                m.recents
                    .iter()
                    .filter(|r| r.eq_ignore_ascii_case("q7"))
                    .count(),
                1
            );
        }

        {
            let mut m = MapModel::default();
            m.places.push(Place::new(
                "a",
                "A",
                Category::Cafe,
                "",
                LatLon::new(0.0, 0.0),
            ));
            assert!(m.toggle_saved("a"));
            assert!(!m.toggle_saved("a"));
            assert!(!m.toggle_saved("missing"));
        }

        {
            let mut m = MapModel::default();
            let mut p = Place::new("x", "X", Category::Shop, "addr", LatLon::new(1.0, 2.0));
            p.saved = true;
            m.upsert_place(p);
            // Re-fetch (unsaved) must not clobber the saved flag.
            m.upsert_place(Place::new(
                "x",
                "X2",
                Category::Shop,
                "addr2",
                LatLon::new(1.0, 2.0),
            ));
            assert!(m.place("x").unwrap().saved);
            assert_eq!(m.place("x").unwrap().name, "X2");
        }
    }
}
