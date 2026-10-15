use super::*;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_join::{TransportHint, sign_transport_hint};
fn fixture() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut host = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "host".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut host, &root).unwrap();
    (key, host)
}
fn signed(payload: &str, key: &SigningKey, host: &DeviceCert) -> TransportHint {
    sign_transport_hint(
        TRANSPORT_DELIVERY_ORIGIN_V1,
        payload,
        host.clone(),
        host.device_public_key,
        key,
    )
    .unwrap()
}
#[test]
fn exact_locator_signature_and_independent_host_pin_are_required() {
    let (key, host) = fixture();
    let value = DeliveryOriginLocatorV1 {
        version: 1,
        origin: "https://127.0.0.1:9443".into(),
    };
    let hint = value.transport_hint(&host, &key).unwrap();
    assert_eq!(
        hint.opaque_json,
        r#"{"version":1,"origin":"https://127.0.0.1:9443"}"#
    );
    assert_eq!(
        DeliveryOriginLocatorV1::from_transport_hint(&hint, &host).unwrap(),
        value
    );
    for case in 0..5 {
        let mut altered = hint.clone();
        let mut pin = host.clone();
        match case {
            0 => altered.kind.push('x'),
            1 => altered.opaque_json = altered.opaque_json.replace("9443", "9444"),
            2 => altered.service_public_key = [99; 32],
            3 => pin.device_label.push('x'),
            _ => altered.host.device_label.push('x'),
        }
        assert!(
            DeliveryOriginLocatorV1::from_transport_hint(&altered, &pin).is_err(),
            "case {case}"
        );
    }
    // These have valid host signatures: reject the unsupported kind or service
    // appointment itself, not merely a mutation invalidating the signature.
    let other_kind = sign_transport_hint(
        "tilo-delivery-origin-v2",
        &hint.opaque_json,
        host.clone(),
        host.device_public_key,
        &key,
    )
    .unwrap();
    assert!(DeliveryOriginLocatorV1::from_transport_hint(&other_kind, &host).is_err());
    let other_service = sign_transport_hint(
        TRANSPORT_DELIVERY_ORIGIN_V1,
        &hint.opaque_json,
        host.clone(),
        [99; 32],
        &key,
    )
    .unwrap();
    assert!(DeliveryOriginLocatorV1::from_transport_hint(&other_service, &host).is_err());
    assert!(
        value
            .transport_hint(&host, &SigningKey::from_bytes(&[43; 32]))
            .is_err()
    );
}
#[test]
fn signed_noncanonical_or_ambiguous_locator_payloads_fail_closed() {
    let (key, host) = fixture();
    for payload in [
        r#"{"origin":"https://localhost:9443","version":1}"#,
        r#"{ "version":1,"origin":"https://localhost:9443"}"#,
        r#"{"version":1,"origin":"https://localhost:9443","extra":0}"#,
        r#"{"version":2,"origin":"https://localhost:9443"}"#,
        r#"{"version":1,"version":1,"origin":"https://localhost:9443"}"#,
    ] {
        assert!(
            DeliveryOriginLocatorV1::from_transport_hint(&signed(payload, &key, &host), &host)
                .is_err()
        );
    }
    for origin in [
        "http://localhost:9443",
        "https://user@localhost:9443",
        "https://localhost:9443/",
        "https://localhost:9443?q=1",
        "https://localhost:9443#x",
        "https://LOCALHOST:9443",
        "https://localhost:0",
        "https://localhost:65536",
        "https://localhost:9443\\x",
    ] {
        // Explicit declaration order; rejection must be the origin, not reordered JSON.
        let payload = format!(
            "{{\"version\":1,\"origin\":{}}}",
            serde_json::to_string(&origin).unwrap()
        );
        assert!(
            DeliveryOriginLocatorV1::from_transport_hint(&signed(&payload, &key, &host), &host)
                .is_err(),
            "{origin}"
        );
    }
    let oversized = format!(
        "{{\"version\":1,\"origin\":\"https://{}\"}}",
        "a".repeat(513)
    );
    assert!(
        DeliveryOriginLocatorV1::from_transport_hint(&signed(&oversized, &key, &host), &host)
            .is_err()
    );
}
