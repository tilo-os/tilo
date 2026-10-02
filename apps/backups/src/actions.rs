//! The Backups app's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Backups keeps almost all of its state *outside* the shared document: the
//! catalog of sources, snapshots and destinations is projected live from the
//! storage backend, and running/verifying/restoring a snapshot are external
//! side effects, not CRDT mutations. The one thing that must replicate, undo
//! and replay byte-identically is the **retention policy**, so that is the
//! whole action surface: a single [`BackupsAction::SetRetention`] that sets one
//! policy field to an absolute, pre-resolved count. The exact same
//! [`EncodedAction`] the on-screen +/- affordance dispatches is what Ctrl+K
//! dispatches, keeping one source of truth and one replay path.
//!
//! Carrying the resolved target (rather than a delta) makes replay order-free:
//! two replicas at the same heads land on the byte-identical result, and a set
//! that would change nothing is a no-op (see the change-guard in
//! [`BackupsAction::apply`]).

use automerge::{AutoCommit, ROOT};

use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, Resource, RetryPolicy,
};

use crate::doc::{BackupsDoc, PolicyField};
use crate::model::RetentionPolicy;
use crate::model::{Catalog, Snapshot};

/// Registry `type_id` for every backups action.
pub const BACKUPS_ACTION_TYPE: &str = "backups.action";
/// Protocol for local, deliberately non-replicated filesystem operations.
/// Paths and credentials never cross this boundary: operations address the
/// already-published catalog by opaque snapshot id only.
pub const BACKUPS_OPERATION_TYPE: &str = "backups.operation";

/// The safe, agent-drivable side-effect surface.  These are intentionally
/// effects rather than document actions: a peer must never replay a backup or
/// restore against a different machine's filesystem merely because it received
/// a collaborative document change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupOperation {
    BackupNow,
    Verify {
        snapshot_id: String,
    },
    Restore {
        snapshot_id: String,
    },
    Setup,
    /// Local navigation only. These IDs are stable catalog identities, never
    /// paths or display labels, so search/agents can target a concrete item.
    OpenSource {
        source_id: String,
    },
    OpenDestination {
        destination_id: String,
    },
    OpenSnapshot {
        snapshot_id: String,
    },
    OpenSchedule {
        source_id: String,
    },
}

/// # Errors
///
/// Returns an explanatory refusal when the payload is not a valid operation.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<BackupOperation>, String> {
    if type_id != BACKUPS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| format!("invalid Backups operation: {error}"))
}

fn operation_spec(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &BackupOperation,
) -> Option<ActionSpec> {
    let spec = ActionSpec::operation(id, label, kind, BACKUPS_OPERATION_TYPE, operation).ok()?;
    // A backup is deliberately local to this device.  Its contract must say
    // more than "effect": agents need to know which calls can be retried and
    // which one crosses the user's recovery-folder boundary.
    Some(match operation {
        BackupOperation::Setup => spec
            .with_description("Choose a local source and a destination outside it.")
            .retry_policy(RetryPolicy::AtMostOnce),
        BackupOperation::BackupNow => spec
            .with_description("Create one new checksummed local snapshot.")
            .requiring(tilo_actions::PermissionRequirement::Write)
            .retry_policy(RetryPolicy::AtMostOnce),
        BackupOperation::Verify { .. } => spec
            .with_description("Read every file in this stored snapshot and verify its checksum.")
            .requiring(tilo_actions::PermissionRequirement::Read)
            .retry_policy(RetryPolicy::Idempotent),
        BackupOperation::Restore { .. } => spec
            .with_description(
                "Choose a new or empty local folder, then restore and verify the whole snapshot.",
            )
            .requiring(tilo_actions::PermissionRequirement::Write)
            .retry_policy(RetryPolicy::AtMostOnce),
        BackupOperation::OpenSource { .. }
        | BackupOperation::OpenDestination { .. }
        | BackupOperation::OpenSnapshot { .. }
        | BackupOperation::OpenSchedule { .. } => spec,
    })
}

/// Every collaborative operation the Backups app exports on its stored
/// configuration.
///
/// The retention policy is the whole stored configuration, so there is exactly
/// one variant: set a policy field to an absolute count. The target is resolved
/// at authoring time and carried whole, so the executor's write is a targeted,
/// change-guarded field set (see [`Action::apply`]) that replicates + replays for
/// free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupsAction {
    /// Set a retention-policy field to an absolute `count` of snapshots to keep,
    /// clamped to `[0, 999]`. A set to the value already stored is a no-op.
    SetRetention { field: PolicyField, count: u32 },
}

