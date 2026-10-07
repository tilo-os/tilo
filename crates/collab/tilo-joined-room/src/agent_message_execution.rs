//! Closed native host service. The only execution input is an actual launcher
//! proof; caller fields, signed data and retained session rows cannot recreate
//! it. Live checks run inside the owner turn without broker/membership locks.
use crate::agent_message::{
    MessageAttention, MessageExecutionBinding, MessagePhase, StoredAgentMessage,
};
use crate::{
    AgentSessionBinding, NativeCommittedView, NativeWorkspaceOwner, RoomId, agent_session,
};
use launcher::managed::{ChannelProcessFacts, ManagedRuntimeFacts, VerifiedManagedProcess};
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;

mod observation;
mod reply;
pub use observation::AgentMessageExecutionObservation;
pub use reply::AgentMessageReplyReceipt;

/// Descriptive addressed input. This is neither document access nor process
/// authority; only the executing Agent's private admitted channel receives it.
pub struct AgentMessageDelivery {
    pub invocation: uuid::Uuid,
    pub room: RoomId,
    pub agent: String,
    pub channel: String,
    pub request_row: String,
    pub text: String,
    pub source_digest: [u8; 32],
    pub launch: String,
    /// Previously retained output, if present. A reconnect must commit this
    /// exact output rather than repeat model generation with a new answer.
    pub prepared_reply: Option<(uuid::Uuid, String)>,
}

/// Private host installation, never a serialized capability or guest RPC.
/// Its proof cannot be built from a process id, DTO or arbitrary verifier.
pub struct AgentMessageHostService {
    owner: NativeWorkspaceOwner,
    attestor: DeviceCert,
    session: AgentSessionBinding,
    process: VerifiedManagedProcess,
}

#[derive(Clone, Copy)]
enum Transition<'a> {
    Deliver,
    Accept,
    PrepareReply(&'a str),
}

impl AgentMessageHostService {
    /// Install from the actual admitted private connection. No guest-selected
    /// session fields, process id or stored digest can construct this service.
    ///
    /// # Errors
    /// The actual proof is stale, its original session is absent, or current
    /// host, membership and durable bound-process custody no longer match.
    pub fn from_connected_process(
        owner: NativeWorkspaceOwner,
        attestor: DeviceCert,
        process: VerifiedManagedProcess,
        now: SystemTime,
    ) -> Result<Self, String> {
        let facts = process.facts()?;
        let room = RoomId::parse(&facts.binding.room).ok_or("connected Agent room is invalid")?;
        let session = owner.with_current(room, |view| {
            agent_session::connected_binding(view, &facts.binding.launch)
        })?;
        Self::from_admitted_process(owner, attestor, session, process, now)
    }

    /// Discover only this connected Agent's pending input identities. The
    /// returned identifiers are not authority: each delivery rechecks its
    /// original source and current execution inside the serialized owner turn.
    /// Admission order is stable, including across private-stream replacement.
    ///
    /// # Errors
    /// The current actual process, host or retained session cannot be verified.
    pub fn pending_invocations(&self, now: SystemTime) -> Result<Vec<uuid::Uuid>, String> {
        let _turn = self.owner.0.mutation.lock();
        self.owner.with_current(self.session.room, |view| {
            self.execution(view, now)?;
            let mut pending = view
                .record
                .agent_messages
                .values()
                .filter(|message| {
                    message.source.agent == self.session.agent
                        && !matches!(message.phase, MessagePhase::NeedsAttention(_))
                })
                .map(|message| (message.admitted_generation, message.source.invocation))
                .collect::<Vec<_>>();
            pending.sort_unstable();
            Ok(pending
                .into_iter()
                .map(|(_, invocation)| invocation)
                .collect())
        })
    }

