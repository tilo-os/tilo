//! Remote-action frames between peers over the shell network.
//!
//! A frame arrives as [`NeighbourMsg::RemoteAction`] on the exact-target
//! plane. [`ShellNetwork`] authenticates the sending device, builds the
//! [`Caller`] from its certificate and sorts the frame: requests queue as
//! [`InboundRemoteAction`] for the broker, which answers them through
//! [`BrokerActionHost`](crate::shell::remote_actions::BrokerActionHost);
//! answers wake whoever is waiting in [`ReplyWaiters`]. [`PeerActionHost`]
//! turns that round trip back into an [`ActionHost`], so a remote peer's
//! actions are used through the same trait as the local session's.
//!
//! [`NeighbourMsg::RemoteAction`]: crate::network::NeighbourMsg::RemoteAction
//! [`ShellNetwork`]: crate::shell::network::ShellNetwork

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, RemoteActionMessage, UpdateMark,
};

/// A request from an authenticated remote device, waiting for the broker.
#[derive(Clone, Debug)]
pub struct InboundRemoteAction {
    /// Workspace topic the frame arrived on; the answer goes back on it.
    pub workspace_topic: u64,
    /// Transport endpoint of the requesting device.
    pub from: iroh::EndpointId,
    /// The principal the transport authenticated for `from`.
    pub caller: Caller,
    pub message: RemoteActionMessage,
}

/// What pairs an answer with the request that asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReplyKey {
    Catalogue(u64),
    Invoke(InvocationId),
    Observe(InvocationId),
    Cancel(InvocationId),
    Record(u64),
    Feedback(u64),
}

impl ReplyKey {
    /// The answer a request expects, or `None` for a message that is not a
    /// request.
    #[must_use]
    pub fn for_request(message: &RemoteActionMessage) -> Option<Self> {
        Some(match message {
            RemoteActionMessage::CatalogueRequest { correlation, .. } => {
                Self::Catalogue(*correlation)
            }
            RemoteActionMessage::Invoke(request) => Self::Invoke(request.invocation),
            RemoteActionMessage::Observe { invocation, .. } => Self::Observe(*invocation),
            RemoteActionMessage::Cancel { invocation } => Self::Cancel(*invocation),
            RemoteActionMessage::RecordRequest { correlation, .. } => Self::Record(*correlation),
            RemoteActionMessage::FeedbackRequest { correlation, .. } => {
                Self::Feedback(*correlation)
            }
            _ => return None,
        })
    }

    /// The request an answer belongs to, or `None` for a request.
    #[must_use]
    pub fn for_reply(message: &RemoteActionMessage) -> Option<Self> {
        Some(match message {
            RemoteActionMessage::CatalogueResponse { correlation, .. } => {
                Self::Catalogue(*correlation)
            }
            RemoteActionMessage::Receipt(receipt) => Self::Invoke(receipt.invocation),
            RemoteActionMessage::Update { invocation, .. } => Self::Observe(*invocation),
            RemoteActionMessage::CancelResponse { invocation, .. } => Self::Cancel(*invocation),
            RemoteActionMessage::RecordResponse { correlation, .. } => Self::Record(*correlation),
            RemoteActionMessage::FeedbackResponse { correlation, .. } => {
                Self::Feedback(*correlation)
            }
            _ => return None,
        })
    }
}

/// How long one long-poll for a peer's next change asks the peer to hold
/// it; well inside [`REPLY_DEADLINE`], so a held answer is never taken for
/// a lost one.
pub const CHANGE_WAIT_MS: u32 = 10_000;

/// How long a device has to answer before its waiters are released. A lost
/// frame then surfaces as an unavailable answer instead of waiting forever.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// Answers owed by one device, each with the moment it stops being awaited.
type Waiting =
    HashMap<(iroh::EndpointId, ReplyKey), Vec<(Instant, Completer<RemoteActionMessage>)>>;

/// Callers waiting for an answer from a specific device. An answer is only
/// delivered to a waiter registered for the device it came from, so one
/// peer cannot answer a question put to another.
#[derive(Default)]
pub struct ReplyWaiters {
    waiting: Mutex<Waiting>,
}

