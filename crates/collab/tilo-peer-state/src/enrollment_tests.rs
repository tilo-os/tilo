use crate::enrollment::*;
use ed25519_dalek::SigningKey;
fn challenge() -> EnrollmentChallenge {
    EnrollmentChallenge {
        version: 2,
        root_public_key: [1; 32],
        room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        workspace_id: 42,
        device_public_key: SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes(),
        request_id: [3; 32],
        nonce: [4; 32],
        expires_at_ns: 9_007_199_254_740_999,
    }
}
#[test]
fn enrollment_proof_roundtrip_is_exact_and_scope_bound() {
    let challenge = challenge();
    let encoded = encode(&challenge).unwrap();
    assert_eq!(decode(&encoded).unwrap(), challenge);
    let complete = sign(&challenge, &SigningKey::from_bytes(&[2; 32])).unwrap();
    let wire = serde_json::to_string(&complete).unwrap();
    let decoded: CompleteRequest = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        verify(&decoded, [1; 32], &challenge.room, 42, 1).unwrap(),
        challenge
    );
    assert!(verify(&decoded, [9; 32], &challenge.room, 42, 1).is_err());
    assert!(verify(&decoded, [1; 32], "other", 42, 1).is_err());
    assert!(verify(&decoded, [1; 32], &challenge.room, 43, 1).is_err());
    assert!(
        verify(
            &decoded,
            [1; 32],
            &challenge.room,
            42,
            challenge.expires_at_ns + 1
        )
        .is_err()
    );
    assert!(sign(&challenge, &SigningKey::from_bytes(&[5; 32])).is_err());
    let mut changed = challenge.clone();
    changed.request_id = [9; 32];
    let forged = CompleteRequest {
        challenge: encode(&changed).unwrap(),
        ..complete
    };
    assert!(verify(&forged, [1; 32], &challenge.room, 42, 1).is_err());
}
