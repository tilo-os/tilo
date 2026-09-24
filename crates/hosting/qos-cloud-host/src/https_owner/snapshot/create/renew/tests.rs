use super::super::super::tests::{run, snapshot_fixture};
use super::super::chunk::tests::live;
use super::super::recover::tests::reopen;
use super::*;
use crate::https_owner::data::tests::register;
fn digest(live: &LiveSnapshot) -> Digest {
    *blake3::hash(
        SnapshotManifestPair::decode(&live.draft.pair.bytes)
            .unwrap()
            .current(),
    )
    .as_bytes()
}
#[tokio::test]
async fn cloud_snapshot_renew_commits_progress_and_previous_retry_without_extending() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let original = digest(&live);
    let id = live.draft.prepared.selection.pin;
    let old_expiry = live.draft.expires_at_ns;
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(0, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    let Renewal::Pending(draft) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        live.renew(turn, &f.session, original)
    })
    .unwrap() else {
        panic!("new durable server progress required")
    };
    let pending = draft.persist().unwrap();
    let committed = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap();
    let live = committed.finalize().unwrap();
    assert!(live.draft.expires_at_ns > old_expiry);
    let renewed = live.draft.pair.bytes.clone();
    let expiry = live.draft.expires_at_ns;
    let pair = SnapshotManifestPair::decode(&renewed).unwrap();
    assert_eq!(pair.renewed_through(), 1);
    assert_eq!(*blake3::hash(pair.previous().unwrap()).as_bytes(), original);
    drop(live);
    let live = reopen(&f, id, original).unwrap();
    assert_eq!(live.served_through, 1);
    let Renewal::Unchanged(live) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        live.renew(turn, &f.session, original)
    })
    .unwrap() else {
        panic!("lost-reply previous anchor must be idempotent")
    };
    assert_eq!(live.draft.pair.bytes, renewed);
    assert_eq!(live.draft.expires_at_ns, expiry);
    let current = digest(&live);
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(0, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    assert!(matches!(
        run(&f, DeliveryOperation::SnapshotRenew, |turn| live
            .renew(turn, &f.session, current)),
        Err(CloudDeliveryError::Denied)
    ));
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert_eq!(turn.delivery_ledger().pins[&id].expires_at_ns, expiry);
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}

async fn clock_fixture() -> (
    crate::https_owner::data::tests::Fixture,
    qos_action_broker::peer_actions::OwnerPolicyClock,
    qos_action_broker::peer_actions::PolicyClockTestControl,
) {
    use crate::https_owner::{Current, data::tests::authenticate_with_capabilities};
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    let mut f = snapshot_fixture().await;
    let (clock, control) = OwnerPolicyClock::for_test(qos_relay::clock::now_ns() + 1_000_000_000);
    let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    f.source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let origin = f.session.binding().origin.clone();
    f.session = match f
        .source
        .with_current(|turn| authenticate_with_capabilities(turn, &f.host, &origin, 9))
    {
        Current::Current(session) => session,
        _ => panic!("actual fresh clock-backed session"),
    };
    (f, clock, control)
}
#[tokio::test]
async fn cloud_snapshot_renew_recovery_selects_live_committed_pending_over_expired_canonical() {
    use crate::https_owner::{Current, data::tests::authenticate_with_capabilities};
    use std::time::Duration;
    let (mut f, clock, control) = clock_fixture().await;
    register(&f);
    let live = live(&f);
    let original = digest(&live);
    let id = live.draft.prepared.selection.pin;
    let canonical = live.draft.pair.bytes.clone();
    control.advance_monotonic(Duration::from_secs(120));
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(0, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    let Renewal::Pending(draft) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        live.renew(turn, &f.session, original)
    })
    .unwrap() else {
        panic!("progress renewal")
    };
    let pending = draft.persist().unwrap();
    let committed = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap();
    // Crash window: the new actual owner pin committed, canonical rename did not.
    drop(committed);
    control.advance_monotonic(Duration::from_secs(240));
    let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    f.source = CloudStoreSource::bind_with_clock(&store, clock.clone()).unwrap();
    drop(store);
    let origin = f.session.binding().origin.clone();
    f.session = match f
        .source
        .with_current(|turn| authenticate_with_capabilities(turn, &f.host, &origin, 9))
    {
        Current::Current(session) => session,
        _ => panic!("reopened genuine session"),
    };
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(
            turn.authenticate_snapshot_pair(&SnapshotManifestPair::decode(&canonical).unwrap())
                .is_err(),
            "canonical really expired under actual source clock"
        );
        Ok(())
    })
    .unwrap();
    let recovered = reopen(&f, id, original).unwrap();
    assert_eq!(recovered.served_through, 1);
    let current = recovered.draft.pair.bytes.clone();
    let Renewal::Unchanged(recovered) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        recovered.renew(turn, &f.session, original)
    })
    .unwrap() else {
        panic!("expired previous remains exact retry anchor only")
    };
    assert_eq!(recovered.draft.pair.bytes, current);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        recovered.manifest(turn, &f.session).map(|_| ())
    })
    .unwrap();
    let current_digest = digest(&recovered);
    drop(recovered);
    control.advance_monotonic(Duration::from_secs(61));
    assert!(matches!(
        reopen(&f, id, current_digest),
        Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))
    ));
}

