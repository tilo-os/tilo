//! Automerge CRDT projection of the weather app's state.
//!
//! What lives in the shared document:
//!   * `saved_locations` (Map id→Map) — the user's saved places.  Every
//!     location has a stable UUID key, so independently-added places merge
//!     instead of replacing one shared JSON/list value.
//!   * `location_order` (List of ids) — presentation order for those places.
//!   * `selected_location` (Str) — the selected stable location id.
//!   * `location_tombstones` (Map id→Map) — permanent remove-wins tombstones
//!     for saved-place ids. This makes a concurrent remove deterministic and
//!     prevents an old peer's stale record from silently resurrecting a place.
//!
//! Region-derived measurement units, provider responses, search state and
//! presentation state are intentionally **not** in this document. They are
//! device-local/transient; only user-owned saved-location decisions are
//! replayable actions. In particular, the document contains no serialized
//! JSON records.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_f64_coerced, read_str};

use crate::model::{SavedLocation, WeatherModel};

pub struct WeatherDoc;

impl WeatherDoc {
    /// True once the keyed schema has been seeded.
    pub fn is_initialised(doc: &AutoCommit) -> bool {
        matches!(
            doc.get(&ROOT, "saved_locations"),
            Ok(Some((Value::Object(ObjType::Map), _)))
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

    /// Create the stable keyed containers through a replayable action. This
    /// is deliberately separate from widget synchronisation: durable document
    /// state is only ever introduced or mutated by `WeatherAction::apply`.
    pub fn ensure_schema(doc: &mut AutoCommit) {
        Self::map_field(doc, "saved_locations");
        Self::map_field(doc, "location_tombstones");
        Self::list_field(doc, "location_order");
    }

    fn is_tombstoned(doc: &AutoCommit, id: &str) -> bool {
        matches!(
            doc.get(&ROOT, "location_tombstones"),
            Ok(Some((Value::Object(ObjType::Map), tombstones)))
                if matches!(doc.get(&tombstones, id), Ok(Some((Value::Object(ObjType::Map), _))))
        )
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
        let id = read_str(doc, m, "id")
            .unwrap_or_default()
            .parse()
            .or_else(|_| fallback_id.parse())
            .ok()?;
        Some(SavedLocation {
            id,
            // An entry without a rank reads as NaN, so the reader places it;
            // zero is a rank like any other (a place moved above one ranked
            // 1024).
            order_rank: read_f64_coerced(doc, m, "order_rank").unwrap_or(f64::NAN),
            name: read_str(doc, m, "name").unwrap_or_default(),
            region: read_str(doc, m, "region").unwrap_or_default(),
            country: read_str(doc, m, "country").unwrap_or_default(),
            country_code: read_str(doc, m, "country_code").unwrap_or_default(),
            latitude: read_f64_coerced(doc, m, "lat").unwrap_or(0.0),
            longitude: read_f64_coerced(doc, m, "lon").unwrap_or(0.0),
            timezone: read_str(doc, m, "tz").unwrap_or_default(),
        })
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
        Self::ensure_schema(doc);
        let id = location.id.to_string();
        // UUIDs are generated for every user add, so a same-id re-add is not
        // a supported edit operation. Remove wins for that id forever, which
        // gives concurrent remove/add a deterministic, non-resurrecting
        // result on every peer.
        if Self::is_tombstoned(doc, &id) {
            return;
        }
        let locations = Self::map_field(doc, "saved_locations");
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
        let rank = if location.order_rank.is_finite() && location.order_rank != 0.0 {
            location.order_rank
        } else {
            Self::next_order_rank(doc)
        };
        ui::doc::put_if_changed(doc, &entry, "order_rank", rank);
        Self::append_location_order(doc, location.id);
    }

    /// Delete just one keyed location and its order references.
    ///
    /// # Panics
    ///
    /// Only if Automerge cannot create the tombstone child map under a valid
    /// root map, which indicates a corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "the tombstone map is created below a just-resolved root map; failure indicates a corrupted document"
    )]
    pub fn remove_location(doc: &mut AutoCommit, id: crate::model::LocationId) {
        let key = id.to_string();
        Self::ensure_schema(doc);
        let tombstones = Self::map_field(doc, "location_tombstones");
        if !matches!(
            doc.get(&tombstones, key.as_str()),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) {
            let tombstone = doc
                .put_object(&tombstones, key.as_str(), ObjType::Map)
                .expect("location tombstone map");
            ui::doc::put_if_changed(doc, &tombstone, "id", key.as_str());
        }
        if let Ok(Some((Value::Object(ObjType::Map), locations))) =
            doc.get(&ROOT, "saved_locations")
        {
            let _ = doc.delete(&locations, key.as_str());
        }
        if let Ok(Some((Value::Object(ObjType::List), order))) = doc.get(&ROOT, "location_order") {
            for index in (0..doc.length(&order)).rev() {
                if read_str(doc, &order, index).as_deref() == Some(key.as_str()) {
                    let _ = doc.delete(&order, index);
                }
            }
        }
    }

