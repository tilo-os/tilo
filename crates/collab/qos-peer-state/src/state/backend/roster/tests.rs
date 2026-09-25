use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
use tilo_user_identity::{DeviceCertError, sign_device_cert};

fn context() -> (NativeAllowance, NativeAllocationSet, NativeWorkTurn) {
    let allowance = NativeAllowance::new(4 * 1024 * 1024).unwrap();
    let allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    (allowance, allocations, turn)
}
fn labeled(mut certificate: DeviceCert, label: &str) -> DeviceCert {
    certificate.device_label = label.into();
    sign_device_cert(&mut certificate, &SigningKey::from_bytes(&[1; 32])).unwrap();
    certificate
}

#[test]
fn bounded_roster_detaches_real_verified_certificate_backing_and_keeps_credit() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let authority = labeled(peer.authority().clone(), &"signed governor 🦀".repeat(1024));
    let member = labeled(member, &"member long label".repeat(2048));
    let stored = vec![authority.clone(), member.clone()];
    let (allowance, _allocations, turn) = context();
    let before = allowance.retained_bytes();
    let roster = assert_precharged(&allowance, || {
        BoundedRoster::restore(&stored, &[], &authority, now_ns(now), &turn)
    })
    .unwrap();
    assert_eq!(roster.certificate(&member.device_public_key), Some(&member));
    assert_ne!(
        roster
            .certificate(&member.device_public_key)
            .unwrap()
            .device_label
            .as_ptr(),
        stored[1].device_label.as_ptr()
    );
    assert!(allowance.retained_bytes() > before + member.device_label.len());
    drop(stored);
    drop(peer);
    allowance.finish_turn();
    assert_eq!(
        roster.certificate(&authority.device_public_key),
        Some(&authority)
    );
    drop(roster);
    assert_eq!(allowance.retained_bytes(), before);
}

#[test]
fn bounded_roster_preserves_historical_enrollment_conflicts_and_signature_checks() {
    let (peer, _, _, mut member, now) = crate::admission_tests::seeded();
    let authority = peer.authority().clone();
    let now = now_ns(now);
    member.expires_at_ns = Some(now);
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let (allowance, _allocations, turn) = context();
    let stored = [member.clone()];
    let enrollment = [member.clone()];
    // Historical exact evidence can outlive expiry; a newly enrolled expired
    // certificate must still fail the current validity check.
    let historical = assert_precharged(&allowance, || {
        BoundedRoster::restore(&stored, &enrollment, &authority, now, &turn)
    })
    .unwrap();
    assert_eq!(
        historical.certificate(&member.device_public_key),
        Some(&member)
    );
    assert!(matches!(
        BoundedRoster::restore(&[], &enrollment, &authority, now, &turn),
        Err(RosterError::Certificate(DeviceCertError::Expired))
    ));
    let conflicting = labeled(member.clone(), "different signed label");
    assert!(matches!(
        BoundedRoster::restore(
            &[member.clone(), conflicting.clone()],
            &[],
            &authority,
            now,
            &turn
        ),
        Err(RosterError::StoredConflict)
    ));
    assert!(matches!(
        BoundedRoster::restore(&stored, &[conflicting], &authority, now, &turn),
        Err(RosterError::EnrollmentConflict)
    ));
    member.signature[0] ^= 1;
    let forged = [member];
    assert!(matches!(
        assert_precharged(&allowance, || BoundedRoster::restore(
            &forged,
            &[],
            &authority,
            now,
            &turn
        )),
        Err(RosterError::Certificate(DeviceCertError::BadSignature))
    ));
}

#[test]
fn bounded_roster_refuses_before_copying_a_real_label_and_releases_failure_credit() {
    let (peer, _, _, member, now) = crate::admission_tests::seeded();
    let authority = peer.authority().clone();
    let member = labeled(member, &"large member label".repeat(4096));
    let stored = [member];
    let (allowance, _allocations, turn) = context();
    let exhausted = turn.reserve(allowance.available_bytes() - 512).unwrap();
    let before = allowance.retained_bytes();
    assert!(matches!(
        assert_precharged(&allowance, || BoundedRoster::restore(
            &stored,
            &[],
            &authority,
            now_ns(now),
            &turn
        )),
        Err(RosterError::Resource(NativeResourceError::WorkBytes))
    ));
    assert_eq!(allowance.retained_bytes(), before);
    drop(exhausted);
}

#[test]
fn bounded_roster_accounts_real_map_splits_and_repeated_signed_entries() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let authority = peer.authority().clone();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut certificates = Vec::new();
    for index in 0u32..128 {
        let mut key = [91; 32];
        key[..4].copy_from_slice(&index.to_le_bytes());
        let mut value = authority.clone();
        value.device_public_key = SigningKey::from_bytes(&key).verifying_key().to_bytes();
        value.device_label = format!("signed member {index}: {}", "label".repeat(200));
        sign_device_cert(&mut value, &root).unwrap();
        certificates.push(value);
    }
    certificates.push(certificates[0].clone());
    let (allowance, _allocations, turn) = context();
    let baseline = allowance.retained_bytes();
    let roster = assert_precharged(&allowance, || {
        BoundedRoster::restore(&certificates, &[], &authority, now_ns(now), &turn)
    })
    .unwrap();
    for certificate in &certificates {
        assert_eq!(
            roster.certificate(&certificate.device_public_key),
            Some(certificate)
        );
    }
    drop(roster);
    assert_eq!(allowance.retained_bytes(), baseline);
}
