//! The app's effects and the live rows it publishes beside its resource. The retention
//! policy, which must replicate and undo, is the resource's model
//! ([`Backups`](crate::resource::Backups)).

use ui::framework::LiveRow;
use ui::tilo_resource;
use ui::tilo_resource::RowBindings;

use crate::model::{Catalog, Snapshot as StoredSnapshot};

/// Paths and credentials never cross this boundary: effects address the already-published
/// catalog by opaque id only.
pub const BACKUPS_OPERATION_TYPE: &str = "backups.operation";

/// The backup sources, as one row.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.source-set", label = "Backup sources")]
pub struct SourceList;

/// One backup source.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.source", label = "Backup source", effects = BackupsEffect)]
pub struct Source;

/// The retention policy observed for one source.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.schedule", label = "Schedule", effects = BackupsEffect)]
pub struct Schedule;

/// Where backups are written, as one row.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.destination-set", label = "Backup destinations")]
pub struct DestinationList;

/// One backup destination.
#[derive(tilo_resource::Row)]
#[row(
    kind = "backups.destination",
    label = "Backup destination",
    effects = BackupsEffect
)]
pub struct Destination;

/// The recent snapshot versions and backup runs, as one row.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.snapshot-set", label = "Snapshots")]
pub struct SnapshotList;

/// One stored snapshot version.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.snapshot", label = "Snapshot", effects = BackupsEffect)]
pub struct Snapshot;

/// The completed run of one stored snapshot.
#[derive(tilo_resource::Row)]
#[row(kind = "backups.run", label = "Backup run", effects = BackupsEffect)]
pub struct Run;

/// The folder a restore of one snapshot writes to: an explicit row rather than an unspoken
/// side effect of Restore, so palette and agent users get the same "new or empty folder"
/// contract the restore sheet shows.
#[derive(tilo_resource::Row)]
#[row(
    kind = "backups.restore-target",
    label = "Restore target",
    effects = BackupsEffect
)]
pub struct RestoreTarget;

/// The safe, agent-drivable side-effect surface. These are effects rather than document
/// actions: a peer must never replay a backup or restore against a different machine's
/// filesystem merely because it received a collaborative document change.
#[derive(Clone, Debug, PartialEq, Eq, ui::framework::Effect)]
#[effect(operation = "backups.operation")]
pub enum BackupsEffect {
    #[effect(kind = "outside")]
    #[effect(job, returns)]
    BackupNow,
    #[effect(kind = "outside")]
    #[effect(attended, job)]
    Setup,
    /// Read snapshot.
    #[effect(view, returns, on = Snapshot)]
    #[effect(label = "backups-operation-read-snapshot")]
    #[effect(tag = tilo_actions::DOCUMENT_READ_TAG)]
    Read {
        #[param("The snapshot row's id, or the snapshot's id")]
        snapshot_id: String,
    },
    /// Open.
    ///
    /// Show what the row is in the window: a source, its schedule, a destination, a stored
    /// snapshot or the run that made it.
    #[effect(
        view,
        on = Source,
        on = Schedule,
        on = Destination,
        on = Snapshot,
        on = Run
    )]
    #[effect(label = "backups-operation-open")]
    Open {
        #[param("The backup row to show")]
        row: String,
    },
    /// Verify snapshot.
    #[effect(job, returns, kind = "read", on = Snapshot)]
    Verify {
        #[param("The snapshot row's id, or the snapshot's id")]
        snapshot_id: String,
    },
    #[effect(kind = "outside", on = Snapshot, on = RestoreTarget)]
    #[effect(attended, job, returns)]
    #[effect(label = "backups-operation-restore-snapshot")]
    Restore {
        #[param("The snapshot row's id, or the snapshot's id")]
        snapshot_id: String,
    },
    #[effect(view, idempotent)]
    OrderSnapshots {
        #[param("How to list them", choices = ORDERS.iter().copied())]
        order: String,
    },
}

