//! Pure data model for the weather app.
//!
//! Everything here is plain data with no IO and no rendering. What is
//! shared is the [`Weather`] record: the saved places, in order, and the
//! default one. Provider responses and presentation state stay local. The
//! other structs are the in-memory projection the widgets render from and
//! the shape the network layer (`api.rs`) deserialises into.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Atom, Collection, Id, Model, Refusal};
use uuid::Uuid;

use crate::units::Units;

/// Stable id for a saved location.
pub type LocationId = Uuid;

/// A saved place as the document shares it. Its item id is the place's
/// uuid and its position in [`Weather::places`] is its place in the list.
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Place::valid)]
pub struct Place {
    /// Name
    #[model(label)]
    pub name: String,
    /// Region
    pub region: String,
    /// Country
    pub country: String,
    /// ISO country code
    pub country_code: Atom,
    /// Latitude
    pub latitude: f64,
    /// Longitude
    pub longitude: f64,
    /// IANA timezone, or `auto`.
    pub timezone: Atom,
}

impl Place {
    /// The shared part of `location`.
    pub fn of(location: &SavedLocation) -> Self {
        Self {
            name: location.name.clone(),
            region: location.region.clone(),
            country: location.country.clone(),
            country_code: Atom(location.country_code.clone()),
            latitude: location.latitude,
            longitude: location.longitude,
            timezone: Atom(location.timezone.clone()),
        }
    }

    /// A place has a name and coordinates on the globe.
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() {
            return Err(Refusal::new("a place has a name"));
        }
        if !(-90.0..=90.0).contains(&self.latitude) || !(-180.0..=180.0).contains(&self.longitude) {
            return Err(Refusal::new(
                "a place lies between latitudes -90 and 90 and longitudes -180 and 180",
            ));
        }
        Ok(())
    }

    /// Whether `other` is the same place: within about five kilometres.
    pub fn near(&self, latitude: f64, longitude: f64) -> bool {
        (self.latitude - latitude).abs() < 0.05 && (self.longitude - longitude).abs() < 0.05
    }
}

/// The saved places a person follows, in order, and the one shown first.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Weather {
    pub places: Collection<Place>,
    /// The id of the default place; empty for the first.
    pub default_place: Atom,
}

impl Weather {
    /// The item id of the place with uuid `id`.
    pub fn item(id: LocationId) -> Id {
        Id::from_raw(id.to_string())
    }

    /// The places as the window shows them, the default one selected.
    pub fn model(&self) -> WeatherModel {
        let locations: Vec<SavedLocation> = self
            .places
            .iter()
            .enumerate()
            .filter_map(|(index, (id, place))| {
                Some(SavedLocation {
                    id: id.as_str().parse().ok()?,
                    order_rank: (index + 1) as f64 * 1024.0,
                    name: place.name.clone(),
                    region: place.region.clone(),
                    country: place.country.clone(),
                    country_code: place.country_code.0.clone(),
                    latitude: place.latitude,
                    longitude: place.longitude,
                    timezone: place.timezone.0.clone(),
                })
            })
            .collect();
        let selected = locations
            .iter()
            .position(|location| location.id.to_string() == self.default_place.0)
            .unwrap_or_default();
        WeatherModel {
            locations,
            selected,
            ..WeatherModel::default()
        }
    }
}

/// A place the user has pinned. Coordinates + timezone drive fetches; the
/// label fields are for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLocation {
    pub id: LocationId,
    /// Sparse, absolute ordering rank persisted with this stable location.
    /// It is deliberately independent per entry so concurrent moves of
    /// different locations merge instead of replacing one shared list.
    #[serde(default)]
    pub order_rank: f64,
    pub name: String,
    /// Region / admin area (e.g. "California", "Bavaria"). May be empty.
    pub region: String,
    pub country: String,
    pub country_code: String,
    pub latitude: f64,
    pub longitude: f64,
    /// IANA timezone string ("auto" when unknown — resolved server-side).
    pub timezone: String,
}

