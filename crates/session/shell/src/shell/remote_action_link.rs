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
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, Catalogue, InvocationReceipt, InvocationState,
    InvokeRequest, RemoteActionMessage,
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
}

impl ReplyKey {
    /// The answer a request expects, or `None` for a message that is not a
    /// request.
    #[must_use]
    pub fn for_request(message: &RemoteActionMessage) -> Option<Self> {
        Some(match message {
            RemoteActionMessage::CatalogueRequest { correlation } => Self::Catalogue(*correlation),
            RemoteActionMessage::Invoke(request) => Self::Invoke(request.invocation),
            RemoteActionMessage::Observe { invocation } => Self::Observe(*invocation),
            RemoteActionMessage::Cancel { invocation } => Self::Cancel(*invocation),
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
            _ => return None,
        })
    }
}

/// Senders waiting for one answer from one device.
type Waiting = HashMap<(iroh::EndpointId, ReplyKey), Vec<mpsc::Sender<RemoteActionMessage>>>;

/// Callers waiting for an answer from a specific device. An answer is only
/// delivered to a waiter registered for the device it came from, so one
/// peer cannot answer a question put to another.
#[derive(Default)]
pub struct ReplyWaiters {
    waiting: Mutex<Waiting>,
}

impl ReplyWaiters {
    /// Wait for the answer `key` from `peer`.
    pub fn register(
        &self,
        peer: iroh::EndpointId,
        key: ReplyKey,
    ) -> mpsc::Receiver<RemoteActionMessage> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.entry((peer, key)).or_default().push(tx);
        }
        rx
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
        for waiter in waiters {
            let _ = waiter.send(message.clone());
        }
        true
    }
}

/// Sends one request to a peer and hands back where its answer arrives.
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
    ) -> Result<mpsc::Receiver<RemoteActionMessage>, String>;
}

/// Another peer's actions, used as an [`ActionHost`].
///
/// The remote host authorizes the transport-authenticated device this shell
/// connects as; the `caller` argument names nobody on the wire, so it is not
/// sent.
pub struct PeerActionHost<L> {
    link: L,
    workspace_topic: u64,
    peer: iroh::EndpointId,
    timeout: Duration,
    next_correlation: std::sync::atomic::AtomicU64,
}

impl<L: RemoteActionLink> PeerActionHost<L> {
    pub fn new(link: L, workspace_topic: u64, peer: iroh::EndpointId, timeout: Duration) -> Self {
        Self {
            link,
            workspace_topic,
            peer,
            timeout,
            next_correlation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn ask(&self, message: RemoteActionMessage) -> Result<RemoteActionMessage, ActionHostError> {
        let unavailable = |reason: String| ActionHostError::Unavailable { reason };
        self.link
            .request(self.workspace_topic, self.peer, message)
            .map_err(unavailable)?
            .recv_timeout(self.timeout)
            .map_err(|_| unavailable("the peer did not answer in time".into()))
    }
}

fn unexpected() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "the peer answered with an unexpected message".into(),
    }
}

impl<L: RemoteActionLink> ActionHost for PeerActionHost<L> {
    fn catalogue(&self, _caller: &Caller) -> Result<Catalogue, ActionHostError> {
        let correlation = self
            .next_correlation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.ask(RemoteActionMessage::CatalogueRequest { correlation })? {
            RemoteActionMessage::CatalogueResponse { result, .. } => result,
            _ => Err(unexpected()),
        }
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        match self.ask(RemoteActionMessage::Invoke(request))? {
            RemoteActionMessage::Receipt(receipt) => match receipt.status {
                tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
                _ => Ok(receipt),
            },
            _ => Err(unexpected()),
        }
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        match self.ask(RemoteActionMessage::Observe { invocation: *id }) {
            Ok(RemoteActionMessage::Update { state, .. }) => state,
            _ => None,
        }
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        match self.ask(RemoteActionMessage::Cancel { invocation: *id })? {
            RemoteActionMessage::CancelResponse { result, .. } => result,
            _ => Err(unexpected()),
        }
    }
}

impl<L: RemoteActionLink> RemoteActionLink for Arc<L> {
    fn request(
        &self,
        workspace_topic: u64,
        peer: iroh::EndpointId,
        message: RemoteActionMessage,
    ) -> Result<mpsc::Receiver<RemoteActionMessage>, String> {
        (**self).request(workspace_topic, peer, message)
    }
}
