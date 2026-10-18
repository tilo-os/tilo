use super::*;
use crate::BoundedPeerInvalid;
use crate::NeighbourMsg;
use crate::PeerLimits;
use crate::StatePeer;
use crate::TransportOrigin;
use crate::state::backend::restore::tests::restore_bounded;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Role, Subject, TreePath};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;
fn fixture(nonpersistent: bool) -> (StatePeer, SystemTime) {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let caps = room.get(ROOT, "grants").unwrap().unwrap().1;
    for (index, lifetime) in [
        Lifetime::Until(8_000),
        Lifetime::Until(7_000),
        Lifetime::Until(7_000),
        if nonpersistent {
            Lifetime::Once
        } else {
            Lifetime::Until(3_000)
        },
    ]
    .into_iter()
    .enumerate()
    {
        let grant = Grant::new(
            "editor-reads",
            Subject::Role(Role::Editor),
            TreePath::room(peer.room()),
            KindSet::READ,
            Answer::Allow,
            Subject::Role(Role::Owner),
        )
        .with_lifetime(lifetime);
        room.put(
            &caps,
            index.to_string(),
            ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
        )
        .unwrap();
    }
    let changes = room
        .get_changes(&[])
        .iter()
        .map(|c| c.raw_bytes().to_vec())
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
    (peer, now)
}
#[test]
fn genuine_bounded_peer_private_basis_matches_exact_legacy_revision() {
    let (peer, now) = fixture(false);
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let basis = native.read().private_policy_basis().unwrap();
    assert_eq!(
        basis.revision(),
        crate::legacy_policy::governance_revision(&peer).unwrap()
    );
    assert_eq!(basis.next_expiry_after(0), Some(3));
    assert_eq!(basis.next_expiry_after(3), Some(7));
    assert_eq!(basis.next_expiry_after(7), Some(8));
    assert_eq!(basis.next_expiry_after(8), None);
}
#[test]
fn actual_private_basis_rejects_nonpersistent_governed_caps() {
    let (peer, now) = fixture(true);
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    assert!(matches!(
        native.read().private_policy_basis(),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))
    ));
}

#[test]
fn private_basis_uses_actual_signed_roster_and_verified_sorted_revocation_union() {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (mut peer, now) = fixture(false);
    let root = SigningKey::from_bytes(&[1; 32]);
    let original = crate::legacy_policy::governance_revision(&peer).unwrap();
    let mut enrollment = Vec::new();
    for byte in [92, 17] {
        let key = SigningKey::from_bytes(&[byte; 32]);
        let mut cert = peer.authority().clone();
        cert.device_public_key = key.verifying_key().to_bytes();
        cert.device_label = format!("verified added cert {byte}: {}", "label".repeat(512));
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        enrollment.push(cert);
    }
    peer = StatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority().clone(),
        enrollment,
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut entries = Vec::new();
    for byte in [88, 9] {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [byte; 32],
            revoked_at_ns: 1,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        entries.push(entry);
    }
    peer.receive_from(
        TransportOrigin::UntrustedPresence(peer.authority().device_public_key),
        NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(peer.authority().clone()),
            revocations: entries,
        },
        now,
    )
    .unwrap();
    let expected = crate::legacy_policy::governance_revision(&peer).unwrap();
    assert_ne!(expected, original);
    assert_eq!(peer.certificates().len(), 4);
    assert_eq!(peer.revocations().len(), 2);
    let native = restore_bounded(&peer, now, &PeerLimits::default());
    let basis = native.read().private_policy_basis().unwrap();
    assert_eq!(basis.revision(), expected);
}

#[test]
fn private_basis_missing_room_and_closed_decode_limits_preserve_typed_refusals() {
    let (source, now) = fixture(false);
    let empty = StatePeer::new(
        source.room(),
        source.authority().clone(),
        source.certificates(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    for decode in [false, true] {
        let mut limits = PeerLimits::default();
        if decode {
            limits.governance_json.allocation_bytes = 1;
        } else {
            limits.governance_collection_bytes = 1;
        }
        let empty = restore_bounded(&empty, now, &limits);
        assert!(matches!(
            empty.read().private_policy_basis(),
            Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))
        ));
        let native = restore_bounded(&source, now, &limits);
        let result = native.read().private_policy_basis();
        if decode {
            assert!(matches!(result, Err(BoundedPeerError::Decode(_))));
        } else {
            assert!(matches!(result, Err(BoundedPeerError::Capacity)));
        }
    }
}
