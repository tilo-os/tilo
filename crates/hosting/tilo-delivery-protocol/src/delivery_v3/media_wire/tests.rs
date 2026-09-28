use super::*;
use tilo_user_identity::canonical::signing_digest;

fn request() -> GrantRequest {
    GrantRequest {
        device: [1; 32],
        room: "018f0f9e-11aa-7c42-8000-000000000001".into(),
        call_id: "02".repeat(32),
        participation_id: [3; 32],
        generation: 9_007_199_254_740_993,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    }
}

#[test]
fn native_challenge_preserves_existing_purpose_zero_digest() {
    let value = NativeChallenge {
        request: request(),
        operation: GrantOperation::Join,
        epoch: 4,
        membership_generation: 5,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
        audience: "https://media.test".into(),
        host: [6; 32],
        host_boot: [7; 32],
        nonce: [8; 32],
        issued_at_ns: 100,
        expires_at_ns: 200,
    };
    // The independent compatibility expression is the existing native tuple,
    // including its exact field order and purpose byte, not the new encoder.
    let expected = signing_digest(
        b"tilo-media-v1\0",
        &(
            0_u8,
            &value.request,
            value.operation,
            value.epoch,
            value.membership_generation,
            value.directions,
            &value.audience,
            value.host,
            value.host_boot,
            value.nonce,
            value.issued_at_ns,
            value.expires_at_ns,
        ),
    )
    .unwrap();
    assert_eq!(
        hex::encode(expected),
        "2edacfbbd7dce809e9b8008aa673cb9f2a835d3e83a68dcb51b7d2b01d1f16c1"
    );
    assert_eq!(
        ChallengeTranscript::Native(value).signing_digest().unwrap(),
        expected
    );
}

#[test]
fn control_decode_rejects_tail_unknown_version_and_overlimit_grant() {
    let value = ControlRequest {
        version: 1,
        command: ControlCommand::RelayChallenge {
            grant: vec![1; 4096],
        },
    };
    let encoded = value.encode().unwrap();
    assert_eq!(ControlRequest::decode(&encoded).unwrap(), value);
    let mut bad = encoded.clone();
    bad.push(0);
    assert!(ControlRequest::decode(&bad).is_err());
    let mut bad = encoded;
    bad[0] = 2;
    assert!(ControlRequest::decode(&bad).is_err());
    let too_large = ControlRequest {
        version: 1,
        command: ControlCommand::RelayChallenge {
            grant: vec![1; 4097],
        },
    };
    assert!(too_large.encode().is_err());
    let untrusted = postcard::to_stdvec(&too_large).unwrap();
    assert!(ControlRequest::decode(&untrusted).is_err());
}