fn admit_large_original(f: &crate::https_owner::data::tests::Fixture) {
    use automerge::{AutoCommit, ROOT, ScalarValue, transaction::Transactable};
    let mut store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    let mut author = store.peer().unwrap();
    // This is the real installed primary Chat record required by Cloud enrollment.
    let mut doc = AutoCommit::load(&author.document("dev.qos.chat", "").unwrap()).unwrap();
    let heads = doc.get_heads();
    let mut bytes = vec![0; 600 * 1024];
    blake3::Hasher::new()
        .update(b"actual Cloud large snapshot record")
        .finalize_xof()
        .fill(&mut bytes);
    doc.put(ROOT, "large-snapshot", ScalarValue::Bytes(bytes))
        .unwrap();
    doc.commit();
    let now = qos_relay::clock::now_ns();
    let message = author
        .publish_incremental(
            "dev.qos.chat",
            "",
            doc.save_after(&heads),
            &store.key(),
            crate::now_time(now),
        )
        .unwrap();
    store
        .receive(
            qos_peer_state::TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
            message,
            now,
        )
        .unwrap();
}
fn finish_renew(
    f: &crate::https_owner::data::tests::Fixture,
    live: LiveSnapshot,
    request: Digest,
) -> LiveSnapshot {
    let Renewal::Pending(draft) = run(f, DeliveryOperation::SnapshotRenew, |turn| {
        live.renew(turn, &f.session, request)
    })
    .unwrap() else {
        panic!("strict new progress")
    };
    let pending = draft.persist().unwrap();
    run(f, DeliveryOperation::SnapshotRenew, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap()
    .finalize()
    .unwrap()
}
#[tokio::test]
async fn cloud_snapshot_large_original_higher_progress_renews_after_restart_and_one_anchor_only() {
    use qos_relay::delivery_v3::framing::{MessageType, decode_frame_prefix};
    let f = snapshot_fixture().await;
    register(&f);
    admit_large_original(&f);
    let live = live(&f);
    assert!(live.draft.prepared.stream.chunk_digests().len() > 2);
    let id = live.draft.prepared.selection.pin;
    let original = digest(&live);
    let idle = f.pool.available_bytes();
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(0, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    let (kind, _, rest) = decode_frame_prefix(reply.bytes()).unwrap();
    assert_eq!(kind, MessageType::SnapshotChunk);
    assert!(rest.is_empty(), "partial page has no complete boundary");
    assert_eq!(
        f.pool.available_bytes() + 1024 * 1024,
        idle,
        "reply owns actual original-pool work credit"
    );
    drop(reply);
    assert_eq!(f.pool.available_bytes(), idle);
    let live = finish_renew(&f, live, original);
    let first = digest(&live);
    drop(live);
    let live = reopen(&f, id, first).unwrap();
    assert_eq!(live.served_through, 1);
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(1, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    let live = finish_renew(&f, live, first);
    let second = digest(&live);
    assert_eq!(
        SnapshotManifestPair::decode(&live.draft.pair.bytes)
            .unwrap()
            .renewed_through(),
        2
    );
    drop(live);
    assert!(
        matches!(reopen(&f, id, original), Err(CloudDeliveryError::Denied)),
        "older than the single retained anchor is not accepted"
    );
    let live = reopen(&f, id, first).unwrap();
    assert_eq!(digest(&live), second);
    assert_eq!(live.served_through, 2);
}

#[tokio::test]
async fn cloud_snapshot_renew_uncertain_barrier_keeps_old_anchor_until_actual_reopen_selection() {
    use crate::{
        https_owner::{Current, data::tests::authenticate_with_capabilities},
        storage::test_support::with_save_failure,
    };
    use atomic_file::test_support::Checkpoint;
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let mut f = snapshot_fixture().await;
        register(&f);
        let live = live(&f);
        let id = live.draft.prepared.selection.pin;
        let original = digest(&live);
        let old_expiry = live.draft.expires_at_ns;
        let old_bytes = live.draft.pair.bytes.clone();
        let requested_digest = digest(&live);
        let job = live.prepare_chunk(0, requested_digest).unwrap();
        let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
            job.authorize(turn, &f.session)
        })
        .unwrap();
        drop(reply);
        let Renewal::Pending(draft) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
            live.renew(turn, &f.session, original)
        })
        .unwrap() else {
            panic!("new progress")
        };
        let next_expiry = draft.next_expiry;
        let next_bytes = draft.next_pair.bytes.clone();
        let pending = draft.persist().unwrap();
        let result = with_save_failure(f.root.path(), point, || {
            run(&f, DeliveryOperation::SnapshotRenew, |turn| {
                pending.commit(turn, &f.session)
            })
        });
        assert!(matches!(result, Err(CloudDeliveryError::OwnerUncertain(_))));
        let dir = f.root.path().join("snapshots-v1");
        let stem = hex::encode(id);
        assert_eq!(
            std::fs::read(dir.join(format!("{stem}.pair"))).unwrap(),
            old_bytes
        );
        assert_eq!(
            std::fs::read(dir.join(format!("{stem}.pending"))).unwrap(),
            next_bytes
        );
        assert!(matches!(f.source.with_current(|_| ()), Current::Denied));
        let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
        f.source = CloudStoreSource::bind(&store).unwrap();
        drop(store);
        let origin = f.session.binding().origin.clone();
        f.session = match f
            .source
            .with_current(|turn| authenticate_with_capabilities(turn, &f.host, &origin, 9))
        {
            Current::Current(session) => session,
            _ => panic!("actual reopened session"),
        };
        let selected_expiry = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
            Ok(turn.delivery_ledger().pins[&id].expires_at_ns)
        })
        .unwrap();
        assert!(selected_expiry == old_expiry || selected_expiry == next_expiry);
        let recovered = reopen(&f, id, original).unwrap();
        assert_eq!(recovered.draft.expires_at_ns, selected_expiry);
        let expected = if selected_expiry == old_expiry {
            &old_bytes
        } else {
            &next_bytes
        };
        assert_eq!(&recovered.draft.pair.bytes, expected);
        assert_eq!(
            std::fs::read(dir.join(format!("{stem}.pair"))).unwrap(),
            *expected
        );
        assert!(!dir.join(format!("{stem}.pending")).exists());
    }
}

