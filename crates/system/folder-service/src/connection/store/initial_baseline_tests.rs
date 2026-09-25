//! Wire fixtures exercise historical reducer/replay, never manufacture native authority.
use super::*;
use sha2::{Digest, Sha256};

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    ConnectionStore,
    ConnectionRecord,
) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("store");
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let binding = |n| EndpointBinding {
        identity: EndpointIdentity {
            volume: [n; 32],
            root: [n + 1; 32],
        },
        locator: EndpointLocator::new(Path::new(if n == 1 { "/source" } else { "/destination" }))
            .unwrap(),
    };
    let record = store
        .create_connection(
            ConnectionId([33; 16]),
            binding(1),
            binding(3),
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap()
        .value;
    (temp, path, store, record)
}
fn fact(store: &ConnectionStore, record: &ConnectionRecord) -> serde_json::Value {
    let connection = store.connection(record.id).unwrap();
    let mut hash = Sha256::new();
    hash.update(b"Connected explicit initial preview v1");
    hash.update(format::encode(&connection).unwrap());
    for side in [0_u8, 1] {
        hash.update([side]);
        hash.update(0_u64.to_le_bytes());
    }
    serde_json::json!({ "version": 1, "connection": record.id.0, "revision": record.revision,
        "source": connection.source.identity, "destination": connection.destination.identity,
        "preview": <[u8; 32]>::from(hash.finalize()) })
}
fn change(value: serde_json::Value) -> Change {
    serde_json::from_value(serde_json::json!({ "event": "InitialEmptyBaseline", "fact": value }))
        .expect("dedicated bounded operationless baseline event")
}
fn historical_write(store: &mut ConnectionStore, change: Change) {
    let event = Event {
        schema: 1,
        predecessor: store.digest,
        change,
    };
    let mut sequence = format::next(store.sequence).unwrap();
    framing::write_record(&mut store.log, &mut sequence, &event).unwrap();
    storage::sync_file(&store.log).unwrap();
}
// Removing the dedicated reducer loses a real baseline on replay/checkpoint;
// accepting the event through the generic append would turn history into authority.
#[test]
fn initial_empty_baseline_replays_checkpoints_without_an_operation() {
    let (_temp, path, mut store, record) = fixture();
    let evidence = fact(&store, &record);
    assert!(
        store.append(change(evidence.clone())).is_err(),
        "historical facts cannot authorize live commits"
    );
    historical_write(&mut store, change(evidence));
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(
        store.load_connection(record.id).unwrap().baseline_revision,
        Some(2)
    );
    assert!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
}

#[test]
fn initial_empty_baseline_rejects_malformed_history_and_duplicate_commit() {
    for damage in 0..8 {
        let (_temp, path, mut store, record) = fixture();
        let mut evidence = fact(&store, &record);
        match damage {
            0 => evidence["version"] = 2.into(),
            1 => evidence["revision"] = 2.into(),
            2 => evidence["preview"][0] = (evidence["preview"][0].as_u64().unwrap() ^ 1).into(),
            3 => evidence["source"]["root"][0] = 99.into(),
            4 => evidence["connection"][0] = 99.into(),
            5 => {
                store
                    .request_disconnect(record.id, record.revision)
                    .unwrap();
            }
            6 => {}
            7 => {
                store
                    .begin_intent(OperationIntent {
                        connection: record.id,
                        expected_revision: record.revision,
                        expected_baseline: None,
                        path: RelativePath::unix(b"file").unwrap(),
                        source_before: Some(EntryState::File {
                            sha256: [7; 32],
                            bytes: 3,
                            executable: false,
                        }),
                        destination_before: None,
                        decision: Decision::Propagate { from: Side::Source },
                    })
                    .unwrap();
            }
            _ => unreachable!(),
        }
        historical_write(&mut store, change(evidence.clone()));
        drop(store);
        if damage == 6 {
            let mut store = ConnectionStore::open_at(&path).unwrap();
            historical_write(&mut store, change(evidence));
            drop(store);
        }
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "reject damaged historical join {damage}"
        );
    }
}

#[test]
fn initial_empty_baseline_torn_frame_never_invents_success() {
    for cut in [0, 1, 20, 80] {
        let (_temp, path, mut store, record) = fixture();
        let before = store.log.metadata().unwrap().len();
        let evidence = fact(&store, &record);
        historical_write(&mut store, change(evidence));
        assert!(store.log.metadata().unwrap().len() > before + cut);
        store.log.set_len(before + cut).unwrap();
        storage::sync_file(&store.log).unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store.load_connection(record.id).unwrap().baseline_revision,
            None
        );
        assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    }
}

