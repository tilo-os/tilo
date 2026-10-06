//! Owner-retained correlation for explicitly addressed Agent input.
//!
//! These records are inert. Neither a decoded binding nor possession of a
//! receipt grants Chat access or proves that a process is currently admitted.
//! The native broker must retain its actual managed process/channel evidence;
//! the owner revalidates that evidence at each live transition.

use crate::RoomId;
use serde::{Deserialize, Serialize};
use tilo_peer_state::owner_commit::CommitBinding;
use tilo_user_identity::DeviceCert;

pub(crate) const MAX_PENDING: usize = 128;
pub(crate) const MAX_TEXT_BYTES: usize = 32_000;
pub(crate) const MAX_TEXT_CHARS: usize = 8_000;

/// Inert identity of the exact winning admission operation in authenticated
/// owner state. Identical values written after removal have different op ids;
/// unrelated room changes do not change this identity. This is not a grant.
pub(crate) fn membership_incarnation(
    peer: &tilo_peer_state::StatePeer,
    agent: &str,
) -> Result<[u8; 32], String> {
    use automerge::{ROOT, ReadDoc};
    use collab_types::caps::{CapOverride, Scope, Subject};
    let room = peer
        .admitted_room()
        .ok_or("Agent membership document is absent")?;
    let maps = room
        .get_all(ROOT, "capabilities")
        .map_err(|_| "Agent membership cannot be read")?;
    let [(_, map)] = maps.as_slice() else {
        return Err("Agent membership map is absent or conflicted".into());
    };
    let mut admission = None;
    for id in room.keys(map) {
        let entries = room
            .get_all(map, &id)
            .map_err(|_| "Agent membership cannot be read")?;
        for (value, operation) in &entries {
            let Some(bytes) = value.as_bytes() else {
                continue;
            };
            let capability: CapOverride = serde_json::from_slice(bytes)
                .map_err(|_| "Agent membership capability is invalid")?;
            if capability.subject != Subject::agent(agent)
                || !matches!(&capability.scope, Scope::Room(scope) if scope == peer.room())
            {
                continue;
            }
            if entries.len() != 1 || admission.is_some() {
                return Err("Agent membership admission is ambiguous".into());
            }
            let mut hash =
                blake3::Hasher::new_derive_key("tilo native Agent membership incarnation v1");
            hash.update(peer.room().as_bytes());
            hash.update(&[0]);
            hash.update(agent.as_bytes());
            hash.update(&[0]);
            hash.update(id.as_bytes());
            hash.update(&[0]);
            hash.update(operation.to_string().as_bytes());
            admission = Some(*hash.finalize().as_bytes());
        }
    }
    admission.ok_or_else(|| "Agent membership admission is absent".into())
}

/// Immutable original admitted request, never a caller-selected delivery route.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageSource {
    pub(crate) room: RoomId,
    pub(crate) invocation: uuid::Uuid,
    pub(crate) request_row: String,
    pub(crate) channel: String,
    pub(crate) caller: DeviceCert,
    /// Original pinned-host certificate, not authority for a replacement host
    /// to revive a pending delivery after custody changed.
    pub(crate) host: DeviceCert,
    pub(crate) agent: String,
    pub(crate) payload: tilo_actions::agent_message::SerializedAgentMessagePayload,
    /// Exact original typed bytes, including inert extras and JSON ordering.
    /// Kept only while input/output remains pending; terminal history keeps a
    /// digest rather than private submitted content.
    pub(crate) action: Vec<u8>,
    /// Digest of the exact broker-retained typed `PostAgentMessage` body, not a
    /// claim that an app's publication/label authenticates that operation.
    pub(crate) action_digest: [u8; 32],
    /// Opaque correlation digest of the original publisher registration and
    /// invocation stream. No live route/process tokens are retained here.
    pub(crate) publisher_digest: [u8; 32],
    pub(crate) operation: crate::agent_message_admission::AgentMessageOperationData,
    pub(crate) executed_action: tilo_actions::ActionId,
    /// Exact executor writer from its immutable envelope. Attributed person
    /// and Agent writers are deliberately independent of the `ActionId`.
    pub(crate) executed_actor: Vec<u8>,
    pub(crate) changes: Vec<[u8; 32]>,
    pub(crate) document: CommitBinding,
}

