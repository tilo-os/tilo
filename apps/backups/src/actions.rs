//! The Backups app's **effects** and the **live rows** it publishes beside
//! its resource.
//!
//! Backups keeps almost all of its state *outside* the shared document: the
//! catalog of sources, snapshots and destinations is projected live from the
//! storage backend on this device, and running/verifying/restoring a snapshot
//! are [`BackupsEffect`]s, not document changes. The retention policy, which
//! must replicate and undo, is the resource's model
//! ([`Backups`](crate::resource::Backups)), edited at its paths.

use tilo_actions::ActionSpec;
use ui::framework::{Effect, LiveRow};
use ui::tilo_resource;

use crate::model::{Catalog, Snapshot};

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
#[effect(operation = "backups.operation", device_only)]
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
                "Observed {} cadence: {}.",
                source.kind.label(),
                crate::view::schedule_label(source.schedule)
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
                format!(
                    "{} with {}.",
                    source.kind.label(),
                    crate::view::schedule_label(source.schedule)
                ),
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
                format!(
                    "{} is a {} backup destination.",
                    destination.name,
                    destination.kind.label()
                ),
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
        "Recent snapshot versions and backup runs",
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
            "{} {} snapshot containing {}.",
            taken_label(snapshot),
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
            "Completed {} run on {}.",
            snapshot.kind.label(),
            taken_label(snapshot)
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
        "Restore into a new or empty local folder",
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

pub(crate) fn taken_label(snapshot: &Snapshot) -> String {
    use chrono::{Datelike, Timelike};
    let at = snapshot.taken;
    format!(
        "{} {} {} at {:02}:{:02}",
        at.day(),
        ui::format::month_name(at.month0() as usize),
        at.year(),
        at.hour(),
        at.minute()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Catalog;
    use tilo_actions::{Availability, OperationKind, Resource};

    fn row_tree(catalog: &Catalog, blocked: Option<&Blocked>) -> Vec<Resource> {
        live_rows(catalog, blocked)
            .into_iter()
            .map(LiveRow::into_resource)
            .collect()
    }

    /// The catalog's rows carry each item's effects with its id bound, and
    /// the reason this device cannot run them.
    #[test]
    fn live_rows_bind_each_items_effects_and_say_why_they_are_unavailable() {
        let catalog = Catalog::sample(ui::clock::local_now().naive_local());
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
        let mut catalog = Catalog::sample(ui::clock::local_now().naive_local());
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
}
