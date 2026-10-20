use super::*;
use crate::SeedState;
use crate::seed_room;
use automerge::AutoCommit;
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
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(peer.core.authority.clone()),
                revocations: vec![entry],
                successions: Vec::new(),
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
            .media()
            .validate_media_governance_for_publication()
            .is_err()
    );
    assert!(
        candidate
            .media_mut()
            .reconcile_media_governance(&key, now)
            .unwrap()
    );
    assert_eq!(
        candidate
            .validated_media_policy()
            .unwrap()
            .unwrap()
            .authorization_revision,
        2
    );
    let bytes = candidate.durable().unwrap();
    assert!(
        !candidate
            .media_mut()
            .reconcile_media_governance(&key, now)
            .unwrap()
    );
    assert_eq!(candidate.durable().unwrap(), bytes);
}
#[test]
fn room_media_exhaustion_keeps_the_record_and_reopens_denied() {
    let (peer, root, key, now) = fixture(u64::MAX);
    let mut candidate = revoked(&peer, &root, now);
    let bytes = candidate.durable().unwrap();
    assert!(
        candidate
            .media_mut()
            .reconcile_media_governance(&key, now)
            .is_err()
    );
    assert_eq!(candidate.durable().unwrap(), bytes);
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
        peer.governance_mut()
            .admit_certificate_with_workspace_delegation(
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
            .governance_mut()
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
    assert!(
        candidate
            .media_mut()
            .reconcile_media_governance(&key, now)
            .unwrap()
    );
    assert!(candidate.validated_media_policy().unwrap().is_none());
    assert_eq!(
        media::security_basis(candidate.admitted_room().unwrap(), &[]).unwrap(),
        media::security_basis(candidate.admitted_room().unwrap(), &candidate.revocations())
            .unwrap()
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
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(peer.core.authority.clone()),
                revocations: vec![entry.clone()],
                successions: Vec::new(),
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
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(sibling),
                revocations: vec![entry],
                successions: Vec::new(),
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
    assert!(
        media::parse(observed.admitted_room().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(peer.durable().unwrap(), before);
    let restore_error = StatePeer::restore(
        &observed.durable().unwrap(),
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
fn room_media_security_no_policy_refuses_foreign_and_forged_revocations() {
    let (peer, root, _, now) = no_policy_document_fixture();
    let candidate = revoked(&peer, &root, now);
    let foreign_root = SigningKey::from_bytes(&[210; 32]);
    let mut foreign = RevocationEntry {
        user_public_key: foreign_root.verifying_key().to_bytes(),
        revoked_device_public_key: peer.core.authority.device_public_key,
        revoked_at_ns: 3,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut foreign, &foreign_root).unwrap();
    let mut received = peer.clone();
    assert!(
        received
            .receive_from(
                TransportOrigin::UntrustedPresence(peer.core.authority.device_public_key),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(peer.core.authority.clone()),
                    revocations: vec![foreign],
                    successions: Vec::new(),
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
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(peer.core.authority.clone()),
                    revocations: vec![invalid_signature],
                    successions: Vec::new(),
                },
                now,
            )
            .is_err()
    );
    assert_eq!(received.durable().unwrap(), peer.durable().unwrap());
}

#[test]
fn room_media_security_basis_ignores_a_revocation_the_room_already_carries() {
    let (peer, root, key, now) = fixture(0);
    let mut governed = revoked(&peer, &root, now);
    assert!(
        governed
            .media_mut()
            .reconcile_media_governance(&key, now)
            .unwrap()
    );
    // The signed room already carries this revocation. Adding the same entry
    // only to its durable sidecar is not a new effective security change.
    governed.core.revocations = RevocationList::new();
    let duplicate = revoked(&governed, &root, now);
    assert_eq!(
        media::security_basis(governed.admitted_room().unwrap(), &governed.revocations()).unwrap(),
        media::security_basis(duplicate.admitted_room().unwrap(), &duplicate.revocations())
            .unwrap()
    );
}
