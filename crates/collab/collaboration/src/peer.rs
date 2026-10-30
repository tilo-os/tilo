//! Host-injected authenticated peer transport. No runtime, socket, GPU or Send
//! requirements: the browser adapter owns its asynchronous transport locally.
use super::{ActionDirectoryRequest, CollabEvent, CollabRequest, RouteSpec, annotation};

/// Transport adapters must authenticate ordinary peer joins and verify inbound
/// envelopes before publishing events. Installing this adapter grants no model,
/// action, HostContext, or storage authority. Queue failures are fail-closed.
pub trait PeerTransport {
    /// Authenticated connection provenance from the host, never app model state.
    fn connection_snapshot(
        &self,
        _: &RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        None
    }
    fn is_connected(&self) -> bool;
    fn send(&mut self, request: CollabRequest) -> Result<(), String>;
    fn poll(&mut self) -> Vec<CollabEvent>;
    /// The real broker acknowledgement of the latest Hello, never an optimistic echo.
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        None
    }
    /// Join `route`, whose record holds `record_kind`. The kind is what a
    /// host needs to create the record on this device's first change.
    fn join_route(&mut self, _: RouteSpec, _: crate::record::RecordKind) -> Result<(), String> {
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
    /// Install the callback that schedules this owner's next frame, as
    /// [`super::CollabTransport::set_wake`] does for a host-supplied
    /// transport: a peer answered on a thread of its own calls it, so what
    /// it carries is not left waiting for an unrelated frame.
    fn set_wake(&mut self, _wake: Box<dyn Fn() + Send + Sync>) {}
}

/// Drives a host-owned [`PeerTransport`] through the manager's single
/// [`super::CollabTransport`] seam. Queue failures close the adapter, so the
/// manager reports it disconnected instead of continuing under the illusion
/// that durable traffic was sent.
///
/// Peer adapters are thread-bound (browser transports own `!Send` handles).
/// Following the seam's contract, this wrapper satisfies `Send` by checking,
/// at runtime, that it is only ever touched from its creating thread: every
/// method asserts it, and dropping it anywhere else leaks the peer rather than
/// running its destructor off the owner thread.
pub(super) struct PeerTransportAdapter {
    peer: std::mem::ManuallyDrop<Box<dyn PeerTransport>>,
    owner: std::thread::ThreadId,
    closed: Option<String>,
    /// The last change the peer answered but would not take. A refusal fails
    /// that change alone; only a real disconnect closes the adapter.
    refused_change: Option<String>,
}

// SAFETY: the `!Send` handles inside `peer` are only reached on `owner`:
// every method asserts the current thread is `owner` before touching `peer`,
// and `Drop` releases `peer` only on `owner` (elsewhere it is leaked, never
// dropped), so no non-atomic state inside it is shared across threads.
#[allow(
    unsafe_code,
    clippy::non_send_fields_in_send_ty,
    reason = "the collaboration seam's documented runtime-checked Send wrapper for thread-bound transports"
)]
unsafe impl Send for PeerTransportAdapter {}

impl PeerTransportAdapter {
    pub(super) fn new(peer: impl PeerTransport + 'static) -> Self {
        Self {
            peer: std::mem::ManuallyDrop::new(Box::new(peer)),
            owner: std::thread::current().id(),
            closed: None,
            refused_change: None,
        }
    }

    fn check_owner(&self) {
        assert_eq!(
            std::thread::current().id(),
            self.owner,
            "a thread-bound peer transport was driven off its owner thread"
        );
    }

    /// Admit a command's answer. A peer that answered "no" to one change
    /// keeps the connection: that change is refused, not the peer lost.
    fn admit(
        &mut self,
        change: bool,
        result: Result<(), String>,
    ) -> Result<(), super::TransportError> {
        let Err(error) = result else {
            if change {
                self.refused_change = None;
            }
            return Ok(());
        };
        if annotation::refused(&error) {
            tracing::warn!(%error, "collaboration peer refused a change");
            self.refused_change = Some(error);
            return Ok(());
        }
        tracing::warn!(%error, "collaboration peer transport unavailable");
        self.closed = Some(error);
        Err(super::TransportError::Closed)
    }
}

impl Drop for PeerTransportAdapter {
    fn drop(&mut self) {
        if std::thread::current().id() == self.owner {
            // SAFETY: on the owner thread, and `peer` is never used again.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.peer) };
        } else {
            tracing::error!(
                "a thread-bound peer transport was dropped off its owner thread; leaking it"
            );
        }
    }
}

