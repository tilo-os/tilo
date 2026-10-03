//! Maps' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Maps performs on its shared map state is a
//! [`MapsAction`]: a small serializable value that deterministically mutates the
//! CRDT. [`MapModel`] is projected into native keyed,
//! scalar, and text structures (see [`crate::doc::MapsDoc`]), so an action's
//! [`apply`](Action::apply) writes exactly the addressed shared field. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the save/star button, the
//! clear-route ×, the offline overlay's download / trash / clear controls)
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! ## What is (and is not) an action here
//!
//! The migrated collaborative doc mutations are the **saved places**, committed
//! **searches/history**, accepted **geocodes/routes/transit**,
//! and the **offline regions** (the manageable rows of what has been downloaded).
//! Camera movement, a focused text caret, status text, and ranked search/POI
//! caches remain presentation or external work. A committed search records its
//! intent in the document; each client resolves that intent locally.
//!
//! External, non-deterministic work stays out of `apply` too. Tile downloads,
//! geocoding, and routing computation are done by the app off the wire; only the
//! resulting metadata *row* crosses the executor. Mirroring how the app's
//! `delete_region` keeps tiles a neighbouring area still needs, [`MapsAction`]
//! only ever touches the **row** — [`DeleteRegion`](MapsAction::DeleteRegion)
//! removes the record, and the app deletes the now-unreferenced tiles as an
//! external side effect. Likewise a route is *stored* as the persisted result of
//! an external OSRM/MOTIS/geocoder result is accepted through a typed action
//! payload; only fetching it stays outside `apply`.
//!
//! ## Determinism contract
//!
//! Ids (a region's) are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock, and a payload that
//! names no existing row is a **no-op** rather than an error, so a bad replay
//! can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, ConfirmationPolicy, Doc,
    EncodedAction, ExecutionLocus, Resource, RetryPolicy, RiskLevel,
};

use crate::doc::MapsDoc;
use crate::model::{
    DownloadedRegion, LatLon, MapModel, Place, Route, TransitJourney, TransitTime, TravelMode,
};

/// Registry `type_id` for every Maps action.
pub const MAPS_ACTION_TYPE: &str = "maps.action";

/// Every collaborative operation Maps exports on its shared map state.
///
/// Each variant carries enough payload to replay deterministically against its
/// addressed native CRDT record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapsAction {
    /// Accept geocoded places into the durable place collection. Search overlays
    /// may stay local, but a result that becomes a map resource always crosses
    /// this replayable boundary.
    UpsertPlaces { places: Vec<Place> },
    /// Save (star) or un-save a place by id. One variant covers both "save a
    /// pin/bookmark" and "remove it from saved"; the target state is explicit in
    /// the payload (never a read-and-toggle) so two replicas converge.
    SetPlaceSaved { id: String, saved: bool },
    /// Commit a search request and add it to the shared, capped history. The
    /// actual geocoding is intentionally not in `apply`: it is external and
    /// nondeterministic, and each peer resolves the same query independently.
    CommitSearch { query: String },
    /// Clear the shared search history. Search history is workspace state, not
    /// an unaddressable presentation cache, so this remains undoable/replayable.
    ClearRecents,
    /// Set the shared directions preference absolutely. The resulting route is
    /// still accepted separately once external planning completes.
    SetTravelMode { mode: TravelMode },
    /// Set the complete depart/arrive anchor absolutely (never an increment),
    /// so replay is independent of a replica's previous local value.
    SetTransitTime { value: TransitTime },
    /// Drop the active road route through the same canonical path that accepts
    /// a completed routing result.
    ClearRoute,
    /// Accept a completed road-routing result. The worker never mutates a
    /// hidden model mirror: the same action updates the visible app and peers.
    SetRoute {
        route: Route,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
        mode: TravelMode,
    },
    /// Accept completed transit planning output through the canonical action
    /// lifecycle, rather than serialising a later whole-model projection.
    SetTransit {
        transit: Vec<TransitJourney>,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
    },
    /// Clear accepted transit options without affecting a road route.
    ClearTransit,
    /// Record a downloaded area as a manageable offline region. Re-downloading
    /// the exact same footprint updates the record in place rather than stacking
    /// a duplicate. The tiles themselves are fetched externally; this is only the
    /// row. Every field (id, name, bounds, zoom range, tile count) is resolved at
    /// authoring time and carried here so `apply` is a pure write.
    AddRegion(DownloadedRegion),
    /// Remove one offline region row by id. The tiles no other area needs are
    /// deleted by the app as an external side effect; the action is the row.
    DeleteRegion { id: String },
    /// Remove every offline region row (the "clear offline areas" control). The
    /// on-disk tile cache is cleared by the app externally.
    ClearRegions,
}