impl MessageSource {
    pub(crate) fn validate(&self) -> Result<(), String> {
        use tilo_peer_state::owner_commit::CommitTarget;
        if self.room.is_nil()
            || self.invocation.is_nil()
            || self.request_row != format!("m_{}", self.invocation.simple())
            || self.channel.is_empty()
            || self.channel.len() > 128
            || self.agent.is_empty()
            || self.agent.len() > tilo_actions::MAX_PARTICIPANT_ID_BYTES
            || self.payload.text.trim().is_empty()
            || self.payload.text.len() > MAX_TEXT_BYTES
            || self.payload.text.chars().count() > MAX_TEXT_CHARS
            || self.payload.intent != tilo_actions::agent_message::AgentIntent::Message
            || self.document.room != self.room.to_string()
            || !matches!(&self.document.target, CommitTarget::Document { app, record }
                if app == "so.tilo.chat" && record.is_empty())
            || self.document.request_digest == [0; 32]
            || self.document.basis == [0; 32]
            || self.action_digest == [0; 32]
            || self.action.is_empty()
            || self.executed_actor.is_empty()
            || self
                .action
                .len()
                .checked_add(self.executed_actor.len())
                .is_none_or(|bytes| bytes > collab_types::MAX_DOCUMENT_TRANSFER_BYTES)
            || self.action_digest != *blake3::hash(&self.action).as_bytes()
            || self.publisher_digest == [0; 32]
            || self.executed_action.0.is_nil()
            || self.changes.is_empty()
            || self.changes.len() > 4096
            || self.changes.contains(&[0; 32])
            || self
                .changes
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return Err("invalid addressed Agent input correlation".into());
        }
        Ok(())
    }

    pub(crate) fn digest(&self) -> Result<[u8; 32], String> {
        self.validate()?;
        let mut hash = blake3::Hasher::new_derive_key("tilo native addressed Agent input v1");
        serde_json::to_writer(HashWriter(&mut hash), self)
            .map_err(|_| "Agent input correlation encoding failed")?;
        Ok(*hash.finalize().as_bytes())
    }
}

/// Hash the actual encoded original correlation without allocating a second
/// JSON byte-array expansion of potentially large inert typed extras.
struct HashWriter<'a>(&'a mut blake3::Hasher);
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Descriptive projection of an owner-verified `ProcessBound` Agent session.
/// Construction stays private to the owner adapter; deserialization is not
/// authorization. No process or socket proof is substituted by these fields.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageExecutionBinding {
    pub(crate) room: RoomId,
    pub(crate) agent: String,
    pub(crate) launch: uuid::Uuid,
    pub(crate) runtime_epoch: uuid::Uuid,
    pub(crate) runtime_digest: [u8; 32],
    pub(crate) session_generation: u64,
    pub(crate) process_digest: [u8; 32],
}

