//! The Backups app's exported **actions**, **effects** and the **live rows** it
//! publishes beside its resource.
//!
//! Backups keeps almost all of its state *outside* the shared document: the
//! catalog of sources, snapshots and destinations is projected live from the
//! storage backend on this device, and running/verifying/restoring a snapshot
//! are [`BackupsEffect`]s, not CRDT mutations. The one thing that must
//! replicate, undo and replay byte-identically is the **retention policy**, so
//! that is the whole action surface: a single [`BackupsAction::SetRetention`]
//! that sets one policy field to an absolute, pre-resolved count. The exact same
//! [`EncodedAction`] the on-screen +/- affordance dispatches is what Ctrl+K
//! dispatches, keeping one source of truth and one replay path.
//!
//! Carrying the resolved target (rather than a delta) makes replay order-free:
//! two replicas at the same heads land on the byte-identical result, and a set
//! that would change nothing is a no-op (see the change-guard in
//! [`BackupsAction::apply`]).

use automerge::ROOT;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource, RetryPolicy,
};
use ui::framework::{Effect, LiveRow};
use ui::tilo_resource;

use crate::doc::{BackupsDoc, PolicyField};
use crate::model::RetentionPolicy;
use crate::model::{Catalog, Snapshot};

/// Registry `type_id` for every backups action.
pub const BACKUPS_ACTION_TYPE: &str = "backups.action";
/// Protocol of the effects: local, deliberately non-replicated filesystem
/// operations. Paths and credentials never cross this boundary: effects
/// address the already-published catalog by opaque id only.
pub const BACKUPS_OPERATION_TYPE: &str = "backups.operation";

/// The safe, agent-drivable side-effect surface. These are effects rather
/// than document actions: a peer must never replay a backup or restore
/// against a different machine's filesystem merely because it received a
/// collaborative document change. The `Show…` views change only what this
/// window shows; their ids are stable catalog identities, never paths or
/// display labels, so search and agents can target a concrete item.
#[derive(Clone, Debug, PartialEq, Eq, ui::framework::Effect)]
#[effect(operation = "backups.operation")]
pub enum BackupsEffect {
    /// Create one new checksummed local snapshot, then keep only what the
    /// retention policy keeps.
    #[effect(job, label = "Run backup now")]
    BackupNow,
    /// Choose a local source and a destination outside it.
    #[effect(job, label = "Set up backups")]
    Setup,
    /// Read every file in this stored snapshot and verify its checksum.
    #[effect(job, permission = "read", label = "Verify snapshot")]
    Verify {
        #[param("Id of the stored snapshot")]
        snapshot_id: String,
    },
    /// Choose a new or empty local folder, then restore and verify the whole
    /// snapshot.
    #[effect(job, label = "Restore snapshot into a new or empty folder…")]
    RestoreSnapshot {
        #[param("Id of the stored snapshot")]
        snapshot_id: String,
    },
    /// Show one backup source.
    #[effect(view, label = "Show source")]
    ShowSource {
        #[param("Id of the source")]
        source_id: String,
    },
    /// Show one backup destination.
    #[effect(view, label = "Show destination")]
    ShowDestination {
        #[param("Id of the destination")]
        destination_id: String,
    },
    /// Show one stored snapshot in the timeline.
    #[effect(view, label = "Show snapshot")]
    ShowSnapshot {
        #[param("Id of the stored snapshot")]
        snapshot_id: String,
    },
    /// Show the retention policy a source's snapshots follow.
    #[effect(view, label = "Show global retention policy")]
    ShowSchedule {
        #[param("Id of the source")]
        source_id: String,
    },
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

/// Retention-policy Document tree: one child per class with set / increase /
/// decrease — the same actions the on-screen affordances dispatch. Rides
/// [`crate::resource::Backups::children`], so palette traffic goes through
/// [`tilo_resource::publish`].
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

// ===========================================================================
// Live rows: the catalog on this device
// ===========================================================================

/// Why the repository effects cannot run on this device right now, for
/// programs (`code`) and people (`reason`); `None` when they can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blocked {
    pub(crate) code: &'static str,
    pub(crate) reason: String,
}

