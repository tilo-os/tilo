use super::*;
use ui::automerge::{ROOT, transaction::Transactable};

fn owner() -> (tempfile::TempDir, SessionOwner) {
    let temp = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    (temp, owner)
}

pub(super) fn upload(owner: &SessionOwner, author: &mut StatePeer, name: &str) -> Vec<u8> {
    let mut doc =
        ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, name, name).unwrap();
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

#[test]
fn delivery_activation_is_explicit_and_restart_preserves_native_floor_pool_and_uuid() {
    let (_temp, mut owner) = owner();
    let path = owner.root().to_owned();
    let old = owner.status();
    assert!(!owner.peer.admission_v5_selected());
    owner.flush().unwrap();
    assert!(!owner.peer.admission_v5_selected());
    owner.activate_delivery().unwrap();
    assert!(owner.peer.admission_v5_selected());
    assert_eq!(owner.status().generation, old.generation + 1);
    assert!(owner.permits_publication());
    let ledger = owner.delivery_ledger().unwrap().clone();
    assert_ne!(ledger.store_generation, [0; 16]);
    assert_eq!(ledger.replay.len(), 1);
    assert_eq!(ledger.resolved_through, owner.status().generation);
    assert!(
        !ledger
            .replay
            .values()
            .next()
            .unwrap()
            .publications
            .is_empty()
    );
    let file = std::fs::read(path.join("session.bin")).unwrap();
    owner.flush().unwrap();
    assert_eq!(file, std::fs::read(path.join("session.bin")).unwrap());
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.delivery_ledger().unwrap(), &ledger);
    assert!(owner.peer.admission_v5_selected());
    assert_eq!(file, std::fs::read(path.join("session.bin")).unwrap());
    owner.activate_delivery().unwrap();
    assert_eq!(file, std::fs::read(path.join("session.bin")).unwrap());
}

