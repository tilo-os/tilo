//! An in-memory [`CollabTransport`] for tests: no threads, no sockets.
//!
//! [`LoopbackTransport::new`] returns the transport together with a
//! [`LoopbackPeer`], the test's view of the remote end. The peer inspects
//! what the manager submitted, delivers events, acknowledges primary routes
//! and changes the transport's status. Command capacity is bounded so tests
//! can exercise the fail-closed contract: presence beyond capacity is shed,
//! any other command beyond it closes the transport.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollabTransport, RouteSpec,
    TransportCommand, TransportError, TransportStatus,
};

struct Shared {
    status: TransportStatus,
    capacity: usize,
    submitted: Vec<TransportCommand>,
    events: VecDeque<CollabEvent>,
    primary_results: VecDeque<Result<RouteSpec, String>>,
    action_directory: VecDeque<ActionDirectoryRequest>,
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

/// The transport half, handed to
/// [`CollaborationManager::with_transport`](super::CollaborationManager::with_transport).
pub struct LoopbackTransport {
    shared: Arc<Mutex<Shared>>,
}

/// The remote half, kept by the test.
#[derive(Clone)]
pub struct LoopbackPeer {
    shared: Arc<Mutex<Shared>>,
}

impl LoopbackTransport {
    /// A ready transport admitting any number of commands.
    pub fn new() -> (Self, LoopbackPeer) {
        Self::with_capacity(usize::MAX)
    }

    /// A ready transport that retains at most `capacity` unread commands.
    pub fn with_capacity(capacity: usize) -> (Self, LoopbackPeer) {
        let shared = Arc::new(Mutex::new(Shared {
            status: TransportStatus::Ready,
            capacity,
            submitted: Vec::new(),
            events: VecDeque::new(),
            primary_results: VecDeque::new(),
            action_directory: VecDeque::new(),
            wake: None,
        }));
        (
            Self {
                shared: shared.clone(),
            },
            LoopbackPeer { shared },
        )
    }
}

impl LoopbackPeer {
    /// Take every command submitted since the last call, freeing capacity.
    pub fn take_submitted(&self) -> Vec<TransportCommand> {
        std::mem::take(&mut lock(&self.shared).submitted)
    }

    /// Queue an event for the manager's next poll and wake its owner.
    pub fn deliver(&self, event: CollabEvent) {
        lock(&self.shared).events.push_back(event);
        self.wake();
    }

    /// Report the remote end's decision about the latest primary `Hello`.
    pub fn acknowledge_primary(&self, result: Result<RouteSpec, String>) {
        lock(&self.shared).primary_results.push_back(result);
        self.wake();
    }

    /// Issue an action-directory query to the owner thread.
    pub fn request_action_directory(&self, request: ActionDirectoryRequest) {
        lock(&self.shared).action_directory.push_back(request);
        self.wake();
    }

    /// Change what the transport reports as its status.
    pub fn set_status(&self, status: TransportStatus) {
        lock(&self.shared).status = status;
    }

