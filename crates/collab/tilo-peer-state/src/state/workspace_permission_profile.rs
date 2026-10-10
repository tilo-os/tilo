use super::{StatePeer, SystemTime};
use crate::workspace_permission_profile::WorkspacePermissionProfileCommand;
use collab_types::caps::{Actor, CapDecision, Principal, ResourceRef, Verb, decide_capability};
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Apply one profile command only under current original Person administration.
    /// # Errors
    /// Refuses replaced caller evidence, restricted administration or stale signing custody.
    pub fn apply_workspace_permission_profile_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspacePermissionProfileCommand,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        command.validate()?;
        if caller.capabilities.can_act_as_host
            || self
                .core
                .certificates
                .get(&caller.device_public_key)
                .map(AsRef::as_ref)
                != Some(caller)
        {
            return Err("permission profile caller is not current Person evidence".into());
        }
        self.certificate(&caller.device_public_key, now)?;
        self.check_workspace_administration(caller.device_public_key, now)?;
        self.require_governance_writer(key, now)?;
        let (role, caps, person) = self.permissions(caller.device_public_key, now)?;
        let action = match command {
            WorkspacePermissionProfileCommand::Save { .. } => "profile.save",
            WorkspacePermissionProfileCommand::Assign { .. } => "profile.assign",
        };
        let room = tilo_desktop_model::RoomId::parse(&self.core.room)
            .ok_or("invalid permission profile room")?;
        let profile = match command {
            WorkspacePermissionProfileCommand::Assign { profile_name, .. } => {
                let doc = self
                    .admitted_room()
                    .ok_or("permission profile room absent")?;
                tilo_desktop_model::projection::workspace_permission_profiles_of(doc)
                    .into_iter()
                    .find(|profile| profile.name.eq_ignore_ascii_case(profile_name.trim()))
            }
            WorkspacePermissionProfileCommand::Save { .. } => None,
        };
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "permission profile clock unavailable")?
            .as_secs();
        let actor = Actor::person(Some(role), person.clone()).in_room(self.core.room.clone());
        let allowed = |resource| {
            decide_capability(
                &actor,
                &ResourceRef::Action {
                    app: tilo_desktop_model::WORKSPACE_GOVERNANCE_APP_ID.into(),
                    resource,
                    action: action.into(),
                },
                Verb::Admin,
                &caps,
                Some(seconds),
            ) == CapDecision::Allow
        };
        if !allowed(tilo_desktop_model::governance_resource_id(&room))
            && !profile.as_ref().is_some_and(|profile| {
                allowed(tilo_desktop_model::permission_profile_resource_id(
                    &room,
                    &profile.name,
                ))
            })
        {
            return Err("permission profile action is currently restricted".into());
        }
        match command {
            WorkspacePermissionProfileCommand::Save { profile } => {
                self.save_governed_profile(profile, key, now)
            }
            WorkspacePermissionProfileCommand::Assign {
                subject, expiry, ..
            } => {
                let profile =
                    profile.ok_or("permission profile is not present in signed room policy")?;
                let assigned = profile
                    .assign_to(subject, &Principal::Person(person), *expiry)
                    .map_err(|_| "invalid permission profile assignment")?;
                let prefix = assigned
                    .first()
                    .and_then(|cap| cap.id.rsplit_once(':'))
                    .map(|(prefix, _)| format!("{prefix}:"))
                    .ok_or("permission profile assignment is empty")?;
                let stale = caps
                    .iter()
                    .filter(|cap| cap.subject == *subject && cap.id.starts_with(&prefix))
                    .map(|cap| cap.id.clone())
                    .collect::<Vec<_>>();
                let mut candidate = self.clone();
                candidate.remove_governed_capabilities(&stale, key, now)?;
                candidate.put_governed_capabilities(&assigned, key, now)?;
                *self = candidate;
                Ok(())
            }
        }
    }

    fn save_governed_profile(
        &mut self,
        profile: &collab_types::PermissionProfile,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("permission profile room absent")?;
        let before = doc.get_heads();
        let profiles = tilo_desktop_model::projection::workspace_permission_profiles_of(&doc);
        // Profile lookup is case-insensitive but assignment IDs bind the canonical
        // stored name. A spelling-only edit must not orphan older grants.
        let name = profiles
            .iter()
            .find(|current| current.name.eq_ignore_ascii_case(profile.name.trim()))
            .map_or_else(
                || profile.name.trim().to_owned(),
                |existing| existing.name.clone(),
            );
        let profile = collab_types::PermissionProfile {
            name,
            ..profile.clone()
        };
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        tilo_desktop_model::projection::upsert_workspace_permission_profile(&mut doc, &profile)
            .map_err(|_| "permission profile encoding failed")?;
        doc.commit();
        self.apply_governance_doc(doc, &before, key, now)?;
        Ok(())
    }
}
