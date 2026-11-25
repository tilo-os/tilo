use super::*;
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ScalarValue};
use ed25519_dalek::SigningKey;

pub(crate) const WORKSPACE: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
const OTHER_WORKSPACE: &str = "5e1d9a40-3c2b-4f6e-8a7d-1b2c3d4e5f60";
/// Within every fixture certificate's validity.
pub(crate) const NOW_NS: u64 = 1_000_000_000_000;

/// One person's device: its root, its certificate and its key.
pub(crate) struct Device {
    pub(crate) root: SigningKey,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
}

impl Device {
    pub(crate) fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }

    /// `shortcut` as this device saves it in `workspace`.
    pub(crate) fn saves(&self, workspace: &str, mut shortcut: Shortcut) -> Shortcut {
        sign_saved_shortcut(workspace, &mut shortcut, &self.cert, &self.key).unwrap();
        shortcut
    }
}

/// A device of the person with root seed `root`, its own key from `device`.
pub(crate) fn device(root: u8, device: u8) -> Device {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[device; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Test device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    Device { root, cert, key }
}

pub(crate) fn shortcut(id: &str) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

fn revoked(device: &Device) -> tilo_user_identity::RevocationEntry {
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: device.cert.user_public_key,
        revoked_device_public_key: device.cert.device_public_key,
        revoked_at_ns: NOW_NS,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &device.root).unwrap();
    entry
}

/// One row per reason a shortcut names, or does not name, who saved it: the
/// saver is proven only by their own device's signature over the shortcut as
/// it stands, in the workspace it was saved in, under a certificate that is
/// valid and not revoked at that instant.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one row per reason: the table stays in one place"
)]
fn a_shortcut_proves_its_saver_only_by_their_own_valid_device_in_its_workspace() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let none = RevocationList::new();
    let saved = ada.saves(WORKSPACE, shortcut("water"));

    let mut unproven = shortcut("water");
    unproven.saved_by = Some(ada.person());
    // Bob's device signing a shortcut that names Ada.
    let mut forged = bob.saves(WORKSPACE, shortcut("water"));
    forged.saved_by = Some(ada.person());
    // Ada's proof on a shortcut changed after she saved it.
    let mut changed = ada.saves(WORKSPACE, shortcut("water"));
    changed.name = "Something else".into();

    let mut no_ada = RevocationList::new();
    no_ada.insert(revoked(&ada)).unwrap();
    let mut no_bob = RevocationList::new();
    no_bob.insert(revoked(&bob)).unwrap();

    // Ada's device under a certificate that expires at `NOW_NS`.
    let mut expiring = device(1, 2);
    expiring.cert.expires_at_ns = Some(NOW_NS);
    tilo_user_identity::sign_device_cert(&mut expiring.cert, &expiring.root).unwrap();
    let brief = expiring.saves(WORKSPACE, shortcut("water"));

    // Ada's proof with the last byte of its signature missing.
    let mut cut = ada.saves(WORKSPACE, shortcut("water"));
    cut.saved_proof.as_mut().unwrap().signature.truncate(126);
    // Ada's proof with no certificate to read.
    let mut unreadable = ada.saves(WORKSPACE, shortcut("water"));
    unreadable.saved_proof.as_mut().unwrap().device_cert = serde_json::Value::Null;
    // Bob's key signing the shortcut as Ada would have saved it, under her
    // certificate.
    let mut stolen = shortcut("water");
    stolen.saved_by = Some(ada.person());
    let signature =
        collab_types::signer::sign_now(&bob.key, stolen.saved_signing_digest(WORKSPACE).to_vec())
            .unwrap();
    stolen.saved_proof = Some(tilo_automation::SavedProof {
        device_cert: serde_json::to_value(&ada.cert).unwrap(),
        signature: hex::encode(signature),
    });

    for (case, workspace, subject, revocations, now_ns, expected) in [
        (
            "the saver's own device",
            WORKSPACE,
            &saved,
            &none,
            NOW_NS,
            Some(ada.person()),
        ),
        (
            "named without a proof",
            WORKSPACE,
            &unproven,
            &none,
            NOW_NS,
            None,
        ),
        (
            "signed by another's device",
            WORKSPACE,
            &forged,
            &none,
            NOW_NS,
            None,
        ),
        (
            "changed after it was saved",
            WORKSPACE,
            &changed,
            &none,
            NOW_NS,
            None,
        ),
        (
            "copied to another workspace",
            OTHER_WORKSPACE,
            &saved,
            &none,
            NOW_NS,
            None,
        ),
        (
            "its device revoked since",
            WORKSPACE,
            &saved,
            &no_ada,
            NOW_NS,
            None,
        ),
        (
            "another's device revoked",
            WORKSPACE,
            &saved,
            &no_bob,
            NOW_NS,
            Some(ada.person()),
        ),
        (
            "a nanosecond before expiry",
            WORKSPACE,
            &brief,
            &none,
            NOW_NS - 1,
            Some(ada.person()),
        ),
        (
            "at the instant of expiry",
            WORKSPACE,
            &brief,
            &none,
            NOW_NS,
            None,
        ),
        (
            "before its certificate was issued",
            WORKSPACE,
            &saved,
            &none,
            0,
            None,
        ),
        (
            "at the instant its certificate was issued",
            WORKSPACE,
            &saved,
            &none,
            1,
            Some(ada.person()),
        ),
        (
            "a signature cut short",
            WORKSPACE,
            &cut,
            &none,
            NOW_NS,
            None,
        ),
        (
            "a certificate that does not read",
            WORKSPACE,
            &unreadable,
            &none,
            NOW_NS,
            None,
        ),
        (
            "another's key under the saver's certificate",
            WORKSPACE,
            &stolen,
            &none,
            NOW_NS,
            None,
        ),
    ] {
        assert_eq!(
            proven_saver(workspace, subject, revocations, now_ns),
            expected,
            "{case}"
        );
    }
}

