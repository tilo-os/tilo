use super::protocol::*;
use super::{identity, relay};
use automerge::resource_limits::NativeAllowance;
use ed25519_dalek::SigningKey;

fn selection() -> Selection {
    Selection {
        account_id: "account".into(),
        machine_id: "machine".into(),
    }
}
fn material() -> Vec<u8> {
    let root = SigningKey::from_bytes(&[71; 32]);
    let device = SigningKey::from_bytes(&[72; 32]);
    let mut certificate = identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Browser".into(),
        capabilities: identity::DeviceCapabilities::member(),
        issued_at_ns: 10,
        expires_at_ns: None,
        signature: [0; 64],
    };
    identity::sign_device_cert(&mut certificate, &root).unwrap();
    serde_json::to_vec(&serde_json::json!({"version":4,"kind":"cloud-owner","account_id":"account","machine_id":"machine","root_secret":root.to_bytes(),"device_secret":device.to_bytes(),"attempt_id":([73u8;32]),"created_at_ns":10,"certificate":certificate,"phase":{"step":"initial"}})).unwrap()
}
fn challenge() -> identity::cloud_enrollment::Challenge {
    let root = SigningKey::from_bytes(&[71; 32]);
    let host = SigningKey::from_bytes(&[74; 32]);
    let mut certificate = identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: host.verifying_key().to_bytes(),
        device_label: "Host".into(),
        capabilities: identity::cloud_enrollment::host_capabilities(),
        issued_at_ns: 10,
        expires_at_ns: None,
        signature: [0; 64],
    };
    identity::sign_device_cert(&mut certificate, &root).unwrap();
    let room = "6d3c0042-a32d-4c3d-9616-03e621da0f47";
    identity::cloud_enrollment::Challenge {
        version: 1,
        machine_id: "machine".into(),
        room: room.into(),
        workspace_id: relay::room_workspace_id(room).unwrap().to_string(),
        host_public_key: host.verifying_key().to_bytes(),
        nonce: [75; 32],
        expires_at_ns: 300_000_000_100,
        root_public_key: Some(root.verifying_key().to_bytes()),
        host_certificate: Some(certificate),
    }
}

#[test]
fn hosted_errors_are_fixed_inline_and_never_contain_material() {
    assert!(std::mem::size_of::<HostedAttemptError>() <= 1);
    let budget = NativeAllowance::new(8 * 1024 * 1024).unwrap();
    let before = budget.available_bytes();
    assert!(
        HostedAttempt::restore(
            b"secret",
            Selection {
                account_id: "account".into(),
                machine_id: "machine".into()
            },
            &budget,
            10
        )
        .is_err()
    );
    assert_eq!(budget.available_bytes(), before);
}

#[test]
fn hosted_attempt_owns_original_allowance_through_parse_stage_ack_and_drop() {
    let budget = NativeAllowance::new(32 * 1024 * 1024).unwrap();
    let before = budget.available_bytes();
    let mut attempt = HostedAttempt::restore(&material(), selection(), &budget, 100).unwrap();
    let parsed = budget.available_bytes();
    assert!(parsed < before);
    let saved = attempt
        .prepare_enrollment(&serde_json::to_vec(&challenge()).unwrap(), 100)
        .unwrap()
        .material
        .to_vec();
    assert!(budget.available_bytes() < parsed);
    attempt.acknowledge(&saved, 101).unwrap();
    assert!(budget.available_bytes() < before);
    attempt.cancel();
    assert!(
        budget.available_bytes() < before,
        "cancel retains owned backing until caller releases attempt"
    );
    drop(attempt);
    assert_eq!(budget.available_bytes(), before);
}

#[test]
fn hosted_checkpoint_is_exact_and_does_not_fresh_sign_after_expiry() {
    let budget = NativeAllowance::new(32 * 1024 * 1024).unwrap();
    let initial = material();
    let mut attempt = HostedAttempt::restore(&initial, selection(), &budget, 100).unwrap();
    let request = challenge();
    let bytes = serde_json::to_vec(&request).unwrap();
    let saved = attempt
        .prepare_enrollment(&bytes, 100)
        .unwrap()
        .material
        .to_vec();
    assert_eq!(
        attempt
            .prepare_enrollment(&bytes, request.expires_at_ns + 1)
            .unwrap()
            .material,
        saved
    );
    assert!(attempt.acknowledge(&initial, 101).is_err());
    assert!(attempt.apply_request().is_err());
    attempt
        .acknowledge(&saved, request.expires_at_ns + 1)
        .unwrap();
    let expected = attempt.apply_request().unwrap().payload.to_vec();
    let restored =
        HostedAttempt::restore(&saved, selection(), &budget, request.expires_at_ns + 2).unwrap();
    assert_eq!(restored.apply_request().unwrap().payload, expected);
    let mut old =
        HostedAttempt::restore(&initial, selection(), &budget, request.expires_at_ns + 1).unwrap();
    assert!(
        old.prepare_enrollment(&bytes, request.expires_at_ns + 1)
            .is_err()
    );
    assert!(old.apply_request().is_err());
}