impl tilo_actions::Action for BackupsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            BackupsAction::SetRetention { field, count } => {
                let target = i64::from(*count).clamp(0, 999) as u32;
                // Change-guard: read the current value and skip the write when it
                // already matches, so a redundant / reordered replay leaves the
                // document (its heads) untouched and can't wedge a peer.
                if BackupsDoc::retention(doc, &ROOT, *field) != target {
                    BackupsDoc::set_retention(doc, &ROOT, *field, target);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Retention policy is personal config: this client applies + replicates
        // and peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the backups action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<BackupsAction>(BACKUPS_ACTION_TYPE);
}

/// The single point that turns a [`BackupsAction`] into the wire envelope both
/// the palette and the on-screen affordances dispatch.
///
/// # Panics
///
/// Never panics: a `BackupsAction` that fails to serialize (it never should —
/// every field is a plain enum or integer) falls back to an empty-payload
/// envelope rather than crashing the caller.
pub fn encode(action: &BackupsAction) -> EncodedAction {
    EncodedAction::encode(BACKUPS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: BACKUPS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Set a retention field to an absolute `count` (clamped to `[0, 999]`).
pub fn set_retention_action(field: PolicyField, count: u32) -> BackupsAction {
    BackupsAction::SetRetention {
        field,
        count: count.min(999),
    }
}

/// The single place both the on-screen +/- affordances and the palette's
/// increment/decrement specs resolve a bump into an absolute target, so a
/// button and its Ctrl+K row build the byte-identical action.
pub fn bump_retention_action(field: PolicyField, cur: u32, delta: i64) -> BackupsAction {
    let count = (i64::from(cur) + delta).clamp(0, 999) as u32;
    BackupsAction::SetRetention { field, count }
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn policy_field(p: &RetentionPolicy, field: PolicyField) -> u32 {
    match field {
        PolicyField::Hourly => p.hourly,
        PolicyField::Daily => p.daily,
        PolicyField::Weekly => p.weekly,
        PolicyField::Monthly => p.monthly,
    }
}

/// Parse a typed retention count, clamped to `[0, 999]`. A non-numeric entry
/// becomes `0` rather than fabricating a surprising value.
fn parse_count(text: &str) -> u32 {
    text.trim().parse::<i64>().unwrap_or(0).clamp(0, 999) as u32
}

fn immediate_spec(id: &str, label: &str, action: &BackupsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, BACKUPS_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(id, label.to_string(), action.locus(), encode(action))
        })
        .with_contract(policy_contract())
}

fn policy_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["configuration".into(), "retention".into()],
        ..ActionContract::default()
    }
}

/// Stable id for the published workspace root ([`tilo_resource::publish`]).
pub const BACKUPS_RESOURCE_ID: &str = "backups";

const DEFAULT_UNAVAILABLE: &str = "Choose a source and destination before running a backup";

/// Retention-policy Document tree: one child per class with set / increase /
/// decrease — the same actions the on-screen affordances dispatch. Rides
/// [`crate::resource::Backups::children`] so hosted palette traffic goes through
/// [`tilo_resource::publish`], never a second hand-built `backups.policy` root.
pub(crate) fn publish_policy_document(policy: &RetentionPolicy) -> Resource {
    let mut root = Resource::new("backups.policy", "backups.policy", "Retention policy");

    for field in PolicyField::ALL {
        let cur = policy_field(policy, field);
        let mut fr = Resource::new(
            format!("backups.retention:{}", field.key()),
            "backups.retention",
            format!("{}: keep {} ({})", field.label(), cur, field.unit()),
        );

        let set_label = format!("Set {} retention", field.label().to_ascii_lowercase());
        fr = fr.with_action(
            ActionSpec::prompted(
                "set",
                set_label,
                "Count",
                "0–999",
                ExecutionLocus::ClientLocal,
                move |text: &str| encode(&set_retention_action(field, parse_count(text))),
            )
            .with_contract(policy_contract()),
        );

        // Offered only where there is room to move.
        if cur < 999 {
            fr = fr.with_action(immediate_spec(
                "increment",
                "Increase by one",
                &bump_retention_action(field, cur, 1),
            ));
        }
        if cur > 0 {
            fr = fr.with_action(immediate_spec(
                "decrement",
                "Decrease by one",
                &bump_retention_action(field, cur, -1),
            ));
        }

        root = root.with_child(fr);
    }
    root
}