#[test]
fn the_workspaces_revocations_are_those_its_identity_rows_carry() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let mut doc = AutoCommit::new();
    let identities = doc
        .put_object(ROOT, "peer_identities_v1", ObjType::Map)
        .unwrap();
    // Ada's row revokes her device; Bob's row cannot revoke Ada's.
    let mut forged = revoked(&ada);
    forged.user_public_key = bob.cert.user_public_key;
    for (device, revocations) in [(&ada, vec![revoked(&ada)]), (&bob, vec![forged])] {
        let row = crate::state::DeviceRow {
            device_cert: device.cert.clone(),
            revocations,
            ..crate::state::DeviceRow::default()
        };
        doc.put(
            &identities,
            hex::encode(device.cert.device_public_key),
            ScalarValue::Bytes(row.encode().unwrap()),
        )
        .unwrap();
    }
    let revocations = workspace_revocations(&doc);
    assert!(revocations.is_revoked(&ada.cert.user_public_key, &ada.cert.device_public_key));
    assert_eq!(revocations.len(), 1);
}

/// The grant `root`'s person certified for `agent` in `workspace`, with key seed `key`.
fn agent_grant(
    root: &SigningKey,
    key: u8,
    agent: &str,
    workspace: &str,
) -> (DeviceCert, SigningKey) {
    let key = SigningKey::from_bytes(&[key; 32]);
    let workspace = *uuid::Uuid::parse_str(workspace).unwrap().as_bytes();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "An agent's grant".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::agent_in(workspace, agent),
        ),
        issued_at_ns: 1,
        expires_at_ns: Some(tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    (cert, key)
}

/// An agent working away from its person's devices saves with its own grant: the shortcut is
/// its person's, acting as that agent. The grant proves nothing as the person, nothing for
/// another agent, and nothing in a workspace it does not reach.
#[test]
fn an_agents_grant_proves_a_shortcut_saved_as_that_agent_and_never_as_its_person() {
    let ada = device(1, 2);
    let none = RevocationList::new();
    let (grant, key) = agent_grant(&ada.root, 3, "scout", WORKSPACE);

    let mut saved = shortcut("brief");
    sign_saved_shortcut_as_agent(WORKSPACE, &mut saved, "scout", &grant, &key).unwrap();
    assert_eq!(
        proven(WORKSPACE, &saved, &none, NOW_NS),
        Some(Saver {
            person: ada.person(),
            agent: Some("scout".into()),
            key: grant.device_public_key,
        })
    );
    assert_eq!(
        proven_saver(WORKSPACE, &saved, &none, NOW_NS),
        Some(ada.person())
    );
    assert_eq!(
        proven(
            WORKSPACE,
            &ada.saves(WORKSPACE, shortcut("brief")),
            &none,
            NOW_NS
        )
        .and_then(|saver| saver.agent),
        None,
        "a device's own proof acts as its person"
    );
    assert_eq!(proven(OTHER_WORKSPACE, &saved, &none, NOW_NS), None);

    // Signed as a device would sign, the grant is no device of the person's.
    let mut as_person = shortcut("brief");
    sign_saved_shortcut(WORKSPACE, &mut as_person, &grant, &key).unwrap();
    assert_eq!(proven(WORKSPACE, &as_person, &none, NOW_NS), None);

    // Another agent's grant cannot sign for this one, nor one for another workspace.
    let (other, other_key) = agent_grant(&ada.root, 4, "courier", WORKSPACE);
    assert!(
        sign_saved_shortcut_as_agent(WORKSPACE, &mut shortcut("x"), "scout", &other, &other_key)
            .is_err()
    );
    let (elsewhere, elsewhere_key) = agent_grant(&ada.root, 5, "scout", OTHER_WORKSPACE);
    assert!(
        sign_saved_shortcut_as_agent(
            WORKSPACE,
            &mut shortcut("x"),
            "scout",
            &elsewhere,
            &elsewhere_key
        )
        .is_err()
    );

    // The agent named beside the grant is part of what holds: naming another breaks it.
    let mut renamed = saved.clone();
    if let Some(proof) = renamed.saved_proof.as_mut() {
        proof.device_cert["agent"] = serde_json::json!("courier");
    }
    assert_eq!(proven(WORKSPACE, &renamed, &none, NOW_NS), None);
}
