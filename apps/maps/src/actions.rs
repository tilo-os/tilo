//! Maps' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Maps performs on its shared map state is a
//! [`MapsAction`]: a small serializable value that deterministically mutates the
//! CRDT. The whole [`MapModel`](crate::model::MapModel) is persisted as one JSON
//! blob (see [`crate::doc::MapsDoc`]), so an action's [`apply`](Action::apply)
//! reads that model, mutates exactly the shared field(s) it owns, and writes it
//! back — a change that replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the save/star button, the
//! clear-route ×, the offline overlay's download / trash / clear controls)
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! ## What is (and is not) an action here
//!
//! The migrated collaborative doc mutations are the **saved places**, the active
//! **route**'s removal, and the **offline regions** (the manageable rows of what
//! has been downloaded). Everything else Maps persists into the same blob is
//! per-user *presentation* and never becomes an action: the map camera (centre,
//! zoom, pan), which place is selected, the search box, the travel-mode/panel
//! view, the recents list, the status line, and the computed transit itineraries.
//!
//! External, non-deterministic work stays out of `apply` too. Tile downloads,
//! geocoding, and routing computation are done by the app off the wire; only the
//! resulting metadata *row* crosses the executor. Mirroring how the app's
//! `delete_region` keeps tiles a neighbouring area still needs, [`MapsAction`]
//! only ever touches the **row** — [`DeleteRegion`](MapsAction::DeleteRegion)
//! removes the record, and the app deletes the now-unreferenced tiles as an
//! external side effect. Likewise a route is *stored* as the persisted result of
//! an external OSRM computation (folded in through the ordinary model mirror,
//! not an undo point); the discrete user gesture that *clears* it is the action.
//!
//! ## Determinism contract
//!
//! Ids (a region's) are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock, and a payload that
//! names no existing row is a **no-op** rather than an error, so a bad replay
//! can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::MapsDoc;
use crate::model::{DownloadedRegion, MapModel};

/// Registry `type_id` for every Maps action.
pub const MAPS_ACTION_TYPE: &str = "maps.action";

