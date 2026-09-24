use super::super::super::tests::{baseline, run, snapshot_fixture};
use super::*;
use crate::https_owner::data::tests::{Fixture, register};
fn pending(f: &Fixture) -> PendingSnapshot {
    let selected = baseline(f);
    run(f, DeliveryOperation::SnapshotCommit, |turn| {
        selected.draft(turn, &f.session, &f.host_work())
    })
    .unwrap()
    .persist()
    .unwrap()
}
pub(in crate::https_owner::snapshot) fn reopen(
    f: &Fixture,
    id: [u8; 16],
    digest: Digest,
) -> Result<LiveSnapshot> {
    let read = run(f, DeliveryOperation::SnapshotSelect, |turn| {
        open_recovery(&f.source, turn, &f.session, &f.host_work(), id)
    })?
    .read()?;
    let selected = run(f, DeliveryOperation::SnapshotSelect, |turn| {
        read.select(turn, &f.session, digest)
    })?;
    let prepared = selected.prepare()?;
    run(f, DeliveryOperation::SnapshotSelect, |turn| {
        prepared.reconcile(turn, &f.session)
    })?
    .finish()
}
#[tokio::test]
async fn cloud_snapshot_recovery_reopens_actual_canonical_pin_without_ack() {
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let digest = *blake3::hash(
        SnapshotManifestPair::decode(&pending.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes();
    let live = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap();
    let original = live.draft.pair.bytes.clone();
    drop(live);
    let recovered = reopen(&f, id, digest).unwrap();
    assert_eq!(recovered.draft.pair.bytes, original);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        recovered.manifest(turn, &f.session)?;
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_recovery_finalizes_only_actual_committed_pending() {
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let digest = *blake3::hash(
        SnapshotManifestPair::decode(&pending.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes();
    let committed = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap();
    drop(committed);
    let recovered = reopen(&f, id, digest).unwrap();
    let dir = f.root.path().join("snapshots-v1");
    assert!(dir.join(format!("{}.pair", hex::encode(id))).exists());
    assert!(!dir.join(format!("{}.pending", hex::encode(id))).exists());
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        recovered.manifest(turn, &f.session).map(|_| ())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_recovery_uncommitted_pending_never_exposes_or_deletes_candidate() {
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let digest = *blake3::hash(
        SnapshotManifestPair::decode(&pending.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes();
    drop(pending);
    assert!(matches!(
        reopen(&f, id, digest),
        Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))
    ));
    let dir = f.root.path().join("snapshots-v1");
    assert!(dir.join(format!("{}.pending", hex::encode(id))).exists());
    assert!(!dir.join(format!("{}.pair", hex::encode(id))).exists());
}

#[tokio::test]
async fn cloud_snapshot_recovery_rejects_invalid_signature_and_unmatched_request_digest() {
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let digest = *blake3::hash(
        SnapshotManifestPair::decode(&pending.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes();
    let live = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap();
    let mut corrupted = live.draft.pair.bytes.clone();
    drop(live);
    assert!(matches!(
        reopen(&f, id, [0x44; 32]),
        Err(CloudDeliveryError::Denied)
    ));
    *corrupted.last_mut().unwrap() ^= 1;
    let path = f
        .root
        .path()
        .join("snapshots-v1")
        .join(format!("{}.pair", hex::encode(id)));
    std::fs::write(&path, &corrupted).unwrap();
    assert!(matches!(
        reopen(&f, id, digest),
        Err(CloudDeliveryError::InvalidInput)
    ));
    assert_eq!(std::fs::read(path).unwrap(), corrupted);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.delivery_ledger().pins.contains_key(&id));
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_unknown_recovery_never_allocates_persistent_slots() {
    let f = snapshot_fixture().await;
    register(&f);
    let before = f.pool.available_bytes();
    for value in 1..=64u8 {
        assert!(matches!(
            reopen(&f, [value; 16], [value; 32]),
            Err(CloudDeliveryError::Denied)
        ));
    }
    assert_eq!(f.pool.available_bytes(), before);
    assert_eq!(
        std::fs::read_dir(f.root.path().join("snapshots-v1"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn cloud_snapshot_recovery_rejects_valid_signature_over_changed_original_stream_claims() {
    use qos_relay::delivery_v3::state::SnapshotManifest;
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let live = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap();
    let altered = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let pair = SnapshotManifestPair::decode(&live.draft.pair.bytes).unwrap();
        let authenticated = turn.authenticate_snapshot_pair(&pair)?;
        let mut issue = authenticated.current().issue().clone();
        issue.chunk_digests[0][0] ^= 1;
        // Signed inert bytes are deliberately not a current owner selection.
        Ok(SnapshotManifest::issue(issue, &f.host, turn.fresh_now_ns().unwrap()).unwrap())
    })
    .unwrap();
    let digest = altered.digest();
    let pair = SnapshotManifestPair::new(altered.as_bytes(), None).unwrap();
    let mut bytes = vec![0; pair.encoded_len()];
    pair.write_to(&mut bytes).unwrap();
    drop(live);
    let path = f
        .root
        .path()
        .join("snapshots-v1")
        .join(format!("{}.pair", hex::encode(id)));
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        reopen(&f, id, digest),
        Err(CloudDeliveryError::Denied)
    ));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.delivery_ledger().pins.contains_key(&id));
        Ok(())
    })
    .unwrap();
}

#[tokio::test]
async fn cloud_snapshot_recovery_partial_pending_is_discarded_only_after_exact_canonical_pin_proof()
{
    let f = snapshot_fixture().await;
    register(&f);
    let pending = pending(&f);
    let id = pending.draft.prepared.selection.pin;
    let live = run(&f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap();
    let canonical = live.draft.pair.bytes.clone();
    let digest =
        *blake3::hash(SnapshotManifestPair::decode(&canonical).unwrap().current()).as_bytes();
    // The actual opaque store owns/syncs this bounded inert candidate. It makes
    // no signed-content or owner-commit assertion on behalf of its caller.
    live.draft
        .reservation
        .prepare(b"partial failed manifest candidate")
        .unwrap();
    drop(live);
    let recovered = reopen(&f, id, digest).unwrap();
    assert_eq!(recovered.draft.pair.bytes, canonical);
    let dir = f.root.path().join("snapshots-v1");
    assert!(!dir.join(format!("{}.pending", hex::encode(id))).exists());
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        recovered.manifest(turn, &f.session)?;
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}
