//! Automerge CRDT projection of the weather app's state.
//!
//! What lives in the shared document:
//!   * `locations` (List of Maps) — the user's saved places (structured
//!     CRDT fields so two peers adding places converge).
//!   * `selected`  (Int)          — which place is in view.
//!   * `units`     (Map)          — temperature + wind unit preference.
//!   * `snapshots` (Map id→Str)   — the last-fetched forecast per place,
//!     stored as an opaque JSON blob (derived/cache data, not something
//!     peers co-edit field-by-field).
//!
//! The widget reads the model out of the doc each sync and writes it back
//! on mutation (add/remove location, switch place, toggle units, new
//! fetch). Writes are coarse but infrequent — this is a preferences-and-
//! cache document, not a hot collaborative text buffer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{AirQuality, SavedLocation, WeatherModel, WeatherSnapshot};
use crate::units::{TempUnit, Units, WindUnit};

pub struct WeatherDoc;

impl WeatherDoc {
    /// True when the document has been seeded with at least a `units`
    /// map — used to decide whether to write the initial sample model.
    pub fn is_initialised(doc: &AutoCommit) -> bool {
        matches!(
            doc.get(&ROOT, "units"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    /// Mirror the entire model into the document (idempotent overwrite).
    ///
    /// # Panics
    /// Never in practice: every `put_object`/`insert_object` call here
    /// targets `ROOT` or a Map/List this function just resolved, so it only
    /// fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT or a just-resolved Map/List, so put_object/insert_object cannot fail short of a corrupted document — falling back to another ObjId would silently write into the wrong object"
    )]
    pub fn write_model(doc: &mut AutoCommit, model: &WeatherModel) {
        // locations — replace the list wholesale (small, infrequent).
        let list = doc
            .put_object(&ROOT, "locations", ObjType::List)
            .expect("put locations list");
        for (i, loc) in model.locations.iter().enumerate() {
            let m = doc
                .insert_object(&list, i, ObjType::Map)
                .expect("insert location map");
            let _ = doc.put(&m, "id", loc.id.to_string());
            let _ = doc.put(&m, "name", loc.name.clone());
            let _ = doc.put(&m, "region", loc.region.clone());
            let _ = doc.put(&m, "country", loc.country.clone());
            let _ = doc.put(&m, "country_code", loc.country_code.clone());
            let _ = doc.put(&m, "lat", loc.latitude);
            let _ = doc.put(&m, "lon", loc.longitude);
            let _ = doc.put(&m, "tz", loc.timezone.clone());
        }

        let _ = doc.put(&ROOT, "selected", model.selected as i64);

        let units = match doc.get(&ROOT, "units") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "units", ObjType::Map)
                .expect("put units map"),
        };
        let _ = doc.put(
            &units,
            "temp",
            match model.units.temp {
                TempUnit::Celsius => "c",
                TempUnit::Fahrenheit => "f",
            },
        );
        let _ = doc.put(
            &units,
            "wind",
            match model.units.wind {
                WindUnit::Kmh => "kmh",
                WindUnit::Mph => "mph",
            },
        );