/// Workspace View / Effect ops that are not document state (run backup, set up).
/// Published via [`crate::resource::Backups::local_operations`].
pub(crate) fn workspace_effect_specs(
    configured: bool,
    unavailable_reason: &str,
) -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    if let Some(spec) = operation_spec(
        "backup-now",
        "Run backup now",
        OperationKind::Effect,
        &BackupOperation::BackupNow,
    ) {
        specs.push(if configured {
            spec
        } else {
            spec.unavailable("not_configured", unavailable_reason)
        });
    }
    if let Some(spec) = operation_spec(
        "setup",
        "Set up backups",
        OperationKind::Effect,
        &BackupOperation::Setup,
    ) {
        specs.push(spec);
    }
    specs
}

fn catalog_source_set(catalog: &Catalog) -> Resource {
    let mut sources = Resource::new("backups.sources", "backups.source-set", "Backup sources");
    for source in &catalog.sources {
        let source_id = source.id.clone();
        let mut source_resource = Resource::new(
            format!("backups.source:{}", source.id),
            "backups.source",
            format!("{} · {}", source.kind.label(), source.cadence.label()),
        );
        if let Some(spec) = operation_spec(
            "open",
            "Show source",
            OperationKind::View,
            &BackupOperation::OpenSource {
                source_id: source_id.clone(),
            },
        ) {
            source_resource = source_resource.with_action(spec);
        }
        let mut schedule = Resource::new(
            format!("backups.schedule:{}", source.id),
            "backups.schedule",
            format!(
                "{} cadence · {} (observed)",
                source.kind.label(),
                source.cadence.label()
            ),
        );
        if let Some(spec) = operation_spec(
            "open",
            "Show global retention policy",
            OperationKind::View,
            &BackupOperation::OpenSchedule { source_id },
        ) {
            schedule = schedule.with_action(spec);
        }
        sources = sources.with_child(source_resource.with_child(schedule));
    }
    sources
}

fn catalog_destination_set(catalog: &Catalog) -> Resource {
    let mut destinations = Resource::new(
        "backups.destinations",
        "backups.destination-set",
        "Backup destinations",
    );
    for destination in &catalog.destinations {
        let mut destination_resource = Resource::new(
            format!("backups.destination:{}", destination.id),
            "backups.destination",
            format!("{} · {}", destination.name, destination.kind.label()),
        );
        if let Some(spec) = operation_spec(
            "open",
            "Show destination",
            OperationKind::View,
            &BackupOperation::OpenDestination {
                destination_id: destination.id.clone(),
            },
        ) {
            destination_resource = destination_resource.with_action(spec);
        }
        destinations = destinations.with_child(destination_resource);
    }
    destinations
}

fn catalog_snapshot_set(catalog: &Catalog, configured: bool, unavailable_reason: &str) -> Resource {
    let mut snapshots = Resource::new(
        "backups.snapshots",
        "backups.snapshot-set",
        "Snapshot versions and runs · recent page",
    );
    for snapshot in catalog.snapshots.iter().take(128) {
        snapshots =
            snapshots.with_child(snapshot_resource(snapshot, configured, unavailable_reason));
    }
    snapshots
}

/// Live catalog Views/Effects that are not document state. Hosted
/// [`PublishesResources`] returns these beside [`tilo_resource::publish`] —
/// never a second `backups.policy` / `backups` Document door.
pub(crate) fn live_catalog_extras(
    catalog: &Catalog,
    configured: bool,
    unavailable_reason: Option<&str>,
) -> Vec<Resource> {
    let reason = unavailable_reason.unwrap_or(DEFAULT_UNAVAILABLE);
    vec![
        catalog_source_set(catalog),
        catalog_destination_set(catalog),
        catalog_snapshot_set(catalog, configured, reason),
    ]
}

