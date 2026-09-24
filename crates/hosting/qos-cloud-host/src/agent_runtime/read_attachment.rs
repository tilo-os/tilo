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
pub(super) fn read_current(
    owner: &RuntimeOwner,
    provider: Option<&Arc<AgentControlProvider>>,
    offer: Option<&GrantOffer>,
    evidence: &CertifiedAction,
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
    let (binding, caller, publication) = current_binding(owner, provider, evidence)?;
    let store = owner.store().map_err(|_| AgentReadError::Unavailable)?;
    let current = store
        .refresh_agent_caller(&caller)
        .map_err(|_| AgentReadError::Denied)?;
    if current.governance_revision() != publication.governance_revision
        || owner.snapshot().map_err(|_| AgentReadError::Unavailable)? != publication
    {
        return Err(AgentReadError::Denied);
    }
    let result = crate::agent_runtime_reads::read(
        &store,
        &current,
        *invocation,
        request,
        binding,
        offer.cloned().into_iter().collect(),
    )
    .map_err(|_| AgentReadError::InvalidResult)?;
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
        CommandKind::Action => handle_certified_command(
            root,
            owner,
            provider,
            offer,
            reads_supported,
            &command.evidence,
            queued,
            &command.policy_time,
        )
        .await
        .map(CommandReply::Action)
        .map_err(CommandError::Owner),
        CommandKind::Binding => current_binding(owner, provider, &command.evidence)
            .map(|(binding, _, _)| CommandReply::Binding(binding))
            .map_err(CommandError::Read),
        CommandKind::Read => read_current(owner, provider, offer, &command.evidence)
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
