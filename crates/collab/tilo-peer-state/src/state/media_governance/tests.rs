use super::*;
use crate::SeedState;
use crate::seed_room;
use automerge::AutoCommit;
use collab_types::CommitNamed as _;
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
};
use web_time::Duration;
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
fn fixture(revision: u64) -> (StatePeer, SigningKey, SigningKey, SystemTime) {
    let root = SigningKey::from_bytes(&[91; 32]);
    let key = SigningKey::from_bytes(&[92; 32]);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "native".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut peer = StatePeer::new(room, cert.clone(), vec![], RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(room, &cert, &[cert.clone()]).unwrap(),
            documents: vec![],
        },
        &key,
        now,
    )
    .unwrap();
    if revision == 0 {
        return (peer, root, key, now);
    }
    let mut doc = peer.admitted_room().unwrap().clone();
    let policy = RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(room).unwrap().as_bytes(),
        authorization_revision: revision,
        authorization_basis: media::security_basis(&doc, &[]).unwrap(),
        calls: BTreeMap::new(),
    };
    doc.put(
        ROOT,
        media::FIELD,
        automerge::ScalarValue::Bytes(media::encode(&policy).unwrap()),
    )
    .unwrap();
    let changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| c.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: room.into(),
            changes,
        },
        now,
    )
    .unwrap();
    (peer, root, key, now)
}
fn revoked(peer: &StatePeer, root: &SigningKey, now: SystemTime) -> StatePeer {
    let mut entry = RevocationEntry {
        user_public_key: peer.core.authority.user_public_key,
        revoked_device_public_key: [99; 32],
        revoked_at_ns: 2,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).unwrap();
    let mut candidate = peer.clone();
    candidate
        .receive_from(
            TransportOrigin::UntrustedPresence(peer.core.authority.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(peer.core.authority.clone()),
                revocations: vec![entry],
            },
            now,
        )
        .unwrap();
    candidate
}
#[test]
fn room_media_reconciliation_promotes_verified_union_once_and_does_not_change_epoch() {
    let (peer, root, key, now) = fixture(1);
    let mut candidate = revoked(&peer, &root, now);
    assert!(
        candidate
            .validate_media_governance_for_publication()
            .is_err()
    );
    assert!(candidate.reconcile_media_governance(&key, now).unwrap());
    assert_eq!(
        candidate
            .validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
    let bytes = candidate.durable().unwrap();
    assert!(!candidate.reconcile_media_governance(&key, now).unwrap());
    assert_eq!(candidate.durable().unwrap(), bytes);
    assert!(peer.verify_security_metadata_only(&candidate).is_err());
}
#[test]
fn room_media_exhaustion_preserves_only_sealed_verified_metadata_and_reopens_denied() {
    let (peer, root, key, now) = fixture(u64::MAX);
    let mut candidate = revoked(&peer, &root, now);
    let bytes = candidate.durable().unwrap();
    assert!(candidate.reconcile_media_governance(&key, now).is_err());
    assert_eq!(candidate.durable().unwrap(), bytes);
    let proof = peer.verify_security_metadata_only(&candidate).unwrap();
    proof.validate(&peer, &candidate).unwrap();
    let restored = StatePeer::restore(
        &bytes,
        peer.room(),
        peer.core.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    assert!(restored.validated_media_policy().is_err());
    assert_eq!(
        media::parse(restored.admitted_room().unwrap())
            .unwrap()
            .unwrap()
            .authorization_revision,
        u64::MAX
    );
    assert!(peer.verify_security_metadata_only(&peer).is_err());
    let mut changed = candidate.clone();
    changed.core.receipts.insert("unrelated".into(), vec![1]);
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    changed
        .core
        .room_doc
        .as_mut()
        .unwrap()
        .put(ROOT, "label", "smuggled")
        .unwrap();
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    changed.core.checkpoint_times.insert(
        Route {
            app: "x".into(),
            record: String::new(),
        },
        now,
    );
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    changed.core.revocations = RevocationList::new();
    assert!(proof.validate(&peer, &changed).is_err());
}
#[test]
fn room_media_combined_certificate_and_delegation_has_one_basis_revision() {
    let (mut peer, root, key, now) = fixture(1);
    let host = SigningKey::from_bytes(&[93; 32]);
    let mut cert = peer.core.authority.clone();
    cert.device_public_key = host.verifying_key().to_bytes();
    cert.device_label = "invitation host".into();
    sign_device_cert(&mut cert, &root).unwrap();
    let delegation = crate::join::sign_workspace_delegation(
        peer.room(),
        1,
        &peer.core.authority,
        cert.clone(),
        crate::join::WorkspaceDelegationPermissions {
            issue_invitations: true,
            admit_invited_members: true,
        },
        [1; 16],
        None,
        &key,
    )
    .unwrap();
    assert!(
        peer.admit_certificate_with_workspace_delegation(
            cert.clone(),
            delegation.clone(),
            &key,
            now
        )
        .unwrap()
    );
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
    assert!(
        !peer
            .admit_certificate_with_workspace_delegation(cert, delegation, &key, now)
            .unwrap()
    );
    assert_eq!(
        peer.validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
}

#[test]
fn room_media_reconciliation_preserves_disabled_policy_before_first_call() {
    let (peer, root, key, now) = fixture(0);
    let mut candidate = revoked(&peer, &root, now);
    assert!(candidate.reconcile_media_governance(&key, now).unwrap());
    assert!(candidate.validated_media_policy().unwrap().is_none());
    assert_eq!(
        media::security_basis(candidate.admitted_room().unwrap(), &[]).unwrap(),
        media::security_basis(candidate.admitted_room().unwrap(), &candidate.revocations())
            .unwrap()
    );
}

#[test]
fn room_media_security_projection_keeps_stored_state_and_binds_stored_proof() {
    let (stored, root, _, now) = fixture(u64::MAX);
    let mut live = stored.clone();
    live.core.receipts.insert("volatile-only".into(), vec![7]);
    let observed = revoked(&live, &root, now);
    live.verify_security_metadata_only(&observed).unwrap();
    let (projected, proof) = stored
        .project_verified_security_metadata(&observed)
        .unwrap();
    assert!(!projected.core.receipts.contains_key("volatile-only"));
    assert_eq!(
        projected.durable().unwrap(),
        revoked(&stored, &root, now).durable().unwrap()
    );
    proof.validate(&stored, &projected).unwrap();
    assert!(proof.validate(&live, &observed).is_err());
    assert!(projected.validated_media_policy().is_err());

    let mut wrong_room = observed.clone();
    wrong_room.core.room = "different".into();
    assert!(
        stored
            .project_verified_security_metadata(&wrong_room)
            .is_err()
    );
    let mut wrong_authority = observed;
    wrong_authority.core.authority.device_label = "different".into();
    assert!(
        stored
            .project_verified_security_metadata(&wrong_authority)
            .is_err()
    );
    assert!(stored.project_verified_security_metadata(&stored).is_err());
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn room_media_security_proof_survives_signed_multi_author_unsorted_roundtrip() {
    let (mut peer, root, key, now) = fixture(0);
    peer.select_admission_v5().unwrap();
    let mut chat = AutoCommit::new();
    chat.put(ROOT, "label", "signed native snapshot").unwrap();
    peer.publish_document("so.tilo.chat", "", chat.save(), &key, now)
        .unwrap();
    for seed in 110..118u8 {
        let author = SigningKey::from_bytes(&[seed; 32]);
        let mut cert = peer.core.authority.clone();
        cert.device_public_key = author.verifying_key().to_bytes();
        cert.device_label = format!("signed author {seed}");
        sign_device_cert(&mut cert, &root).unwrap();
        peer.admit_certificate(cert, &key, now).unwrap();
        let before = chat.get_heads();
        chat.put(ROOT, format!("author-{seed}"), u64::from(seed))
            .unwrap();
        chat.commit_edit("fixture", "fixture");
        peer.publish_incremental("so.tilo.chat", "", chat.save_after(&before), &author, now)
            .unwrap();
    }
    let policy = RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(peer.room()).unwrap().as_bytes(),
        authorization_revision: u64::MAX,
        authorization_basis: media::security_basis(peer.admitted_room().unwrap(), &[]).unwrap(),
        calls: BTreeMap::new(),
    };
    let mut room = peer.admitted_room().unwrap().clone();
    room.put(
        ROOT,
        media::FIELD,
        automerge::ScalarValue::Bytes(media::encode(&policy).unwrap()),
    )
    .unwrap();
    let changes = room
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    peer.receive_from(
        TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes,
        },
        now,
    )
    .unwrap();
    let exported = peer.durable().unwrap();
    let mut old: DurableV5<'_> = postcard::from_bytes(&exported).unwrap();
    let watermarks = &mut old.base.base.documents[0].watermarks;
    assert_eq!(
        watermarks.len(),
        9,
        "each real signed author must reach admission"
    );
    watermarks.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    assert!(watermarks.windows(2).all(|pair| pair[0].0 > pair[1].0));
    // Signed owner containers may hold any HashMap iteration order. This
    // deliberately descending representation must remain readable.
    let literal_old = postcard::to_allocvec(&old).unwrap();
    let restore = || {
        StatePeer::restore(
            &literal_old,
            peer.room(),
            peer.core.authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap()
    };
    let stored = restore();
    let (projected, proof) = stored
        .project_verified_security_metadata(&revoked(&stored, &root, now))
        .unwrap();
    // Require an actual different randomized map order, not a lucky single
    // restore whose bytes happen to equal the first export.
    let first = stored.durable().unwrap();
    let independently_restored = (0..32)
        .map(|_| restore())
        .find(|candidate| candidate.durable().unwrap() != first)
        .expect("independent multi-author maps must exercise a distinct serialized order");
    assert_eq!(
        stored
            .core
            .documents
            .values()
            .next()
            .unwrap()
            .channel
            .last_seq_per_author,
        independently_restored
            .core
            .documents
            .values()
            .next()
            .unwrap()
            .channel
            .last_seq_per_author
    );
    proof
        .validate(&independently_restored, &projected)
        .expect("proof must compare semantic watermark maps across stored-state restores");
    let mut tampered = projected.clone();
    *tampered
        .core
        .documents
        .values_mut()
        .next()
        .unwrap()
        .channel
        .last_seq_per_author
        .values_mut()
        .next()
        .unwrap() += 1;
    assert!(
        proof.validate(&stored, &tampered).is_err(),
        "normalization must not discard watermark values"
    );
    let mut tampered = projected;
    tampered
        .core
        .receipts
        .insert("not-security-metadata".into(), vec![1]);
    assert!(proof.validate(&stored, &tampered).is_err());
    assert!(
        StatePeer::restore(
            &literal_old,
            peer.room(),
            peer.core.authority.clone(),
            vec![],
            RevocationList::new(),
            now
        )
        .unwrap()
        .validated_media_policy()
        .is_ok()
    );
}

fn no_policy_document_fixture() -> (StatePeer, SigningKey, SigningKey, SystemTime) {
    let (mut peer, root, key, now) = fixture(0);
    peer.select_admission_v5().unwrap();
    let mut chat = AutoCommit::new();
    chat.put(ROOT, "label", "unchanged signed document")
        .unwrap();
    peer.publish_document("so.tilo.chat", "", chat.save(), &key, now)
        .unwrap();
    (peer, root, key, now)
}

#[test]
fn room_media_security_no_policy_accepts_host_revocation_without_enabling_media() {
    let (mut peer, root, key, now) = no_policy_document_fixture();
    let sibling_key = SigningKey::from_bytes(&[209; 32]);
    let mut sibling = peer.core.authority.clone();
    sibling.device_public_key = sibling_key.verifying_key().to_bytes();
    sibling.device_label = "same-root revocation carrier".into();
    sign_device_cert(&mut sibling, &root).unwrap();
    assert!(peer.admit_certificate(sibling.clone(), &key, now).unwrap());
    let before = peer.durable().unwrap();
    let mut entry = RevocationEntry {
        user_public_key: peer.core.authority.user_public_key,
        revoked_device_public_key: peer.core.authority.device_public_key,
        revoked_at_ns: 2,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    let mut observed = peer.clone();
    let error = observed
        .receive_from(
            TransportOrigin::UntrustedPresence(peer.core.authority.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(peer.core.authority.clone()),
                revocations: vec![entry.clone()],
            },
            now,
        )
        .unwrap_err();
    assert!(error.contains("device revoked"), "{error}");
    assert_eq!(observed.durable().unwrap(), before);
    observed
        .receive_from(
            TransportOrigin::UntrustedPresence(sibling.device_public_key),
            NeighbourMsg::IdentityHello {
                cert: Box::new(sibling),
                revocations: vec![entry],
            },
            now,
        )
        .unwrap();
    assert!(
        observed
            .core
            .revocations
            .is_revoked(&peer.core.authority.device_public_key)
    );
    let proof = peer.verify_security_metadata_only(&observed).unwrap();
    proof.validate(&peer, &observed).unwrap();
    let (projected, projected_proof) = peer.project_verified_security_metadata(&observed).unwrap();
    projected_proof.validate(&peer, &projected).unwrap();
    assert_eq!(projected.durable().unwrap(), observed.durable().unwrap());
    assert!(
        media::parse(projected.admitted_room().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(peer.durable().unwrap(), before);
    let restore_error = StatePeer::restore(
        &projected.durable().unwrap(),
        peer.room(),
        peer.core.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .err()
    .expect("a persisted host revocation must deny reopen");
    assert!(
        restore_error.contains("pinned authority device is revoked"),
        "{restore_error}"
    );
}

#[test]
fn room_media_security_no_policy_rejects_wrong_root_and_unrelated_mutations() {
    let (peer, root, key, now) = no_policy_document_fixture();
    let candidate = revoked(&peer, &root, now);
    let proof = peer.verify_security_metadata_only(&candidate).unwrap();
    assert!(peer.verify_security_metadata_only(&peer).is_err());
    assert!(peer.project_verified_security_metadata(&peer).is_err());
    let foreign_root = SigningKey::from_bytes(&[210; 32]);
    let mut foreign = RevocationEntry {
        user_public_key: foreign_root.verifying_key().to_bytes(),
        revoked_device_public_key: peer.core.authority.device_public_key,
        revoked_at_ns: 3,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut foreign, &foreign_root).unwrap();
    let mut wrong_root = peer.clone();
    wrong_root.core.revocations.insert(foreign.clone()).unwrap();
    assert!(peer.verify_security_metadata_only(&wrong_root).is_err());
    assert!(
        peer.project_verified_security_metadata(&wrong_root)
            .is_err()
    );
    let mut received = peer.clone();
    assert!(
        received
            .receive_from(
                TransportOrigin::UntrustedPresence(peer.core.authority.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(peer.core.authority.clone()),
                    revocations: vec![foreign],
                },
                now,
            )
            .is_err()
    );
    assert_eq!(received.durable().unwrap(), peer.durable().unwrap());
    let mut invalid_signature = candidate.core.revocations.iter().next().unwrap().clone();
    invalid_signature.signature[0] ^= 1;
    assert!(
        received
            .receive_from(
                TransportOrigin::UntrustedPresence(peer.core.authority.device_public_key),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(peer.core.authority.clone()),
                    revocations: vec![invalid_signature],
                },
                now,
            )
            .is_err()
    );
    assert_eq!(received.durable().unwrap(), peer.durable().unwrap());
    let mut changed = candidate.clone();
    changed
        .core
        .room_doc
        .as_mut()
        .unwrap()
        .put(ROOT, "label", "smuggled")
        .unwrap();
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    changed.core.receipts.insert("unrelated".into(), vec![1]);
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    let mut chat = AutoCommit::load(&changed.document("so.tilo.chat", "").unwrap()).unwrap();
    chat.put(ROOT, "label", "unrelated signed edit").unwrap();
    changed
        .publish_document("so.tilo.chat", "", chat.save(), &key, now)
        .unwrap();
    assert!(proof.validate(&peer, &changed).is_err());
    let route = Route {
        app: "so.tilo.chat".into(),
        record: String::new(),
    };
    let mut changed = candidate.clone();
    changed
        .core
        .checkpoint_times
        .insert(route.clone(), now + Duration::from_secs(1));
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate.clone();
    changed
        .core
        .documents
        .get_mut(&route)
        .unwrap()
        .channel
        .max_in_memory_ops += 1;
    assert!(proof.validate(&peer, &changed).is_err());
    let mut changed = candidate;
    changed.core.revocations = RevocationList::new();
    assert!(proof.validate(&peer, &changed).is_err());
}

#[test]
fn room_media_security_no_policy_requires_an_effective_security_basis_change() {
    let (peer, root, key, now) = fixture(0);
    let mut governed = revoked(&peer, &root, now);
    assert!(governed.reconcile_media_governance(&key, now).unwrap());
    // The signed room already carries this revocation. Adding the same entry
    // only to its durable sidecar is not a new effective security change.
    governed.core.revocations = RevocationList::new();
    let duplicate = revoked(&governed, &root, now);
    assert_eq!(
        media::security_basis(governed.admitted_room().unwrap(), &governed.revocations()).unwrap(),
        media::security_basis(duplicate.admitted_room().unwrap(), &duplicate.revocations())
            .unwrap()
    );
    assert_eq!(
        governed
            .verify_security_metadata_only(&duplicate)
            .unwrap_err(),
        "security metadata does not change the verified security basis"
    );
    assert!(
        governed
            .project_verified_security_metadata(&duplicate)
            .is_err()
    );
}
