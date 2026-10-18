//! Content binding for an authenticated owner-private execution envelope.

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::task::{
        WorkspaceTaskActiveRun, WorkspaceTaskApproval, WorkspaceTaskControl,
        WorkspaceTaskDirection, WorkspaceTaskExecutionAuthority,
    };

    #[test]
    fn desired_private_digest_precedes_applied_without_losing_content_or_run_fences() {
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let mut private = NativePrivateTaskInstructions {
            instruction: Some("Inspect the document".into()),
            ..Default::default()
        };
        private.directions.insert(1, "Summarize the changes".into());
        let reference = tilo_peer_state::workspace_task::private_instruction_reference;
        let control = WorkspaceTaskControl {
            run_epoch: 1,
            private_instruction_ref: Some(reference(private.instruction.as_ref().unwrap())),
            execution_authority: Some(WorkspaceTaskExecutionAuthority {
                epoch: 1,
                scope: tilo_action_policy::TreePath::room(room.to_string()),
                starter: tilo_action_policy::Standing::person(
                    tilo_action_policy::RoomPolicy {
                        room: room.to_string(),
                        owner: Some("owner".into()),
                        members: Default::default(),
                        joined: Default::default(),
                        grants: Vec::new(),
                    },
                    "owner",
                ),
            }),
            active_run: Some(WorkspaceTaskActiveRun {
                epoch: 1,
                agent_id: "scribe".into(),
                started_at: 1,
                applied_direction_revision: 0,
                approval: Some(WorkspaceTaskApproval {
                    person: "owner".into(),
                    device: [1; 32],
                    approved_at: 1,
                    instruction_revision: 0,
                }),
            }),
            directions: vec![WorkspaceTaskDirection::owner_admitted(
                1,
                uuid::Uuid::from_u128(1),
                "owner".into(),
                reference(&private.directions[&1]),
                2,
            )],
            ..Default::default()
        };
        let task = serde_json::from_value(serde_json::json!({
            "id": "task", "title": "Review", "created_by": "owner",
            "assigned_agent": "scribe", "route": "model", "status": "in_progress",
            "resource": null, "created_at": 1, "updated_at": 2,
        }))
        .unwrap();
        let mut snapshot = WorkspaceTaskSnapshot {
            tasks: vec![task],
            controls: [("task".into(), control)].into(),
            grants: Vec::new(),
        };
        let mut binding = NativeTaskBinding {
            task_id: "task".into(),
            agent_id: "scribe".into(),
            epoch: 1,
            revision: 1,
            starter: Some("owner".into()),
        };
        let prepared = private_run_digest(room, &snapshot, &binding, &private).unwrap();
        snapshot
            .controls
            .get_mut("task")
            .unwrap()
            .active_run
            .as_mut()
            .unwrap()
            .applied_direction_revision = 1;
        assert_eq!(
            private_run_digest(room, &snapshot, &binding, &private).unwrap(),
            prepared
        );
        private.directions.insert(1, "Different direction".into());
        assert!(private_run_digest(room, &snapshot, &binding, &private).is_err());
        private.directions.insert(1, "Summarize the changes".into());
        binding.revision = 0;
        assert!(private_run_digest(room, &snapshot, &binding, &private).is_err());
        binding.revision = 1;
        binding.epoch = 2;
        assert!(private_run_digest(room, &snapshot, &binding, &private).is_err());
        binding.epoch = 1;
        binding.starter = Some("another person".into());
        assert!(private_run_digest(room, &snapshot, &binding, &private).is_err());
    }
}
use crate::task_policy::NativeTaskBinding;
use crate::{NativeCommittedView, NativePrivateTaskInstructions, NativeWorkspaceOwner, RoomId};
use std::time::SystemTime;
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskSnapshot};

