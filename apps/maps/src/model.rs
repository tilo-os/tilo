//! Pure data model for the Maps app: what a window shows of the shared
//! [`crate::workspace::Workspace`] beside its per-window state. The offline areas are this device's
//! ([`crate::offline_areas`]).

use crate::failure;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Model};

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

/// Point-of-interest category: the marker colour, the card icon, and the OSM class it maps from.
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

    pub fn label(self) -> String {
        failure::t(match self {
            Category::Landmark => "maps-category-landmark",
            Category::Cafe => "maps-category-cafe",
            Category::Restaurant => "maps-category-restaurant",
            Category::Bar => "maps-category-bar",
            Category::Museum => "maps-category-museum",
            Category::Park => "maps-category-park",
            Category::Transit => "maps-category-transit",
            Category::Hotel => "maps-category-hotel",
            Category::Shop => "maps-category-shop",
            Category::Hospital => "maps-category-hospital",
            Category::Other => "maps-category-other",
        })
    }

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

/// A point of interest. `id` is stable: markers, the place card and the list all reference it.
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

/// A POI from Overpass for the current viewport, drawn as our own themed marker layer: ephemeral
/// view data that carries the OSM tags the card shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Poi {
    /// Stable OSM identity, e.g. `osm-node-12345`.
    pub id: String,
    pub name: String,
    pub category: Category,
    pub pos: LatLon,
    pub address: String,
    pub opening_hours: Option<String>,
    pub website: Option<String>,
    pub phone: Option<String>,
    pub kind: Option<String>,
}

impl Poi {
    pub fn kind_label(&self) -> String {
        let Some(kind) = self.kind.as_deref().filter(|k| !k.trim().is_empty()) else {
            return self.category.label();
        };
        ui::format::capitalize_first(&kind.trim().replace(['_', ';'], " "))
    }
}

/// Travel mode. `Car`/`Bike`/`Foot` route on roads (OSRM); `Transit` plans a journey (MOTIS) inside
/// Directions.
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

    pub fn label(self) -> String {
        failure::t(match self {
            TravelMode::Car => "maps-mode-drive",
            TravelMode::Transit => "maps-mode-transit",
            TravelMode::Bike => "maps-mode-bike",
            TravelMode::Foot => "maps-mode-foot",
        })
    }

    /// The mode's stable name, as its serialized value spells it; never translated. Never
    /// translated: it is an identity, not a label.
    pub const fn id(self) -> &'static str {
        match self {
            TravelMode::Car => "car",
            TravelMode::Transit => "transit",
            TravelMode::Bike => "bike",
            TravelMode::Foot => "foot",
        }
    }

    /// Whether the mode plans a public-transport journey; callers route through OSRM only for the
    /// road modes.
    pub fn is_transit(self) -> bool {
        matches!(self, TravelMode::Transit)
    }

    /// OSRM's profile path component; `Transit` never routes through OSRM (see
    /// [`Self::is_transit`]).
    /// OSRM public-server profile path component. Only meaningful for the road
    /// modes; `Transit` never routes via OSRM (guarded by `is_transit`).
    pub fn osrm_profile(self) -> &'static str {
        match self {
            // `Transit` shares Car's profile only because the return type needs a value; it never
            // routes through OSRM.
            TravelMode::Car | TravelMode::Transit => "driving",
            TravelMode::Bike => "cycling",
            TravelMode::Foot => "foot",
        }
    }

    /// Plausible average speed for the offline road-route fallback; unused for `Transit`.
    pub fn speed_kmh(self) -> f64 {
        match self {
            TravelMode::Car => 32.0,
            TravelMode::Bike => 15.0,
            TravelMode::Foot => 4.8,
            TravelMode::Transit => 20.0,
        }
    }
}

/// Turn type for a routing step, from OSRM's `maneuver.modifier`.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteStep {
    pub instruction: String,
    pub distance_m: f64,
    pub maneuver: Maneuver,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Route {
    pub mode: TravelMode,
    pub distance_m: f64,
    pub duration_s: f64,
    pub geometry: Vec<LatLon>,
    pub steps: Vec<RouteStep>,
    pub from: LatLon,
    pub to: LatLon,
}

