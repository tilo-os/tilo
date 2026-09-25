//! Live attachment to the existing serial Cloud runtime owner. No constructor
//! from persisted facts, no new queue, and no detached Store/worker owner.
use super::*;
use qos_action_broker::{
    agent_control::AgentProviderAuthority,
    agent_reads::{
        AgentReadCustody, AgentReadError, AgentReadFuture, AgentReadReply, CertifiedAgentReads,
    },
};
use qos_relay::agent_reads::AgentReadBinding;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant as Deadline;

#[derive(Clone, Copy)]
pub(super) enum CommandKind {
    Action,
    Binding,
    Read,
    /// Fresh retained status of the evidence's own invocation (read-only).
    Observe(InvocationId),
}
pub(super) enum CommandReply {
    Action(Option<ActionMessage>),
    Binding(AgentReadBinding),
}
pub(super) enum CommandError {
    Read(AgentReadError),
    Owner(String),
}
impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => e.fmt(f),
            Self::Owner(e) => f.write_str(e),
        }
    }
}
impl From<AgentReadError> for CommandError {
    fn from(e: AgentReadError) -> Self {
        Self::Read(e)
    }
}
pub(super) struct CloseOnDrop(
    pub Arc<AtomicBool>,
    pub Arc<crate::agent_owner::RuntimePolicyClock>,
);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        self.1.close();
    }
}

