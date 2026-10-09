use super::{StatePeer, SystemTime};
use crate::workspace_membership::WorkspaceMembershipCommand;
use collab_types::caps::{Access, CapOverride, Principal, Scope};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{GrantId, Subject, roles};
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
        self.check_workspace_administration(caller.device_public_key, now)?;
        let member = self.permissions(caller.device_public_key, now)?;
        self.require_governance_writer(key, now)?;
        if let WorkspaceMembershipCommand::GrantAppAuthorship { app } = command {
            let capability = CapOverride::grant(
                collab_types::caps::Subject::app(app),
                Scope::Workspace,
                Access::Write,
            )
            .with_id(format!("app-author:{app}"))
            .granted_by(Principal::Person(member.person));
            return self.put_governed_sandbox(&[capability], key, now);
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
            WorkspaceMembershipCommand::GrantAppAuthorship { .. } => {
                return Err("app authorship entered the agent membership branch".into());
            }
        };
        *self = candidate;
        Ok(removed || added)
    }
}
