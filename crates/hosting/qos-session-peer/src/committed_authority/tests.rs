use super::*;
use crate::{SessionOwner, action_host::Authority};
use qos_action_broker::peer_actions::CertificateAuthority;

#[test]
fn committed_authority_survives_action_transport_unavailability() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let actions = Authority::new(committed.clone());
    let room = owner.peer().room().parse().unwrap();
    let cert = owner.peer().authority();
    actions.set_ready(true);
    assert!(actions.authorize(room, cert).is_ok());
    actions.set_ready(false);
    assert!(actions.authorize(room, cert).is_err());
    let Observation::Current(current) = committed.observe() else {
        panic!("transport failure erased durable authority")
    };
    assert_eq!(current.generation(), owner.committed_generation());
    assert!(
        current
            .peer()
            .permissions(cert.device_public_key, web_time::SystemTime::now())
            .is_ok()
    );
}

#[test]
fn committed_authority_fences_updates_and_publishes_only_after_durability() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let before = owner.committed_generation();
    let update = committed.begin(&owner).unwrap();
    assert!(matches!(committed.observe(), Observation::Updating));
    use ui::automerge::{AutoCommit, ROOT, transaction::Transactable};
    let mut candidate = owner.peer().clone();
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
    document
        .put(ROOT, "committed-authority-test", true)
        .unwrap();
    candidate
        .publish_document(
            app,
            record,
            document.save(),
            &owner.identity.native,
            web_time::SystemTime::now(),
        )
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    assert!(matches!(committed.observe(), Observation::Updating));
    update.publish(&owner).unwrap();
    let Observation::Current(current) = committed.observe() else {
        panic!("missing committed view")
    };
    assert_eq!(current.generation(), before + 1);
    assert_eq!(current.generation(), owner.committed_generation());
    use ui::automerge::ReadDoc;
    let committed_doc = AutoCommit::load(&current.peer().document(app, record).unwrap()).unwrap();
    assert!(
        committed_doc
            .get(ROOT, "committed-authority-test")
            .unwrap()
            .unwrap()
            .0
            .to_bool()
            .unwrap()
    );
}

#[test]
fn committed_authority_failed_durable_commit_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let update = committed.begin(&owner).unwrap();
    std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    std::fs::create_dir(path.join("session.bin")).unwrap();
    assert!(owner.flush().is_err());
    assert!(!owner.permits_publication());
    assert!(update.publish(&owner).is_err());
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn committed_authority_abandoned_turn_denies_and_wrong_owner_cannot_replace_view() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let other = SessionOwner::initialize(&dir.path().join("other")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    assert!(committed.begin(&other).is_err());
    assert!(matches!(committed.observe(), Observation::Current(_)));
    drop(committed.begin(&owner).unwrap());
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn committed_authority_incomplete_commit_preserves_evidence_and_keeps_owner_turns_alive() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let retained_time = committed.read().unwrap().view().policy_time().unwrap();
    let author = owner.identity.native.verifying_key().to_bytes();
    let mut encrypted = owner
        .peer()
        .clone()
        .signed_snapshots(&owner.identity.native, web_time::SystemTime::now())
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
        panic!("signed snapshot")
    };
    envelope.seq += 1;
    envelope.encryption = Some(collab_types::EncryptionMetadata {
        epoch: 1,
        channel: collab_types::document_channel_id(app_id, cluster_id, record_id),
    });
    envelope.payload = vec![1, 2, 3];
    collab_types::sign_op(envelope, &owner.identity.native).unwrap();
    committed
        .with_owner(&mut owner, |owner| {
            owner.receive(TransportOrigin::RelayDelta(author), encrypted)
        })
        .expect("incompleteness must not end the host loop")
        .unwrap()
        .unwrap();
    assert!(owner.peer().incomplete_reason().is_some());
    assert!(
        retained_time.now_ns().is_err(),
        "actual incomplete publication retires queued time"
    );
    let incomplete_generation = owner.committed_generation();
    assert!(matches!(committed.observe(), Observation::Denied));
    let evidence = owner.peer().durable().unwrap();
    assert!(
        committed
            .with_owner_view(&mut owner, |_, _| panic!(
                "incomplete owner has no committed view"
            ))
            .is_err()
    );
    assert!(owner.durability_healthy());
    committed
        .with_owner(&mut owner, SessionOwner::flush)
        .unwrap()
        .unwrap();
    assert_eq!(owner.committed_generation(), incomplete_generation);
    assert_eq!(owner.peer().durable().unwrap(), evidence);
    assert!(matches!(committed.observe(), Observation::Denied));
    let update = committed.begin(&owner).unwrap();
    assert!(matches!(committed.observe(), Observation::Denied));
    update.publish(&owner).unwrap();
    assert!(matches!(committed.observe(), Observation::Denied));
}

