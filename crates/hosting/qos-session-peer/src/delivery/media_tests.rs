use super::*;
use crate::committed_authority::CommittedAuthority;

fn fixture() -> (
    tempfile::TempDir,
    SessionOwner,
    DeviceCert,
    tilo_user_identity::RevocationEntry,
) {
    let (temp, mut owner) = super::tests::hosted_owner();
    let (root, _, cert) = crate::room_media_policy::tests::member(
        &mut owner,
        qos_peer_state::join::MemberRole::Editor,
        None,
    );
    crate::room_media_policy::tests::activate(&mut owner);
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: [88; 32],
        revoked_at_ns: 2,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    (temp, owner, cert, entry)
}

fn revoked_candidate(
    owner: &SessionOwner,
    cert: DeviceCert,
    entry: tilo_user_identity::RevocationEntry,
) -> StatePeer {
    let mut candidate = owner.peer.clone();
    candidate
        .receive_from(
            TransportOrigin::UntrustedPresence(cert.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(cert),
                revocations: vec![entry],
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(
        candidate
            .validate_media_governance_for_publication()
            .is_err()
    );
    candidate
}

#[test]
fn media_writer_final_normal_gate_rejects_unreconciled_candidate_before_barrier() {
    let (_temp, mut owner, cert, entry) = fixture();
    let candidate = revoked_candidate(&owner, cert, entry);
    let before = std::fs::read(owner.root().join("session.bin")).unwrap();
    let generation = owner.status().generation;
    assert!(
        owner.commit(owner.aggregate.clone(), candidate).is_err(),
        "normal commit must reject unprepared media security metadata"
    );
    assert_eq!(owner.status().generation, generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before
    );
    assert!(owner.peer.validated_media_policy().is_ok());
}

#[test]
fn media_writer_suspended_owner_denies_delivery_source_without_denying_data_view() {
    let (_temp, mut owner, cert, entry) = fixture();
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let committed = CommittedAuthority::new(&owner);
    let session = super::tests::authenticated_session(&owner, &committed);
    {
        let current = committed.read().unwrap();
        owner
            .delivery_source(current.view(), &session, super::tests::now_ns())
            .unwrap();
    }
    committed
        .with_owner(&mut owner, |owner| {
            owner.receive(
                TransportOrigin::UntrustedPresence(cert.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: vec![entry],
                },
            )
        })
        .unwrap()
        .unwrap();
    let current = committed.read().expect("healthy data remains Current");
    assert!(
        current
            .peer()
            .permissions(owner.peer.authority().device_public_key, SystemTime::now())
            .is_ok()
    );
    assert!(
        owner
            .delivery_source(current.view(), &session, super::tests::now_ns())
            .is_err(),
        "suspended basis cannot mint a delivery source or cursor/replay authority"
    );
}

#[test]
fn media_writer_sealed_mode_rejects_changed_aggregate_and_reused_candidate_proof() {
    let (_temp, mut owner, cert, entry) = fixture();
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let candidate = revoked_candidate(&owner, cert, entry);
    let before = std::fs::read(owner.root().join("session.bin")).unwrap();
    let generation = owner.status().generation;
    let (persisted, proof) = owner
        .committed_peer()
        .unwrap()
        .project_verified_security_metadata(&candidate)
        .unwrap();
    let mut extra_metadata = owner.aggregate.clone();
    extra_metadata.pending.push(PendingOperation {
        invocation_id: "cannot-smuggle-pending-effect".into(),
        encoded_request: vec![1],
        outcome_unknown: false,
    });
    let error = owner
        .commit_delivery(
            extra_metadata,
            persisted,
            None,
            CommitMode::VerifiedSecurityMetadataOnly {
                stored_peer: owner.committed_peer_identity(),
                proof,
            },
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("changed owner aggregate"),
        "{error}"
    );

    let proof = owner
        .peer
        .verify_security_metadata_only(&candidate)
        .unwrap();
    let mut changed_candidate = candidate;
    // Native publication changes receipts/admission state, so the earlier
    // exact metadata proof must no longer authorize this candidate.
    changed_candidate
        .durable_bootstrap_messages(&owner.identity.native, SystemTime::now(), generation + 1)
        .unwrap();
    assert!(
        owner
            .commit_verified_security_metadata_only(changed_candidate, proof)
            .is_err()
    );
    assert_eq!(owner.status().generation, generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before
    );
}

#[test]
fn media_writer_restored_revocation_reconciles_only_in_new_explicit_checkpoint() {
    let (_temp, mut owner, cert, entry) = fixture();
    let prior = owner.peer.validated_media_policy().unwrap().unwrap();
    let candidate = revoked_candidate(&owner, cert, entry);
    let generation = owner.status().generation;
    let store = owner.delivery_ledger().unwrap().store_generation;
    // A real signed imported snapshot with extra verified local knowledge.
    // Ordinary open must preserve this snapshot and its denied media projection.
    let mut imported = owner.aggregate.clone();
    imported.peer = candidate.durable().unwrap();
    let bytes = encode(&imported, &owner.identity).unwrap();
    owner.store.commit(&bytes).unwrap();
    let path = owner.root().to_owned();
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert!(owner.peer.validated_media_policy().is_err());
    assert_eq!(owner.status().generation, generation);
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), bytes);
    owner.refresh_checkpoint().unwrap();
    let restored = owner.peer.validated_media_policy().unwrap().unwrap();
    assert_eq!(
        restored.authorization_revision,
        prior.authorization_revision + 1
    );
    assert_eq!(restored.calls, prior.calls);
    assert_eq!(owner.status().generation, generation + 1);
    assert_eq!(owner.delivery_ledger().unwrap().store_generation, store);
    assert!(
        owner
            .delivery_ledger()
            .unwrap()
            .replay
            .contains_key(&(generation + 1))
    );
}