#[test]
fn hosted_restore_rejects_corrupt_identity_and_legacy_tags() {
    let budget = NativeAllowance::new(32 * 1024 * 1024).unwrap();
    let initial = budget.available_bytes();
    let original: serde_json::Value = serde_json::from_slice(&material()).unwrap();
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("version", serde_json::json!(3)),
        ("kind", serde_json::json!("invited")),
        ("account_id", serde_json::json!("other")),
        ("machine_id", serde_json::json!("other")),
        ("root_secret", serde_json::json!(([0u8; 32]))),
        ("device_secret", serde_json::json!(([0u8; 32]))),
        ("attempt_id", serde_json::json!(([0u8; 32]))),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(
            HostedAttempt::restore(
                &serde_json::to_vec(&changed).unwrap(),
                selection(),
                &budget,
                100
            )
            .is_err(),
            "{field}"
        );
    }
    let mut changed = original;
    changed["certificate"]["signature"][0] =
        serde_json::json!(changed["certificate"]["signature"][0].as_u64().unwrap() ^ 1);
    assert!(
        HostedAttempt::restore(
            &serde_json::to_vec(&changed).unwrap(),
            selection(),
            &budget,
            100
        )
        .is_err()
    );
    assert_eq!(budget.available_bytes(), initial);
}

#[test]
fn hosted_challenge_has_no_first_claim_or_scope_substitution() {
    let budget = NativeAllowance::new(32 * 1024 * 1024).unwrap();
    let original = challenge();
    let mut changes = Vec::new();
    let mut changed = original.clone();
    changed.root_public_key = None;
    changes.push(changed);
    let mut changed = original.clone();
    changed.host_certificate = None;
    changes.push(changed);
    let mut changed = original.clone();
    changed.workspace_id = "7".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.room = "invalid".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.nonce = [0; 32];
    changes.push(changed);
    let mut changed = original.clone();
    changed.host_public_key = [9; 32];
    changes.push(changed);
    let mut changed = original;
    changed.host_certificate.as_mut().unwrap().signature[0] ^= 1;
    changes.push(changed);
    for changed in changes {
        let mut attempt = HostedAttempt::restore(&material(), selection(), &budget, 100).unwrap();
        assert!(
            attempt
                .prepare_enrollment(&serde_json::to_vec(&changed).unwrap(), 100)
                .is_err()
        );
        assert!(attempt.apply_request().is_err());
    }
}

#[test]
fn hosted_bounds_precede_decode_and_pressure_does_not_reset_allowance() {
    let budget = NativeAllowance::new(64).unwrap();
    let initial = budget.available_bytes();
    let oversized = vec![b' '; 1024 * 1024 + 1];
    assert!(matches!(
        HostedAttempt::restore(&oversized, selection(), &budget, 100),
        Err(HostedAttemptError::Material)
    ));
    assert!(matches!(
        HostedAttempt::restore(&material(), selection(), &budget, 100),
        Err(HostedAttemptError::Capacity)
    ));
    assert_eq!(budget.available_bytes(), initial);
    let budget = NativeAllowance::new(5 * 1024 * 1024).unwrap();
    let initial = budget.available_bytes();
    let mut attempt = HostedAttempt::restore(&material(), selection(), &budget, 100).unwrap();
    let retained = budget.available_bytes();
    assert!(matches!(
        attempt.prepare_enrollment(&serde_json::to_vec(&challenge()).unwrap(), 100),
        Err(HostedAttemptError::Capacity)
    ));
    assert_eq!(budget.available_bytes(), retained);
    assert!(attempt.apply_request().is_err());
    drop(attempt);
    assert_eq!(budget.available_bytes(), initial);
}

#[test]
fn hosted_material_and_challenge_exact_limits_are_accepted_then_rejected() {
    let budget = NativeAllowance::new(32 * 1024 * 1024).unwrap();
    let mut full = material();
    full.resize(1024 * 1024, b' ');
    let mut attempt = HostedAttempt::restore(&full, selection(), &budget, 100).unwrap();
    full.push(b' ');
    assert!(HostedAttempt::restore(&full, selection(), &budget, 100).is_err());
    let mut input = serde_json::to_vec(&challenge()).unwrap();
    input.resize(12 * 1024 + 1, b' ');
    assert!(attempt.prepare_enrollment(&input, 100).is_err());
    input.pop();
    let saved = attempt
        .prepare_enrollment(&input, 100)
        .unwrap()
        .material
        .to_vec();
    attempt.acknowledge(&saved, 100).unwrap();
    assert!(attempt.apply_request().is_ok());
}
