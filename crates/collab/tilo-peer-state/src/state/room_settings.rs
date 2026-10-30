//! The room's own settings: its name, its saved grant sets and their
//! assignment, the actions its apps may use of each other and its apps'
//! sandbox capabilities, which every member reads from the room's
//! governance document ([`tilo_desktop_model::projection::workspace_presets_of`],
//! [`tilo_desktop_model::projection::room_sandbox_of`]).
//!
//! A member's device writes a change to them itself, signed with its own
//! key; the room's admission decides it against the writer's own grants
//! where they made it (`room::membership`), here and on every device that
//! receives it.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{StatePeer, SystemTime, err};
use crate::workspace_membership::SettingsChange;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject, roles};
use tilo_desktop_model::projection;
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Make `change` to the room's settings as `caller`, signed by `key`,
    /// the caller's own device. Answers whether governance changed.
    pub(super) fn apply_settings_change(
        &mut self,
        caller: &DeviceCert,
        change: &SettingsChange,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let bound = roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut workspace = self.settings_workspace()?;
        let refused = |error: &dyn std::fmt::Display| error.to_string();
        match change {
            SettingsChange::Rename { name } => {
                let name = crate::workspace_membership::valid_room_name(name)?.to_owned();
                if self.governance().room_name().as_deref() == Some(name.as_str()) {
                    return Ok(false);
                }
                self.governance_mut().author_room_name(&name, key, now)?;
                Ok(true)
            }
            SettingsChange::SavePreset { preset } => {
                workspace
                    .save_preset_with_at(&authority, preset.clone(), seconds)
                    .map_err(|error| refused(&error))?;
                self.governance_mut()
                    .put_governed_presets(&workspace.presets, key, now)
            }
            SettingsChange::AssignPreset {
                preset,
                subject,
                lifetime,
            } => {
                let assigned = workspace
                    .assign_preset_with_at(
                        &member.person,
                        &authority,
                        preset,
                        subject,
                        lifetime,
                        seconds,
                    )
                    .map_err(|error| refused(&error))?;
                let grants: Vec<Grant> = workspace
                    .policy
                    .grants
                    .iter()
                    .filter(|grant| assigned.contains(&grant.id))
                    .cloned()
                    .collect();
                self.governance_mut().put_governed_grants(&grants, key, now)
            }
            SettingsChange::GrantAppAction(action) => {
                if action.target_app == tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID {
                    return Err("governing the workspace is granted to people, not apps".into());
                }
                // Enough for any action that at most changes the room
                // undoably: an outside effect still asks.
                let grant = Grant::new(
                    format!("app-action:{}", uuid::Uuid::new_v4().simple()),
                    Subject::App(action.app.clone()),
                    action.node(&workspace.node()),
                    KindSet::READ_UNDOABLE,
                    Answer::Allow,
                    Subject::person(member.person.as_str()),
                )
                .with_lifetime(Lifetime::UntilRevoked);
                self.apply_member_change(
                    caller,
                    &tilo_action_policy::Change::Grant(grant),
                    key,
                    now,
                )
            }
            SettingsChange::RevokeAppAction(action) => {
                let revoked = app_action_grants(&workspace, action);
                if revoked.is_empty() {
                    return Ok(false);
                }
                self.governance_mut()
                    .remove_governed_grants(&revoked, key, now)
            }
            SettingsChange::GrantSandbox { capability } => {
                let by = collab_types::caps::Principal::Person(member.person.clone());
                grant_sandbox(&mut workspace, capability, by)?;
                self.governance_mut()
                    .put_governed_sandbox(&workspace.sandbox, key, now)
            }
            SettingsChange::RevokeSandbox { capability } => {
                if !workspace.revoke_sandbox(capability) {
                    return Ok(false);
                }
                self.governance_mut()
                    .put_governed_sandbox(&workspace.sandbox, key, now)
            }
        }
    }

    /// The room's governance as a workspace reads it: its name, policy,
    /// sandbox and grant sets.
    fn settings_workspace(&self) -> Result<tilo_desktop_model::Workspace, String> {
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("room governance missing")?;
        let room = tilo_desktop_model::ids::RoomId::parse(&self.core.room)
            .ok_or("the room is not a UUID")?;
        let mut workspace = tilo_desktop_model::Workspace::in_room(
            tilo_desktop_model::ids::WorkspaceId(0),
            room,
            String::new(),
        );
        crate::room::adopt_room(&mut workspace, doc, &[]);
        Ok(workspace)
    }
}

/// Every grant given for `action`, through a grant here or by answering the
/// app's ask in place.
fn app_action_grants(
    workspace: &tilo_desktop_model::Workspace,
    action: &crate::workspace_membership::AppAction,
) -> Vec<tilo_action_policy::GrantId> {
    let subject = Subject::App(action.app.clone());
    let node = action.node(&workspace.node());
    workspace
        .policy
        .grants
        .iter()
        .filter(|grant| {
            grant.subject == subject && (grant.node == node || node.covers(&grant.node))
        })
        .map(|grant| grant.id.clone())
        .collect()
}

/// Record `capability` in `workspace`'s sandbox as granted `by` a member,
/// under an id of its own when it came without one. One that ends with a
/// session or a single use is never written into the room.
fn grant_sandbox(
    workspace: &mut tilo_desktop_model::Workspace,
    capability: &collab_types::caps::CapOverride,
    by: collab_types::caps::Principal,
) -> Result<(), String> {
    if !capability.persists_across_restart() {
        return Err(
            "a capability that ends with a session or a single use is never written into the room"
                .into(),
        );
    }
    let mut capability = capability.clone();
    if capability.id.trim().is_empty() {
        capability.id = format!("cap-{}", uuid::Uuid::new_v4().simple());
    }
    workspace.grant_sandbox(capability.granted_by(by));
    Ok(())
}

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
    /// Make the room's saved grant sets `presets`, written by `key`. A set
    /// grants nothing itself; assigning it writes grants.
    /// Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
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

    /// Make the room's sandbox capabilities `sandbox`, written by `key`.
    /// Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
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

    /// Write `edit` into the room's governance document as `key`, and
    /// admit it the way every member admits it.
    pub(super) fn author(
        self,
        key: &SigningKey,
        now: SystemTime,
        edit: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), automerge::AutomergeError>,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.room_actor_of(key));
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
    use tilo_desktop_model::projection::ROOM_NAME_KEY;
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
    fn any_device_of_the_owner_writes_the_rooms_sandbox() {
        let (_, _, mut peer) = governor();
        let other = SigningKey::from_bytes(&[13; 32]);
        peer.governance_mut()
            .put_governed_sandbox(&[], &other, SystemTime::now())
            .unwrap();
        let stranger = SigningKey::from_bytes(&[99; 32]);
        assert!(
            peer.governance_mut()
                .put_governed_sandbox(&[], &stranger, SystemTime::now())
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
    fn any_device_of_the_owner_renames_the_room_and_rewrites_its_grant_sets() {
        let (_, key, mut peer) = governor();
        let (_, other) = device(13);
        write(
            &mut peer,
            &other,
            "scratch",
            ScalarValue::Str("mine".into()),
        )
        .unwrap();
        write(
            &mut peer,
            &other,
            ROOM_NAME_KEY,
            ScalarValue::Str("Mine".into()),
        )
        .unwrap();
        assert_eq!(peer.governance().room_name().as_deref(), Some("Mine"));
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
