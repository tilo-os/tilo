use super::*;
use crate::state::now_ns;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCertError, sign_device_cert};

fn labeled(mut certificate: DeviceCert, label: &str) -> DeviceCert {
    certificate.device_label = label.into();
    sign_device_cert(&mut certificate, &SigningKey::from_bytes(&[1; 32])).unwrap();
    certificate
}

#[test]
fn roster_preserves_historical_enrollment_conflicts_and_signature_checks() {
    let (peer, _, _, mut member, now) = crate::admission_tests::seeded();
    let authority = peer.owner_certificates()[0].clone();
    let now = now_ns(now);
    member.expires_at_ns = Some(now);
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let stored = [member.clone()];
    let enrollment = [member.clone()];
    // Historical exact evidence can outlive expiry; a newly enrolled expired
    // certificate must still fail the current validity check.
    let historical = restore_legacy(stored.to_vec(), enrollment.to_vec(), &authority, now).unwrap();
    assert_eq!(
        historical.get(&member.device_public_key).map(AsRef::as_ref),
        Some(&member)
    );
    assert!(matches!(
        restore_legacy(Vec::new(), enrollment.to_vec(), &authority, now),
        Err(RosterError::Certificate(DeviceCertError::Expired))
    ));
    let conflicting = labeled(member.clone(), "different signed label");
    assert!(matches!(
        restore_legacy(
            vec![member.clone(), conflicting.clone()],
            Vec::new(),
            &authority,
            now
        ),
        Err(RosterError::StoredConflict)
    ));
    assert!(matches!(
        restore_legacy(stored.to_vec(), vec![conflicting], &authority, now),
        Err(RosterError::EnrollmentConflict)
    ));
    member.signature[0] ^= 1;
    let forged = [member];
    assert!(matches!(
        restore_legacy(forged.to_vec(), Vec::new(), &authority, now),
        Err(RosterError::Certificate(DeviceCertError::BadSignature))
    ));
}

#[test]
fn roster_restores_many_and_repeated_signed_entries() {
    let (peer, _, _, _, now) = crate::admission_tests::seeded();
    let authority = peer.owner_certificates()[0].clone();
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
    let roster = restore_legacy(certificates.clone(), Vec::new(), &authority, now_ns(now)).unwrap();
    for certificate in &certificates {
        assert_eq!(
            roster
                .get(&certificate.device_public_key)
                .map(AsRef::as_ref),
            Some(certificate)
        );
    }
}