#[test]
fn delivery_upload_and_lost_reply_retry_share_exact_native_replay_evidence() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "upload-A");
    let source = TransportOrigin::RelayDelta(owner.peer.authority().device_public_key);
    let result = owner
        .receive_delivery(
            source,
            CompleteStateItem {
                delivery_id: [1; 16],
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    let AdmissionOutcome::Durable(first) = result else {
        panic!("not durable: {result:?}")
    };
    assert_eq!(first.result, DurableAdmissionKind::Accepted);
    assert_eq!(first.operations.len(), 1);
    let path = owner.root().to_owned();
    let document = owner.peer.document("so.tilo.chat", "").unwrap();
    let generation = owner.status().generation;
    assert_eq!(first.committed.sequence, generation);
    let replay = owner.delivery_ledger().unwrap().replay[&generation].clone();
    assert!(replay.admitted_operations.contains(&first.operations[0]));
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    let result = owner
        .receive_delivery(
            source,
            CompleteStateItem {
                delivery_id: [1; 16],
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    let AdmissionOutcome::Durable(retry) = result else {
        panic!("not durable")
    };
    assert_eq!(retry.committed, first.committed);
    assert_eq!(retry.operations, first.operations);
    assert_eq!(owner.status().generation, generation);
    let result = owner
        .receive_delivery(
            source,
            CompleteStateItem {
                delivery_id: [2; 16],
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    let AdmissionOutcome::Durable(duplicate) = result else {
        panic!("not durable")
    };
    assert_eq!(duplicate.result, DurableAdmissionKind::Duplicate);
    assert_eq!(duplicate.committed, first.committed);
    assert_eq!(owner.peer.document("so.tilo.chat", "").unwrap(), document);
    assert_eq!(owner.delivery_ledger().unwrap().replay[&generation], replay);
    assert_eq!(owner.delivery_ledger().unwrap().replay.len(), 2);
}

fn send(owner: &mut SessionOwner, id: u8, bytes: &[u8]) -> Result<AdmissionOutcome> {
    owner.receive_delivery(
        TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
        CompleteStateItem {
            delivery_id: [id; 16],
            canonical: bytes,
        },
        SystemTime::now(),
    )
}
fn durable(result: AdmissionOutcome) -> AdmissionRecord {
    match result {
        AdmissionOutcome::Durable(record) => record,
        other => panic!("expected durable: {other:?}"),
    }
}
#[test]
fn delivery_conflict_and_stale_sequence_cannot_mutate_or_claim_duplicate() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "original");
    let first = durable(send(&mut owner, 1, &bytes).unwrap());
    let before = owner.status();
    let file = std::fs::read(owner.root().join("session.bin")).unwrap();
    let mut changed: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut changed else {
        unreachable!()
    };
    envelope.timestamp += web_time::Duration::from_secs(1);
    collab_types::sign_op(envelope, &owner.identity.native).unwrap();
    let changed = postcard::to_stdvec(&changed).unwrap();
    assert_eq!(
        send(&mut owner, 1, &changed)
            .unwrap_err()
            .downcast_ref::<error::OwnerDataFailure>()
            .unwrap()
            .kind(),
        error::OwnerDataFailureKind::ExactIdConflict
    );
    assert!(send(&mut owner, 2, &changed).is_err());
    assert_eq!(owner.status(), before);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        file
    );
    assert_eq!(owner.delivery_ledger().unwrap().admissions.len(), 1);
    assert_eq!(durable(send(&mut owner, 1, &bytes).unwrap()), first);
}
#[test]
fn delivery_prerequisite_later_admission_records_settled_operations_without_early_receipt() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let first = upload(&owner, &mut author, "prerequisite");
    let second = upload(&owner, &mut author, "depends-on-first");
    let before = owner.status();
    assert!(matches!(
        send(&mut owner, 2, &second).unwrap(),
        AdmissionOutcome::RetryLater(MissingPrerequisite::CausalDependencies)
    ));
    assert_eq!(owner.status(), before);
    assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
    let first_receipt = durable(send(&mut owner, 1, &first).unwrap());
    assert_eq!(owner.delivery_ledger().unwrap().admissions.len(), 1);
    let second_receipt = durable(send(&mut owner, 2, &second).unwrap());
    assert_eq!(second_receipt.result, DurableAdmissionKind::Duplicate);
    assert_eq!(second_receipt.committed, first_receipt.committed);
    assert_ne!(second_receipt.operations, first_receipt.operations);
}
#[test]
fn delivery_metadata_gap_and_checkpoint_retains_exact_replay_payloads() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let before = owner.delivery_ledger().unwrap().clone();
    let original = before.replay.values().next().unwrap().publications.clone();
    owner.queue_pending("metadata-only", &[4], false).unwrap();
    assert_eq!(owner.delivery_ledger().unwrap().replay, before.replay);
    assert_eq!(
        owner.delivery_ledger().unwrap().resolved_through,
        before.resolved_through + 1
    );
    owner.refresh_checkpoint().unwrap();
    let ledger = owner.delivery_ledger().unwrap();
    assert_eq!(ledger.replay.len(), 2);
    assert!(!ledger.replay.contains_key(&(before.resolved_through + 1)));
    assert_eq!(
        ledger.replay.last_key_value().unwrap().0,
        &(before.resolved_through + 2)
    );
    let PublicationStorage::V5 { pool, roots, .. } = &owner.aggregate.publication else {
        unreachable!()
    };
    for p in &original {
        assert!(!pool.resolve(&p.payload).unwrap().is_empty());
    }
    assert!(roots.tail.is_empty());
    let path = owner.root().to_owned();
    let bytes = original
        .iter()
        .map(|p| pool.resolve(&p.payload).unwrap().to_vec())
        .collect::<Vec<_>>();
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    let PublicationStorage::V5 { pool, .. } = &owner.aggregate.publication else {
        unreachable!()
    };
    for (p, bytes) in original.iter().zip(bytes) {
        assert_eq!(pool.resolve(&p.payload).unwrap(), bytes);
    }
}
#[test]
fn delivery_restore_rotates_only_explicitly_and_retains_native_receipt_identity() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "survives-restore");
    let first = durable(send(&mut owner, 1, &bytes).unwrap());
    let old = owner.delivery_ledger().unwrap().clone();
    let doc = owner.peer.document("so.tilo.chat", "").unwrap();
    owner.rotate_store_generation_for_restore().unwrap();
    let new = owner.delivery_ledger().unwrap();
    assert_ne!(new.store_generation, old.store_generation);
    assert_eq!(new.replay.len(), old.replay.len() + 1);
    let retry = durable(send(&mut owner, 1, &bytes).unwrap());
    assert_eq!(retry.digest, first.digest);
    assert_eq!(retry.operations, first.operations);
    assert_eq!(retry.result, first.result);
    assert_eq!(retry.committed.sequence, first.committed.sequence);
    assert_ne!(retry.committed.digest, first.committed.digest);
    assert_eq!(owner.peer.document("so.tilo.chat", "").unwrap(), doc);
}

