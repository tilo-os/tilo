//! What "New note" writes into the room record reaches another replica whole: every change it
//! made follows the changes it depends on, and a replica that receives them the way a window
//! takes committed changes from the room ends up with the same heads and the same record.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Map;
use tilo_automerge::ApplyIncoming as _;
use tilolang_tilo::action::resource_ids;
use ui::automerge::{ActorId, AutoCommit, ChangeHash, ReadDoc as _};
use ui::framework::testing::HostedApp;

const NOTES: &str = "so.tilo.lang.notes";

fn settle(app: &mut HostedApp) {
    for _ in 0..12 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

/// Counts the error events logged while it is the thread's subscriber.
struct Errors(Arc<AtomicUsize>);

impl tracing::Subscriber for Errors {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::ERROR
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Every resource `resources` publishes, with the ones under them.
fn flattened(resources: &[ui::tilo_actions::Resource]) -> Vec<&ui::tilo_actions::Resource> {
    resources
        .iter()
        .flat_map(|resource| std::iter::once(resource).chain(flattened(&resource.children)))
        .collect()
}

#[test]
fn a_new_note_reaches_another_replica_in_order_and_whole() {
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    settle(&mut app);
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let (before, held) = app.read(|doc| {
        let record = doc.committed_document();
        (record.get_heads(), record.save())
    });
    // Another device, holding the record as it was.
    let mut replica = AutoCommit::load(&held).unwrap();

    let resources = app.harness().resources();
    let new_note = flattened(&resources)
        .into_iter()
        .find(|resource| resource.id.as_str() == notebook)
        .and_then(|resource| resource.actions.iter().find(|a| a.label == "New note"))
        .map(|action| action.id.clone())
        .expect("the notebook publishes New note");
    app.invoke(None, None, &notebook, &new_note, &Map::new(), false)
        .unwrap();
    settle(&mut app);

    let (after, changes, delta, whole) = app.read(|doc| {
        let record = doc.committed_document();
        let changes = record.get_changes(&before);
        let delta: Vec<u8> = changes
            .iter()
            .flat_map(|change| change.raw_bytes().to_vec())
            .collect();
        let whole = record.hydrate(None);
        (record.get_heads(), changes, delta, whole)
    });
    assert!(!changes.is_empty(), "New note changed the room record");

    // Each change depends only on what the record held before or on a change listed ahead of
    // it, and continues its actor's sequence with no gap.
    let mut known: BTreeSet<ChangeHash> = BTreeSet::new();
    let mut seq: BTreeMap<ActorId, u64> = BTreeMap::new();
    for change in &changes {
        for dep in change.deps() {
            assert!(
                known.contains(dep) || replica.get_change_by_hash(dep).is_some(),
                "change {} depends on {dep}, which neither the record before nor an earlier change holds",
                change.hash()
            );
        }
        let last = seq.entry(change.actor_id().clone()).or_insert_with(|| {
            replica
                .get_changes(&[])
                .iter()
                .filter(|held| held.actor_id() == change.actor_id())
                .map(ui::automerge::Change::seq)
                .max()
                .unwrap_or(0)
        });
        assert_eq!(
            change.seq(),
            *last + 1,
            "{} skips its actor's sequence",
            change.hash()
        );
        *last = change.seq();
        known.insert(change.hash());
    }

    // The replica takes the changes as a window takes committed changes from the room.
    let errors = Arc::new(AtomicUsize::new(0));
    tracing::subscriber::with_default(Errors(Arc::clone(&errors)), || {
        replica.load_incoming_whole(&delta).unwrap();
    });
    assert_eq!(
        errors.load(Ordering::Relaxed),
        0,
        "receiving the changes logged an error"
    );
    assert!(
        replica.get_missing_deps(&[]).is_empty(),
        "the replica waits for changes it was never sent"
    );
    let mut heads = replica.get_heads();
    let mut expected = after;
    heads.sort_unstable();
    expected.sort_unstable();
    assert_eq!(heads, expected, "the replica's heads are the room record's");
    assert_eq!(
        replica.document().hydrate(None),
        whole,
        "the replica holds the same record"
    );
}
