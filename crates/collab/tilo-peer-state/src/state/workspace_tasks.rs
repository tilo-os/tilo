use super::{Role, StatePeer, SystemTime};
use crate::workspace_task::{
    AdmittedTaskActor, AppliedTaskCommand, WorkspaceTaskAuthority, WorkspaceTaskCommand,
};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};

impl StatePeer {
    /// Apply one typed task command against the current admitted room document.
    /// The caller or host certificate is rechecked here, in the same serialized
    /// owner turn as the mutation; strings in the command confer no authority.
    ///
    /// # Errors
    /// The authority is not current, the command is stale or malformed, or
    /// the admitted room document cannot be updated.
    pub fn apply_workspace_task_command(
        &mut self,
        invocation: uuid::Uuid,
        request_digest: [u8; 32],
        authority: &WorkspaceTaskAuthority,
        command: &WorkspaceTaskCommand,
        now: SystemTime,
    ) -> Result<(bool, tilo_desktop_model::WorkspaceTaskMutationResult), String> {
        let certificate = authority.certificate();
        if self
            .core
            .certificates
            .get(&certificate.device_public_key)
            .map(AsRef::as_ref)
            != Some(certificate)
        {
            return Err("workspace task certificate is not current admitted evidence".into());
        }
        self.certificate(&certificate.device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "workspace task clock is before the Unix epoch")?
            .as_secs();
        let room = self
            .core
            .room_doc
            .as_deref()
            .ok_or("workspace task room document is absent")?;
        let (_, owner, caps) = crate::room::extract_governed_fields(room);
        let agent_role = |agent: &str| {
            let mut workspace = tilo_desktop_model::Workspace::new(
                tilo_desktop_model::WorkspaceId(1),
                "task authority",
            );
            workspace.owner.clone_from(&owner);
            workspace.capabilities.clone_from(&caps);
            workspace.role_of_agent_at(agent, seconds)
        };
        if let Some(agent) = command.agent()
            && agent_role(agent).is_none_or(|role| role == Role::Denied)
        {
            return Err("workspace task agent is not a current room member".into());
        }
        let actor = match authority {
            WorkspaceTaskAuthority::Person { caller } => {
                let (role, caps, person) = self.permissions(caller.device_public_key, now)?;
                let actor =
                    Actor::person(Some(role), person.clone()).in_room(self.core.room.clone());
                if !matches!(role, Role::Owner | Role::Editor)
                    || decide_capability(
                        &actor,
                        &ResourceRef::Workspace,
                        Verb::Write,
                        &caps,
                        Some(seconds),
                    ) != CapDecision::Allow
                {
                    return Err("workspace task person lacks write authority".into());
                }
                AdmittedTaskActor::Person {
                    id: person,
                    device: caller.device_public_key,
                }
            }
            WorkspaceTaskAuthority::Host { attestor } => {
                if !attestor.capabilities.can_act_as_host
                    && attestor.device_public_key != self.core.authority.device_public_key
                {
                    return Err("workspace task attestor is not a host".into());
                }
                AdmittedTaskActor::Host {
                    device: attestor.device_public_key,
                }
            }
            WorkspaceTaskAuthority::Agent { attestor, agent_id } => {
                if (!attestor.capabilities.can_act_as_host
                    && attestor.device_public_key != self.core.authority.device_public_key)
                    || agent_role(agent_id).is_none_or(|role| role == Role::Denied)
                {
                    return Err("workspace task agent lacks current host attestation".into());
                }
                AdmittedTaskActor::Agent {
                    id: agent_id.clone(),
                    device: attestor.device_public_key,
                }
            }
        };
        let mut candidate = room.clone();
        let AppliedTaskCommand { changed, result } = crate::workspace_task::apply(
            &mut candidate,
            invocation,
            request_digest,
            &actor,
            command,
            i64::try_from(seconds).unwrap_or(i64::MAX),
        )?;
        if changed {
            self.core.room_doc = Some(candidate.into());
        }
        Ok((changed, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SeedState, seed_room};
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{WorkspaceTaskRoute, WorkspaceTaskStatus};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    use web_time::Duration;

    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn certificate(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, root).unwrap();
        certificate
    }

    fn peer() -> (StatePeer, DeviceCert, DeviceCert, SystemTime) {
        let governor_root = SigningKey::from_bytes(&[1; 32]);
        let governor_key = SigningKey::from_bytes(&[2; 32]);
        let governor = certificate(&governor_root, &governor_key, "Governor");
        let other_root = SigningKey::from_bytes(&[3; 32]);
        let other_key = SigningKey::from_bytes(&[4; 32]);
        let other = certificate(&other_root, &other_key, "Other primary");
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(2))
            .unwrap();
        let mut peer = StatePeer::new(
            ROOM,
            governor.clone(),
            vec![governor.clone(), other.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            SeedState {
                room_changes: seed_room(ROOM, &governor, &[governor.clone(), other.clone()])
                    .unwrap(),
                documents: vec![],
            },
            &governor_key,
            now,
        )
        .unwrap();
        peer.put_governed_capabilities(
            &[
                CapOverride::grant(Subject::agent("scribe"), Scope::Workspace, Verb::Write)
                    .with_id("agent-scribe"),
            ],
            &governor_key,
            now,
        )
        .unwrap();
        (peer, governor, other, now)
    }

    #[test]
    fn current_owner_with_read_restriction_cannot_queue_a_task() {
        let (mut peer, governor, _, now) = peer();
        let restriction = CapOverride::new(
            Subject::Role(Role::Owner),
            Scope::Workspace,
            collab_types::caps::Effect::Restrict,
            Verb::Read,
        )
        .with_id("read-only-owner");
        peer.put_governed_capabilities(&[restriction], &SigningKey::from_bytes(&[2; 32]), now)
            .unwrap();
        let before = peer.durable().unwrap();
        let result = peer.apply_workspace_task_command(
            uuid::Uuid::from_u128(1),
            [1; 32],
            &WorkspaceTaskAuthority::Person { caller: governor },
            &WorkspaceTaskCommand::Queue {
                title: "Must not execute".into(),
                assigned_agent: "scribe".into(),
                route: WorkspaceTaskRoute::Model,
                resource: None,
            },
            now,
        );
        assert_eq!(
            result.unwrap_err(),
            "workspace task person lacks write authority"
        );
        assert_eq!(peer.durable().unwrap(), before);
        assert!(
            crate::workspace_task::snapshot(peer.admitted_room().unwrap())
                .unwrap()
                .tasks
                .is_empty()
        );
    }

    #[test]
    fn exact_primary_governor_may_supervise_but_another_primary_may_not() {
        let (mut peer, governor, other, now) = peer();
        assert!(!governor.capabilities.can_act_as_host);
        assert!(!other.capabilities.can_act_as_host);
        let queue = WorkspaceTaskCommand::Queue {
            title: "Prepare the release notes".into(),
            assigned_agent: "scribe".into(),
            route: WorkspaceTaskRoute::Model,
            resource: None,
        };
        let (_, result) = peer
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(1),
                [1; 32],
                &WorkspaceTaskAuthority::Person {
                    caller: governor.clone(),
                },
                &queue,
                now,
            )
            .unwrap();
        let tilo_desktop_model::WorkspaceTaskMutationResult::Queued { task_id } = result else {
            panic!("queue result")
        };
        let start = WorkspaceTaskCommand::StartRun {
            task_id: task_id.clone(),
            agent_id: "scribe".into(),
        };
        let refused = peer
            .apply_workspace_task_command(
                uuid::Uuid::from_u128(2),
                [2; 32],
                &WorkspaceTaskAuthority::Host { attestor: other },
                &start,
                now,
            )
            .unwrap_err();
        assert!(refused.contains("attestor is not a host"), "{refused}");
        peer.apply_workspace_task_command(
            uuid::Uuid::from_u128(3),
            [3; 32],
            &WorkspaceTaskAuthority::Host { attestor: governor },
            &start,
            now,
        )
        .unwrap();
        let task = crate::workspace_task::snapshot(peer.admitted_room().unwrap())
            .unwrap()
            .tasks
            .into_iter()
            .find(|task| task.id == task_id)
            .unwrap();
        assert_eq!(task.status, WorkspaceTaskStatus::InProgress);
    }
}