impl ReplyWaiters {
    /// Wait for the answer `key` from `peer`.
    pub fn register(&self, peer: iroh::EndpointId, key: ReplyKey) -> Pending<RemoteActionMessage> {
        let (completer, answer) = tilo_actions::remote::pending();
        let now = Instant::now();
        self.expire(now);
        let deadline = now.checked_add(REPLY_DEADLINE).unwrap_or(now);
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting
                .entry((peer, key))
                .or_default()
                .push((deadline, completer));
        }
        answer
    }

    /// Release unanswered requests even when no further request is made.
    /// Callbacks may immediately try another peer, so release them outside
    /// the registry lock rather than while retaining its entries.
    pub(crate) fn expire(&self, now: Instant) {
        let expired = {
            let Ok(mut waiting) = self.waiting.lock() else {
                return;
            };
            let mut expired = Vec::new();
            waiting.retain(|_, waiters| {
                for (until, completer) in std::mem::take(waiters) {
                    if until <= now {
                        expired.push(completer);
                    } else {
                        waiters.push((until, completer));
                    }
                }
                !waiters.is_empty()
            });
            drop(waiting);
            expired
        };
        drop(expired);
    }

    /// Hand an answer from `peer` to everyone waiting for it. Returns
    /// whether anyone was.
    pub fn deliver(&self, peer: iroh::EndpointId, message: &RemoteActionMessage) -> bool {
        let Some(key) = ReplyKey::for_reply(message) else {
            return false;
        };
        let Some(waiters) = self
            .waiting
            .lock()
            .ok()
            .and_then(|mut waiting| waiting.remove(&(peer, key)))
        else {
            return false;
        };
        for (_, waiter) in waiters {
            waiter.complete(message.clone());
        }
        true
    }
}

/// Sends one request to a peer and hands back its answer, when it comes.
pub trait RemoteActionLink: Send + Sync {
    /// Send `message` to `peer` on `workspace_topic`.
    ///
    /// # Errors
    ///
    /// Returns a reason when the message is not a request or cannot be sent.
    fn request(
        &self,
        workspace_topic: u64,
        peer: iroh::EndpointId,
        message: RemoteActionMessage,
    ) -> Result<Pending<RemoteActionMessage>, String>;
}

/// Another peer's actions, used as an [`ActionHost`].
///
/// The remote host authorizes the transport-authenticated device this shell
/// connects as; the `caller` argument names nobody on the wire, so it is not
/// sent. Nothing here blocks: every answer is the peer's reply, when it
/// arrives.
pub struct PeerActionHost<L> {
    link: L,
    workspace_topic: u64,
    peer: iroh::EndpointId,
    next_correlation: std::sync::atomic::AtomicU64,
    follow: std::sync::Arc<Following>,
}

/// What [`PeerActionHost::next_change`] knows of the invocations it follows.
#[derive(Default)]
struct Following {
    /// How far each running invocation was last seen to get: what a
    /// long-poll asks the peer to move past.
    seen: Mutex<HashMap<InvocationId, UpdateMark>>,
    /// The state a long-poll brought back, for the observe that follows its
    /// wake, so one change costs one round trip, not two.
    fresh: Mutex<HashMap<InvocationId, InvocationState>>,
    /// Set once the peer answered a long-poll at once with nothing new: it
    /// predates waits, so its callers keep their own schedule.
    unheld: std::sync::atomic::AtomicBool,
}

/// Most invocations one host follows at a time; past it the oldest marks are
/// forgotten, which costs a forgotten one a longer wait, nothing else.
const MAX_FOLLOWED: usize = 1024;

impl Following {
    fn saw(&self, id: InvocationId, state: Option<&InvocationState>) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        match state.filter(|state| !state.is_terminal()) {
            Some(state) => {
                if seen.len() >= MAX_FOLLOWED && !seen.contains_key(&id) {
                    seen.clear();
                }
                seen.insert(id, UpdateMark::of(&state.update));
            }
            None => {
                seen.remove(&id);
            }
        }
    }

    fn seen(&self, id: &InvocationId) -> Option<UpdateMark> {
        self.seen.lock().ok()?.get(id).copied()
    }

    /// Keep what a long-poll brought back. A `None` is not kept: the observe
    /// that follows asks again, and hears the same refusal.
    fn keep_fresh(&self, id: InvocationId, state: Option<InvocationState>) {
        if let (Some(state), Ok(mut fresh)) = (state, self.fresh.lock()) {
            if fresh.len() >= MAX_FOLLOWED && !fresh.contains_key(&id) {
                fresh.clear();
            }
            fresh.insert(id, state);
        }
    }

    fn take_fresh(&self, id: &InvocationId) -> Option<InvocationState> {
        self.fresh.lock().ok()?.remove(id)
    }
}

impl<L: RemoteActionLink> PeerActionHost<L> {
    pub fn new(link: L, workspace_topic: u64, peer: iroh::EndpointId) -> Self {
        Self {
            link,
            workspace_topic,
            peer,
            next_correlation: std::sync::atomic::AtomicU64::new(1),
            follow: std::sync::Arc::default(),
        }
    }

