//! Stable resource/action addresses exported by Weather.
//!
//! Saved locations, their order, and the selected saved location are
//! collaborative document resources. Search, agents, accessibility and
//! in-app navigation therefore address the same exported `open` [`View`]
//! operation, while the `select` document action retains the durable choice.
//! Network refresh remains a separate [`Effect`] operation.
//!
//! [`View`]: tilo_actions::OperationKind::View
//! [`Effect`]: tilo_actions::OperationKind::Effect

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionAddress, ActionError, ActionRegistry, ExecutionLocus};

use crate::doc::WeatherDoc;
use crate::model::{LocationId, SavedLocation};

pub const WEATHER_ACTION_TYPE: &str = "weather.action";

/// Widget-local operation type used to reveal one saved forecast.
pub const OPEN_LOCATION_OPERATION: &str = "weather.open-location";
/// Widget-local effect type used to refresh forecasts from the network.
pub const REFRESH_OPERATION: &str = "weather.refresh";
/// Widget-local effect type used to refresh one saved location only.
pub const REFRESH_LOCATION_OPERATION: &str = "weather.refresh-location";

/// Stable resource id for one saved location.
pub fn location_resource_id(id: LocationId) -> String {
    format!("weather.location:{id}")
}

/// Canonical view action that selects and reveals one saved forecast.
pub fn open_location_address(id: LocationId) -> ActionAddress {
    ActionAddress::new(location_resource_id(id), "open")
}

/// Durable saved-location mutations. Forecast and air-quality responses are
/// external cache effects and intentionally do not appear here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WeatherAction {
    /// Explicitly create the keyed schema. Adding the first saved place also
    /// creates it; ordinary live synchronisation leaves blank records intact.
    Initialize,
    AddLocation {
        location: SavedLocation,
    },
    RemoveLocation {
        id: String,
    },
    SelectLocation {
        id: String,
    },
    SetLocationOrder {
        ids: Vec<String>,
    },
    /// Absolute, granular reorder operation for one stable location.
    SetLocationRank {
        id: String,
        rank: f64,
    },
}