pub(super) fn authenticated_session(
    owner: &SessionOwner,
    committed: &crate::committed_authority::CommittedAuthority,
) -> tilo_relay::delivery_v3::auth::VerifiedSession {
    use tilo_relay::delivery_v3::auth::*;
    let current = committed.read().unwrap();
    let authority = NativeDeliveryAuthority {
        view: current.view(),
        workspace: owner.workspace_id,
        host: &owner.identity.host_cert,
    };
    let binding = Binding {
        workspace: owner.workspace_id,
        workspace_root: owner.identity.native_cert.user_public_key,
        device: owner.identity.native_cert.device_public_key,
        device_root: owner.identity.native_cert.user_public_key,
        host: owner.identity.host_cert.device_public_key,
        host_root: owner.identity.host_cert.user_public_key,
        origin: "https://localhost:9443".into(),
        descriptor_generation: 1,
        descriptor_digest: [7; 32],
        server_boot_id: [8; 16],
        carrier: Carrier::Https,
        requested_capabilities: 1,
        mandatory_capabilities: 1,
    };
    let revocations = RevocationList::new();
    let context = VerificationContext {
        binding: &binding,
        capabilities: CapabilityPolicy {
            supported: 1,
            allowed: 1,
        },
        credentials: Credentials {
            device: &owner.identity.native_cert,
            host: &owner.identity.host_cert,
            device_revocations: &revocations,
            host_revocations: &revocations,
        },
        authority: &authority,
        now_ns: now_ns(),
    };
    let (challenge, proof) = Challenge::issue(&context, [9; 32], &owner.identity.host).unwrap();
    let client = verify_server(
        &ClientExpectations {
            scope: ClientScope::from(&binding),
            capabilities: context.capabilities,
            credentials: context.credentials,
            authority: &authority,
            now_ns: context.now_ns,
        },
        [9; 32],
        &proof,
    )
    .unwrap()
    .client_proof(&owner.identity.native)
    .unwrap();
    challenge.finish(&client, &context).unwrap()
}
pub(super) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}
pub(super) fn hosted_owner() -> (tempfile::TempDir, SessionOwner) {
    let (temp, mut owner) = owner();
    let (candidate, _) = owner
        .prepare_workspace_host_delegation(SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    owner.activate_delivery().unwrap();
    (temp, owner)
}
#[test]
fn delivery_consumer_ack_is_committed_under_the_same_view_and_idempotent_after_reopen() {
    use crate::committed_authority::{CommittedAuthority, Observation};
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let id = committed
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.delivery_source(view, &session, now_ns())?;
            owner.register_delivery_consumer(&source)
        })
        .unwrap()
        .unwrap();
    let token = {
        let current = committed.read().unwrap();
        let source = owner
            .delivery_source(current.view(), &session, now_ns())
            .unwrap();
        let (seq, entry) = owner
            .delivery_ledger()
            .unwrap()
            .replay
            .last_key_value()
            .unwrap();
        owner
            .issue_delivery_cursor(&source, reference(*seq, entry))
            .unwrap()
    };
    let before_replay = owner.delivery_ledger().unwrap().replay.clone();
    let before = owner.status().generation;
    committed
        .with_owner_view(&mut owner, |view, owner| {
            assert!(matches!(committed.observe(), Observation::Updating));
            let source = owner.delivery_source(view, &session, now_ns())?;
            let cut = owner.verify_delivery_cursor(&source, &token)?.into_cut();
            owner.acknowledge_delivery(&source, cut)
        })
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, before + 1);
    assert_eq!(owner.delivery_ledger().unwrap().replay, before_replay);
    assert!(
        owner.delivery_ledger().unwrap().consumers[&owner.identity.native_cert.device_public_key]
            .acknowledged
            .is_some()
    );
    let path = owner.root().to_owned();
    drop(committed);
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let generation = owner.status().generation;
    committed
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.delivery_source(view, &session, now_ns())?;
            assert_eq!(owner.register_delivery_consumer(&source)?, id);
            let cut = owner.verify_delivery_cursor(&source, &token)?.into_cut();
            owner.acknowledge_delivery(&source, cut)
        })
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, generation);
}

#[test]
fn delivery_real_owner_faults_do_not_acknowledge_and_reopen_reconciles_full_aggregate() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (_temp, mut owner) = owner();
        owner.activate_delivery().unwrap();
        let path = owner.root().to_owned();
        let before = owner.status().generation;
        let mut author = owner.peer.clone();
        let bytes = upload(&owner, &mut author, "faulted-upload");
        let result = with_fault(&path, point, || send(&mut owner, 1, &bytes));
        assert!(
            result.is_err(),
            "fault returned durable result at {point:?}"
        );
        assert_eq!(owner.status().generation, before);
        assert!(!owner.permits_publication());
        assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
        assert!(send(&mut owner, 1, &bytes).is_err());
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        assert!(owner.permits_publication());
        let installed = point != Point::BeforeWrite;
        assert_eq!(
            owner.delivery_ledger().unwrap().admissions.len(),
            usize::from(installed)
        );
        assert_eq!(
            owner.delivery_ledger().unwrap().replay.len(),
            if installed { 2 } else { 1 }
        );
        let recovered = durable(send(&mut owner, 1, &bytes).unwrap());
        assert_eq!(recovered.committed.sequence, before + 1);
        assert_eq!(owner.status().generation, before + 1);
        assert_eq!(owner.delivery_ledger().unwrap().replay.len(), 2);
    }
}
#[test]
fn delivery_failed_activation_never_changes_live_writer_floor() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (_temp, mut owner) = owner();
        let path = owner.root().to_owned();
        let generation = owner.status().generation;
        assert!(with_fault(&path, point, || owner.activate_delivery()).is_err());
        assert!(!owner.peer.admission_v5_selected());
        assert_eq!(owner.status().generation, generation);
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(
            owner.peer.admission_v5_selected(),
            point != Point::BeforeWrite
        );
        if point != Point::BeforeWrite {
            assert_eq!(owner.delivery_ledger().unwrap().replay.len(), 1);
            assert_eq!(
                owner.delivery_ledger().unwrap().resolved_through,
                generation + 1
            );
        }
    }
}