/// Cloneable process-local sender, minted only by an actually started Cloud
/// runtime. Its clone does not create another Store, worker, or execution owner.
#[derive(Clone)]
pub struct CloudAgentCommands {
    commands: mpsc::Sender<Command>,
    closed: Arc<AtomicBool>,
    reads_supported: bool,
    policy: Arc<crate::agent_owner::RuntimePolicyClock>,
}
impl CloudAgentCommands {
    pub(super) fn from_runtime(runtime: &CloudAgentRuntime) -> Self {
        Self {
            commands: runtime.commands.clone(),
            closed: runtime.closed.clone(),
            reads_supported: runtime.reads_supported,
            policy: runtime.owner.policy_context(),
        }
    }
    pub(crate) fn policy_clock(&self) -> qos_action_broker::peer_actions::OwnerPolicyClock {
        self.policy.clock()
    }
    /// The same owner clock that validates certified private command times.
    pub(crate) fn policy_context(&self) -> Arc<crate::agent_owner::RuntimePolicyClock> {
        self.policy.clone()
    }
    pub(crate) fn bind_source_policy_time(
        &self,
        time: qos_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        self.policy.bind_source(time)
    }
    async fn request(
        &self,
        evidence: CertifiedAction,
        kind: CommandKind,
        deadline: Deadline,
        custody: Option<AgentReadCustody>,
    ) -> Result<AgentReadReply<CommandReply>, CommandError> {
        if self.closed.load(Ordering::Acquire) || self.commands.is_closed() {
            return Err(AgentReadError::Unavailable.into());
        }
        let now = Deadline::now();
        let deadline = deadline.min(now + Duration::from_secs(5));
        if deadline <= now {
            return Err(AgentReadError::Deadline.into());
        }
        if matches!(kind, CommandKind::Binding | CommandKind::Read) && !self.reads_supported {
            return Err(AgentReadError::Unavailable.into());
        }
        match kind {
            CommandKind::Binding
                if !matches!(
                    &evidence.action().message,
                    ActionMessage::CatalogueRefresh { .. }
                ) =>
            {
                return Err(AgentReadError::InvalidRequest.into());
            }
            CommandKind::Read if !matches!(&evidence.action().message,ActionMessage::AgentReadV1{invocation,request,..}if !invocation.is_nil()&&qos_relay::agent_reads::valid_request(request)) =>
            {
                return Err(AgentReadError::InvalidRequest.into());
            }
            _ => {}
        }
        let policy_time = match custody.as_ref() {
            Some(custody) => match custody.policy_time() {
                Some(time) => time.clone(),
                None if self.policy.is_installed() => return Err(AgentReadError::Denied.into()),
                None => self.policy.capture().map_err(CommandError::Owner)?,
            },
            None => self.policy.capture().map_err(CommandError::Owner)?, // Explicit direct legacy API.
        };
        self.policy
            .validate(&policy_time)
            .map_err(CommandError::Owner)?;
        let permit = self.commands.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => AgentReadError::Capacity,
            mpsc::error::TrySendError::Closed(_) => AgentReadError::Unavailable,
        })?;
        let (reply, response) = oneshot::channel();
        permit.send(Command {
            policy_time,
            evidence,
            kind,
            deadline,
            reply,
            custody,
        });
        // Dropping/timing out a read closes its receiver. Already admitted mutation
        // work is never rolled back; its original invocation remains the status key.
        let result = tokio::time::timeout_at(deadline, response)
            .await
            .map_err(|_| AgentReadError::Deadline)?
            .map_err(|_| AgentReadError::Unavailable)?;
        // timeout_at polls an already-ready inner future before its timer. A
        // caller resumed late must drop that value and custody, not disclose it.
        // Any performed effect remains retained for ordinary status recovery.
        if Deadline::now() >= deadline {
            return Err(AgentReadError::Deadline.into());
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(AgentReadError::Unavailable.into());
        }
        result.try_map(|value| value)
    }
    /// Preserve the existing full agent dispatch, including retained status/cancel.
    /// None leaves ordinary application commands to the same owner's existing broker.
    pub async fn handle_certified(
        &self,
        evidence: CertifiedAction,
        deadline: Deadline,
    ) -> Result<Option<ActionMessage>, String> {
        let reply = self
            .request(evidence, CommandKind::Action, deadline, None)
            .await
            .map_err(|e| e.to_string())?;
        let (value, custody) = reply.into_parts();
        debug_assert!(custody.is_none());
        match value {
            CommandReply::Action(value) => Ok(value),
            _ => Err("invalid agent owner command reply".into()),
        }
    }
    /// Read-only fresh status of the invocation named by the original certified
    /// evidence, for streaming watch transitions. `None` when the invocation is
    /// not an agent-control invocation of this owner.
    pub async fn observe_certified(
        &self,
        evidence: CertifiedAction,
        invocation: InvocationId,
        deadline: Deadline,
    ) -> Result<Option<ActionMessage>, String> {
        let reply = self
            .request(evidence, CommandKind::Observe(invocation), deadline, None)
            .await
            .map_err(|e| e.to_string())?;
        match reply.into_parts().0 {
            CommandReply::Action(value) => Ok(value),
            _ => Err("invalid agent owner observation reply".into()),
        }
    }
    /// Forward the original request's capacity through actual work and response backing.
    pub async fn handle_certified_with_custody(
        &self,
        evidence: CertifiedAction,
        deadline: Deadline,
        custody: AgentReadCustody,
    ) -> Result<AgentReadReply<Option<ActionMessage>>, String> {
        self.request(evidence, CommandKind::Action, deadline, Some(custody))
            .await
            .map_err(|e| e.to_string())?
            .try_map(|reply| match reply {
                CommandReply::Action(value) => Ok(value),
                _ => Err("invalid agent owner command reply".into()),
            })
    }
}
fn read_error(error: CommandError) -> AgentReadError {
    match error {
        CommandError::Read(e) => e,
        CommandError::Owner(_) => AgentReadError::Unavailable,
    }
}
impl CertifiedAgentReads for CloudAgentCommands {
    fn binding(
        &self,
        evidence: CertifiedAction,
        deadline: Deadline,
        custody: Option<AgentReadCustody>,
    ) -> AgentReadFuture<'_, AgentReadReply<AgentReadBinding>> {
        Box::pin(async move {
            self.request(evidence, CommandKind::Binding, deadline, custody)
                .await
                .map_err(read_error)?
                .try_map(|reply| match reply {
                    CommandReply::Binding(value) => Ok(value),
                    _ => Err(AgentReadError::InvalidResult),
                })
        })
    }
    fn read(
        &self,
        evidence: CertifiedAction,
        deadline: Deadline,
        custody: Option<AgentReadCustody>,
    ) -> AgentReadFuture<'_, AgentReadReply<ActionMessage>> {
        Box::pin(async move {
            self.request(evidence, CommandKind::Read, deadline, custody)
                .await
                .map_err(read_error)?
                .try_map(|reply| match reply {
                    CommandReply::Action(Some(value)) => Ok(value),
                    _ => Err(AgentReadError::InvalidResult),
                })
        })
    }
}