impl Action for WeatherAction {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        // A pre-action document from the first Weather schema has an ordered
        // `locations` list. Promote it before adding/removing so an upgrade
        // cannot hide a user's existing saved places.
        if !matches!(self, Self::SelectLocation { .. }) {
            WeatherDoc::migrate_legacy_locations(doc);
        }
        match self {
            Self::Initialize => WeatherDoc::ensure_schema(doc),
            Self::AddLocation { location } => {
                WeatherDoc::upsert_location(doc, location);
            }
            Self::RemoveLocation { id } => {
                if let Ok(id) = id.parse::<LocationId>() {
                    WeatherDoc::remove_location(doc, id);
                }
            }
            Self::SelectLocation { id } => {
                if let Ok(id) = id.parse::<LocationId>() {
                    WeatherDoc::select_location(doc, id);
                }
            }
            Self::SetLocationOrder { ids } => {
                let ids: Vec<LocationId> = ids.iter().filter_map(|id| id.parse().ok()).collect();
                WeatherDoc::set_location_order(doc, &ids);
            }
            Self::SetLocationRank { id, rank } => {
                if let Ok(id) = id.parse::<LocationId>() {
                    WeatherDoc::set_location_rank(doc, id, *rank);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<WeatherAction>(WEATHER_ACTION_TYPE);
}

/// Serialize a Weather mutation for direct replay tests and integrations that
/// do not own a widget `ActionHost`.
pub fn encode(action: &WeatherAction) -> tilo_actions::EncodedAction {
    tilo_actions::EncodedAction::encode(WEATHER_ACTION_TYPE, action).unwrap_or_else(|_| {
        tilo_actions::EncodedAction {
            type_id: WEATHER_ACTION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

pub fn remove_location_action(id: LocationId) -> WeatherAction {
    WeatherAction::RemoveLocation { id: id.to_string() }
}

pub fn select_location_action(id: LocationId) -> WeatherAction {
    WeatherAction::SelectLocation { id: id.to_string() }
}

pub fn location_order_action(ids: &[LocationId]) -> WeatherAction {
    WeatherAction::SetLocationOrder {
        ids: ids.iter().map(ToString::to_string).collect(),
    }
}

pub fn location_rank_action(id: LocationId, rank: f64) -> WeatherAction {
    WeatherAction::SetLocationRank {
        id: id.to_string(),
        rank,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::default();
        register(&mut registry);
        registry
    }

    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &crate::sample::sample_model());
        doc.commit();
        doc
    }

    #[test]
    fn saved_location_addresses_are_stable_and_unique() {
        let locations = crate::sample::sample_model().locations;
        let first = locations[0].id;
        let second = locations[1].id;

        assert_eq!(open_location_address(first), open_location_address(first));
        assert_ne!(open_location_address(first), open_location_address(second));
        assert_eq!(
            open_location_address(first),
            ActionAddress::new(location_resource_id(first), "open")
        );
    }

    #[test]
    fn location_mutations_replay_byte_identically_on_a_peer() {
        let mut first = seeded_doc();
        let bytes = first.save();
        let mut second = AutoCommit::load(&bytes).unwrap();
        let existing = WeatherDoc::read_model(&first).locations[0].id;
        let added = SavedLocation::new(
            "Auckland",
            "Auckland",
            "New Zealand",
            "NZ",
            -36.8485,
            174.7633,
            "Pacific/Auckland",
        );
        let actions = [
            WeatherAction::AddLocation {
                location: added.clone(),
            },
            WeatherAction::SelectLocation {
                id: added.id.to_string(),
            },
            WeatherAction::SetLocationOrder {
                ids: vec![added.id.to_string(), existing.to_string()],
            },
            WeatherAction::RemoveLocation {
                id: existing.to_string(),
            },
        ];

        for action in actions {
            let envelope = tilo_actions::apply_local(&mut first, &action, encode(&action)).unwrap();
            tilo_actions::replay(&mut second, &envelope, &registry()).unwrap();
        }

        assert_eq!(first.get_heads(), second.get_heads());
        let model = WeatherDoc::read_model(&second);
        assert_eq!(
            model.locations.first().map(|location| location.id),
            Some(added.id)
        );
        assert_eq!(
            model.selected_location().map(|location| location.id),
            Some(added.id)
        );
        assert!(
            !model
                .locations
                .iter()
                .any(|location| location.id == existing)
        );
    }

    #[test]
    fn independently_added_keyed_locations_survive_document_merge() {
        let mut base = seeded_doc();
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).unwrap();
        let mut right = AutoCommit::load(&bytes).unwrap();
        let berlin = SavedLocation::new(
            "Berlin",
            "Berlin",
            "Germany",
            "DE",
            52.52,
            13.405,
            "Europe/Berlin",
        );
        let tokyo = SavedLocation::new(
            "Tokyo",
            "Tokyo",
            "Japan",
            "JP",
            35.6762,
            139.6503,
            "Asia/Tokyo",
        );

        WeatherAction::AddLocation {
            location: berlin.clone(),
        }
        .apply(&mut left)
        .unwrap();
        WeatherAction::AddLocation {
            location: tokyo.clone(),
        }
        .apply(&mut right)
        .unwrap();
        left.commit();
        right.commit();
        let right_changes = right.get_changes(&base.get_heads());
        left.apply_incoming(right_changes).unwrap();

        let merged = WeatherDoc::read_model(&left);
        assert!(
            merged
                .locations
                .iter()
                .any(|location| location.id == berlin.id)
        );
        assert!(
            merged
                .locations
                .iter()
                .any(|location| location.id == tokyo.id)
        );
    }

    #[test]
    fn encoded_moves_of_different_locations_converge_across_divergent_replicas() {
        let mut base = seeded_doc();
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).unwrap();
        let mut right = AutoCommit::load(&bytes).unwrap();
        let locations = WeatherDoc::read_model(&base).locations;
        let first = locations[0].id;
        let last = locations[3].id;
        let left_action = WeatherAction::SetLocationRank {
            id: last.to_string(),
            rank: -1024.0,
        };
        let right_action = WeatherAction::SetLocationRank {
            id: first.to_string(),
            rank: 8192.0,
        };

        let left_envelope =
            tilo_actions::apply_local(&mut left, &left_action, encode(&left_action)).unwrap();
        let right_envelope =
            tilo_actions::apply_local(&mut right, &right_action, encode(&right_action)).unwrap();
        tilo_actions::replay(&mut left, &right_envelope, &registry()).unwrap();
        tilo_actions::replay(&mut right, &left_envelope, &registry()).unwrap();

        assert_eq!(left.get_heads(), right.get_heads());
        let left_ids: Vec<_> = WeatherDoc::read_model(&left)
            .locations
            .into_iter()
            .map(|location| location.id)
            .collect();
        let right_ids: Vec<_> = WeatherDoc::read_model(&right)
            .locations
            .into_iter()
            .map(|location| location.id)
            .collect();
        assert_eq!(left_ids, right_ids);
        assert_eq!(left_ids.first(), Some(&last));
        assert_eq!(left_ids.last(), Some(&first));
    }
}