    fn wake(&self) {
        if let Some(wake) = lock(&self.shared).wake.as_ref() {
            wake();
        }
    }
}

/// A test double has no invariant a panicking test could break mid-update,
/// so a poisoned lock is still usable.
fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CollabTransport for LoopbackTransport {
    fn status(&self) -> TransportStatus {
        lock(&self.shared).status.clone()
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        let mut shared = lock(&self.shared);
        if !shared.status.is_open() {
            return Err(TransportError::Closed);
        }
        if shared.submitted.len() >= shared.capacity {
            let transient = matches!(
                &command,
                TransportCommand::Primary(CollabRequest::Presence { .. })
                    | TransportCommand::ToRoute(_, CollabRequest::Presence { .. })
            );
            if transient {
                return Err(TransportError::Shed);
            }
            shared.status = TransportStatus::Closed("loopback command capacity exceeded".into());
            return Err(TransportError::Closed);
        }
        if let TransportCommand::InstallDocumentEncryption { reply, .. } = &command {
            let _ = reply.send(Ok(()));
        }
        shared.submitted.push(command);
        Ok(())
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        out.extend(lock(&self.shared).events.drain(..));
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        lock(&self.shared).primary_results.pop_front()
    }

    fn take_action_directory_requests(&mut self, out: &mut Vec<ActionDirectoryRequest>) {
        out.extend(lock(&self.shared).action_directory.drain(..));
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        lock(&self.shared).wake = Some(wake);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::collaboration::{
        CollaborationManager, DocumentPublicationError, PresencePayload, TransportCommand,
    };
    use crate::presence::PresenceKey;

    fn hello(route: &RouteSpec) -> CollabRequest {
        CollabRequest::Hello {
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id,
            record: route.record.clone(),
            record_kind: None,
            handle: None,
        }
    }

    fn presence() -> CollabRequest {
        CollabRequest::Presence {
            key: PresenceKey::Tab { id: "tab".into() },
            payload: PresencePayload::Pointer { x: 1.0, y: 2.0 },
        }
    }

    #[test]
    fn loopback_connection_follows_transport_and_acknowledged_hello() {
        // Case `connection_follows_the_transport_status`.
        {
            let (transport, peer) = LoopbackTransport::new();
            let manager = CollaborationManager::with_transport(Box::new(transport));
            assert!(manager.is_connected());
            peer.set_status(TransportStatus::Connecting);
            assert!(!manager.is_connected());
            peer.set_status(TransportStatus::Degraded("slow".into()));
            assert!(manager.is_connected());
            peer.set_status(TransportStatus::Closed("gone".into()));
            assert!(!manager.is_connected());
        }

        // Case `primary_route_requires_the_remote_acknowledgement_of_the_latest_hello`.
        {
            let (transport, peer) = LoopbackTransport::new();
            let mut manager = CollaborationManager::with_transport(Box::new(transport));
            let a = RouteSpec::record("so.tilo.notes", uuid::Uuid::new_v4(), "a");
            let b = RouteSpec::record("so.tilo.notes", a.cluster_id, "b");

            assert!(manager.send(hello(&a)));
            assert!(matches!(
                peer.take_submitted().as_slice(),
                [TransportCommand::Primary(CollabRequest::Hello { .. })]
            ));
            let _ = manager.poll();
            assert!(
                manager.primary_route().is_none(),
                "submitting a Hello is not an acknowledgement"
            );

            peer.acknowledge_primary(Ok(a.clone()));
            let _ = manager.poll();
            assert_eq!(manager.primary_route().map(|w| &w.route), Some(&a));

            assert!(manager.send(hello(&b)));
            assert!(manager.primary_route().is_none());
            peer.acknowledge_primary(Ok(a));
            let _ = manager.poll();
            assert!(
                manager.primary_route().is_none(),
                "an acknowledgement of an older Hello is refused"
            );
            peer.acknowledge_primary(Err("denied".into()));
            let _ = manager.poll();
            assert!(manager.primary_route().is_none());
            peer.acknowledge_primary(Ok(b.clone()));
            let _ = manager.poll();
            assert_eq!(manager.primary_route().map(|w| &w.route), Some(&b));

            peer.set_status(TransportStatus::Closed("gone".into()));
            assert!(manager.primary_route().is_none());
        }

        // Case `routes_are_forwarded_and_hello_never_targets_an_extra_route`.
        {
            let (transport, peer) = LoopbackTransport::new();
            let mut manager = CollaborationManager::with_transport(Box::new(transport));
            let extra = RouteSpec::cluster(uuid::Uuid::new_v4());
            manager.join_route(extra.clone());
            manager.send_to_route(extra.clone(), hello(&extra));
            manager.send_to_route(extra.clone(), CollabRequest::RequestDocumentSnapshot);
            let submitted = peer.take_submitted();
            assert!(matches!(
                submitted.as_slice(),
                [
                    TransportCommand::Join(joined),
                    TransportCommand::ToRoute(target, CollabRequest::RequestDocumentSnapshot),
                ] if joined == &extra && target == &extra
            ));
        }
    }

    #[test]
    fn loopback_delivery_sheds_presence_and_publishes_directories_once() {
        // Case `presence_is_shed_and_durable_overflow_disconnects`.
        {
            let (transport, peer) = LoopbackTransport::with_capacity(1);
            let mut manager = CollaborationManager::with_transport(Box::new(transport));
            assert!(manager.send(CollabRequest::document_incremental(b"one".to_vec())));
            assert!(!manager.send(presence()), "presence may be shed");
            assert!(
                manager.is_connected(),
                "shedding presence keeps the transport"
            );
            assert!(
                !manager.send(CollabRequest::document_incremental(b"two".to_vec())),
                "durable overflow is refused"
            );
            assert!(!manager.is_connected(), "and visibly disconnects");
            assert_eq!(peer.take_submitted().len(), 1);
            assert!(!manager.send(CollabRequest::document_incremental(b"three".to_vec())));
        }

        // Case `events_and_directory_requests_reach_the_owner_and_wake_it`.
        {
            let mut scope = crate::wake::WakeScope::new();
            let wakes = Arc::new(AtomicUsize::new(0));
            let count = wakes.clone();
            scope
                .bind(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
            let _entry = scope.enter();
            let (transport, peer) = LoopbackTransport::new();
            let mut manager = CollaborationManager::with_transport(Box::new(transport));

            peer.deliver(CollabEvent::FollowChanged { peer_id: None });
            assert!(wakes.load(Ordering::SeqCst) >= 1);
            let events = manager.poll();
            assert!(matches!(
                events.as_slice(),
                [CollabEvent::FollowChanged { peer_id: None }]
            ));
            assert!(manager.poll().is_empty());

            let (reply, _answer) = tokio::sync::oneshot::channel();
            peer.request_action_directory(ActionDirectoryRequest::Resolve {
                revision: "r".into(),
                resource: "doc".into(),
                action: "open".into(),
                reply,
            });
            assert_eq!(manager.take_action_directory_requests().len(), 1);
            assert!(manager.take_action_directory_requests().is_empty());
        }

        // Case `action_directory_is_published_once_per_primary_route`.
        {
            let (transport, peer) = LoopbackTransport::new();
            let mut manager = CollaborationManager::with_transport(Box::new(transport));
            let route = RouteSpec::app("so.tilo.notes", uuid::Uuid::new_v4());
            manager.publish_action_directory("so.tilo.notes");
            manager.publish_action_directory("so.tilo.notes");
            assert!(manager.send(hello(&route)));
            manager.publish_action_directory("so.tilo.notes");
            let publications = peer
                .take_submitted()
                .into_iter()
                .filter(|command| {
                    matches!(command, TransportCommand::PublishActionDirectory { .. })
                })
                .count();
            assert_eq!(publications, 2, "a new Hello re-registers the directory");
        }

        // Case `publication_barrier_reports_unsupported_or_unavailable`.
        {
            let (transport, _peer) = LoopbackTransport::new();
            let mut manager = CollaborationManager::with_transport(Box::new(transport));
            assert!(matches!(
                manager.begin_document_publication_barrier(),
                Err(DocumentPublicationError::Unsupported)
            ));
            assert!(matches!(
                CollaborationManager::disabled().begin_document_publication_barrier(),
                Err(DocumentPublicationError::Unavailable)
            ));
        }
    }
}
