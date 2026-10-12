use super::*;

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