struct FaultReset;
impl Drop for FaultReset {
    fn drop(&mut self) {
        super::tests::FAULT.with(|fault| fault.set(None));
    }
}
#[test]
fn initial_empty_baseline_interrupted_checkpoint_keeps_genuine_history() {
    let mut failures = 0;
    for point in 0..12 {
        let (_temp, path, mut store, record) = fixture();
        let evidence = fact(&store, &record);
        historical_write(&mut store, change(evidence));
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        let reset = FaultReset;
        super::tests::FAULT.with(|fault| fault.set(Some((point, false))));
        if store.checkpoint().is_err() {
            failures += 1;
        }
        drop(reset);
        drop(store);
        let store = ConnectionStore::open_at(&path).unwrap();
        assert_eq!(
            store.load_connection(record.id).unwrap().baseline_revision,
            Some(2)
        );
        assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
        assert!(
            store
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        failures >= 6,
        "exercise actual CURRENT and directory durability cuts"
    );
}
#[test]
fn initial_empty_baseline_checkpoint_corruption_and_unknown_evidence_refuse() {
    for damage in 0..6 {
        let (_temp, path, mut store, record) = fixture();
        let evidence = fact(&store, &record);
        historical_write(&mut store, change(evidence));
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        store.checkpoint().unwrap();
        let mut wire = serde_json::to_value(store.connection(record.id).unwrap()).unwrap();
        match damage {
            0 => wire["initial_empty"]["version"] = 2.into(),
            1 => {
                wire["initial_empty"]["preview"][1] =
                    (wire["initial_empty"]["preview"][1].as_u64().unwrap() ^ 1).into()
            }
            2 => wire["baseline_count"] = 1.into(),
            3 => wire["baseline"] = 3.into(),
            4 => wire["initial_empty"] = serde_json::Value::Null,
            5 => wire["initial_empty"]["unexpected"] = true.into(),
            _ => unreachable!(),
        }
        // Authenticated page framing is not enough: even a rehashed malformed
        // fact in an otherwise valid empty-log checkpoint must fail to open.
        let page = serde_json::json!({"schema":1, "node":{"node":"Leaf", "rows":[{"table":"Connection", "value":wire}]}});
        let bytes = format::encode(&page).unwrap();
        let hash = format::digest(&bytes);
        store
            .storage
            .create_bytes(&store.storage.pages, &format::hex(&hash), &bytes)
            .unwrap();
        let mut next = store.current.clone();
        next.root = Some(hash);
        store
            .publish_current(&next, &store.log, &store.log_identity)
            .unwrap();
        drop(store);
        assert!(
            ConnectionStore::open_at(&path).is_err(),
            "malformed checkpoint evidence {damage}"
        );
    }
}

#[test]
fn accepted_empty_is_bound_to_explicit_complete_preview_store_and_session() {
    use crate::connection::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::initial_preview_for_test,
    };
    let (_temp, path, mut store, record) = fixture();
    let tree = |identity| {
        ObservedTree::new(
            Endpoint {
                identity,
                encoding: Encoding::Unix,
                names: NamePolicy::Exact,
            },
            [],
            Observation::Complete,
        )
        .unwrap()
    };
    let preview = || {
        initial_preview_for_test(
            &store,
            record.id,
            tree(record.source.identity),
            tree(record.destination.identity),
        )
        .unwrap()
    };
    let initial = preview();
    let unavailable = ObservedTree::new(
        Endpoint {
            identity: record.source.identity,
            encoding: Encoding::Unix,
            names: NamePolicy::Exact,
        },
        [],
        Observation::Unavailable(crate::connection::model::PendingReason::Offline),
    )
    .unwrap();
    assert!(
        initial_preview_for_test(
            &store,
            record.id,
            unavailable,
            tree(record.destination.identity)
        )
        .is_err()
    );
    let nonempty = ObservedTree::new(
        Endpoint {
            identity: record.source.identity,
            encoding: Encoding::Unix,
            names: NamePolicy::Exact,
        },
        [(RelativePath::unix(b"new").unwrap(), EntryState::Directory)],
        Observation::Complete,
    )
    .unwrap();
    let nonempty = initial_preview_for_test(
        &store,
        record.id,
        nonempty,
        tree(record.destination.identity),
    )
    .unwrap();
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
    let session = store.transaction_session(record.id).unwrap();
    assert!(nonempty.accept_empty(&store, &session).is_err());
    let accepted = initial.accept_empty(&store, &session).unwrap();
    assert!(accepted.validate(&store, &session).is_ok());
    session.request_disconnect();
    assert!(matches!(
        accepted.validate(&store, &session),
        Err(StoreError::Cancelled)
    ));
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let fresh = store.transaction_session(record.id).unwrap();
    assert!(
        accepted.validate(&store, &fresh).is_err(),
        "old acceptance is not restart authority"
    );
    assert!(store.load_baseline(record.id).unwrap().entries().is_none());
}

fn replace_checkpoint_connection(store: &ConnectionStore, wire: serde_json::Value) {
    let page = serde_json::json!({"schema":1, "node":{"node":"Leaf", "rows":[{"table":"Connection", "value":wire}]}});
    let bytes = format::encode(&page).unwrap();
    let hash = format::digest(&bytes);
    store
        .storage
        .create_bytes(&store.storage.pages, &format::hex(&hash), &bytes)
        .unwrap();
    let mut next = store.current.clone();
    next.root = Some(hash);
    store
        .publish_current(&next, &store.log, &store.log_identity)
        .unwrap();
}

