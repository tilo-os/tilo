use super::*;
use ed25519_dalek::SigningKey;
use qos_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
};

const NOW: u64 = 1_000_000;
const VERIFIER: [u8; 32] = [91; 32];

fn party(expires_at_ns: Option<u64>) -> (SigningKey, SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[11; 32]);
    let device = SigningKey::from_bytes(&[22; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Alice's laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: NOW - 100,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (root, device, cert)
}

fn proof_for(
    verifier: &mut PendingPeerChallenges,
    device: &SigningKey,
    cert: DeviceCert,
    nonce: u8,
) -> PeerAuthProof {
    let challenge = verifier.register([nonce; 32], NOW, 1_000).unwrap();
    PeerAuthProof::respond(cert, device.verifying_key().to_bytes(), challenge, device).unwrap()
}

fn verifier() -> PendingPeerChallenges {
    PendingPeerChallenges::new(VERIFIER, 4, 10_000)
}

#[test]
fn establishes_human_and_device_identity() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert.clone(), 1);
    let peer = pending
        .verify(
            &proof,
            device.verifying_key().to_bytes(),
            &RevocationList::new(),
            NOW,
        )
        .unwrap();
    assert_eq!(peer.user_public_key, cert.user_public_key);
    assert_eq!(peer.device_public_key, cert.device_public_key);
    assert_eq!(peer.device_label, "Alice's laptop");
    assert!(pending.is_empty());
}

#[test]
fn tampered_proof_is_rejected_and_burns_challenge() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let mut proof = proof_for(&mut pending, &device, cert, 2);
    proof.signature[0] ^= 1;
    let endpoint = device.verifying_key().to_bytes();
    assert_eq!(
        pending
            .verify(&proof, endpoint, &RevocationList::new(), NOW)
            .unwrap_err(),
        PeerAuthError::BadProofSignature
    );
    assert_eq!(
        pending
            .verify(&proof, endpoint, &RevocationList::new(), NOW)
            .unwrap_err(),
        PeerAuthError::UnknownOrReplayedChallenge
    );
}

#[test]
fn tampered_certificate_is_rejected() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let mut proof = proof_for(&mut pending, &device, cert, 13);
    proof.cert.device_label.push_str(" (attacker)");
    assert!(matches!(
        pending.verify(
            &proof,
            device.verifying_key().to_bytes(),
            &RevocationList::new(),
            NOW
        ),
        Err(PeerAuthError::Certificate(DeviceCertError::BadSignature))
    ));
}

#[test]
fn expired_certificate_is_rejected() {
    let (_root, device, cert) = party(Some(NOW));
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 3);
    assert!(matches!(
        pending.verify(
            &proof,
            device.verifying_key().to_bytes(),
            &RevocationList::new(),
            NOW
        ),
        Err(PeerAuthError::Certificate(DeviceCertError::Expired))
    ));
}

#[test]
fn effective_same_user_revocation_is_rejected() {
    let (root, device, cert) = party(None);
    let mut entry = RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: NOW - 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 4);
    assert_eq!(
        pending
            .verify(&proof, device.verifying_key().to_bytes(), &revocations, NOW)
            .unwrap_err(),
        PeerAuthError::Revoked
    );
}

#[test]
fn another_users_revocation_does_not_poison_device_identity() {
    let (_root, device, cert) = party(None);
    let other_root = SigningKey::from_bytes(&[33; 32]);
    let mut entry = RevocationEntry {
        user_public_key: other_root.verifying_key().to_bytes(),
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: NOW - 1,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &other_root).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 5);
    pending
        .verify(&proof, device.verifying_key().to_bytes(), &revocations, NOW)
        .unwrap();
}

#[test]
fn proof_wire_round_trip_preserves_signed_bytes() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 14);
    let encoded = postcard::to_stdvec(&proof).unwrap();
    let decoded: PeerAuthProof = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(decoded, proof);
    pending
        .verify(
            &decoded,
            device.verifying_key().to_bytes(),
            &RevocationList::new(),
            NOW,
        )
        .unwrap();
}

#[test]
fn future_issued_certificate_is_rejected() {
    let (root, device, mut cert) = party(None);
    cert.issued_at_ns = NOW + 1;
    sign_device_cert(&mut cert, &root).unwrap();
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 15);
    assert_eq!(
        pending
            .verify(
                &proof,
                device.verifying_key().to_bytes(),
                &RevocationList::new(),
                NOW,
            )
            .unwrap_err(),
        PeerAuthError::CertificateNotYetValid
    );
}

#[test]
fn observed_endpoint_must_equal_certified_device() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 6);
    assert_eq!(
        pending
            .verify(&proof, [44; 32], &RevocationList::new(), NOW)
            .unwrap_err(),
        PeerAuthError::EndpointMismatch
    );
}

#[test]
fn valid_proof_cannot_be_replayed() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert, 7);
    let endpoint = device.verifying_key().to_bytes();
    pending
        .verify(&proof, endpoint, &RevocationList::new(), NOW)
        .unwrap();
    assert_eq!(
        pending
            .verify(&proof, endpoint, &RevocationList::new(), NOW)
            .unwrap_err(),
        PeerAuthError::UnknownOrReplayedChallenge
    );
}

#[test]
fn expired_or_changed_challenge_fails_closed() {
    let (_root, device, cert) = party(None);
    let mut pending = verifier();
    let proof = proof_for(&mut pending, &device, cert.clone(), 8);
    assert_eq!(
        pending
            .verify(
                &proof,
                device.verifying_key().to_bytes(),
                &RevocationList::new(),
                NOW + 1_000,
            )
            .unwrap_err(),
        PeerAuthError::ChallengeExpired
    );

    let mut pending = verifier();
    let mut proof = proof_for(&mut pending, &device, cert, 9);
    proof.challenge.expires_at_ns += 1;
    assert_eq!(
        pending
            .verify(
                &proof,
                device.verifying_key().to_bytes(),
                &RevocationList::new(),
                NOW,
            )
            .unwrap_err(),
        PeerAuthError::ChallengeMismatch
    );
}

#[test]
fn challenge_store_is_bounded_and_rejects_duplicate_nonce() {
    let mut pending = PendingPeerChallenges::new(VERIFIER, 1, 100);
    let challenge = pending.register([10; 32], NOW, 1_000).unwrap();
    assert_eq!(challenge.expires_at_ns, NOW + 100, "TTL must be capped");
    assert_eq!(
        pending.register([11; 32], NOW, 1).unwrap_err(),
        PeerAuthError::ChallengeCapacity
    );

    let mut pending = verifier();
    pending.register([12; 32], NOW, 1).unwrap();
    assert_eq!(
        pending.register([12; 32], NOW, 1).unwrap_err(),
        PeerAuthError::DuplicateChallenge
    );
}
