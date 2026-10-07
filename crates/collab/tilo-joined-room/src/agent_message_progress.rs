//! Sanitized public progress, independent of the private execution observer.
use crate::{
    NativeWorkspaceOwner, RoomId,
    agent_message::{MessageAttention, MessageOutcome, MessagePhase},
    agent_message_history,
};
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;

pub use tilo_actions::agent_message::{
    AgentMessagePublicPhase, AgentMessagePublicProgress, AgentMessagePublicReason,
};

fn public_reason(reason: &MessageAttention) -> AgentMessagePublicReason {
    match reason {
        MessageAttention::MembershipChanged => AgentMessagePublicReason::MembershipChanged,
        MessageAttention::AuthorRevoked => AgentMessagePublicReason::AuthorRevoked,
        MessageAttention::HostChanged => AgentMessagePublicReason::HostChanged,
        MessageAttention::ExecutionRetired => AgentMessagePublicReason::ExecutionRetired,
        MessageAttention::RequestChanged => AgentMessagePublicReason::RequestChanged,
    }
}

/// A negative owner-state fence only. Retained bytes never prove that a
/// process/model is alive and this read cannot acquire execution authority.
fn execution_retired(
    view: &crate::NativeCommittedView,
    message: &crate::agent_message::StoredAgentMessage,
    now: SystemTime,
) -> bool {
    let Some(execution) = &message.execution else {
        return false;
    };
    let Ok(binding) = crate::agent_session::connected_binding(view, &execution.launch.to_string())
    else {
        return true;
    };
    binding.room != execution.room
        || binding.agent != execution.agent
        || binding.launch != execution.launch.to_string()
        || binding.runtime_epoch != execution.runtime_epoch.to_string()
        || binding.runtime_digest != execution.runtime_digest
        || binding.generation != execution.session_generation
        || crate::agent_session::revalidate_bound_process(
            view,
            &binding,
            &message.source.host,
            execution.process_digest,
            now,
        )
        .is_err()
}

#[cfg(test)]
#[path = "agent_message_progress_tests.rs"]
mod tests;

impl NativeWorkspaceOwner {
    /// Observe one canonical original under current Chat document Read.
    /// Supplied identity is inert: its exact current certificate and permission
    /// are checked inside the serialized owner turn, not at an earlier mint.
    /// No input, generated text or process proof crosses this public boundary.
    ///
    /// # Errors
    /// Refuses invalid correlation, revoked Read, fenced/unavailable storage,
    /// mismatched source, or malformed authoritative history.
    pub fn agent_message_public_progress(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        invocation: uuid::Uuid,
        request_row: &str,
        now: SystemTime,
    ) -> Result<Option<AgentMessagePublicProgress>, String> {
        if room.is_nil()
            || invocation.is_nil()
            || request_row != format!("m_{}", invocation.simple())
        {
            return Err("invalid public Agent Message correlation".into());
        }
        let _turn = self.0.mutation.lock();
        self.with_current(room, |view| {
            if view.host().unwrap_or_else(|| view.governor()) != view.device() {
                return Err("public Message progress requires the governing native owner".into());
            }
            if !view.peer().certificates().contains(caller) {
                return Err("public Agent Message Read lacks current caller evidence".into());
            }
            view.peer()
                .check_document_read(caller.device_public_key, "so.tilo.chat", "", now)?;
            let phase = if let Some(message) = view.record.agent_messages.get(&invocation) {
                if message.source.room != room || message.source.request_row != request_row {
                    return Err("public Agent Message source differs from its canonical row".into());
                }
                if let Some(reason) =
                    crate::agent_message_execution::source_attention(view, message, now)
                {
                    AgentMessagePublicPhase::NeedsAttention(public_reason(&reason))
                } else if execution_retired(view, message, now) {
                    AgentMessagePublicPhase::NeedsAttention(
                        AgentMessagePublicReason::ExecutionRetired,
                    )
                } else {
                    match &message.phase {
                        MessagePhase::AwaitingAgent
                        | MessagePhase::Bound
                        | MessagePhase::Accepted => AgentMessagePublicPhase::WaitingForReply,
                        MessagePhase::ReplyPrepared { .. } => AgentMessagePublicPhase::Publishing,
                        MessagePhase::NeedsAttention(reason) => {
                            AgentMessagePublicPhase::NeedsAttention(public_reason(reason))
                        }
                    }
                }
            } else if let Some(terminal) = agent_message_history::lookup(
                self.0.store.as_ref(),
                view.record.agent_message_history_root,
                invocation,
            )? {
                match terminal.outcome {
                    MessageOutcome::Replied {
                        invocation: reply, ..
                    } => AgentMessagePublicPhase::Committed {
                        reply_row: format!("m_{}", reply.simple()),
                    },
                    MessageOutcome::DeliveryUnavailable => AgentMessagePublicPhase::NeedsAttention(
                        AgentMessagePublicReason::DeliveryUnavailable,
                    ),
                    MessageOutcome::Closed => {
                        AgentMessagePublicPhase::NeedsAttention(AgentMessagePublicReason::Closed)
                    }
                }
            } else {
                return Ok(None);
            };
            Ok(Some(AgentMessagePublicProgress {
                generation: view.generation(),
                phase,
            }))
        })
    }
}
