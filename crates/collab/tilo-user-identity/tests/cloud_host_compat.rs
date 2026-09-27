#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use ed25519_dalek::SigningKey;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert,
    cloud_enrollment::{
        Challenge, EnrollmentRequest, host_capabilities, sign_request, verify_request,
    },
    sign_device_cert,
};

fn certificate(root: &SigningKey, key: [u8; 32], capabilities: DeviceCapabilities) -> DeviceCert {
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key,
        device_label: "compatibility fixture".into(),
        capabilities,
        issued_at_ns: 10,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, root).unwrap();
    certificate
}

#[test]
fn deployed_cloud_enrollment_transcript_remains_verifiable() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let browser = SigningKey::from_bytes(&[2; 32]);
    let host = SigningKey::from_bytes(&[3; 32]);
    let challenge = Challenge {
        version: 1,
        machine_id: "machine-a".into(),
        room: "0199f1cc-8080-7000-8000-000000000001".into(),
        workspace_id: "42".into(),
        host_public_key: host.verifying_key().to_bytes(),
        nonce: [4; 32],
        expires_at_ns: 1_000,
        root_public_key: None,
        host_certificate: None,
    };
    let mut request = EnrollmentRequest {
        version: 1,
        browser_certificate: certificate(
            &root,
            browser.verifying_key().to_bytes(),
            DeviceCapabilities::member(),
        ),
        host_certificate: certificate(&root, host.verifying_key().to_bytes(), host_capabilities()),
        challenge: challenge.clone(),
        root_signature: Vec::new(),
        device_signature: Vec::new(),
    };
    sign_request(&mut request, &root, &browser).unwrap();

    verify_request(&request, &challenge, 20).unwrap();

    request.version = 2;
    assert!(verify_request(&request, &challenge, 20).is_err());
}