#[test]
fn delivery_replay_metadata_cannot_mint_native_admission() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let before = owner.status();
    let mut next = owner.aggregate.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
        unreachable!()
    };
    let (_, entry) = delivery.replay.first_key_value().unwrap();
    let generation = entry.owner_generation;
    let entry = delivery.replay.get_mut(&generation).unwrap();
    entry.admitted_operations.push([99; 32]);
    entry.digest = replay_digest(
        delivery.store_generation,
        &next.room,
        owner.workspace_id,
        entry,
    )
    .unwrap();
    assert!(
        owner.commit(next, owner.peer.clone()).is_err(),
        "public replay DTO minted native evidence"
    );
    assert_eq!(owner.status(), before);
}
#[test]
fn delivery_legacy_exact_duplicate_without_mapping_gets_a_genuine_current_group() {
    let (_temp, mut owner) = owner();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "legacy-exact");
    let message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    owner.peer.select_admission_v5().unwrap();
    owner
        .peer
        .receive_from_report(
            TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
            message,
            SystemTime::now(),
        )
        .unwrap();
    // A genuine legacy native-v5 import has exact evidence but no delivery index.
    owner.aggregate.peer = owner.peer.durable().unwrap();
    owner
        .store
        .commit(&encode(&owner.aggregate, &owner.identity).unwrap())
        .unwrap();
    owner.activate_delivery().unwrap();
    let generation = owner.status().generation;
    let result = durable(send(&mut owner, 1, &bytes).unwrap());
    assert_eq!(result.result, DurableAdmissionKind::Duplicate);
    assert_eq!(result.committed.sequence, generation + 1);
    assert_eq!(owner.status().generation, generation + 1);
    assert!(
        owner.delivery_ledger().unwrap().replay[&(generation + 1)]
            .admitted_operations
            .contains(&result.operations[0])
    );
}
#[test]
fn delivery_admission_metadata_reference_and_aggregate_pressure_preserve_accepted_evidence() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "accepted-before-pressure");
    let record = durable(send(&mut owner, 1, &bytes).unwrap());
    // Bulk-fill metadata with already proven exact duplicate associations. This
    // exercises the real aggregate barrier without 4095 redundant SSD writes.
    let mut next = owner.aggregate.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
        unreachable!()
    };
    let mut duplicate = record.clone();
    duplicate.result = DurableAdmissionKind::Duplicate;
    for i in 2..=MAX_ADMISSIONS as u128 {
        delivery.admissions.insert(
            ReceiptKey {
                device: owner.peer.authority().device_public_key,
                delivery: i.to_le_bytes(),
            },
            duplicate.clone(),
        );
    }
    owner.commit(next, owner.peer.clone()).unwrap();
    let before = owner.status();
    let file = std::fs::read(owner.root().join("session.bin")).unwrap();
    assert!(send(&mut owner, 9, &bytes).is_err());
    assert_eq!(owner.status(), before);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        file
    );
    assert_eq!(durable(send(&mut owner, 1, &bytes).unwrap()), record);
    let mut metadata = owner.aggregate.clone();
    let PublicationStorage::V5 { delivery, .. } = &mut metadata.publication else {
        unreachable!()
    };
    let entry = delivery.replay.values_mut().next().unwrap();
    entry
        .publications
        .resize(MAX_REFERENCES, entry.publications[0].clone());
    entry
        .admitted_operations
        .resize(MAX_REFERENCES, entry.admitted_operations[0]);
    let mut previous = None;
    for (sequence, entry) in &mut delivery.replay {
        entry.previous = previous;
        entry.digest = replay_digest(
            delivery.store_generation,
            &metadata.room,
            owner.workspace_id,
            entry,
        )
        .unwrap();
        previous = Some(reference(*sequence, entry));
    }
    for admission in delivery.admissions.values_mut() {
        admission.committed.digest = delivery.replay[&admission.committed.sequence].digest;
    }
    let error = owner.commit(metadata, owner.peer.clone()).unwrap_err();
    assert!(
        error.to_string().contains("quota"),
        "expected encoded ledger byte cap: {error}"
    );
    let mut refs = owner.aggregate.clone();
    let PublicationStorage::V5 { roots, .. } = &mut refs.publication else {
        unreachable!()
    };
    roots
        .baseline
        .resize(MAX_REFERENCES + 1, roots.baseline[0].clone());
    assert!(owner.commit(refs, owner.peer.clone()).is_err());
    let mut aggregate = owner.aggregate.clone();
    aggregate.pending.push(PendingOperation {
        invocation_id: "oversize".into(),
        encoded_request: vec![0; crate::store::MAX_AGGREGATE_BYTES],
        outcome_unknown: true,
    });
    assert!(owner.commit(aggregate, owner.peer.clone()).is_err());
    assert!(owner.permits_publication());
    assert_eq!(owner.status(), before);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        file
    );
}