#[test]
fn committed_authority_recoverable_fence_requires_new_durable_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let clock = committed.policy_clock().unwrap();
    let old_time = committed.read().unwrap().view().policy_time().unwrap();
    let generation = owner.committed_generation();
    let documents = owner.peer().rows();
    let evidence = owner.peer().durable().unwrap();
    // Authority-state unit fixture only: the actual owner and every admitted
    // record remain intact. This does not simulate completion of MLS recovery.
    enter_test_recovery(&committed);
    assert!(old_time.now_ns().is_err());
    assert!(
        committed
            .with_owner_view(&mut owner, |_, _| panic!(
                "recovery cannot fabricate a current view"
            ))
            .is_err()
    );
    committed
        .with_owner(&mut owner, SessionOwner::flush)
        .unwrap()
        .unwrap();
    assert!(matches!(committed.observe(), Observation::Denied));
    assert_eq!(owner.peer().durable().unwrap(), evidence);
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("recovery-observation", b"retained", false)
        })
        .unwrap()
        .unwrap();
    let Observation::Current(view) = committed.observe() else {
        panic!("new validated committed view was not published")
    };
    assert!(view.generation() > generation);
    assert_eq!(view.peer().durable().unwrap(), evidence);
    assert_eq!(view.peer().rows(), documents);
    let recovered_time = view.view().policy_time().unwrap();
    assert!(
        old_time.now_ns().is_err(),
        "recovery must never resurrect queued old work"
    );
    assert!(!old_time.same_origin(&recovered_time));
    assert!(recovered_time.belongs_to(&clock));
    assert!(committed.policy_clock().unwrap().same_clock(&clock));
    assert!(view.view().check_policy_time(&old_time).is_err());
    drop(view);
    committed.invalidate();
    assert!(
        recovered_time.now_ns().is_err(),
        "recovery completed first must still lose its queued origin on terminal retirement"
    );
    owner.flush().unwrap();
    assert!(committed.begin(&owner).is_err());
    assert!(matches!(committed.observe(), Observation::Denied));
}

#[cfg(feature = "media")]
#[test]
fn committed_authority_recovery_requires_fresh_media_grant_proof_and_session() {
    use crate::media_authority::*;
    use crate::media_service::{MediaService, encode_relay_proof};
    use ed25519_dalek::Signer;
    use qos_moq::relay::{Direction, RelayAuthority};
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    crate::room_media_policy::tests::activate(&mut owner);
    let committed = CommittedAuthority::new(&owner);
    let service = MediaService::for_owner(
        &owner,
        committed.clone(),
        "https://relay.test/moq/v21/voice",
    )
    .unwrap();
    let request = GrantRequest {
        device: owner.identity.native.verifying_key().to_bytes(),
        room: owner.peer().room().into(),
        call_id: hex::encode(crate::room_media_policy::tests::CALL),
        participation_id: [8; 32],
        generation: 1,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    };
    let issue = |owner: &SessionOwner| {
        let challenge = service
            .challenge(request.clone(), GrantOperation::Join)
            .unwrap();
        let GrantCompletion::Granted(grant) = service
            .complete(
                owner,
                challenge.nonce,
                owner
                    .identity
                    .native
                    .sign(&challenge.signing_digest)
                    .to_bytes(),
            )
            .unwrap()
        else {
            panic!("grant")
        };
        grant
    };
    let prove = |owner: &SessionOwner, grant: &MediaGrant| {
        let c = service.relay_challenge(grant.as_bytes()).unwrap();
        encode_relay_proof(
            c.session_id,
            c.challenge.nonce,
            owner
                .identity
                .native
                .sign(&c.challenge.signing_digest)
                .to_bytes(),
        )
        .unwrap()
    };
    let old_grant = issue(&owner);
    let old_permit = service.admit(&prove(&owner, &old_grant)).unwrap();
    let pending_proof = prove(&owner, &old_grant);
    let pending_leave = service
        .challenge(request.clone(), GrantOperation::Leave)
        .unwrap();
    let leave_signature = owner
        .identity
        .native
        .sign(&pending_leave.signing_digest)
        .to_bytes();
    // State-machine fixture: preserve every committed document. This tests the
    // authority continuity fence, not completion of missing MLS prerequisites.
    *committed.0.state.lock().unwrap() = State::Incomplete {
        generation: owner.committed_generation(),
        continuity: 1,
    };
    committed.0.recovering.store(true, Ordering::Release);
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("verified-recovery", b"retained", false)
        })
        .unwrap()
        .unwrap();
    assert!(
        !old_permit.current(Direction::Publish, old_permit.publication().unwrap()),
        "recovered authority revived an old media session"
    );
    assert!(service.admit(&pending_proof).is_err());
    assert!(service.relay_challenge(old_grant.as_bytes()).is_err());
    assert!(
        service
            .complete(&owner, pending_leave.nonce, leave_signature)
            .is_err()
    );
    let fresh = issue(&owner);
    assert_ne!(fresh.metadata.host_boot, old_grant.metadata.host_boot);
    let fresh_permit = service.admit(&prove(&owner, &fresh)).unwrap();
    assert!(fresh_permit.current(Direction::Publish, fresh_permit.publication().unwrap()));
    assert!(!old_permit.current(Direction::Publish, old_permit.publication().unwrap()));
}

