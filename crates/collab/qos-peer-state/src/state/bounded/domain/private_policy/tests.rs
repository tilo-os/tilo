use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};
fn fixture(nonpersistent: bool) -> (StatePeer, SystemTime) {
    let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
    let mut room = peer.admitted_room().unwrap().clone();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    for (index, expiry) in [
        Expiry::AtUnix(8),
        Expiry::AtUnix(7),
        Expiry::AtUnix(7),
        if nonpersistent {
            Expiry::AfterUse
        } else {
            Expiry::AtUnix(3)
        },
    ]
    .into_iter()
    .enumerate()
    {
        let cap = CapOverride::grant(
            Subject::Role(collab_types::Role::Editor),
            Scope::Workspace,
            Verb::Read,
        )
        .expiring(expiry);
        room.put(
            &caps,
            index.to_string(),
            ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
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
fn restore(peer: &StatePeer, now: SystemTime, work: &mut PeerWork<'_>) -> BoundedStatePeer {
    BoundedStatePeer::restore(
        &peer.durable().unwrap(),
        peer.room(),
        peer.authority(),
        &[],
        &RevocationList::new(),
        now,
        work,
    )
    .unwrap()
}
#[test]
fn genuine_bounded_peer_private_basis_matches_exact_legacy_revision() {
    let (peer, now) = fixture(false);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let read = native.read(&work).unwrap();
    let basis = assert_precharged(&current, || read.private_policy_basis()).unwrap();
    assert_eq!(
        basis.revision(),
        crate::owner_commit::governance_revision(&peer).unwrap()
    );
    assert_eq!(basis.next_expiry_after(0), Some(3));
    assert_eq!(basis.next_expiry_after(3), Some(7));
    assert_eq!(basis.next_expiry_after(7), Some(8));
    assert_eq!(basis.next_expiry_after(8), None);
}
#[test]
fn private_basis_holds_current_credit_after_actual_peer_turn_and_registry_drop() {
    let (peer, now) = fixture(false);
    let resident = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let native = {
        let mut turn = registry
            .begin_turn(resident.clone(), PeerLimits::default())
            .unwrap();
        restore(&peer, now, &mut turn.work().unwrap())
    };
    let retained = resident.retained_bytes();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let baseline = current.retained_bytes();
    let basis = {
        let mut turn = registry
            .begin_turn(current.clone(), PeerLimits::default())
            .unwrap();
        let work = turn.work().unwrap();
        let read = native.read(&work).unwrap();
        let basis = assert_precharged(&current, || read.private_policy_basis()).unwrap();
        assert_eq!(resident.retained_bytes(), retained);
        let all = work.reserve_output(current.available_bytes()).unwrap();
        assert!(matches!(
            read.private_policy_basis(),
            Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
        ));
        drop(all);
        current.finish_turn();
        assert!(read.private_policy_basis().is_err());
        basis
    };
    drop(native);
    drop(registry);
    assert!(current.retained_bytes() > baseline);
    assert_eq!(basis.next_expiry_after(7), Some(8));
    drop(basis);
    assert_eq!(current.retained_bytes(), baseline);
}
#[test]
fn actual_private_basis_rejects_nonpersistent_governed_caps_and_foreign_domain() {
    let (peer, now) = fixture(true);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let read = native.read(&work).unwrap();
    assert!(matches!(
        read.private_policy_basis(),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))
    ));
    let mut other = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut other_turn = other
        .begin_turn(NativeAllowance::new(4096).unwrap(), PeerLimits::default())
        .unwrap();
    assert!(native.read(&other_turn.work().unwrap()).is_err());
}

#[test]
fn private_basis_uses_actual_signed_roster_and_verified_sorted_revocation_union() {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
    let (mut peer, now) = fixture(false);
    let root = SigningKey::from_bytes(&[1; 32]);
    let original = crate::owner_commit::governance_revision(&peer).unwrap();
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
            cert: Box::new(peer.authority().clone()),
            revocations: entries,
        },
        now,
    )
    .unwrap();
    let expected = crate::owner_commit::governance_revision(&peer).unwrap();
    assert_ne!(expected, original);
    assert_eq!(peer.certificates().len(), 4);
    assert_eq!(peer.revocations().len(), 2);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let native = restore(&peer, now, &mut work);
    let basis = assert_precharged(&current, || {
        native.read(&work).unwrap().private_policy_basis()
    })
    .unwrap();
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
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let (native, empty) = {
        let mut turn = registry
            .begin_turn(
                NativeAllowance::new(64 * 1024 * 1024).unwrap(),
                PeerLimits::default(),
            )
            .unwrap();
        let mut work = turn.work().unwrap();
        (
            restore(&source, now, &mut work),
            restore(&empty, now, &mut work),
        )
    };
    for decode in [false, true] {
        let current = NativeAllowance::new(8 * 1024 * 1024).unwrap();
        let mut limits = PeerLimits::default();
        if decode {
            limits.governance_json.allocation_bytes = 1;
        } else {
            limits.governance_collection_bytes = 1;
        }
        let mut turn = registry.begin_turn(current.clone(), limits).unwrap();
        let work = turn.work().unwrap();
        assert!(matches!(
            assert_precharged(&current, || empty
                .read(&work)
                .unwrap()
                .private_policy_basis()),
            Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))
        ));
        let baseline = current.retained_bytes();
        let result = assert_precharged(&current, || {
            native.read(&work).unwrap().private_policy_basis()
        });
        if decode {
            assert!(matches!(result, Err(BoundedPeerError::Decode(_))));
        } else {
            assert!(matches!(
                result,
                Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
            ));
        }
        assert_eq!(current.retained_bytes(), baseline);
    }
}