/// `effect` as the row `id` offers it, unavailable while `blocked`.
fn offer(id: &str, effect: &BackupsEffect, blocked: Option<&Blocked>) -> ActionSpec {
    let mut spec = effect.bound();
    id.clone_into(&mut spec.id);
    match blocked {
        Some(blocked) => spec.unavailable(blocked.code, blocked.reason.clone()),
        None => spec,
    }
}

fn labelled(mut spec: ActionSpec, label: &str) -> ActionSpec {
    label.clone_into(&mut spec.label);
    spec
}

fn source_rows(catalog: &Catalog) -> LiveRow {
    let mut sources = LiveRow::new("backups.sources", "backups.source-set", "Backup sources");
    for source in &catalog.sources {
        let schedule = LiveRow::new(
            format!("backups.schedule:{}", source.id),
            "backups.schedule",
            format!(
                "{} cadence · {} (observed)",
                source.kind.label(),
                source.cadence.label()
            ),
        )
        .operation(offer(
            "open",
            &BackupsEffect::ShowSchedule {
                source_id: source.id.clone(),
            },
            None,
        ));
        sources = sources.row(
            LiveRow::new(
                format!("backups.source:{}", source.id),
                "backups.source",
                format!("{} · {}", source.kind.label(), source.cadence.label()),
            )
            .operation(offer(
                "open",
                &BackupsEffect::ShowSource {
                    source_id: source.id.clone(),
                },
                None,
            ))
            .row(schedule),
        );
    }
    sources
}

fn destination_rows(catalog: &Catalog) -> LiveRow {
    let mut destinations = LiveRow::new(
        "backups.destinations",
        "backups.destination-set",
        "Backup destinations",
    );
    for destination in &catalog.destinations {
        destinations = destinations.row(
            LiveRow::new(
                format!("backups.destination:{}", destination.id),
                "backups.destination",
                format!("{} · {}", destination.name, destination.kind.label()),
            )
            .operation(offer(
                "open",
                &BackupsEffect::ShowDestination {
                    destination_id: destination.id.clone(),
                },
                None,
            )),
        );
    }
    destinations
}

/// How many recent snapshots the published tree lists; older ones resolve
/// by id ([`live_row`]) and are found through search.
const SNAPSHOT_PAGE: usize = 128;

/// The live catalog as rows beneath the resource: sources with their
/// schedules, destinations, and the recent page of stored snapshots with the
/// effects each offers on this device.
pub(crate) fn live_rows(catalog: &Catalog, blocked: Option<&Blocked>) -> Vec<LiveRow> {
    let mut snapshots = LiveRow::new(
        "backups.snapshots",
        "backups.snapshot-set",
        "Snapshot versions and runs · recent page",
    );
    for snapshot in catalog.snapshots.iter().take(SNAPSHOT_PAGE) {
        snapshots = snapshots.row(snapshot_row(snapshot, blocked));
    }
    vec![source_rows(catalog), destination_rows(catalog), snapshots]
}

/// The live row `id`, also for a snapshot outside the recent page: search
/// result addresses use these ids, so an older recovery point stays
/// invokable with the same Verify/Restore operations.
pub(crate) fn live_row(catalog: &Catalog, blocked: Option<&Blocked>, id: &str) -> Option<LiveRow> {
    let snapshot_id = id
        .strip_prefix("backups.snapshot:")
        .or_else(|| id.strip_prefix("backups.run:"))
        .or_else(|| id.strip_prefix("backups.restore-target:"))?;
    let snapshot = catalog
        .snapshots
        .iter()
        .find(|snapshot| snapshot.id == snapshot_id)?;
    if id.starts_with("backups.snapshot:") {
        return Some(snapshot_row(snapshot, blocked));
    }
    let (run, target) = snapshot_children(snapshot, blocked);
    Some(if id.starts_with("backups.run:") {
        run
    } else {
        target
    })
}