        // snapshots — JSON blobs keyed by location id.
        let snaps = match doc.get(&ROOT, "snapshots") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "snapshots", ObjType::Map)
                .expect("put snapshots map"),
        };
        for (id, snap) in &model.snapshots {
            if let Ok(json) = serde_json::to_string(snap) {
                let _ = doc.put(&snaps, id.to_string().as_str(), json);
            }
        }

        // air_quality — JSON blobs keyed by location id (same cache treatment).
        let aq = match doc.get(&ROOT, "air_quality") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "air_quality", ObjType::Map)
                .expect("put air_quality map"),
        };
        for (id, reading) in &model.air_quality {
            if let Ok(json) = serde_json::to_string(reading) {
                let _ = doc.put(&aq, id.to_string().as_str(), json);
            }
        }
    }

    /// Write a single freshly-fetched snapshot without touching the rest.
    ///
    /// # Panics
    /// Never in practice: the parent is `ROOT` or a Map this function just
    /// resolved, so `put_object` only fails for an already-corrupted
    /// document.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT or a just-resolved Map, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write the snapshot into the wrong object"
    )]
    pub fn put_snapshot(doc: &mut AutoCommit, snap: &WeatherSnapshot) {
        let snaps = match doc.get(&ROOT, "snapshots") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "snapshots", ObjType::Map)
                .expect("put snapshots map"),
        };
        if let Ok(json) = serde_json::to_string(snap) {
            let _ = doc.put(&snaps, snap.location_id.to_string().as_str(), json);
        }
    }

    fn str_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    fn f64_field(doc: &AutoCommit, obj: &ObjId, key: &str) -> f64 {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_f64().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Read the model back out of the document.
    pub fn read_model(doc: &AutoCommit) -> WeatherModel {
        let mut model = WeatherModel::default();

        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, "locations") {
            let len = doc.length(&list);
            for i in 0..len {
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                    continue;
                };
                let id = Self::str_field(doc, &m, "id")
                    .parse()
                    .unwrap_or_else(|_| uuid::Uuid::new_v4());
                model.locations.push(SavedLocation {
                    id,
                    name: Self::str_field(doc, &m, "name"),
                    region: Self::str_field(doc, &m, "region"),
                    country: Self::str_field(doc, &m, "country"),
                    country_code: Self::str_field(doc, &m, "country_code"),
                    latitude: Self::f64_field(doc, &m, "lat"),
                    longitude: Self::f64_field(doc, &m, "lon"),
                    timezone: Self::str_field(doc, &m, "tz"),
                });
            }
        }

        model.selected = match doc.get(&ROOT, "selected") {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0).max(0) as usize,
            _ => 0,
        };
        if model.selected >= model.locations.len() && !model.locations.is_empty() {
            model.selected = 0;
        }

        if let Ok(Some((Value::Object(ObjType::Map), units))) = doc.get(&ROOT, "units") {
            let temp = match Self::str_field(doc, &units, "temp").as_str() {
                "f" => TempUnit::Fahrenheit,
                _ => TempUnit::Celsius,
            };
            let wind = match Self::str_field(doc, &units, "wind").as_str() {
                "mph" => WindUnit::Mph,
                _ => WindUnit::Kmh,
            };
            model.units = Units { temp, wind };
        }

        if let Ok(Some((Value::Object(ObjType::Map), snaps))) = doc.get(&ROOT, "snapshots") {
            for key in doc.keys(&snaps) {
                let json = Self::str_field(doc, &snaps, &key);
                if let Ok(snap) = serde_json::from_str::<WeatherSnapshot>(&json) {
                    model.snapshots.insert(snap.location_id, snap);
                }
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), aq))) = doc.get(&ROOT, "air_quality") {
            for key in doc.keys(&aq) {
                let json = Self::str_field(doc, &aq, &key);
                if let Ok(reading) = serde_json::from_str::<AirQuality>(&json) {
                    model.air_quality.insert(reading.location_id, reading);
                }
            }
        }

        model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_locations_units_selected() {
        let mut model = crate::sample::sample_model();
        model.selected = 2;
        model.set_units(Units::IMPERIAL);

        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &model);
        assert!(WeatherDoc::is_initialised(&doc));

        let back = WeatherDoc::read_model(&doc);
        assert_eq!(back.locations.len(), model.locations.len());
        assert_eq!(back.selected, 2);
        assert_eq!(back.units, Units::IMPERIAL);
        // Snapshots survive as JSON blobs.
        let loc0 = &back.locations[0];
        assert!(back.snapshot_for(loc0.id).is_some());
    }

    #[test]
    fn put_snapshot_is_incremental() {
        let model = crate::sample::sample_model();
        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &model);
        let loc = &model.locations[1];
        let mut snap = model.snapshot_for(loc.id).unwrap().clone();
        snap.current.temperature = 99.0;
        WeatherDoc::put_snapshot(&mut doc, &snap);
        let back = WeatherDoc::read_model(&doc);
        assert_eq!(back.snapshot_for(loc.id).unwrap().current.temperature, 99.0);
    }
}
