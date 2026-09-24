//! Bounded carrier-neutral access to the same native broker request queue.
use super::*;
use tokio::time::Instant;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarrierError {
    Capacity,
    Closed,
    Deadline,
    Rejected,
}
impl std::fmt::Display for CarrierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "private broker capacity",
            Self::Closed => "private broker closed",
            Self::Deadline => "private broker deadline",
            Self::Rejected => "private broker rejected",
        })
    }
}
impl std::error::Error for CarrierError {}
pub struct PendingPeerAction {
    evidence: CertifiedAction,
    reply: oneshot::Receiver<Result<ActionMessage, ()>>,
    deadline: Instant,
    completed: bool,
    policy_time: Option<PolicyTime>,
    _permit: Arc<OwnedSemaphorePermit>,
}
#[derive(Clone)]
pub struct CarrierTransition {
    source: CertifiedAction,
    update: tilo_actions::InvocationUpdate,
}
impl CarrierTransition {
    pub fn source(&self) -> &CertifiedAction {
        &self.source
    }
    pub fn update(&self) -> &tilo_actions::InvocationUpdate {
        &self.update
    }
}
pub struct CarrierTransitions(
    tokio::sync::broadcast::Receiver<(CertifiedAction, Option<tilo_actions::InvocationUpdate>)>,
);
impl CarrierTransitions {
    pub fn try_next(&mut self) -> Result<Option<CarrierTransition>, CarrierError> {
        match self.0.try_recv() {
            Ok((source, Some(update))) => Ok(Some(CarrierTransition { source, update })),
            Ok((_, None))
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed)
            | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                Err(CarrierError::Closed)
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => Ok(None),
        }
    }
}
impl PendingPeerAction {
    pub fn policy_time(&self) -> Option<&PolicyTime> {
        self.policy_time.as_ref()
    }
    pub fn evidence(&self) -> &CertifiedAction {
        &self.evidence
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn try_reply(&mut self) -> Result<Option<DirectedAction>, CarrierError> {
        if self.completed {
            return Err(CarrierError::Closed);
        }
        if self
            .policy_time
            .as_ref()
            .is_some_and(|time| time.now_ns().is_err())
        {
            self.completed = true;
            return Err(CarrierError::Rejected);
        }
        if Instant::now() >= self.deadline {
            self.completed = true;
            return Err(CarrierError::Deadline);
        }
        match self.reply.try_recv() {
            Ok(Ok(message)) => {
                self.completed = true;
                Ok(Some(bounded_reply(self.evidence.action(), message)))
            }
            Ok(Err(())) => {
                self.completed = true;
                Err(CarrierError::Rejected)
            }
            Err(oneshot::error::TryRecvError::Empty) => Ok(None),
            Err(oneshot::error::TryRecvError::Closed) => {
                self.completed = true;
                Err(CarrierError::Closed)
            }
        }
    }
}
impl PeerActionQueue {
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn carrier_transitions(&self) -> CarrierTransitions {
        CarrierTransitions(self.transitions.subscribe())
    }
    pub fn submit_certified(
        &self,
        evidence: CertifiedAction,
        deadline: Instant,
    ) -> Result<PendingPeerAction, CarrierError> {
        self.submit_carrier(evidence, None, None, deadline, None)
    }
    pub fn submit_certified_with_policy_time(
        &self,
        evidence: CertifiedAction,
        deadline: Instant,
        time: PolicyTime,
    ) -> Result<PendingPeerAction, CarrierError> {
        self.submit_carrier(evidence, None, None, deadline, Some(time))
    }
    pub fn submit_transition_with_policy_time(
        &self,
        evidence: CertifiedAction,
        transition: CarrierTransition,
        deadline: Instant,
        time: PolicyTime,
    ) -> Result<PendingPeerAction, CarrierError> {
        self.submit_transition_inner(evidence, transition, deadline, Some(time))
    }
    pub fn submit_transition(
        &self,
        evidence: CertifiedAction,
        transition: CarrierTransition,
        deadline: Instant,
    ) -> Result<PendingPeerAction, CarrierError> {
        self.submit_transition_inner(evidence, transition, deadline, None)
    }
    fn submit_transition_inner(
        &self,
        evidence: CertifiedAction,
        transition: CarrierTransition,
        deadline: Instant,
        policy_time: Option<PolicyTime>,
    ) -> Result<PendingPeerAction, CarrierError> {
        let action = evidence.action();
        let source = transition.source.action();
        let invocation = transition.update.invocation();
        if action.room != source.room
            || action.source != source.source
            || action.target != source.target
            || action.message.invocation() != Some(invocation)
        {
            return Err(CarrierError::Rejected);
        }
        self.submit_carrier(
            evidence,
            Some(invocation),
            Some(transition.update),
            deadline,
            policy_time,
        )
    }
    fn submit_carrier(
        &self,
        evidence: CertifiedAction,
        poll: Option<tilo_actions::InvocationId>,
        notification: Option<tilo_actions::InvocationUpdate>,
        deadline: Instant,
        policy_time: Option<PolicyTime>,
    ) -> Result<PendingPeerAction, CarrierError> {
        if let Some(time) = &policy_time {
            time.now_ns().map_err(|_| CarrierError::Rejected)?;
        }
        let deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(5));
        if deadline <= Instant::now() {
            return Err(CarrierError::Deadline);
        }
        let permit = Arc::new(
            self.permits
                .clone()
                .try_acquire_owned()
                .map_err(|e| match e {
                    tokio::sync::TryAcquireError::Closed => CarrierError::Closed,
                    tokio::sync::TryAcquireError::NoPermits => CarrierError::Capacity,
                })?,
        );
        let (reply, rx) = oneshot::channel();
        self.sender
            .try_send(Request {
                policy_time: policy_time.clone(),
                evidence: Some(evidence.clone()),
                poll,
                notification,
                deadline,
                reply,
                _permit: permit.clone(),
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => CarrierError::Capacity,
                mpsc::error::TrySendError::Closed(_) => CarrierError::Closed,
            })?;
        Ok(PendingPeerAction {
            evidence,
            reply: rx,
            deadline,
            completed: false,
            policy_time,
            _permit: permit,
        })
    }
}

#[cfg(test)]
mod tests;
