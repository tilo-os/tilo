//! Incremental publication, certified checkpoints, receipt release and
//! snapshot + suffix rotation on the native delivery owner.
use super::tests::{authenticated_session, hosted_owner, now_ns, register_and_cursor, upload};
use super::*;
use crate::owner::retention::{
    CHECKPOINT_IDLE_NS, CHECKPOINT_MAX_AGE_NS, CHECKPOINT_TAIL_ENTRIES, RetentionAction,
};
use ui::automerge::{ROOT, transaction::Transactable};

fn activated() -> (tempfile::TempDir, SessionOwner) {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    (temp, owner)
}
fn id(n: u32) -> DeliveryId {
    let mut id = [0xA5; 16];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id
}
fn send(owner: &mut SessionOwner, n: u32, bytes: &[u8]) -> AdmissionRecord {
    let outcome = owner
        .receive_delivery(
            TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
            CompleteStateItem {
                delivery_id: id(n),
                canonical: bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    match outcome {
        AdmissionOutcome::Durable(record) => record,
        other => panic!("expected durable admission: {other:?}"),
    }
}
/// One author edit carrying `len` incompressible bytes.
fn large_upload(owner: &SessionOwner, author: &mut StatePeer, name: &str, len: usize) -> Vec<u8> {
    let mut doc =
        ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
    let heads = doc.get_heads();
    let mut bytes = vec![0; len];
    let mut generator = blake3::Hasher::new();
    generator.update(name.as_bytes());
    generator.finalize_xof().fill(&mut bytes);
    doc.put(ROOT, name, ui::automerge::ScalarValue::Bytes(bytes))
        .unwrap();
    doc.commit();
    let message = author
        .publish_incremental(
            "so.tilo.chat",
            "",
            doc.save_after(&heads),
            &owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}
fn storage(owner: &SessionOwner) -> (&PublicationPool, &PublicationRootsV1, &DeliveryLedgerV1) {
    let PublicationStorage::V5 {
        pool,
        roots,
        delivery,
        ..
    } = &owner.aggregate.publication
    else {
        unreachable!()
    };
    (pool, roots, delivery)
}
/// Admit one committed replay group exactly as a browser consumer does:
/// one native publication cut under the pinned governing publisher.
fn admit_group(
    replica: &StatePeer,
    owner: &SessionOwner,
    publications: &[PublicationRef],
) -> StatePeer {
    let (pool, _, _) = storage(owner);
    let now = SystemTime::now();
    let mut cut = replica
        .begin_native_publication_cut(owner.peer.authority().device_public_key, now)
        .unwrap();
    for reference in publications {
        let message: NeighbourMsg =
            postcard::from_bytes(pool.resolve(&reference.payload).unwrap()).unwrap();
        cut = cut.push(message, now).unwrap();
    }
    cut.finish(now).unwrap()
}
fn replay_after(owner: &SessionOwner, after: u64) -> Vec<(u64, ReplayCommit)> {
    storage(owner)
        .2
        .replay
        .range(after + 1..)
        .map(|(sequence, entry)| (*sequence, entry.clone()))
        .collect()
}
/// Upload one small author edit and return its durable receipt.
fn edit(owner: &mut SessionOwner, author: &mut StatePeer, n: u32, name: &str) -> AdmissionRecord {
    let bytes = upload(owner, author, name);
    send(owner, n, &bytes)
}
fn wall(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(ns)
}

#[test]
fn upload_commits_publish_the_authors_original_bytes_and_consumers_converge() {
    let (_temp, mut owner) = activated();
    let mut replica = owner.peer.clone();
    let start = owner.status().generation;
    let pool_before = storage(&owner).0.retained_payload_capacity().unwrap();
    let mut author = owner.peer.clone();
    let mut uploaded = Vec::new();
    for n in 0..20 {
        let bytes = upload(&owner, &mut author, &format!("edit-{n}"));
        let record = send(&mut owner, n, &bytes);
        let entry = storage(&owner).2.replay[&record.committed.sequence].clone();
        // No host snapshot: the group is exactly the author's signed item.
        assert_eq!(
            entry.publications,
            vec![PublicationRef {
                payload: *blake3::hash(&bytes).as_bytes(),
                kind: Retention::Delta,
            }]
        );
        assert_eq!(entry.admitted_operations, record.operations);
        uploaded.push(bytes);
    }
    let grown = storage(&owner).0.retained_payload_capacity().unwrap() - pool_before;
    assert_eq!(grown, uploaded.iter().map(Vec::len).sum::<usize>());
    // A consumer admits each group through the retained native carrier.
    for (_, entry) in replay_after(&owner, start) {
        replica = admit_group(&replica, &owner, &entry.publications);
    }
    assert_eq!(replica.rows(), owner.peer.rows());
    assert_eq!(
        replica.document("so.tilo.chat", "").unwrap(),
        owner.peer.document("so.tilo.chat", "").unwrap()
    );
    // Coverage (outbox retirement) still comes only from a certified checkpoint.
    let original: NeighbourMsg = postcard::from_bytes(uploaded.last().unwrap()).unwrap();
    assert!(!replica.outbox_message_retired(&original));
    let covered_from = owner.status().generation;
    let report = owner
        .retain_delivery(RetentionAction::Checkpoint, SystemTime::now())
        .unwrap();
    assert!(!report.rotated);
    assert_eq!(report.after.tail_entries, 0);
    for (_, entry) in replay_after(&owner, covered_from) {
        assert!(
            entry
                .publications
                .iter()
                .any(|p| p.kind == Retention::Checkpoint)
        );
        replica = admit_group(&replica, &owner, &entry.publications);
    }
    assert!(replica.outbox_message_retired(&original));
    assert_eq!(
        replica
            .durable_coverage("so.tilo.chat", "")
            .unwrap()
            .unwrap()
            .generation,
        owner.status().generation
    );
    assert_eq!(replica.rows(), owner.peer.rows());
}

#[test]
fn checkpoints_follow_idle_age_and_tail_budgets_and_wait_for_active_snapshots() {
    let (_temp, mut owner) = activated();
    let mut author = owner.peer.clone();
    assert_eq!(
        owner
            .delivery_retention_due(SystemTime::now(), false)
            .unwrap(),
        None
    );
    edit(&mut owner, &mut author, 0, "first");
    let pressure = owner.delivery_pressure().unwrap();
    assert_eq!((pressure.tail_entries, pressure.author_deltas), (1, 1));
    let last = pressure.last_commit_ns;
    assert!(!pressure.checkpoint_due(last, false));
    assert!(pressure.checkpoint_due(last + CHECKPOINT_IDLE_NS, false));
    // An active snapshot download defers the optional checkpoint.
    assert!(!pressure.checkpoint_due(last + CHECKPOINT_IDLE_NS, true));
    // Under continuous activity the oldest uncovered commit bounds latency.
    let first = pressure.first_uncovered_ns.unwrap();
    let mut active = pressure;
    active.last_commit_ns = first + CHECKPOINT_MAX_AGE_NS;
    assert!(!active.checkpoint_due(first + CHECKPOINT_MAX_AGE_NS - 1, false));
    assert!(active.checkpoint_due(first + CHECKPOINT_MAX_AGE_NS, false));
    // The tail budget applies without waiting.
    for n in 1..CHECKPOINT_TAIL_ENTRIES as u32 {
        edit(&mut owner, &mut author, n, &format!("burst-{n}"));
    }
    let pressure = owner.delivery_pressure().unwrap();
    assert_eq!(pressure.tail_entries, CHECKPOINT_TAIL_ENTRIES);
    assert!(pressure.checkpoint_due(pressure.last_commit_ns, false));
    assert!(!pressure.checkpoint_due(pressure.last_commit_ns, true));
    let store = storage(&owner).2.store_generation;
    let report = owner
        .maintain_delivery(wall(pressure.last_commit_ns), false)
        .unwrap()
        .expect("checkpoint due");
    assert_eq!(report.action, RetentionAction::Checkpoint);
    assert!(!report.rotated);
    let (_, roots, ledger) = storage(&owner);
    assert!(roots.tail.is_empty());
    assert_eq!(ledger.store_generation, store);
    assert_eq!(roots.baseline_generation, owner.status().generation);
    // Covered: nothing further is due, even after the idle period.
    let now = owner.delivery_pressure().unwrap().last_commit_ns + CHECKPOINT_MAX_AGE_NS;
    assert_eq!(
        owner.delivery_retention_due(wall(now), false).unwrap(),
        None
    );
}

#[test]
fn oversized_checkpoint_rotates_to_a_certified_incarnation_releasing_history() {
    let (_temp, mut owner) = activated();
    let mut author = owner.peer.clone();
    let mut records = Vec::new();
    let mut items = Vec::new();
    for n in 0..3 {
        let bytes = large_upload(&owner, &mut author, &format!("large-{n}"), 400 * 1024);
        records.push(send(&mut owner, n, &bytes));
        items.push(bytes);
    }
    for n in 3..6 {
        let bytes = upload(&owner, &mut author, &format!("small-{n}"));
        records.push(send(&mut owner, n, &bytes));
        items.push(bytes);
    }
    let old = storage(&owner).2.clone();
    let before = owner.delivery_pressure().unwrap();
    let report = owner
        .retain_delivery(RetentionAction::Checkpoint, SystemTime::now())
        .unwrap();
    assert!(report.rotated, "a checkpoint above one replay page rotates");
    let generation = owner.status().generation;
    let (_, roots, ledger) = storage(&owner);
    let (roots, ledger) = (roots.clone(), ledger.clone());
    assert_ne!(ledger.store_generation, old.store_generation);
    assert_eq!(ledger.replay.len(), 1);
    let (sequence, first) = ledger.replay.first_key_value().unwrap();
    assert_eq!(*sequence, generation);
    assert_eq!(first.previous, None);
    assert_eq!(first.publications, roots.baseline);
    assert!(roots.tail.is_empty());
    // The certified first entry names every admitted operation.
    let all = crate::owner::retention::all_admitted_operations(&owner.peer).unwrap();
    assert!(all.iter().all(|op| first.admitted_operations.contains(op)));
    // History is gone: the pool holds exactly the certified baseline.
    let after = owner.delivery_pressure().unwrap();
    assert_eq!(after.history_bytes, 0);
    assert_eq!(
        storage(&owner).0.retained_payload_capacity().unwrap(),
        after.root_bytes
    );
    assert!(after.pool_bytes < before.pool_bytes);
    // No consumer acknowledged: every receipt is retained and re-bound to
    // the certified entry of the new incarnation.
    assert_eq!(ledger.admissions.len(), records.len());
    for record in ledger.admissions.values() {
        assert_eq!(record.committed, reference(*sequence, first));
    }
    let sequence = &sequence.clone();
    // Lost reply: the same delivery ID returns its re-bound receipt.
    let retry = send(&mut owner, 0, &items[0]);
    assert_eq!(retry.operations, records[0].operations);
    assert_eq!(
        retry.committed,
        reference(*sequence, &ledger.replay[sequence])
    );
    // A released or unknown ID is a certified duplicate of the same entry,
    // without a new publication or host checkpoint.
    let duplicate = send(&mut owner, 99, &items[1]);
    assert_eq!(duplicate.result, DurableAdmissionKind::Duplicate);
    assert_eq!(duplicate.committed, retry.committed);
    assert_eq!(storage(&owner).2.replay, ledger.replay);
    // Reopen: the rotated incarnation is the durable state.
    let path = owner.root().to_owned();
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    // Fixture authors sign with the host key; checkpoints advance that same
    // device's sequence, so the fixture author rebases (real authors differ).
    let mut author = owner.peer.clone();
    assert_eq!(storage(&owner).2.replay, ledger.replay);
    assert_eq!(storage(&owner).2.store_generation, ledger.store_generation);
    let bytes = upload(&owner, &mut author, "after-reopen");
    let record = send(&mut owner, 100, &bytes);
    assert_eq!(
        storage(&owner).2.replay[&record.committed.sequence].previous,
        Some(reference(*sequence, &ledger.replay[sequence]))
    );
}

#[test]
fn receipts_are_released_only_for_devices_that_acknowledged_their_commit() {
    let (_temp, mut owner) = activated();
    let mut author = owner.peer.clone();
    let items = (0..3)
        .map(|n| upload(&owner, &mut author, &format!("receipt-{n}")))
        .collect::<Vec<_>>();
    let records = items
        .iter()
        .enumerate()
        .map(|(n, bytes)| send(&mut owner, n as u32, bytes))
        .collect::<Vec<_>>();
    let device = owner.peer.authority().device_public_key;
    let mut next = owner.aggregate.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
        unreachable!()
    };
    delivery.consumers.insert(
        device,
        ConsumerState {
            id: [3; 16],
            device,
            root: owner.peer.authority().user_public_key,
            authorization_view: [4; 32],
            // Installed through the second commit, not the third.
            acknowledged: Some(records[1].committed.clone()),
        },
    );
    owner.commit(next, owner.peer.clone()).unwrap();
    assert_eq!(owner.delivery_pressure().unwrap().prunable_receipts, 2);
    let report = owner
        .retain_delivery(RetentionAction::ReleaseReceipts, SystemTime::now())
        .unwrap();
    assert_eq!(report.receipts_released, 2);
    let ledger = storage(&owner).2.clone();
    assert_eq!(ledger.admissions.len(), 1);
    assert_eq!(
        ledger.admissions.values().next().unwrap().operations,
        records[2].operations
    );
    // The retained (unacknowledged) receipt still answers its lost reply.
    assert_eq!(send(&mut owner, 2, &items[2]), records[2]);
    // A retransmission of a released item is an exact native duplicate of
    // its original commit: no new replay group, no host checkpoint.
    let released = send(&mut owner, 0, &items[0]);
    assert_eq!(released.result, DurableAdmissionKind::Duplicate);
    assert_eq!(released.committed, records[0].committed);
    assert_eq!(released.operations, records[0].operations);
    assert_eq!(storage(&owner).2.replay, ledger.replay);
}

#[test]
fn offline_consumer_cursor_resets_after_rotation_and_recovers_with_snapshot_and_suffix() {
    use crate::committed_authority::CommittedAuthority;
    use tilo_relay::delivery_v3::state::ResetReason;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let token = register_and_cursor(&mut owner, &committed, &session);
    // The consumer goes offline with the state its cursor names.
    let offline = owner.peer.clone();
    let mut author = owner.peer.clone();
    for n in 0..4 {
        let bytes = upload(&owner, &mut author, &format!("while-offline-{n}"));
        committed
            .with_owner_view(&mut owner, |_, owner| {
                send(owner, n, &bytes);
            })
            .unwrap();
    }
    committed
        .with_owner_view(&mut owner, |_, owner| {
            owner.retain_delivery(RetentionAction::Rotate, SystemTime::now())
        })
        .unwrap()
        .unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "after-rotation");
    committed
        .with_owner_view(&mut owner, |_, owner| {
            send(owner, 10, &bytes);
        })
        .unwrap();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    let reset = owner.verify_delivery_cursor(&source, &token).err().unwrap();
    assert!(matches!(
        reset
            .downcast_ref::<error::OwnerDataFailure>()
            .map(|failure| failure.kind()),
        Some(DataFailure::Reset(ResetReason::StoreRotated))
    ));
    // Registration survived; acknowledgements of the old chain were cleared.
    let device = session.device_certificate().device_public_key;
    let consumer = &storage(&owner).2.consumers[&device];
    assert_eq!(consumer.acknowledged, None);
    // Snapshot (current baseline + tail) then suffix converges the replica.
    let (_, roots, ledger) = storage(&owner);
    let snapshot = roots
        .baseline
        .iter()
        .chain(&roots.tail)
        .cloned()
        .collect::<Vec<_>>();
    let replica = admit_group(&offline, &owner, &snapshot);
    assert_eq!(replica.rows(), owner.peer.rows());
    let (sequence, entry) = ledger.replay.last_key_value().unwrap();
    let cursor = owner
        .issue_delivery_cursor(&source, reference(*sequence, entry))
        .unwrap();
    assert!(owner.verify_delivery_cursor(&source, &cursor).is_ok());
}

#[test]
fn rotation_is_crash_consistent_at_every_barrier_and_survives_journal_replay() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (_temp, mut owner) = activated();
        let mut author = owner.peer.clone();
        for n in 0..3 {
            edit(&mut owner, &mut author, n, &format!("pre-{n}"));
        }
        let path = owner.root().to_owned();
        let old = storage(&owner).2.clone();
        let rows = owner.peer.rows();
        let result = with_fault(&path, point, || {
            owner.retain_delivery(RetentionAction::Rotate, SystemTime::now())
        });
        assert!(result.is_err(), "fault returned a rotation at {point:?}");
        assert!(!owner.permits_publication());
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        let ledger = storage(&owner).2.clone();
        if point == Point::BeforeWrite {
            assert_eq!(ledger, old, "nothing written: old incarnation intact");
        } else {
            assert_ne!(ledger.store_generation, old.store_generation);
            assert_eq!(ledger.replay.len(), 1);
        }
        assert_eq!(owner.peer.rows(), rows);
        // The recovered owner keeps admitting and retaining.
        let mut author = owner.peer.clone();
        let record = edit(&mut owner, &mut author, 50, "recovered");
        assert!(
            storage(&owner)
                .2
                .replay
                .contains_key(&record.committed.sequence)
        );
        owner
            .retain_delivery(RetentionAction::Rotate, SystemTime::now())
            .unwrap();
    }
    // Journaled commits: an abrupt exit after rotation replays the journal.
    crate::store::test_journal::with_journal(|| {
        let (_temp, mut owner) = activated();
        let mut author = owner.peer.clone();
        for n in 0..3 {
            edit(&mut owner, &mut author, n, &format!("journal-{n}"));
        }
        owner
            .retain_delivery(RetentionAction::Rotate, SystemTime::now())
            .unwrap();
        let mut author = owner.peer.clone();
        let record = edit(&mut owner, &mut author, 9, "journal-after");
        let ledger = storage(&owner).2.clone();
        let rows = owner.peer.rows();
        let path = owner.root().to_owned();
        owner.crash_store_on_drop();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(storage(&owner).2, &ledger);
        assert_eq!(owner.peer.rows(), rows);
        assert!(
            storage(&owner)
                .2
                .admissions
                .values()
                .any(|r| r.committed == record.committed)
        );
    });
}