impl Route {
    pub fn is_same_trip(&self, other: &Route) -> bool {
        (self.duration_s - other.duration_s).abs() < 0.5
            && (self.distance_m - other.distance_m).abs() < 0.5
    }
}

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
    pub fn label(self) -> String {
        failure::t(match self {
            TransitMode::Walk => "maps-transit-walk",
            TransitMode::Bus => "maps-transit-bus",
            TransitMode::Tram => "maps-transit-tram",
            TransitMode::Subway => "maps-transit-subway",
            TransitMode::Rail => "maps-transit-rail",
            TransitMode::Ferry => "maps-transit-ferry",
        })
    }

    pub fn from_motis(mode: &str) -> TransitMode {
        match mode.to_ascii_uppercase().as_str() {
            "WALK" => TransitMode::Walk,
            "TRAM" | "STREETCAR" | "LIGHT_RAIL" => TransitMode::Tram,
            "SUBWAY" | "METRO" => TransitMode::Subway,
            "RAIL" | "REGIONAL_RAIL" | "REGIONAL_FAST_RAIL" | "LONG_DISTANCE"
            | "HIGHSPEED_RAIL" | "NIGHT_RAIL" => TransitMode::Rail,
            "FERRY" => TransitMode::Ferry,
            _ => TransitMode::Bus,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitLeg {
    pub mode: TransitMode,
    pub line: String,
    pub headsign: String,
    pub from_name: String,
    pub to_name: String,
    pub dep: String,
    pub arr: String,
    pub duration_min: u32,
    pub num_stops: u32,
    pub geometry: Vec<LatLon>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub text_color: Option<String>,
    #[serde(default)]
    pub agency: String,
    /// Whether this leg's times are real-time rather than schedule only; the card shows it as a
    /// live indicator.
    #[serde(default)]
    pub realtime: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitJourney {
    pub dep: String,
    pub arr: String,
    pub duration_min: u32,
    pub transfers: u32,
    pub legs: Vec<TransitLeg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TransitTimeMode {
    Depart,
    Arrive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransitTime {
    pub mode: TransitTimeMode,
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
    pub fn is_now(&self) -> bool {
        self.mode == TransitTimeMode::Depart && self.offset_min == 0
    }

    pub fn arrive_by(&self) -> bool {
        self.mode == TransitTimeMode::Arrive
    }
}

/// Which view the panel shows: `Explore`, or `Directions` reached from a place card, with a back
/// control returning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Panel {
    Explore,
    Directions,
}

impl Panel {
    pub const ALL: [Panel; 2] = [Panel::Explore, Panel::Directions];

    pub fn label(self) -> String {
        failure::t(match self {
            Panel::Explore => "maps-panel-explore",
            Panel::Directions => "maps-panel-directions",
        })
    }
}

/// A map area downloaded for offline use: the tiles live in the on-disk cache, and this is
/// what the offline panel lists and a delete targets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DownloadedRegion {
    /// Stable id (a monotonic timestamp) so the list can target a row.
    pub id: String,
    pub name: String,
    pub min: LatLon,
    pub max: LatLon,
    pub z0: u8,
    pub z1: u8,
    pub tiles: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapModel {
    pub places: Vec<Place>,
    pub recents: crate::workspace::RecentSearches,
    pub selected: Option<String>,
    pub route: Option<Route>,
    pub route_alternatives: Vec<Route>,
    pub transit: Vec<TransitJourney>,
    pub transit_choice: usize,
    pub panel: Panel,
    pub mode: TravelMode,
    pub query: String,
    #[serde(default)]
    pub origin_query: String,
    #[serde(default)]
    pub dest_query: String,
    #[serde(default)]
    pub transit_time: TransitTime,
    pub results: Vec<Place>,
    pub origin: Option<LatLon>,
    pub origin_name: String,
    #[serde(default)]
    pub dest: Option<LatLon>,
    #[serde(default)]
    pub dest_name: String,
    pub center: LatLon,
    pub zoom: f64,
    pub status: String,
    /// Map areas this device saved for offline use: its device-local setting, with the tiles in the
    /// on-disk cache.
    #[serde(default)]
    pub regions: Vec<DownloadedRegion>,
}

impl Default for MapModel {
    fn default() -> Self {
        Self {
            places: Vec::new(),
            recents: crate::workspace::RecentSearches::default(),
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
            origin_name: failure::t("maps-map-centre"),
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

    pub fn toggle_saved(&mut self, id: &str) -> bool {
        if let Some(p) = self.places.iter_mut().find(|p| p.id == id) {
            p.saved = !p.saved;
            p.saved
        } else {
            false
        }
    }

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
    fn saved_places_dedupe_and_keep_flags() {
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
