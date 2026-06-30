//! Pure data model for the weather app.
//!
//! Everything here is plain serde data with no IO and no rendering. The
//! authoritative copy of this lives in the Automerge CRDT (see `doc.rs`);
//! these structs are the in-memory projection the widgets render from and
//! the shape the network layer (`api.rs`) deserialises into.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::units::{TempUnit, Units, WindUnit};

/// Stable id for a saved location.
pub type LocationId = Uuid;

/// A place the user has pinned. Coordinates + timezone drive fetches; the
/// label fields are for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLocation {
    pub id: LocationId,
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
    /// "14" style hour-of-day label parsed from the ISO time.
    pub fn hour_label(&self) -> String {
        // "YYYY-MM-DDTHH:MM" → "HH"
        self.time
            .get(11..13)
            .map(|h| h.to_string())
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
/// selected, the unit preferences, and the cached snapshots keyed by
/// location id. Mirrored into the CRDT by `doc.rs`.
#[derive(Clone, Debug, Default)]
pub struct WeatherModel {
    pub locations: Vec<SavedLocation>,
    pub selected: usize,
    pub units: Units,
    pub snapshots: std::collections::HashMap<LocationId, WeatherSnapshot>,
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
    pub fn add_location(&mut self, loc: SavedLocation) -> usize {
        if let Some(idx) = self.locations.iter().position(|l| {
            (l.latitude - loc.latitude).abs() < 0.05
                && (l.longitude - loc.longitude).abs() < 0.05
        }) {
            self.selected = idx;
            return idx;
        }
        self.locations.push(loc);
        let idx = self.locations.len() - 1;
        self.selected = idx;
        idx
    }

    /// Remove the location at `idx` (and its cached snapshot). Keeps the
    /// selection valid.
    pub fn remove_location(&mut self, idx: usize) {
        if idx >= self.locations.len() {
            return;
        }
        let removed = self.locations.remove(idx);
        self.snapshots.remove(&removed.id);
        if self.locations.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.locations.len() {
            self.selected = self.locations.len() - 1;
        }
    }

    /// Toggle temperature unit, keeping wind paired sensibly.
    pub fn toggle_temp_unit(&mut self) {
        self.units.temp = match self.units.temp {
            TempUnit::Celsius => TempUnit::Fahrenheit,
            TempUnit::Fahrenheit => TempUnit::Celsius,
        };
    }

    pub fn toggle_wind_unit(&mut self) {
        self.units.wind = match self.units.wind {
            WindUnit::Kmh => WindUnit::Mph,
            WindUnit::Mph => WindUnit::Kmh,
        };
    }

    /// Apply a whole-system preset (metric / imperial) in one shot.
    pub fn set_units(&mut self, units: Units) {
        self.units = units;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc() -> SavedLocation {
        SavedLocation::new("Test", "Region", "Country", "CC", 50.0, 8.0, "auto")
    }

    #[test]
    fn add_then_dedup_by_coords() {
        let mut m = WeatherModel::default();
        let i0 = m.add_location(loc());
        assert_eq!(i0, 0);
        let i1 = m.add_location(loc());
        assert_eq!(i1, 0, "near-identical coords must dedup");
        assert_eq!(m.locations.len(), 1);
    }

    #[test]
    fn select_relative_wraps() {
        let mut m = WeatherModel::default();
        m.locations.push(loc());
        m.locations.push(SavedLocation::new("B", "", "C", "CC", 10.0, 10.0, "auto"));
        m.selected = 0;
        m.select_relative(-1);
        assert_eq!(m.selected, 1, "wrap to last");
        m.select_relative(1);
        assert_eq!(m.selected, 0, "wrap to first");
    }

    #[test]
    fn remove_keeps_selection_valid() {
        let mut m = WeatherModel::default();
        m.locations.push(loc());
        m.locations.push(SavedLocation::new("B", "", "C", "CC", 10.0, 10.0, "auto"));
        m.selected = 1;
        m.remove_location(1);
        assert_eq!(m.selected, 0);
        assert_eq!(m.locations.len(), 1);
    }
}
