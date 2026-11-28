//! The workspace's own settings: its name, its saved grant sets and their
//! assignment, its apps' sandbox capabilities, its entry and its saved
//! shortcuts, which every member reads
//! from the workspace's governance
//! ([`tilo_desktop_model::projection::workspace_presets_of`],
//! [`tilo_desktop_model::projection::workspace_sandbox_of`],
//! [`tilo_desktop_model::projection::workspace_entry_of`],
//! [`tilo_desktop_model::projection::workspace_shortcuts_of`]).
//!
//! A member's device writes a change to them itself, signed with its own
//! key; the workspace's admission decides it against the writer's own grants
//! where they made it (`workspace::membership`), here and on every device that
//! receives it.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{StatePeer, SystemTime, err, person_of};
use crate::workspace_membership::SettingsChange;
use collab_types::signer::DeviceSigner;
use tilo_action_policy::Grant;
use tilo_desktop_model::projection;
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Make `change` to the workspace's settings as `caller`, signed by `key`,
    /// the caller's own device. Answers whether governance changed.
    pub(super) fn apply_settings_change(
        &mut self,
        caller: &DeviceCert,
        change: &SettingsChange,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.membership(caller.device_public_key, now)?;
        let authority = member.authority();
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut workspace = self.settings_workspace()?;
        let refused = |error: &dyn std::fmt::Display| error.to_string();
        match change {
            SettingsChange::Rename { name } => {
                let name = crate::workspace_membership::valid_workspace_name(name)?.to_owned();
                if self.governance().workspace_name().as_deref() == Some(name.as_str()) {
                    return Ok(false);
                }
                self.governance_mut()
                    .author_workspace_name(&name, key, now)?;
                Ok(true)
            }
            SettingsChange::SavePreset { preset } => {
                workspace
                    .policy
                    .save_preset_at(&mut workspace.presets, &authority, preset.clone(), seconds)
                    .map_err(|error| refused(&error))?;
                self.governance_mut()
                    .put_saved_presets(&workspace.presets, key, now)
            }
            SettingsChange::AssignPreset {
                preset,
                subject,
                lifetime,
            } => {
                let assigned = workspace
                    .policy
                    .assign_preset_at(
                        &workspace.presets,
                        person_of(&member)?,
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
                self.governance_mut().put_member_grants(&grants, key, now)
            }
            SettingsChange::GrantSandbox { capability } => {
                let by = tilo_member_types::MemberId::person(person_of(&member)?.to_owned());
                grant_sandbox(&mut workspace, capability, by)?;
                self.governance_mut()
                    .put_member_sandbox(&workspace.sandbox, key, now)
            }
            SettingsChange::RevokeSandbox { capability } => {
                if !tilo_permissions::caps::sandbox::revoke(&mut workspace.sandbox, capability) {
                    return Ok(false);
                }
                self.governance_mut()
                    .put_member_sandbox(&workspace.sandbox, key, now)
            }
            SettingsChange::Entry(entry) => {
                let entry = entry.clone().checked().map_err(|error| error.to_string())?;
                self.governance_mut().put_workspace_entry(&entry, key, now)
            }
            SettingsChange::Content(policy) => {
                self.governance_mut().put_content_policy(policy, key, now)
            }
            SettingsChange::Unlist => {
                let mut entry = self.governance().workspace_entry();
                entry.listing = None;
                self.governance_mut().put_workspace_entry(&entry, key, now)
            }
            SettingsChange::Limits {
                file_bytes,
                member_bytes,
                hourly_bytes,
            } => {
                let named = |named: &Option<u64>, current: Option<u64>| {
                    named.map_or(current, |bytes| (bytes > 0).then_some(bytes))
                };
                let current = self.governance().content_policy();
                let policy = tilo_action_policy::ContentPolicy {
                    file_bytes: named(file_bytes, current.file_bytes),
                    member_bytes: named(member_bytes, current.member_bytes),
                    hourly_bytes: named(hourly_bytes, current.hourly_bytes),
                };
                self.governance_mut().put_content_policy(&policy, key, now)
            }
            SettingsChange::Shortcuts(change) => self
                .workspace_mut()
                .apply_shortcut_change(caller, change, key, now),
        }
    }

    /// The workspace's governance as a workspace reads it: its name, policy,
    /// sandbox and grant sets.
    fn settings_workspace(&self) -> Result<tilo_desktop_model::Workspace, String> {
        let doc = self
            .core
            .workspace_doc
            .as_deref()
            .ok_or("workspace governance missing")?;
        let workspace = tilo_desktop_model::ids::WorkspaceId::parse(&self.core.workspace)
            .ok_or("the workspace is not a UUID")?;
        let mut workspace = tilo_desktop_model::Workspace::new(workspace, String::new());
        crate::workspace::adopt_workspace(&mut workspace, doc, &[]);
        Ok(workspace)
    }
}