    fn ask<T: Send + 'static>(
        &self,
        message: RemoteActionMessage,
        answer: impl FnOnce(RemoteActionMessage) -> Result<T, ActionHostError> + Send + 'static,
    ) -> Pending<Result<T, ActionHostError>> {
        match self.link.request(self.workspace_topic, self.peer, message) {
            Err(reason) => Pending::ready(Err(ActionHostError::Unavailable { reason })),
            Ok(reply) => {
                let (completer, result) = tilo_actions::remote::pending();
                reply.then(move |reply| {
                    completer.complete(reply.map_or_else(
                        || {
                            Err(ActionHostError::Unavailable {
                                reason: "the peer did not answer in time".into(),
                            })
                        },
                        answer,
                    ));
                });
                result
            }
        }
    }

    fn ask_now<T: Send + 'static>(
        &self,
        message: RemoteActionMessage,
        answer: impl FnOnce(RemoteActionMessage) -> Result<T, ActionHostError> + Send + 'static,
    ) -> Result<T, ActionHostError> {
        match self.ask(message, answer).wait_timeout(REPLY_DEADLINE) {
            Ok(Some(result)) => result,
            Ok(None) | Err(_) => Err(ActionHostError::Unavailable {
                reason: "the peer did not answer in time".into(),
            }),
        }
    }
}

fn unexpected() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "the peer answered with an unexpected message".into(),
    }
}

impl<L: RemoteActionLink> ActionHost for PeerActionHost<L> {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let correlation = self
            .next_correlation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ask(
            RemoteActionMessage::CatalogueRequest { correlation, query },
            |answer| match answer {
                RemoteActionMessage::CatalogueResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.ask(
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => match receipt.status {
                    tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
                    _ => Ok(receipt),
                },
                _ => Err(unexpected()),
            },
        )
    }