fn current_binding(
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    evidence: &CertifiedAction,
) -> Result<(AgentReadBinding, VerifiedAgentCaller, AgentProviderState), AgentReadError> {
    let store = owner.store().map_err(|_| AgentReadError::Unavailable)?;
    let caller = authenticate(&store, evidence).map_err(|_| AgentReadError::Denied)?;
    drop(store);
    let provider = provider.ok_or(AgentReadError::Unavailable)?;
    // Provider authorization opens Store itself: never retain its file lock here.
    let binding = provider
        .read_binding(evidence.certificate())
        .map_err(|_| AgentReadError::Denied)?;
    let publication = owner.snapshot().map_err(|_| AgentReadError::Unavailable)?;
    let store = owner.store().map_err(|_| AgentReadError::Unavailable)?;
    let current = store
        .refresh_agent_caller(&caller)
        .map_err(|_| AgentReadError::Denied)?;
    if publication.governance_revision != current.governance_revision()
        || binding.agent.room != evidence.action().room.to_string()
        || binding.agent.device != hex::encode(current.device())
        || !qos_relay::agent_reads::valid_binding(&binding, publication.host)
    {
        return Err(AgentReadError::Denied);
    }
    Ok((binding, current, publication))
}
/// Private-read bursts served from one Store observation. The browser reads
/// Capabilities, the List and every agent's Conversation back to back each
/// refresh; each read used to open the Store five to six times. The first read
/// of a burst proves the caller and its binding exactly as before and
/// captures a [`ReadBasis`] under one Store lock, stamped with the exact Store
/// files (`store_hint::Verified`). Later reads of the same caller answer from
/// it while the files are unchanged, the policy time stays before the next
/// roster/revocation boundary, the provider publication is identical and the
/// capture is younger than `store_hint::STORE_RECHECK`.
///
/// Only [`read_current`] consults it: a read-only projection, never an
/// authorization verdict. Status, Cancel, catalogue, admission and task
/// actions keep their own fresh Store checks.
#[derive(Default)]
pub(crate) struct ReadCache {
    entries: Vec<ReadEntry>,
    hits: u64,
    misses: u64,
    reported: Option<std::time::Instant>,
}
struct ReadEntry {
    verified: crate::agent_runtime_adapter::store_hint::Verified,
    /// Room and host device of the peer the caller was proved against.
    room: Uuid,
    host_device: [u8; 32],
    publication: AgentProviderState,
    binding: AgentReadBinding,
    basis: crate::agent_runtime_reads::ReadBasis,
}
/// Callers retained at once (one per signed-in device reading agents).
const READ_CACHE_CALLERS: usize = 16;
impl ReadCache {
    /// The retained basis answering `evidence` now, if still exact.
    fn current(
        &mut self,
        root: &Path,
        evidence: &CertifiedAction,
        publication: &AgentProviderState,
        now_ns: u64,
    ) -> Option<&ReadEntry> {
        let action = evidence.action();
        let certificate = evidence.certificate();
        let index = self.entries.iter().position(|entry| {
            entry.basis.certificate() == certificate
                && action.source == certificate.device_public_key
                && action.room == entry.room
                && action.target == entry.host_device
                && entry.publication == *publication
        })?;
        if !self.entries[index].verified.current(root, now_ns) {
            self.entries.swap_remove(index);
            return None;
        }
        Some(&self.entries[index])
    }
    fn retain(&mut self, entry: ReadEntry) {
        let certificate = entry.basis.certificate().clone();
        self.entries
            .retain(|retained| *retained.basis.certificate() != certificate);
        if self.entries.len() >= READ_CACHE_CALLERS {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }
    /// Content-free reuse accounting, one line per 10 s while reads arrive.
    fn count(&mut self, hit: bool) {
        if hit {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        let started = *self.reported.get_or_insert_with(std::time::Instant::now);
        if started.elapsed() >= Duration::from_secs(10) {
            if qos_action_broker::agent_timing::store_spans_enabled() {
                tracing::info!(
                    target: "qos_cloud_host::agent_timing",
                    event = "agent_read_reuse",
                    hits = self.hits,
                    misses = self.misses,
                    window_ms = started.elapsed().as_millis() as u64,
                    "agent timing"
                );
            }
            self.hits = 0;
            self.misses = 0;
            self.reported = Some(std::time::Instant::now());
        }
    }
    /// The retained binding of this caller's exact burst basis, if any: the
    /// catalogue's `agentReadsV1` projection without a Store open.
    pub(crate) fn binding(
        &mut self,
        root: &Path,
        evidence: &CertifiedAction,
        owner: &RuntimeOwner,
        policy_time: &qos_action_broker::peer_actions::PolicyTime,
    ) -> Option<AgentReadBinding> {
        let publication = owner.snapshot().ok()?;
        let now_ns = policy_time.now_ns().ok()?;
        self.current(root, evidence, &publication, now_ns)
            .map(|entry| entry.binding.clone())
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}
pub(super) fn read_current(
    root: &Path,
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    offer: Option<&GrantOffer>,
    evidence: &CertifiedAction,
    policy_time: &qos_action_broker::peer_actions::PolicyTime,
    cache: &mut ReadCache,
) -> Result<ActionMessage, AgentReadError> {
    let ActionMessage::AgentReadV1 {
        host,
        invocation,
        request,
    } = &evidence.action().message
    else {
        return Err(AgentReadError::InvalidRequest);
    };
    if *host != evidence.action().room
        || invocation.is_nil()
        || !qos_relay::agent_reads::valid_request(request)
    {
        return Err(AgentReadError::InvalidRequest);
    }
    let began = std::time::Instant::now();
    let offers: Vec<GrantOffer> = offer.cloned().into_iter().collect();
    let publication = owner.snapshot().map_err(|_| AgentReadError::Unavailable)?;
    let now_ns = policy_time
        .now_ns()
        .map_err(|_| AgentReadError::Unavailable)?;
    if let Some(entry) = cache.current(root, evidence, &publication, now_ns) {
        let result = entry
            .basis
            .answer(*invocation, request, entry.binding.clone(), offers)
            .map_err(|_| AgentReadError::InvalidResult);
        cache.count(true);
        let _timing = ReadTiming(began, 0, 1);
        return result;
    }
    cache.count(false);
    let (binding, caller, publication) = current_binding(owner, provider, evidence)?;
    let binding_us = qos_action_broker::agent_timing::us(began);
    let _timing = ReadTiming(began, binding_us, 0);
    let store = owner
        .store_for(policy_time)
        .map_err(|_| AgentReadError::Unavailable)?;
    let current = store
        .refresh_agent_caller(&caller)
        .map_err(|_| AgentReadError::Denied)?;
    if current.governance_revision() != publication.governance_revision
        || owner.snapshot().map_err(|_| AgentReadError::Unavailable)? != publication
    {
        return Err(AgentReadError::Denied);
    }
    let peers = store
        .peer_shared()
        .map_err(|_| AgentReadError::Unavailable)?;
    let host_device = peers.1.authority().device_public_key;
    let room = peers
        .1
        .room()
        .parse::<Uuid>()
        .map_err(|_| AgentReadError::Unavailable)?;
    let basis = crate::agent_runtime_reads::ReadBasis::capture(&store, &current)
        .map_err(|_| AgentReadError::InvalidResult)?;
    // Stamped while the Store lock is still held: exactly these files.
    let verified = store.policy_now_ns().ok().and_then(|at_ns| {
        crate::agent_runtime_adapter::store_hint::Verified::under_lock(root, &store.state, at_ns)
    });
    drop(store);
    let result = basis
        .answer(*invocation, request, binding.clone(), offers)
        .map_err(|_| AgentReadError::InvalidResult)?;
    if let Some(verified) = verified {
        cache.retain(ReadEntry {
            verified,
            room,
            host_device,
            publication,
            binding,
            basis,
        });
    }
    Ok(result)
}
pub(super) async fn dispatch(
    command: Command,
    root: &Path,
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    offer: Option<&GrantOffer>,
    reads_supported: bool,
    queued: &mut AgentWorkQueue,
    reads: &mut ReadCache,
) {
    // Reads are disposable. A lost mutation waiter still requires exact retained
    // status/uncertainty; do not change already-started mutation semantics.
    let disposable = matches!(
        command.kind,
        CommandKind::Binding | CommandKind::Read | CommandKind::Observe(_)
    ) || matches!(
        &command.evidence.action().message,
        ActionMessage::CatalogueRefresh { .. } | ActionMessage::AgentReadV1 { .. }
    );
    if disposable && command.reply.is_closed() {
        return;
    }
    if Deadline::now() >= command.deadline {
        let _ = command.reply.send(AgentReadReply::new(
            Err(AgentReadError::Deadline.into()),
            command.custody,
        ));
        return;
    }
    if let Err(error) = owner.validate_policy_time(&command.policy_time) {
        let _ = command.reply.send(AgentReadReply::new(
            Err(CommandError::Owner(error)),
            command.custody,
        ));
        return;
    }
    let result = match command.kind {
        // A private read on the certified command path: the read itself
        // proves the caller (freshly, or from the exact burst basis); the two
        // earlier caller proofs of the generic path only reopened the Store.
        CommandKind::Action
            if matches!(
                &command.evidence.action().message,
                ActionMessage::AgentReadV1 { .. }
            ) =>
        {
            if reads_supported {
                read_current(
                    root,
                    owner,
                    provider,
                    offer,
                    &command.evidence,
                    &command.policy_time,
                    reads,
                )
                .map(|value| CommandReply::Action(Some(value)))
                .map_err(|error| CommandError::Owner(error.to_string()))
            } else {
                Ok(CommandReply::Action(None))
            }
        }
        CommandKind::Action => handle_certified_command(
            root,
            owner,
            provider,
            offer,
            reads_supported,
            &command.evidence,
            queued,
            &command.policy_time,
            reads,
        )
        .await
        .map(CommandReply::Action)
        .map_err(CommandError::Owner),
        CommandKind::Binding => current_binding(owner, provider, &command.evidence)
            .map(|(binding, _, _)| CommandReply::Binding(binding))
            .map_err(CommandError::Read),
        CommandKind::Read => read_current(
            root,
            owner,
            provider,
            offer,
            &command.evidence,
            &command.policy_time,
            reads,
        )
        .map(|value| CommandReply::Action(Some(value)))
        .map_err(CommandError::Read),
        CommandKind::Observe(invocation) => observe_certified(owner, &command.evidence, invocation)
            .map(CommandReply::Action)
            .map_err(CommandError::Owner),
    };
    let result = owner
        .validate_policy_time(&command.policy_time)
        .map_err(CommandError::Owner)
        .and(result);
    let _ = command
        .reply
        .send(AgentReadReply::new(result, command.custody));
}
#[cfg(test)]
pub(crate) mod tests;

/// Content-free duration of one agent read on the owner thread; `reused`
/// is 1 when it was answered from the burst basis without a Store open.
struct ReadTiming(std::time::Instant, u64, u64);
impl Drop for ReadTiming {
    fn drop(&mut self) {
        qos_action_broker::agent_timing::slow(
            "agent_read",
            None,
            self.0,
            &[("binding_us", self.1), ("reused", self.2)],
        );
    }
}
