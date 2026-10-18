//! Blocking facade over the typed collaboration client.
//!
//! Production connects through the launcher-inherited `TILO_SVC_COLLAB_FD`
//! capability. Headless diagnostics use the same protocol through an
//! explicit Cap'n Proto endpoint.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager,
    DocumentPublicationError, RouteSpec,
};
use ui::lock::MutexRecover as _;

/// One connection to the broker.
pub struct Link {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
    last_publication: LastPublication,
    /// Correlated requests sent without waiting in line, by request id.
    detached: HashMap<u64, Slot>,
    /// Wakes from the transport when an event arrives, so a receive blocks
    /// instead of polling. `None` for a transport that cannot wake.
    wake: Option<Wake>,
    /// A task mailbox has no reconnect path once its owner is gone.
    task_mailbox: bool,
}

enum LastPublication {
    None,
    Pending,
    Failed(DocumentPublicationError),
}

/// The reply to a request sent with [`Link::send_detached`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detached {
    /// A task cancellation poll; `Ok(true)` is cancelled.
    Cancellation(Result<bool, String>),
}

enum Slot {
    /// Not answered yet. `keep` holds the answer for [`Link::take_detached`];
    /// otherwise it is dropped when it arrives.
    Awaiting {
        keep: bool,
    },
    Arrived(Detached),
}

/// The transport's owner wake, routed to a channel this link blocks on.
struct Wake {
    /// Closes the route when dropped, so it lives as long as the link.
    _scope: ui::wake::WakeScope,
    rx: mpsc::Receiver<()>,
}

impl Wake {
    /// Run `connect` inside a fresh wake scope: the transport it builds
    /// captures that scope and wakes this link whenever an event arrives.
    fn around<T>(connect: impl FnOnce() -> T) -> (T, Self) {
        let mut scope = ui::wake::WakeScope::new();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        // A fresh scope has no destination yet, so binding cannot fail.
        let _ = scope.bind(move |_| {
            let _ = tx.lock_recover().send(());
        });
        let connected = {
            let _entered = scope.enter();
            connect()
        };
        (connected, Self { _scope: scope, rx })
    }
}

fn take_next_polled(
    pending: &mut VecDeque<CollabEvent>,
    polled: Vec<CollabEvent>,
) -> Option<CollabEvent> {
    pending.extend(polled);
    pending.pop_front()
}

/// How long a receive on a transport that cannot wake sleeps between polls.
const RECV_POLL: Duration = Duration::from_millis(1);

impl Link {
    /// Build a test task facade over a transport without wake notifications.
    #[cfg(test)]
    pub(crate) fn for_task(transport: impl ui::collaboration::PeerTransport + 'static) -> Self {
        let mut link =
            Self::with_manager(CollaborationManager::with_peer_transport(transport), None);
        link.task_mailbox = true;
        link
    }

    pub(crate) fn wake_handle(&self) -> Option<ui::wake::WakeHandle> {
        let wake = self.wake.as_ref()?;
        let _entered = wake._scope.enter();
        Some(ui::wake::WakeHandle::new())
    }

    /// Task mailboxes register their wake in this link's private scope.
    pub(crate) fn for_task_waking(
        transport: impl ui::collaboration::PeerTransport + 'static,
        register: impl FnOnce(ui::wake::WakeHandle),
    ) -> Self {
        let (manager, wake) = Wake::around(|| {
            register(ui::wake::WakeHandle::new());
            CollaborationManager::with_peer_transport(transport)
        });
        let mut link = Self::with_manager(manager, Some(wake));
        link.task_mailbox = true;
        link
    }

    /// Wrap a host-authenticated SDK connection without replacing its transport.
    /// Native model requests still pass through that connection's broker custody.
    #[doc(hidden)]
    pub fn from_service(manager: CollaborationManager) -> Self {
        Self::with_manager(manager, None)
    }

