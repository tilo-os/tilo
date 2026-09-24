use super::super::super::tests::{baseline, run, snapshot_fixture};
use super::*;
use crate::https_owner::data::tests::{Fixture, register};
use qos_relay::delivery_v3::{
    framing::{MessageType, decode_frame_prefix},
    state::{CommitBoundary, SnapshotChunk},
};
pub(in crate::https_owner::snapshot) fn live(f: &Fixture) -> LiveSnapshot {
    let selected = baseline(f);
    let pending = run(f, DeliveryOperation::SnapshotCommit, |turn| {
        selected.draft(turn, &f.session, &f.host_work())
    })
    .unwrap()
    .persist()
    .unwrap();
    run(f, DeliveryOperation::SnapshotCommit, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap()
}
#[tokio::test]
async fn cloud_snapshot_chunk_actual_stream_boundary_never_acknowledges() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let chunks = live.draft.prepared.stream.chunk_digests().len();
    let original = live.draft.pair.bytes.clone();
    let requested_digest =
        *blake3::hash(SnapshotManifestPair::decode(&original).unwrap().current()).as_bytes();
    let job = live
        .prepare_chunk((chunks - 1) as u32, requested_digest)
        .unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    assert_eq!(live.served_through, chunks as u64);
    let (kind, payload, rest) = decode_frame_prefix(reply.bytes()).unwrap();
    assert_eq!(kind, MessageType::SnapshotChunk);
    let chunk = SnapshotChunk::decode(payload).unwrap();
    SnapshotManifestPair::decode(&original)
        .unwrap()
        .authenticate(&f.host.verifying_key(), qos_relay::clock::now_ns())
        .unwrap()
        .current()
        .validate_chunk(&chunk)
        .unwrap();
    let (kind, payload, rest) = decode_frame_prefix(rest).unwrap();
    assert_eq!(kind, MessageType::CommitBoundary);
    let boundary = CommitBoundary::decode(payload).unwrap();
    assert_eq!(
        boundary.committed(),
        &live.draft.prepared.selection.scope.committed
    );
    assert!(rest.is_empty());
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        assert_eq!(
            turn.delivery_ledger().pins[&live.draft.prepared.selection.pin].expires_at_ns,
            live.draft.expires_at_ns
        );
        Ok(())
    })
    .unwrap();
}

#[tokio::test]
async fn cloud_snapshot_chunk_digest_refusal_at_fresh_authorization_emits_no_retargeted_reply() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let id = live.draft.prepared.selection.pin;
    let requested = *blake3::hash(
        SnapshotManifestPair::decode(&live.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes();
    let mut job = live.prepare_chunk(0, requested).unwrap();
    // Alter only the inert requested digest on a genuinely created paid worker
    // result, to exercise the second comparison at the fresh disclosure turn.
    job.requested_digest = [0x7b; 32];
    let result = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    });
    assert!(matches!(result, Err(CloudDeliveryError::Denied)));
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