#[test]
fn committed_authority_explicit_invalidation_wins_over_inflight_update() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let retained_time = committed.read().unwrap().view().policy_time().unwrap();
    let update = committed.begin(&owner).unwrap();
    std::thread::scope(|scope| {
        let other = committed.clone();
        let invalidation = scope.spawn(move || other.invalidate());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !committed.0.terminal.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(matches!(committed.observe(), Observation::Denied));
        assert!(retained_time.now_ns().is_err());
        assert!(update.publish(&owner).is_err());
        invalidation.join().unwrap();
    });
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn committed_owner_view_borrows_prior_generation_and_publishes_real_mutation() {
    use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let generation = owner.committed_generation();
    let app = "dev.qos.word-processor";
    let record = "tab:0000000000000001";
    let receipt = committed
        .with_owner_view(&mut owner, |view, owner| {
            assert_eq!(view.generation(), generation);
            assert_eq!(view.continuity(), 0);
            assert_eq!(view.peer().room(), owner.peer().room());
            assert!(matches!(committed.observe(), Observation::Updating));
            // observe uses try_lock, so this independent reader cannot block or
            // acquire a detached authority view during the mutation callback.
            std::thread::scope(|scope| {
                let reader = committed.clone();
                scope
                    .spawn(move || assert!(matches!(reader.observe(), Observation::Updating)))
                    .join()
                    .unwrap();
            });
            let mut candidate = owner.peer().clone();
            let mut document = AutoCommit::load(&candidate.document(app, record).unwrap()).unwrap();
            document.put(ROOT, "owner-view-mutation", true).unwrap();
            candidate
                .publish_document(
                    app,
                    record,
                    document.save(),
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )
                .unwrap();
            let receipt = owner.commit_mutation_candidate(candidate, owner.directory().to_vec());
            assert_eq!(
                view.generation(),
                generation,
                "callback retains the pre-mutation committed view"
            );
            let prior = AutoCommit::load(&view.peer().document(app, record).unwrap()).unwrap();
            assert!(prior.get(ROOT, "owner-view-mutation").unwrap().is_none());
            receipt
        })
        .unwrap()
        .unwrap();
    let current = committed.read().unwrap();
    assert_eq!(current.view().generation(), generation + 1);
    assert_eq!(current.view().generation(), receipt.generation);
    assert_eq!(current.view().continuity(), 0);
    assert!(std::ptr::eq(current.peer(), current.view().peer()));
    let document = AutoCommit::load(&current.view().peer().document(app, record).unwrap()).unwrap();
    assert_eq!(
        document
            .get(ROOT, "owner-view-mutation")
            .unwrap()
            .unwrap()
            .0
            .to_bool(),
        Some(true)
    );
    drop(current);
    let expected = owner.status();
    drop(owner);
    assert_eq!(SessionOwner::open(&root).unwrap().status(), expected);
}