    pub fn select_location(doc: &mut AutoCommit, id: crate::model::LocationId) {
        ui::doc::put_if_changed(doc, &ROOT, "selected_location", id.to_string().as_str());
    }

    /// Set one entry's absolute sparse rank. Unlike replacing an order list,
    /// this operation touches only the moved stable resource, so unrelated
    /// concurrent moves remain independent Automerge changes.
    pub fn set_location_rank(doc: &mut AutoCommit, id: crate::model::LocationId, rank: f64) {
        if !rank.is_finite() || Self::is_tombstoned(doc, &id.to_string()) {
            return;
        }
        let Ok(Some((Value::Object(ObjType::Map), locations))) = doc.get(&ROOT, "saved_locations")
        else {
            return;
        };
        if let Ok(Some((Value::Object(ObjType::Map), location))) =
            doc.get(&locations, id.to_string())
        {
            ui::doc::put_if_changed(doc, &location, "order_rank", rank);
        }
    }

    fn next_order_rank(doc: &AutoCommit) -> f64 {
        let Ok(Some((Value::Object(ObjType::Map), locations))) = doc.get(&ROOT, "saved_locations")
        else {
            return 1024.0;
        };
        doc.keys(&locations)
            .filter_map(|id| match doc.get(&locations, id).ok().flatten() {
                Some((Value::Object(ObjType::Map), location)) => {
                    Some(read_f64_coerced(doc, &location, "order_rank").unwrap_or(0.0))
                }
                _ => None,
            })
            .filter(|rank| rank.is_finite())
            .fold(0.0, f64::max)
            + 1024.0
    }

    fn append_location_order(doc: &mut AutoCommit, id: crate::model::LocationId) {
        let order = Self::list_field(doc, "location_order");
        let id = id.to_string();
        if !(0..doc.length(&order))
            .any(|index| read_str(doc, &order, index).as_deref() == Some(id.as_str()))
        {
            let _ = doc.insert(&order, doc.length(&order), id);
        }
    }

    /// Replace only the ordering list. Individual location records remain
    /// independent, so a concurrent add cannot be lost to a reorder.
    pub fn set_location_order(doc: &mut AutoCommit, ids: &[crate::model::LocationId]) {
        Self::ensure_schema(doc);
        let order = Self::list_field(doc, "location_order");
        for index in (0..doc.length(&order)).rev() {
            let _ = doc.delete(&order, index);
        }
        for (index, id) in ids.iter().enumerate() {
            let _ = doc.insert(&order, index, id.to_string());
            Self::set_location_rank(doc, *id, (index + 1) as f64 * 1024.0);
        }
    }

