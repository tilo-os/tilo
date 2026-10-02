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
use tilo_actions::{
    Action, ActionAddress, ActionContract, ActionError, ActionRegistry, ActionSpec, Availability,
    DiscoveryScope, ExecutionLocus, ExecutionMode, OperationKind, PermissionRequirement, Resource,
    ResultSpec, RetryPolicy,
};

use crate::doc::WeatherDoc;
use crate::model::{LocationId, SavedLocation, WeatherModel};

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

/// Root Effect: refresh every saved forecast. Availability for offline /
/// empty libraries is applied by [`provide_resources`] for standalone hosts;
/// the live widget still refuses when network work is disabled.
pub(crate) fn root_local_operations() -> Vec<ActionSpec> {
    refresh_all_spec(Availability::Available)
        .into_iter()
        .collect()
}

/// Saved-location children for [`tilo_resource::publish`]: open View, network
/// refresh Effect, and the durable select / reorder / remove actions.
pub(crate) fn document_children(state: &WeatherModel) -> Vec<Resource> {
    let top_rank = state
        .locations
        .first()
        .map_or(0.0, |first| first.order_rank - 1024.0);
    state
        .locations
        .iter()
        .enumerate()
        .map(|(index, location)| {
            location_document_resource(location, index > 0, top_rank, Availability::Available)
        })
        .collect()
}

/// Standalone / `--shots` / unit-test tree: [`tilo_resource::publish`] owns the
/// saved-places door; refresh contracts reflect whether this host can fetch.
pub fn provide_resources(state: &WeatherModel, live: bool) -> Vec<Resource> {
    let mut root = ui::tilo_resource::publish::<crate::resource::Weather>(
        "weather.locations",
        "Weather",
        state,
    );
    let refresh_availability = if !live {
        Availability::Unavailable {
            code: "network_disabled".into(),
            reason: "Network refresh is disabled for this weather view".into(),
        }
    } else if state.locations.is_empty() {
        Availability::Unavailable {
            code: "no_locations".into(),
            reason: "Save a weather location before refreshing".into(),
        }
    } else {
        Availability::Available
    };
    let location_refresh = if live {
        Availability::Available
    } else {
        Availability::Unavailable {
            code: "network_disabled".into(),
            reason: "Network refresh is disabled for this weather view".into(),
        }
    };
    replace_action_contract(&mut root, "refresh", refresh_availability);
    for child in &mut root.children {
        replace_action_contract(child, "refresh", location_refresh.clone());
    }
    vec![root]
}

fn replace_action_contract(resource: &mut Resource, action_id: &str, availability: Availability) {
    if let Some(action) = resource
        .actions
        .iter_mut()
        .find(|action| action.id == action_id)
    {
        action.contract.availability = availability;
    }
}

fn refresh_all_spec(availability: Availability) -> Option<ActionSpec> {
    ActionSpec::operation(
        "refresh",
        "Refresh forecasts",
        OperationKind::Effect,
        REFRESH_OPERATION,
        &"",
    )
    .ok()
    .map(|spec| {
        spec.with_contract(ActionContract {
            availability,
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            discovery: DiscoveryScope::Launchable,
            result: ResultSpec::Job,
            ..ActionContract::default()
        })
    })
}

fn refresh_location_spec(id: LocationId, availability: Availability) -> Option<ActionSpec> {
    ActionSpec::operation(
        "refresh",
        "Refresh this forecast",
        OperationKind::Effect,
        REFRESH_LOCATION_OPERATION,
        &id.to_string(),
    )
    .ok()
    .map(|spec| {
        spec.with_contract(ActionContract {
            availability,
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            discovery: DiscoveryScope::Launchable,
            result: ResultSpec::Job,
            ..ActionContract::default()
        })
    })
}

