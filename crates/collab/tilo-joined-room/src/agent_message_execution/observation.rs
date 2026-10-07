//! Read-only recovery under the same owner turn as acceptance and publication.
use super::{AgentMessageHostService, source_attention};
use crate::agent_message::{MessageOutcome, MessagePhase};
use std::time::SystemTime;

/// Descriptive state, not permission to retry an uncertain effect.
#[derive(Clone, PartialEq, Eq)]
pub enum AgentMessageExecutionObservation {
    Bound,
    Accepted,
    Prepared { reply: uuid::Uuid, text: String },
    Committed { reply: uuid::Uuid },
    NeedsAttention,
}

impl AgentMessageHostService {
    /// Observe exactly this actual process's original Message without binding,
    /// accepting, signing, or publishing anything. Historical committed effects
    /// remain true after a parent edit; they grant no further effects.
    ///
    /// # Errors
    /// Missing history, foreign source/Agent/launch, stale process or unavailable
    /// authoritative owner state. Failure must never be interpreted as absence.
    pub fn observe(
        &self,
        invocation: uuid::Uuid,
        digest: [u8; 32],
        now: SystemTime,
    ) -> Result<AgentMessageExecutionObservation, String> {
        let _turn = self.owner.0.mutation.lock();
        self.owner.with_current(self.session.room, |view| {
            let execution = self.execution(view, now)?;
            if let Some(terminal) = crate::agent_message_history::lookup(
                self.owner.0.store.as_ref(),
                view.record.agent_message_history_root,
                invocation,
            )? {
                if terminal.source_digest != digest {
                    return Err("Message observation differs from original source".into());
                }
                let MessageOutcome::Replied {
                    invocation: reply,
                    execution: original,
                    ..
                } = terminal.outcome
                else {
                    return Err("Message observation has no executing reply receipt".into());
                };
                if original.as_ref() != &execution {
                    return Err("Message observation belongs to another executing launch".into());
                }
                return Ok(AgentMessageExecutionObservation::Committed { reply });
            }
            let message = view
                .record
                .agent_messages
                .get(&invocation)
                .ok_or("Message observation has no authoritative retained original")?;
            if message.source.agent != self.session.agent
                || message.source.digest()? != digest
                || message.execution.as_ref() != Some(&execution)
            {
                return Err("Message observation belongs to another source or execution".into());
            }
            if source_attention(view, message, now).is_some() {
                return Ok(AgentMessageExecutionObservation::NeedsAttention);
            }
            Ok(match &message.phase {
                MessagePhase::Bound => AgentMessageExecutionObservation::Bound,
                MessagePhase::Accepted => AgentMessageExecutionObservation::Accepted,
                MessagePhase::ReplyPrepared { invocation, text } => {
                    AgentMessageExecutionObservation::Prepared {
                        reply: *invocation,
                        text: text.clone(),
                    }
                }
                MessagePhase::NeedsAttention(_) => AgentMessageExecutionObservation::NeedsAttention,
                MessagePhase::AwaitingAgent => {
                    return Err("Message observation cannot bind input".into());
                }
            })
        })
    }
}
