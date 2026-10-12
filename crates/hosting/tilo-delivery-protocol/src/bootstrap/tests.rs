use super::*;

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

/// Joined rooms persist hints obtained through this exchange and re-verify the
/// governor signature over these exact bytes, so the encoding and both signing
/// digests are frozen. Captured from the `delivery_v3::media_wire` definitions
/// this module replaced.
#[test]
fn bootstrap_request_bytes_are_stable() {
    let request = BootstrapRequest {
        version: 1,
        device: [1; 32],
        nonce: [2; 32],
        workspace: 7,
        root: [3; 32],
        room: "room".into(),
        candidate_digest: [4; 32],
        signature: [[5; 32], [6; 32]],
    };
    let response = BootstrapResponse {
        version: 1,
        request: request.clone(),
        host_certificate: vec![7, 8],
        hint: vec![9, 10, 11],
        generation: 300,
        descriptor_digest: [4; 32],
        issued_at_ns: 100,
        expires_at_ns: 30_000_000_100,
        signature: [[12; 32], [13; 32]],
    };
    let request_bytes = concat!(
        "01",
        "0101010101010101010101010101010101010101010101010101010101010101",
        "0202020202020202020202020202020202020202020202020202020202020202",
        "07",
        "0303030303030303030303030303030303030303030303030303030303030303",
        "04726f6f6d",
        "0404040404040404040404040404040404040404040404040404040404040404",
        "0505050505050505050505050505050505050505050505050505050505050505",
        "0606060606060606060606060606060606060606060606060606060606060606",
    );
    assert_eq!(hex::encode(request.encode().unwrap()), request_bytes);
    assert_eq!(
        hex::encode(request.signing_digest().unwrap()),
        "1b6698a98a0b1dcf7da63d695d7e0f2034423427e7a0a2a6bf15510bce88861b"
    );
    let response_bytes = [
        "01",
        request_bytes,
        "020708",
        "03090a0b",
        "ac02",
        "0404040404040404040404040404040404040404040404040404040404040404",
        "64",
        "e4d88ee16f",
        "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
        "0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d",
    ]
    .concat();
    assert_eq!(hex::encode(response.encode().unwrap()), response_bytes);
    assert_eq!(
        hex::encode(response.signing_digest().unwrap()),
        "710efb603f6a4eb3bb67a597087ef019a2aeb64e3d3f6a571dc5f9c99d39decc"
    );
}
