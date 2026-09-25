use super::super::tests::{baseline, run, snapshot_fixture};
use super::*;
use crate::https_owner::{DeliveryOperation, data::tests::register};
use qos_session_peer::ingress::SnapshotManifestPair;
#[tokio::test]
async fn cloud_snapshot_prepared_pair_is_real_durable_inert_disk_before_owner_pin() {
    let f = snapshot_fixture().await;
    register(&f);
    let prepared = baseline(&f);
    let id = prepared.selection.pin;
    let inventory = f.host_work();
    let draft = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        prepared.draft(turn, &f.session, &inventory)
    })
    .unwrap();
    let expected = draft.pair.bytes.clone();
    let expiry = draft.expires_at_ns;
    let pending = draft.persist().unwrap();
    let directory = f.root.path().join("snapshots-v1");
    let name = hex::encode(id);
    assert_eq!(
        std::fs::read(directory.join(format!("{name}.pending"))).unwrap(),
        expected
    );
    assert!(!directory.join(format!("{name}.pair")).exists());
    let pair = SnapshotManifestPair::decode(&expected).unwrap();
    let signed = pair
        .authenticate(&f.host.verifying_key(), qos_relay::clock::now_ns())
        .unwrap();
    assert_eq!(signed.current().issue().pin, id);
    assert_eq!(signed.current().issue().expires_at_ns, expiry);
    assert_eq!(signed.renewed_through(), 0);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.delivery_ledger().pins.is_empty());
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    drop(pending);
    assert!(
        directory.join(format!("{name}.pending")).exists(),
        "cancellation cannot forget physical disk custody or pretend owner commit"
    );
}

#[tokio::test]
async fn cloud_snapshot_owner_pin_barrier_precedes_canonical_pair_and_manifest_handoff() {
    let f = snapshot_fixture().await;
    register(&f);
    let prepared = baseline(&f);
    let id = prepared.selection.pin;
    let scope = prepared.selection.scope.clone();
    let inventory = f.host_work();
    let draft = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        prepared.draft(turn, &f.session, &inventory)
    })
    .unwrap();
    let expiry = draft.expires_at_ns;
    let pending = draft.persist().unwrap();
    let committed = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap();
    let directory = f.root.path().join("snapshots-v1");
    let name = hex::encode(id);
    assert!(directory.join(format!("{name}.pending")).exists());
    assert!(!directory.join(format!("{name}.pair")).exists());
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let pin = &turn.delivery_ledger().pins[&id];
        assert_eq!(pin.consumer, scope.consumer);
        assert_eq!(pin.watermark, scope.committed);
        assert_eq!(pin.expires_at_ns, expiry);
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    let live = committed.finalize().unwrap();
    assert!(!directory.join(format!("{name}.pending")).exists());
    let canonical = std::fs::read(directory.join(format!("{name}.pair"))).unwrap();
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let manifest = live.manifest(turn, &f.session)?;
        assert_eq!(
            manifest,
            SnapshotManifestPair::decode(&canonical).unwrap().current()
        );
        Ok(())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_uncertain_owner_barrier_keeps_pending_and_never_exposes_pair() {
    use crate::storage::test_support::with_save_failure;
    use atomic_file::test_support::Checkpoint;
    for checkpoint in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let f = snapshot_fixture().await;
        register(&f);
        let prepared = baseline(&f);
        let id = prepared.selection.pin;
        let inventory = f.host_work();
        let pending = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
            prepared.draft(turn, &f.session, &inventory)
        })
        .unwrap()
        .persist()
        .unwrap();
        let result = with_save_failure(f.root.path(), checkpoint, || {
            run(&f, DeliveryOperation::SnapshotCommit, |turn| {
                pending.commit(turn, &f.session)
            })
        });
        assert!(matches!(result, Err(CloudDeliveryError::OwnerUncertain(_))));
        let directory = f.root.path().join("snapshots-v1");
        let name = hex::encode(id);
        assert!(directory.join(format!("{name}.pending")).exists());
        assert!(!directory.join(format!("{name}.pair")).exists());
    }
}

#[tokio::test]
async fn cloud_snapshot_signer_rejects_changed_worker_claims_kind_and_cut() {
    let f = snapshot_fixture().await;
    register(&f);
    let prepared = baseline(&f);
    let inventory = f.host_work();
    let draft = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        prepared.draft(turn, &f.session, &inventory)
    })
    .unwrap();
    let parsed = SnapshotManifestPair::decode(&draft.pair.bytes).unwrap();
    let signed = parsed
        .authenticate(&f.host.verifying_key(), qos_relay::clock::now_ns())
        .unwrap();
    let original = signed.current().issue().clone();
    run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        assert!(
            turn.sign_snapshot_manifest(original.clone(), &draft.prepared.stream)
                .is_ok()
        );
        for change in 0..7 {
            let mut issue = original.clone();
            match change {
                0 => issue.item_count += 1,
                1 => issue.stream_bytes += 1,
                2 => issue.chunk_size -= 1,
                3 => issue.chunk_digests[0][0] ^= 1,
                4 => issue.kind = SnapshotKind::Replay,
                5 => issue.scope.committed.digest[0] ^= 1,
                6 => issue.baseline_generation += 1,
                _ => unreachable!(),
            }
            assert!(
                turn.sign_snapshot_manifest(issue, &draft.prepared.stream)
                    .is_err(),
                "changed claim {change}"
            );
        }
        assert!(turn.delivery_ledger().pins.is_empty());
        Ok(())
    })
    .unwrap();
    run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        assert!(
            turn.sign_snapshot_manifest(original, &draft.prepared.stream)
                .is_err()
        );
        Ok(())
    })
    .unwrap();
}

#[tokio::test]
async fn cloud_snapshot_replay_lifecycle_signs_actual_next_group_and_predecessor() {
    use super::super::tests::edit;
    let f = snapshot_fixture().await;
    register(&f);
    let initial = baseline(&f);
    let previous = initial.selection.scope.committed.clone();
    drop(initial);
    let cursor = run(&f, DeliveryOperation::Cursor, |turn| {
        turn.issue_delivery_cursor(&f.session, previous.clone())
    })
    .unwrap();
    edit(&f, "snapshot-replay-lifecycle");
    let prepared = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        select(
            &f.source,
            turn,
            &f.session,
            &SnapshotOperation::ReplayAfter { cursor },
        )
    })
    .unwrap()
    .prepare()
    .unwrap();
    assert_eq!(prepared.selection.predecessor, Some(previous.clone()));
    let cut = prepared.selection.scope.committed.clone();
    let pending = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        prepared.draft(turn, &f.session, &f.host_work())
    })
    .unwrap()
    .persist()
    .unwrap();
    let live = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap();
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let bytes = live.manifest(turn, &f.session)?;
        let pair = SnapshotManifestPair::new(bytes, None).unwrap();
        let signed = turn.authenticate_snapshot_pair(&pair)?;
        let issue = signed.current().issue();
        assert_eq!(issue.kind, SnapshotKind::Replay);
        assert_eq!(issue.predecessor, Some(previous));
        assert_eq!(issue.scope.committed, cut);
        assert_eq!(
            issue.baseline_generation,
            turn.delivery_ledger().replay[&cut.sequence].owner_generation
        );
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}