impl MessageExecutionBinding {
    pub(crate) fn validate(&self, source: &MessageSource) -> Result<(), String> {
        if self.room != source.room
            || self.agent != source.agent
            || self.launch.is_nil()
            || self.runtime_epoch.is_nil()
            || self.runtime_digest == [0; 32]
            || self.session_generation == 0
            || self.process_digest == [0; 32]
        {
            return Err("Agent input execution binding differs from its original target".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum MessagePhase {
    AwaitingAgent,
    Bound,
    Accepted,
    ReplyPrepared {
        invocation: uuid::Uuid,
        text: String,
    },
    NeedsAttention(MessageAttention),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MessageAttention {
    MembershipChanged,
    AuthorRevoked,
    HostChanged,
    ExecutionRetired,
    /// The original canonical request version cannot be revalidated. This
    /// does not revoke input already delivered or claim model cancellation.
    RequestChanged,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredAgentMessage {
    pub(crate) source: MessageSource,
    pub(crate) execution: Option<MessageExecutionBinding>,
    /// Digest of the winning authenticated membership admission CRDT operation,
    /// not just its value/id (remove and identical re-add must differ).
    pub(crate) membership_incarnation: [u8; 32],
    pub(crate) phase: MessagePhase,
    pub(crate) admitted_generation: u64,
}

impl std::fmt::Debug for StoredAgentMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The containing durable record implements Debug. Do not let that
        // accidentally disclose explicitly submitted input or model output.
        formatter.write_str("StoredAgentMessage(..)")
    }
}

impl StoredAgentMessage {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.source.validate()?;
        if let Some(execution) = &self.execution {
            execution.validate(&self.source)?;
        }
        if self.admitted_generation == 0
            || self.membership_incarnation == [0; 32]
            || (matches!(self.phase, MessagePhase::AwaitingAgent) && self.execution.is_some())
            || (matches!(
                self.phase,
                MessagePhase::Bound | MessagePhase::Accepted | MessagePhase::ReplyPrepared { .. }
            ) && self.execution.is_none())
            || (matches!(&self.phase, MessagePhase::ReplyPrepared { invocation, text }
                if *invocation != self.reply_invocation() || text.trim().is_empty() || text.len() > 64 * 1024))
        {
            return Err("invalid retained Agent input phase".into());
        }
        Ok(())
    }

    pub(crate) fn check_replay(&self, source: &MessageSource) -> Result<(), String> {
        if self.source != *source {
            return Err("Agent Message invocation was already bound to another input".into());
        }
        Ok(())
    }

    pub(crate) fn reply_invocation(&self) -> uuid::Uuid {
        collab_types::document_artifact_id(
            "tilo/native-agent-message-reply/v1",
            &self.source.agent,
            &format!("{}:{}", self.source.room, self.source.invocation),
        )
        .0
    }

    /// First assignment only. The owner must freshly verify the actual winning
    /// membership admission, original author, host and `ProcessBound` evidence in
    /// the same serialized transaction. This method provides correlation only.
    pub(crate) fn bind_first(
        &mut self,
        execution: &MessageExecutionBinding,
        membership_incarnation: [u8; 32],
    ) -> Result<bool, String> {
        execution.validate(&self.source)?;
        if self.membership_incarnation != membership_incarnation {
            return Err("Agent Message membership admission was replaced".into());
        }
        if self.phase == MessagePhase::AwaitingAgent && self.execution.is_none() {
            self.execution = Some(execution.clone());
            self.phase = MessagePhase::Bound;
            return Ok(true);
        }
        if self.execution.as_ref() == Some(execution)
            && !matches!(self.phase, MessagePhase::NeedsAttention(_))
        {
            return Ok(false);
        }
        Err("Agent Message cannot implicitly change its assigned execution".into())
    }

    pub(crate) fn needs_attention(&mut self, reason: MessageAttention) -> bool {
        if matches!(self.phase, MessagePhase::NeedsAttention(_)) {
            return false;
        }
        self.phase = MessagePhase::NeedsAttention(reason);
        true
    }

    /// Correlation only, not live authorization. The serialized owner adapter
    /// must verify actual current process/channel custody before calling this.
    pub(crate) fn accept(&mut self, execution: &MessageExecutionBinding) -> Result<bool, String> {
        if self.execution.as_ref() != Some(execution) {
            return Err("Agent Message acceptance belongs to another launch".into());
        }
        if self.phase == MessagePhase::Bound {
            self.phase = MessagePhase::Accepted;
            return Ok(true);
        }
        if matches!(
            self.phase,
            MessagePhase::Accepted | MessagePhase::ReplyPrepared { .. }
        ) {
            return Ok(false);
        }
        Err("Agent Message is not available for acceptance".into())
    }

    /// Persist output before attempting its fixed canonical reply invocation.
    /// A retry may observe that original output, never replace or regenerate it.
    pub(crate) fn prepare_reply(
        &mut self,
        execution: &MessageExecutionBinding,
        text: &str,
    ) -> Result<bool, String> {
        if self.execution.as_ref() != Some(execution)
            || text.trim().is_empty()
            || text.len() > 64 * 1024
        {
            return Err("invalid Agent Message reply correlation or body".into());
        }
        let next = MessagePhase::ReplyPrepared {
            invocation: self.reply_invocation(),
            text: text.to_owned(),
        };
        match &self.phase {
            MessagePhase::AwaitingAgent | MessagePhase::Bound | MessagePhase::NeedsAttention(_) => {
                Err("Agent Message was not accepted for execution".into())
            }
            MessagePhase::Accepted => {
                self.phase = next;
                Ok(true)
            }
            old if *old == next => Ok(false),
            MessagePhase::ReplyPrepared { .. } => {
                Err("Agent Message reply was already bound to another output".into())
            }
        }
    }
}

pub(crate) fn validate(view: &crate::NativeCommittedView) -> Result<(), String> {
    if view.record.agent_messages.len() > MAX_PENDING {
        return Err("pending Agent Message capacity exceeded".into());
    }
    for (id, message) in &view.record.agent_messages {
        message.validate()?;
        if *id != message.source.invocation
            || message.source.room != view.record.room
            || message.admitted_generation > view.generation()
        {
            return Err("retained Agent Message differs from its owner record".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "agent_message_tests.rs"]
mod tests;

/// Cold terminal correlation contains no submitted text or generated reply.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalMessage {
    pub(crate) invocation: uuid::Uuid,
    pub(crate) source_digest: [u8; 32],
    pub(crate) outcome: MessageOutcome,
    pub(crate) generation: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum MessageOutcome {
    Replied {
        invocation: uuid::Uuid,
        document: CommitBinding,
    },
    /// The canonical Chat request remains accepted. This is an explicit
    /// delivery outcome, not permission to drop/retry the same UUID silently.
    DeliveryUnavailable,
    Closed,
}

impl TerminalMessage {
    pub(crate) fn check_replay(&self, source: &MessageSource) -> Result<(), String> {
        if self.invocation != source.invocation || self.source_digest != source.digest()? {
            return Err("terminal Agent Message invocation has a different original input".into());
        }
        Ok(())
    }
}