pub const ORDERS: [&str; 2] = ["Newest first", "Oldest first"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blocked {
    pub(crate) code: &'static str,
    pub(crate) reason: String,
}

fn source_rows(catalog: &Catalog) -> LiveRow {
    let mut sources = LiveRow::typed::<SourceList>("backups.sources", "Backup sources");
    for source in &catalog.sources {
        let schedule = open_row::<Schedule>(
            format!("backups.schedule:{}", source.id),
            format!(
                "Observed {} cadence: {}.",
                source.kind.label(),
                crate::view::schedule_label(source.schedule)
            ),
        );
        sources = sources.row(
            open_row::<Source>(
                format!("backups.source:{}", source.id),
                format!(
                    "{} with {}.",
                    source.kind.label(),
                    crate::view::schedule_label(source.schedule)
                ),
            )
            .row(schedule),
        );
    }
    sources
}

fn destination_rows(catalog: &Catalog) -> LiveRow {
    let mut destinations =
        LiveRow::typed::<DestinationList>("backups.destinations", "Backup destinations");
    for destination in &catalog.destinations {
        destinations = destinations.row(open_row::<Destination>(
            format!("backups.destination:{}", destination.id),
            format!(
                "{} is a {} backup destination.",
                destination.name,
                destination.kind.label()
            ),
        ));
    }
    destinations
}

const SNAPSHOT_PAGE: usize = 128;

/// The live catalog as rows beneath the resource: sources with their schedules,
/// destinations, and the recent snapshots.
pub(crate) fn live_rows(catalog: &Catalog) -> Vec<LiveRow> {
    let mut snapshots = LiveRow::typed::<SnapshotList>(
        "backups.snapshots",
        "Recent snapshot versions and backup runs",
    );
    for snapshot in catalog.snapshots.iter().take(SNAPSHOT_PAGE) {
        snapshots = snapshots.row(snapshot_row(snapshot));
    }
    vec![source_rows(catalog), destination_rows(catalog), snapshots]
}

/// The live row `id`, also for a snapshot outside the recent page: a search result's
/// address keeps its Verify and Restore operations.
pub(crate) fn live_row(catalog: &Catalog, id: &str) -> Option<LiveRow> {
    let snapshot_id = id
        .strip_prefix("backups.snapshot:")
        .or_else(|| id.strip_prefix("backups.run:"))
        .or_else(|| id.strip_prefix("backups.restore-target:"))?;
    let snapshot = catalog
        .snapshots
        .iter()
        .find(|snapshot| snapshot.id == snapshot_id)?;
    if id.starts_with("backups.snapshot:") {
        return Some(snapshot_row(snapshot));
    }
    let (run, target) = snapshot_children(snapshot);
    Some(if id.starts_with("backups.run:") {
        run
    } else {
        target
    })
}

/// A row of the type `R` whose `open` shows it, and whose other effects, when the type
/// offers them, are bound to `bindings`.
fn row<R: ui::tilo_resource::words::RowType>(
    id: String,
    label: String,
    bindings: impl FnOnce(RowBindings) -> RowBindings,
) -> LiveRow {
    let bindings = bindings(RowBindings::new().bind(
        BackupsEffect::OPEN,
        [("row", serde_json::Value::from(id.clone()))],
    ));
    LiveRow::typed_with::<R>(id, label, &bindings)
}

/// A row of the type `R` whose `open` shows it.
fn open_row<R: ui::tilo_resource::words::RowType>(id: String, label: String) -> LiveRow {
    row::<R>(id, label, |bindings| bindings)
}

fn snapshot_row(snapshot: &StoredSnapshot) -> LiveRow {
    let (run, target) = snapshot_children(snapshot);
    let id = format!("backups.snapshot:{}", snapshot.id);
    let snapshot_of = |bindings: RowBindings| {
        bindings
            .bind(
                BackupsEffect::READ,
                [("snapshot_id", serde_json::Value::from(snapshot.id.clone()))],
            )
            .bind(
                BackupsEffect::VERIFY,
                [("snapshot_id", serde_json::Value::from(snapshot.id.clone()))],
            )
            .bind(
                BackupsEffect::RESTORE,
                [("snapshot_id", serde_json::Value::from(snapshot.id.clone()))],
            )
    };
    row::<Snapshot>(
        id,
        format!(
            "{} {} snapshot containing {}.",
            taken_label(snapshot),
            snapshot.kind.label(),
            ui::format::size(snapshot.size_bytes)
        ),
        snapshot_of,
    )
    .row(run)
    .row(target)
}

fn snapshot_children(snapshot: &StoredSnapshot) -> (LiveRow, LiveRow) {
    let run = open_row::<Run>(
        format!("backups.run:{}", snapshot.id),
        format!(
            "Completed {} run on {}.",
            snapshot.kind.label(),
            taken_label(snapshot)
        ),
    );
    let target = row::<RestoreTarget>(
        format!("backups.restore-target:{}", snapshot.id),
        "Restore into a new or empty local folder".to_owned(),
        |bindings| {
            bindings.bind(
                BackupsEffect::RESTORE,
                [("snapshot_id", serde_json::Value::from(snapshot.id.clone()))],
            )
        },
    );
    (run, target)
}

pub(crate) fn taken_label(snapshot: &StoredSnapshot) -> String {
    ui::format::Formats::current().timestamp(snapshot.taken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionKind, OperationKind, ResourceSpec};
    use ui::tilo_resource::Effect as _;

    fn row_tree(catalog: &Catalog) -> Vec<ResourceSpec> {
        live_rows(catalog)
            .into_iter()
            .map(LiveRow::into_resource)
            .collect()
    }

    /// Each row is its declared type, offering that type's effects bound to it.
    #[test]
    fn live_rows_offer_their_types_effects_bound_to_the_row() {
        let catalog = Catalog::sample(ui::clock::local_now().datetime());
        let rows = row_tree(&catalog);
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
        assert_eq!(action("read").kind, OperationKind::View);
        assert_eq!(
            action("read").contract.kind,
            ActionKind::Read,
            "a snapshot read reads the app's document"
        );
        let verify = action("verify");
        assert_eq!(verify.kind, OperationKind::Effect);
        assert_eq!(verify.contract.kind, ActionKind::Read);
        let encoded = verify.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(encoded.type_id, BACKUPS_OPERATION_TYPE);
        assert_eq!(
            BackupsEffect::from_payload(&encoded.payload).unwrap(),
            BackupsEffect::Verify {
                snapshot_id: first.id.clone(),
            },
            "the row binds the snapshot it is"
        );
        let restore = action("restore");
        assert_eq!(restore.contract.kind, ActionKind::Outside);
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
        assert_eq!(target.actions[0].id, "restore");
        assert!(
            snapshot
                .children
                .iter()
                .any(|child| child.kind == "backups.run"),
            "every stored version exposes its completed run"
        );
    }

    /// An older snapshot outside the published page still resolves by the id its search
    /// result names.
    #[test]
    fn snapshots_beyond_the_page_resolve_by_id() {
        let mut catalog = Catalog::sample(ui::clock::local_now().datetime());
        let template = catalog.snapshots[0].clone();
        catalog.snapshots = (0..SNAPSHOT_PAGE + 5)
            .map(|n| {
                let mut snapshot = template.clone();
                snapshot.id = format!("snap-{n}");
                snapshot
            })
            .collect();
        let rows = row_tree(&catalog);
        assert_eq!(rows[2].children.len(), SNAPSHOT_PAGE);
        let last = format!("snap-{}", SNAPSHOT_PAGE + 4);
        for prefix in [
            "backups.snapshot:",
            "backups.run:",
            "backups.restore-target:",
        ] {
            let id = format!("{prefix}{last}");
            let row = live_row(&catalog, &id).expect("resolves").into_resource();
            assert_eq!(row.id.as_str(), id);
        }
        assert!(live_row(&catalog, "backups.snapshot:missing").is_none());
    }
}
