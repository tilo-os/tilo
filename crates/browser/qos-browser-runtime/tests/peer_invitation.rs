//! Isolated invitation owner tests while the reviewed WASM entrypoint is frozen.
#[path = "../src/peer_invitation.rs"]
mod peer_invitation;

#[path = "../src/identity.rs"]
mod enrollment_identity;

#[test]
fn literal_v2_enrollment_material_remains_accepted_only_by_the_existing_owner_source() {
    let legacy = br#"{"version":2,"secret":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7],"request_id":[9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9],"scope":null}"#;
    let request: serde_json::Value =
        serde_json::from_str(&enrollment_identity::request(legacy).unwrap()).unwrap();
    assert_eq!(
        request["device_public_key"],
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    assert_eq!(
        request["request_id"],
        "0909090909090909090909090909090909090909090909090909090909090909"
    );
    assert!(peer_invitation::InvitedIdentity::read(legacy, 1_000_000_000_000).is_err());
}