fn snapshot_row(snapshot: &Snapshot, blocked: Option<&Blocked>) -> LiveRow {
    let restore = BackupsEffect::RestoreSnapshot {
        snapshot_id: snapshot.id.clone(),
    };
    let (run, target) = snapshot_children(snapshot, blocked);
    LiveRow::new(
        format!("backups.snapshot:{}", snapshot.id),
        "backups.snapshot",
        format!(
            "{} · {} · {}",
            snapshot.time,
            snapshot.kind.label(),
            ui::format::bytes(snapshot.size_bytes)
        ),
    )
    .operation(offer(
        "open",
        &BackupsEffect::ShowSnapshot {
            snapshot_id: snapshot.id.clone(),
        },
        None,
    ))
    .operation(offer(
        "verify",
        &BackupsEffect::Verify {
            snapshot_id: snapshot.id.clone(),
        },
        blocked,
    ))
    .operation(offer("restore", &restore, blocked))
    .row(run)
    .row(target)
}

/// A stored snapshot's completed run, and the folder a restore writes to:
/// an explicit child rather than an unspoken side effect of Restore, so
/// palette and agent users get the same "new or empty folder" contract the
/// restore sheet shows.
fn snapshot_children(snapshot: &Snapshot, blocked: Option<&Blocked>) -> (LiveRow, LiveRow) {
    let run = LiveRow::new(
        format!("backups.run:{}", snapshot.id),
        "backups.run",
        format!(
            "Completed {} run · {}",
            snapshot.kind.label(),
            snapshot.time
        ),
    )
    .operation(labelled(
        offer(
            "open",
            &BackupsEffect::ShowSnapshot {
                snapshot_id: snapshot.id.clone(),
            },
            None,
        ),
        "Show run result",
    ));
    let target = LiveRow::new(
        format!("backups.restore-target:{}", snapshot.id),
        "backups.restore-target",
        "Restore target · choose a new or empty local folder",
    )
    .operation(labelled(
        offer(
            "restore",
            &BackupsEffect::RestoreSnapshot {
                snapshot_id: snapshot.id.clone(),
            },
            blocked,
        ),
        "Restore into a new or empty folder…",
    ));
    (run, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Catalog;
    use crate::resource::Backups;
    use automerge::AutoCommit;
    use tilo_actions::{Availability, LocalTransport, OperationKind};
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// The resource as the framework publishes it for `doc`.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let state = Backups::read(doc).expect("a backups record reads");
        vec![tilo_resource::publish::<Backups>(
            Backups::KIND,
            "Backups",
            &state,
        )]
    }

    fn field_resource(root: &Resource, field: PolicyField) -> &Resource {
        let policy = root
            .children
            .iter()
            .find(|resource| resource.kind == "backups.policy")
            .expect("retention-policy child");
        let id = format!("backups.retention:{}", field.key());
        policy
            .children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("a resource for {id}"))
    }

    fn row_tree(catalog: &Catalog, blocked: Option<&Blocked>) -> Vec<Resource> {
        live_rows(catalog, blocked)
            .into_iter()
            .map(LiveRow::into_resource)
            .collect()
    }

    /// The published retention tree: one child per class, bounded steppers,
    /// and only the resource's own action at the root.
    #[test]
    fn the_resource_publishes_the_retention_tree_with_bounded_steppers() {
        let mut doc = AutoCommit::new();
        let resources = published(&doc);
        let workspace = &resources[0];
        assert_eq!(workspace.kind, "backups.workspace");
        assert!(
            workspace
                .actions
                .iter()
                .filter(|action| action.kind == OperationKind::Document)
                .all(|action| action.id == "apply" || action.id == "group"),
            "retention edits ride their class, not the root"
        );
        let daily = field_resource(workspace, PolicyField::Daily);
        assert_eq!(daily.kind, "backups.retention");
        let ids: Vec<&str> = daily.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["set", "increment", "decrement"]);

        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Weekly, 0);
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Monthly, 999);
        let resources = published(&doc);
        let weekly = field_resource(&resources[0], PolicyField::Weekly);
        assert!(
            !weekly.actions.iter().any(|a| a.id == "decrement"),
            "a 0-count field can't decrement"
        );
        let monthly = field_resource(&resources[0], PolicyField::Monthly);
        assert!(
            !monthly.actions.iter().any(|a| a.id == "increment"),
            "a 999-count field can't increment"
        );
        assert!(
            resources[0]
                .children
                .iter()
                .flat_map(|resource| &resource.children)
                .flat_map(|resource| &resource.actions)
                .all(|action| action.contract.reversible
                    && action.contract.retry == RetryPolicy::Idempotent)
        );
    }

    /// The catalog's rows carry each item's effects with its id bound, and
    /// the reason this device cannot run them.
    #[test]
    fn live_rows_bind_each_items_effects_and_say_why_they_are_unavailable() {
        let catalog = Catalog::sample();
        let rows = row_tree(&catalog, None);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "backups.sources",
                "backups.destinations",
                "backups.snapshots"
            ]
        );
        let source = &rows[0].children[0];
        assert!(source.id.as_str().starts_with("backups.source:"));
        assert!(
            source.children[0]
                .id
                .as_str()
                .starts_with("backups.schedule:")
        );
        assert!(
            rows[1].children[0]
                .id
                .as_str()
                .starts_with("backups.destination:")
        );

        let first = &catalog.snapshots[0];
        let snapshot = &rows[2].children[0];
        assert_eq!(
            snapshot.id.as_str(),
            format!("backups.snapshot:{}", first.id)
        );
        let action = |id: &str| snapshot.actions.iter().find(|a| a.id == id).unwrap();
        assert_eq!(action("open").kind, OperationKind::View);
        let verify = action("verify");
        assert_eq!(verify.kind, OperationKind::Effect);
        assert_eq!(
            verify.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        // Invoking the row's action hands the handler this snapshot.
        let encoded = verify.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(encoded.type_id, BACKUPS_OPERATION_TYPE);
        assert_eq!(
            BackupsEffect::from_payload(&encoded.payload).unwrap(),
            BackupsEffect::Verify {
                snapshot_id: first.id.clone()
            }
        );
        let restore = action("restore");
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
            .expect("explicit restore target row");
        assert!(target.label.contains("new or empty"));
        assert_eq!(target.actions.len(), 1);
        assert!(
            snapshot
                .children
                .iter()
                .any(|child| child.kind == "backups.run"),
            "every stored version exposes its completed run"
        );

        let blocked = Blocked {
            code: "not_configured",
            reason: "The saved backup location is unavailable.".into(),
        };
        let rows = row_tree(&catalog, Some(&blocked));
        let snapshot = &rows[2].children[0];
        for id in ["verify", "restore"] {
            let action = snapshot.actions.iter().find(|a| a.id == id).unwrap();
            assert!(matches!(
                &action.contract.availability,
                Availability::Unavailable { code, reason }
                    if code == "not_configured" && *reason == blocked.reason
            ));
        }
        let open = snapshot.actions.iter().find(|a| a.id == "open").unwrap();
        assert_eq!(open.contract.availability, Availability::Available);
    }

    /// An older snapshot outside the published page still resolves by the id
    /// its search result names.
    #[test]
    fn snapshots_beyond_the_page_resolve_by_id() {
        let mut catalog = Catalog::sample();
        let template = catalog.snapshots[0].clone();
        catalog.snapshots = (0..SNAPSHOT_PAGE + 5)
            .map(|n| {
                let mut snapshot = template.clone();
                snapshot.id = format!("snap-{n}");
                snapshot
            })
            .collect();
        let rows = row_tree(&catalog, None);
        assert_eq!(rows[2].children.len(), SNAPSHOT_PAGE);
        let last = format!("snap-{}", SNAPSHOT_PAGE + 4);
        for prefix in [
            "backups.snapshot:",
            "backups.run:",
            "backups.restore-target:",
        ] {
            let id = format!("{prefix}{last}");
            let row = live_row(&catalog, None, &id)
                .expect("resolves")
                .into_resource();
            assert_eq!(row.id.as_str(), id);
        }
        assert!(live_row(&catalog, None, "backups.snapshot:missing").is_none());
    }

    #[test]
    fn a_set_to_the_stored_count_changes_nothing() {
        let mut doc = AutoCommit::new();
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
        seed: AutoCommit::new(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["Set daily retention", "Increase by one", "Decrease by one"],
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
}