#[test]
fn delivery_authenticated_replay_requires_actual_native_coverage_not_response_metadata() {
    use crate::committed_authority::CommittedAuthority;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    committed
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.delivery_source(view, &session, now_ns())?;
            owner.register_delivery_consumer(&source)
        })
        .unwrap()
        .unwrap();
    let mut replica = owner.peer.clone();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "native-coverage-only");
    let original: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let receipt = committed
        .with_owner_view(&mut owner, |view, owner| {
            let source = owner.delivery_source(view, &session, now_ns())?;
            owner.admit_delivery(
                &source,
                CompleteStateItem {
                    delivery_id: [1; 16],
                    canonical: &bytes,
                },
                SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    let receipt = durable(receipt);
    assert!(
        !replica.covers(&original),
        "a public delivery response is not native coverage"
    );
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    // The committed group is the author's exact signed item (no host copy):
    // it converges a replica but is not itself coverage.
    let replay = owner.replay_delivery(&source, &receipt.committed).unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].bytes, bytes.as_slice());
    let governor = owner.peer.authority().device_public_key;
    let receive = move |replica: &mut StatePeer, replay: Vec<PublicationView<'_>>| {
        for publication in replay {
            let message: NeighbourMsg = postcard::from_bytes(publication.bytes).unwrap();
            replica
                .receive_from(
                    TransportOrigin::RelayDelta(governor),
                    message,
                    SystemTime::now(),
                )
                .unwrap();
        }
    };
    receive(&mut replica, replay);
    assert_eq!(replica.rows(), owner.peer.rows());
    assert!(
        !replica.covers(&original),
        "an incremental group is not coverage"
    );
    drop(source);
    drop(current);
    // Coverage comes only from the next actual native certified checkpoint.
    committed
        .with_owner_view(&mut owner, |_, owner| {
            owner.retain_delivery(
                crate::owner::retention::RetentionAction::Checkpoint,
                SystemTime::now(),
            )
        })
        .unwrap()
        .unwrap();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    let (sequence, entry) = owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    let checkpoint = reference(*sequence, entry);
    receive(
        &mut replica,
        owner.replay_delivery(&source, &checkpoint).unwrap(),
    );
    assert!(replica.covers(&original));
    let coverage = replica
        .durable_coverage("so.tilo.chat", "")
        .unwrap()
        .unwrap();
    assert_eq!(coverage.generation, checkpoint.sequence);
    assert_eq!(replica.rows(), owner.peer.rows());
}

pub(super) fn register_and_cursor(
    owner: &mut SessionOwner,
    committed: &crate::committed_authority::CommittedAuthority,
    session: &tilo_relay::delivery_v3::auth::VerifiedSession,
) -> tilo_relay::delivery_v3::cursor::CursorToken {
    committed
        .with_owner_view(owner, |view, owner| {
            let source = owner.delivery_source(view, session, now_ns())?;
            owner.register_delivery_consumer(&source)
        })
        .unwrap()
        .unwrap();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), session, now_ns())
        .unwrap();
    let (sequence, entry) = owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    owner
        .issue_delivery_cursor(&source, reference(*sequence, entry))
        .unwrap()
}
#[test]
fn delivery_cursor_native_scope_checks_and_restore_reset_survive_content_mutation() {
    use crate::committed_authority::CommittedAuthority;
    use tilo_relay::delivery_v3::cursor::CursorToken;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let token = register_and_cursor(&mut owner, &committed, &session);
    let consumer = owner.delivery_ledger().unwrap().consumers
        [&owner.identity.native_cert.device_public_key]
        .clone();
    committed
        .with_owner(&mut owner, |owner| {
            mutate_owned_record_for_test(owner, "so.tilo.chat", "", |doc| {
                doc.put(ROOT, "content-only", "retains policy").unwrap();
            })
        })
        .unwrap()
        .unwrap();
    {
        let current = committed.read().unwrap();
        let source = owner
            .delivery_source(current.view(), &session, now_ns())
            .unwrap();
        owner.verify_delivery_cursor(&source, &token).unwrap();
        let good = token
            .authenticate(&owner.identity.host.verifying_key())
            .unwrap()
            .issue()
            .clone();
        for field in 0..6 {
            let mut wrong = good.clone();
            match field {
                0 => wrong.consumer = [33; 16],
                1 => wrong.store_generation = [34; 16],
                2 => wrong.authorization_view = [35; 32],
                3 => wrong.committed.digest = [36; 32],
                4 => wrong.committed.sequence = owner.status().generation + 1,
                _ => wrong.consumer_root = [37; 32],
            }
            let token = CursorToken::issue(wrong, &owner.identity.host).unwrap();
            assert!(
                owner.verify_delivery_cursor(&source, &token).is_err(),
                "cursor scope {field} accepted"
            );
        }
    }
    assert_eq!(
        owner.delivery_ledger().unwrap().consumers[&consumer.device].authorization_view,
        consumer.authorization_view
    );
    committed
        .with_owner(
            &mut owner,
            SessionOwner::rotate_store_generation_for_restore,
        )
        .unwrap()
        .unwrap();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    assert!(owner.verify_delivery_cursor(&source, &token).is_err());
    assert_eq!(
        owner.delivery_ledger().unwrap().consumers[&consumer.device].id,
        consumer.id
    );
    assert!(
        owner.delivery_ledger().unwrap().consumers[&consumer.device]
            .acknowledged
            .is_none()
    );
}
#[test]
fn delivery_ack_failure_reopens_monotonic_progress_without_releasing_replay() {
    use crate::{
        committed_authority::CommittedAuthority,
        store::test_fault::{Point, with_fault},
    };
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (_temp, mut owner) = hosted_owner();
        let committed = CommittedAuthority::new(&owner);
        let session = authenticated_session(&owner, &committed);
        let token = register_and_cursor(&mut owner, &committed, &session);
        let path = owner.root().to_owned();
        let replay = owner.delivery_ledger().unwrap().replay.clone();
        let outcome = with_fault(&path, point, || {
            committed.with_owner_view(&mut owner, |view, owner| {
                let source = owner.delivery_source(view, &session, now_ns())?;
                let cut = owner.verify_delivery_cursor(&source, &token)?.into_cut();
                owner.acknowledge_delivery(&source, cut)
            })
        });
        assert!(outcome.is_err());
        assert!(!owner.permits_publication());
        assert_eq!(owner.delivery_ledger().unwrap().replay, replay);
        assert!(
            owner.delivery_ledger().unwrap().consumers
                [&owner.identity.native_cert.device_public_key]
                .acknowledged
                .is_none()
        );
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        assert_eq!(
            owner.delivery_ledger().unwrap().consumers
                [&owner.identity.native_cert.device_public_key]
                .acknowledged
                .is_some(),
            point != Point::BeforeWrite
        );
        assert_eq!(owner.delivery_ledger().unwrap().replay, replay);
        let committed = CommittedAuthority::new(&owner);
        let session = authenticated_session(&owner, &committed);
        committed
            .with_owner_view(&mut owner, |view, owner| {
                let source = owner.delivery_source(view, &session, now_ns())?;
                let cut = owner.verify_delivery_cursor(&source, &token)?.into_cut();
                owner.acknowledge_delivery(&source, cut)
            })
            .unwrap()
            .unwrap();
        assert_eq!(owner.delivery_ledger().unwrap().replay, replay);
    }
}
#[test]
fn delivery_restore_fault_preserves_a_complete_old_or_new_incarnation() {
    use crate::store::test_fault::{Point, with_fault};
    for point in [
        Point::BeforeWrite,
        Point::AfterAtomicWrite,
        Point::DirectoryBarrier,
    ] {
        let (_temp, mut owner) = owner();
        owner.activate_delivery().unwrap();
        let mut author = owner.peer.clone();
        let bytes = upload(&owner, &mut author, "restore-fault");
        let old_receipt = durable(send(&mut owner, 1, &bytes).unwrap());
        let old = owner.delivery_ledger().unwrap().clone();
        let path = owner.root().to_owned();
        assert!(with_fault(&path, point, || owner.rotate_store_generation_for_restore()).is_err());
        assert_eq!(owner.delivery_ledger().unwrap(), &old);
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        assert_eq!(
            owner.delivery_ledger().unwrap().store_generation == old.store_generation,
            point == Point::BeforeWrite
        );
        let receipt = durable(send(&mut owner, 1, &bytes).unwrap());
        assert_eq!(receipt.operations, old_receipt.operations);
        assert_eq!(receipt.digest, old_receipt.digest);
        assert_eq!(receipt.committed.sequence, old_receipt.committed.sequence);
    }
}