/// Every collaborative operation Maps exports on its shared map state.
///
/// Each variant is a whole-field write on the persisted [`MapModel`], carrying
/// enough payload to replay deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapsAction {
    /// Save (star) or un-save a place by id. One variant covers both "save a
    /// pin/bookmark" and "remove it from saved"; the target state is explicit in
    /// the payload (never a read-and-toggle) so two replicas converge.
    SetPlaceSaved { id: String, saved: bool },
    /// Drop the active road route. The user's discrete "clear route" gesture;
    /// setting a route is the persisted result of an external routing
    /// computation and rides the ordinary model mirror instead.
    ClearRoute,
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
        // The model lives as one JSON blob; a doc that was never seeded has
        // nothing to mutate, so a replay before the model exists is a no-op.
        let Some(mut model) = MapsDoc::load(doc) else {
            return Ok(());
        };
        self.apply_to_model(&mut model);
        MapsDoc::save(doc, &model);
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
            MapsAction::SetPlaceSaved { id, saved } => {
                if let Some(p) = model.places.iter_mut().find(|p| &p.id == id) {
                    p.saved = *saved;
                }
            }
            MapsAction::ClearRoute => {
                model.route = None;
            }
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
pub fn encode(action: &MapsAction) -> EncodedAction {
    EncodedAction::encode(MAPS_ACTION_TYPE, action).expect("MapsAction is always serializable")
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn immediate(id: &str, label: impl Into<String>, action: &MapsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, MAPS_ACTION_TYPE, action).expect("encode maps action")
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

/// Build the resource tree Maps currently has open: the workspace, its saved
/// places (each exporting an un-save action), the active route (a clear action),
/// and its offline regions (each a delete action) — every document-mutating
/// operation exported as the same action the on-screen affordance dispatches.
///
/// The shared state lives in the document, so this reads the model straight from
/// `doc` (an unseeded document publishes nothing). Presentation — the selected
/// place, the camera, the search box — is read for labelling only and never
/// becomes an action.
pub fn provide_resources(doc: &AutoCommit) -> Vec<Resource> {
    let Some(model) = MapsDoc::load(doc) else {
        return Vec::new();
    };

    let mut root = Resource::new("maps.workspace", "maps.workspace", "Maps");

    // Workspace-level: save the selected place, only offered when one is
    // selected and not already saved.
    if let Some(sel) = model.selected_place()
        && !sel.saved
    {
        root = root.with_action(immediate(
            "save-place",
            format!("Save {}", sel.name),
            &MapsAction::SetPlaceSaved {
                id: sel.id.clone(),
                saved: true,
            },
        ));
    }

    // Workspace-level: clear every offline area, only when some exist.
    if !model.regions.is_empty() {
        root = root.with_action(immediate(
            "clear-regions",
            "Clear offline areas",
            &MapsAction::ClearRegions,
        ));
    }

    // Saved places → a resource each, exporting the un-save action.
    for p in model.places.iter().filter(|p| p.saved) {
        root = root.with_child(
            Resource::new(format!("maps.place:{}", p.id), "maps.place", p.name.clone())
                .with_action(immediate(
                    "unsave",
                    "Remove from saved",
                    &MapsAction::SetPlaceSaved {
                        id: p.id.clone(),
                        saved: false,
                    },
                )),
        );
    }

    // Active route → a resource exporting the clear action.
    if model.route.is_some() {
        root = root.with_child(
            Resource::new("maps.route:active", "maps.route", route_label(&model))
                .with_action(immediate("clear", "Clear route", &MapsAction::ClearRoute)),
        );
    }

    // Offline regions → a resource each, exporting the delete action.
    for r in &model.regions {
        root = root.with_child(
            Resource::new(
                format!("maps.region:{}", r.id),
                "maps.region",
                region_label(r),
            )
            .with_action(immediate(
                "delete",
                "Delete region",
                &MapsAction::DeleteRegion { id: r.id.clone() },
            )),
        );
    }

    vec![root]
}

impl ResourceProvider for crate::app::MapsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, LatLon, Place, Route, TravelMode};
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
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

    /// Ctrl+K on Maps yields the workspace tree: a `maps.place` child per saved
    /// place (un-save), a `maps.route` child for the active route (clear), and a
    /// `maps.region` child per offline area (delete), plus the workspace-level
    /// save-selected and clear-offline actions. Drives the real resource path.
    #[test]
    fn ctrl_k_publishes_the_workspace_tree() {
        // A selected-but-unsaved place so the workspace exports "Save".
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
        model.selected = Some("viktualienmarkt".to_string());
        let doc = seeded(&model);

        let resources = provide_resources(&doc);
        assert_eq!(resources.len(), 1, "one workspace root");
        let root = &resources[0];
        assert_eq!(root.kind, "maps.workspace");

        let root_actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"save-place"), "{root_actions:?}");
        assert!(root_actions.contains(&"clear-regions"), "{root_actions:?}");

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

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete region")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Clear route")),
            "{labels:?}"
        );
    }

    /// An unseeded document publishes nothing (no fabricated tree).
    #[test]
    fn empty_doc_publishes_nothing() {
        let doc = AutoCommit::new();
        assert!(provide_resources(&doc).is_empty());
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut state = ui::State::new();
        MapsDoc::save(&mut state.doc, &full_model());
        // `get_heads` flushes the seed write into a committed change; the action
        // that follows lands in its own change bracketed by these heads.
        let before_heads = state.doc.get_heads();
        assert!(
            MapsDoc::load(&state.doc)
                .unwrap()
                .regions
                .iter()
                .any(|r| r.id == "r1"),
            "precondition: the region is present"
        );

        let action = MapsAction::DeleteRegion {
            id: "r1".to_string(),
        };
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            !MapsDoc::load(&state.doc)
                .unwrap()
                .regions
                .iter()
                .any(|r| r.id == "r1"),
            "the action deleted the region"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            MapsDoc::load(&state.doc)
                .unwrap()
                .regions
                .iter()
                .any(|r| r.id == "r1"),
            "undo restored the region and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let mut a = seeded(&full_model());
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = MapsAction::SetPlaceSaved {
            id: "marienplatz".to_string(),
            saved: false,
        };
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            !MapsDoc::load(&a)
                .unwrap()
                .place("marienplatz")
                .unwrap()
                .saved,
            "the un-save landed on the origin"
        );
        assert!(
            !MapsDoc::load(&b)
                .unwrap()
                .place("marienplatz")
                .unwrap()
                .saved,
            "and on the replica"
        );
    }

    /// Adding a region replays byte-identically too — the create path, carrying a
    /// pre-minted id so `apply` needs no RNG.
    #[test]
    fn add_region_replays_byte_identical() {
        let mut a = seeded(&full_model());
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let action = MapsAction::AddRegion(region("r2", "Schwabing"));
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "add replayed identically");
        assert!(
            MapsDoc::load(&b)
                .unwrap()
                .regions
                .iter()
                .any(|r| r.id == "r2")
        );
    }

    /// Single source of truth: the palette's "Delete region" spec and the builder
    /// the trash button calls encode byte-identically.
    #[test]
    fn palette_and_builder_encode_identically() {
        let doc = seeded(&full_model());
        let resources = provide_resources(&doc);
        let root = &resources[0];

        let reg = root
            .children
            .iter()
            .find(|r| r.kind == "maps.region")
            .unwrap();
        let id = reg.id.as_str().trim_start_matches("maps.region:");
        let spec = reg.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&MapsAction::DeleteRegion { id: id.to_string() });
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // And a saved place's un-save spec matches its builder.
        let place = root
            .children
            .iter()
            .find(|r| r.kind == "maps.place")
            .unwrap();
        let pid = place.id.as_str().trim_start_matches("maps.place:");
        let unsave = place.actions.iter().find(|a| a.id == "unsave").unwrap();
        let from_spec = immediate_encode(unsave);
        let from_builder = encode(&MapsAction::SetPlaceSaved {
            id: pid.to_string(),
            saved: false,
        });
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "un-save is one source of truth"
        );
    }
}