#[test]
fn committed_owner_view_rejection_preserves_current_and_storage_failure_denies() {
    use qos_peer_state::{NeighbourMsg, TransportOrigin};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("owner");
    let mut owner = SessionOwner::initialize(&root).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let generation = owner.committed_generation();
    let mut message = owner
        .peer()
        .clone()
        .signed_snapshots(&owner.identity.native, web_time::SystemTime::now())
        .unwrap()
        .remove(0);
    let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
        panic!("signed snapshot")
    };
    envelope.payload.push(0xff); // Invalidates the actual native signature.
    let origin = TransportOrigin::RelayDelta(owner.identity.native.verifying_key().to_bytes());
    let rejected = committed
        .with_owner_view(&mut owner, |view, owner| {
            assert_eq!(view.generation(), generation);
            owner.receive(origin, message)
        })
        .unwrap();
    assert!(rejected.is_err());
    assert_eq!(committed.read().unwrap().view().generation(), generation);
    assert!(owner.durability_healthy());
    std::fs::rename(root.join("session.bin"), root.join("saved.bin")).unwrap();
    std::fs::create_dir(root.join("session.bin")).unwrap();
    assert!(
        committed
            .with_owner_view(&mut owner, |_, owner| owner.flush())
            .is_err()
    );
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn committed_owner_view_panic_abandons_guard_and_denies() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = committed.with_owner_view(&mut owner, |_, _| panic!("abandoned callback"));
    }));
    assert!(panic.is_err());
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn committed_owner_view_rejects_wrong_owner_and_generation_bypass() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let mut other = SessionOwner::initialize(&dir.path().join("other")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    assert!(
        committed
            .with_owner_view(&mut other, |_, _| panic!("wrong owner callback"))
            .is_err()
    );
    assert!(matches!(committed.observe(), Observation::Current(_)));
    owner
        .queue_pending("bypassed-owner-turn", b"retained", false)
        .unwrap();
    assert!(
        committed
            .with_owner_view(&mut owner, |_, _| panic!("bypassed generation callback"))
            .is_err()
    );
    assert!(matches!(committed.observe(), Observation::Denied));
}

fn publish_private_cap(owner: &mut SessionOwner, committed: &CommittedAuthority, expiry: u64) {
    use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};
    use ui::automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
    committed
        .with_owner(owner, |owner| {
            let mut candidate = owner.peer().clone();
            let mut doc = candidate.admitted_room().unwrap().clone();
            doc.set_actor(ui::automerge::ActorId::from(
                owner.identity.native.verifying_key().to_bytes().to_vec(),
            ));
            let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
            let cap = CapOverride::grant(
                Subject::Role(collab_types::Role::Editor),
                Scope::Workspace,
                Verb::Read,
            )
            .expiring(Expiry::AtUnix(expiry));
            doc.put(
                &caps,
                "disclosure-test",
                ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
            doc.commit();
            candidate
                .receive_from(
                    qos_peer_state::TransportOrigin::RelayDelta(
                        owner.identity.native.verifying_key().to_bytes(),
                    ),
                    qos_peer_state::NeighbourMsg::SyncResp {
                        room: owner.peer().room().into(),
                        changes: doc
                            .get_changes(&[])
                            .into_iter()
                            .map(|c| c.raw_bytes().to_vec())
                            .collect(),
                    },
                    web_time::SystemTime::now(),
                )
                .unwrap();
            owner
                .commit_mutation_candidate(candidate, owner.directory().to_vec())
                .unwrap();
        })
        .unwrap();
}
#[test]
fn native_private_disclosure_fence_actual_policy_aba_and_progress() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let expiry = committed.policy_clock().unwrap().now_ns().unwrap() / 1_000_000_000 + 200;
    publish_private_cap(&mut owner, &committed, expiry);
    let fence = committed
        .read()
        .unwrap()
        .view()
        .private_disclosure_fence()
        .unwrap();
    assert!(fence.check(committed.read().unwrap().view()).is_ok());
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("private-progress", b"progress", false)
        })
        .unwrap()
        .unwrap();
    assert!(
        fence.check(committed.read().unwrap().view()).is_ok(),
        "ordinary progress must preserve policy lineage"
    );
    let basis = qos_action_broker::peer_actions::PrivatePolicyBasis::from_peer(
        committed.read().unwrap().peer(),
    )
    .unwrap();
    publish_private_cap(&mut owner, &committed, expiry + 100);
    assert!(
        fence.check(committed.read().unwrap().view()).is_err(),
        "policy changed"
    );
    publish_private_cap(&mut owner, &committed, expiry);
    assert_eq!(
        basis,
        qos_action_broker::peer_actions::PrivatePolicyBasis::from_peer(
            committed.read().unwrap().peer()
        )
        .unwrap()
    );
    assert!(
        fence.check(committed.read().unwrap().view()).is_err(),
        "A to B to A must not revive old authorization"
    );
    let fresh = committed
        .read()
        .unwrap()
        .view()
        .private_disclosure_fence()
        .unwrap();
    let replacement = CommittedAuthority::new(&owner);
    assert!(
        fresh.check(replacement.read().unwrap().view()).is_err(),
        "distinct owner publication origin"
    );
}

