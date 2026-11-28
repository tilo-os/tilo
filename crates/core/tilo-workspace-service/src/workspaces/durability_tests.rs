//! What the workspace service answered is on disk before it answered: a part
//! that ends as a killed process ends loses nothing it said was done.

#[cfg(feature = "device-keys")]
use automerge::ReadDoc as _;

use super::fixtures::{hosting_device, test_host};
#[cfg(feature = "device-keys")]
use super::fixtures::{test_host_keyed, write_record};

/// The record `(app, record)` of `workspace` as this service holds it now, opened.
#[cfg(feature = "device-keys")]
async fn read_record(
    workspaces: &super::Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
) -> automerge::AutoCommit {
    let bytes = workspaces
        .resource_document(workspace, app, record)
        .await
        .expect("the record reads")
        .expect("the workspace holds the record");
    automerge::AutoCommit::load(&bytes).expect("the record opens")
}

#[tokio::test]
async fn a_created_workspace_is_held_after_a_part_ends_that_saved_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let made = server
        .workspaces()
        .create("Studio")
        .await
        .expect("the workspace is made");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);

    // The next part opens the same store: the workspace was held before the create answered.
    let reopened = test_host(dir.path(), &key, &cert).await;
    let held = reopened
        .workspaces()
        .workspace(&made.workspace)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the workspace {} the create answered is held after a part that saved nothing: \
                 {error}; the files on disk: {:?}",
                made.workspace,
                std::fs::read_dir(dir.path().join("workspaces"))
                    .map(|entries| entries
                        .flatten()
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            )
        });
    assert_eq!(
        held.lock().await.summary().name,
        "Studio",
        "the workspace the create answered is the one held"
    );
    drop(workspaces);
    reopened.shutdown().await;
}

#[tokio::test]
async fn the_persons_workspace_is_held_after_a_part_ends_that_saved_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let own = super::person_workspace::workspace_id(server.workspaces());
    super::person_workspace::found(server.workspaces())
        .await
        .expect("the person's workspace is founded here");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);

    let reopened = test_host(dir.path(), &key, &cert).await;
    reopened
        .workspaces()
        .workspace(&own)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "the person's own workspace {own} the founding answered is held after a part \
                 that saved nothing: {error}; the files on disk: {:?}",
                std::fs::read_dir(dir.path().join("workspaces"))
                    .map(|entries| entries
                        .flatten()
                        .map(|entry| entry.file_name())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            )
        });
    assert!(
        super::person_workspace::holds_person_workspace(reopened.workspaces()).await,
        "the reopened service names this device as one holding the person's workspace"
    );
    drop(workspaces);
    reopened.shutdown().await;
}

#[cfg(feature = "device-keys")]
#[tokio::test]
async fn a_record_a_part_wrote_is_there_after_it_ends_without_a_save() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    // A device whose channel service keeps its keys, as a device's does: they open what it sealed
    // when it starts again.
    let (server, keys) = test_host_keyed(dir.path(), &key, &cert).await;
    let workspace = server
        .workspaces()
        .create("Studio")
        .await
        .expect("the workspace is made")
        .workspace;
    write_record(
        server.workspaces(),
        &workspace,
        ("so.tilo.notes", "note"),
        |doc| {
            use automerge::transaction::Transactable as _;
            doc.put(automerge::ROOT, "title", "Groceries")
                .map_err(|error| error.to_string())
        },
    )
    .await
    .expect("the record is written");
    // The part ends as the process being killed does: no stop, and nothing saved on the way out.
    let workspaces = server.workspaces().clone();
    drop(server);
    drop(keys);

    let (reopened, keys) = test_host_keyed(dir.path(), &key, &cert).await;
    // The app that keeps the record declares its kind where it runs again, as every embedder
    // does: what names the record is the store's, not a surface's.
    super::fixtures::declare_record_kind(reopened.workspaces(), "so.tilo.notes");
    let record = read_record(reopened.workspaces(), &workspace, ("so.tilo.notes", "note")).await;
    assert_eq!(
        record
            .get(automerge::ROOT, "title")
            .expect("the title reads")
            .map(|(value, _)| value.to_string()),
        Some("\"Groceries\"".to_owned()),
        "the record a part wrote is there after it ends without a save"
    );
    drop(workspaces);
    drop(keys);
    reopened.shutdown().await;
}

/// A workspace on a host, filled with records, for measuring what a write in it costs.
#[cfg(feature = "device-keys")]
struct Measured {
    _dir: tempfile::TempDir,
    key: ed25519_dalek::SigningKey,
    server: super::fixtures::TestHost,
    workspace: String,
    records: usize,
}

/// The median and the 99th percentile of `costs`.
#[cfg(feature = "device-keys")]
fn spread(mut costs: Vec<std::time::Duration>) -> (std::time::Duration, std::time::Duration) {
    costs.sort();
    let at = |share: usize| {
        let at = costs.len().saturating_mul(share) / 100;
        costs
            .get(at.min(costs.len().saturating_sub(1)))
            .copied()
            .unwrap_or_default()
    };
    (at(50), at(99))
}

