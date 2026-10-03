#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::*;
use ed25519_dalek::{Signer, SigningKey};
fn certificate(key: &SigningKey) -> DeviceCert {
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "https join".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    cert
}
fn split(s: [u8; 64]) -> [[u8; 32]; 2] {
    [s[..32].try_into().unwrap(), s[32..].try_into().unwrap()]
}
#[test]
fn onboarding_signed_original_request_and_reply_fence_exact_scope_nonce_and_body() {
    let device = SigningKey::from_bytes(&[2; 32]);
    let host = SigningKey::from_bytes(&[3; 32]);
    let cert = certificate(&device);
    let cert_bytes = postcard::to_stdvec(&cert).unwrap();
    let inner = tilo_peer_state::join::JoinTransportRequest::Inspect {
        signed_invitation: vec![1, 2, 3],
    };
    let body = postcard::to_stdvec(&inner).unwrap();
    let mut request = OnboardingRequest {
        version: 3,
        operation: OnboardingOperation::Inspect,
        scope: OnboardingScope {
            origin: "https://host.test",
            host: host.verifying_key().to_bytes(),
            workspace: 7,
            root: [4; 32],
            room: Uuid::from_u128(5),
        },
        certificate: &cert_bytes,
        nonce: [6; 32],
        issued_at_ns: 100,
        expires_at_ns: 1000,
        body_digest: *blake3::hash(&body).as_bytes(),
        body: &body,
        signature: [[0; 32]; 2],
    };
    request.signature = split(device.sign(&request.signing_digest().unwrap()).to_bytes());
    // Independent literal-domain tuple freezes field order and body/certificate digests.
    let reference = tilo_user_identity::canonical::signing_digest(
        b"tilo-delivery-v3-onboarding-request\0",
        &(
            3_u16,
            OnboardingOperation::Inspect,
            request.scope,
            *blake3::hash(&cert_bytes).as_bytes(),
            [6_u8; 32],
            100_u64,
            1000_u64,
            *blake3::hash(&body).as_bytes(),
        ),
    )
    .unwrap();
    assert_eq!(request.signing_digest().unwrap(), reference);
    assert_eq!(
        hex::encode(reference),
        "8d7ca5f2b1153cf37879f3fb0ad79fd565f97ca7fd201d192745f2c628ac448c"
    );
    for field in 0..11 {
        let mut changed = request.clone();
        match field {
            0 => changed.version = 4,
            1 => changed.operation = OnboardingOperation::Challenge,
            2 => changed.scope.origin = "https://other.test",
            3 => changed.scope.host[0] ^= 1,
            4 => changed.scope.workspace += 1,
            5 => changed.scope.root[0] ^= 1,
            6 => changed.scope.room = Uuid::from_u128(8),
            7 => changed.nonce[0] ^= 1,
            8 => changed.issued_at_ns += 1,
            9 => changed.expires_at_ns -= 1,
            _ => changed.body_digest[0] ^= 1,
        }
        assert!(
            changed
                .verify_device_signature(request.scope, &cert, 100)
                .is_err(),
            "request field {field}"
        );
    }
    let bytes = request.encode().unwrap();
    let parsed = OnboardingRequest::decode(&bytes).unwrap();
    assert_eq!(parsed, request);
    assert_eq!(parsed.body, body);
    assert_eq!(parsed.device_certificate().unwrap(), cert);
    parsed
        .verify_device_signature(request.scope, &cert, 100)
        .unwrap();
    for field in 0..6 {
        let mut wrong = request.scope;
        match field {
            0 => wrong.origin = "https://other.test",
            1 => wrong.host[0] ^= 1,
            2 => wrong.workspace += 1,
            3 => wrong.root[0] ^= 1,
            4 => wrong.room = Uuid::from_u128(9),
            _ => {
                let mut bad = request.clone();
                bad.nonce[0] ^= 1;
                assert!(
                    bad.verify_device_signature(request.scope, &cert, 100)
                        .is_err()
                );
                continue;
            }
        }
        assert!(parsed.verify_device_signature(wrong, &cert, 100).is_err());
    }
    let response_body = postcard::to_stdvec(&tilo_peer_state::join::JoinTransportResponse::Error {
        code: 403,
        message: "denied".into(),
    })
    .unwrap();
    let mut response = OnboardingResponse {
        version: 3,
        operation: request.operation,
        body_kind: ReplyBodyKind::InlineLegacy,
        request_digest: request.request_digest().unwrap(),
        request_nonce: request.nonce,
        host: request.scope.host,
        server_boot_id: [7; 16],
        issued_at_ns: 100,
        expires_at_ns: 1000,
        body_digest: *blake3::hash(&response_body).as_bytes(),
        body: &response_body,
        signature: [[0; 32]; 2],
    };
    response.signature = split(host.sign(&response.signing_digest().unwrap()).to_bytes());
    for field in 0..10 {
        let mut changed = response.clone();
        match field {
            0 => changed.version = 4,
            1 => changed.operation = OnboardingOperation::Challenge,
            2 => changed.body_kind = ReplyBodyKind::HandoffManifest,
            3 => changed.request_digest[0] ^= 1,
            4 => changed.request_nonce[0] ^= 1,
            5 => changed.host[0] ^= 1,
            6 => changed.server_boot_id[0] ^= 1,
            7 => changed.issued_at_ns += 1,
            8 => changed.expires_at_ns -= 1,
            _ => changed.body_digest[0] ^= 1,
        }
        assert!(
            changed.verify_host_signature(&request, 100).is_err(),
            "response field {field}"
        );
    }
    let bytes = response.encode().unwrap();
    let parsed = OnboardingResponse::decode(&bytes).unwrap();
    parsed.verify_host_signature(&request, 100).unwrap();
    let mut retry = request.clone();
    retry.nonce = [8; 32];
    retry.signature = split(device.sign(&retry.signing_digest().unwrap()).to_bytes());
    assert!(parsed.verify_host_signature(&retry, 100).is_err());
    assert!(parsed.verify_host_signature(&request, 1000).is_err());
    let mut reflected = response.clone();
    reflected.signature = request.signature;
    assert!(reflected.verify_host_signature(&request, 100).is_err());
    let mut tampered = request.clone();
    tampered.body = &[0, 1, 9];
    assert!(tampered.encode().is_err());
    for end in 0..bytes.len() {
        assert!(OnboardingResponse::decode(&bytes[..end]).is_err());
    }
}
#[test]
fn onboarding_closed_operations_bounds_and_no_declared_length_allocation() {
    for (index, op) in [
        OnboardingOperation::Inspect,
        OnboardingOperation::Challenge,
        OnboardingOperation::Complete,
        OnboardingOperation::MembershipChallenge,
        OnboardingOperation::MembershipComplete,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            postcard::to_stdvec(&op).unwrap(),
            [u8::try_from(index).unwrap()]
        );
        assert_eq!(
            OnboardingOperation::from_target("POST", op.path()).unwrap(),
            op
        );
        assert!(OnboardingOperation::from_target("GET", op.path()).is_err());
    }
    assert!(postcard::from_bytes::<OnboardingOperation>(&[5]).is_err());
    assert!(OnboardingRequest::decode(&vec![0; MAX_REQUEST_BYTES + 1]).is_err());
    // version3/op0 then a hostile origin byte-count: borrowed decode cannot reserve it.
    assert!(
        OnboardingRequest::decode(&[3, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1]).is_err()
    );
}

#[test]
fn onboarding_inline_owner_membership_reply_is_scoped_to_membership_completion() {
    let body = [6u8, 1, 2, 3];
    for (operation, accepted) in [
        (OnboardingOperation::Inspect, false),
        (OnboardingOperation::Challenge, false),
        (OnboardingOperation::Complete, false),
        (OnboardingOperation::MembershipChallenge, false),
        (OnboardingOperation::MembershipComplete, true),
    ] {
        let response = OnboardingResponse {
            version: 3,
            operation,
            body_kind: ReplyBodyKind::InlineLegacy,
            request_digest: [1; 32],
            request_nonce: [2; 32],
            host: [3; 32],
            server_boot_id: [4; 16],
            issued_at_ns: 100,
            expires_at_ns: 1000,
            body_digest: *blake3::hash(&body).as_bytes(),
            body: &body,
            signature: [[0; 32]; 2],
        };
        assert_eq!(response.encode().is_ok(), accepted, "{operation:?}");
    }
}
