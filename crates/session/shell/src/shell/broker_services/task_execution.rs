//! Native task reports use the already enrolled host and durable owner.
use super::NativeOwnerCommitService;
use tilo_action_broker::services::{
    NativeTaskActionRequirement, NativeTaskBinding, WorkspaceTaskExecution,
};
use tilo_peer_state::{
    owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService},
    workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand, WorkspaceTaskSnapshot},
};

impl WorkspaceTaskExecution for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        OwnerCommitService::requires_room(self, room)
    }

    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String> {
        tilo_action_broker::services::WorkspaceMemberships::task_snapshot(self, room)
    }

    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
        let owner = self.owner.as_ref().ok_or("native task owner unavailable")?;
        owner.with_current(room_id, |view| {
            view.peer()
                .check_task_execution_actor(agent, None, std::time::SystemTime::now())?;
            tilo_peer_state::workspace_task::snapshot(
                view.peer()
                    .admitted_room()
                    .ok_or("native task document unavailable")?,
            )
        })
    }

    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let prepared = (|| {
            let room_id = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
            let owner = self.owner.as_ref().ok_or("native task owner unavailable")?;
            let host = owner.with_current(room_id, |view| {
                view.host()
                    .cloned()
                    .or_else(|| {
                        (view.device().device_public_key == view.governor().device_public_key)
                            .then(|| view.governor().clone())
                    })
                    .ok_or_else(|| "task host attestation unavailable".into())
            })?;
            // Exact retries reuse the owner's retained receipt, across reconnects.
            let bytes = serde_json::to_vec(&(room, &binding.task_id, binding.epoch, &command))
                .map_err(|error| error.to_string())?;
            let hash = blake3::hash(&bytes);
            let id = *hash
                .as_bytes()
                .first_chunk::<16>()
                .ok_or("invalid task report digest")?;
            Ok::<_, String>(OwnerCommitRequest {
                room: room.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::from_bytes(id),
                    authority: if matches!(command, WorkspaceTaskCommand::Abandon { .. }) {
                        WorkspaceTaskAuthority::Host { attestor: host }
                    } else {
                        WorkspaceTaskAuthority::Agent {
                            attestor: host,
                            agent_id: binding.agent_id,
                        }
                    },
                    command,
                },
            })
        })();
        match prepared {
            Ok(request) => {
                tilo_action_broker::services::WorkspaceMemberships::execute(self, request)
            }
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let room_id = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
        let owner = self.owner.as_ref().ok_or("native task owner unavailable")?;
        owner.with_current(room_id, |view| {
            let now = std::time::SystemTime::now();
            view.peer().check_task_execution_actor(agent, None, now)?;
            requirement.check(view.peer(), room, agent, now)
        })
    }

    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<
            tilo_action_broker::collab_broker_capnp::InvocationJob,
            String,
        >,
    ) -> Result<tilo_action_broker::collab_broker_capnp::InvocationJob, String> {
        let room = super::super::state::RoomId::parse(room).ok_or("invalid task room")?;
        let owner = self.owner.as_ref().ok_or("native task owner unavailable")?;
        if let Some(network) = &self.network {
            network.check_native_custody(owner, room)?;
        }
        owner.with_task_action_admission(
            room,
            &binding.task_id,
            &binding.agent_id,
            binding.epoch,
            binding.revision,
            || {
                self.authorize_action(&room.to_string(), &binding.agent_id, requirement)?;
                action()
            },
        )
    }
}
