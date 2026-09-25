use super::*;
use crate::state::now_ns;
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
fn roster(
    peer: &crate::StatePeer,
    now: std::time::SystemTime,
) -> super::super::roster::BoundedRoster {
    super::super::roster::BoundedRoster::restore(
        &peer.certificates(),
        &[],
        peer.authority(),
        now_ns(now),
    )
    .unwrap()
}

#[test]
fn bounded_revocations_preserves_independent_verified_union_and_earliest_floor() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let roster = roster(&peer, now);
    let root = SigningKey::from_bytes(&[1; 32]);
    let independent = SigningKey::from_bytes(&[8; 32]);
    let stored = vec![
        signed(member.device_public_key, 30, &root),
        signed([99; 32], 10, &independent),
    ];
    let mut enrollment = RevocationList::new();
    enrollment
        .insert(signed(member.device_public_key, 20, &root))
        .unwrap();
    let union = BoundedRevocations::restore(
        &stored,
        &enrollment,
        &roster,
        peer.authority().device_public_key,
    )
    .unwrap();
    drop(stored);
    drop(enrollment);
    assert_eq!(union.iter().count(), 2);
    assert_eq!(
        union
            .iter()
            .find(|v| v.revoked_device_public_key == member.device_public_key)
            .unwrap()
            .revoked_at_ns,
        20
    );
    assert_eq!(
        union
            .iter()
            .find(|v| v.revoked_device_public_key == [99; 32])
            .unwrap()
            .user_public_key,
        independent.verifying_key().to_bytes()
    );
}

#[test]
fn bounded_revocations_rejects_mismatched_root_signature_and_pinned_authority() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let roster = roster(&peer, now);
    let empty = RevocationList::new();
    let root = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[8; 32]);
    let mismatch = [signed(member.device_public_key, 1, &other)];
    let mut forged = [signed([99; 32], 1, &root)];
    forged[0].signature[0] ^= 1;
    let revoked = [signed(peer.authority().device_public_key, 1, &root)];
    for (entries, expected) in [
        (&mismatch[..], RestoreRevocationsError::RootMismatch),
        (
            &forged[..],
            RestoreRevocationsError::Signature(RevocationError::BadSignature),
        ),
        (&revoked[..], RestoreRevocationsError::AuthorityRevoked),
    ] {
        let result = BoundedRevocations::restore(
            entries,
            &empty,
            &roster,
            peer.authority().device_public_key,
        );
        assert!(matches!(result, Err(error) if error == expected));
    }
}

#[test]
fn bounded_revocations_restores_growing_real_tables() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let roster = roster(&peer, now);
    let root = SigningKey::from_bytes(&[1; 32]);
    let entries = (0u32..129)
        .map(|n| {
            let mut key = [91; 32];
            key[..4].copy_from_slice(&n.to_le_bytes());
            signed(key, 1, &root)
        })
        .collect::<Vec<_>>();
    let empty = RevocationList::new();
    for count in [0, 1, 3, 4, 7, 8, 14, 15, 28, 29, 128, 129] {
        let value = BoundedRevocations::restore(
            &entries[..count],
            &empty,
            &roster,
            peer.authority().device_public_key,
        )
        .unwrap();
        assert_eq!(value.iter().count(), count);
    }
}
