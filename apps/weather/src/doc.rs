//! Automerge CRDT projection of the weather app's state.
//!
//! What lives in the shared document:
//!   * `saved_locations` (Map id→Map) — the user's saved places.  Every
//!     location has a stable UUID key, so independently-added places merge
//!     instead of replacing one shared JSON/list value.
//!   * `location_order` (List of ids) — presentation order for those places.
//!   * `selected_location` (Str) — the selected stable location id.
//!   * `snapshots` (Map id→Str)   — the last-fetched forecast per place,
//!     stored as an opaque JSON blob (derived/cache data, not something
//!     peers co-edit field-by-field).
//!
//! Region-derived measurement units are intentionally **not** in this
//! document. They come from the local Settings/region preference, and never
//! become a surprising cross-device collaborative choice. Forecast responses
//! are external cache effects; only saved-location decisions are replayable
//! actions.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{AirQuality, SavedLocation, WeatherModel, WeatherSnapshot};

pub struct WeatherDoc;

impl WeatherDoc {
    /// True once either the current keyed schema or the old list schema has
    /// been seeded. The old schema is read and migrated lazily on the next
    /// location action; it is never discarded in place.
    pub fn is_initialised(doc: &AutoCommit) -> bool {
        matches!(
            doc.get(&ROOT, "saved_locations"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) || matches!(
            doc.get(&ROOT, "locations"),
            Ok(Some((Value::Object(ObjType::List), _)))
        )
    }

    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge map/list cannot fail under normal operation"
    )]
    fn map_field(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, key, ObjType::Map).expect("root map"),
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge list cannot fail under normal operation"
    )]
    fn list_field(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&ROOT, key, ObjType::List)
                .expect("root list"),
        }
    }

    fn location_from_map(doc: &AutoCommit, m: &ObjId, fallback_id: &str) -> Option<SavedLocation> {
        let id = Self::str_field(doc, m, "id")
            .parse()
            .or_else(|_| fallback_id.parse())
            .ok()?;
        Some(SavedLocation {
            id,
            name: Self::str_field(doc, m, "name"),
            region: Self::str_field(doc, m, "region"),
            country: Self::str_field(doc, m, "country"),
            country_code: Self::str_field(doc, m, "country_code"),
            latitude: Self::f64_field(doc, m, "lat"),
            longitude: Self::f64_field(doc, m, "lon"),
            timezone: Self::str_field(doc, m, "tz"),
        })
    }

    /// Copy the legacy ordered list into the keyed map once. Keeping the
    /// legacy value makes this safe for a peer that has not upgraded yet.
    pub(crate) fn migrate_legacy_locations(doc: &mut AutoCommit) {
        if matches!(
            doc.get(&ROOT, "saved_locations"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) {
            return;
        }
        let legacy = Self::read_legacy_locations(doc);
        if legacy.is_empty() {
            return;
        }
        for location in &legacy {
            Self::upsert_location(doc, location);
        }
        Self::set_location_order(
            doc,
            &legacy
                .iter()
                .map(|location| location.id)
                .collect::<Vec<_>>(),
        );
        let selected = match doc.get(&ROOT, "selected") {
            Ok(Some((Value::Scalar(value), _))) => {
                legacy.get(value.to_i64().unwrap_or(0).max(0) as usize)
            }
            _ => legacy.first(),
        };
        if let Some(selected) = selected {
            Self::select_location(doc, selected.id);
        }
    }

    /// Upsert a saved location as independent CRDT fields under its stable id.
    /// This is the only writer used by the replayable add action.
    ///
    /// # Panics
    ///
    /// Only if Automerge cannot create a child map in an otherwise valid root
    /// map, which indicates a corrupted document rather than invalid user data.
    #[allow(
        clippy::expect_used,
        reason = "a child map under the root map can only fail for a corrupted document"
    )]
    pub fn upsert_location(doc: &mut AutoCommit, location: &SavedLocation) {
        let locations = Self::map_field(doc, "saved_locations");
        let id = location.id.to_string();
        let entry = match doc.get(&locations, id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), entry))) => entry,
            _ => doc
                .put_object(&locations, id.as_str(), ObjType::Map)
                .expect("location map"),
        };
        ui::doc::put_if_changed(doc, &entry, "id", id.as_str());
        ui::doc::put_if_changed(doc, &entry, "name", location.name.as_str());
        ui::doc::put_if_changed(doc, &entry, "region", location.region.as_str());
        ui::doc::put_if_changed(doc, &entry, "country", location.country.as_str());
        ui::doc::put_if_changed(doc, &entry, "country_code", location.country_code.as_str());
        ui::doc::put_if_changed(doc, &entry, "lat", location.latitude);
        ui::doc::put_if_changed(doc, &entry, "lon", location.longitude);
        ui::doc::put_if_changed(doc, &entry, "tz", location.timezone.as_str());
        Self::append_location_order(doc, location.id);
    }

    /// Delete just one keyed location and its order references.
    pub fn remove_location(doc: &mut AutoCommit, id: crate::model::LocationId) {
        let key = id.to_string();
        if let Ok(Some((Value::Object(ObjType::Map), locations))) =
            doc.get(&ROOT, "saved_locations")
        {
            let _ = doc.delete(&locations, key.as_str());
        }
        if let Ok(Some((Value::Object(ObjType::List), order))) = doc.get(&ROOT, "location_order") {
            for index in (0..doc.length(&order)).rev() {
                if Self::str_at(doc, &order, index).as_deref() == Some(key.as_str()) {
                    let _ = doc.delete(&order, index);
                }
            }
        }
    }

    pub fn select_location(doc: &mut AutoCommit, id: crate::model::LocationId) {
        ui::doc::put_if_changed(doc, &ROOT, "selected_location", id.to_string().as_str());
    }

    fn str_at(doc: &AutoCommit, list: &ObjId, index: usize) -> Option<String> {
        match doc.get(list, index).ok().flatten().map(|(value, _)| value) {
            Some(Value::Scalar(value)) => match value.as_ref() {
                ScalarValue::Str(value) => Some(value.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn append_location_order(doc: &mut AutoCommit, id: crate::model::LocationId) {
        let order = Self::list_field(doc, "location_order");
        let id = id.to_string();
        if !(0..doc.length(&order))
            .any(|index| Self::str_at(doc, &order, index).as_deref() == Some(id.as_str()))
        {
            let _ = doc.insert(&order, doc.length(&order), id);
        }
    }

    /// Replace only the ordering list. Individual location records remain
    /// independent, so a concurrent add cannot be lost to a reorder.
    pub fn set_location_order(doc: &mut AutoCommit, ids: &[crate::model::LocationId]) {
        let order = Self::list_field(doc, "location_order");
        for index in (0..doc.length(&order)).rev() {
            let _ = doc.delete(&order, index);
        }
        for (index, id) in ids.iter().enumerate() {
            let _ = doc.insert(&order, index, id.to_string());
        }
    }

    /// Seed a model into the current keyed schema. Subsequent user edits use
    /// the narrow writers above; cache-only writes never need to replace saved
    /// locations or local display units.
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
        Self::migrate_legacy_locations(doc);
        for location in &model.locations {
            Self::upsert_location(doc, location);
        }
        Self::set_location_order(
            doc,
            &model
                .locations
                .iter()
                .map(|location| location.id)
                .collect::<Vec<_>>(),
        );
        if let Some(location) = model.selected_location() {
            Self::select_location(doc, location.id);
        }

        Self::write_cache(doc, model);
    }

    /// Persist network-derived forecast cache data. This deliberately avoids
    /// touching user-owned saved locations, ordering, selection, or local
    /// regional unit preferences.
    ///
    /// # Panics
    ///
    /// Only if Automerge cannot create the root cache maps, indicating a
    /// corrupted document rather than an unavailable weather provider.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT or a just-resolved Map/List, so put_object/insert_object cannot fail short of a corrupted document — falling back to another ObjId would silently write into the wrong object"
    )]
    pub fn write_cache(doc: &mut AutoCommit, model: &WeatherModel) {
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

    fn read_legacy_locations(doc: &AutoCommit) -> Vec<SavedLocation> {
        let mut locations = Vec::new();
        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, "locations") {
            for index in 0..doc.length(&list) {
                let Ok(Some((Value::Object(ObjType::Map), location))) = doc.get(&list, index)
                else {
                    continue;
                };
                if let Some(location) = Self::location_from_map(doc, &location, "") {
                    locations.push(location);
                }
            }
        }
        locations
    }

    /// Read the model back out of the document.
    pub fn read_model(doc: &AutoCommit) -> WeatherModel {
        let mut model = WeatherModel::default();

        if let Ok(Some((Value::Object(ObjType::Map), locations))) =
            doc.get(&ROOT, "saved_locations")
        {
            let mut remaining: Vec<String> = doc.keys(&locations).collect();
            if let Ok(Some((Value::Object(ObjType::List), order))) =
                doc.get(&ROOT, "location_order")
            {
                for index in 0..doc.length(&order) {
                    let Some(id) = Self::str_at(doc, &order, index) else {
                        continue;
                    };
                    let Ok(Some((Value::Object(ObjType::Map), location))) =
                        doc.get(&locations, id.as_str())
                    else {
                        continue;
                    };
                    if let Some(location) = Self::location_from_map(doc, &location, &id) {
                        remaining.retain(|candidate| candidate != &id);
                        model.locations.push(location);
                    }
                }
            }
            remaining.sort_unstable();
            for id in remaining {
                if let Ok(Some((Value::Object(ObjType::Map), location))) =
                    doc.get(&locations, id.as_str())
                    && let Some(location) = Self::location_from_map(doc, &location, &id)
                {
                    model.locations.push(location);
                }
            }
        } else {
            model.locations = Self::read_legacy_locations(doc);
        }

        let selected_id: Option<crate::model::LocationId> =
            Self::str_field(doc, &ROOT, "selected_location")
                .parse()
                .ok();
        model.selected = selected_id
            .and_then(|id| {
                model
                    .locations
                    .iter()
                    .position(|location| location.id == id)
            })
            .or_else(|| match doc.get(&ROOT, "selected") {
                Ok(Some((Value::Scalar(value), _))) => {
                    Some(value.to_i64().unwrap_or(0).max(0) as usize)
                }
                _ => None,
            })
            .unwrap_or(0);
        if model.selected >= model.locations.len() && !model.locations.is_empty() {
            model.selected = 0;
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
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::units::Units;

    #[test]
    fn round_trip_locations_and_selected_without_persisting_local_units() {
        let mut model = crate::sample::sample_model();
        model.selected = 2;
        model.set_units(Units::IMPERIAL);

        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &model);
        assert!(WeatherDoc::is_initialised(&doc));

        let back = WeatherDoc::read_model(&doc);
        assert_eq!(back.locations.len(), model.locations.len());
        assert_eq!(back.selected, 2);
        assert_eq!(
            back.units,
            Units::default(),
            "units are local Settings state"
        );
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

    #[test]
    fn legacy_location_list_is_read_then_migrated_to_keyed_records() {
        let model = crate::sample::sample_model();
        let mut legacy = AutoCommit::new();
        let list = legacy
            .put_object(&ROOT, "locations", ObjType::List)
            .unwrap();
        for (index, location) in model.locations.iter().enumerate() {
            let entry = legacy.insert_object(&list, index, ObjType::Map).unwrap();
            legacy.put(&entry, "id", location.id.to_string()).unwrap();
            legacy.put(&entry, "name", location.name.as_str()).unwrap();
            legacy
                .put(&entry, "region", location.region.as_str())
                .unwrap();
            legacy
                .put(&entry, "country", location.country.as_str())
                .unwrap();
            legacy
                .put(&entry, "country_code", location.country_code.as_str())
                .unwrap();
            legacy.put(&entry, "lat", location.latitude).unwrap();
            legacy.put(&entry, "lon", location.longitude).unwrap();
            legacy
                .put(&entry, "tz", location.timezone.as_str())
                .unwrap();
        }

        WeatherDoc::migrate_legacy_locations(&mut legacy);
        assert!(matches!(
            legacy.get(&ROOT, "saved_locations"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ));
        assert_eq!(
            WeatherDoc::read_model(&legacy).locations.len(),
            model.locations.len()
        );
    }
}
