//! Private, non-serializable authority retained between conscious Start and registration.
use super::{
    ActionKind, AdmittedTaskActor, StatePeer, SystemTime, TreePath, WorkspaceTaskAuthority,
    WorkspaceTaskCommand,
};
use tilo_desktop_model::{WorkspaceTask, WorkspaceTaskControl, WorkspaceTaskExecutionAuthority};

/// An exact queued frontier and its original person's permission ceiling.
/// This capability is never accepted from the wire.
pub struct PreparedTaskStart {
    authority: WorkspaceTaskAuthority,
    command: WorkspaceTaskCommand,
    task: WorkspaceTask,
    control: WorkspaceTaskControl,
    ceiling: WorkspaceTaskExecutionAuthority,
    model_start: crate::workspace_task::WorkspaceTaskModelStart,
}

impl StatePeer {
    /// Freeze Start without requiring the not-yet-provisioned executor to exist.
    /// # Errors
    /// Refuses stale queues, non-person requests and unauthorized callers.
    pub fn prepare_workspace_task_start(
        &self,
        authority: &WorkspaceTaskAuthority,
        command: &WorkspaceTaskCommand,
        model_start: &crate::workspace_task::WorkspaceTaskModelStart,
        now: SystemTime,
    ) -> Result<PreparedTaskStart, String> {
        let WorkspaceTaskAuthority::Person { caller } = authority else {
            return Err("only a person may prepare Start".into());
        };
        let WorkspaceTaskCommand::ApproveRun {
            task_id,
            agent_id,
            expected_run_epoch,
            after_revision,
        } = command
        else {
            return Err("prepared Start requires ApproveRun".into());
        };
        if self
            .core
            .certificates
            .get(&caller.device_public_key)
            .map(AsRef::as_ref)
            != Some(caller)
        {
            return Err("Start caller certificate changed".into());
        }
        self.certificate(&caller.device_public_key, now)?;
        let member = self.permissions(caller.device_public_key, now)?;
        if !member.allows(&TreePath::room(&self.core.room), ActionKind::Undoable, now) {
            return Err("Start caller lacks write authority".into());
        }
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("task document absent")?;
        let snapshot = crate::workspace_task::snapshot(doc)?;
        let task = snapshot
            .tasks
            .into_iter()
            .find(|task| task.id == *task_id)
            .ok_or("task absent")?;
        let control = crate::workspace_task::controls_of(doc)?
            .remove(task_id)
            .ok_or("task control absent")?;
        if agent_id.is_empty()
            || task.route != tilo_desktop_model::WorkspaceTaskRoute::Model
            || task.status != tilo_desktop_model::WorkspaceTaskStatus::Queued
            || task
                .assigned_agent
                .as_ref()
                .is_some_and(|agent| agent != agent_id)
            || task
                .resource
                .as_ref()
                .is_some_and(|resource| resource.agent_request.is_some())
            || control.active_run.is_some()
            || control.run_epoch != *expected_run_epoch
            || control.directions.last().map_or(0, |value| value.revision) != *after_revision
        {
            return Err("queued Start frontier changed".into());
        }
        self.check_task_metadata_authority(
            &WorkspaceTaskCommand::BindExecutor {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                expected_run_epoch: *expected_run_epoch,
                after_revision: *after_revision,
            },
            &AdmittedTaskActor::Person {
                id: member.person.clone(),
                device: caller.device_public_key,
            },
            doc,
            now,
        )?;
        let mut scope = TreePath::room(&self.core.room);
        if let Some(resource) = &task.resource {
            scope = scope
                .app(
                    resource
                        .app_id
                        .as_ref()
                        .ok_or("task scope lacks app identity")?,
                )
                .kind(&resource.kind)
                .resource(&resource.id);
        }
        self.validate_prepared_model_start(model_start, &member, now)?;
        let ceiling = WorkspaceTaskExecutionAuthority {
            epoch: expected_run_epoch
                .checked_add(1)
                .ok_or("task epoch exhausted")?,
            scope,
            starter: member.standing(),
        };
        Ok(PreparedTaskStart {
            authority: authority.clone(),
            command: command.clone(),
            task,
            control,
            ceiling,
            model_start: model_start.clone(),
        })
    }

    fn validate_prepared_model_start(
        &self,
        model_start: &crate::workspace_task::WorkspaceTaskModelStart,
        member: &crate::state::MemberAuthority,
        now: SystemTime,
    ) -> Result<(), String> {
        if model_start.request_model.room_id() != Some(self.core.room.as_str())
            || model_start.request_model.is_root()
            || model_start
                .request_model
                .segments()
                .iter()
                .any(|segment| segment.id().is_none())
            || !model_start.contract.is_object()
            || !member.allows(&model_start.request_model, model_start.kind, now)
        {
            return Err("prepared Start model source or permission invalid".into());
        }
        Ok(())
    }

    /// Revalidate and atomically bind/approve on an unpublished candidate.
    /// # Errors
    /// Refuses any frontier or caller/actor admission change. Never widens the ceiling.
    pub fn apply_prepared_workspace_task_start(
        &mut self,
        prepared: &PreparedTaskStart,
        invocation: uuid::Uuid,
        request_digest: [u8; 32],
        now: SystemTime,
    ) -> Result<(), String> {
        let WorkspaceTaskCommand::ApproveRun {
            task_id,
            agent_id,
            expected_run_epoch,
            after_revision,
        } = &prepared.command
        else {
            return Err("invalid prepared Start".into());
        };
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("task document absent")?;
        let current = crate::workspace_task::snapshot(doc)?;
        if current.tasks.iter().find(|task| task.id == *task_id) != Some(&prepared.task)
            || crate::workspace_task::controls_of(doc)?.get(task_id) != Some(&prepared.control)
        {
            return Err("prepared Start frontier changed".into());
        }
        // Repeat all current certificate, write and model checks, but retain the
        // original standing below rather than adopting any later policy widening.
        self.prepare_workspace_task_start(
            &prepared.authority,
            &prepared.command,
            &prepared.model_start,
            now,
        )?;
        let mut candidate = self.clone();
        if prepared.task.assigned_agent.is_none() {
            candidate.workspace_mut().apply_workspace_task_command(
                uuid::Uuid::new_v4(),
                request_digest,
                &prepared.authority,
                &WorkspaceTaskCommand::BindExecutor {
                    task_id: task_id.clone(),
                    agent_id: agent_id.clone(),
                    expected_run_epoch: *expected_run_epoch,
                    after_revision: *after_revision,
                },
                now,
            )?;
        }
        candidate
            .workspace_mut()
            .apply_workspace_task_command_with_prepared(
                invocation,
                request_digest,
                &prepared.authority,
                &prepared.command,
                now,
                crate::workspace_task::WorkspaceTaskOwnerPreparation {
                    model_start: Some(&prepared.model_start),
                    private_retry: None,
                },
                Some(&prepared.ceiling),
            )?;
        *self = candidate;
        Ok(())
    }
}