    /// Adopt the actual connected Agent proof installed by the native broker.
    ///
    /// # Errors
    /// The kernel/channel/runtime proof differs from exact durable `ProcessBound`
    /// custody or current host/launch/membership authority is absent.
    pub fn from_admitted_process(
        owner: NativeWorkspaceOwner,
        attestor: DeviceCert,
        session: AgentSessionBinding,
        process: VerifiedManagedProcess,
        now: SystemTime,
    ) -> Result<Self, String> {
        let service = Self {
            owner,
            attestor,
            session,
            process,
        };
        {
            let _turn = service.owner.0.mutation.lock();
            service.owner.with_current(service.session.room, |view| {
                service.execution(view, now).map(|_| ())
            })?;
        }
        Ok(service)
    }

    /// First bind to this actual launch and reveal only the explicit input.
    /// Repeated delivery uses the same immutable request, never a new UUID.
    ///
    /// # Errors
    /// Any source or live custody check changed, or durable publication failed.
    pub fn deliver(
        &self,
        invocation: uuid::Uuid,
        now: SystemTime,
    ) -> Result<AgentMessageDelivery, String> {
        let message = self.transition(invocation, Transition::Deliver, now)?;
        Ok(AgentMessageDelivery {
            invocation,
            room: message.source.room,
            agent: message.source.agent.clone(),
            channel: message.source.channel.clone(),
            request_row: message.source.request_row.clone(),
            text: message.source.payload.text.clone(),
            source_digest: message.source.digest()?,
            launch: self.session.launch.clone(),
            prepared_reply: match &message.phase {
                MessagePhase::ReplyPrepared { invocation, text } => {
                    Some((*invocation, text.clone()))
                }
                _ => None,
            },
        })
    }

    /// Acknowledge this exact delivery, not reading the whole Chat document.
    ///
    /// # Errors
    /// The first delivery was not bound, custody changed, or publication failed.
    pub fn accept(&self, invocation: uuid::Uuid, now: SystemTime) -> Result<(), String> {
        self.transition(invocation, Transition::Accept, now)
            .map(|_| ())
    }

    /// Retain output and its fixed reply UUID before any canonical invocation.
    /// This does not publish an Agent reply or claim a model actually ran.
    ///
    /// # Errors
    /// No accepted delivery, changed custody/output, invalid text or failed store.
    pub fn prepare_reply(
        &self,
        invocation: uuid::Uuid,
        text: &str,
        now: SystemTime,
    ) -> Result<uuid::Uuid, String> {
        self.transition(invocation, Transition::PrepareReply(text), now)
            .map(|message| message.reply_invocation())
    }

    fn execution(
        &self,
        view: &NativeCommittedView,
        now: SystemTime,
    ) -> Result<MessageExecutionBinding, String> {
        // `facts` refreshes runtime ownership, registry, pidfd, supervisor
        // ancestry and executing image. It never calls broker membership.
        let facts = self.process.facts()?;
        if facts.binding.room != self.session.room.to_string()
            || facts.binding.agent != self.session.agent
            || facts.binding.launch != self.session.launch
            || facts.binding.generation != self.session.generation
            || facts.binding.app != launcher::managed::WORKSPACE_AGENT_PACKAGE_ID
            || facts.runtime.runtime_epoch != self.session.runtime_epoch
            || runtime_digest(&facts.runtime)? != self.session.runtime_digest
        {
            return Err("Agent Message live proof differs from original session".into());
        }
        let digest = process_digest(&facts)?;
        agent_session::revalidate_bound_process(view, &self.session, &self.attestor, digest, now)?;
        Ok(MessageExecutionBinding {
            room: self.session.room,
            agent: self.session.agent.clone(),
            launch: uuid::Uuid::parse_str(&self.session.launch)
                .map_err(|_| "invalid retained Agent launch")?,
            runtime_epoch: uuid::Uuid::parse_str(&self.session.runtime_epoch)
                .map_err(|_| "invalid retained Agent runtime epoch")?,
            runtime_digest: self.session.runtime_digest,
            session_generation: self.session.generation,
            process_digest: digest,
        })
    }