impl Action for MapsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            MapsAction::UpsertPlaces { places } => MapsDoc::upsert_places(doc, places),
            MapsAction::SetPlaceSaved { id, saved } => MapsDoc::set_place_saved(doc, id, *saved),
            MapsAction::CommitSearch { query } => MapsDoc::commit_search(doc, query),
            MapsAction::ClearRecents => MapsDoc::clear_recents(doc),
            MapsAction::SetTravelMode { mode } => MapsDoc::set_travel_mode(doc, *mode),
            MapsAction::SetTransitTime { value } => MapsDoc::set_transit_time(doc, *value),
            MapsAction::ClearRoute => MapsDoc::clear_route(doc),
            MapsAction::SetRoute {
                route,
                origin,
                origin_name,
                dest,
                dest_name,
                mode,
            } => {
                MapsDoc::set_active_route(
                    doc,
                    route,
                    *origin,
                    origin_name,
                    *dest,
                    dest_name,
                    *mode,
                );
            }
            MapsAction::SetTransit {
                transit,
                origin,
                origin_name,
                dest,
                dest_name,
            } => {
                MapsDoc::set_active_transit(doc, transit, *origin, origin_name, *dest, dest_name);
            }
            MapsAction::ClearTransit => MapsDoc::clear_transit(doc),
            MapsAction::AddRegion(region) => MapsDoc::add_region(doc, region),
            MapsAction::DeleteRegion { id } => MapsDoc::delete_region(doc, id),
            MapsAction::ClearRegions => MapsDoc::clear_regions(doc),
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

impl MapsAction {
    /// The pure mutation on the plain model — the single implementation the
    /// action's [`apply`](Action::apply) runs against the document. Deterministic
    /// w.r.t. `(self, model)`: no RNG, no wall-clock, stable iteration.
    pub fn apply_to_model(&self, model: &mut MapModel) {
        match self {
            MapsAction::UpsertPlaces { places } => {
                for place in places {
                    model.upsert_place(place.clone());
                }
            }
            MapsAction::SetPlaceSaved { id, saved } => {
                if let Some(p) = model.places.iter_mut().find(|p| &p.id == id) {
                    p.saved = *saved;
                }
            }
            MapsAction::CommitSearch { query } => {
                let query = query.trim();
                if !query.is_empty() {
                    model.push_recent(query);
                }
            }
            MapsAction::ClearRecents => {
                model.recents.clear();
            }
            MapsAction::SetTravelMode { mode } => model.mode = *mode,
            MapsAction::SetTransitTime { value } => model.transit_time = *value,
            MapsAction::ClearRoute => {
                model.route = None;
            }
            MapsAction::SetRoute {
                route,
                origin,
                origin_name,
                dest,
                dest_name,
                mode,
            } => {
                model.route = Some(route.clone());
                model.origin = *origin;
                model.origin_name.clone_from(origin_name);
                model.dest = *dest;
                model.dest_name.clone_from(dest_name);
                model.mode = *mode;
            }
            MapsAction::SetTransit {
                transit,
                origin,
                origin_name,
                dest,
                dest_name,
            } => {
                model.transit.clone_from(transit);
                model.route = None;
                model.origin = *origin;
                model.origin_name.clone_from(origin_name);
                model.dest = *dest;
                model.dest_name.clone_from(dest_name);
                model.mode = crate::model::TravelMode::Transit;
            }
            MapsAction::ClearTransit => model.transit.clear(),
            MapsAction::AddRegion(region) => {
                // Same footprint (bounds + zoom range) → update in place rather
                // than stack a duplicate row.
                model.regions.retain(|r| !same_footprint(r, region));
                model.regions.push(region.clone());
            }
            MapsAction::DeleteRegion { id } => {
                model.regions.retain(|r| &r.id != id);
            }
            MapsAction::ClearRegions => {
                model.regions.clear();
            }
        }
    }
}

