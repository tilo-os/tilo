use super::*;

#[test]
fn network_wake_wire_is_bounded_and_cannot_carry_commands() {
    let command = WifiContinuityCommand::NetworkWake {
        profile_uuid: "11111111-1111-4111-8111-111111111111".into(),
        enabled: Some(true),
    };
    assert!(command.valid());
    let wire = serde_json::to_value(&command).unwrap();
    assert_eq!(
        serde_json::from_value::<WifiContinuityCommand>(wire.clone()).unwrap(),
        command
    );
    let mut injected = wire;
    injected["program"] = serde_json::json!("unrelated-program");
    assert!(serde_json::from_value::<WifiContinuityCommand>(injected).is_err());
    let mut status = NetworkWakeStatus {
        profile: "11111111-1111-4111-8111-111111111111".into(),
        interface: Some("wlp1s0".into()),
        configured: true,
        supported: Some(true),
        applied: Some(true),
        problem: None,
    };
    assert!(
        WifiContinuityReply::NetworkWake {
            status: status.clone()
        }
        .valid()
    );
    status.problem = Some("x".repeat(513));
    assert!(!WifiContinuityReply::NetworkWake { status }.valid());
}

#[test]
fn certified_join_round_trips_and_prefixed_garbage_never_downgrades() {
    let identity = CertifiedJoinIdentity {
        workspace_id: "workspace-a".into(),
        device_certificate: vec![1, 2, 3],
        proof: vec![4; 64],
    };
    let encoded = identity.encode().expect("encode");
    assert_eq!(
        CertifiedJoinIdentity::decode(&encoded).expect("decode"),
        Some(identity)
    );
    assert_eq!(
        CertifiedJoinIdentity::decode(b"uncertified identity").expect("uncertified bytes"),
        None
    );
    let mut malformed = CERTIFIED_JOIN_PREFIX.to_vec();
    malformed.extend_from_slice(b"not postcard");
    assert!(CertifiedJoinIdentity::decode(&malformed).is_err());
}
