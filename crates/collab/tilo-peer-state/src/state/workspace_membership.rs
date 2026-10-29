use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{StatePeer, SystemTime};
use crate::workspace_membership::WorkspaceMembershipCommand;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Change, GrantId, Subject, roles};
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Apply a bot command (install, renew, uninstall, pause, resume,
    /// appearance, adopt); `None` for every other command.
    fn apply_bot_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Option<Result<bool, String>> {
        match command {
            WorkspaceMembershipCommand::InstallBot(install) => {
                Some(self.apply_install_bot(caller, install, key, now))
            }
            WorkspaceMembershipCommand::RenewBot(renewal) => {
                Some(self.apply_renew_bot(caller, renewal, key, now))
            }
            WorkspaceMembershipCommand::UninstallBot { installation } => {
                Some(self.apply_uninstall_bot(caller, installation.as_str(), key, now))
            }
            WorkspaceMembershipCommand::PauseBot { installation } => Some(self.apply_bot_flag(
                caller,
                installation.as_str(),
                roles::governance::PAUSE_BOT,
                |row| !std::mem::replace(&mut row.paused, true),
                key,
                now,
            )),
            WorkspaceMembershipCommand::ResumeBot { installation } => Some(self.apply_bot_flag(
                caller,
                installation.as_str(),
                roles::governance::RESUME_BOT,
                |row| std::mem::replace(&mut row.paused, false),
                key,
                now,
            )),
            WorkspaceMembershipCommand::SetBotAppearance {
                installation,
                appearance,
            } => {
                let appearance = *appearance;
                Some(self.apply_bot_flag(
                    caller,
                    installation.as_str(),
                    roles::governance::SET_BOT_APPEARANCE,
                    move |row| std::mem::replace(&mut row.appearance, appearance) != appearance,
                    key,
                    now,
                ))
            }
            WorkspaceMembershipCommand::AdoptBot(adoption) => {
                Some(self.apply_adopt_bot(caller, adoption, key, now))
            }
            WorkspaceMembershipCommand::RequestBot(request) => {
                Some(self.apply_request_bot(caller, request, key, now))
            }
            WorkspaceMembershipCommand::WithdrawBotRequest { entry } => {
                Some(self.apply_withdraw_bot_request(caller, entry, key, now))
            }
            _ => None,
        }
    }

    /// Make `change` to the room's members, signed by `key`, the caller's
    /// own device: what it writes is admitted as far as the caller's grants
    /// reach where they made it (`room::membership`), here as on every
    /// device that receives it.
    pub(super) fn apply_member_change(
        &mut self,
        caller: &DeviceCert,
        change: &Change,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let person = |person: &str| -> Result<[u8; 32], String> {
            hex::decode(person)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| "a member is named by their user root key".to_owned())
        };
        let by = hex::encode(caller.user_public_key);
        let room = self.core.room.clone();
        match change {
            Change::AddMember { person: who, role } | Change::SetRole { person: who, role } => {
                let role = super::member_role_from_role(*role)
                    .ok_or("the owner's role is not given through membership")?;
                self.governance_mut()
                    .set_member_role(person(who)?, role, key, now)
            }
            Change::RemoveMember { person: who } => {
                self.governance_mut().remove_member(person(who)?, key, now)
            }
            Change::Ban { person: who } => {
                self.governance_mut()
                    .ban_member(person(who)?, &by, key, now)
            }
            Change::Unban { person: who } => {
                self.governance_mut().unban_member(person(who)?, key, now)
            }
            Change::Restrict {
                person: who,
                until_ms,
            } => {
                person(who)?;
                self.governance_mut().put_governed_grants(
                    &[roles::restriction_grant(
                        &room,
                        who,
                        *until_ms,
                        Subject::Person(by),
                    )],
                    key,
                    now,
                )
            }
            Change::Unrestrict { person: who } => {
                person(who)?;
                self.governance_mut().remove_governed_grants(
                    &[roles::restriction_grant_id(who)],
                    key,
                    now,
                )
            }
            Change::Grant(grant) => self.apply_governance_grant_command(
                caller,
                &WorkspaceMembershipCommand::Grant {
                    grant: grant.clone(),
                },
                key,
                now,
            ),
            Change::Revoke(grant) => self.apply_governance_grant_command(
                caller,
                &WorkspaceMembershipCommand::Revoke {
                    grant: grant.clone(),
                },
                key,
                now,
            ),
        }
    }

    /// Give or take back a grant in the room, as the caller: the room's
    /// policy decides, exactly as on a device that governs it here.
    fn apply_governance_grant_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let bound = roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let cx = tilo_action_policy::Context::at_time(now);
        let change = match command {
            WorkspaceMembershipCommand::Grant { grant } => Change::Grant(grant.clone()),
            WorkspaceMembershipCommand::Revoke { grant } => Change::Revoke(grant.clone()),
            _ => return Err("not a governance grant command".into()),
        };
        let mut checked = member.policy.clone();
        checked
            .apply(&member.person, &authority, change, &cx, false)
            .map_err(|refusal| refusal.to_string())?;
        match command {
            WorkspaceMembershipCommand::Grant { grant } => {
                let recorded = checked
                    .grants
                    .iter()
                    .find(|held| held.id == grant.id)
                    .cloned()
                    .ok_or("the grant was not recorded")?;
                self.governance_mut()
                    .put_governed_grants(&[recorded], key, now)
            }
            WorkspaceMembershipCommand::Revoke { grant } => self
                .governance_mut()
                .remove_governed_grants(std::slice::from_ref(grant), key, now),
            _ => Err("not a governance grant command".into()),
        }
    }
}
impl PeerWorkspace<'_> {
    /// Check that this admitted device may grant in the room now.
    ///
    /// # Errors
    /// Refuses stale membership or a person not granted the room's grant action.
    pub fn check_workspace_administration(
        self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.peer.permissions(device, now)?;
        if !member.allows(
            &roles::governance_action(&self.peer.core.room, roles::governance::GRANT),
            roles::GOVERNANCE_KIND,
            now,
        ) {
            return Err("workspace membership requires current administration authority".into());
        }
        Ok(())
    }
}
impl PeerWorkspaceMut<'_> {
    /// Apply the retained person's typed membership command using actual
    /// governor signing custody. Public command arguments grant no authority.
    ///
    /// # Errors
    /// Refuses stale certificates, non-administrators, malformed identifiers,
    /// unavailable governor signing custody or invalid governed policy.
    pub fn apply_workspace_membership_command(
        self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        command.validate()?;
        if self
            .peer
            .core
            .certificates
            .get(&caller.device_public_key)
            .map(AsRef::as_ref)
            != Some(caller)
        {
            return Err("membership caller is not current admitted evidence".into());
        }
        self.peer.certificate(&caller.device_public_key, now)?;
        if let Some(outcome) = self.peer.apply_bot_command(caller, command, key, now) {
            return outcome;
        }
        if let Some(outcome) = self.peer.apply_app_command(caller, command, key, now) {
            return outcome;
        }
        if key.verifying_key().to_bytes() != caller.device_public_key
            && self.peer.require_governance_writer(key, now).is_err()
        {
            // Only the owner's devices write on another device's behalf.
            return Err("a membership change is signed by its caller's own device".into());
        }
        if matches!(
            command,
            WorkspaceMembershipCommand::Grant { .. } | WorkspaceMembershipCommand::Revoke { .. }
        ) {
            return self
                .peer
                .apply_governance_grant_command(caller, command, key, now);
        }
        if let WorkspaceMembershipCommand::Member { change } = command {
            return self.peer.apply_member_change(caller, change, key, now);
        }
        if let WorkspaceMembershipCommand::Settings(change) = command {
            return self.peer.apply_settings_change(caller, change, key, now);
        }
        self.peer
            .workspace()
            .check_workspace_administration(caller.device_public_key, now)?;
        let member = self.peer.permissions(caller.device_public_key, now)?;
        if let WorkspaceMembershipCommand::GrantAppAuthorship { app } = command {
            self.peer.require_governance_writer(key, now)?;
            let grant =
                roles::app_authorship(&self.peer.core.room, app, Subject::Person(member.person));
            return self
                .peer
                .governance_mut()
                .put_governed_grants(&[grant], key, now);
        }
        let subject = Subject::agent(command.agent().ok_or("missing agent membership target")?);
        let room = member.policy.node();
        let removals = member
            .policy
            .grants
            .iter()
            .filter(|grant| {
                grant.subject == subject
                    && (matches!(command, WorkspaceMembershipCommand::RemoveAgent { .. })
                        || grant.node.is_within(&room))
            })
            .map(|grant| grant.id.clone())
            .collect::<Vec<GrantId>>();
        // Work on a clone: an invalid later addition must not leave a removal
        // applied to the caller's candidate.
        let mut candidate = self.peer.clone();
        let removed = candidate
            .governance_mut()
            .remove_governed_grants(&removals, key, now)?;
        let added = match command {
            WorkspaceMembershipCommand::AddAgent { agent, preset } => {
                let grants =
                    preset.grants(&self.peer.core.room, agent, &Subject::Person(member.person));
                candidate
                    .governance_mut()
                    .put_governed_grants(&grants, key, now)?
            }
            WorkspaceMembershipCommand::RemoveAgent { .. } => false,
            WorkspaceMembershipCommand::GrantAppAuthorship { .. }
            | WorkspaceMembershipCommand::RetiredGrantForTask { .. }
            | WorkspaceMembershipCommand::RetiredRevokeForTask { .. }
            | WorkspaceMembershipCommand::Grant { .. }
            | WorkspaceMembershipCommand::Revoke { .. }
            | WorkspaceMembershipCommand::InstallBot(_)
            | WorkspaceMembershipCommand::RenewBot(_)
            | WorkspaceMembershipCommand::UninstallBot { .. }
            | WorkspaceMembershipCommand::PauseBot { .. }
            | WorkspaceMembershipCommand::ResumeBot { .. }
            | WorkspaceMembershipCommand::SetBotAppearance { .. }
            | WorkspaceMembershipCommand::AdoptBot(_)
            | WorkspaceMembershipCommand::RequestBot(_)
            | WorkspaceMembershipCommand::WithdrawBotRequest { .. }
            | WorkspaceMembershipCommand::InstallApp(_)
            | WorkspaceMembershipCommand::UninstallApp { .. }
            | WorkspaceMembershipCommand::Member { .. }
            | WorkspaceMembershipCommand::Settings(_) => {
                return Err("a command entered the agent membership branch".into());
            }
        };
        *self.peer = candidate;
        Ok(removed || added)
    }
}
