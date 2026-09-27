use super::*;
use crate::peer_cloud::owner::tests::{ACCOUNT, NOW, enrolled_owner_for};
use ed25519_dalek::SigningKey;
use tilo_user_identity::machine_mesh::{BROKER_ACTIONS_SCOPE, verify_request};

const AT: u64 = NOW + 10;

struct Machine {
    id: &'static str,
    material: Vec<u8>,
    challenge: String,
}
fn machine(id: &'static str, host: u8, nonce: u8) -> Machine {
    let (owner, material) = enrolled_owner_for(id, &SigningKey::from_bytes(&[host; 32]));
    let scope = owner.enrolled().unwrap();
    let challenge = MeshChallenge {
        version: 1,
        machine_id: id.into(),
        room: scope.room().into(),
        root_public_key: scope.host().user_public_key,
        host_certificate: scope.host().clone(),
        action_address: "{}".into(),
        nonce: [nonce; 32],
        expires_at_ns: AT + 60_000_000_000,
    };
    Machine {
        id,
        material,
        challenge: serde_json::to_string(&challenge).unwrap(),
    }
}
fn side(machine: &Machine) -> MeshSide<'_> {
    MeshSide {
        machine_id: machine.id,
        material: &machine.material,
    }
}
fn signed_link(a: &Machine, b: &Machine) -> String {
    link(
        ACCOUNT,
        &side(a),
        &side(b),
        &a.challenge,
        &b.challenge,
        "link-1",
        AT,
    )
    .unwrap()
}

#[test]
fn link_is_signed_by_both_enrolled_roots_and_carries_no_private_material() {
    let (a, b) = (machine("m-a", 91, 1), machine("m-b", 93, 2));
    let payload = signed_link(&a, &b);
    let request: MeshRequest = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        (request.kind.as_str(), request.link_id.as_str()),
        ("link", "link-1")
    );
    assert_eq!(request.allowed_actions, [BROKER_ACTIONS_SCOPE]);
    verify_request(&request, AT).unwrap();
    assert!(!payload.contains("secret") && !payload.contains("material"));
}

#[test]
fn unlink_reuses_the_grant_orientation_and_verifies() {
    let (a, b) = (machine("m-a", 91, 1), machine("m-b", 93, 2));
    let grant = signed_link(&a, &b);
    let payload = unlink(ACCOUNT, &side(&a), &side(&b), &grant, AT).unwrap();
    let request: MeshRequest = serde_json::from_str(&payload).unwrap();
    let original: MeshRequest = serde_json::from_str(&grant).unwrap();
    assert_eq!(request.kind, "unlink");
    assert_eq!(request.link_id, original.link_id);
    assert_eq!(request.source, original.source);
    assert_ne!(request.source_signature, original.source_signature);
    verify_request(&request, AT).unwrap();
    // The grant's orientation is binding: swapped sides refuse.
    assert!(unlink(ACCOUNT, &side(&b), &side(&a), &grant, AT).is_err());
    // Only a link grant can be unlinked.
    assert!(unlink(ACCOUNT, &side(&a), &side(&b), &payload, AT).is_err());
}

#[test]
fn foreign_machines_accounts_and_unenrolled_custody_refuse() {
    let (a, b) = (machine("m-a", 91, 1), machine("m-b", 93, 2));
    // Challenges swapped relative to the selected machines.
    assert!(
        link(
            ACCOUNT,
            &side(&a),
            &side(&b),
            &b.challenge,
            &a.challenge,
            "l",
            AT
        )
        .is_err()
    );
    // Same machine on both sides.
    assert!(
        link(
            ACCOUNT,
            &side(&a),
            &side(&a),
            &a.challenge,
            &a.challenge,
            "l",
            AT
        )
        .is_err()
    );
    // Custody of another account.
    assert!(
        link(
            "other",
            &side(&a),
            &side(&b),
            &a.challenge,
            &b.challenge,
            "l",
            AT
        )
        .is_err()
    );
    // Material of machine `a` offered as machine `b`'s custody.
    let impostor = MeshSide {
        machine_id: b.id,
        material: &a.material,
    };
    assert!(
        link(
            ACCOUNT,
            &side(&a),
            &impostor,
            &a.challenge,
            &b.challenge,
            "l",
            AT
        )
        .is_err()
    );
    // A fresh, never enrolled identity cannot approve a link.
    let fresh = crate::peer_cloud::create(ACCOUNT, b.id, "Owner browser", NOW).unwrap();
    let error = link(
        ACCOUNT,
        &side(&a),
        &MeshSide {
            machine_id: b.id,
            material: &fresh,
        },
        &a.challenge,
        &b.challenge,
        "l",
        AT,
    )
    .unwrap_err();
    assert!(error.contains("not enrolled"), "{error}");
    // Invalid link ids and oversized challenges.
    for id in ["", "a b", &"x".repeat(129)] {
        assert!(
            link(
                ACCOUNT,
                &side(&a),
                &side(&b),
                &a.challenge,
                &b.challenge,
                id,
                AT
            )
            .is_err()
        );
    }
    let huge = "x".repeat(MAX_MESH_JSON + 1);
    assert!(link(ACCOUNT, &side(&a), &side(&b), &huge, &b.challenge, "l", AT).is_err());
}