    /// Seed a model into the current keyed schema. This convenience is used by
    /// document-level tests; running widgets seed through replayable actions.
    ///
    /// # Panics
    /// Never in practice: every `put_object`/`insert_object` call here
    /// targets `ROOT` or a Map/List this function just resolved, so it only
    /// fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT or a just-resolved Map/List, so put_object/insert_object cannot fail short of a corrupted document — falling back to another ObjId would silently write into the wrong object"
    )]
    #[cfg(test)]
    pub fn write_model(doc: &mut AutoCommit, model: &WeatherModel) {
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
                    let Some(id) = read_str(doc, &order, index) else {
                        continue;
                    };
                    if Self::is_tombstoned(doc, &id) {
                        continue;
                    }
                    let Ok(Some((Value::Object(ObjType::Map), location))) =
                        doc.get(&locations, id.as_str())
                    else {
                        continue;
                    };
                    if let Some(mut location) = Self::location_from_map(doc, &location, &id) {
                        if !location.order_rank.is_finite() {
                            location.order_rank = (model.locations.len() + 1) as f64 * 1024.0;
                        }
                        remaining.retain(|candidate| candidate != &id);
                        model.locations.push(location);
                    }
                }
            }
            remaining.sort_unstable();
            for id in remaining {
                if Self::is_tombstoned(doc, &id) {
                    continue;
                }
                if let Ok(Some((Value::Object(ObjType::Map), location))) =
                    doc.get(&locations, id.as_str())
                    && let Some(mut location) = Self::location_from_map(doc, &location, &id)
                {
                    if !location.order_rank.is_finite() {
                        location.order_rank = (model.locations.len() + 1) as f64 * 1024.0;
                    }
                    model.locations.push(location);
                }
            }
            model.locations.sort_by(|left, right| {
                left.order_rank
                    .total_cmp(&right.order_rank)
                    .then_with(|| left.id.cmp(&right.id))
            });
        }

        let selected_id: Option<crate::model::LocationId> =
            read_str(doc, &ROOT, "selected_location")
                .unwrap_or_default()
                .parse()
                .ok();
        model.selected = selected_id
            .and_then(|id| {
                model
                    .locations
                    .iter()
                    .position(|location| location.id == id)
            })
            .unwrap_or(0);
        if model.selected >= model.locations.len() && !model.locations.is_empty() {
            model.selected = 0;
        }

        model
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::units::Units;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn weather_document_holds_only_keyed_shared_locations() {
        // Case `round_trip_locations_and_selected_without_persisting_local_units`.
        {
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
            assert!(back.snapshots.is_empty(), "provider cache is device-local");
            assert!(
                back.air_quality.is_empty(),
                "provider cache is device-local"
            );
        }
        // Case `provider_records_never_enter_the_collaborative_document`.
        {
            let model = crate::sample::sample_model();
            let mut doc = AutoCommit::new();
            WeatherDoc::write_model(&mut doc, &model);
            assert!(doc.get(&ROOT, "snapshots").unwrap().is_none());
            assert!(doc.get(&ROOT, "air_quality").unwrap().is_none());
        }
        // Case `remove_wins_over_a_concurrent_same_id_add_without_resurrection`.
        {
            let mut base = AutoCommit::new();
            WeatherDoc::ensure_schema(&mut base);
            let location = SavedLocation::new(
                "Oslo",
                "Oslo",
                "Norway",
                "NO",
                59.9139,
                10.7522,
                "Europe/Oslo",
            );
            WeatherDoc::upsert_location(&mut base, &location);
            base.commit();
            let bytes = base.save();
            let mut remover = AutoCommit::load(&bytes).unwrap();
            let mut stale_adder = AutoCommit::load(&bytes).unwrap();

            WeatherDoc::remove_location(&mut remover, location.id);
            remover.commit();
            WeatherDoc::upsert_location(&mut stale_adder, &location);
            stale_adder.commit();
            let changes = stale_adder.get_changes(&base.get_heads());
            remover.apply_incoming(changes).unwrap();

            assert!(WeatherDoc::read_model(&remover).locations.is_empty());
        }
    }
}
