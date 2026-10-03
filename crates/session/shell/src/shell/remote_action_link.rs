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
    InvocationReceipt, InvocationState, InvokeRequest, Pending, RemoteActionMessage,
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
            RemoteActionMessage::Observe { invocation } => Self::Observe(*invocation),
            RemoteActionMessage::Cancel { invocation } => Self::Cancel(*invocation),
            RemoteActionMessage::RecordRequest { correlation, .. } => Self::Record(*correlation),
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
            _ => return None,
        })
    }
}

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
        let deadline = now.checked_add(REPLY_DEADLINE).unwrap_or(now);
        if let Ok(mut waiting) = self.waiting.lock() {
            // Dropping an expired completer abandons its answer.
            waiting.retain(|_, waiters| {
                waiters.retain(|(until, _)| *until > now);
                !waiters.is_empty()
            });
            waiting
                .entry((peer, key))
                .or_default()
                .push((deadline, completer));
        }
        answer
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
}

impl<L: RemoteActionLink> PeerActionHost<L> {
    pub fn new(link: L, workspace_topic: u64, peer: iroh::EndpointId) -> Self {
        Self {
            link,
            workspace_topic,
            peer,
            next_correlation: std::sync::atomic::AtomicU64::new(1),
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

    /// Asks the peer and waits for its answer, as the trait's synchronous
    /// shape requires; call it off any thread that delivers peer frames.
    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.ask_now(
            RemoteActionMessage::Observe { invocation: *id },
            |answer| match answer {
                RemoteActionMessage::Update { state, .. } => Ok(state),
                _ => Err(unexpected()),
            },
        )
        .ok()
        .flatten()
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
