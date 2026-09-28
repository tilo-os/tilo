//! Ordinary governance JSON compatibility fixtures. These are not allocation
//! qualification or authority admission; later guarded tests reuse their data.

use super::*;

#[expect(
    clippy::similar_names,
    reason = "`root` and `room` are the domain's own names"
)]
pub(crate) fn identity_fixture() -> (GovernedIdentity, SystemTime) {
    let (peer, owner_key, _, delegate, now) = crate::admission_tests::seeded();
    let room = peer.admitted_room().unwrap();
    let identities = room.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
    let (value, _) = room
        .get(&identities, hex::encode(peer.authority().device_public_key))
        .unwrap()
        .unwrap();
    let automerge::Value::Scalar(value) = value else {
        panic!("actual seeded identity is not scalar")
    };
    let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
        panic!("actual seeded identity is not bytes")
    };
    let mut identity: GovernedIdentity = serde_json::from_slice(bytes).unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    identity.device_cert.device_label = "Desk 🦀\n\"Room\"\\".into();
    identity.device_cert.expires_at_ns = Some(u64::MAX);
    tilo_user_identity::sign_device_cert(&mut identity.device_cert, &root).unwrap();
    let mut revocation = tilo_user_identity::RevocationEntry {
        user_public_key: identity.device_cert.user_public_key,
        revoked_device_public_key: [99; 32],
        revoked_at_ns: 7,
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
    identity.revocations.push(revocation);
    identity.workspace_delegations.push(
        crate::join::sign_workspace_delegation(
            peer.room(),
            u64::MAX,
            &identity.device_cert,
            delegate,
            crate::join::WorkspaceDelegationPermissions {
                issue_invitations: true,
                admit_invited_members: true,
            },
            [6; 16],
            Some(u64::MAX),
            &owner_key,
        )
        .unwrap(),
    );
    (identity, now)
}

pub(crate) fn capability_fixture() -> CapOverride {
    CapOverride::grant(
        Subject::Principal(Principal::Person("person 🦀\n\"named\"\\".into())),
        Scope::Action {
            app: "so.tilo.chat".into(),
            action: "message.\u{0001}send".into(),
        },
        Verb::Write,
    )
    .with_expiry(u64::MAX)
}

fn verify_identity(identity: &GovernedIdentity, now: SystemTime) {
    tilo_user_identity::verify_device_cert_at(&identity.device_cert, now_ns(now)).unwrap();
    for entry in &identity.revocations {
        tilo_user_identity::verify_revocation_entry(entry).unwrap();
    }
    for delegation in &identity.workspace_delegations {
        crate::join::verify_workspace_delegation(
            delegation,
            &identity.device_cert,
            &RevocationList::new(),
            now_ns(now),
        )
        .unwrap();
    }
}

#[test]
fn json_resource_fixture_signed_nested_identity_roundtrip_preserves_canonical_bytes() {
    let (identity, now) = identity_fixture();
    verify_identity(&identity, now);
    let wire = serde_json::to_vec(&identity).unwrap();
    let decoded: GovernedIdentity = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        postcard::to_allocvec(&decoded).unwrap(),
        postcard::to_allocvec(&identity).unwrap()
    );
    verify_identity(&decoded, now);
}

#[test]
fn json_resource_fixture_escaped_keys_and_surrogate_values_preserve_signed_identity() {
    let (identity, now) = identity_fixture();
    let ordinary = serde_json::to_string(&identity).unwrap();
    let escaped = ordinary
        .replace("\"device_cert\"", "\"device_\\u0063ert\"")
        .replace("\"device_label\"", "\"device_\\u006cabel\"")
        .replace('🦀', "\\uD83E\\uDD80");
    assert_ne!(escaped, ordinary);
    let decoded: GovernedIdentity = serde_json::from_str(&escaped).unwrap();
    assert_eq!(
        postcard::to_allocvec(&decoded).unwrap(),
        postcard::to_allocvec(&identity).unwrap()
    );
    verify_identity(&decoded, now);
}

#[test]
fn json_resource_fixture_capability_control_utf8_and_maximum_u64_roundtrip() {
    let capability = capability_fixture();
    let wire = serde_json::to_string(&capability).unwrap();
    assert!(wire.contains("\\u0001"));
    assert!(wire.contains("18446744073709551615"));
    let escaped = wire.replace('🦀', "\\uD83E\\uDD80");
    let decoded: CapOverride = serde_json::from_str(&escaped).unwrap();
    assert_eq!(decoded, capability);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), wire);
}

#[test]
fn json_resource_fixture_ignored_extension_keeps_ordinary_numeric_behavior() {
    let (identity, now) = identity_fixture();
    let mut wire = serde_json::to_string(&identity).unwrap();
    assert_eq!(wire.pop(), Some('}'));
    wire.push_str(
        ",\"future\":{\"small_decimal\":0.1000000000000000001,\"nested\":[true,null,1e-64]}}",
    );
    let decoded: GovernedIdentity = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        postcard::to_allocvec(&decoded).unwrap(),
        postcard::to_allocvec(&identity).unwrap()
    );
    verify_identity(&decoded, now);
}