    fn transition(
        &self,
        invocation: uuid::Uuid,
        transition: Transition<'_>,
        now: SystemTime,
    ) -> Result<StoredAgentMessage, String> {
        let _turn = self.owner.0.mutation.lock();
        let candidate = self
            .owner
            .with_current(self.session.room, |view| Ok(view.clone()))?;
        let mut message = candidate
            .record
            .agent_messages
            .get(&invocation)
            .cloned()
            .ok_or("Agent Message input is absent or terminal")?;
        if message.source.agent != self.session.agent {
            return Err("Agent Message belongs to another Agent principal".into());
        }
        let attention = source_attention(&candidate, &message, now);
        let execution = self.execution(&candidate, now);
        if let Some(reason) = attention.or_else(|| {
            execution
                .as_ref()
                .err()
                .map(|_| MessageAttention::ExecutionRetired)
        }) {
            if message.needs_attention(reason) {
                self.publish_message(candidate, message)?;
            }
            return Err(
                "Agent Message needs attention; source or execution custody changed".into(),
            );
        }
        let execution = execution?;
        let changed = match transition {
            Transition::Deliver => {
                message.bind_first(&execution, message.membership_incarnation)?
            }
            Transition::Accept => message.accept(&execution)?,
            Transition::PrepareReply(text) => message.prepare_reply(&execution, text)?,
        };
        if changed {
            self.publish_message(candidate, message.clone())?;
        }
        Ok(message)
    }

    fn publish_message(
        &self,
        mut candidate: NativeCommittedView,
        message: StoredAgentMessage,
    ) -> Result<(), String> {
        candidate.record.generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("Agent Message owner generation exhausted")?;
        candidate
            .record
            .agent_messages
            .insert(message.source.invocation, message);
        self.owner.publish_room(self.session.room, candidate, None)
    }
}

pub(crate) fn source_attention(
    view: &NativeCommittedView,
    message: &StoredAgentMessage,
    now: SystemTime,
) -> Option<MessageAttention> {
    if view.host().unwrap_or_else(|| view.governor()) != &message.source.host
        || view.device() != &message.source.host
    {
        return Some(MessageAttention::HostChanged);
    }
    if !view.peer().certificates().contains(&message.source.caller)
        || crate::agent_message_admission::revalidate_operation(
            view,
            &message.source.caller,
            &message.source.operation,
            now,
        )
        .is_err()
    {
        return Some(MessageAttention::AuthorRevoked);
    }
    if !crate::agent_message::membership_incarnation(view.peer(), &message.source.agent)
        .is_ok_and(|incarnation| incarnation == message.membership_incarnation)
    {
        return Some(MessageAttention::MembershipChanged);
    }
    // Runs under the same serialized owner turn as first binding, delivery,
    // acceptance and reply preparation. A saved source digest is not proof
    // that the original parent still exists unchanged in current Chat.
    if crate::agent_message_commit::verify_row(view, &message.source, now).is_err() {
        return Some(MessageAttention::RequestChanged);
    }
    None
}

fn runtime_digest(facts: &ManagedRuntimeFacts) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.native-agent-runtime.v1");
    for field in [
        &facts.runtime_epoch,
        &facts.package_id,
        &facts.package_hash,
        &facts.executable_hash,
        &facts.runtime_hash,
    ] {
        hash_field(&mut hash, field)?;
    }
    Ok(*hash.finalize().as_bytes())
}

fn process_digest(facts: &ChannelProcessFacts) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.native-agent-process.v1");
    hash.update(&runtime_digest(&facts.runtime)?);
    for field in [&facts.binding.launch, &facts.channel_id] {
        hash_field(&mut hash, field)?;
    }
    for identity in [facts.process, facts.supervisor] {
        hash.update(&identity.pid.to_le_bytes());
        hash.update(&identity.start.to_le_bytes());
    }
    Ok(*hash.finalize().as_bytes())
}

fn hash_field(hash: &mut blake3::Hasher, field: &str) -> Result<(), String> {
    let length =
        u64::try_from(field.len()).map_err(|_| "Agent Message custody field length overflow")?;
    hash.update(&length.to_le_bytes());
    hash.update(field.as_bytes());
    Ok(())
}

#[cfg(test)]
#[cfg(feature = "managed-message-test")]
#[path = "agent_message_execution_tests.rs"]
mod tests;