#[tokio::test]
async fn cloud_snapshot_recovery_partial_pending_cannot_erase_a_new_committed_pin_candidate() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let id = live.draft.prepared.selection.pin;
    let original = digest(&live);
    let canonical = live.draft.pair.bytes.clone();
    let requested_digest = digest(&live);
    let job = live.prepare_chunk(0, requested_digest).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    let Renewal::Pending(draft) = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        live.renew(turn, &f.session, original)
    })
    .unwrap() else {
        panic!("new progress")
    };
    let pending = draft.persist().unwrap();
    let committed = run(&f, DeliveryOperation::SnapshotRenew, |turn| {
        pending.commit(turn, &f.session)
    })
    .unwrap();
    drop(committed);
    let dir = f.root.path().join("snapshots-v1");
    let path = dir.join(format!("{}.pending", hex::encode(id)));
    std::fs::write(&path, b"damaged committed candidate").unwrap();
    assert!(reopen(&f, id, original).is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"damaged committed candidate"
    );
    assert_eq!(
        std::fs::read(dir.join(format!("{}.pair", hex::encode(id)))).unwrap(),
        canonical
    );
}

#[tokio::test]
async fn cloud_snapshot_chunk_digest_refusal_preserves_actor_credit_and_current_retry() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let id = live.draft.prepared.selection.pin;
    let original = digest(&live);
    let before = f.pool.available_bytes();
    let old_pin = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        Ok(turn.delivery_ledger().pins[&id].clone())
    })
    .unwrap();
    let refusal = live
        .prepare_chunk(0, [0xa5; 32])
        .err()
        .expect("foreign digest must be refused before worker allocation");
    let (live, error) = refusal.into_parts();
    assert_eq!(error, CloudDeliveryError::Denied);
    assert_eq!(f.pool.available_bytes(), before);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert_eq!(turn.delivery_ledger().pins[&id], old_pin);
        Ok(())
    })
    .unwrap();
    let job = live.prepare_chunk(0, original).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    drop(reply);
    let live = finish_renew(&f, live, original);
    let current = digest(&live);
    assert_ne!(current, original);
    let before = f.pool.available_bytes();
    let renewed_pin = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        Ok(turn.delivery_ledger().pins[&id].clone())
    })
    .unwrap();
    let refusal = live
        .prepare_chunk(0, original)
        .err()
        .expect("previous anchor belongs only to Renew/recovery");
    let (live, error) = refusal.into_parts();
    assert_eq!(error, CloudDeliveryError::Denied);
    assert_eq!(f.pool.available_bytes(), before);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert_eq!(turn.delivery_ledger().pins[&id], renewed_pin);
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    let job = live.prepare_chunk(0, current).unwrap();
    let (live, reply) = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        job.authorize(turn, &f.session)
    })
    .unwrap();
    assert_eq!(digest(&live), current);
    assert!(!reply.bytes().is_empty());
}