/// Record `capability` in `workspace`'s sandbox as granted `by` a member,
/// under an id of its own when it came without one. One that ends with a
/// session or a single use is never written into the workspace.
fn grant_sandbox(
    workspace: &mut tilo_desktop_model::Workspace,
    capability: &tilo_permissions::caps::CapOverride,
    by: tilo_member_types::MemberId,
) -> Result<(), String> {
    if !capability.is_durable() {
        return Err(
            "a capability that ends with a session or a single use is never written into the workspace"
                .into(),
        );
    }
    let mut capability = capability.clone();
    if capability.id.trim().is_empty() {
        capability.id.clear();
    }
    tilo_permissions::caps::sandbox::grant(&mut workspace.sandbox, capability.granted_by(by));
    Ok(())
}

/// The digest a join names the workspace's entry by: a domain tag and the
/// entry's canonical encoding. Every device computes it alike, and the value
/// says exactly the rule a guest came in under, not merely that it was open.
///
/// # Errors
///
/// The entry does not encode.
pub fn entry_digest(entry: &tilo_action_policy::EntryPolicy) -> Result<[u8; 32], String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTRY_DIGEST_DOMAIN);
    hasher.update(&postcard::to_allocvec(entry).map_err(|error| error.to_string())?);
    Ok(*hasher.finalize().as_bytes())
}

/// What separates an entry's digest from any other digest of the same bytes.
const ENTRY_DIGEST_DOMAIN: &[u8] = b"tilo.workspace.entry.v1";

impl PeerGovernance<'_> {
    /// The workspace's entry, as its governance records it: whether a directory
    /// lists it, and whom it lets in. Every host serving the workspace answers
    /// its entry from this one record.
    #[must_use]
    pub fn workspace_entry(self) -> tilo_action_policy::EntryPolicy {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map(projection::workspace_entry_of)
            .unwrap_or_default()
    }

    /// The content policy the workspace's owners set, as its governance
    /// records it: the limits its content keeps when they set none.
    #[must_use]
    pub fn content_policy(self) -> tilo_action_policy::ContentPolicy {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map(projection::workspace_content_of)
            .unwrap_or_default()
    }

    /// The grant sets the workspace has saved, as its governance records them.
    #[must_use]
    pub fn saved_presets(self) -> Vec<tilo_action_policy::Preset> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map(projection::workspace_presets_of)
            .unwrap_or_default()
    }
}

