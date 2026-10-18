//! Host-owned adaptation of authenticated task channels to the canonical owner.
//! No caller certificate, actor label or new grant is accepted from task arguments.
use crate::collab_broker_capnp::InvocationJob;
use tilo_peer_state::workspace_task::{WorkspaceTaskCommand, WorkspaceTaskSnapshot};

#[path = "task_admission_transfer.rs"]
mod admission_transfer;
pub use admission_transfer::{
    NativeTaskAdmissionCaller, NativeTaskAdmissionFuture, NativeTaskAdmissionReady,
    NativeTaskAdmissionTransfer,
};

#[cfg(test)]
#[path = "task_execution_tests.rs"]
mod tests;

pub use tilo_joined_room::task_policy::{
    NativeTaskActionRequirement, NativeTaskBinding, NativeTaskInvocation,
};

/// Broker actor adaptation stays here: canonical policy must not depend on
/// the broker's transport types.
pub(crate) trait NativeTaskBindingActor {
    fn actor(&self) -> Result<crate::collab_broker_capnp::BrokerActor, String>;
}

impl NativeTaskBindingActor for NativeTaskBinding {
    fn actor(&self) -> Result<crate::collab_broker_capnp::BrokerActor, String> {
        let starter = self.starter.clone().ok_or("native task starter absent")?;
        Ok(
            crate::collab_broker_capnp::BrokerActor::agent(format!("agent:{}", self.agent_id))
                .for_person(starter)
                .session_delegated(),
        )
    }
}

/// Owner-private text for exactly one authenticated task/run read. Never merge
/// this into a public workspace snapshot.
#[derive(Clone, Default)]
pub struct NativeTaskPrivateInstructions {
    pub instruction: Option<String>,
    pub agent_request: Option<tilo_desktop_model::WorkspaceAgentRequest>,
    pub directions: std::collections::BTreeMap<u64, String>,
}

impl std::fmt::Debug for NativeTaskPrivateInstructions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeTaskPrivateInstructions { private: redacted }")
    }
}

/// Private owner inbox read. Dropping it ends observation, never delivery.
pub type NativeTaskInboxFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<Vec<tilo_actions::task_messaging::TaskMessage>, String>,
            > + Send
            + 'a,
    >,
>;

