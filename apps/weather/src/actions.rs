//! What Weather does with its saved places.
//!
//! Saved places, their order and the default place are the document: each
//! change is a [`WeatherAction`], published on each place with its arguments
//! bound. Opening a forecast and fetching one change no document, so they are
//! [`WeatherEffect`]s answered by the window, offered on each place as a live
//! row joining that place's published child.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, ExecutionLocus};
use ui::prelude::{ActionDecl, Effect, LiveRow, tilo_resource};

use crate::doc::WeatherDoc;
use crate::model::{LocationId, SavedLocation, WeatherModel};
use crate::resource::Weather;

pub const WEATHER_ACTION_TYPE: &str = "weather.action";

/// Stable resource id for one saved location.
pub fn location_resource_id(id: LocationId) -> String {
    format!("weather.location:{id}")
}

/// What Weather fetches and shows, with no document delta.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "weather.effects")]
pub enum WeatherEffect {
    /// Refresh forecasts.
    ///
    /// Fetch the forecast and air quality of every saved place.
    #[effect(job, permission = "read")]
    Refresh,
    /// Refresh this forecast.
    ///
    /// Fetch the forecast and air quality of one saved place.
    #[effect(job, permission = "read")]
    RefreshLocation {
        #[param("Id of the saved place")]
        location: String,
    },
    /// Open forecast.
    ///
    /// Show one saved place's forecast.
    #[effect(permission = "read")]
    OpenLocation {
        #[param("Id of the saved place")]
        location: String,
    },
}

