use super::{StatePeer, SystemTime};
use crate::workspace_membership::WorkspaceMembershipCommand;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{ActionKind, Change, Grant, GrantId, Lifetime, Subject, TreePath, roles};
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Check that this admitted device may grant in the room now.
    ///
    /// # Errors
    /// Refuses stale membership or a person not granted the room's grant action.
    pub fn check_workspace_administration(
        &self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.permissions(device, now)?;
        if !member.allows(
            &roles::governance_action(&self.core.room, roles::governance::GRANT),
            roles::GOVERNANCE_KIND,
            now,
        ) {
            return Err("workspace membership requires current administration authority".into());
        }
        Ok(())
    }

    /// Apply the retained person's typed membership command using actual
    /// governor signing custody. Public command arguments grant no authority.
    ///
    /// # Errors
    /// Refuses stale certificates, non-administrators, malformed identifiers,
    /// unavailable governor signing custody or invalid governed policy.
    pub fn apply_workspace_membership_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        command.validate()?;
        if self
            .core
            .certificates
            .get(&caller.device_public_key)
            .map(AsRef::as_ref)
            != Some(caller)
        {
            return Err("membership caller is not current admitted evidence".into());
        }
        self.certificate(&caller.device_public_key, now)?;
        if matches!(
            command,
            WorkspaceMembershipCommand::GrantForTask { .. }
                | WorkspaceMembershipCommand::RevokeForTask { .. }
        ) {
            return self.apply_task_grant_command(caller, command, key, now);
        }
        if matches!(
            command,
            WorkspaceMembershipCommand::Grant { .. } | WorkspaceMembershipCommand::Revoke { .. }
        ) {
            return self.apply_governance_grant_command(caller, command, key, now);
        }
        self.check_workspace_administration(caller.device_public_key, now)?;
        let member = self.permissions(caller.device_public_key, now)?;
        self.require_governance_writer(key, now)?;
        if let WorkspaceMembershipCommand::GrantAppAuthorship { app } = command {
            let grant = roles::app_authorship(&self.core.room, app, Subject::Person(member.person));
            return self.put_governed_grants(&[grant], key, now);
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
        let mut candidate = self.clone();
        let removed = candidate.remove_governed_grants(&removals, key, now)?;
        let added = match command {
            WorkspaceMembershipCommand::AddAgent { agent, preset } => {
                let grants = preset.grants(&self.core.room, agent, &Subject::Person(member.person));
                candidate.put_governed_grants(&grants, key, now)?
            }
            WorkspaceMembershipCommand::RemoveAgent { .. } => false,
            WorkspaceMembershipCommand::GrantAppAuthorship { .. }
            | WorkspaceMembershipCommand::GrantForTask { .. }
            | WorkspaceMembershipCommand::RevokeForTask { .. }
            | WorkspaceMembershipCommand::Grant { .. }
            | WorkspaceMembershipCommand::Revoke { .. } => {
                return Err("a command entered the agent membership branch".into());
            }
        };
        *self = candidate;
        Ok(removed || added)
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
                self.put_governed_grants(&[recorded], key, now)
            }
            WorkspaceMembershipCommand::Revoke { grant } => {
                self.remove_governed_grants(std::slice::from_ref(grant), key, now)
            }
            _ => Err("not a governance grant command".into()),
        }
    }

    /// Change an open task's grant set, for a person who may work in the
    /// room. A task's grants only bound the work done in it, so changing
    /// them needs no administration.
    fn apply_task_grant_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let room = TreePath::room(&self.core.room);
        if !member.allows(&room, ActionKind::Undoable, now) {
            return Err("changing a task's grants needs write authority in the room".into());
        }
        let (WorkspaceMembershipCommand::GrantForTask { task, .. }
        | WorkspaceMembershipCommand::RevokeForTask { task, .. }) = command
        else {
            return Err("not a task grant command".into());
        };
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("workspace task room document is absent")?;
        if !tilo_desktop_model::projection::open_task_ids(doc).contains(task) {
            return Err("the task is not open".into());
        }
        match command {
            WorkspaceMembershipCommand::GrantForTask {
                task,
                node,
                kinds,
                answer,
            } => {
                let grant = Grant::new(
                    crate::workspace_membership::task_grant_id(task, node).0,
                    Subject::Task(task.clone()),
                    room.join(node),
                    *kinds,
                    *answer,
                    Subject::Person(member.person),
                )
                .with_lifetime(Lifetime::Task(task.clone()));
                self.put_governed_grants(&[grant], key, now)
            }
            WorkspaceMembershipCommand::RevokeForTask { grant, .. } => {
                self.remove_governed_grants(std::slice::from_ref(grant), key, now)
            }
            _ => Err("not a task grant command".into()),
        }
    }
}
