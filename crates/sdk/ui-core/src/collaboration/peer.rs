//! Host-injected authenticated peer transport. No runtime, socket, GPU or Send
//! requirements: the browser adapter owns its asynchronous transport locally.
use super::{ActionDirectoryRequest, CollabEvent, CollabRequest, RouteSpec};

/// Transport adapters must authenticate ordinary peer joins and verify inbound
/// envelopes before publishing events. Installing this adapter grants no model,
/// action, HostContext, or storage authority. Queue failures are fail-closed.
pub trait PeerTransport {
    fn is_connected(&self) -> bool;
    fn send(&mut self, request: CollabRequest) -> Result<(), String>;
    fn poll(&mut self) -> Vec<CollabEvent>;
    /// The real broker acknowledgement of the latest Hello, never an optimistic echo.
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        None
    }
    fn join_route(&mut self, _: RouteSpec) -> Result<(), String> {
        Err("route joining unavailable".into())
    }
    fn send_to_route(&mut self, _: RouteSpec, _: CollabRequest) -> Result<(), String> {
        Err("route sending unavailable".into())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Err("action directory unavailable".into())
    }
    fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        vec![]
    }
    fn install_document_encryption(
        &mut self,
        _: RouteSpec,
        _: channel_crypto::ChannelGroup,
    ) -> Result<(), String> {
        Err("document encryption unavailable".into())
    }
}

/// Drives a host-owned [`PeerTransport`] through the manager's single
/// [`super::CollabTransport`] seam. Queue failures close the adapter, so the
/// manager reports it disconnected instead of continuing under the illusion
/// that durable traffic was sent.
///
/// Peer adapters are thread-bound (browser transports own `!Send` handles).
/// Following the seam's contract, this wrapper satisfies `Send` by checking,
/// at runtime, that it is only ever touched from its creating thread; the
/// manager is only moved before it is first driven.
pub(super) struct PeerTransportAdapter {
    peer: Box<dyn PeerTransport>,
    owner: std::thread::ThreadId,
    closed: Option<String>,
}

// SAFETY: every method asserts it runs on the thread that created the peer,
// so the `!Send` handles inside `peer` are never used from another thread.
// Dropping happens with the owning manager, on that same owner thread.
#[allow(
    unsafe_code,
    reason = "the collaboration seam's documented runtime-checked Send wrapper for thread-bound transports"
)]
unsafe impl Send for PeerTransportAdapter {}

impl PeerTransportAdapter {
    pub(super) fn new(peer: impl PeerTransport + 'static) -> Self {
        Self {
            peer: Box::new(peer),
            owner: std::thread::current().id(),
            closed: None,
        }
    }

    fn check_owner(&self) {
        assert_eq!(
            std::thread::current().id(),
            self.owner,
            "a thread-bound peer transport was driven off its owner thread"
        );
    }

    fn admit(&mut self, result: Result<(), String>) -> Result<(), super::TransportError> {
        result.map_err(|error| {
            tracing::warn!(%error, "collaboration peer transport unavailable");
            self.closed = Some(error);
            super::TransportError::Closed
        })
    }
}

impl super::CollabTransport for PeerTransportAdapter {
    fn status(&self) -> super::TransportStatus {
        self.check_owner();
        if let Some(reason) = &self.closed {
            super::TransportStatus::Closed(reason.clone())
        } else if self.peer.is_connected() {
            super::TransportStatus::Ready
        } else {
            super::TransportStatus::Connecting
        }
    }

    fn submit(&mut self, command: super::TransportCommand) -> Result<(), super::TransportError> {
        use super::TransportCommand;
        self.check_owner();
        if self.closed.is_some() {
            return Err(super::TransportError::Closed);
        }
        let result = match command {
            TransportCommand::Primary(request) => self.peer.send(request),
            TransportCommand::ToRoute(route, request) => self.peer.send_to_route(route, request),
            TransportCommand::Join(route) => self.peer.join_route(route),
            // The adapter reports the real acknowledgement of the latest Hello
            // through `take_primary_route_result`; there is nothing to enqueue.
            TransportCommand::ConfirmPrimaryRoute => Ok(()),
            TransportCommand::InstallDocumentEncryption {
                route,
                group,
                reply,
            } => {
                let installed = self.peer.install_document_encryption(route, *group);
                let _ = reply.send(installed);
                Ok(())
            }
            TransportCommand::PublishActionDirectory { app_id } => {
                self.peer.publish_action_directory(app_id)
            }
        };
        self.admit(result)
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.check_owner();
        if self.closed.is_none() {
            out.extend(self.peer.poll());
        }
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.check_owner();
        self.peer.take_primary_route_result()
    }

    fn take_action_directory_requests(&mut self, out: &mut Vec<ActionDirectoryRequest>) {
        self.check_owner();
        out.extend(self.peer.take_action_directory_requests());
    }
}