/// Whether two region records cover the identical footprint (bounds + zoom
/// range) — the dedupe key for [`MapsAction::AddRegion`].
fn same_footprint(a: &DownloadedRegion, b: &DownloadedRegion) -> bool {
    const EPS: f64 = 1e-6;
    a.z0 == b.z0
        && a.z1 == b.z1
        && (a.min.lat - b.min.lat).abs() < EPS
        && (a.min.lon - b.min.lon).abs() < EPS
        && (a.max.lat - b.max.lat).abs() < EPS
        && (a.max.lon - b.max.lon).abs() < EPS
}

/// Register Maps' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<MapsAction>(MAPS_ACTION_TYPE);
}

/// Encode any [`MapsAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Maps action into a
/// replayable envelope.
///
/// # Panics
///
/// Never panics: a `MapsAction` that fails to serialize (it never should —
/// every field is a plain string, bool or finite coordinate) falls back to an
/// empty-payload envelope rather than crashing the caller.
pub fn encode(action: &MapsAction) -> EncodedAction {
    EncodedAction::encode(MAPS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: MAPS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Per-collection ceiling for the contextual Ctrl+K snapshot. Exact canonical
/// ids still resolve through [`ResourceProvider::resource`], so a large map
/// library remains agent-addressable without rebuilding every row each frame.
const EAGER_PER_KIND_LIMIT: usize = 12;

fn immediate(id: &str, label: impl Into<String>, action: &MapsAction) -> ActionSpec {
    let label = label.into();
    ActionSpec::immediate(id, label.clone(), MAPS_ACTION_TYPE, action)
        .unwrap_or_else(|_| {
            // The fallible builder's fallback. `ActionSpec`'s fields are
            // private now that it carries typed parameters, so construct it
            // through the constructor that takes an already-encoded action.
            ActionSpec::immediate_encoded(id, label, action.locus(), encode(action))
        })
        .with_contract(ActionContract {
            risk: if matches!(
                action,
                MapsAction::DeleteRegion { .. } | MapsAction::ClearRegions
            ) {
                RiskLevel::Destructive
            } else {
                RiskLevel::Low
            },
            confirmation: if matches!(
                action,
                MapsAction::DeleteRegion { .. } | MapsAction::ClearRegions
            ) {
                ConfirmationPolicy::Confirm
            } else {
                ConfirmationPolicy::None
            },
            retry: RetryPolicy::Idempotent,
            reversible: true,
            tags: vec!["maps".into(), "workspace".into()],
            ..ActionContract::default()
        })
}

/// A one-line label for the active route (its resolved endpoints when known).
fn route_label(model: &MapModel) -> String {
    let dest = model.dest_name.trim();
    if dest.is_empty() {
        "Active route".to_string()
    } else {
        format!("Route to {dest}")
    }
}

/// A one-line label for an offline region: its human name plus the covered zoom
/// range, so the palette row is self-describing.
fn region_label(r: &DownloadedRegion) -> String {
    format!("{} · z{}–{}", r.name, r.z0, r.z1)
}

fn history_resource(query: &str) -> Resource {
    let mut resource = Resource::new(
        format!("maps.search-history:{query}"),
        "maps.search-history",
        query,
    );
    if let Ok(action) = ActionSpec::operation(
        "search-again",
        "Search again",
        tilo_actions::OperationKind::View,
        "maps.search",
        &query.to_string(),
    ) {
        resource = resource.with_action(action.with_contract(ActionContract::view()));
    }
    resource
}

fn place_view_ops(place_id: &str) -> Vec<ActionSpec> {
    let mut ops = Vec::new();
    if let Ok(spec) = ActionSpec::operation(
        "open",
        "Open place",
        tilo_actions::OperationKind::View,
        "maps.open-place",
        &place_id,
    ) {
        ops.push(spec.with_contract(ActionContract::view()));
    }
    if let Ok(spec) = ActionSpec::operation(
        "directions",
        "Get directions",
        tilo_actions::OperationKind::View,
        "maps.directions",
        &place_id,
    ) {
        ops.push(spec.with_contract(ActionContract::view()));
    }
    ops
}

fn saved_place_resource(place: &crate::model::Place) -> Resource {
    let mut resource = Resource::new(
        format!("maps.place:{}", place.id),
        "maps.place",
        place.name.clone(),
    )
    .with_action(immediate(
        "unsave",
        "Remove from saved",
        &MapsAction::SetPlaceSaved {
            id: place.id.clone(),
            saved: false,
        },
    ));
    // Presentation open/directions ride the place child so hosted publish
    // keeps them without a second hand-built workspace tree.
    for spec in place_view_ops(&place.id) {
        resource.actions.insert(0, spec);
    }
    resource
}

fn route_resource(model: &MapModel) -> Resource {
    Resource::new("maps.route:active", "maps.route", route_label(model)).with_action(immediate(
        "clear",
        "Clear route",
        &MapsAction::ClearRoute,
    ))
}

fn transit_resource(model: &MapModel) -> Resource {
    Resource::new(
        "maps.transit:active",
        "maps.transit",
        format!("{} transit option(s)", model.transit.len()),
    )
    .with_action(immediate(
        "clear",
        "Clear transit options",
        &MapsAction::ClearTransit,
    ))
}

fn plan_settings_resource(model: &MapModel) -> Resource {
    let mut resource = Resource::new(
        "maps.plan-settings",
        "maps.plan-settings",
        "Route planning settings",
    );
    for mode in TravelMode::ALL {
        resource = resource.with_action(immediate(
            &format!("travel-mode-{}", mode.label().to_ascii_lowercase()),
            format!("Travel by {}", mode.label()),
            &MapsAction::SetTravelMode { mode },
        ));
    }
    for (id, label, value) in [
        (
            "travel-time-earlier",
            "Plan 15 minutes earlier",
            TransitTime {
                offset_min: (model.transit_time.offset_min - 15).clamp(0, 1440),
                ..model.transit_time
            },
        ),
        ("travel-time-now", "Leave now", TransitTime::default()),
        (
            "travel-time-later",
            "Plan 15 minutes later",
            TransitTime {
                offset_min: (model.transit_time.offset_min + 15).clamp(0, 1440),
                ..model.transit_time
            },
        ),
    ] {
        resource =
            resource.with_action(immediate(id, label, &MapsAction::SetTransitTime { value }));
    }
    resource
}

fn region_resource(region: &DownloadedRegion) -> Resource {
    Resource::new(
        format!("maps.region:{}", region.id),
        "maps.region",
        region_label(region),
    )
    .with_action(immediate(
        "delete",
        "Delete region",
        &MapsAction::DeleteRegion {
            id: region.id.clone(),
        },
    ))
}

/// Prompted Search maps View — local geocode broker, not a document command.
pub(crate) fn search_maps_spec() -> ActionSpec {
    ActionSpec::prompted_operation(
        "search",
        "Search maps",
        "Place, address, or category",
        "e.g. museum near me",
        tilo_actions::OperationKind::View,
        "maps.search",
        |query| {
            EncodedAction::encode_operation("maps.search", &query.trim().to_string())
                .unwrap_or_else(|_| EncodedAction {
                    type_id: "maps.search".into(),
                    payload: Vec::new(),
                })
        },
    )
}

/// Document-bound workspace actions for [`tilo_resource::publish`].
pub(crate) fn document_bound_actions(model: &MapModel) -> Vec<ActionSpec> {
    let mut actions = Vec::new();
    if !model.regions.is_empty() {
        actions.push(immediate(
            "clear-regions",
            "Clear offline areas",
            &MapsAction::ClearRegions,
        ));
    }
    if !model.recents.is_empty() {
        actions.push(immediate(
            "clear-search-history",
            "Clear search history",
            &MapsAction::ClearRecents,
        ));
    }
    actions
}

fn find_maps(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_maps(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

/// Collaborative children for [`tilo_resource::publish`]: history, saved
/// places, active route/transit, offline regions, and plan settings.
pub(crate) fn document_children(model: &MapModel) -> Vec<Resource> {
    let mut children = Vec::new();
    for query in model.recents.iter().take(EAGER_PER_KIND_LIMIT) {
        children.push(history_resource(query));
    }
    for place in model
        .places
        .iter()
        .filter(|place| place.saved)
        .take(EAGER_PER_KIND_LIMIT)
    {
        children.push(saved_place_resource(place));
    }
    if model.route.is_some() {
        children.push(route_resource(model));
    }
    if !model.transit.is_empty() {
        children.push(transit_resource(model));
    }
    for region in model.regions.iter().take(EAGER_PER_KIND_LIMIT) {
        children.push(region_resource(region));
    }
    children.push(plan_settings_resource(model));
    children
}

/// Build the resource tree Maps currently has open via
/// [`tilo_resource::publish`]: the workspace door, its saved places, the
/// active route, and offline regions — every document-mutating operation
/// exported as the same action the on-screen affordance dispatches.
///
/// The shared state lives in the document, so this reads the model straight from
/// `doc` (an unseeded document publishes nothing). Presentation — the selected
/// place, the camera, the search box — is read for labelling only and never
/// becomes an action. Ranked search hits and map POIs stay on the widget.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let Some(model) = MapsDoc::load(doc) else {
        return Vec::new();
    };
    vec![ui::tilo_resource::publish::<crate::resource::Maps>(
        "maps.workspace",
        "Maps",
        &model,
    )]
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::MapsApp {
    /// Standalone / `--shots` / unit tests: one tree from [`tilo_resource::publish`].
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if let Some(found) = find_maps(&provide_resources(doc), id) {
            return Some(found);
        }
        let model = MapsDoc::load(doc)?;
        if id == "maps.workspace" {
            return provide_resources(doc).into_iter().next();
        }
        if id == "maps.route:active" {
            return model.route.is_some().then(|| route_resource(&model));
        }
        if id == "maps.transit:active" {
            return (!model.transit.is_empty()).then(|| transit_resource(&model));
        }
        if id == "maps.plan-settings" {
            return Some(plan_settings_resource(&model));
        }
        if let Some(query) = id.strip_prefix("maps.search-history:") {
            return model
                .recents
                .iter()
                .find(|candidate| candidate.as_str() == query)
                .map(|candidate| history_resource(candidate));
        }
        if let Some(place_id) = id.strip_prefix("maps.place:") {
            return model
                .places
                .iter()
                .find(|place| place.saved && place.id == place_id)
                .map(saved_place_resource);
        }
        if let Some(region_id) = id.strip_prefix("maps.region:") {
            return model
                .regions
                .iter()
                .find(|region| region.id == region_id)
                .map(region_resource);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, LatLon, Place, Route, TravelMode};
    use automerge::ActorId;
    use tilo_actions::ActionInput;
    use tilo_automerge::ApplyIncoming;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn region(id: &str, name: &str) -> DownloadedRegion {
        DownloadedRegion {
            id: id.to_string(),
            name: name.to_string(),
            min: LatLon::new(48.10, 11.55),
            max: LatLon::new(48.18, 11.62),
            z0: 14,
            z1: 16,
            tiles: 42,
        }
    }

    /// A model carrying one saved place, an active route, and one offline region
    /// — enough to exercise every resource kind and every action variant.
    fn full_model() -> MapModel {
        let mut m = MapModel::default();
        let mut p = Place::new(
            "marienplatz",
            "Marienplatz",
            Category::Landmark,
            "",
            LatLon::new(48.137, 11.575),
        );
        p.saved = true;
        m.places.push(p);
        m.selected = Some("marienplatz".to_string());
        m.dest_name = "Marienplatz".to_string();
        m.route = Some(Route {
            mode: TravelMode::Car,
            distance_m: 1200.0,
            duration_s: 300.0,
            geometry: vec![LatLon::new(48.13, 11.57), LatLon::new(48.14, 11.58)],
            steps: Vec::new(),
            from: LatLon::new(48.13, 11.57),
            to: LatLon::new(48.14, 11.58),
        });
        m.regions.push(region("r1", "Altstadt"));
        m
    }

    fn seeded(model: &MapModel) -> AutoCommit {
        let mut doc = AutoCommit::new();
        MapsDoc::save(&mut doc, model);
        doc
    }

    /// The full model plus a committed search, so every resource kind with a
    /// document action is published.
    fn contract_seed_doc() -> AutoCommit {
        let mut model = full_model();
        model.push_recent("art museum");
        seeded(&model)
    }

    fn resource_key<'a>(res: &'a Resource, prefix: &str) -> &'a str {
        res.id
            .as_str()
            .strip_prefix(prefix)
            .expect("a keyed resource")
    }

    ui::action_contract_tests! {
        seed: contract_seed_doc(),
        registry: registry(),
        resources: |doc| provide_resources(doc),
        palette: ["Delete region", "Clear route", "Remove from saved", "Clear search history"],
        replay: [
            unsave_place => |_| encode(&MapsAction::SetPlaceSaved {
                id: "marienplatz".to_string(),
                saved: false,
            }),
            add_region => |_| encode(&MapsAction::AddRegion(region("r2", "Schwabing"))),
        ],
        undo: [
            delete_region => |_| encode(&MapsAction::DeleteRegion { id: "r1".to_string() }),
        ],
        encodes: [
            delete_region => ("maps.region", "delete", |_, res| {
                encode(&MapsAction::DeleteRegion {
                    id: resource_key(res, "maps.region:").to_string(),
                })
            }),
            unsave_place => ("maps.place", "unsave", |_, res| {
                encode(&MapsAction::SetPlaceSaved {
                    id: resource_key(res, "maps.place:").to_string(),
                    saved: false,
                })
            }),
            clear_route => ("maps.route:active", "clear", |_, _| encode(&MapsAction::ClearRoute)),
        ],
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_resources_publish_bounded_exact_trees() {
        // Phase `ctrl_k_publishes_the_workspace_tree`.
        // Ctrl+K on Maps yields the workspace tree: a `maps.place` child per saved
        // place (un-save), a `maps.route` child for the active route (clear), and a
        // `maps.region` child per offline area (delete), plus the workspace-level
        // clear-offline action. Drives the real resource path.
        {
            let mut model = full_model();
            model.places.push({
                let mut p = Place::new(
                    "viktualienmarkt",
                    "Viktualienmarkt",
                    Category::Shop,
                    "",
                    LatLon::new(48.135, 11.576),
                );
                p.saved = false;
                p
            });
            let doc = seeded(&model);

            let resources = provide_resources(&doc);
            assert_eq!(resources.len(), 1, "one workspace root");
            let root = &resources[0];
            assert_eq!(root.kind, "maps.workspace");

            let root_actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(root_actions.contains(&"clear-regions"), "{root_actions:?}");
            assert_eq!(
                root.actions
                    .iter()
                    .find(|action| action.id == "clear-regions")
                    .expect("clear regions action")
                    .contract
                    .risk,
                RiskLevel::Destructive
            );

            let place = root
                .children
                .iter()
                .find(|r| r.kind == "maps.place")
                .expect("a saved-place resource");
            assert!(
                place.actions.iter().any(|a| a.id == "unsave"),
                "saved place exports un-save"
            );
            let route = root
                .children
                .iter()
                .find(|r| r.kind == "maps.route")
                .expect("a route resource");
            assert!(
                route.actions.iter().any(|a| a.id == "clear"),
                "route exports clear"
            );
            let reg = root
                .children
                .iter()
                .find(|r| r.kind == "maps.region")
                .expect("a region resource");
            assert!(
                reg.actions.iter().any(|a| a.id == "delete"),
                "region exports delete"
            );
            assert_eq!(
                reg.actions[0].contract.confirmation,
                ConfirmationPolicy::Confirm
            );
        }

        // Phase `empty_doc_publishes_nothing`.
        // An unseeded document publishes nothing (no fabricated tree).
        {
            let doc = AutoCommit::new();
            assert!(provide_resources(&doc).is_empty());
        }

        // Phase `search_results_and_history_publish_as_resources`.
        // Search intent/history are shared and agent-addressable; the ranked
        // result list itself is an intentionally local derived cache.
        {
            let mut model = full_model();
            let result = Place::new(
                "pinakothek",
                "Alte Pinakothek",
                Category::Museum,
                "Barer Straße 27",
                LatLon::new(48.1484, 11.5707),
            );
            model.places.push(result);
            let _ = result;
            model.push_recent("art museum");
            let doc = seeded(&model);

            let root = &provide_resources(&doc)[0];
            let search = root
                .actions
                .iter()
                .find(|action| action.id == "search")
                .expect("prompted Search maps action");
            assert!(matches!(search.input, ActionInput::Prompted { .. }));
            assert!(
                root.actions
                    .iter()
                    .any(|action| action.id == "clear-search-history")
            );

            assert!(
                root.children
                    .iter()
                    .all(|resource| resource.kind != "maps.search-result"),
                "ranked search results are local derived cache, not shared resources"
            );

            let history = root
                .children
                .iter()
                .find(|resource| resource.kind == "maps.search-history")
                .expect("search history resource");
            assert_eq!(history.label, "art museum");
            assert!(
                history
                    .actions
                    .iter()
                    .any(|action| action.id == "search-again")
            );
        }

        // Phase `large_collections_are_bounded_eagerly_and_resolve_exactly`.
        {
            let mut model = full_model();
            model.results.clear();
            model.places.clear();
            model.regions.clear();
            model.recents = (0..20).map(|index| format!("query-{index}")).collect();
            for index in 0..20 {
                let mut place = Place::new(
                    &format!("place-{index}"),
                    &format!("Place {index}"),
                    Category::Landmark,
                    "",
                    LatLon::new(48.0, 11.0),
                );
                place.saved = true;
                model.places.push(place);
                model.regions.push(region(
                    &format!("region-{index}"),
                    &format!("Region {index}"),
                ));
            }
            let doc = seeded(&model);
            let root = &provide_resources(&doc)[0];
            for kind in ["maps.search-history", "maps.place", "maps.region"] {
                assert_eq!(
                    root.children
                        .iter()
                        .filter(|resource| resource.kind == kind)
                        .count(),
                    EAGER_PER_KIND_LIMIT,
                    "{kind} must stay bounded in the contextual snapshot"
                );
            }

            let app = crate::app::MapsApp::offline();
            let published_place = root
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "maps.place:place-0")
                .expect("the first saved place is on the published page");
            let looked_up = app
                .publish_resource(&doc, "maps.place:place-0")
                .expect("an eager saved place resolves");
            assert_eq!(
                looked_up
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                published_place
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                "an eager saved place is the published row"
            );
            assert!(
                app.publish_resource(&doc, "maps.place:place-19").is_some(),
                "an omitted saved place remains exactly addressable"
            );
            assert!(
                app.publish_resource(&doc, "maps.region:region-19")
                    .is_some(),
                "an omitted offline region remains exactly addressable"
            );
            assert!(
                app.publish_resource(&doc, "maps.search-history:query-19")
                    .is_some(),
                "an omitted history item remains exactly addressable"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn model_actions_are_deterministic_and_converge() {
        // Phase `committed_search_history_is_a_deterministic_model_action`.
        {
            let mut model = full_model();
            model.results = model.places.clone();

            MapsAction::CommitSearch {
                query: "  museums  ".to_string(),
            }
            .apply_to_model(&mut model);
            assert_eq!(model.recents.first().map(String::as_str), Some("museums"));

            MapsAction::ClearRecents.apply_to_model(&mut model);
            assert!(model.recents.is_empty());
        }

        // Phase `divergent_encoded_route_acceptances_converge_without_torn_context`.
        {
            let mut seed = seeded(&full_model());
            seed.commit();
            let bytes = seed.save();
            let mut a = AutoCommit::load(&bytes).unwrap();
            a.set_actor(ActorId::from([1; 16].as_slice()));
            let mut b = AutoCommit::load(&bytes).unwrap();
            b.set_actor(ActorId::from([2; 16].as_slice()));

            let mut route_a = full_model().route.unwrap();
            route_a.distance_m = 1_111.0;
            let mut route_b = route_a.clone();
            route_b.distance_m = 2_222.0;
            for (doc, action) in [
                (
                    &mut a,
                    MapsAction::SetRoute {
                        route: route_a,
                        origin: Some(LatLon::new(1.0, 1.0)),
                        origin_name: "Alpha origin".into(),
                        dest: Some(LatLon::new(2.0, 2.0)),
                        dest_name: "Alpha destination".into(),
                        mode: TravelMode::Car,
                    },
                ),
                (
                    &mut b,
                    MapsAction::SetRoute {
                        route: route_b,
                        origin: Some(LatLon::new(3.0, 3.0)),
                        origin_name: "Beta origin".into(),
                        dest: Some(LatLon::new(4.0, 4.0)),
                        dest_name: "Beta destination".into(),
                        mode: TravelMode::Foot,
                    },
                ),
            ] {
                let encoded = encode(&action);
                registry().decode(&encoded).unwrap().apply(doc).unwrap();
            }

            a.merge_incoming(&mut b).unwrap();
            b.merge_incoming(&mut a).unwrap();
            let left = MapsDoc::load(&a).unwrap();
            let right = MapsDoc::load(&b).unwrap();
            assert_eq!(left, right, "both merge directions project identically");
            let distance = left.route.unwrap().distance_m;
            assert!(
                (distance == 1_111.0
                    && left.origin_name == "Alpha origin"
                    && left.dest_name == "Alpha destination"
                    && left.mode == TravelMode::Car)
                    || (distance == 2_222.0
                        && left.origin_name == "Beta origin"
                        && left.dest_name == "Beta destination"
                        && left.mode == TravelMode::Foot),
                "the winning route and its labels/mode must remain one atomic acceptance"
            );
        }
    }
}