#[test]
fn inventory_is_exact_sorted_unique_and_bounded() {
    let a = MediaNamespace {
        call: [1; 32],
        publisher: [2; 32],
    };
    let b = MediaNamespace {
        call: [1; 32],
        publisher: [3; 32],
    };
    assert!(
        PublisherInventory {
            revision: 1,
            namespaces: vec![a, b]
        }
        .validate()
        .is_ok()
    );
    assert!(
        PublisherInventory {
            revision: 1,
            namespaces: vec![b, a]
        }
        .validate()
        .is_err()
    );
    assert!(
        PublisherInventory {
            revision: 1,
            namespaces: vec![a, a]
        }
        .validate()
        .is_err()
    );
    assert!(
        PublisherInventory {
            revision: 1,
            namespaces: vec![a; 8]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn relay_refresh_and_setup_preserve_native_bytes_and_literal_digests() {
    let relay = RelayProofChallenge {
        grant_digest: [1; 32],
        session_id: [2; 32],
        audience: "https://media.test".into(),
        host: [3; 32],
        host_boot: [4; 32],
        nonce: [5; 32],
        issued_at_ns: 100,
        expires_at_ns: 200,
    };
    let refresh = RefreshChallenge {
        grant_digest: relay.grant_digest,
        session_id: relay.session_id,
        authorization: [6; 32],
        audience: relay.audience.clone(),
        host: relay.host,
        host_boot: relay.host_boot,
        nonce: relay.nonce,
        issued_at_ns: 100,
        expires_at_ns: 200,
    };
    let expected_relay = signing_digest(
        b"tilo-media-v1\0",
        &(
            2_u8,
            &relay.grant_digest,
            relay.session_id,
            &relay.audience,
            relay.host,
            relay.host_boot,
            relay.nonce,
            relay.issued_at_ns,
            relay.expires_at_ns,
        ),
    )
    .unwrap();
    let expected_refresh = signing_digest(
        b"tilo-media-v1\0",
        &(
            5_u8,
            &refresh.grant_digest,
            refresh.session_id,
            refresh.authorization,
            &refresh.audience,
            refresh.host,
            refresh.host_boot,
            refresh.nonce,
            refresh.issued_at_ns,
            refresh.expires_at_ns,
        ),
    )
    .unwrap();
    assert_eq!(
        ChallengeTranscript::Relay(relay).signing_digest().unwrap(),
        expected_relay
    );
    assert_eq!(
        ChallengeTranscript::Refresh(refresh)
            .signing_digest()
            .unwrap(),
        expected_refresh
    );
    assert_eq!(
        (hex::encode(expected_relay), hex::encode(expected_refresh)),
        (
            "cd6378bf459cae53c7952234fcb9913500a5396f1d65403db9079016689b4810".into(),
            "0b9e88869c2d9bf734db42763aec4bc77a45660bb340a6230fecf047c7151974".into()
        )
    );
    let proof = RelaySubmission {
        version: 1,
        session: [2; 32],
        nonce: [5; 32],
        signature: [[7; 32], [8; 32]],
    };
    let mut literal = vec![1];
    literal.extend([2; 32]);
    literal.extend([5; 32]);
    literal.extend([7; 32]);
    literal.extend([8; 32]);
    assert_eq!(proof.encode().unwrap(), literal);
    assert_eq!(RelaySubmission::decode(&literal).unwrap(), proof);
}

#[test]
fn join_reservation_cannot_be_reused_as_finalized_or_renew_intent() {
    let mut pending = request();
    pending.generation = 0;
    let mut wire = ControlRequest {
        version: 1,
        command: ControlCommand::GrantChallenge {
            request: pending.clone(),
            operation: GrantOperation::Join,
        },
    };
    assert!(wire.encode().is_ok());
    for operation in [
        GrantOperation::Renew,
        GrantOperation::Leave,
        GrantOperation::Heartbeat,
    ] {
        wire.command = ControlCommand::GrantChallenge {
            request: pending.clone(),
            operation,
        };
        assert!(wire.encode().is_err());
        assert!(ControlRequest::decode(&postcard::to_stdvec(&wire).unwrap()).is_err());
    }
    assert!(pending.validate(false).is_err());
}

fn context() -> MediaContext {
    MediaContext {
        version: 1,
        descriptor_digest: [1; 32],
        delivery_session_id: [2; 16],
        delivery_boot: [3; 16],
        origin: "https://media.test".into(),
        workspace: 7,
        room: request().room,
        root: [4; 32],
        delivery_host: [5; 32],
        media_host: [6; 32],
        media_boot: [7; 32],
        request: request(),
        epoch: 4,
        membership_generation: 5,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
        inventory: PublisherInventory {
            revision: 1,
            namespaces: vec![],
        },
        issued_at_ns: 100,
        expires_at_ns: 200,
    }
}

#[test]
fn signed_context_cannot_describe_another_inner_proof_identity() {
    let context = context();
    let transcript = ChallengeTranscript::Native(NativeChallenge {
        request: context.request.clone(),
        operation: GrantOperation::Join,
        epoch: context.epoch,
        membership_generation: context.membership_generation,
        directions: context.directions,
        audience: context.origin.clone(),
        host: context.media_host,
        host_boot: context.media_boot,
        nonce: [8; 32],
        issued_at_ns: 100,
        expires_at_ns: 200,
    });
    let valid = SignedChallenge {
        context,
        transcript,
        signature: [[0; 32]; 2],
    };
    assert!(valid.encode().is_ok());
    for field in 0..6 {
        let mut bad = valid.clone();
        match field {
            0 => bad.context.media_host = [9; 32],
            1 => bad.context.media_boot = [9; 32],
            2 => bad.context.request.generation += 1,
            3 => bad.context.epoch += 1,
            4 => bad.context.membership_generation += 1,
            _ => bad.context.directions.publish = false,
        }
        assert!(bad.encode().is_err(), "conflicting context field {field}");
        assert!(SignedChallenge::decode(&postcard::to_stdvec(&bad).unwrap()).is_err());
    }
}

#[test]
fn bootstrap_binds_candidate_nonce_host_and_exact_bounded_response() {
    let request = BootstrapRequest {
        version: 1,
        device: [1; 32],
        nonce: [2; 32],
        workspace: 7,
        root: [3; 32],
        room: "room".into(),
        candidate_digest: [4; 32],
        signature: [[0; 32]; 2],
    };
    let encoded = request.encode().unwrap();
    assert_eq!(BootstrapRequest::decode(&encoded).unwrap(), request);
    let mut response = BootstrapResponse {
        version: 1,
        request,
        host_certificate: vec![5],
        hint: vec![6],
        generation: 1,
        descriptor_digest: [4; 32],
        issued_at_ns: 100,
        expires_at_ns: 30_000_000_100,
        signature: [[0; 32]; 2],
    };
    assert_eq!(
        BootstrapResponse::decode(&response.encode().unwrap()).unwrap(),
        response
    );
    let digest = response.signing_digest().unwrap();
    response.request.nonce[0] ^= 1;
    assert_ne!(response.signing_digest().unwrap(), digest);
    response.hint = vec![7; 24 * 1024 + 1];
    assert!(response.encode().is_err());
    response.hint = vec![7];
    response.descriptor_digest = [8; 32];
    assert!(response.encode().is_err());
    response.descriptor_digest = [4; 32];
    response.expires_at_ns += 1;
    assert!(response.encode().is_err());
    let mut tail = encoded;
    tail.push(0);
    assert!(BootstrapRequest::decode(&tail).is_err());
    assert!(BootstrapResponse::decode(&vec![0; 32 * 1024 + 1]).is_err());
}
