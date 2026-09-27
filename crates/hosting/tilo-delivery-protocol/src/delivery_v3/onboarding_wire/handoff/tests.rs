#![allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use uuid::Uuid;
fn split(s: [u8; 64]) -> [[u8; 32]; 2] {
    [s[..32].try_into().unwrap(), s[32..].try_into().unwrap()]
}
#[test]
fn handoff_signed_pinned_range_and_exact_governance_retention() {
    let host = SigningKey::from_bytes(&[3; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let bundle = GovernanceBundle {
        items: vec![
            GovernanceItem {
                bytes: b"old exact message",
                retention: tilo_peer_state::transport::Retention::Checkpoint,
            },
            GovernanceItem {
                bytes: b"next exact message",
                retention: tilo_peer_state::transport::Retention::Delta,
            },
        ],
    };
    let size = bundle.encoded_len().unwrap();
    let bytes = postcard::to_stdvec(&bundle).unwrap();
    assert_eq!(size, bytes.len());
    assert_eq!(GovernanceBundle::decode(&bytes).unwrap(), bundle);
    let hash = *blake3::hash(&bytes).as_bytes();
    let mut manifest = HandoffManifest {
        version: 1,
        purpose: HandoffPurpose::LocalGovernance,
        scope: OnboardingScope {
            origin: "https://host.test",
            host: host.verifying_key().to_bytes(),
            workspace: 7,
            root: [4; 32],
            room: Uuid::from_u128(5),
        },
        device: device.verifying_key().to_bytes(),
        request_digest: [6; 32],
        request_nonce: [7; 32],
        owner_generation: 8,
        server_boot_id: [9; 16],
        issued_at_ns: 100,
        expires_at_ns: 1000,
        total_bytes: bytes.len() as u64,
        content_digest: hash,
        segments: vec![HandoffSegment {
            digest: hash,
            length: bytes.len() as u64,
        }],
        signature: [[0; 32]; 2],
    };
    manifest.signature = split(host.sign(&manifest.signing_digest().unwrap()).to_bytes());
    let encoded = manifest.encode().unwrap();
    let parsed = HandoffManifest::decode(&encoded).unwrap();
    assert_eq!(parsed, manifest);
    manifest
        .verify_host_signature(manifest.scope, manifest.device, [6; 32], [7; 32], 100)
        .unwrap();
    for field in 0..15 {
        let mut changed = manifest.clone();
        match field {
            0 => changed.version = 2,
            1 => changed.purpose = HandoffPurpose::JoinResponse,
            2 => changed.scope.origin = "https://other.test",
            3 => changed.scope.host[0] ^= 1,
            4 => changed.scope.workspace += 1,
            5 => changed.scope.root[0] ^= 1,
            6 => changed.scope.room = Uuid::from_u128(8),
            7 => changed.device[0] ^= 1,
            8 => changed.request_digest[0] ^= 1,
            9 => changed.request_nonce[0] ^= 1,
            10 => changed.owner_generation += 1,
            11 => changed.server_boot_id[0] ^= 1,
            12 => changed.expires_at_ns -= 1,
            13 => changed.content_digest[0] ^= 1,
            _ => changed.segments[0].digest[0] ^= 1,
        }
        assert!(
            changed
                .verify_host_signature(manifest.scope, manifest.device, [6; 32], [7; 32], 100)
                .is_err(),
            "manifest field {field}"
        );
    }
    let mut over = manifest.clone();
    over.segments = vec![
        HandoffSegment {
            digest: [1; 32],
            length: 1
        };
        65
    ];
    over.total_bytes = 65;
    assert!(over.encode().is_err());
    let encoded = postcard::to_stdvec(&over).unwrap();
    assert!(HandoffManifest::decode(&encoded).is_err());
    let mut too_long = manifest.clone();
    too_long.expires_at_ns = too_long.issued_at_ns + 30_000_000_001;
    assert!(too_long.encode().is_err());
    assert!(
        manifest
            .verify_host_signature(manifest.scope, manifest.device, [6; 32], [8; 32], 100)
            .is_err()
    );
    let mut request = HandoffRequest {
        version: 1,
        manifest_digest: manifest.manifest_digest().unwrap(),
        device: manifest.device,
        offset: 0,
        length: u32::try_from(bytes.len()).unwrap(),
        nonce: [10; 32],
        issued_at_ns: 100,
        expires_at_ns: 999,
        signature: [[0; 32]; 2],
    };
    request.signature = split(device.sign(&request.signing_digest().unwrap()).to_bytes());
    request.verify_device_signature(&manifest, 100).unwrap();
    let encoded = request.encode().unwrap();
    assert_eq!(HandoffRequest::decode(&encoded).unwrap(), request);
    let mut chunk = HandoffChunk {
        version: 1,
        manifest_digest: request.manifest_digest,
        request_digest: request.request_digest().unwrap(),
        offset: 0,
        body_digest: hash,
        body: &bytes,
        signature: [[0; 32]; 2],
    };
    chunk.signature = split(host.sign(&chunk.signing_digest().unwrap()).to_bytes());
    let encoded = chunk.encode().unwrap();
    let parsed = HandoffChunk::decode(&encoded).unwrap();
    parsed
        .verify_host_signature(&manifest, &request, 100)
        .unwrap();
    let mut other = request.clone();
    other.nonce = [11; 32];
    other.signature = split(device.sign(&other.signing_digest().unwrap()).to_bytes());
    assert!(
        parsed
            .verify_host_signature(&manifest, &other, 100)
            .is_err()
    );
    let mut outside = request.clone();
    outside.offset = manifest.total_bytes;
    outside.signature = split(device.sign(&outside.signing_digest().unwrap()).to_bytes());
    assert!(outside.verify_device_signature(&manifest, 100).is_err());
    assert!(
        parsed
            .verify_host_signature(&manifest, &request, 1000)
            .is_err()
    );
}
#[test]
fn handoff_preflight_rejects_huge_counts_and_invalid_segments() {
    assert!(GovernanceBundle::decode(&[255, 255, 255, 255, 255, 255, 255, 255, 255, 1]).is_err());
    let over = GovernanceBundle {
        items: vec![
            GovernanceItem {
                bytes: b"x",
                retention: tilo_peer_state::transport::Retention::Delta
            };
            MAX_GOVERNANCE_ITEMS + 1
        ],
    };
    assert!(over.encoded_len().is_err());
    assert!(GovernanceBundle::decode(&postcard::to_stdvec(&over).unwrap()).is_err());
    let valid = GovernanceBundle {
        items: vec![
            GovernanceItem {
                bytes: b"x",
                retention: tilo_peer_state::transport::Retention::Delta
            };
            MAX_GOVERNANCE_ITEMS
        ],
    };
    let bytes = postcard::to_stdvec(&valid).unwrap();
    assert!(GovernanceBundle::decode(&bytes).is_ok());
    assert!(HandoffManifest::decode(&vec![0; MAX_MANIFEST_BYTES + 1]).is_err());
    assert!(HandoffChunk::decode(&vec![0; MAX_HANDOFF_CHUNK_BYTES + 1]).is_err());
}

#[test]
fn handoff_range_body_and_manifest_boundaries_are_exact() {
    let body = vec![7; MAX_HANDOFF_RANGE_BYTES];
    let chunk = HandoffChunk {
        version: 1,
        manifest_digest: [1; 32],
        request_digest: [2; 32],
        offset: 0,
        body_digest: *blake3::hash(&body).as_bytes(),
        body: &body,
        signature: [[0; 32]; 2],
    };
    let encoded = chunk.encode().unwrap();
    assert!(encoded.len() <= MAX_HANDOFF_CHUNK_BYTES);
    assert_eq!(HandoffChunk::decode(&encoded).unwrap(), chunk);
    let too_large = vec![7; MAX_HANDOFF_RANGE_BYTES + 1];
    let chunk = HandoffChunk {
        body: &too_large,
        body_digest: *blake3::hash(&too_large).as_bytes(),
        ..chunk
    };
    assert!(chunk.encode().is_err());
    assert!(HandoffChunk::decode(&postcard::to_stdvec(&chunk).unwrap()).is_err());
    let scope = OnboardingScope {
        origin: "https://host.test",
        host: [1; 32],
        workspace: 7,
        root: [2; 32],
        room: Uuid::from_u128(3),
    };
    let mut manifest = HandoffManifest {
        version: 1,
        purpose: HandoffPurpose::LocalGovernance,
        scope,
        device: [4; 32],
        request_digest: [5; 32],
        request_nonce: [6; 32],
        owner_generation: 1,
        server_boot_id: [7; 16],
        issued_at_ns: 1,
        expires_at_ns: 30_000_000_001,
        total_bytes: MAX_GOVERNANCE_BYTES as u64,
        content_digest: [8; 32],
        segments: vec![
            HandoffSegment {
                digest: [9; 32],
                length: MAX_SEGMENT_BYTES
            };
            64
        ],
        signature: [[0; 32]; 2],
    };
    assert!(manifest.encode().is_ok());
    manifest.purpose = HandoffPurpose::JoinResponse;
    assert!(manifest.encode().is_err());
    manifest.segments.truncate(16);
    manifest.total_bytes = MAX_RESPONSE_BYTES as u64;
    assert!(manifest.encode().is_ok());
    manifest.total_bytes += 1;
    assert!(manifest.encode().is_err());
}