#[test]
fn media_writer_host_certificate_and_delegation_advance_basis_once() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    crate::room_media_policy::tests::activate(&mut owner);
    let before = owner.peer.validated_media_policy().unwrap().unwrap();
    let (candidate, _) = owner
        .prepare_workspace_host_delegation(SystemTime::now())
        .unwrap();
    owner
        .commit_admission(candidate, owner.directory().to_vec())
        .unwrap();
    let after = owner.peer.validated_media_policy().unwrap().unwrap();
    assert_eq!(
        after.authorization_revision,
        before.authorization_revision + 1
    );
    assert_eq!(after.calls, before.calls);
    let (duplicate, _) = owner
        .prepare_workspace_host_delegation(SystemTime::now())
        .unwrap();
    assert_eq!(duplicate.validated_media_policy().unwrap().unwrap(), after);
}

#[test]
fn media_writer_delivery_security_metadata_has_no_accepted_wrapper_or_pool_change() {
    let (_temp, mut owner, cert, entry) = fixture();
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let generation = owner.status().generation;
    let original = owner.aggregate.clone();
    let source = TransportOrigin::RelayDelta(cert.device_public_key);
    let bytes = postcard::to_stdvec(&NeighbourMsg::IdentityHello {
        cert: Box::new(cert),
        revocations: vec![entry],
    })
    .unwrap();
    let outcome = owner
        .receive_delivery(
            source,
            CompleteStateItem {
                delivery_id: [39; 16],
                canonical: &bytes,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(outcome, AdmissionOutcome::MetadataOnly));
    assert!(owner.peer.validated_media_policy().is_err());
    assert_eq!(owner.status().generation, generation + 1);
    let mut normalized = owner.aggregate.clone();
    normalized.generation = original.generation;
    normalized.peer = original.peer.clone();
    if let PublicationStorage::V5 { delivery, .. } = &mut normalized.publication {
        delivery.resolved_through = generation;
    }
    assert_eq!(
        normalized.encode_payload().unwrap(),
        original.encode_payload().unwrap(),
        "only verified peer metadata, generation and resolved gap may change"
    );
}

fn deferred_then_suspended_uses_stored_basis(delivery: bool) {
    let (_temp, mut owner, cert, entry) = fixture();
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let before_file = std::fs::read(owner.root().join("session.bin")).unwrap();
    let (before_aggregate, before_signed) = decode(&before_file).unwrap();
    let (before_peer, _) = restore(&before_aggregate, &before_signed, &owner.identity).unwrap();
    assert_eq!(before_peer.pending_count(), 0);
    let generation = owner.status().generation;
    let mut author = owner.peer.clone();
    let bytes = super::tests::upload(&owner, &mut author, "unknown-certificate-deferred");
    let mut message: NeighbourMsg = postcard::from_bytes(&bytes).unwrap();
    let unknown = ed25519_dalek::SigningKey::from_bytes(&[47; 32]);
    let device = unknown.verifying_key().to_bytes();
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut message {
        *peer = qos_peer_state::peer_handle(&device);
        envelope.author = collab_types::Ed25519PublicKey::from_bytes(device);
        collab_types::sign_op(envelope, &unknown).unwrap();
    } else {
        panic!("expected genuine document envelope");
    }
    let committed = CommittedAuthority::new(&owner);
    committed
        .with_owner(&mut owner, |owner| {
            if delivery {
                let bytes = postcard::to_stdvec(&message)?;
                let outcome = owner.receive_delivery(
                    TransportOrigin::RelayDelta(device),
                    CompleteStateItem {
                        delivery_id: [51; 16],
                        canonical: &bytes,
                    },
                    SystemTime::now(),
                )?;
                assert!(matches!(
                    outcome,
                    AdmissionOutcome::RetryLater(MissingPrerequisite::Certificate)
                ));
            } else {
                assert!(
                    owner
                        .receive(TransportOrigin::RelayDelta(device), message)?
                        .is_none()
                );
            }
            Ok::<_, anyhow::Error>(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        owner.peer.pending_count(),
        1,
        "native bounded queue retains unadmitted input"
    );
    assert_eq!(owner.status().generation, generation);
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        before_file
    );
    assert_eq!(
        CommittedAuthority::new(&owner)
            .read()
            .unwrap()
            .peer()
            .pending_count(),
        0,
        "fresh authority capture excludes live-only deferred custody"
    );
    let hello = NeighbourMsg::IdentityHello {
        cert: Box::new(cert.clone()),
        revocations: vec![entry],
    };
    committed
        .with_owner(&mut owner, |owner| {
            if delivery {
                let bytes = postcard::to_stdvec(&hello)?;
                let outcome = owner.receive_delivery(
                    TransportOrigin::RelayDelta(cert.device_public_key),
                    CompleteStateItem {
                        delivery_id: [52; 16],
                        canonical: &bytes,
                    },
                    SystemTime::now(),
                )?;
                assert!(matches!(outcome, AdmissionOutcome::MetadataOnly));
            } else {
                owner.receive(
                    TransportOrigin::UntrustedPresence(cert.device_public_key),
                    hello,
                )?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    assert_eq!(
        owner.peer.pending_count(),
        1,
        "unadmitted queue remains volatile custody"
    );
    let stored_file = std::fs::read(owner.root().join("session.bin")).unwrap();
    let (stored, signed) = decode(&stored_file).unwrap();
    let (stored_peer, _) = restore(&stored, &signed, &owner.identity).unwrap();
    assert_eq!(
        stored_peer.pending_count(),
        0,
        "security-only barrier must not persist earlier volatile deferred input"
    );
    before_peer
        .verify_security_metadata_only(&stored_peer)
        .expect("every persisted peer field except verified union is unchanged");
    let mut normalized = stored.clone();
    normalized.peer = before_aggregate.peer.clone();
    normalized.generation = before_aggregate.generation;
    if let PublicationStorage::V5 { delivery, .. } = &mut normalized.publication {
        delivery.resolved_through = generation;
    }
    assert_eq!(
        normalized.encode_payload().unwrap(),
        before_aggregate.encode_payload().unwrap()
    );
    {
        let current = committed.read().unwrap();
        assert!(current.peer().validated_media_policy().is_err());
        assert_eq!(
            current.peer().pending_count(),
            0,
            "new-generation authority is exactly the stored peer"
        );
        assert_eq!(
            current.view().committed_peer_identity(),
            *blake3::hash(&stored.peer).as_bytes()
        );
    }
    let path = owner.root().to_owned();
    drop(owner);
    let reopened = SessionOwner::open(&path).unwrap();
    assert!(reopened.peer.validated_media_policy().is_err());
    assert_eq!(reopened.peer.pending_count(), 0);
    assert_eq!(
        std::fs::read(path.join("session.bin")).unwrap(),
        stored_file
    );
}

#[test]
fn media_writer_native_deferred_input_stays_volatile_during_security_only_commit() {
    deferred_then_suspended_uses_stored_basis(false);
}

#[test]
fn media_writer_delivery_deferred_input_stays_volatile_during_security_only_commit() {
    deferred_then_suspended_uses_stored_basis(true);
}

#[test]
fn media_writer_committed_capture_restore_failure_denies_instead_of_using_live_peer() {
    use crate::committed_authority::Observation;
    let (_temp, mut owner, _, _) = fixture();
    let committed = CommittedAuthority::new(&owner);
    assert!(matches!(committed.observe(), Observation::Current(_)));
    let file = std::fs::read(owner.root().join("session.bin")).unwrap();
    // Inject an unavailable stored projection while leaving the live peer valid.
    owner.aggregate.peer = vec![255];
    assert!(matches!(
        CommittedAuthority::new(&owner).observe(),
        Observation::Denied
    ));
    let result = committed.with_owner(&mut owner, |owner| {
        owner.aggregate.generation += 1;
        Ok::<_, anyhow::Error>(())
    });
    assert!(result.is_err());
    assert!(matches!(committed.observe(), Observation::Denied));
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        file
    );
}

fn multi_author_fixture() -> (
    tempfile::TempDir,
    SessionOwner,
    DeviceCert,
    tilo_user_identity::RevocationEntry,
) {
    use ui::automerge::{ROOT, transaction::Transactable};
    let (temp, mut owner, member, entry) = fixture();
    for byte in [31, 32, 33] {
        let key = ed25519_dalek::SigningKey::from_bytes(&[byte; 32]);
        let mut certificate = owner.identity.native_cert.clone();
        certificate.device_public_key = key.verifying_key().to_bytes();
        certificate.device_label = format!("multi-author-{byte}");
        tilo_user_identity::sign_device_cert(&mut certificate, &owner.identity.root).unwrap();
        let mut candidate = owner.peer.clone();
        candidate
            .admit_certificate(certificate, &owner.identity.native, SystemTime::now())
            .unwrap();
        owner
            .commit_admission(candidate, owner.directory().to_vec())
            .unwrap();
        let mut author = owner.peer.clone();
        let mut document =
            ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
        let before = document.get_heads();
        document
            .put(ROOT, format!("author-{byte}"), byte as u64)
            .unwrap();
        document.commit();
        let message = author
            .publish_incremental(
                "so.tilo.chat",
                "",
                document.save_after(&before),
                &key,
                SystemTime::now(),
            )
            .unwrap();
        owner
            .receive(
                TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
                message,
            )
            .unwrap()
            .unwrap();
    }
    assert!(
        owner
            .peer
            .replay_floors()
            .iter()
            .filter(|(app, record, _, _)| app == "so.tilo.chat" && record.is_empty())
            .count()
            >= 4
    );
    (temp, owner, member, entry)
}

#[test]
fn media_writer_multiauthor_restore_keeps_delivery_and_suspension_valid() {
    let (_temp, mut owner, member, entry) = multi_author_fixture();
    let original = std::fs::read(owner.root().join("session.bin")).unwrap();
    let initial = CommittedAuthority::new(&owner);
    let session = super::tests::authenticated_session(&owner, &initial);
    // Each capture restores independently randomized native watermark maps.
    for _ in 0..32 {
        let authority = CommittedAuthority::new(&owner);
        let current = authority.read().unwrap();
        owner
            .delivery_source(current.view(), &session, super::tests::now_ns())
            .expect("same stored multi-author peer remains the current delivery source");
    }
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        original
    );
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let generation = owner.status().generation;
    owner
        .receive(
            TransportOrigin::UntrustedPresence(member.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(member),
                revocations: vec![entry],
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(owner.status().generation, generation + 1);
    let stored = owner.aggregate.clone();
    assert!(owner.peer.validated_media_policy().is_err());
    let path = owner.root().to_owned();
    drop(owner);
    for _ in 0..8 {
        let owner = SessionOwner::open(&path).unwrap();
        assert!(owner.peer.validated_media_policy().is_err());
        assert_eq!(owner.aggregate.peer, stored.peer);
        assert_eq!(
            owner.aggregate.encode_payload().unwrap(),
            stored.encode_payload().unwrap()
        );
    }
}

#[test]
fn media_writer_multiauthor_changed_raw_baseline_cannot_reuse_security_proof() {
    let (_temp, mut owner, member, entry) = multi_author_fixture();
    crate::room_media_policy::tests::edit_policy(&mut owner, |policy| {
        policy.authorization_revision = u64::MAX
    });
    let candidate = revoked_candidate(&owner, member, entry);
    let committed = owner.committed_peer().unwrap();
    let (persisted, proof) = committed
        .project_verified_security_metadata(&candidate)
        .unwrap();
    let raw_before = *blake3::hash(&owner.aggregate.peer).as_bytes();
    let replacement = (0..128)
        .find_map(|_| {
            let bytes = owner.committed_peer().unwrap().durable().unwrap();
            (bytes != owner.aggregate.peer).then_some(bytes)
        })
        .expect("multi-author restore exposes an equivalent alternate raw watermark order");
    owner.aggregate.peer = replacement;
    let file = encode(&owner.aggregate, &owner.identity).unwrap();
    owner.store.commit(&file).unwrap();
    let result = owner.commit_delivery(
        owner.aggregate.clone(),
        persisted,
        None,
        CommitMode::VerifiedSecurityMetadataOnly {
            stored_peer: raw_before,
            proof,
        },
    );
    assert!(
        result.is_err(),
        "equivalent but different stored bytes must invalidate the prior raw-baseline proof"
    );
    assert_eq!(
        std::fs::read(owner.root().join("session.bin")).unwrap(),
        file
    );
}
