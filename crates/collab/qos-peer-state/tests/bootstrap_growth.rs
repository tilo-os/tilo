use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use qos_peer_state::{
    NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin, seed_room,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "bootstrap.growth";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RoomStats {
    changes: usize,
    bytes: usize,
}

fn room_stats(message: &NeighbourMsg) -> RoomStats {
    let NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("expected room SyncResp")
    };
    RoomStats {
        changes: changes.len(),
        bytes: changes.iter().map(Vec::len).sum(),
    }
}

fn room_delta(after: RoomStats, before: RoomStats) -> RoomStats {
    RoomStats {
        changes: after
            .changes
            .checked_sub(before.changes)
            .expect("room history must not shrink"),
        bytes: after
            .bytes
            .checked_sub(before.bytes)
            .expect("room history bytes must not shrink"),
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
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
    let mut peer = StatePeer::new(ROOM, cert.clone(), vec![], RevocationList::new(), now).unwrap();
    let documents = (0..count)
        .map(|index| {
            let mut document = AutoCommit::new().with_actor(automerge::ActorId::from(
                format!("bootstrap-growth-{index:02}").into_bytes(),
            ));
            document
                .put(ROOT, "value", format!("value-{index:02}"))
                .unwrap();
            document.commit();
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
            room_changes: seed_room(ROOM, &cert, &[cert.clone()]).unwrap(),
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

fn room_document(message: &NeighbourMsg) -> AutoCommit {
    let NeighbourMsg::SyncResp { changes, .. } = message else {
        panic!("expected room SyncResp")
    };
    qos_peer_state::room::candidate(None, changes).unwrap()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measurement {
    documents: usize,
    baseline: RoomStats,
    first: RoomStats,
    repeated: RoomStats,
}

fn measure(count: usize) -> Measurement {
    let (key, mut peer, now) = fixture_with_documents(count);
    let baseline = room_stats(&peer.room_message());
    let first_batch = peer.durable_bootstrap_messages(&key, now, 1).unwrap();
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
    let first_full = room_stats(&first_batch[0]);

    let repeated_batch = peer.durable_bootstrap_messages(&key, now, 2).unwrap();
    assert_eq!(repeated_batch.len(), count + 1);
    assert!(matches!(repeated_batch[0], NeighbourMsg::SyncResp { .. }));
    let repeated_full = room_stats(&repeated_batch[0]);

    Measurement {
        documents: count,
        baseline,
        first: room_delta(first_full, baseline),
        repeated: room_delta(repeated_full, first_full),
    }
}

#[test]
fn durable_bootstrap_room_history_growth_is_measured_for_one_eight_thirty_two_documents() {
    let measurements = [1, 8, 32].map(measure);
    let replay = [1, 8, 32].map(measure);
    assert_eq!(
        measurements, replay,
        "fixed actors/time must make the fixture reproducible"
    );
    for measurement in measurements {
        assert_eq!(
            measurement.first.changes, 2,
            "a durable bootstrap adds one complete coverage change and one receipt change"
        );
        assert!(measurement.first.bytes > 0);
        assert_eq!(
            measurement.repeated.changes, 2,
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
    let serial_room = room_document(&serial.room_message());
    let expected_coverage = qos_peer_state::coverage::entries(&serial_room).unwrap();
    let expected_receipts = expected_coverage
        .iter()
        .map(|(route, coverage)| {
            (
                route.clone(),
                qos_peer_state::durability::DurableReceipt::new(coverage.clone(), 7).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut batched = baseline.clone();
    let messages = batched.durable_bootstrap_messages(&key, now, 7).unwrap();
    assert_eq!(&messages[1..], expected_messages);
    let batched_room = room_document(&messages[0]);
    assert_eq!(
        qos_peer_state::coverage::entries(&batched_room).unwrap(),
        expected_coverage
    );
    assert_eq!(
        qos_peer_state::durability::entries(&batched_room).unwrap(),
        expected_receipts
    );
    assert_eq!(batched.rows(), serial.rows());
    assert_eq!(batched.replay_floors(), serial.replay_floors());

    let authority = batched.authority().clone();
    let mut fresh = StatePeer::new(
        ROOM,
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
        ROOM,
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
    let before = room_stats(&peer.room_message());
    let messages = peer.bootstrap_messages(&key, now).unwrap();
    assert_eq!(messages.len(), 9);
    assert_eq!(room_delta(room_stats(&messages[0]), before).changes, 1);
}
