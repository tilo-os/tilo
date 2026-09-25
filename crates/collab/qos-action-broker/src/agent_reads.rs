//! Host-installed certified read service. The trait is a local service contract,
//! not an authority witness; production hosts attach their actual live owner.
use std::{future::Future, pin::Pin, sync::Arc};
use tilo_relay::{
    agent_reads::AgentReadBinding, client::CertifiedAction, peer_actions::ActionMessage,
};
use tokio::time::Instant;

pub type AgentReadFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AgentReadError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentReadError {
    Unavailable,
    Capacity,
    Deadline,
    Denied,
    InvalidRequest,
    InvalidResult,
}
impl std::fmt::Display for AgentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "certified agent owner unavailable",
            Self::Capacity => "certified agent owner capacity",
            Self::Deadline => "certified agent read deadline",
            Self::Denied => "certified agent read denied",
            Self::InvalidRequest => "invalid certified agent read request",
            Self::InvalidResult => "invalid certified agent read result",
        })
    }
}
impl std::error::Error for AgentReadError {}
/// Custody of the original broker queue slot; no new capacity is minted.
pub struct AgentReadCustody {
    policy_time: Option<crate::peer_actions::PolicyTime>,
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
}
impl std::fmt::Debug for AgentReadCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentReadCustody")
            .field("has_policy_time", &self.policy_time.is_some())
            .finish_non_exhaustive()
    }
}
impl AgentReadCustody {
    pub fn policy_time(&self) -> Option<&crate::peer_actions::PolicyTime> {
        self.policy_time.as_ref()
    }
    pub fn from_request(request: &crate::peer_actions::Request) -> Self {
        Self {
            policy_time: request.policy_time().cloned(),
            _permit: request._permit.clone(),
        }
    }
}
/// Keep result allocations alive before releasing the original queue slot.
#[derive(Debug)]
pub struct AgentReadReply<T> {
    value: T,
    custody: Option<AgentReadCustody>,
}
impl<T> AgentReadReply<T> {
    pub fn new(value: T, custody: Option<AgentReadCustody>) -> Self {
        Self { value, custody }
    }
    pub fn value(&self) -> &T {
        &self.value
    }
    /// A response adapter must retain the returned custody through body release.
    pub fn into_parts(self) -> (T, Option<AgentReadCustody>) {
        (self.value, self.custody)
    }
    pub fn try_map<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<AgentReadReply<U>, E> {
        let value = f(self.value)?;
        Ok(AgentReadReply::new(value, self.custody))
    }
}
pub trait CertifiedAgentReads: Send + Sync {
    fn binding(
        &self,
        evidence: CertifiedAction,
        deadline: Instant,
        custody: Option<AgentReadCustody>,
    ) -> AgentReadFuture<'_, AgentReadReply<AgentReadBinding>>;
    fn read(
        &self,
        evidence: CertifiedAction,
        deadline: Instant,
        custody: Option<AgentReadCustody>,
    ) -> AgentReadFuture<'_, AgentReadReply<ActionMessage>>;
}

/// Execute the disposable read with its original evidence and correlate the
/// bounded result before the ordinary broker hands it to response custody.
pub async fn read_from_owner(
    owner: &dyn CertifiedAgentReads,
    host: uuid::Uuid,
    evidence: CertifiedAction,
    deadline: Instant,
    custody: Option<AgentReadCustody>,
) -> Result<AgentReadReply<ActionMessage>, AgentReadError> {
    let ActionMessage::AgentReadV1 {
        host: requested_host,
        invocation,
        request,
    } = &evidence.action().message
    else {
        return Err(AgentReadError::InvalidRequest);
    };
    if *requested_host != host || !evidence.action().message.validate() {
        return Err(AgentReadError::InvalidRequest);
    }
    if Instant::now() >= deadline {
        return Err(AgentReadError::Deadline);
    }
    let invocation = *invocation;
    let room = evidence.action().room.to_string();
    let device = hex::encode(evidence.action().source);
    let request = request.clone();
    let result = owner.read(evidence, deadline, custody).await?;
    if Instant::now() >= deadline {
        return Err(AgentReadError::Deadline);
    }
    match result.value() {
        ActionMessage::AgentReadResultV1 {
            host: actual,
            invocation: id,
            binding,
            response,
        } if *actual == host
            && *id == invocation
            && binding.agent.room == room
            && binding.agent.device == device
            && tilo_relay::agent_reads::matches_request(&request, response)
            && result.value().validate() =>
        {
            Ok(result)
        }
        _ => Err(AgentReadError::InvalidResult),
    }
}
