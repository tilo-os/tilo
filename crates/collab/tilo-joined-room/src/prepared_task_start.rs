//! Owner custody for first-use native Start, separate from enrollment admission.
use crate::{NativeWorkspaceOwner, RoomId, SystemTime};
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_user_identity::DeviceCert;

/// In-memory capability; neither wire input nor a durable grant.
pub struct PreparedNativeTaskStart {
    request: OwnerCommitRequest,
    frontier: tilo_peer_state::PreparedTaskStart,
    room: RoomId,
    continuity: u64,
    governor: DeviceCert,
    model: crate::NativeModelContract,
}

impl NativeWorkspaceOwner {
    /// Freeze the original person's ceiling and exact task frontier without a run.
    /// # Errors
    /// Refuses non-governor custody, invalid requests and stale/unauthorized queues.
    pub fn prepare_task_start(
        &self,
        request: OwnerCommitRequest,
        model_start: &dyn crate::NativeModelStartAuthority,
        now: SystemTime,
    ) -> Result<PreparedNativeTaskStart, String> {
        request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid Start room")?;
        let CommitOperation::WorkspaceTaskCommand {
            authority, command, ..
        } = &request.operation
        else {
            return Err("Start requires a task command".into());
        };
        let _turn = self.0.mutation.lock();
        let (frontier, continuity, governor, model) = self.with_current(room, |view| {
            if view.device() != view.governor() {
                return Err("prepared Start requires the actual governor".into());
            }
            let tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person { caller } =
                authority
            else {
                return Err("only a person may prepare Start".into());
            };
            let tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun {
                task_id, ..
            } = command
            else {
                return Err("prepared Start requires ApproveRun".into());
            };
            let model = model_start.approved_model_start(view, task_id, caller, now)?;
            validate_model(&model, &request.room, task_id)?;
            let snapshot = model_snapshot(&model)?;
            let frontier = view
                .peer
                .prepare_workspace_task_start(authority, command, &snapshot, now)?;
            Ok((
                frontier,
                view.record.continuity,
                view.governor().clone(),
                model,
            ))
        })?;
        Ok(PreparedNativeTaskStart {
            request,
            frontier,
            room,
            continuity,
            governor,
            model,
        })
    }

    /// Revalidate admission, bind and approve in one serialized owner publication.
    /// The trusted hook resamples configuration/source custody inside this turn;
    /// it must exactly match the selection consciously frozen before launch.
    /// # Errors
    /// Refuses changed custody, task/Stop frontier, certificates or model setup.
    pub fn approve_prepared_task_start(
        &self,
        prepared: &PreparedNativeTaskStart,
        executor: &DeviceCert,
        key: &ed25519_dalek::SigningKey,
        model_start: &dyn crate::NativeModelStartAuthority,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(prepared.room, |view| Ok(view.clone()))?;
        if candidate.record.continuity != prepared.continuity
            || candidate.governor() != &prepared.governor
            || candidate.device() != &prepared.governor
            || key.verifying_key().to_bytes() != prepared.governor.device_public_key
        {
            return Err("prepared Start governor custody changed".into());
        }
        let CommitOperation::WorkspaceTaskCommand {
            invocation,
            command,
            authority,
        } = &prepared.request.operation
        else {
            return Err("invalid prepared Start request".into());
        };
        let agent = command.agent().ok_or("Start has no executor")?;
        if executor.user_public_key != prepared.governor.user_public_key
            || !candidate
                .peer
                .certificates()
                .iter()
                .any(|cert| cert == executor)
            || !candidate
                .peer
                .governance()
                .governed_agents(now)?
                .iter()
                .any(|actor| {
                    actor.agent_id == agent
                        && actor.device_public_key == executor.device_public_key
                        && actor.acting
                })
        {
            return Err("Start executor is not the exact admitted managed actor".into());
        }
        candidate
            .peer
            .permissions(executor.device_public_key, now)?;
        let tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person { caller } = authority
        else {
            return Err("prepared Start person absent".into());
        };
        let tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun { task_id, .. } =
            command
        else {
            return Err("prepared Start command changed".into());
        };
        let model = model_start.approved_model_start(&candidate, task_id, caller, now)?;
        validate_model(&model, &prepared.request.room, task_id)?;
        if model != prepared.model {
            return Err("prepared Start model selection or source changed".into());
        }
        let binding = prepared.request.binding()?;
        candidate.peer.apply_prepared_workspace_task_start(
            &prepared.frontier,
            *invocation,
            binding.request_digest,
            now,
        )?;
        // Only the digest is in the shared control. The complete original
        // source/profile contract follows the existing owner-private boundary.
        let state = tilo_peer_state::workspace_task::snapshot(
            candidate
                .peer
                .admitted_room()
                .ok_or("prepared Start room absent")?,
        )?;
        let mut full = state
            .controls
            .get(task_id)
            .and_then(|control| control.model_authority.clone())
            .ok_or("prepared Start model authority absent")?;
        full.contract = serde_json::to_value(&prepared.model).map_err(|e| e.to_string())?;
        candidate
            .record
            .private_task_text
            .model_authorities
            .insert(task_id.clone(), full);
        candidate.record.private_task_text.validate()?;
        // Migration remains in the same unpublished candidate as approval.
        crate::enrollment::ensure_private_task_baseline(&mut candidate, key, now)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: candidate.generation(),
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        receipt.validate_for(&prepared.request)?;
        self.publish_room(prepared.room, candidate, None)?;
        Ok(receipt)
    }
}

fn validate_model(
    model: &crate::NativeModelContract,
    room: &str,
    task: &str,
) -> Result<(), String> {
    model.source_for(room, task)?;
    if model.selection.app.is_empty()
        || model.selection.provider.is_empty()
        || model.selection.model.is_empty()
        || !model.selection.contract.is_object()
    {
        return Err("prepared MODEL full selection snapshot incomplete".into());
    }
    Ok(())
}

fn model_snapshot(
    model: &crate::NativeModelContract,
) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskModelStart, String> {
    Ok(tilo_peer_state::workspace_task::WorkspaceTaskModelStart {
        request_model: model.request_model.clone(),
        kind: model.kind,
        contract: serde_json::to_value(model).map_err(|e| e.to_string())?,
    })
}