impl SavedLocation {
    pub fn new(
        name: impl Into<String>,
        region: impl Into<String>,
        country: impl Into<String>,
        country_code: impl Into<String>,
        latitude: f64,
        longitude: f64,
        timezone: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            order_rank: 0.0,
            name: name.into(),
            region: region.into(),
            country: country.into(),
            country_code: country_code.into(),
            latitude,
            longitude,
            timezone: timezone.into(),
        }
    }

    /// "Name, Region" or "Name, Country" subtitle for the header.
    pub fn subtitle(&self) -> String {
        if !self.region.is_empty() && self.region != self.name {
            format!("{}, {}", self.region, self.country)
        } else {
            self.country.clone()
        }
    }
}

/// Present-moment conditions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurrentConditions {
    pub temperature: f64,
    pub apparent: f64,
    pub humidity: f64,
    pub precipitation: f64,
    pub weather_code: i64,
    pub wind_speed: f64,
    pub wind_direction: f64,
    pub pressure: f64,
    pub uv_index: f64,
    pub is_day: bool,
    /// Local time of the observation, ISO8601 "YYYY-MM-DDTHH:MM".
    pub time: String,
}

/// One point on the 24h hourly strip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HourPoint {
    /// ISO8601 "YYYY-MM-DDTHH:MM".
    pub time: String,
    pub temperature: f64,
    pub precip_probability: f64,
    pub weather_code: i64,
    pub is_day: bool,
}

impl HourPoint {
    /// The hour this point is for, in the person's clock ("14:00" or
    /// "2:00 PM"), parsed from the ISO time "YYYY-MM-DDTHH:MM".
    pub fn hour_label(&self) -> String {
        self.time
            .get(11..13)
            .and_then(|hour| hour.parse::<u32>().ok())
            .and_then(|hour| chrono::NaiveTime::from_hms_opt(hour, 0, 0))
            .map(|time| ui::format::time(&time))
            .unwrap_or_default()
    }
}

/// One day of the 7-day forecast.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayForecast {
    /// ISO8601 date "YYYY-MM-DD".
    pub date: String,
    pub temp_max: f64,
    pub temp_min: f64,
    pub precip_probability: f64,
    pub weather_code: i64,
    pub wind_max: f64,
    pub uv_index_max: f64,
    /// ISO8601 sunrise/sunset "YYYY-MM-DDTHH:MM" (local).
    pub sunrise: String,
    pub sunset: String,
}

/// An air-quality reading for one location, from Open-Meteo's air-quality
/// API. AQI values are unitless indices; the pollutant concentrations are in
/// µg/m³. Every field is optional so a partial or missing response degrades to
/// an honest "unavailable" rather than a fabricated zero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AirQuality {
    pub location_id: LocationId,
    /// European AQI (0–20 good … >100 extremely poor).
    pub european_aqi: Option<f64>,
    /// US AQI (0–500).
    pub us_aqi: Option<f64>,
    /// Fine particulate matter, µg/m³.
    pub pm2_5: Option<f64>,
    /// Coarse particulate matter, µg/m³.
    pub pm10: Option<f64>,
    /// Nitrogen dioxide, µg/m³.
    pub nitrogen_dioxide: Option<f64>,
    /// Ground-level ozone, µg/m³.
    pub ozone: Option<f64>,
    /// Local time of the reading, ISO8601 "YYYY-MM-DDTHH:MM".
    pub time: String,
    /// Wall-clock epoch seconds when this reading was fetched (0 = sample).
    pub fetched_at: u64,
}

impl AirQuality {
    /// Whether any index or pollutant value is present — false means the
    /// section shows its honest "unavailable" state rather than empty tiles.
    pub fn has_any(&self) -> bool {
        self.european_aqi.is_some()
            || self.us_aqi.is_some()
            || self.pm2_5.is_some()
            || self.pm10.is_some()
            || self.nitrogen_dioxide.is_some()
            || self.ozone.is_some()
    }
}

/// A complete fetched snapshot for one location.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeatherSnapshot {
    pub location_id: LocationId,
    pub current: CurrentConditions,
    pub hourly: Vec<HourPoint>,
    pub daily: Vec<DayForecast>,
    /// Units the numeric values in THIS snapshot are expressed in. The UI
    /// converts to the user's preference at render time so a units toggle
    /// is instant and never needs a refetch.
    pub source_units: Units,
    /// Wall-clock epoch seconds when this snapshot was fetched (0 = sample).
    pub fetched_at: u64,
}