#[test]
fn delivery_enrollment_and_governance_transactions_enter_replay_while_receipt_only_gaps_do_not() {
    use crate::{collaboration::*, enrollment::EnrollmentAuthority};
    use qos_peer_state::{enrollment, join::*};
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let now = now_ns();
    let enrolled = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let mut enrollment = EnrollmentAuthority::default();
    let make_proof = |enrollment: &mut EnrollmentAuthority, owner: &SessionOwner, id: u8| {
        let challenge = enrollment
            .challenge(
                owner,
                enrollment::ChallengeRequest {
                    device_public_key: hex::encode(enrolled.verifying_key().to_bytes()),
                    request_id: hex::encode([id; 32]),
                },
                now,
            )
            .unwrap();
        enrollment::sign(
            &enrollment::decode(&challenge.challenge).unwrap(),
            &enrolled,
        )
        .unwrap()
    };
    let proof = make_proof(&mut enrollment, &owner, 1);
    enrollment.complete(&mut owner, proof, now).unwrap();
    let generation = owner.status().generation;
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&generation)
    );
    let count = owner.delivery_ledger().unwrap().replay.len();
    let proof = make_proof(&mut enrollment, &owner, 2);
    enrollment.complete(&mut owner, proof, now).unwrap();
    assert_eq!(owner.delivery_ledger().unwrap().replay.len(), count);
    assert_eq!(
        owner.delivery_ledger().unwrap().resolved_through,
        generation + 1
    );
    let root = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
    let mut guest = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "delivery guest".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now - 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut guest, &root).unwrap();
    let admin = owner.peer.authority().device_public_key;
    let service = CollaborationService::new(vec![]);
    let invitation = service
        .issue(
            &mut owner,
            admin,
            IssueInvitationRequest {
                request_id: [11; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: Some("delivery guest".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&owner.status().generation)
    );
    let before_challenge = owner.delivery_ledger().unwrap().replay.len();
    let challenge = service
        .challenge(
            &mut owner,
            guest.device_public_key,
            JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: guest.clone(),
                request_id: [12; 32],
            },
            now,
        )
        .unwrap();
    assert_eq!(
        owner.delivery_ledger().unwrap().replay.len(),
        before_challenge
    );
    service
        .complete(
            &mut owner,
            guest.device_public_key,
            JoinCompleteRequest {
                proof: sign_join_proof(challenge.challenge, &key).unwrap(),
            },
            now,
        )
        .unwrap();
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&owner.status().generation)
    );
    service
        .set_member_role(
            &mut owner,
            admin,
            SetMemberRoleRequest {
                request_id: [13; 32],
                user_public_key: guest.user_public_key,
                role: MemberRole::Viewer,
            },
            now,
        )
        .unwrap();
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&owner.status().generation)
    );
    service
        .remove_member(
            &mut owner,
            admin,
            RemoveMemberRequest {
                request_id: [14; 32],
                user_public_key: guest.user_public_key,
            },
            now,
        )
        .unwrap();
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&owner.status().generation)
    );
    assert_eq!(
        owner.delivery_ledger().unwrap().resolved_through,
        owner.status().generation
    );
}
#[test]
fn delivery_deferred_encryption_metadata_has_no_admission_or_replay_and_blocks_activation() {
    for selected in [false, true] {
        let (_temp, mut owner) = owner();
        if selected {
            owner.activate_delivery().unwrap();
        }
        let before = owner.status().generation;
        let mut encrypted = owner
            .peer
            .clone()
            .signed_snapshots(&owner.identity.native, SystemTime::now())
            .unwrap()
            .remove(0);
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = &mut encrypted
        else {
            unreachable!()
        };
        envelope.seq += 1;
        envelope.encryption = Some(collab_types::EncryptionMetadata {
            epoch: 1,
            channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
        });
        envelope.payload = vec![1, 2, 3];
        collab_types::sign_op(envelope, &owner.identity.native).unwrap();
        if selected {
            let count = owner.delivery_ledger().unwrap().replay.len();
            assert!(matches!(
                send(&mut owner, 1, &postcard::to_stdvec(&encrypted).unwrap()).unwrap(),
                AdmissionOutcome::RetryLater(MissingPrerequisite::PersistedMls)
            ));
            assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
            assert_eq!(owner.delivery_ledger().unwrap().replay.len(), count);
        } else {
            owner
                .receive(
                    TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
                    encrypted,
                )
                .unwrap();
            assert!(owner.activate_delivery().is_err());
            assert!(!owner.peer.admission_v5_selected());
        }
        assert_eq!(owner.status().generation, before + 1);
        assert!(!owner.permits_publication());
        let path = owner.root().to_owned();
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert!(!owner.permits_publication());
        assert_eq!(owner.status().generation, before + 1);
    }
}