impl super::CollabTransport for PeerTransportAdapter {
    fn connection_snapshot(
        &self,
        route: &RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        self.check_owner();
        if self.closed.is_some() {
            return None;
        }
        self.peer.connection_snapshot(route)
    }
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
        let change = matches!(
            &command,
            TransportCommand::Primary(CollabRequest::Document { .. })
                | TransportCommand::ToRoute(_, CollabRequest::Document { .. })
        );
        let result = match command {
            TransportCommand::Primary(request) => self.peer.send(request),
            TransportCommand::ToRoute(route, request) => self.peer.send_to_route(route, request),
            TransportCommand::Join(route, record_kind) => self.peer.join_route(route, record_kind),
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
            // A peer that serves no directory still carries the connection's
            // documents: the directory is an offer, not traffic it lost.
            TransportCommand::PublishActionDirectory { app_id } => {
                return self.peer.publish_action_directory(app_id).map_err(|error| {
                    tracing::debug!(%error, "collaboration peer serves no action directory");
                    super::TransportError::Unsupported
                });
            }
        };
        self.admit(change, result)
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.check_owner();
        if self.closed.is_none() {
            out.extend(self.peer.poll());
        }
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        self.check_owner();
        if self.closed.is_none() {
            self.peer.set_wake(wake);
        }
    }

    fn refused_change(&self) -> Option<String> {
        self.check_owner();
        self.refused_change.clone()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CollabTransport, TransportCommand, TransportStatus};

    /// A host peer that answers every document change with whatever its
    /// `answer` says, as the browser's workspace host does.
    struct AnsweringPeer {
        answer: Result<(), String>,
        connected: bool,
    }

    impl PeerTransport for AnsweringPeer {
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn send(&mut self, request: CollabRequest) -> Result<(), String> {
            match request {
                CollabRequest::Document { .. } => self.answer.clone(),
                _ => Ok(()),
            }
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            vec![]
        }
        fn join_route(&mut self, _: RouteSpec, _: crate::record::RecordKind) -> Result<(), String> {
            Ok(())
        }
        fn send_to_route(&mut self, _: RouteSpec, request: CollabRequest) -> Result<(), String> {
            self.send(request)
        }
    }

    fn adapter(answer: Result<(), String>) -> PeerTransportAdapter {
        PeerTransportAdapter::new(AnsweringPeer {
            answer,
            connected: true,
        })
    }

    fn submit_change(adapter: &mut PeerTransportAdapter) -> Result<(), crate::TransportError> {
        adapter.submit(TransportCommand::Primary(
            CollabRequest::document_incremental(b"change".to_vec()),
        ))
    }

    #[test]
    fn a_change_the_peer_refuses_leaves_the_connection_open() {
        let mut adapter = adapter(Err(
            "the workspace refused the change: named no kind for a new record".into(),
        ));
        submit_change(&mut adapter).expect("a refusal is not a queue failure");
        assert!(
            adapter.status().is_open(),
            "the peer answered; the connection is fine"
        );
        assert_eq!(
            adapter.refused_change().as_deref(),
            Some("the workspace refused the change: named no kind for a new record")
        );
    }

    #[test]
    fn a_lost_or_broken_peer_still_closes() {
        let mut adapter = adapter(Err("peer connection dropped mid-change".into()));
        assert_eq!(
            submit_change(&mut adapter),
            Err(crate::TransportError::Closed)
        );
        assert!(matches!(adapter.status(), TransportStatus::Closed(_)));
        assert_eq!(adapter.refused_change(), None);
        assert_eq!(
            submit_change(&mut adapter),
            Err(crate::TransportError::Closed)
        );
    }

    #[test]
    fn the_next_change_that_goes_through_withdraws_the_refusal() {
        struct RefuseThenAccept(Option<String>);
        impl PeerTransport for RefuseThenAccept {
            fn is_connected(&self) -> bool {
                true
            }
            fn send(&mut self, _: CollabRequest) -> Result<(), String> {
                self.0.take().map_or(Ok(()), Err)
            }
            fn poll(&mut self) -> Vec<CollabEvent> {
                vec![]
            }
        }
        let mut adapter = PeerTransportAdapter::new(RefuseThenAccept(Some(
            "the workspace refused the change: named no kind".into(),
        )));
        submit_change(&mut adapter).unwrap();
        assert!(adapter.refused_change().is_some());
        submit_change(&mut adapter).unwrap();
        assert_eq!(
            adapter.refused_change(),
            None,
            "a change that goes through answers for the ones before it"
        );
        assert!(adapter.status().is_open());
    }

    #[test]
    fn a_peer_that_serves_no_directory_keeps_carrying_the_connection() {
        /// A peer with no action directory, counting what it is asked.
        struct NoDirectory(std::rc::Rc<std::cell::Cell<(usize, usize)>>);
        impl PeerTransport for NoDirectory {
            fn is_connected(&self) -> bool {
                true
            }
            fn send(&mut self, request: CollabRequest) -> Result<(), String> {
                if matches!(request, CollabRequest::Document { .. }) {
                    let (changes, offers) = self.0.get();
                    self.0.set((changes + 1, offers));
                }
                Ok(())
            }
            fn poll(&mut self) -> Vec<CollabEvent> {
                vec![]
            }
            fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
                let (changes, offers) = self.0.get();
                self.0.set((changes, offers + 1));
                Err("action directory unavailable".into())
            }
        }
        let asked = std::rc::Rc::default();
        let mut manager = crate::CollaborationManager::with_peer_transport(NoDirectory(
            std::rc::Rc::clone(&asked),
        ));
        assert!(manager.is_connected());
        manager.publish_action_directory("so.tilo.example");
        manager.publish_action_directory("so.tilo.example");
        assert!(manager.is_connected(), "no directory is no lost connection");
        assert!(!manager.action_directory_observation().0);
        assert!(manager.send(CollabRequest::document_incremental(b"change".to_vec())));
        assert_eq!(
            asked.get(),
            (1, 1),
            "the change went through, and the directory was asked for once on the route"
        );
    }
}