    /// The state the long-poll that just woke its caller brought back, or
    /// else the peer's answer to asking, waited for as the trait's
    /// synchronous shape requires; call it off any thread that delivers
    /// peer frames.
    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let state = self.follow.take_fresh(id).or_else(|| {
            self.ask_now(RemoteActionMessage::observe(*id), |answer| match answer {
                RemoteActionMessage::Update { state, .. } => Ok(state),
                _ => Err(unexpected()),
            })
            .ok()
            .flatten()
        });
        self.follow.saw(*id, state.as_ref());
        state
    }

    /// A long-poll: one waiting observe the peer answers when the
    /// invocation moves past what was last seen of it, or after
    /// [`CHANGE_WAIT_MS`]. No thread waits for it. `None` for a peer that
    /// answered one at once with nothing new, so its callers keep their own
    /// schedule instead of asking as fast as it answers.
    fn next_change(&self, _caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        use std::sync::atomic::Ordering;
        if self.follow.unheld.load(Ordering::Relaxed) {
            return None;
        }
        let seen = self.follow.seen(id);
        let reply = self
            .link
            .request(
                self.workspace_topic,
                self.peer,
                RemoteActionMessage::Observe {
                    invocation: *id,
                    wait_ms: Some(CHANGE_WAIT_MS),
                    seen,
                },
            )
            .ok()?;
        let sent = tilo_clock::now();
        let follow = std::sync::Arc::clone(&self.follow);
        let invocation = *id;
        let (changed, change) = tilo_actions::remote::pending();
        reply.then(move |reply| {
            if let Some(RemoteActionMessage::Update { state, .. }) = reply {
                let same = state.as_ref().map(|state| UpdateMark::of(&state.update)) == seen
                    && state.is_some();
                let early = tilo_clock::now().saturating_duration_since(sent)
                    < Duration::from_millis(u64::from(CHANGE_WAIT_MS / 2));
                if same && early {
                    follow.unheld.store(true, Ordering::Relaxed);
                }
                follow.keep_fresh(invocation, state);
            }
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        self.ask_now(
            RemoteActionMessage::Cancel { invocation: *id },
            |answer| match answer {
                RemoteActionMessage::CancelResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }
}

impl<L: RemoteActionLink> RemoteActionLink for std::sync::Arc<L> {
    fn request(
        &self,
        workspace_topic: u64,
        peer: iroh::EndpointId,
        message: RemoteActionMessage,
    ) -> Result<Pending<RemoteActionMessage>, String> {
        (**self).request(workspace_topic, peer, message)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHANGE_WAIT_MS, PeerActionHost, REPLY_DEADLINE, RemoteActionLink, ReplyKey, ReplyWaiters,
    };
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tilo_actions::remote::{
        ActionHost, Caller, Completer, InvocationState, Pending, PendingPoll, RemoteActionMessage,
        UpdateMark,
    };
    use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

    /// A link whose requests the test answers by hand.
    #[derive(Default)]
    struct HeldLink {
        sent: Mutex<Vec<(RemoteActionMessage, Completer<RemoteActionMessage>)>>,
    }

    impl RemoteActionLink for HeldLink {
        fn request(
            &self,
            _workspace_topic: u64,
            _peer: iroh::EndpointId,
            message: RemoteActionMessage,
        ) -> Result<Pending<RemoteActionMessage>, String> {
            let (reply, answer) = tilo_actions::remote::pending();
            self.sent.lock().unwrap().push((message, reply));
            Ok(answer)
        }
    }

    impl HeldLink {
        /// Answer the oldest unanswered request with `update`, returning it.
        fn answer(&self, id: InvocationId, update: InvocationUpdate) -> RemoteActionMessage {
            let (asked, reply) = self.sent.lock().unwrap().remove(0);
            reply.complete(RemoteActionMessage::Update {
                invocation: id,
                state: Some(InvocationState::new(id, update)),
            });
            asked
        }
    }

    fn peer_host() -> (Arc<HeldLink>, PeerActionHost<Arc<HeldLink>>) {
        let link = Arc::new(HeldLink::default());
        let peer = iroh::SecretKey::from_bytes(&[0xC3; 32]).public();
        (Arc::clone(&link), PeerActionHost::new(link, 7, peer))
    }

    /// Following a peer's invocation is one waiting request per change: it
    /// returns at once, holds no thread, and the observe its wake prompts
    /// reads what the answer brought instead of asking again.
    #[test]
    fn peer_host_next_change_long_polls() {
        let (link, host) = peer_host();
        let caller = Caller::local_person("person:owner");
        let id = InvocationId::new();
        let mut change = host
            .next_change(&caller, &id)
            .expect("a peer can be waited on");
        assert!(matches!(change.poll_ready(), PendingPoll::Waiting));
        let done = InvocationUpdate::Succeeded {
            invocation: id,
            output: ActionOutput::default(),
        };
        assert_eq!(
            link.answer(id, done.clone()),
            RemoteActionMessage::Observe {
                invocation: id,
                wait_ms: Some(CHANGE_WAIT_MS),
                seen: None,
            }
        );
        assert!(matches!(change.poll_ready(), PendingPoll::Ready(())));
        assert_eq!(
            host.observe(&caller, &id).map(|state| state.update),
            Some(done)
        );
        assert!(
            link.sent.lock().unwrap().is_empty(),
            "the woken observe cost no second round trip"
        );
    }

    /// A peer that answers a long-poll at once with nothing new predates
    /// waits; its callers go back to their own schedule rather than ask as
    /// fast as it answers.
    #[test]
    fn a_peer_that_does_not_hold_long_polls_is_not_long_polled() {
        let (link, host) = peer_host();
        let caller = Caller::local_person("person:owner");
        let id = InvocationId::new();
        let accepted = InvocationUpdate::Accepted { invocation: id };
        let first = host.next_change(&caller, &id).unwrap();
        link.answer(id, accepted.clone());
        drop(first);
        assert!(host.observe(&caller, &id).is_some());
        let _second = host.next_change(&caller, &id).unwrap();
        let asked = link.answer(id, accepted);
        assert!(matches!(
            asked,
            RemoteActionMessage::Observe {
                seen: Some(UpdateMark::Accepted),
                ..
            }
        ));
        assert!(host.next_change(&caller, &id).is_none());
    }

    #[test]
    fn an_idle_expired_request_is_abandoned_without_another_request() {
        let waiters = ReplyWaiters::default();
        let peer = iroh::SecretKey::from_bytes(&[0xC1; 32]).public();
        let mut answer = waiters.register(peer, ReplyKey::Record(1));
        waiters.expire(Instant::now());
        assert!(matches!(answer.poll_ready(), PendingPoll::Waiting));
        waiters.expire(Instant::now().checked_add(REPLY_DEADLINE).unwrap());
        assert!(matches!(answer.poll_ready(), PendingPoll::Abandoned));
    }

    #[test]
    fn expiry_callback_can_register_the_next_request_without_deadlocking() {
        let waiters = Arc::new(ReplyWaiters::default());
        let next = Arc::new(Mutex::new(None));
        let peer = iroh::SecretKey::from_bytes(&[0xC2; 32]).public();
        let callback_waiters = Arc::clone(&waiters);
        let callback_next = Arc::clone(&next);
        waiters
            .register(peer, ReplyKey::Record(1))
            .then(move |reply| {
                assert!(reply.is_none());
                *callback_next.lock().unwrap() =
                    Some(callback_waiters.register(peer, ReplyKey::Record(2)));
            });
        waiters.expire(Instant::now().checked_add(REPLY_DEADLINE).unwrap());
        let mut answer = next
            .lock()
            .unwrap()
            .take()
            .expect("retry callback ran outside the registry lock");
        assert!(matches!(answer.poll_ready(), PendingPoll::Waiting));
        waiters.expire(
            Instant::now()
                .checked_add(REPLY_DEADLINE)
                .unwrap()
                .checked_add(Duration::from_secs(1))
                .unwrap(),
        );
        assert!(matches!(answer.poll_ready(), PendingPoll::Abandoned));
    }
}