/// Hash an authenticated, internally consistent private run snapshot. Callers
/// must obtain both inputs from the same governor generation, never room gossip.
/// Public task titles, progress, confirmations and timestamps are not execution
/// instructions and deliberately do not participate. This binds the latest
/// desired instructions, including a prepared MODEL turn before Applied. It is
/// not a dispatch permit: action admission independently requires Applied at
/// that exact revision.
/// # Errors
/// Missing private custody, mismatched instruction references or stale run scope.
pub fn private_run_digest(
    room: RoomId,
    snapshot: &WorkspaceTaskSnapshot,
    binding: &NativeTaskBinding,
    private: &NativePrivateTaskInstructions,
) -> Result<[u8; 32], String> {
    let task = snapshot
        .tasks
        .iter()
        .find(|task| task.id == binding.task_id)
        .ok_or("private run task absent")?;
    let control = snapshot
        .controls
        .get(&binding.task_id)
        .ok_or("private run control absent")?;
    let run = control.active_run.as_ref().ok_or("private run ended")?;
    let approval = run.approval.as_ref().ok_or("private run starter absent")?;
    let authority = control
        .execution_authority
        .as_ref()
        .ok_or("private run execution authority absent")?;
    if task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
        || task.assigned_agent.as_deref() != Some(binding.agent_id.as_str())
        || run.agent_id != binding.agent_id
        || run.epoch != binding.epoch
        || control.run_epoch != binding.epoch
        || authority.epoch != binding.epoch
        || control
            .directions
            .last()
            .map_or(0, |direction| direction.revision)
            != binding.revision
        || binding.starter.as_deref() != Some(approval.person.as_str())
    {
        return Err("private run binding is stale".into());
    }
    if let Some(reference) = &control.private_instruction_ref {
        let instruction = private
            .instruction
            .as_deref()
            .or_else(|| {
                private
                    .agent_request
                    .as_ref()
                    .map(|request| request.instruction.as_str())
            })
            .ok_or("private run instruction unavailable")?;
        if tilo_peer_state::workspace_task::private_instruction_reference(instruction) != *reference
            || private.agent_request.as_ref().is_some_and(|request| {
                tilo_peer_state::workspace_task::private_instruction_reference(&request.instruction)
                    != *reference
            })
        {
            return Err("private run instruction binding differs".into());
        }
    }
    let mut directions = std::collections::BTreeMap::new();
    for direction in &control.directions {
        let text = private
            .directions
            .get(&direction.revision)
            .ok_or("private run direction unavailable")?;
        if tilo_peer_state::workspace_task::private_instruction_reference(text) != direction.text {
            return Err("private run direction binding differs".into());
        }
        directions.insert(
            direction.revision,
            (&direction.author, direction.invocation, text),
        );
    }
    // The private request stays intact: never reconstruct it from a redacted
    // public resource or replace its original instruction with later directions.
    let mut resource = task.resource.clone();
    if let Some(resource) = &mut resource {
        resource.agent_request = None;
    }
    let envelope = serde_json::json!({
        "domain": "tilo-native-private-run-v1",
        "room": room,
        "task": binding.task_id,
        "agent": binding.agent_id,
        "epoch": binding.epoch,
        "revision": binding.revision,
        "starter": { "person": approval.person, "device": approval.device },
        "execution_authority": authority,
        "execution_budget": control.execution_budget,
        "route": task.route,
        "resource": resource,
        "subject": task.subject,
        "instruction": private.instruction,
        "request": private.agent_request,
        "directions": directions,
    });
    let bytes = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

impl NativeWorkspaceOwner {
    /// Compute within the caller's already-authenticated owner transaction. No
    /// generation boundary separates the private content and execution scope.
    /// # Errors
    /// The private envelope is absent or the exact run binding is no longer current.
    pub fn current_private_run_digest(
        view: &NativeCommittedView,
        binding: &NativeTaskBinding,
    ) -> Result<[u8; 32], String> {
        let snapshot = view.task_policy_snapshot()?;
        let private = view
            .record
            .private_task_text
            .instructions()
            .get(&binding.task_id)
            .ok_or("private run instruction custody absent")?;
        private_run_digest(view.record.room, &snapshot, binding, private)
    }

    /// Authenticated private read for a broker or gateway. The generation fence
    /// prevents mixing authorization and content from different owner snapshots.
    /// # Errors
    /// Reader admission ended, the generation changed or private custody is absent.
    pub fn workspace_task_private_run_digest(
        &self,
        room: RoomId,
        binding: &NativeTaskBinding,
        authority: &WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<[u8; 32], String> {
        if !matches!(authority, WorkspaceTaskAuthority::Agent { agent_id, .. } if agent_id == &binding.agent_id)
        {
            return Err("private run digest requires its authenticated agent".into());
        }
        let generation = self.with_current(room, |view| Ok(view.generation()))?;
        self.workspace_task_private_instructions(
            room,
            &binding.task_id,
            binding.epoch,
            authority,
            now,
        )?;
        self.with_current(room, |view| {
            if view.generation() != generation {
                return Err("private run authorization changed; retry the read".into());
            }
            Self::current_private_run_digest(view, binding)
        })
    }
}