fn location_document_resource(
    location: &SavedLocation,
    can_move_to_top: bool,
    top_rank: f64,
    refresh_availability: Availability,
) -> Resource {
    let mut resource = Resource::new(
        location_resource_id(location.id),
        "weather.location",
        location.name.clone(),
    );
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Open forecast",
        OperationKind::View,
        OPEN_LOCATION_OPERATION,
        &location.id.to_string(),
    ) {
        resource = resource.with_action(spec.with_contract(ActionContract::view()));
    }
    if let Some(spec) = refresh_location_spec(location.id, refresh_availability) {
        resource = resource.with_action(spec);
    }
    if let Ok(spec) = ActionSpec::immediate(
        "use-as-default",
        "Use as default location",
        WEATHER_ACTION_TYPE,
        &select_location_action(location.id),
    ) {
        resource = resource.with_action(spec.reversible());
    }
    if can_move_to_top
        && let Ok(spec) = ActionSpec::immediate(
            "move-to-top",
            "Move to top",
            WEATHER_ACTION_TYPE,
            &location_rank_action(location.id, top_rank),
        )
    {
        resource = resource.with_action(spec.reversible());
    }
    if let Ok(spec) = ActionSpec::immediate(
        "remove",
        "Remove location",
        WEATHER_ACTION_TYPE,
        &remove_location_action(location.id),
    ) {
        resource = resource.with_action(spec.destructive().reversible());
    }
    resource
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

    /// The app's published tree for `doc`: an offline app adopts the
    /// document's saved places the way the host's `sync` does.
    fn resources(doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut app = crate::app::WeatherApp::new();
        let mut doc = doc.clone();
        ui::widget::Widget::sync(&mut app, &mut doc, &automerge::ROOT);
        ui::resources::resources(&app, &doc)
    }

    #[test]
    fn resource_publish_owns_the_locations_door_and_hosted_adds_no_second_tree() {
        let model = WeatherDoc::read_model(&seeded_doc());
        let published = ui::tilo_resource::publish::<crate::resource::Weather>(
            "weather.locations",
            "Weather",
            &model,
        );
        let root_actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            root_actions.contains(&"add-saved-location"),
            "{root_actions:?}"
        );
        assert!(root_actions.contains(&"refresh"), "{root_actions:?}");
        assert!(
            published.children.iter().all(|child| {
                child.kind == "weather.location"
                    && child.actions.iter().any(|action| action.id == "open")
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "use-as-default")
                    && child.actions.iter().any(|action| action.id == "remove")
            }),
            "location document + view actions ride Resource::children"
        );
        assert!(
            published.children.iter().any(|child| {
                child
                    .actions
                    .iter()
                    .any(|action| action.id == "move-to-top")
            }),
            "non-first locations expose move-to-top from shared state"
        );

        let mut app = crate::app::WeatherApp::new();
        let mut doc = seeded_doc();
        ui::widget::Widget::sync(&mut app, &mut doc, &automerge::ROOT);
        assert!(
            !app.actions_are_hosted(),
            "standalone tests stay on the local host"
        );
        let standalone = app.publish_resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "weather.locations");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "add-saved-location")
        );
        // Offline standalone overlays network_disabled on refresh.
        let refresh = standalone[0]
            .actions
            .iter()
            .find(|action| action.id == "refresh")
            .expect("root refresh");
        assert!(matches!(
            refresh.contract.availability,
            Availability::Unavailable { ref code, .. } if code == "network_disabled"
        ));

        // Hosted surfaces must not republish the saved-places door beside
        // ResourceRoot's tilo_resource::publish.
        app.mark_actions_hosted_for_test();
        assert!(app.actions_are_hosted());
        assert!(ui::resources::resource(&app, &doc, "weather.locations").is_none());
        if let Some(place_id) = standalone[0]
            .children
            .first()
            .map(|child| child.id.as_str().to_owned())
        {
            assert!(
                ui::resources::resource(&app, &doc, &place_id).is_some(),
                "a hosted place stays the published row"
            );
        }
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert!(
            extras.is_empty(),
            "hosted weather publishes only through the framework resource"
        );
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
            "Open forecast",
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

            assert_eq!(open_location_address(first), open_location_address(first));
            assert_ne!(open_location_address(first), open_location_address(second));
            assert_eq!(
                open_location_address(first),
                ActionAddress::new(location_resource_id(first), "open")
            );
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