/// The resource tree Backups publishes standalone / `--shots` / unit tests:
/// retention policy Document ops plus the safe backup automation catalog.
#[allow(
    clippy::too_many_lines,
    reason = "the complete stable resource hierarchy is intentionally visible together"
)]
pub fn provide_resources(
    policy: &RetentionPolicy,
    catalog: &Catalog,
    configured: bool,
) -> Vec<Resource> {
    provide_resources_with_unavailable_reason(policy, catalog, configured, None)
}

/// As [`provide_resources`], but lets a live local backend surface the reason
/// an otherwise-saved configuration is unavailable. The public three-argument
/// form keeps deterministic callers and tests independent of host-only paths.
pub fn provide_resources_with_unavailable_reason(
    policy: &RetentionPolicy,
    catalog: &Catalog,
    configured: bool,
    unavailable_reason: Option<&str>,
) -> Vec<Resource> {
    let unavailable_reason = unavailable_reason.unwrap_or(DEFAULT_UNAVAILABLE);
    let state = crate::resource::BackupsState { policy: *policy };
    let mut backups = ui::tilo_resource::publish::<crate::resource::Backups>(
        BACKUPS_RESOURCE_ID,
        "Backups",
        &state,
    );
    let mut extra = Vec::new();
    for spec in workspace_effect_specs(configured, unavailable_reason) {
        if let Some(existing) = backups
            .actions
            .iter_mut()
            .find(|action| action.id == spec.id)
        {
            *existing = spec;
        } else {
            extra.push(spec);
        }
    }
    for spec in extra {
        backups = backups.with_action(spec);
    }
    backups = backups
        .with_child(catalog_source_set(catalog))
        .with_child(catalog_destination_set(catalog))
        .with_child(catalog_snapshot_set(
            catalog,
            configured,
            unavailable_reason,
        ));

    vec![backups]
}

/// Resolve one stable resource without requiring it to be in the contextual
/// recent-history page. Search result addresses use these ids, so an older
/// recovery point remains agent-invokable with the same Verify/Restore tree.
pub fn resource(
    policy: &RetentionPolicy,
    catalog: &Catalog,
    configured: bool,
    id: &str,
) -> Option<Resource> {
    resource_with_unavailable_reason(policy, catalog, configured, id, None)
}

/// Resolve one resource with the live backend's current unavailable reason.
/// This keeps a search result's individually-materialized action tree in sync
/// with the contextual resource page.
pub fn resource_with_unavailable_reason(
    policy: &RetentionPolicy,
    catalog: &Catalog,
    configured: bool,
    id: &str,
    unavailable_reason: Option<&str>,
) -> Option<Resource> {
    if id == "backups.policy" || id == "backups" {
        return provide_resources_with_unavailable_reason(
            policy,
            catalog,
            configured,
            unavailable_reason,
        )
        .into_iter()
        .find(|resource| resource.id.as_str() == id);
    }
    let snapshot_id = id
        .strip_prefix("backups.snapshot:")
        .or_else(|| id.strip_prefix("backups.run:"))
        .or_else(|| id.strip_prefix("backups.restore-target:"))?;
    let snapshot = catalog
        .snapshots
        .iter()
        .find(|snapshot| snapshot.id == snapshot_id)?;
    let snapshot_resource = snapshot_resource(
        snapshot,
        configured,
        unavailable_reason.unwrap_or("This local repository is not configured on this device"),
    );
    if id.starts_with("backups.snapshot:") {
        return Some(snapshot_resource);
    }
    snapshot_resource
        .children
        .into_iter()
        .find(|child| child.id.as_str() == id)
}