pub trait WorkspaceTaskExecution: Send + Sync {
    fn requires_room(&self, room: &str) -> bool;
    /// Read the canonical task controls in this native workspace.
    ///
    /// # Errors
    /// The owner is unavailable or this service does not govern the room.
    fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// Recheck the owner's admission barrier, not a runner or projection flag.
    /// Historical delivery remains valid after this gate closes; ACK authority does not.
    ///
    /// # Errors
    /// The owner is unavailable, or the run's MODEL admission barrier closed.
    fn ensure_model_run_open(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<(), String> {
        Err("native MODEL admission barrier unavailable".into())
    }
    /// Read instructions only while the assigned agent remains a current member.
    ///
    /// # Errors
    /// The owner is unavailable or current agent admission ended.
    fn agent_snapshot(&self, room: &str, agent: &str) -> Result<WorkspaceTaskSnapshot, String>;
    /// Read the exact owner-private draft and answer for an authenticated run.
    /// # Errors
    /// Refuses stale authority or unavailable private custody.
    fn private_confirmation(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<Option<tilo_desktop_model::WorkspaceTaskConfirmation>, String> {
        Err("native task private confirmation owner unavailable".into())
    }
    /// Freeze the exact child ceiling before publishing its request for approval.
    /// # Errors
    /// Refuses unavailable custody, stale authority, or a changed request.
    fn freeze_confirmation(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _asking: uuid::Uuid,
        _requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        Err("native task confirmation freeze owner unavailable".into())
    }
    /// Read only the exact active run's private text. The broker supplies its
    /// authenticated run binding; the owner rechecks the current host and actor.
    ///
    /// # Errors
    /// The owner is unavailable, the run changed, or current admission ended.
    fn private_instructions(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<Option<NativeTaskPrivateInstructions>, String> {
        Err("native task private instruction owner unavailable".into())
    }
    /// Recover the original child identity only while this exact review is held.
    ///
    /// # Errors
    /// Review, run or request-time owner custody changed.
    fn confirmation_invocation(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _asking: uuid::Uuid,
    ) -> Result<uuid::Uuid, String> {
        Err("native confirmation invocation custody unavailable".into())
    }

    /// Digest the exact authenticated owner-private run envelope, not its lease.
    ///
    /// # Errors
    /// Refuses unavailable private custody or a stale run/frontier.
    fn private_run_digest(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<[u8; 32], String> {
        Err("native task private run digest owner unavailable".into())
    }
    /// Read untrusted mail only for this authenticated native run.
    ///
    /// # Errors
    /// Returns an error if the owner is unavailable or current run authorization fails.
    fn messages(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<Vec<tilo_actions::task_messaging::TaskMessage>, String> {
        Ok(Vec::new())
    }
    /// Subscribe before the first inbox read to avoid losing a committed send.
    /// Wake hints never carry private data or confer read authority.
    ///
    /// # Errors
    /// The owner is unavailable or this service does not govern the room.
    fn message_changes(
        &self,
        _room: &str,
    ) -> Result<Option<tokio::sync::watch::Receiver<()>>, String> {
        Ok(None)
    }

    /// Subscribe to private mailbox changes scoped to the actual native run.
    /// Remote governors must authenticate physical host and run custody.
    ///
    /// # Errors
    /// The owner is unavailable, the run changed, or this host does not hold it.
    fn message_changes_for_run(
        &self,
        room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<Option<tokio::sync::watch::Receiver<()>>, String> {
        self.message_changes(room)
    }

    /// One asynchronous, currently authorized owner-private inbox observation.
    /// Wait actions must not treat the legacy empty inbox default as evidence.
    /// Implementations must keep network/blocking work off the broker task.
    fn poll_messages(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _requirement: &NativeTaskActionRequirement,
    ) -> NativeTaskInboxFuture<'_> {
        Box::pin(async { Err("native task private inbox observation unavailable".into()) })
    }
    /// Read exact original reports only for this authenticated native run.
    ///
    /// # Errors
    /// Refuses unavailable custody or stale run authority.
    fn report_transcript(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
    ) -> Result<Vec<tilo_actions::task_messaging::TaskReport>, String> {
        Err("private task transcript owner unavailable".into())
    }
    /// Links from the broker's actual durable native invocation result.
    ///
    /// # Errors
    /// Returns an error if output recording is unavailable or current run/action checks fail.
    fn record_outputs(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _outputs: Vec<tilo_actions::task_messaging::TaskOutputRef>,
        _requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        Err("native task output owner unavailable".into())
    }
    /// Internal native prompt boundary, not a public messaging action.
    ///
    /// # Errors
    /// Returns an error if acknowledgement is unavailable or current run authorization fails.
    fn acknowledge_messages(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _ids: &[String],
    ) -> Result<(), String> {
        Err("native inbox acknowledgement unavailable".into())
    }
    /// Called only by the registered inbox action's native invocation handler.
    ///
    /// # Errors
    /// Returns an error if messaging is unavailable or message admission or delivery fails.
    fn send_message(
        &self,
        _room: &str,
        _binding: &NativeTaskBinding,
        _recipient: &str,
        _invocation: uuid::Uuid,
        _send: tilo_actions::task_messaging::TaskMessageSend,
        _requirement: &NativeTaskActionRequirement,
    ) -> Result<tilo_actions::task_messaging::TaskMessage, String> {
        Err("private task messaging unavailable".into())
    }
    /// The broker supplies a binding derived from its authenticated channel.
    /// Authority uses the host's existing enrolled attestation, never a wire grant.
    fn report(
        &self,
        room: &str,
        binding: NativeTaskBinding,
        command: WorkspaceTaskCommand,
    ) -> tilo_owner_commit::CommitFuture;
    /// Check the current owner capability policy before observing retained action output.
    ///
    /// # Errors
    /// The room or actor is unavailable, or its action capability was revoked.
    fn authorize_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String>;
    /// Observe a child whose exact invocation has already crossed durable host
    /// admission. This read-only door may retain Ask-compatible observation; it
    /// must never reserve, consume approval or return new dispatch authority.
    ///
    /// # Errors
    /// No durable dispatch custody exists, or current authority denies the action.
    fn authorize_retained_action(
        &self,
        room: &str,
        agent: &str,
        requirement: &NativeTaskActionRequirement,
        _invocation: uuid::Uuid,
    ) -> Result<(), String> {
        self.authorize_action(room, agent, requirement)
    }
    /// Admit a host activation in the same serialized task turn as an action.
    /// The callback must retain custody before invoking the ordinary launcher.
    ///
    /// # Errors
    /// Membership, starter authority, frozen scope or instruction frontier changed.
    fn admit_host_launch(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        launch: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;

    /// Prepare one asynchronous transfer of the owner's guarded enqueue turn.
    /// Only owned metadata crosses to the worker; the original non-Send enqueue
    /// closure stays on the broker `LocalSet`. The worker must recheck the original
    /// caller and task policy on the exact guarded owner view, including live
    /// expiry immediately before Ready, and hold its guard until completion or drop.
    /// Implementations must not re-enter the owner mutex from that callback.
    /// The default fails closed rather than invoking synchronous admission.
    fn prepare_admission(
        &self,
        _room: String,
        _binding: NativeTaskBinding,
        _requirement: NativeTaskActionRequirement,
        _caller: NativeTaskAdmissionCaller,
    ) -> NativeTaskAdmissionFuture {
        Box::pin(async { Err("native task asynchronous admission owner unavailable".into()) })
    }

    /// Runs ordinary action admission inside the existing serialized task turn.
    ///
    /// # Errors
    /// Current membership, approval or instruction frontier changed, or admission fails.
    fn admit(
        &self,
        room: &str,
        binding: &NativeTaskBinding,
        requirement: &NativeTaskActionRequirement,
        action: &mut dyn FnMut() -> Result<InvocationJob, String>,
    ) -> Result<InvocationJob, String>;
}
