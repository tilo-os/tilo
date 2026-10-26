use super::*;
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ScalarValue};

pub(crate) const ROOM: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
const OTHER_ROOM: &str = "5e1d9a40-3c2b-4f6e-8a7d-1b2c3d4e5f60";
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

    /// `shortcut` as this device saves it in `room`.
    pub(crate) fn saves(&self, room: &str, mut shortcut: Shortcut) -> Shortcut {
        sign_saved_shortcut(room, &mut shortcut, &self.cert, &self.key).unwrap();
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

#[test]
fn the_savers_own_device_proves_who_saved_it() {
    let ada = device(1, 2);
    let saved = ada.saves(ROOM, shortcut("water"));
    assert_eq!(
        proven_saver(ROOM, &saved, &RevocationList::new(), NOW_NS),
        Some(ada.person())
    );
}

#[test]
fn a_named_saver_without_their_devices_proof_is_no_saver() {
    let ada = device(1, 2);
    let bob = device(3, 4);
    let none = RevocationList::new();

    let mut unproven = shortcut("water");
    unproven.saved_by = Some(ada.person());
    assert_eq!(proven_saver(ROOM, &unproven, &none, NOW_NS), None);

    // Bob's device signing a shortcut that names Ada.
    let mut forged = bob.saves(ROOM, shortcut("water"));
    forged.saved_by = Some(ada.person());
    assert_eq!(proven_saver(ROOM, &forged, &none, NOW_NS), None);

    // Ada's proof on a shortcut changed after she saved it.
    let mut changed = ada.saves(ROOM, shortcut("water"));
    changed.name = "Something else".into();
    assert_eq!(proven_saver(ROOM, &changed, &none, NOW_NS), None);
}

#[test]
fn a_shortcut_copied_into_another_room_proves_nothing_there() {
    let ada = device(1, 2);
    let saved = ada.saves(ROOM, shortcut("water"));
    assert_eq!(
        proven_saver(OTHER_ROOM, &saved, &RevocationList::new(), NOW_NS),
        None
    );
}

#[test]
fn a_device_revoked_after_it_saved_proves_nothing_from_then() {
    let ada = device(1, 2);
    let saved = ada.saves(ROOM, shortcut("water"));
    let mut revocations = RevocationList::new();
    revocations.insert(revoked(&ada)).unwrap();
    assert_eq!(proven_saver(ROOM, &saved, &revocations, NOW_NS), None);
}

#[test]
fn an_expired_certificate_proves_nothing() {
    let mut ada = device(1, 2);
    ada.cert.expires_at_ns = Some(NOW_NS);
    tilo_user_identity::sign_device_cert(&mut ada.cert, &ada.root).unwrap();
    let saved = ada.saves(ROOM, shortcut("water"));
    assert_eq!(
        proven_saver(ROOM, &saved, &RevocationList::new(), NOW_NS - 1),
        Some(ada.person())
    );
    assert_eq!(
        proven_saver(ROOM, &saved, &RevocationList::new(), NOW_NS),
        None
    );
}

#[test]
fn the_rooms_revocations_are_those_its_identity_rows_carry() {
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
        let row = serde_json::json!({ "device_cert": device.cert, "revocations": revocations });
        doc.put(
            &identities,
            hex::encode(device.cert.device_public_key),
            ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
        )
        .unwrap();
    }
    let revocations = room_revocations(&doc);
    assert!(revocations.is_revoked(&ada.cert.device_public_key));
    assert_eq!(revocations.len(), 1);
}