fn find_backup(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_backup(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

fn snapshot_resource(snapshot: &Snapshot, configured: bool, unavailable_reason: &str) -> Resource {
    let mut resource = Resource::new(
        format!("backups.snapshot:{}", snapshot.id),
        "backups.snapshot",
        format!(
            "{} · {} · {}",
            snapshot.time,
            snapshot.kind.label(),
            ui::format::bytes(snapshot.size_bytes)
        ),
    );
    if let Some(spec) = operation_spec(
        "open",
        "Show snapshot",
        OperationKind::View,
        &BackupOperation::OpenSnapshot {
            snapshot_id: snapshot.id.clone(),
        },
    ) {
        resource = resource.with_action(spec);
    }
    if let Some(spec) = operation_spec(
        "verify",
        "Verify snapshot",
        OperationKind::Effect,
        &BackupOperation::Verify {
            snapshot_id: snapshot.id.clone(),
        },
    ) {
        resource = resource.with_action(if configured {
            spec
        } else {
            spec.unavailable("not_configured", unavailable_reason)
        });
    }
    if let Some(spec) = operation_spec(
        "restore",
        "Restore snapshot into a new or empty folder…",
        OperationKind::Effect,
        &BackupOperation::Restore {
            snapshot_id: snapshot.id.clone(),
        },
    ) {
        resource = resource.with_action(if configured {
            spec
        } else {
            spec.unavailable("not_configured", unavailable_reason)
        });
    }
    let mut run = Resource::new(
        format!("backups.run:{}", snapshot.id),
        "backups.run",
        format!(
            "Completed {} run · {}",
            snapshot.kind.label(),
            snapshot.time
        ),
    );
    if let Some(spec) = operation_spec(
        "open",
        "Show run result",
        OperationKind::View,
        &BackupOperation::OpenSnapshot {
            snapshot_id: snapshot.id.clone(),
        },
    ) {
        run = run.with_action(spec);
    }
    resource = resource.with_child(run);
    // The selected folder is an explicit child resource rather than an
    // unspoken side effect of Restore.  This gives palette/agent users the
    // same truthful "new or empty folder" contract the restore sheet shows.
    let mut restore_target = Resource::new(
        format!("backups.restore-target:{}", snapshot.id),
        "backups.restore-target",
        "Restore target · choose a new or empty local folder",
    );
    if let Some(spec) = operation_spec(
        "restore",
        "Restore into a new or empty folder…",
        OperationKind::Effect,
        &BackupOperation::Restore {
            snapshot_id: snapshot.id.clone(),
        },
    ) {
        restore_target = restore_target.with_action(if configured {
            spec
        } else {
            spec.unavailable("not_configured", unavailable_reason)
        });
    }
    resource = resource.with_child(restore_target);
    resource
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::BackupsApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Level-triggered: recomputed each frame from the document, the source of
        // truth every action reads and writes.
        provide_resources_with_unavailable_reason(
            &BackupsDoc::policy(doc, &ROOT),
            self.resource_catalog(),
            self.is_configured(),
            self.unavailable_reason(),
        )
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if let Some(found) = find_backup(&self.publish_resources(doc), id) {
            return Some(found);
        }
        // A snapshot outside the recent page stays addressable.
        resource_with_unavailable_reason(
            &BackupsDoc::policy(doc, &ROOT),
            self.resource_catalog(),
            self.is_configured(),
            id,
            self.unavailable_reason(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BackupsApp;
    use crate::model::Catalog;
    use tilo_actions::{Availability, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// `sync` seeds the default retention policy, so this is ready for actions
    /// to read.
    fn seeded() -> (BackupsApp, AutoCommit) {
        let mut app = BackupsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn field_resource(root: &Resource, field: PolicyField) -> &Resource {
        let policy = if root.kind == "backups.policy" {
            root
        } else {
            root.children
                .iter()
                .find(|resource| resource.kind == "backups.policy")
                .unwrap_or(root)
        };
        let id = format!("backups.retention:{}", field.key());
        policy
            .children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("a resource for {id}"))
    }

    /// The exported resource tree: the retention classes and catalog hierarchy,
    /// bounded steppers, unavailable-repository reasons and honest effect
    /// contracts; each case builds its own resources.
    #[test]
    fn resource_tree_publishes_retention_catalog_and_honest_contracts() {
        ctrl_k_publishes_the_retention_tree();
        bounded_fields_omit_the_impossible_move();
        unavailable_repository_reason_reaches_the_canonical_backup_action();
        local_effects_publish_honest_retry_risk_and_restore_target_contracts();
    }

    /// Drives the real resource hook and the real palette, not a mock tree.
    /// Case of `resource_tree_publishes_retention_catalog_and_honest_contracts`.
    fn ctrl_k_publishes_the_retention_tree() {
        let (app, doc) = seeded();
        let resources = ui::resources::resources(&app, &doc);
        assert_eq!(
            resources.len(),
            1,
            "the workspace publishes the policy and the backup catalog"
        );
        let workspace = &resources[0];
        assert_eq!(workspace.kind, "backups.workspace");
        let root = workspace
            .children
            .iter()
            .find(|resource| resource.kind == "backups.policy")
            .expect("retention-policy child");
        assert_eq!(root.kind, "backups.policy");
        assert_eq!(root.children.len(), 4, "one child per retention class");

        let daily = field_resource(root, PolicyField::Daily);
        assert_eq!(daily.kind, "backups.retention");
        let d_actions: Vec<&str> = daily.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(d_actions.contains(&"set"), "{d_actions:?}");
        assert!(d_actions.contains(&"increment"), "{d_actions:?}");
        assert!(d_actions.contains(&"decrement"), "{d_actions:?}");

        // The flattened tree exports every action across all four classes.
        let flat: Vec<&str> = tilo_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Increase by one"), "{flat:?}");
        assert!(flat.iter().any(|l| l.starts_with("Set daily")), "{flat:?}");

        let catalog_resources =
            provide_resources(&BackupsDoc::policy(&doc, &ROOT), &Catalog::sample(), false);
        let workspace = catalog_resources
            .iter()
            .find(|resource| resource.kind == "backups.workspace")
            .expect("backup workspace root");
        let source = workspace
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "backups.sources")
            .expect("sources root")
            .children
            .first()
            .expect("a source");
        assert!(source.id.as_str().starts_with("backups.source:"));
        assert!(
            source.children[0]
                .id
                .as_str()
                .starts_with("backups.schedule:")
        );
        let destination = workspace
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "backups.destinations")
            .expect("destinations root")
            .children
            .first()
            .expect("a destination");
        assert!(destination.id.as_str().starts_with("backups.destination:"));
        let snapshot = workspace
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "backups.snapshots")
            .expect("snapshots root")
            .children
            .first()
            .expect("a snapshot");
        assert!(snapshot.id.as_str().starts_with("backups.snapshot:"));
    }

    /// Case of `resource_tree_publishes_retention_catalog_and_honest_contracts`.
    fn bounded_fields_omit_the_impossible_move() {
        let (_app, mut doc) = seeded();
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Weekly, 0);
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Monthly, 999);
        let resources =
            provide_resources(&BackupsDoc::policy(&doc, &ROOT), &Catalog::empty(), false);
        assert!(
            resources
                .iter()
                .flat_map(|resource| &resource.children)
                .flat_map(|resource| &resource.actions)
                .all(|action| action.contract.reversible
                    && action.contract.retry == RetryPolicy::Idempotent)
        );
        let root = &resources[0];

        let weekly = field_resource(root, PolicyField::Weekly);
        let w_actions: Vec<&str> = weekly.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            !w_actions.contains(&"decrement"),
            "a 0-count field can't decrement: {w_actions:?}"
        );
        assert!(w_actions.contains(&"increment"), "{w_actions:?}");

        let monthly = field_resource(root, PolicyField::Monthly);
        let m_actions: Vec<&str> = monthly.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            !m_actions.contains(&"increment"),
            "a 999-count field can't increment: {m_actions:?}"
        );
        assert!(m_actions.contains(&"decrement"), "{m_actions:?}");
    }

    /// Case of `resource_tree_publishes_retention_catalog_and_honest_contracts`.
    fn unavailable_repository_reason_reaches_the_canonical_backup_action() {
        let reason = "The saved backup location is unavailable. Choose another location.";
        let resources = provide_resources_with_unavailable_reason(
            &RetentionPolicy::default(),
            &Catalog::empty(),
            false,
            Some(reason),
        );
        let workspace = resources
            .iter()
            .find(|resource| resource.id.as_str() == "backups")
            .expect("backups workspace resource");
        let backup = workspace
            .actions
            .iter()
            .find(|action| action.id == "backup-now")
            .expect("backup action");
        assert!(matches!(
            &backup.contract.availability,
            Availability::Unavailable { reason: actual, .. } if actual == reason
        ));
    }

    /// Case of `resource_tree_publishes_retention_catalog_and_honest_contracts`.
    fn local_effects_publish_honest_retry_risk_and_restore_target_contracts() {
        let resources = provide_resources(&RetentionPolicy::default(), &Catalog::sample(), true);
        let workspace = resources
            .iter()
            .find(|resource| resource.kind == "backups.workspace")
            .expect("backup workspace root");
        let snapshots = workspace
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "backups.snapshots")
            .expect("snapshot collection");
        let backup_now = workspace
            .actions
            .iter()
            .find(|action| action.id == "backup-now")
            .expect("backup action");
        assert_eq!(
            backup_now.contract.permission,
            tilo_actions::PermissionRequirement::Write
        );
        let snapshot = snapshots.children.first().expect("sample snapshot");
        let verify = snapshot
            .actions
            .iter()
            .find(|action| action.id == "verify")
            .expect("verify action");
        assert_eq!(verify.contract.retry, RetryPolicy::Idempotent);
        assert_eq!(
            verify.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        let restore = snapshot
            .actions
            .iter()
            .find(|action| action.id == "restore")
            .expect("restore action");
        assert_eq!(restore.contract.risk, tilo_actions::RiskLevel::Low);
        assert_eq!(
            restore.contract.permission,
            tilo_actions::PermissionRequirement::Write
        );
        assert!(
            restore.contract.validate_declaration().is_empty(),
            "the empty-target restore contract has no misleading confirmation"
        );
        let target = snapshot
            .children
            .iter()
            .find(|child| child.kind == "backups.restore-target")
            .expect("explicit restore target resource");
        assert!(target.label.contains("new or empty"));
        assert_eq!(target.actions.len(), 1);
        assert!(
            snapshot
                .children
                .iter()
                .any(|child| child.kind == "backups.run"),
            "every stored version exposes its completed run in the history tree"
        );
    }

    /// The on-screen +/- control dispatches the same action as the palette, and
    /// an unchanged set is a no-op; each case uses a fresh seeded app.
    #[test]
    fn retention_controls_dispatch_the_palette_action_and_skip_no_ops() {
        redundant_action_is_a_no_op();
        bump_button_dispatches_the_palette_action();
    }

    /// Case of `retention_controls_dispatch_the_palette_action_and_skip_no_ops`.
    fn redundant_action_is_a_no_op() {
        let (_app, mut doc) = seeded();
        doc.commit();
        let before = doc.get_heads();
        let cur = BackupsDoc::retention(&doc, &ROOT, PolicyField::Hourly);

        let action = set_retention_action(PolicyField::Hourly, cur);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        assert_eq!(doc.get_heads(), before, "a no-change set changed nothing");
    }

    fn daily(doc: &AutoCommit) -> u32 {
        BackupsDoc::retention(doc, &ROOT, PolicyField::Daily)
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| ui::resources::resources(&BackupsApp::new(), doc),
        palette: ["Set daily retention", "Increase by one", "Decrease by one", "Set up backups"],
        replay: [
            set_monthly => |doc| encode(&set_retention_action(
                PolicyField::Monthly,
                BackupsDoc::retention(doc, &ROOT, PolicyField::Monthly) + 6,
            )),
        ],
        undo: [
            set_daily => |doc| encode(&set_retention_action(PolicyField::Daily, daily(doc) + 3)),
        ],
        encodes: [
            increment => ("backups.retention:ret_daily", "increment", |doc, _| {
                encode(&bump_retention_action(PolicyField::Daily, daily(doc), 1))
            }),
            decrement => ("backups.retention:ret_daily", "decrement", |doc, _| {
                encode(&bump_retention_action(PolicyField::Daily, daily(doc), -1))
            }),
            set => ("backups.retention:ret_daily", "set", arg: "42", |_, _| {
                encode(&set_retention_action(PolicyField::Daily, 42))
            }),
        ],
    }

    /// `bump_retention` is what both the mouse hit and the keyboard arrow/`+`/`-`
    /// handlers funnel through.
    /// Case of `retention_controls_dispatch_the_palette_action_and_skip_no_ops`.
    fn bump_button_dispatches_the_palette_action() {
        let (mut app, mut doc) = seeded();

        // The palette spec for Daily's increment, captured before we mutate.
        let (cur, spec_encoded) = {
            let resources = ui::resources::resources(&app, &doc);
            let daily = field_resource(&resources[0], PolicyField::Daily);
            let spec = daily.actions.iter().find(|a| a.id == "increment").unwrap();
            (
                BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily),
                ui::testing::action_contract::spec_encoding(spec, None),
            )
        };

        // Drive the real on-screen affordance.
        app.bump_retention(&mut doc, &ROOT, PolicyField::Daily, 1);

        let dispatched = app
            .last_dispatched()
            .expect("the real +/- control dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the +/- control and the palette are one action"
        );
        // And it actually moved the stored policy.
        assert_eq!(
            BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily),
            cur + 1
        );
    }
}