impl PeerGovernanceMut<'_> {
    /// Settle the workspace's entry as `entry`, written by `key`: whether a
    /// directory lists it, whether it takes people by invitation only, and how
    /// many requests it keeps. Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
    pub fn put_workspace_entry(
        self,
        entry: &tilo_action_policy::EntryPolicy,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_workspace_entry(doc, entry).map_err(super::err)
        })
    }

    /// Settle the workspace's content policy as `policy`, written by `key`:
    /// the largest file a host keeps, and what one member's files may take,
    /// now and in any hour. Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
    pub fn put_content_policy(
        self,
        policy: &tilo_action_policy::ContentPolicy,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_workspace_content(doc, policy).map_err(|error| error.to_string())
        })
    }

    /// Make the workspace's saved grant sets `presets`, written by `key`. A set
    /// grants nothing itself; assigning it writes grants.
    /// Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
    pub fn put_saved_presets(
        self,
        presets: &[tilo_action_policy::Preset],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_workspace_presets(doc, presets)
        })
    }

    /// Make the workspace's sandbox capabilities `sandbox`, written by `key`.
    /// Answers whether governance changed.
    ///
    /// # Errors
    ///
    /// `key` is not a member's device, or governance cannot be updated.
    pub fn put_member_sandbox(
        self,
        sandbox: &[tilo_permissions::caps::CapOverride],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.author(key, now, |doc| {
            projection::project_workspace_sandbox(doc, sandbox)
        })
    }

    /// Write `edit` into the workspace's governance signed by `key`, and
    /// admit it the way every member admits it.
    pub(super) fn author(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
        edit: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        edit(&mut doc).map_err(err)?;
        super::commit_governance(&mut doc, super::now_secs(now), None);
        if doc.get_heads() == before {
            return Ok(false);
        }
        self.peer.write_governance(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NeighbourMsg, SigningKey, StatePeer, TransportOrigin};
    use automerge::{ROOT, ScalarValue, transaction::Transactable as _};
    use tilo_desktop_model::projection::WORKSPACE_NAME_KEY;
    use tilo_user_identity::{DeviceCert, RevocationList};

    use crate::fixture::member_cert;
    const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";

    /// A device of the person whose root is `[11; 32]`.
    fn device(seed: u8) -> (DeviceCert, SigningKey) {
        let root = SigningKey::from_bytes(&[11; 32]);
        let device = SigningKey::from_bytes(&[seed; 32]);
        let cert = member_cert(&root, &device, &format!("device {seed}"));
        (cert, device)
    }

    /// The workspace, with the owner's device `[12; 32]` as its host;
    /// the owner's other device `[13; 32]` is on its roster.
    fn owner_copy() -> (DeviceCert, SigningKey, StatePeer) {
        let (cert, device) = device(12);
        let now = tilo_clock::wall();
        let roster = vec![cert.clone(), self::device(13).0];
        let mut peer = StatePeer::new(
            WORKSPACE,
            cert.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            super::super::workspace_seed(WORKSPACE, &cert, &roster, "Workspace", now).unwrap(),
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
                tilo_action_types::KindSet::READ,
                tilo_action_policy::Answer::Allow,
            )],
        }
    }

    tilo_test_env::scenario! {
        fn the_owners_devices_write_the_workspaces_settings;

        phase the_owner_saves_the_workspaces_grant_sets_and_members_read_them;
        {
            let (_, key, mut peer) = owner_copy();
            let now = tilo_clock::wall();
            assert!(
                peer.governance_mut()
                    .put_saved_presets(&[preset("Reviewers")], &key, now)
                    .unwrap()
            );
            let names: Vec<String> = peer
                .governance()
                .saved_presets()
                .into_iter()
                .map(|preset| preset.name)
                .collect();
            assert_eq!(names, ["Reviewers"]);
            assert!(
                !peer
                    .governance_mut()
                    .put_saved_presets(&[preset("Reviewers")], &key, now)
                    .unwrap(),
                "saving the same sets again changes nothing"
            );
        }

        phase any_device_of_the_owner_writes_the_workspaces_sandbox;
        {
            let (_, _, mut peer) = owner_copy();
            let other = SigningKey::from_bytes(&[13; 32]);
            peer.governance_mut()
                .put_member_sandbox(&[], &other, tilo_clock::wall())
                .unwrap();
            let stranger = SigningKey::from_bytes(&[99; 32]);
            assert!(
                peer.governance_mut()
                    .put_member_sandbox(&[], &stranger, tilo_clock::wall())
                    .is_err()
            );
        }

        phase any_device_of_the_owner_renames_the_workspace_and_rewrites_its_grant_sets;
        {
            let (cert, key, mut peer) = owner_copy();
            let (other_cert, other) = device(13);
            write(
                &mut peer,
                (&other_cert, &other),
                "scratch",
                ScalarValue::Str("mine".into()),
                false,
            )
            .unwrap();
            // A rename is the operation that names it, from the owner's
            // devices as from anyone's.
            assert!(
                write(
                    &mut peer,
                    (&other_cert, &other),
                    WORKSPACE_NAME_KEY,
                    ScalarValue::Str("Mine".into()),
                    false,
                )
                .is_err()
            );
            assert_eq!(
                peer.governance().workspace_name().as_deref(),
                Some("Workspace")
            );
            write(
                &mut peer,
                (&other_cert, &other),
                WORKSPACE_NAME_KEY,
                ScalarValue::Str("Mine".into()),
                true,
            )
            .unwrap();
            assert_eq!(peer.governance().workspace_name().as_deref(), Some("Mine"));
            write(
                &mut peer,
                (&cert, &key),
                WORKSPACE_NAME_KEY,
                ScalarValue::Str("Ours".into()),
                true,
            )
            .unwrap();
            assert_eq!(peer.governance().workspace_name().as_deref(), Some("Ours"));
        }
    }

    /// A change `key` makes to the workspace: `field` set to `value`, delivered
    /// as that device's, with the operation naming the settings it changes
    /// where `named`.
    fn write(
        peer: &mut StatePeer,
        (cert, key): (&DeviceCert, &SigningKey),
        field: &str,
        value: ScalarValue,
        named: bool,
    ) -> Result<(), String> {
        let mut doc = peer.admitted_workspace().unwrap().clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(ROOT, field, value).unwrap();
        if named {
            let op = crate::members::SignedMembershipOp::signed(
                cert,
                key,
                crate::members::heads(&crate::members::ops(&doc)),
                tilo_clock::unix_now_ns(),
                crate::members::MembershipOpCommand::Governance {
                    rows: vec![crate::workspace::GovernanceRow::Settings],
                },
            )
            .unwrap();
            crate::members::put_op(&mut doc, &op).unwrap();
        }
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            TransportOrigin::ServerDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                workspace: WORKSPACE.into(),
                changes,
            },
            tilo_clock::wall(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
}