impl WeatherSnapshot {
    /// Temperature value converted into the user's preferred unit.
    pub fn temp_in(&self, value: f64, units: Units) -> f64 {
        crate::units::convert_temp(value, self.source_units.temp, units.temp)
    }

    pub fn wind_in(&self, value: f64, units: Units) -> f64 {
        crate::units::convert_wind(value, self.source_units.wind, units.wind)
    }
}

/// The top-level application model: the saved locations, which one is
/// selected, the unit preferences, and the device-local snapshots keyed by
/// location id. Only saved-location decisions are mirrored by `doc.rs`.
#[derive(Clone, Debug, Default)]
pub struct WeatherModel {
    pub locations: Vec<SavedLocation>,
    pub selected: usize,
    pub units: Units,
    pub snapshots: std::collections::HashMap<LocationId, WeatherSnapshot>,
    /// Latest air-quality reading per place (fetched from a separate endpoint,
    /// so it never races the forecast snapshot).
    pub air_quality: std::collections::HashMap<LocationId, AirQuality>,
}

impl WeatherModel {
    pub fn selected_location(&self) -> Option<&SavedLocation> {
        self.locations.get(self.selected)
    }

    pub fn selected_snapshot(&self) -> Option<&WeatherSnapshot> {
        let loc = self.selected_location()?;
        self.snapshots.get(&loc.id)
    }

    pub fn snapshot_for(&self, id: LocationId) -> Option<&WeatherSnapshot> {
        self.snapshots.get(&id)
    }

    pub fn air_quality_for(&self, id: LocationId) -> Option<&AirQuality> {
        self.air_quality.get(&id)
    }

    pub fn selected_air_quality(&self) -> Option<&AirQuality> {
        let loc = self.selected_location()?;
        self.air_quality.get(&loc.id)
    }

    /// Move selection by `delta`, clamped to the location list.
    pub fn select_relative(&mut self, delta: i64) {
        if self.locations.is_empty() {
            return;
        }
        let n = self.locations.len() as i64;
        let next = (self.selected as i64 + delta).rem_euclid(n);
        self.selected = next as usize;
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.locations.len() {
            self.selected = idx;
        }
    }

    /// Add a location if it isn't already pinned (matched by rounded
    /// coordinates). Returns the index of the (existing or new) entry and
    /// selects it.
    pub fn add_location(&mut self, mut loc: SavedLocation) -> usize {
        if let Some(idx) = self.locations.iter().position(|l| {
            (l.latitude - loc.latitude).abs() < 0.05 && (l.longitude - loc.longitude).abs() < 0.05
        }) {
            self.selected = idx;
            return idx;
        }
        loc.order_rank = self
            .locations
            .last()
            .map_or(1024.0, |location| location.order_rank + 1024.0);
        self.locations.push(loc);
        let idx = self.locations.len() - 1;
        self.selected = idx;
        idx
    }

    /// Remove the location at `idx` (and its cached snapshot). Keeps the
    /// selection valid by tracking the *identity* of the selected place,
    /// not its index — so deleting an entry above the selection doesn't
    /// silently shift which place is in view.
    pub fn remove_location(&mut self, idx: usize) {
        if idx >= self.locations.len() {
            return;
        }
        let selected_id = self.selected_location().map(|l| l.id);
        let removed = self.locations.remove(idx);
        self.snapshots.remove(&removed.id);
        self.air_quality.remove(&removed.id);
        self.reselect_by_id(selected_id);
    }

    /// Move the location at `from` to sit at `to`, shifting the rest. The
    /// selection follows the *place* it was on, not the slot index, so a
    /// reorder never changes which forecast is shown. No-op for invalid or
    /// equal indices. Returns true if the order actually changed.
    pub fn move_location(&mut self, from: usize, to: usize) -> bool {
        let n = self.locations.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let selected_id = self.selected_location().map(|l| l.id);
        let moved = self.locations.remove(from);
        self.locations.insert(to, moved);
        let previous = to
            .checked_sub(1)
            .and_then(|index| self.locations.get(index))
            .map(|location| location.order_rank);
        let next = self
            .locations
            .get(to.saturating_add(1))
            .map(|location| location.order_rank);
        let rank = match (previous, next) {
            (Some(previous), Some(next)) => f64::midpoint(previous, next),
            (Some(previous), None) => previous + 1024.0,
            (None, Some(next)) => next - 1024.0,
            (None, None) => 1024.0,
        };
        if let Some(location) = self.locations.get_mut(to) {
            location.order_rank = rank;
        }
        self.reselect_by_id(selected_id);
        true
    }