#[test]
fn delivery_policy_denies_fine_or_malformed_visibility_and_expired_sessions() {
    use crate::committed_authority::CommittedAuthority;
    use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
    use ui::automerge::{ObjType, ReadDoc};
    for malformed in [false, true] {
        let (_temp, mut owner) = hosted_owner();
        let committed = CommittedAuthority::new(&owner);
        let session = authenticated_session(&owner, &committed);
        let token = register_and_cursor(&mut owner, &committed, &session);
        {
            let current = committed.read().unwrap();
            assert!(
                owner
                    .delivery_source(current.view(), &session, session.expires_at_ns() + 1)
                    .is_err()
            );
        }
        committed
            .with_owner(&mut owner, |owner| -> Result<()> {
                let mut room = owner.peer.admitted_room().unwrap().clone();
                let caps = match room.get(ROOT, "capabilities")? {
                    Some((_, map)) => map,
                    None => room.put_object(ROOT, "capabilities", ObjType::Map)?,
                };
                let cap = CapOverride::new(
                    Subject::User(collab_types::Ed25519PublicKey::from_bytes(
                        owner.identity.native_cert.user_public_key,
                    )),
                    Scope::App("so.tilo.chat".into()),
                    Effect::Grant,
                    Verb::Read,
                );
                room.put(
                    caps,
                    "delivery-unsupported",
                    if malformed {
                        vec![0xff]
                    } else {
                        serde_json::to_vec(&cap)?
                    },
                )?;
                room.commit();
                let message = NeighbourMsg::SyncResp {
                    room: owner.peer.room().into(),
                    changes: room
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                };
                owner.receive(
                    TransportOrigin::RelayDelta(owner.peer.authority().device_public_key),
                    message,
                )?;
                Ok(())
            })
            .unwrap()
            .unwrap();
        let current = committed.read().unwrap();
        assert!(
            owner
                .delivery_source(current.view(), &session, now_ns())
                .is_err(),
            "unsupported policy authorized cursor {:?}",
            token.as_bytes().len()
        );
    }
}
#[test]
fn delivery_current_revocation_denies_previously_authenticated_host() {
    use crate::committed_authority::CommittedAuthority;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    register_and_cursor(&mut owner, &committed, &session);
    committed
        .with_owner(&mut owner, |owner| -> Result<()> {
            let mut entry = tilo_user_identity::RevocationEntry {
                user_public_key: owner.identity.host_cert.user_public_key,
                revoked_device_public_key: owner.identity.host_cert.device_public_key,
                revoked_at_ns: now_ns(),
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut entry, &owner.identity.root)?;
            let mut candidate = owner.peer.clone();
            candidate
                .publish_governed_revocations(&[entry], &owner.identity.native, SystemTime::now())
                .map_err(anyhow::Error::msg)?;
            owner.commit_mutation_candidate(candidate, owner.directory().to_vec())?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    let current = committed.read().unwrap();
    assert!(
        owner
            .delivery_source(current.view(), &session, now_ns())
            .is_err()
    );
}
#[test]
fn delivery_bounded_item_and_original_signature_fail_closed_before_effects() {
    let (_temp, mut owner) = owner();
    owner.activate_delivery().unwrap();
    let mut author = owner.peer.clone();
    let bytes = upload(&owner, &mut author, "bounded");
    let before = owner.status();
    assert!(send(&mut owner, 0, &bytes).is_err());
    assert!(send(&mut owner, 1, &[]).is_err());
    assert!(send(&mut owner, 1, &vec![0; MAX_DELIVERY_ITEM_BYTES + 1]).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(send(&mut owner, 1, &trailing).is_err());
    let mut forged: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = &mut forged else {
        unreachable!()
    };
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    assert!(send(&mut owner, 1, &postcard::to_stdvec(&forged).unwrap()).is_err());
    assert_eq!(owner.status(), before);
    assert!(owner.delivery_ledger().unwrap().admissions.is_empty());
}

#[test]
fn delivery_replay_guard_prevents_concurrent_owner_update_until_selection_finishes() {
    use crate::committed_authority::CommittedAuthority;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let token = register_and_cursor(&mut owner, &committed, &session);
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    owner.verify_delivery_cursor(&source, &token).unwrap();
    let claims = token
        .authenticate(&owner.identity.host.verifying_key())
        .unwrap();
    let copied = owner
        .replay_delivery(&source, &claims.issue().committed)
        .unwrap()
        .iter()
        .map(|p| p.bytes.to_vec())
        .collect::<Vec<_>>();
    assert!(!copied.is_empty());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = committed.clone();
    let waiting = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        writer
            .with_owner(&mut owner, |owner| {
                owner.queue_pending("after-replay", &[1], false)
            })
            .unwrap()
            .unwrap();
        done_tx.send(()).unwrap();
        owner
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(20))
            .is_err()
    );
    drop(current);
    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let owner = waiting.join().unwrap();
    assert_eq!(owner.pending().len(), 1);
}

#[test]
fn delivery_wrong_consumer_is_denied_before_any_requested_index_lookup() {
    use crate::committed_authority::CommittedAuthority;
    use tilo_relay::delivery_v3::cursor::CursorToken;
    let (_temp, mut owner) = hosted_owner();
    let committed = CommittedAuthority::new(&owner);
    let session = authenticated_session(&owner, &committed);
    let token = register_and_cursor(&mut owner, &committed, &session);
    let mut issue = token
        .authenticate(&owner.identity.host.verifying_key())
        .unwrap()
        .issue()
        .clone();
    issue.consumer_device = [42; 32];
    issue.committed.sequence = owner.status().generation + 100;
    let wrong = CursorToken::issue(issue, &owner.identity.host).unwrap();
    let current = committed.read().unwrap();
    let source = owner
        .delivery_source(current.view(), &session, now_ns())
        .unwrap();
    let error = match owner.verify_delivery_cursor(&source, &wrong) {
        Ok(_) => panic!("foreign cursor accepted"),
        Err(error) => error,
    };
    assert!(
        error
            .downcast_ref::<error::OwnerDataFailure>()
            .is_some_and(|e| e.kind() == error::OwnerDataFailureKind::CurrentDenied),
        "foreign identity reached requested index lookup: {error}"
    );
}

#[test]
fn delivery_cross_owner_ack_cut_is_denied_before_requested_index_lookup() {
    use crate::committed_authority::CommittedAuthority;
    let (_first_temp, mut first) = hosted_owner();
    // This real certified position exists only in the first owner.
    first.refresh_checkpoint().unwrap();
    let first_committed = CommittedAuthority::new(&first);
    let first_session = authenticated_session(&first, &first_committed);
    let token = register_and_cursor(&mut first, &first_committed, &first_session);
    let cut = {
        let current = first_committed.read().unwrap();
        let source = first
            .delivery_source(current.view(), &first_session, now_ns())
            .unwrap();
        first
            .verify_delivery_cursor(&source, &token)
            .unwrap()
            .into_cut()
    };
    let (_second_temp, mut second) = hosted_owner();
    let second_committed = CommittedAuthority::new(&second);
    let second_session = authenticated_session(&second, &second_committed);
    register_and_cursor(&mut second, &second_committed, &second_session);
    let sequence = token
        .authenticate(&first.identity.host.verifying_key())
        .unwrap()
        .issue()
        .committed
        .sequence;
    assert!(
        !second
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&sequence)
    );
    let before = std::fs::read(second.root().join("session.bin")).unwrap();
    let generation = second.status().generation;
    let error = second_committed
        .with_owner_view(&mut second, |view, owner| {
            let source = owner.delivery_source(view, &second_session, now_ns())?;
            owner.acknowledge_delivery(&source, cut)
        })
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<error::OwnerDataFailure>()
            .is_some_and(|e| e.kind() == error::OwnerDataFailureKind::CurrentDenied),
        "cross-owner cut reached requested index lookup: {error}"
    );
    assert_eq!(second.status().generation, generation);
    assert_eq!(
        std::fs::read(second.root().join("session.bin")).unwrap(),
        before
    );
    assert!(
        second
            .delivery_ledger()
            .unwrap()
            .consumers
            .values()
            .all(|consumer| consumer.acknowledged.is_none())
    );
}
