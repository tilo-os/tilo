use super::*;
use crate::BoundedStatePeer;
use crate::PeerLimits;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{RevocationReason, sign_revocation_entry};
fn signed(key: [u8; 32], time: u64, root: &SigningKey) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: key,
        revoked_at_ns: time,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).unwrap();
    entry
}
#[test]
fn retained_enrollment_revocations_matches_verified_union_and_outlives_inputs() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[7; 32]);
    let mut entries = (0u8..80)
        .map(|i| signed([i; 32], 20, if i % 2 == 0 { &root } else { &other }))
        .collect::<Vec<_>>();
    entries.push(signed([12; 32], 9, &root));
    entries.push(signed([12; 32], 30, &root));
    let mut expected = RevocationList::new();
    for entry in &entries {
        expected.insert(entry.clone()).unwrap();
    }
    let retained = PeerRevocations::verified(&entries).unwrap();
    assert_eq!(retained.list().len(), expected.len());
    for row in expected.iter() {
        assert_eq!(
            retained
                .list()
                .iter()
                .find(|entry| entry.revoked_device_public_key == row.revoked_device_public_key),
            Some(row)
        );
    }
    drop(entries);
    drop(expected);
    assert_eq!(retained.list().len(), 80);
}
#[test]
fn retained_enrollment_revocations_checks_duplicate_signatures() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let row = signed([33; 32], 1, &root);
    let mut bad = row.clone();
    bad.signature[0] ^= 1;
    assert!(matches!(
        PeerRevocations::verified(&[row.clone(), bad]),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Revocation))
    ));
    let holder = PeerRevocations::verified(std::slice::from_ref(&row)).unwrap();
    assert_eq!(holder.list().len(), 1);
}

#[test]
fn retained_enrollment_revocations_does_not_bypass_actual_restore_root_or_authority_checks() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let bytes = peer.durable().unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[7; 32]);
    let restore = |revocations: &RevocationList| {
        BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            revocations,
            now,
            &PeerLimits::default(),
        )
    };
    for (entry, expected) in [
        (
            signed(member.device_public_key, 1, &other),
            BoundedPeerInvalid::Revocation,
        ),
        (
            signed(peer.authority().device_public_key, 1, &root),
            BoundedPeerInvalid::AuthorityRevoked,
        ),
    ] {
        let holder = PeerRevocations::verified(&[entry]).unwrap();
        assert!(
            matches!(restore(holder.list()), Err(BoundedPeerError::Invalid(actual)) if actual == expected)
        );
    }
    let valid = PeerRevocations::verified(&[signed([99; 32], 1, &other)]).unwrap();
    let restored = restore(valid.list()).unwrap();
    assert!(restored.core.revocations.list().is_revoked(&[99; 32]));
    assert!(
        restored
            .read()
            .member(member.device_public_key, now)
            .is_ok()
    );
}
#[test]
fn retained_enrollment_revocations_empty_union_is_empty() {
    let empty = PeerRevocations::verified(&[]).unwrap();
    assert!(empty.list().is_empty());
}