/// Durable saved-location mutations. Forecast and air-quality responses are
/// external cache effects and intentionally do not appear here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WeatherAction {
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
        match self {
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

/// Serialize a Weather mutation for direct replay.
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

/// Each saved place, with the document actions bound to it: use it as the
/// default place, move it to the top, remove it.
pub(crate) fn document_children(state: &WeatherModel) -> Vec<tilo_actions::Resource> {
    let top_rank = state
        .locations
        .first()
        .map_or(0.0, |first| first.order_rank - 1024.0);
    state
        .locations
        .iter()
        .enumerate()
        .map(|(index, location)| location_resource(location, index > 0, top_rank))
        .collect()
}

fn bound(id: &'static str, label: &str, action: WeatherAction) -> ActionDecl<Weather> {
    ActionDecl::new(id, label, Vec::new(), move |_, _| Ok(action.clone()))
}

fn location_resource(
    location: &SavedLocation,
    can_move_to_top: bool,
    top_rank: f64,
) -> tilo_actions::Resource {
    let mut decls = vec![bound(
        "use-as-default",
        "Use as default location",
        select_location_action(location.id),
    )];
    if can_move_to_top {
        decls.push(bound(
            "move-to-top",
            "Move to top",
            location_rank_action(location.id, top_rank),
        ));
    }
    decls.push(
        bound(
            "remove",
            "Remove location",
            remove_location_action(location.id),
        )
        .destructive(),
    );
    decls.into_iter().fold(
        tilo_actions::Resource::new(
            location_resource_id(location.id),
            "weather.location",
            location.name.clone(),
        ),
        |resource, decl| resource.with_action(ui::tilo_resource::declaration_spec(&decl)),
    )
}

/// A saved place as the window offers it: open its forecast, refresh it.
/// Joins the place's published child of the same id.
pub(crate) fn location_row(location: &SavedLocation) -> LiveRow {
    let id = location.id.to_string();
    let mut open = WeatherEffect::OpenLocation {
        location: id.clone(),
    }
    .bound();
    open.id = "open".into();
    let mut refresh = WeatherEffect::RefreshLocation { location: id }.bound();
    refresh.id = "refresh".into();
    LiveRow::new(
        location_resource_id(location.id),
        "weather.location",
        location.name.clone(),
    )
    .operation(open)
    .operation(refresh)
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

    /// The id the seed gives its `n`th saved place, so tests can address a
    /// location resource without reading the seed first.
    fn nth_id(n: usize) -> LocationId {
        LocationId::from_u128(u128::try_from(n).expect("small index") + 1)
    }

    /// The bundled sample places, with stable ids, written through the real
    /// document path.
    fn seeded_doc() -> AutoCommit {
        let mut model = crate::sample::sample_model();
        for (n, location) in model.locations.iter_mut().enumerate() {
            location.id = nth_id(n);
        }
        let mut doc = AutoCommit::new();
        WeatherDoc::write_model(&mut doc, &model);
        doc.commit();
        doc
    }

    /// The resource's publication for `doc`.
    fn resources(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![ui::tilo_resource::publish::<Weather>(
            "weather.locations",
            "Weather",
            &WeatherDoc::read_model(doc),
        )]
    }

    #[test]
    fn each_place_publishes_its_edits_and_its_row_offers_its_effects() {
        let published = resources(&seeded_doc()).remove(0);
        let root_actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            root_actions.contains(&"add-saved-location"),
            "{root_actions:?}"
        );
        assert!(published.children.iter().all(|child| {
            child.kind == "weather.location"
                && child
                    .actions
                    .iter()
                    .any(|action| action.id == "use-as-default")
                && child.actions.iter().any(|action| action.id == "remove")
        }));
        assert!(
            published.children[0]
                .actions
                .iter()
                .all(|action| action.id != "move-to-top"),
            "the first place is already on top"
        );
        assert!(published.children[1..].iter().all(|child| {
            child
                .actions
                .iter()
                .any(|action| action.id == "move-to-top")
        }));

        let place = WeatherDoc::read_model(&seeded_doc()).locations.remove(1);
        let row = location_row(&place).into_resource();
        assert_eq!(row.id.as_str(), location_resource_id(place.id));
        for (id, effect) in [
            (
                "open",
                WeatherEffect::OpenLocation {
                    location: place.id.to_string(),
                },
            ),
            (
                "refresh",
                WeatherEffect::RefreshLocation {
                    location: place.id.to_string(),
                },
            ),
        ] {
            let spec = row.actions.iter().find(|action| action.id == id).unwrap();
            assert_eq!(spec.kind, tilo_actions::OperationKind::Effect);
            let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(
                WeatherEffect::from_payload(&encoded.payload).unwrap(),
                effect
            );
        }
    }

    fn auckland() -> SavedLocation {
        SavedLocation::new(
            "Auckland",
            "Auckland",
            "New Zealand",
            "NZ",
            -36.8485,
            174.7633,
            "Pacific/Auckland",
        )
    }

    fn move_to_top_rank(doc: &AutoCommit) -> f64 {
        WeatherDoc::read_model(doc).locations[0].order_rank - 1024.0
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| resources(doc),
        palette: [
            "Save location",
            "Use as default location",
            "Move to top",
            "Remove location",
        ],
        replay: [
            add_location => |_| encode(&WeatherAction::AddLocation { location: auckland() }),
            select_location => |_| encode(&select_location_action(nth_id(1))),
            set_location_order => |_| encode(&location_order_action(&[nth_id(3), nth_id(0)])),
            set_location_rank => |_| encode(&location_rank_action(nth_id(2), -4096.0)),
            remove_location => |_| encode(&remove_location_action(nth_id(0))),
        ],
        undo: [
            add_location => |_| encode(&WeatherAction::AddLocation { location: auckland() }),
            remove_location => |_| encode(&remove_location_action(nth_id(0))),
            select_location => |_| encode(&select_location_action(nth_id(2))),
        ],
        encodes: [
            use_as_default => (&location_resource_id(nth_id(1)), "use-as-default", |_, _| {
                encode(&select_location_action(nth_id(1)))
            }),
            move_to_top => (&location_resource_id(nth_id(1)), "move-to-top", |doc, _| {
                encode(&location_rank_action(nth_id(1), move_to_top_rank(doc)))
            }),
            remove => (&location_resource_id(nth_id(2)), "remove", |_, _| {
                encode(&remove_location_action(nth_id(2)))
            }),
        ],
    }

    #[test]
    fn keyed_saved_locations_are_stable_and_converge() {
        // Case `saved_location_addresses_are_stable_and_unique`.
        {
            let locations = crate::sample::sample_model().locations;
            let first = locations[0].id;
            let second = locations[1].id;
            assert_eq!(location_resource_id(first), location_resource_id(first));
            assert_ne!(location_resource_id(first), location_resource_id(second));
        }
        // Case `independently_added_keyed_locations_survive_document_merge`.
        {
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
        // Case `encoded_moves_of_different_locations_converge_across_divergent_replicas`.
        {
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
                tilo_actions::apply_local(&mut right, &right_action, encode(&right_action))
                    .unwrap();
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
}