#[cfg(feature = "test-support")]
#[test]
fn native_private_owner_clock_survives_publication_and_wall_rollback_until_expiry() {
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let (clock, control) = OwnerPolicyClock::for_test(100_000_000_000);
    let committed = CommittedAuthority::with_policy_clock(&owner, Ok(clock.clone()));
    publish_private_cap(&mut owner, &committed, 200);
    let fence = committed
        .read()
        .unwrap()
        .view()
        .private_disclosure_fence()
        .unwrap();
    let original = fence.policy_time().clone();
    control.set_wall_ns(50_000_000_000);
    control.advance_monotonic(Duration::from_secs(1));
    committed
        .with_owner(&mut owner, |owner| {
            owner.queue_pending("clock-progress", b"progress", false)
        })
        .unwrap()
        .unwrap();
    let guard = committed.read().unwrap();
    let fresh = guard.view().policy_time().unwrap();
    assert!(original.same_origin(&fresh));
    assert!(fresh.belongs_to(&clock));
    assert!(committed.policy_clock().unwrap().same_clock(&clock));
    assert_eq!(fence.check(guard.view()).unwrap(), 101_000_000_000);
    let unrelated_origin = clock.new_origin();
    assert!(
        guard
            .view()
            .check_policy_time(&unrelated_origin.capture().unwrap())
            .is_err(),
        "same clock alone must not mint the current owner's origin"
    );
    control.advance_monotonic(Duration::from_secs(99));
    assert_eq!(original.now_ns().unwrap(), 200_000_000_000);
    assert!(
        fence.check(guard.view()).is_err(),
        "expiry at the exact boundary despite wall rollback"
    );
}

#[test]
fn native_private_owner_clock_initialization_failure_denies_every_current_path() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::with_policy_clock(
        &owner,
        Err(qos_action_broker::peer_actions::PolicyClockError),
    );
    assert!(committed.policy_clock().is_err());
    assert!(committed.read().is_err());
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(committed.begin(&owner).is_err());
}

#[test]
fn native_private_owner_poison_retires_already_queued_time() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let retained = committed.read().unwrap().view().policy_time().unwrap();
    let other = committed.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _current = other.read().unwrap();
            panic!("current owner guard poisoned");
        }))
        .is_err()
    );
    assert!(
        retained.now_ns().is_err(),
        "poison must retire queued origin before another authority read"
    );
    assert!(matches!(committed.observe(), Observation::Denied));
}

#[test]
fn native_private_terminal_retirement_cannot_leave_recovery_time_live() {
    use std::sync::mpsc::sync_channel;
    let dir = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let original = committed.read().unwrap().view().policy_time().unwrap();
    enter_test_recovery(&committed);
    let update = committed.begin(&owner).unwrap();
    owner
        .queue_pending("clock-race-recovery", b"durable progress", false)
        .unwrap();
    let (retired, retirement) = sync_channel(0);
    let (resume, resumption) = sync_channel(0);
    *committed.0.after_time_retirement.lock().unwrap() = Some(Box::new(move || {
        retired.send(()).unwrap();
        resumption
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }));
    let escaped = std::thread::scope(|scope| {
        let other = committed.clone();
        let invalidation = scope.spawn(move || other.invalidate());
        retirement
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Old code exposes a fresh real Current during the terminal gap. The
        // fixed transition may instead reject recovery before publication.
        let escaped = if update.publish(&owner).is_ok() {
            Some(committed.read().unwrap().view().policy_time().unwrap())
        } else {
            None
        };
        resume.send(()).unwrap();
        invalidation.join().unwrap();
        escaped
    });
    assert!(matches!(committed.observe(), Observation::Denied));
    assert!(original.now_ns().is_err());
    if let Some(time) = escaped {
        assert!(
            time.now_ns().is_err(),
            "queued recovery effect context survived terminal owner denial"
        );
    }
}

#[test]
fn native_private_disclosure_fence_invalidation_wins_with_retained_current_guard() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SessionOwner::initialize(&dir.path().join("owner")).unwrap();
    let committed = CommittedAuthority::new(&owner);
    let current = committed.read().unwrap();
    let fence = current.view().private_disclosure_fence().unwrap();
    let retained_time = current.view().policy_time().unwrap();
    std::thread::scope(|scope| {
        let other = committed.clone();
        let invalidation = scope.spawn(move || other.invalidate());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !committed.0.terminal.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(retained_time.now_ns().is_err());
        assert!(fence.check(current.view()).is_err());
        assert!(current.view().private_disclosure_fence().is_err());
        drop(current);
        invalidation.join().unwrap();
    });
}
