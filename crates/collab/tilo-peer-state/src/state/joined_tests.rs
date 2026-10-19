//! A replica built from a join's answer takes the room's roster as the
//! room's evidence: a guest whose certificate expired since is still in it,
//! and is still refused whenever it acts.
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";

fn certificate(root: &SigningKey, device: &SigningKey, expires_at_ns: Option<u64>) -> DeviceCert {
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "device".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, root).unwrap();
    certificate
}

#[test]
fn a_joined_replica_keeps_a_guest_whose_certificate_expired_and_refuses_it_still() {
    let now = SystemTime::now();
    let host_root = SigningKey::from_bytes(&[1; 32]);
    let authority = certificate(&host_root, &SigningKey::from_bytes(&[2; 32]), None);
    // A watching page's device, certified for an hour that has passed.
    let guest_device = SigningKey::from_bytes(&[4; 32]);
    let guest = certificate(
        &SigningKey::from_bytes(&[3; 32]),
        &guest_device,
        Some(now_ns(now) - 1),
    );
    let member = certificate(
        &SigningKey::from_bytes(&[5; 32]),
        &SigningKey::from_bytes(&[6; 32]),
        None,
    );
    let roster = vec![authority.clone(), guest.clone(), member.clone()];

    assert!(
        StatePeer::new(
            ROOM,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now
        )
        .is_err(),
        "a replica the device builds itself admits only current certificates"
    );
    let peer = StatePeer::joined(
        ROOM,
        authority.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .expect("the room's roster is its evidence, an expired guest included");
    assert!(
        peer.certificate(&guest.device_public_key, now).is_err(),
        "the expired guest's expiry is still checked when it is used"
    );
    assert_eq!(
        peer.certificate(&member.device_public_key, now),
        Ok(&member)
    );
}

#[test]
fn a_joined_replica_refuses_an_expired_authority_or_a_forged_roster_certificate() {
    let now = SystemTime::now();
    let host_root = SigningKey::from_bytes(&[1; 32]);
    let host_device = SigningKey::from_bytes(&[2; 32]);
    let expired_authority = certificate(&host_root, &host_device, Some(now_ns(now) - 1));
    assert!(
        StatePeer::joined(
            ROOM,
            expired_authority.clone(),
            vec![expired_authority],
            RevocationList::new(),
            now,
        )
        .is_err(),
        "the governing authority must be valid now"
    );

    let authority = certificate(&host_root, &host_device, None);
    let mut forged = certificate(
        &SigningKey::from_bytes(&[3; 32]),
        &SigningKey::from_bytes(&[4; 32]),
        Some(now_ns(now) - 1),
    );
    forged.device_label = "someone else".into();
    assert!(
        StatePeer::joined(
            ROOM,
            authority.clone(),
            vec![authority.clone(), forged],
            RevocationList::new(),
            now,
        )
        .is_err(),
        "a roster certificate's signature is checked"
    );
}
