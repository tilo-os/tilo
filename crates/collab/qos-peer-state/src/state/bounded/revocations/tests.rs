use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::NativeAllowance;
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
fn retained_enrollment_revocations_matches_verified_union_and_outlives_inputs_turn_registry() {
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
    let current = NativeAllowance::new(1024 * 1024).unwrap();
    let baseline = current.retained_bytes();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let retained = assert_precharged(&current, || work.verified_revocations(&entries)).unwrap();
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
    drop(work);
    drop(turn);
    drop(registry);
    assert_eq!(retained.list().len(), 80);
    assert!(current.retained_bytes() > baseline);
    drop(retained);
    assert_eq!(current.retained_bytes(), baseline);
}
#[test]
fn retained_enrollment_revocations_checks_duplicate_signatures_and_exact_preallocation_limit() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let row = signed([33; 32], 1, &root);
    let mut bad = row.clone();
    bad.signature[0] ^= 1;
    let current = NativeAllowance::new(65536).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let baseline = current.retained_bytes();
    assert!(matches!(
        assert_precharged(&current, || work.verified_revocations(&[row.clone(), bad])),
        Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Revocation))
    ));
    assert_eq!(current.retained_bytes(), baseline);
    let needed = backend::revocations::insertion_bytes(1).unwrap();
    let short = work
        .reserve_output(current.available_bytes() - needed + 1)
        .unwrap();
    assert!(matches!(
        assert_precharged(&current, || work
            .verified_revocations(std::slice::from_ref(&row))),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(short);
    let exact = work
        .reserve_output(current.available_bytes() - needed)
        .unwrap();
    let holder = assert_precharged(&current, || {
        work.verified_revocations(std::slice::from_ref(&row))
    })
    .unwrap();
    assert_eq!(holder.list().len(), 1);
    assert_eq!(current.available_bytes(), 0);
    drop(holder);
    drop(exact);
    assert_eq!(current.retained_bytes(), baseline);
}

#[test]
fn retained_enrollment_revocations_does_not_bypass_actual_restore_root_or_authority_checks() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let bytes = peer.durable().unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[7; 32]);
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
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
        let holder = assert_precharged(&current, || work.verified_revocations(&[entry])).unwrap();
        assert!(
            matches!(assert_precharged(&current,||BoundedStatePeer::restore(&bytes,peer.room(),peer.authority(),&[],holder.list(),now,&mut work)),Err(BoundedPeerError::Invalid(actual)) if actual==expected)
        );
    }
    let valid = assert_precharged(&current, || {
        work.verified_revocations(&[signed([99; 32], 1, &other)])
    })
    .unwrap();
    let restored = assert_precharged(&current, || {
        BoundedStatePeer::restore(
            &bytes,
            peer.room(),
            peer.authority(),
            &[],
            valid.list(),
            now,
            &mut work,
        )
    })
    .unwrap();
    assert!(restored.core.revocations.list().is_revoked(&[99; 32]));
    assert!(
        restored
            .read(&work)
            .unwrap()
            .member(member.device_public_key, now)
            .is_ok()
    );
}
#[test]
fn retained_enrollment_revocations_empty_union_is_allocation_free_but_sealed_turn_refuses() {
    let current = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = turn.work().unwrap();
    let exhausted = work.reserve_output(current.available_bytes()).unwrap();
    let empty = assert_precharged(&current, || work.verified_revocations(&[])).unwrap();
    assert!(empty.list().is_empty());
    current.finish_turn();
    assert!(matches!(
        work.verified_revocations(&[]),
        Err(BoundedPeerError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(empty);
    drop(exhausted);
}
