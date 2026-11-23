use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{StatePeer, SystemTime, person_of};
use crate::workspace_membership::WorkspaceMembershipCommand;
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Change, Grantee, Role, TreePath, roles};
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Write a row that is the calling device's own, a lease claim, a schedule's answer or work
    /// kept done: no other device signs for it.
    fn apply_own_row(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let own = |caller: &DeviceCert| {
            if key.device_public_key() == caller.device_public_key {
                Ok(())
            } else {
                Err(String::from("a device's own row is written by that device"))
            }
        };
        match command {
            WorkspaceMembershipCommand::Lease { id, ttl_ms, rank } => {
                own(caller)?;
                let before = self.workspace_heads().unwrap_or_default();
                if *ttl_ms == 0 {
                    self.workspace_mut().release_lease(caller, id, now)?;
                } else {
                    self.workspace_mut().take_lease(
                        caller,
                        id,
                        std::time::Duration::from_millis(*ttl_ms),
                        *rank,
                        now,
                    )?;
                }
                Ok(self.workspace_heads().unwrap_or_default() != before)
            }
            WorkspaceMembershipCommand::AnswerSchedule {
                automation,
                trigger,
                mark,
                answered_ms,
            } => {
                own(caller)?;
                self.workspace_mut().answer_schedule(
                    caller,
                    automation,
                    usize::try_from(*trigger).map_err(|error| error.to_string())?,
                    mark,
                    *answered_ms,
                    now,
                )
            }
            WorkspaceMembershipCommand::KeepDone { id, answer } => {
                own(caller)?;
                self.workspace_mut()
                    .keep_done(caller, id, answer.clone(), now)
            }
            _ => Ok(false),
        }
    }

    /// Apply a bot command (install, renew, uninstall, pause, resume,
    /// appearance, adopt); `None` for every other command.
    fn apply_bot_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
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

    /// Make `change` to the workspace's members, signed by `key`, the caller's
    /// own device: what it writes is admitted as far as the caller's grants
    /// reach where they made it (`workspace::membership`), here as on every
    /// device that receives it.
    pub(super) fn apply_member_change(
        &mut self,
        caller: &DeviceCert,
        change: &Change,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let person = |person: &str| -> Result<[u8; 32], String> {
            hex::decode(person)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| "a member is named by their user root key".to_owned())
        };
        let by = hex::encode(caller.user_public_key);
        let workspace = self.core.workspace.clone();
        match change {
            Change::AddMember { person: who, role } | Change::SetRole { person: who, role } => {
                if *role == Role::Owner {
                    return Err("the owner's role is not given through membership".into());
                }
                self.governance_mut()
                    .set_member_role(person(who)?, *role, key, now)
            }
            Change::RemoveMember { person: who } => {
                self.governance_mut().remove_member(person(who)?, key, now)
            }
            Change::TransferOwnership { person: who } => {
                self.governance_mut()
                    .transfer_ownership(person(who)?, key, now)
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
                        &workspace,
                        who,
                        *until_ms,
                        Grantee::person(by),
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
            Change::RetiredSetMemberRoles { .. } => {
                Err("roles are assigned one at a time".to_owned())
            }
            Change::DefineRole { .. }
            | Change::RemoveRole { .. }
            | Change::SetMemberCustomRole { .. }
            | Change::AddAgent { .. }
            | Change::RemoveAgent { .. } => {
                self.governance_mut()
                    .change_member_list(change.clone(), key, now)
            }
        }
    }

    /// Share `node` with the workspace as `access` gives, under `share`, as the
    /// caller: the grants a receiver admits from them
    /// ([`crate::workspace::share::share_grants`]). A share the workspace already
    /// holds is no change.
    fn apply_share(
        &mut self,
        caller: &DeviceCert,
        share: &str,
        node: &TreePath,
        access: tilo_action_policy::share::ShareAccess,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.membership(caller.device_public_key, now)?;
        let authority = member.authority();
        let cx = tilo_action_policy::Context::at_time(now);
        let device = hex::encode(caller.device_public_key);
        let grants = crate::workspace::share::share_grants(
            &member.policy,
            (person_of(&member)?, &device, &authority, &cx),
            share,
            node,
            access,
        )?;
        if grants
            .iter()
            .all(|grant| member.policy.grants.contains(grant))
        {
            return Ok(false);
        }
        self.governance_mut().put_governed_grants(&grants, key, now)
    }

    /// End the share `share` as the caller
    /// ([`crate::workspace::share::ended_by`]): no grant of it theirs to end is
    /// no change.
    fn apply_unshare(
        &mut self,
        caller: &DeviceCert,
        share: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.membership(caller.device_public_key, now)?;
        let authority = member.authority();
        let cx = tilo_action_policy::Context::at_time(now);
        let ended = crate::workspace::share::ended_by(
            &member.policy,
            (person_of(&member)?, &authority, &cx),
            share,
        );
        if ended.is_empty() {
            return Ok(false);
        }
        self.governance_mut()
            .remove_governed_grants(&ended, key, now)
    }

    /// Give or take back a grant in the workspace, as the caller: the workspace's
    /// policy decides, exactly as on a device that governs it here.
    fn apply_governance_grant_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.membership(caller.device_public_key, now)?;
        let authority = member.authority();
        let cx = tilo_action_policy::Context::at_time(now);
        let change = match command {
            WorkspaceMembershipCommand::Grant { grant } => Change::Grant(grant.clone()),
            WorkspaceMembershipCommand::Revoke { grant } => Change::Revoke(grant.clone()),
            _ => return Err("not a governance grant command".into()),
        };
        let mut checked = (*member.policy).clone();
        checked
            .apply(person_of(&member)?, &authority, change, &cx, false)
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
    /// The workspace's own document as this device holds it: its governance, and the layout the
    /// workspace keeps at its root, whole.
    #[must_use]
    pub fn document(self) -> Option<Vec<u8>> {
        self.peer
            .core
            .workspace_doc
            .as_ref()
            .map(crate::cached_document::CachedDocument::saved)
    }

    /// Check that this admitted device may grant in the workspace now.
    pub fn check_workspace_administration(
        self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.peer.membership(device, now)?;
        if !member.allows(
            &roles::governance_action(&self.peer.core.workspace, roles::governance::GRANT),
            roles::GOVERNANCE_KIND,
            now,
        ) {
            return Err("workspace membership requires current administration authority".into());
        }
        Ok(())
    }
}
impl PeerWorkspaceMut<'_> {
    /// Whether `key` may sign for `caller`: a device acts for its own person, and another
    /// person's certificate is not one its signature may stand for.
    fn signed_for_caller(
        &self,
        caller: &DeviceCert,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.device_public_key() != caller.device_public_key
            && self
                .peer
                .certificate(&key.device_public_key(), now)
                .map_err(|error| error.to_string())?
                .user_public_key
                != caller.user_public_key
        {
            return Err("a membership change is signed by its caller's own device".into());
        }
        Ok(())
    }

    /// Apply the retained person's typed membership command using actual
    /// governor signing ownership. Public command arguments grant no authority.
    pub fn apply_workspace_membership_command(
        self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
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
        self.peer
            .certificate(&caller.device_public_key, now)
            .map_err(|error| error.to_string())?;
        if let Some(outcome) = self.peer.apply_bot_command(caller, command, key, now) {
            return outcome;
        }
        if let Some(outcome) = self.peer.apply_app_command(caller, command, key, now) {
            return outcome;
        }
        if let Some(outcome) = self.peer.apply_call_command(caller, command, key, now) {
            return outcome;
        }
        self.signed_for_caller(caller, key, now)?;
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
        if let WorkspaceMembershipCommand::Place {
            person,
            place,
            name,
        } = command
        {
            return self.peer.governance_mut().place_member(
                *person,
                *place,
                name.as_deref(),
                key,
                now,
            );
        }
        if let WorkspaceMembershipCommand::Settings(change) = command {
            return self.peer.apply_settings_change(caller, change, key, now);
        }
        if let WorkspaceMembershipCommand::Share {
            share,
            node,
            access,
        } = command
        {
            return self
                .peer
                .apply_share(caller, share, node, *access, key, now);
        }
        if let WorkspaceMembershipCommand::Unshare { share } = command {
            return self.peer.apply_unshare(caller, share, key, now);
        }
        self.peer.apply_own_row(caller, command, key, now)
    }
}