    /// Restore `selected` to point at the place with `id` after the list
    /// was mutated, clamping if it's gone.
    fn reselect_by_id(&mut self, id: Option<LocationId>) {
        if let Some(id) = id
            && let Some(pos) = self.locations.iter().position(|l| l.id == id)
        {
            self.selected = pos;
            return;
        }
        if self.locations.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.locations.len() {
            self.selected = self.locations.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc() -> SavedLocation {
        SavedLocation::new("Test", "Region", "Country", "CC", 50.0, 8.0, "auto")
    }

    #[test]
    fn locations_add_dedup_and_select_relative() {
        // Case `add_then_dedup_by_coords`.
        {
            let mut m = WeatherModel::default();
            let i0 = m.add_location(loc());
            assert_eq!(i0, 0);
            let i1 = m.add_location(loc());
            assert_eq!(i1, 0, "near-identical coords must dedup");
            assert_eq!(m.locations.len(), 1);
        }
        // Case `select_relative_wraps`.
        {
            let mut m = WeatherModel::default();
            m.locations.push(loc());
            m.locations
                .push(SavedLocation::new("B", "", "C", "CC", 10.0, 10.0, "auto"));
            m.selected = 0;
            m.select_relative(-1);
            assert_eq!(m.selected, 1, "wrap to last");
            m.select_relative(1);
            assert_eq!(m.selected, 0, "wrap to first");
        }
    }

    #[test]
    fn location_removal_and_moves_keep_selection_on_place() {
        // Case `remove_keeps_selection_valid`.
        {
            let mut m = WeatherModel::default();
            m.locations.push(loc());
            m.locations
                .push(SavedLocation::new("B", "", "C", "CC", 10.0, 10.0, "auto"));
            m.selected = 1;
            m.remove_location(1);
            assert_eq!(m.selected, 0);
            assert_eq!(m.locations.len(), 1);
        }
        // Case `remove_above_selection_keeps_same_place_in_view`.
        {
            let mut m = WeatherModel::default();
            let a = SavedLocation::new("A", "", "C", "CC", 1.0, 1.0, "auto");
            let b = SavedLocation::new("B", "", "C", "CC", 2.0, 2.0, "auto");
            let c = SavedLocation::new("C", "", "C", "CC", 3.0, 3.0, "auto");
            let c_id = c.id;
            m.locations = vec![a, b, c];
            m.selected = 2; // "C" is in view.
            m.remove_location(0); // delete "A" above it.
            assert_eq!(
                m.selected_location().unwrap().id,
                c_id,
                "selection tracks the place, not the slot"
            );
        }
        // Case `move_location_reorders_and_keeps_selection_on_place`.
        {
            let mut m = WeatherModel::default();
            let a = SavedLocation::new("A", "", "C", "CC", 1.0, 1.0, "auto");
            let b = SavedLocation::new("B", "", "C", "CC", 2.0, 2.0, "auto");
            let c = SavedLocation::new("C", "", "C", "CC", 3.0, 3.0, "auto");
            let (a_id, b_id, c_id) = (a.id, b.id, c.id);
            m.locations = vec![a, b, c];
            m.selected = 0; // "A" in view.
            // Drag "A" (idx 0) to the end (idx 2): order becomes B, C, A.
            assert!(m.move_location(0, 2));
            assert_eq!(m.locations[0].id, b_id, "B shifted to the front");
            assert_eq!(m.locations[1].id, c_id, "C shifted into the middle");
            assert_eq!(m.locations[2].id, a_id, "A landed at the end");
            assert_eq!(m.selected_location().unwrap().id, a_id, "view still on A");
            // Moving C to the front.
            let c_pos = m.locations.iter().position(|l| l.id == c_id).unwrap();
            assert!(m.move_location(c_pos, 0));
            assert_eq!(m.locations[0].id, c_id);
        }
        // Case `move_location_rejects_out_of_range`.
        {
            let mut m = WeatherModel::default();
            m.locations.push(loc());
            assert!(!m.move_location(0, 5));
            assert!(!m.move_location(0, 0));
        }
    }
}