#[test]
fn acknowledgement_side_records_of_a_rotated_incarnation_are_inert_on_recovery() {
    use crate::committed_authority::CommittedAuthority;
    crate::store::test_journal::with_journal(|| {
        let (_temp, mut owner) = hosted_owner();
        let committed = CommittedAuthority::new(&owner);
        let session = authenticated_session(&owner, &committed);
        let token = register_and_cursor(&mut owner, &committed, &session);
        committed
            .with_owner_view(&mut owner, |view, owner| {
                let source = owner.delivery_source(view, &session, now_ns())?;
                let cut = owner.verify_delivery_cursor(&source, &token)?.into_cut();
                owner.acknowledge_delivery(&source, cut)
            })
            .unwrap()
            .unwrap();
        let device = session.device_certificate().device_public_key;
        assert!(storage(&owner).2.consumers[&device].acknowledged.is_some());
        committed
            .with_owner_view(&mut owner, |_, owner| {
                owner.retain_delivery(RetentionAction::Rotate, SystemTime::now())
            })
            .unwrap()
            .unwrap();
        let ledger = storage(&owner).2.clone();
        assert_eq!(ledger.consumers[&device].acknowledged, None);
        let path = owner.root().to_owned();
        owner.crash_store_on_drop();
        drop(committed);
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        // The side record names the old incarnation and is not re-applied.
        assert_eq!(storage(&owner).2, &ledger);
    });
}