// Removing any tuple join admits an unreachable state even when the page hash,
// original review digest, endpoint identities and empty baseline remain valid.
#[test]
fn initial_empty_baseline_checkpoint_requires_exact_operationless_lifecycle() {
    let mut accepted = Vec::new();
    for (lifecycle, revision, fence) in [
        (0, 3, 0),
        (1, 2, 1),
        (1, 4, 1),
        (1, 3, 2),
        (2, 2, 1),
        (2, 3, 1),
        (2, 5, 1),
        (2, 4, 2),
    ] {
        let (_temp, path, mut store, record) = fixture();
        let evidence = fact(&store, &record);
        historical_write(&mut store, change(evidence));
        drop(store);
        let mut store = ConnectionStore::open_at(&path).unwrap();
        store.checkpoint().unwrap();
        let mut wire = serde_json::to_value(store.connection(record.id).unwrap()).unwrap();
        wire["lifecycle"] = lifecycle.into();
        wire["revision"] = revision.into();
        wire["fence"] = fence.into();
        replace_checkpoint_connection(&store, wire);
        drop(store);
        match ConnectionStore::open_at(&path) {
            Err(StoreError::Corrupt) => (),
            Ok(_) => accepted.push((lifecycle, revision, fence)),
            Err(error) => panic!("checkpoint must be refused as corrupt: {error:?}"),
        }
    }
    assert!(
        accepted.is_empty(),
        "unreachable operationless checkpoint tuples accepted: {accepted:?}"
    );
}

#[test]
fn initial_empty_baseline_valid_disconnect_lifecycle_survives_checkpoint_and_reopen() {
    let (_temp, path, mut store, record) = fixture();
    let evidence = fact(&store, &record);
    historical_write(&mut store, change(evidence));
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    let current = store.load_connection(record.id).unwrap();
    assert_eq!(
        (
            current.lifecycle,
            current.revision,
            current.fence_generation
        ),
        (Lifecycle::Active, 2, 0)
    );
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    store.request_disconnect(record.id, 2).unwrap();
    let current = store.load_connection(record.id).unwrap();
    assert_eq!(
        (
            current.lifecycle,
            current.revision,
            current.fence_generation
        ),
        (Lifecycle::Disconnecting, 3, 1)
    );
    // Repeating the fence request only obtains another current-lifetime ack.
    store.request_disconnect(record.id, 3).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap(), current);
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap(), current);
    let fence = store.request_disconnect(record.id, 3).unwrap();
    let disconnected = store.finish_disconnect(&fence).unwrap().value;
    assert_eq!(
        (
            disconnected.lifecycle,
            disconnected.revision,
            disconnected.fence_generation
        ),
        (Lifecycle::Disconnected, 4, 1)
    );
    assert!(matches!(
        store.request_disconnect(record.id, 4),
        Err(StoreError::Disconnected)
    ));
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap(), disconnected);
    assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
    assert!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.transaction_session(record.id),
        Err(StoreError::Disconnected)
    ));
}

#[test]
fn initial_empty_baseline_subsequent_intent_history_keeps_its_later_revisions() {
    let (_temp, path, mut store, record) = fixture();
    let evidence = fact(&store, &record);
    historical_write(&mut store, change(evidence));
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    // This uses the production intent reducer. It is durable pending history,
    // not a native endpoint witness or a fabricated completion receipt.
    let operation = store
        .begin_intent(OperationIntent {
            connection: record.id,
            expected_revision: 2,
            expected_baseline: Some(2),
            path: RelativePath::unix(b"later").unwrap(),
            source_before: Some(EntryState::File {
                sha256: [7; 32],
                bytes: 3,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        })
        .unwrap()
        .value;
    assert_eq!(operation.serial, 1);
    let current = store.load_connection(record.id).unwrap();
    assert_eq!(
        (
            current.lifecycle,
            current.revision,
            current.fence_generation
        ),
        (Lifecycle::Active, 3, 0)
    );
    store.checkpoint().unwrap();
    drop(store);
    let mut store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap(), current);
    assert_eq!(
        store.executor_position(record.id).unwrap(),
        (Some(operation), Some(operation))
    );
    let fence = store.request_disconnect(record.id, 3).unwrap();
    let current = store.load_connection(record.id).unwrap();
    assert_eq!(
        (
            current.lifecycle,
            current.revision,
            current.fence_generation
        ),
        (Lifecycle::Disconnecting, 4, 1)
    );
    assert!(matches!(
        store.finish_disconnect(&fence),
        Err(StoreError::RecoveryRequired)
    ));
    store.checkpoint().unwrap();
    drop(store);
    let store = ConnectionStore::open_at(&path).unwrap();
    assert_eq!(store.load_connection(record.id).unwrap(), current);
    assert_eq!(
        store.executor_position(record.id).unwrap(),
        (Some(operation), Some(operation))
    );
    assert_eq!(
        store.executor_receipt(operation).unwrap().stage,
        ReceiptStage::Intent
    );
    assert!(
        store
            .load_baseline(record.id)
            .unwrap()
            .entries()
            .unwrap()
            .is_empty()
    );
}
