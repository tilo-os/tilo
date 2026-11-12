#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, TransportOrigin, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "bootstrap.growth";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceStats {
    changes: usize,
    bytes: usize,
}

fn workspace_stats(message: &NeighbourMsg) -> WorkspaceStats {
    let NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("expected workspace SyncResp")
    };
    WorkspaceStats {
        changes: changes.len(),
        // The history's own size: a change goes out compressed, and what it
        // compresses to varies with its random content.
        bytes: changes
            .iter()
            .map(|bytes| {
                automerge::Change::try_from(bytes.as_slice())
                    .expect("a workspace change")
                    .raw_bytes()
                    .len()
            })
            .sum(),
    }
}

fn workspace_delta(after: WorkspaceStats, before: WorkspaceStats) -> WorkspaceStats {
    WorkspaceStats {
        changes: after
            .changes
            .checked_sub(before.changes)
            .expect("workspace history must not shrink"),
        bytes: after
            .bytes
            .checked_sub(before.bytes)
            .expect("workspace history bytes must not shrink"),
    }
}

fn fixture_with_documents_and_expiry(
    count: usize,
    expires_at_ns: Option<u64>,
) -> (SigningKey, StatePeer, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "bootstrap growth governor".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
    let mut peer =
        StatePeer::new(WORKSPACE, cert.clone(), vec![], RevocationList::new(), now).unwrap();
    let documents = (0..count)
        .map(|index| {
            let mut document = AutoCommit::new().with_actor(automerge::ActorId::from(
                format!("bootstrap-growth-{index:02}").into_bytes(),
            ));
            document
                .put(ROOT, "value", format!("value-{index:02}"))
                .unwrap();
            document.commit_edit("fixture", "fixture");
            SeedDocument {
                app_id: APP.into(),
                record_id: format!("tab-{index:02}"),
                kind: "diagnostic".into(),
                bytes: document.save(),
            }
        })
        .collect::<Vec<_>>();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &cert, &[cert.clone()]).unwrap(),
            documents,
        },
        &key,
        now,
    )
    .unwrap();
    (key, peer, now)
}

fn fixture_with_documents(count: usize) -> (SigningKey, StatePeer, SystemTime) {
    fixture_with_documents_and_expiry(count, None)
}

fn workspace_document(message: &NeighbourMsg) -> AutoCommit {
    let NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("expected workspace SyncResp")
    };
    tilo_peer_state::workspace::candidate(None, changes).unwrap()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measurement {
    documents: usize,
    baseline: WorkspaceStats,
    first: WorkspaceStats,
    repeated: WorkspaceStats,
}

