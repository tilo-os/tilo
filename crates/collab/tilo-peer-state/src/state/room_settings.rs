//! The room's saved grant sets and sandbox capabilities, which its governor
//! alone writes and every member reads from the room's governance document
//! ([`tilo_desktop_model::projection::workspace_presets_of`],
//! [`tilo_desktop_model::projection::room_sandbox_of`]).
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{SystemTime, err};
use ed25519_dalek::SigningKey;
use tilo_desktop_model::projection;

impl PeerGovernance<'_> {
    /// The grant sets the room has saved, as its governance records them.
    #[must_use]
    pub fn governed_presets(self) -> Vec<tilo_action_policy::Preset> {
        self.peer
            .core
            .room_doc
            .as_deref()
            .map(projection::workspace_presets_of)
            .unwrap_or_default()
    }
}

impl PeerGovernanceMut<'_> {
    /// Make the room's saved grant sets `presets`, as the governor `key`
    /// decided. A set grants nothing itself; assigning it writes grants.
    /// Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not the governance writer, or governance cannot be updated.
    pub fn put_governed_presets(
        self,
        presets: &[tilo_action_policy::Preset],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_workspace_presets(doc, presets)
        })
    }

    /// Make the room's sandbox capabilities `sandbox`, as the governor `key`
    /// decided. Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not the governance writer, or governance cannot be updated.
    pub fn put_governed_sandbox(
        self,
        sandbox: &[collab_types::caps::CapOverride],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_room_sandbox(doc, sandbox)
        })
    }

    /// Write `edit` into the room's governance document as the governor
    /// `key`, and admit it the way every member admits the governor's
    /// changes.
    pub(super) fn author(
        self,
        key: &SigningKey,
        now: SystemTime,
        edit: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), automerge::AutomergeError>,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        edit(&mut doc).map_err(err)?;
        doc.commit();
        if doc.get_heads() == before {
            return Ok(false);
        }
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NeighbourMsg, SigningKey, StatePeer, SystemTime, TransportOrigin};
    use automerge::{ROOT, ScalarValue, transaction::Transactable as _};
    use tilo_desktop_model::projection::{ROOM_NAME_KEY, ROOM_PRESETS_KEY};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";

    /// A device of the person whose root is `[11; 32]`.
    fn device(seed: u8) -> (DeviceCert, SigningKey) {
        let root = SigningKey::from_bytes(&[11; 32]);
        let device = SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: format!("device {seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (cert, device)
    }

    /// The room as its governor, the owner's device `[12; 32]`, holds it;
    /// the owner's other device `[13; 32]` is on its roster.
    fn governor() -> (DeviceCert, SigningKey, StatePeer) {
        let (cert, device) = device(12);
        let now = SystemTime::now();
        let roster = vec![cert.clone(), self::device(13).0];
        let mut peer = StatePeer::new(
            ROOM,
            cert.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            super::super::workspace_seed(ROOM, &cert, &roster, "Room").unwrap(),
            &device,
            now,
        )
        .unwrap();
        (cert, device, peer)
    }

    fn preset(name: &str) -> tilo_action_policy::Preset {
        tilo_action_policy::Preset {
            name: name.into(),
            description: String::new(),
            rules: vec![tilo_action_policy::Rule::new(
                tilo_action_policy::TreePath::root().app("so.tilo.notes"),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
            )],
        }
    }

    #[test]
    fn the_governor_saves_the_rooms_grant_sets_and_members_read_them() {
        let (_, key, mut peer) = governor();
        let now = SystemTime::now();
        assert!(
            peer.governance_mut()
                .put_governed_presets(&[preset("Reviewers")], &key, now)
                .unwrap()
        );
        let names: Vec<String> = peer
            .governance()
            .governed_presets()
            .into_iter()
            .map(|preset| preset.name)
            .collect();
        assert_eq!(names, ["Reviewers"]);
        assert!(
            !peer
                .governance_mut()
                .put_governed_presets(&[preset("Reviewers")], &key, now)
                .unwrap(),
            "saving the same sets again changes nothing"
        );
    }

    #[test]
    fn only_the_governor_writes_the_rooms_sandbox() {
        let (_, _, mut peer) = governor();
        let other = SigningKey::from_bytes(&[13; 32]);
        assert!(
            peer.governance_mut()
                .put_governed_sandbox(&[], &other, SystemTime::now())
                .is_err()
        );
    }

    /// A change `key` makes to the room: `field` set to `value`, delivered
    /// as that device's.
    fn write(
        peer: &mut StatePeer,
        key: &SigningKey,
        field: &str,
        value: ScalarValue,
    ) -> Result<(), String> {
        let mut doc = peer.admitted_room().unwrap().clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(ROOT, field, value).unwrap();
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: ROOM.into(),
                changes,
            },
            SystemTime::now(),
        )
        .map(|_| ())
    }

    #[test]
    fn a_member_cannot_rename_the_room_or_rewrite_its_grant_sets() {
        let (_, key, mut peer) = governor();
        let (_, other) = device(13);
        // The owner's other device writes the room as a member: what is
        // not governed is taken.
        write(
            &mut peer,
            &other,
            "scratch",
            ScalarValue::Str("mine".into()),
        )
        .unwrap();
        for field in [ROOM_NAME_KEY, ROOM_PRESETS_KEY] {
            assert!(
                write(&mut peer, &other, field, ScalarValue::Str("Mine".into())).is_err(),
                "{field} is the governor's"
            );
        }
        assert_eq!(peer.governance().room_name().as_deref(), Some("Room"));
        write(
            &mut peer,
            &key,
            ROOM_NAME_KEY,
            ScalarValue::Str("Ours".into()),
        )
        .unwrap();
        assert_eq!(peer.governance().room_name().as_deref(), Some("Ours"));
    }
}