#[cfg(feature = "device-keys")]
impl Measured {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspace = server
            .workspaces()
            .create("Studio")
            .await
            .expect("the workspace is made")
            .workspace;
        Self {
            _dir: dir,
            key,
            server,
            workspace,
            records: 0,
        }
    }

    /// Add records of `size` bytes until the workspace holds `records` of them, kept as one
    /// change.
    async fn fill(&mut self, records: usize, size: usize) {
        let held = self
            .server
            .workspaces()
            .workspace(&self.workspace)
            .await
            .unwrap();
        let mut held = held.lock().await;
        let state = held.collaboration().clone();
        let mut candidate = held.peer().clone();
        for n in self.records..records {
            use automerge::transaction::Transactable as _;
            // Text no compression makes small, so the record keeps its size in the file.
            let mut text = vec![0u8; size / 2];
            blake3::Hasher::new()
                .update(&n.to_le_bytes())
                .finalize_xof()
                .fill(&mut text);
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "text", hex::encode(text)).unwrap();
            candidate
                .publication_mut()
                .publish_document(
                    "so.tilo.bulk",
                    &format!("r{n}"),
                    doc.save(),
                    &self.key,
                    tilo_clock::wall(),
                )
                .unwrap();
        }
        held.keep(candidate, &self.key, tilo_clock::wall(), |_, _| {
            Ok(state.clone())
        })
        .unwrap();
        drop(held);
        self.records = records;
    }

    /// What one write costs its caller over `writes` writes: of one record written again and
    /// again, and of new records, each the median and the 99th percentile.
    async fn cost(&self, writes: usize) -> [(std::time::Duration, std::time::Duration); 2] {
        let mut costs = [Vec::with_capacity(writes), Vec::with_capacity(writes)];
        for n in 0..writes {
            for (new, costs) in costs.iter_mut().enumerate() {
                let record = if new == 1 {
                    format!("note {} {n}", self.records)
                } else {
                    "note".to_owned()
                };
                let started = std::time::Instant::now();
                write_record(
                    self.server.workspaces(),
                    &self.workspace,
                    ("so.tilo.notes", &record),
                    |doc| {
                        use automerge::transaction::Transactable as _;
                        doc.put(automerge::ROOT, "title", format!("Draft {n}"))
                            .map_err(|error| error.to_string())
                    },
                )
                .await
                .expect("the record is written");
                costs.push(started.elapsed());
            }
        }
        costs.map(spread)
    }
}

/// A write is answered once it is on disk, and what keeping it costs is its own size, not the
/// workspace's: a workspace whose file holds megabytes of records answers a write about as fast
/// as an empty one.
#[cfg(feature = "device-keys")]
#[tokio::test(flavor = "multi_thread")]
async fn a_write_costs_the_same_in_a_large_workspace() {
    let mut measured = Measured::new().await;
    let [(small_median, small_p99), _] = measured.cost(100).await;
    // Records small enough to be kept in the workspace file itself, which then holds 3 MB.
    measured.fill(300, 10 * 1024).await;
    let [(large_median, large_p99), _] = measured.cost(100).await;
    measured.server.shutdown().await;
    eprintln!(
        "a write: median {small_median:?}, p99 {small_p99:?} in an empty workspace; median \
         {large_median:?}, p99 {large_p99:?} in one of 3 MB"
    );
    let bound = |small: std::time::Duration| {
        small
            .saturating_mul(3)
            .saturating_add(std::time::Duration::from_millis(10))
    };
    assert!(
        large_median <= bound(small_median) && large_p99 <= bound(small_p99),
        "a write in a large workspace costs what one in an empty workspace does: median \
         {small_median:?} against {large_median:?}, p99 {small_p99:?} against {large_p99:?}"
    );
}

/// A write to a record, and the write that makes a new one, are answered within 10 ms however
/// many records the workspace holds: at 300, 3,000 and 10,000 records of 1 KB. Built optimised,
/// as a host runs: `t test workspace-write-cost`.
#[cfg(feature = "device-keys")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measured in an optimised build, by the workspace-write-cost check"]
async fn a_write_is_answered_in_10_ms_at_ten_thousand_records() {
    let bound = std::time::Duration::from_millis(10);
    let mut measured = Measured::new().await;
    let mut over = Vec::new();
    for records in [300, 3_000, 10_000] {
        measured.fill(records, 1024).await;
        let [(median, p99), (new_median, new_p99)] = measured.cost(200).await;
        eprintln!(
            "{records} records: a write median {median:?}, p99 {p99:?}; a new record median \
             {new_median:?}, p99 {new_p99:?}"
        );
        if p99 > bound || new_p99 > bound {
            over.push(format!(
                "{records} records: a write's p99 {p99:?}, a new record's {new_p99:?}"
            ));
        }
    }
    measured.server.shutdown().await;
    assert!(
        over.is_empty(),
        "a write is answered within {bound:?}: {over:?}"
    );
}
