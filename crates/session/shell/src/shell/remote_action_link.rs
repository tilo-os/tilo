//! Remote-action frames between peers over the shell network.
//!
//! A frame arrives as [`NeighbourMsg::RemoteAction`] on the exact-target
//! plane. [`ShellNetwork`] authenticates the sending device, builds the
//! [`Caller`] from its certificate and sorts the frame: requests queue as
//! [`InboundRemoteAction`] for the broker, which answers them through
//! [`BrokerActionHost`](crate::shell::remote_actions::BrokerActionHost);
//! answers wake whoever is waiting in [`ReplyWaiters`], which asks one peer
//! for one thing at a time ([`RemoteActionLink`]) — a record piece, or the
//! person's answer to a confirmation — and never for an action.
//!
//! Running an action on another of the person's devices is the device
//! engine's, in `tilo-peer-state`.
//!
//! [`NeighbourMsg::RemoteAction`]: crate::network::NeighbourMsg::RemoteAction
//! [`ShellNetwork`]: crate::shell::network::ShellNetwork

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tilo_actions::InvocationId;
use tilo_actions::remote::{Caller, Completer, Pending, RemoteActionMessage};

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
    Confirmation(u64),
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
            RemoteActionMessage::ConfirmationRequest { correlation, .. } => {
                Self::Confirmation(*correlation)
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
            RemoteActionMessage::ConfirmationResponse { correlation, .. } => {
                Self::Confirmation(*correlation)
            }
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
