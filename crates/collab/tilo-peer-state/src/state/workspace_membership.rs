use super::{Role, StatePeer, SystemTime};
use crate::workspace_membership::WorkspaceMembershipCommand;
use collab_types::caps::{
    Actor, CapDecision, CapOverride, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Authorize managed startup without creating membership or task authority.
    /// The retained original person's exact certificate must still be admitted,
    /// and the target must already be a current workspace Agent.
    ///
    /// # Errors
    /// Refuses invalid identifiers, replaced certificates, restricted callers,
    /// missing Agent membership and invalid policy time.
    pub fn check_workspace_agent_start(
        &self,
        caller: &DeviceCert,
        agent: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        WorkspaceMembershipCommand::RemoveAgent {
            agent: agent.into(),
        }
        .validate()?;
        if self
            .core
            .certificates
            .get(&caller.device_public_key)
            .map(AsRef::as_ref)
            != Some(caller)
        {
            return Err("agent startup caller is not current admitted evidence".into());
        }
        self.certificate(&caller.device_public_key, now)?;
        self.check_workspace_administration(caller.device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "agent startup clock is before the Unix epoch")?
            .as_secs();
        let document = self.admitted_room().ok_or("agent startup room is absent")?;
        let (_, owner, caps) = crate::room::extract_governed_fields(document);
        let mut workspace = tilo_desktop_model::Workspace::new(
            tilo_desktop_model::WorkspaceId(1),
            "agent startup authority",
        );
        workspace.owner = owner;
        workspace.capabilities = caps;
        if workspace
            .role_of_agent_at(agent, seconds)
            .is_none_or(|role| role == Role::Denied)
        {
            return Err("agent startup target is not a current room member".into());
        }
        Ok(())
    }

    /// Check current deny-biased workspace administration for this admitted device.
    ///
    /// # Errors
    /// Refuses stale membership, non-owner roles, restricted authority or invalid time.
    pub fn check_workspace_administration(
        &self,
        device: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        let (role, caps, person) = self.permissions(device, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "workspace governance clock is before the Unix epoch")?
            .as_secs();
        let actor = Actor::person(Some(role), person).in_room(self.core.room.clone());
        if role != Role::Owner
            || decide_capability(
                &actor,
                &ResourceRef::Workspace,
                Verb::Admin,
                &caps,
                Some(seconds),
            ) != CapDecision::Allow
        {
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
        let (_, capabilities, person) = self.permissions(caller.device_public_key, now)?;
        self.require_governance_writer(key, now)?;
        if let WorkspaceMembershipCommand::GrantAppAuthorship { app } = command {
            let mut capability =
                CapOverride::grant(Subject::app(app), Scope::Workspace, Verb::Write)
                    .with_id(format!("app-author:{app}"));
            capability.granted_by = Some(Principal::Person(person));
            return self.put_governed_capabilities(&[capability], key, now);
        }
        let subject = Subject::agent(command.agent().ok_or("missing agent membership target")?);
        let room_scope = Scope::Room(self.core.room.clone());
        let removals = capabilities
            .iter()
            .filter(|cap| {
                cap.subject == subject
                    && (matches!(command, WorkspaceMembershipCommand::RemoveAgent { .. })
                        || cap.scope == room_scope)
            })
            .map(|cap| cap.id.clone())
            .collect::<Vec<_>>();
        // Work on a clone: an invalid later addition must not leave a removal
        // applied to the caller's candidate.
        let mut candidate = self.clone();
        let removed = candidate.remove_governed_capabilities(&removals, key, now)?;
        let added = match command {
            WorkspaceMembershipCommand::AddAgent { agent, preset } => {
                let mut capability = CapOverride::membership(
                    subject,
                    self.core.room.clone(),
                    preset.admission_verb(),
                )
                .with_id(format!("agent-member:{agent}:{}", preset.id()));
                capability.granted_by = Some(collab_types::caps::Principal::Person(person));
                candidate.put_governed_capabilities(&[capability], key, now)?
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