    fn with_manager(manager: CollaborationManager, wake: Option<Wake>) -> Self {
        Self {
            manager,
            pending: VecDeque::new(),
            last_publication: LastPublication::None,
            detached: HashMap::new(),
            wake,
            task_mailbox: false,
        }
    }

    /// Connect to the typed service capability inherited from the launcher.
    ///
    /// # Errors
    ///
    /// Returns an error when `TILO_SVC_COLLAB_FD` is missing or the launcher's
    /// inherited capability cannot be turned into a typed collaboration
    /// client (wrong fd type, broker not listening, handshake rejected).
    pub fn connect(
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self> {
        let agent_id = handle.ok_or_else(|| anyhow::anyhow!("agent connection needs an id"))?;
        let fd = if std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some() {
            Some(runtime_portal::agent_transport::claim_managed_agent_fd()?)
        } else {
            None
        };
        let (manager, wake) = Wake::around(|| match fd {
            Some(fd) => {
                CollaborationManager::for_agent_route_fd(fd, app_id, cluster_id, record, agent_id)
            }
            None => CollaborationManager::for_agent_route(app_id, cluster_id, record, agent_id),
        });
        let manager = manager.context("connect through TILO_SVC_COLLAB_FD")?;
        Ok(Self::with_manager(manager, Some(wake)))
    }

    /// Connect to an explicit typed Cap'n Proto socket. This is for headless
    /// diagnostics that run outside the launcher but still exercise the
    /// production protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when the Unix socket at `path` cannot be connected
    /// to, or when the typed collaboration client fails its handshake over
    /// that socket.
    pub fn connect_endpoint(
        path: &Path,
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: Option<CertifiedEndpointIdentity>,
    ) -> Result<Self> {
        let fd = runtime_portal::agent_transport::connect_endpoint(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let agent_id = handle.ok_or_else(|| anyhow::anyhow!("agent connection needs an id"))?;
        let (manager, wake) = Wake::around(|| match identity {
            Some(identity) => CollaborationManager::for_agent_route_fd_with_identity(
                fd, app_id, cluster_id, record, agent_id, identity,
            ),
            None => {
                CollaborationManager::for_agent_route_fd(fd, app_id, cluster_id, record, agent_id)
            }
        });
        let manager = manager.context("start typed collaboration client")?;
        Ok(Self::with_manager(manager, Some(wake)))
    }

    /// Connect a certified human peer to an explicit diagnostic endpoint.
    ///
    /// The agent integration harness hosts real application peers alongside
    /// the agent. Keeping this constructor distinct prevents those app peers
    /// from being misrepresented as delegated agents during admission.
    #[doc(hidden)]
    pub fn connect_person_endpoint(
        path: &Path,
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self> {
        let fd = runtime_portal::agent_transport::connect_endpoint(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let (manager, wake) = Wake::around(|| {
            CollaborationManager::for_route_fd_with_identity(
                fd, app_id, cluster_id, record, handle, identity,
            )
        });
        let manager = manager.context("start typed collaboration client")?;
        Ok(Self::with_manager(manager, Some(wake)))
    }

    /// Enqueue one request and record an ordered receipt for document publications.
    ///
    /// # Errors
    ///
    /// Returns an error if the request or its document completion marker cannot enqueue.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        let queued = self.manager.send(req.clone());
        self.record_enqueue(req, queued)
    }

    /// Subscribe to another typed route. This is used for cluster-wide
    /// discovery; mutations go through [`Self::send`] on the primary route.
    pub fn join_route(&mut self, route: RouteSpec) {
        self.manager.join_route(route);
    }

    fn record_enqueue(&mut self, req: &CollabRequest, queued: bool) -> Result<()> {
        let document = matches!(
            req,
            CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
        );
        if !queued {
            // Healthy presence shedding is an explicit transport contract.
            if matches!(req, CollabRequest::Presence { .. }) && self.manager.is_connected() {
                return Ok(());
            }
            let error =
                DocumentPublicationError::Failed("collaboration request enqueue rejected".into());
            if document {
                self.last_publication = LastPublication::Failed(error.clone());
            }
            return Err(error.into());
        }
        if document {
            if let LastPublication::Failed(error) = &self.last_publication {
                return Err(error.clone().into());
            }
            match self.manager.begin_document_publication_barrier() {
                Ok(_) => self.last_publication = LastPublication::Pending,
                Err(error) => {
                    // Invalidate the older prefix even if the caller ignores this error.
                    self.last_publication = LastPublication::Failed(error.clone());
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    /// Read the next event, waiting at most `timeout`. `Ok(None)` on
    /// timeout; `Err` once the broker has gone away.
    ///
    /// # Errors
    ///
    /// Reserved for a future fallible transport; currently always succeeds
    /// (a dead broker currently surfaces as an indefinite empty poll, not an
    /// `Err`).
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        let deadline = ui::time::now() + timeout;
        loop {
            self.clear_wakes();
            let polled = self.poll_intercepted();
            if let Some(event) = take_next_polled(&mut self.pending, polled) {
                return Ok(Some(event));
            }
            if self.task_mailbox && !self.manager.is_connected() {
                anyhow::bail!("task connection owner stopped");
            }
            let now = ui::time::now();
            if now >= deadline {
                return Ok(None);
            }
            self.wait_for_wake(deadline - now);
        }
    }

    /// Drop wakes already delivered: the poll that follows sees their events,
    /// so only a wake raised after it means something new arrived.
    fn clear_wakes(&self) {
        if let Some(wake) = &self.wake {
            while wake.rx.try_recv().is_ok() {}
        }
    }

    /// Block until the transport has news or `most` passes. A transport that
    /// cannot wake is polled: a correlated reply is on the critical path of
    /// every step, so it is noticed within ~1 ms, not 5.
    fn wait_for_wake(&self, most: Duration) {
        match &self.wake {
            Some(wake) => {
                let _ = wake.rx.recv_timeout(most);
            }
            None => std::thread::sleep(most.min(RECV_POLL)),
        }
    }

    /// Everything the transport delivered since the last poll, with the
    /// replies to detached requests taken out.
    fn poll_intercepted(&mut self) -> Vec<CollabEvent> {
        let polled = self.manager.poll();
        polled
            .into_iter()
            .filter_map(|event| self.intercept(event))
            .collect()
    }

    /// Move whatever the transport delivered into the pending queue without
    /// waiting, settling detached replies on the way.
    fn pump(&mut self) {
        self.clear_wakes();
        let polled = self.poll_intercepted();
        self.pending.extend(polled);
    }

    /// Take `event` if it answers a detached request; otherwise hand it back.
    fn intercept(&mut self, event: CollabEvent) -> Option<CollabEvent> {
        let (request_id, reply) = match event {
            CollabEvent::TaskRunCancellation { request_id, result }
                if self.detached.contains_key(&request_id) =>
            {
                (request_id, Detached::Cancellation(result))
            }
            other => return Some(other),
        };
        match self.detached.remove(&request_id) {
            Some(Slot::Awaiting { keep: true }) => {
                self.detached.insert(request_id, Slot::Arrived(reply));
            }
            Some(Slot::Awaiting { keep: false }) | None => {}
            // A second answer to one request id: the first one stands.
            Some(arrived @ Slot::Arrived(_)) => {
                self.detached.insert(request_id, arrived);
            }
        }
        None
    }

    /// Send a correlated request without waiting for its answer: the answer is
    /// taken off the event stream wherever it arrives, so no wait loop has to
    /// know about it. The transport dispatches requests in the order they are
    /// sent, so a later request still reaches the broker after this one.
    ///
    /// A cancellation poll's answer is kept for [`Self::take_detached`].
    ///
    /// # Errors
    ///
    /// When `request` has no correlated answer to detach, or cannot enqueue.
    pub fn send_detached(&mut self, request: &CollabRequest) -> Result<()> {
        let CollabRequest::TaskRunCancellation { request_id, .. } = request else {
            anyhow::bail!("only cancellation polls are detached");
        };
        let request_id = *request_id;
        self.detached
            .insert(request_id, Slot::Awaiting { keep: true });
        if let Err(error) = self.send(request) {
            self.detached.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    /// The answer to the detached request `request_id`, once it has arrived.
    pub fn take_detached(&mut self, request_id: u64) -> Option<Detached> {
        self.pump();
        match self.detached.remove(&request_id)? {
            Slot::Arrived(reply) => Some(reply),
            awaiting @ Slot::Awaiting { .. } => {
                self.detached.insert(request_id, awaiting);
                None
            }
        }
    }

    /// Wait until `deadline` for the answer to the detached request
    /// `request_id`. Every other event stays queued, in order, for the
    /// session loop. `None` when the deadline passes first or nothing with
    /// that id is outstanding.
    pub fn wait_detached(
        &mut self,
        request_id: u64,
        deadline: ui::time::Instant,
    ) -> Option<Detached> {
        loop {
            if let Some(reply) = self.take_detached(request_id) {
                return Some(reply);
            }
            if !matches!(self.detached.get(&request_id), Some(Slot::Awaiting { .. }))
                || (self.task_mailbox && !self.manager.is_connected())
            {
                return None;
            }
            let now = ui::time::now();
            if now >= deadline {
                return None;
            }
            self.wait_for_wake(deadline - now);
        }
    }

    /// Stop holding the answer to `request_id`: it is dropped when it arrives.
    pub fn forget(&mut self, request_id: u64) {
        match self.detached.get_mut(&request_id) {
            Some(Slot::Awaiting { keep }) => *keep = false,
            Some(Slot::Arrived(_)) => {
                self.detached.remove(&request_id);
            }
            None => {}
        }
    }

    /// Take every queued event `wanted` picks out, without waiting; the rest
    /// keep their order.
    pub fn take_queued(&mut self, wanted: impl Fn(&CollabEvent) -> bool) -> Vec<CollabEvent> {
        self.pump();
        let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(wanted);
        self.pending = kept.into();
        taken
    }

    /// Put events consumed while waiting for a correlated broker reply back
    /// in front of the normal session loop, preserving their order.
    pub fn defer(&mut self, events: Vec<CollabEvent>) {
        for event in events.into_iter().rev() {
            self.pending.push_front(event);
        }
    }
}

#[cfg(test)]
pub(crate) fn test_link(transport: impl ui::collaboration::PeerTransport + 'static) -> Link {
    Link::with_manager(CollaborationManager::with_peer_transport(transport), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AcceptedPeer;

    impl ui::collaboration::PeerTransport for AcceptedPeer {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> std::result::Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }
    }

    fn peer_link() -> Link {
        Link::with_manager(
            CollaborationManager::with_peer_transport(AcceptedPeer),
            None,
        )
    }

    #[test]
    fn accepted_document_without_completion_support_is_sticky_failure() {
        let mut link = peer_link();
        assert!(
            link.send(&CollabRequest::document_incremental(vec![1]))
                .is_err()
        );
        assert!(
            link.send(&CollabRequest::document_incremental(vec![2]))
                .is_err()
        );
    }

    #[test]
    fn typed_poll_batches_keep_every_event_in_order() {
        let mut pending = VecDeque::new();
        let first = take_next_polled(
            &mut pending,
            vec![
                CollabEvent::RemoteCursorMoved {
                    user_id: "one".into(),
                    x: 1.0,
                    y: 0.0,
                },
                CollabEvent::RemoteCursorMoved {
                    user_id: "two".into(),
                    x: 2.0,
                    y: 0.0,
                },
                CollabEvent::RemoteCursorMoved {
                    user_id: "three".into(),
                    x: 3.0,
                    y: 0.0,
                },
            ],
        );

        assert!(matches!(
            first,
            Some(CollabEvent::RemoteCursorMoved { x: 1.0, .. })
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::RemoteCursorMoved { x: 2.0, .. })
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::RemoteCursorMoved { x: 3.0, .. })
        ));
        assert!(take_next_polled(&mut pending, Vec::new()).is_none());
    }

    /// Answers every cancellation poll at once, each followed by
    /// an ordinary room event, so detached answers arrive interleaved with
    /// the traffic the session loop reads.
    struct AnsweringHost {
        events: Vec<CollabEvent>,
    }

    impl ui::collaboration::PeerTransport for AnsweringHost {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, request: CollabRequest) -> std::result::Result<(), String> {
            let CollabRequest::TaskRunCancellation { request_id, .. } = request else {
                return Ok(());
            };
            let reply = CollabEvent::TaskRunCancellation {
                request_id,
                result: Ok(request_id != 2),
            };
            self.events.push(reply);
            self.events.push(CollabEvent::RemoteCursorMoved {
                user_id: format!("after-{request_id}"),
                x: 0.0,
                y: 0.0,
            });
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            std::mem::take(&mut self.events)
        }
    }

    fn poll(request_id: u64) -> CollabRequest {
        CollabRequest::TaskRunCancellation {
            request_id,
            run_id: 7,
            lease: "lease".into(),
        }
    }

    /// Detached answers never reach the session loop: each waits for its
    /// owner — and every other event keeps its order, including an answer
    /// nobody detached (an in-line wait still sees it).
    #[test]
    fn detached_answers_leave_the_event_stream_without_reordering_it() {
        let mut link = test_link(AnsweringHost { events: Vec::new() });
        link.send_detached(&poll(1)).unwrap();
        link.send_detached(&poll(2)).unwrap();
        // Sent in line: its answer is the caller's to read.
        link.send(&poll(3)).unwrap();
        assert!(
            link.send_detached(&CollabRequest::RequestDocumentSnapshot)
                .is_err(),
            "only requests with a correlated answer can be detached"
        );

        let mut seen = Vec::new();
        while let Some(event) = link.recv(Duration::ZERO).unwrap() {
            seen.push(match event {
                CollabEvent::RemoteCursorMoved { user_id, .. } => user_id,
                CollabEvent::TaskRunCancellation { request_id, .. } => {
                    format!("reply-{request_id}")
                }
                other => panic!("unexpected event {other:?}"),
            });
        }
        assert_eq!(
            seen,
            ["after-1", "after-2", "reply-3", "after-3"],
            "detached answers are taken out; the rest keep their order"
        );
        assert_eq!(
            link.take_detached(1),
            Some(Detached::Cancellation(Ok(true)))
        );
        assert_eq!(
            link.take_detached(2),
            Some(Detached::Cancellation(Ok(false)))
        );
        assert_eq!(link.take_detached(1), None, "an answer is taken once");
    }

    /// A forgotten poll's answer is dropped when it arrives, and waiting for
    /// an answer that is already in returns at once.
    #[test]
    fn a_forgotten_poll_is_dropped_and_an_arrived_one_needs_no_wait() {
        let mut link = test_link(AnsweringHost { events: Vec::new() });
        link.send_detached(&poll(1)).unwrap();
        link.forget(1);
        link.send_detached(&poll(3)).unwrap();
        let began = ui::time::now();
        assert_eq!(
            link.wait_detached(3, began + Duration::from_secs(5)),
            Some(Detached::Cancellation(Ok(true)))
        );
        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(link.take_detached(1), None);
        assert_eq!(
            link.wait_detached(1, ui::time::now()),
            None,
            "nothing is outstanding under a forgotten id"
        );
    }
}