fn measure(count: usize) -> Measurement {
    let (key, mut peer, now) = fixture_with_documents(count);
    let baseline = workspace_stats(&peer.workspace_message());
    let first_batch = peer
        .publication_mut()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    assert_eq!(first_batch.len(), count + 1);
    assert!(matches!(first_batch[0], NeighbourMsg::SyncResp { .. }));
    assert_eq!(
        first_batch
            .iter()
            .skip(1)
            .filter(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
            .count(),
        count
    );
    let first_full = workspace_stats(&first_batch[0]);

    let repeated_batch = peer
        .publication_mut()
        .durable_bootstrap_messages(&key, now, 2)
        .unwrap();
    assert_eq!(repeated_batch.len(), count + 1);
    assert!(matches!(repeated_batch[0], NeighbourMsg::SyncResp { .. }));
    let repeated_full = workspace_stats(&repeated_batch[0]);

    Measurement {
        documents: count,
        baseline,
        first: workspace_delta(first_full, baseline),
        repeated: workspace_delta(repeated_full, first_full),
    }
}

#[test]
fn durable_bootstrap_workspace_history_growth_is_measured_for_one_eight_thirty_two_documents() {
    let measurements = [1, 8, 32].map(measure);
    let replay = [1, 8, 32].map(measure);
    assert_eq!(
        measurements, replay,
        "fixed actors/time must make the fixture reproducible"
    );
    for measurement in measurements {
        assert_eq!(
            measurement.first.changes, 1,
            "a durable bootstrap adds one change carrying coverage and receipts"
        );
        assert!(measurement.first.bytes > 0);
        assert_eq!(
            measurement.repeated.changes, 1,
            "repeating a durable bootstrap must not add one coverage-map change per document"
        );
        assert!(measurement.repeated.bytes > 0);
    }

    let output = measurements
        .iter()
        .map(|measurement| {
            format!(
                "documents={} baseline_changes={} baseline_bytes={} first_changes={} first_bytes={} repeated_changes={} repeated_bytes={}",
                measurement.documents,
                measurement.baseline.changes,
                measurement.baseline.bytes,
                measurement.first.changes,
                measurement.first.bytes,
                measurement.repeated.changes,
                measurement.repeated.bytes,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(output_path) = std::env::var_os("BOOTSTRAP_GROWTH_OUTPUT").map(PathBuf::from) {
        std::fs::write(output_path, format!("{output}\n")).unwrap();
    }
    eprintln!("{output}");
}

#[test]
fn durable_bootstrap_batches_exact_snapshots_coverage_receipts_and_replay() {
    let (key, baseline, now) = fixture_with_documents(8);
    let mut serial = baseline.clone();
    let mut expected_messages = Vec::new();
    for row in baseline.rows() {
        expected_messages.push(
            serial
                .publication_mut()
                .publish_document(
                    &row.app_id,
                    &row.record_id,
                    baseline.document(&row.app_id, &row.record_id).unwrap(),
                    &key,
                    now,
                )
                .unwrap(),
        );
    }
    let serial_workspace = workspace_document(&serial.workspace_message());
    let expected_coverage = tilo_peer_state::coverage::entries(&serial_workspace).unwrap();
    let expected_receipts = expected_coverage
        .iter()
        .map(|(route, coverage)| {
            (
                route.clone(),
                tilo_peer_state::durability::DurableReceipt::new(coverage.clone(), 7).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut batched = baseline;
    let messages = batched
        .publication_mut()
        .durable_bootstrap_messages(&key, now, 7)
        .unwrap();
    assert_eq!(&messages[1..], expected_messages);
    let batched_workspace = workspace_document(&messages[0]);
    assert_eq!(
        tilo_peer_state::coverage::entries(&batched_workspace).unwrap(),
        expected_coverage
    );
    assert_eq!(
        tilo_peer_state::durability::entries(&batched_workspace).unwrap(),
        expected_receipts
    );
    assert_eq!(batched.rows(), serial.rows());
    assert_eq!(batched.replay_floors(), serial.replay_floors());

    let authority = batched.owner_certificates()[0].clone();
    let mut fresh = StatePeer::new(
        WORKSPACE,
        authority.clone(),
        batched.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    for message in messages {
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
    assert_eq!(fresh.rows(), batched.rows());
    assert_eq!(fresh.replay_floors(), batched.replay_floors());
    for row in batched.rows() {
        assert_eq!(
            fresh.durable_coverage(&row.app_id, &row.record_id).unwrap(),
            batched
                .durable_coverage(&row.app_id, &row.record_id)
                .unwrap()
        );
    }
    let restored = StatePeer::restore(
        &fresh.durable().unwrap(),
        WORKSPACE,
        authority,
        fresh.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert_eq!(restored.rows(), batched.rows());
    assert_eq!(restored.replay_floors(), batched.replay_floors());
}

#[test]
fn failed_public_snapshot_batch_keeps_the_original_peer_exact() {
    let expiration = 2_500_u64 * 1_000_000_000;
    let (key, mut peer, now) = fixture_with_documents_and_expiry(8, Some(expiration));
    let before = peer.durable().unwrap();
    let error = peer
        .publication_mut()
        .signed_snapshots(&key, now + Duration::from_secs(1_000))
        .unwrap_err();
    assert!(error.contains("expired"), "unexpected failure: {error}");
    assert_eq!(peer.durable().unwrap(), before);
}

#[test]
fn empty_durable_bootstrap_writes_empty_receipts_once() {
    let measurement = measure(0);
    assert_eq!(measurement.first.changes, 1);
    // No document receipts contain a generation, so the second empty map is
    // identical and Automerge does not create another change.
    assert_eq!(measurement.repeated.changes, 0);
    assert_eq!(measurement.repeated.bytes, 0);
}

#[test]
fn nondurable_bootstrap_batches_coverage_into_one_change() {
    let (key, mut peer, now) = fixture_with_documents(8);
    let before = workspace_stats(&peer.workspace_message());
    let messages = peer
        .publication_mut()
        .bootstrap_messages(&key, now)
        .unwrap();
    assert_eq!(messages.len(), 9);
    assert_eq!(
        workspace_delta(workspace_stats(&messages[0]), before).changes,
        1
    );
}

/// Workspace governance growth per durable generation: one document
/// edited per generation among `documents`, each published through
/// `durable_mutation_messages` exactly as the Cloud owner commits it. A
/// whole-map layout (the coverage and receipt maps rewritten as one scalar
/// each) is replayed alongside on a plain document for comparison.
fn durable_generation_growth(documents: usize, generations: u64) -> (usize, usize, usize) {
    use automerge::ScalarValue;
    let (key, mut peer, now) = fixture_with_documents(documents);
    peer.publication_mut()
        .durable_bootstrap_messages(&key, now, 1)
        .unwrap();
    let start = workspace_stats(&peer.workspace_message());
    let mut whole_map = AutoCommit::new();
    let mut whole_map_bytes = 0usize;
    let mut actors = (0..documents)
        .map(|index| {
            AutoCommit::load(&peer.document(APP, &format!("tab-{index:02}")).unwrap())
                .unwrap()
                .with_actor(automerge::ActorId::from(
                    format!("growth-editor-{index:02}").into_bytes(),
                ))
        })
        .collect::<Vec<_>>();
    for generation in 2..generations + 2 {
        let index = usize::try_from(generation).unwrap() % documents;
        let before = peer.clone();
        let doc = &mut actors[index];
        let heads = doc.get_heads();
        doc.put(ROOT, "value", format!("edit-{generation}"))
            .unwrap();
        doc.commit_edit("fixture", "value");
        peer.publication_mut()
            .publish_incremental(
                APP,
                &format!("tab-{index:02}"),
                doc.save_after(&heads),
                &key,
                now,
            )
            .unwrap();
        peer.publication_mut()
            .durable_mutation_messages(&before, &key, now, generation)
            .unwrap();
        // The whole-map layout: both maps rewritten as one scalar each.
        let workspace = peer.admitted_workspace().unwrap();
        let coverage = tilo_peer_state::coverage::entries(workspace).unwrap();
        let receipts = tilo_peer_state::durability::entries(workspace).unwrap();
        let heads = whole_map.get_heads();
        whole_map
            .put(
                ROOT,
                "whole_coverage_map",
                ScalarValue::Bytes(postcard::to_allocvec(&coverage).unwrap()),
            )
            .unwrap();
        whole_map.commit_edit("fixture", "fixture");
        whole_map
            .put(
                ROOT,
                "whole_receipt_map",
                ScalarValue::Bytes(postcard::to_allocvec(&receipts).unwrap()),
            )
            .unwrap();
        whole_map.commit_edit("fixture", "fixture");
        whole_map_bytes += whole_map
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().len())
            .sum::<usize>();
    }
    let end = workspace_stats(&peer.workspace_message());
    let saved = workspace_document(&peer.workspace_message()).save().len();
    eprintln!(
        "documents={documents}: {} workspace changes over {generations} generations",
        end.changes - start.changes
    );
    let mut workspace = workspace_document(&peer.workspace_message());
    let mut changes = workspace
        .get_changes(&[])
        .into_iter()
        .rev()
        .take(3)
        .map(|change| (change.raw_bytes().len(), change.len()))
        .collect::<Vec<_>>();
    changes.reverse();
    eprintln!("last changes (bytes, ops): {changes:?}");
    (end.bytes - start.bytes, whole_map_bytes, saved)
}

#[test]
fn durable_generation_workspace_growth_is_small_per_generation() {
    let generations = 200u64;
    let mut report = Vec::new();
    for documents in [2usize, 8] {
        let (bytes, whole_map, saved) = durable_generation_growth(documents, generations);
        let per = bytes / usize::try_from(generations).unwrap();
        let whole_map_per = whole_map / usize::try_from(generations).unwrap();
        report.push(format!(
            "documents={documents} generations={generations} workspace_bytes_per_generation={per} whole_map_bytes_per_generation={whole_map_per} workspace_history_bytes={bytes} saved_workspace_bytes={saved}"
        ));
        // Only the edited route is rewritten, with compact values: well
        // below the whole-map rewrite, and flat in the route count.
        assert!(per * 5 < whole_map_per, "{}", report.join("\n"));
        assert!(per < 400, "{}", report.join("\n"));
    }
    eprintln!("{}", report.join("\n"));
}
