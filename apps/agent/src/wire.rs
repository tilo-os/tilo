//! Blocking facade over the typed collaboration client.
//!
//! Production connects through the launcher-inherited `TILO_SVC_COLLAB_FD`
//! capability. Headless diagnostics use the same protocol through an
//! explicit endpoint.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{
    CatalogueChange, CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager,
    DocumentPublicationError, RouteSpec,
};
use ui::lock::MutexRecover as _;
use ui::resource::ResourceType;

/// One connection to the broker.
pub struct Link {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
    last_publication: LastPublication,
    /// The invocations the broker asked this agent to stop, by their handle: a turn the person
    /// ended reads as stopped wherever its answer is, without a wait of its own.
    cancelled: HashSet<u64>,
    /// Wakes from the transport when an event arrives, so a receive blocks
    /// instead of polling. `None` for a transport that cannot wake.
    wake: Option<Wake>,
    /// Waits the link has blocked in, for a test that proves it sleeps.
    #[cfg(test)]
    waits: std::cell::Cell<u64>,
}

enum LastPublication {
    None,
    Pending,
    Failed(DocumentPublicationError),
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
    fn with_manager(manager: CollaborationManager, wake: Option<Wake>) -> Self {
        Self {
            manager,
            pending: VecDeque::new(),
            last_publication: LastPublication::None,
            cancelled: HashSet::new(),
            wake,
            #[cfg(test)]
            waits: std::cell::Cell::new(0),
        }
    }

    /// Connect to the typed service capability inherited from the launcher.
    ///
    /// # Errors
    ///
    /// Returns an error when `TILO_SVC_COLLAB_FD` is missing or the launcher's
    /// inherited capability cannot be turned into a typed collaboration
    /// client (wrong fd type, broker not listening, handshake rejected).
    ///
    /// With `identity` (a task agent's: its task delegation and task key) the
    /// join presents it instead of the launcher's identity service.
    pub fn connect(
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: Option<CertifiedEndpointIdentity>,
    ) -> Result<Self> {
        let agent_id = handle.ok_or_else(|| anyhow::anyhow!("agent connection needs an id"))?;
        let fd = if std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some() {
            Some(runtime_portal::agent_transport::claim_managed_agent_fd()?)
        } else {
            None
        };
        let (manager, wake) = Wake::around(|| match (fd, identity) {
            (Some(fd), Some(identity)) => CollaborationManager::for_agent_route_fd_with_identity(
                fd, app_id, cluster_id, record, agent_id, identity,
            ),
            (Some(fd), None) => {
                CollaborationManager::for_agent_route_fd(fd, app_id, cluster_id, record, agent_id)
            }
            (None, Some(identity)) => CollaborationManager::for_agent_route_with_identity(
                app_id, cluster_id, record, agent_id, identity,
            ),
            (None, None) => {
                CollaborationManager::for_agent_route(app_id, cluster_id, record, agent_id)
            }
        });
        let manager = manager.context("connect through TILO_SVC_COLLAB_FD")?;
        Ok(Self::with_manager(manager, Some(wake)))
    }

    /// Connect to an explicit typed collaboration socket. This is for headless
    /// diagnostics that run outside the launcher but still exercise the
    /// production protocol.
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
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        let queued = self.manager.send(req.clone());
        self.record_enqueue(req, queued)
    }

    /// Subscribe to another typed route. This is used for cluster-wide
    /// discovery; mutations go through [`Self::send`] on the primary route.
    /// A cluster route holds no resource, so it names no kind: only a route
    /// whose resource a host may have to create names what it holds.
    pub fn join_route(&mut self, route: RouteSpec) {
        self.manager.join_route(route, ResourceType::new(""));
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
    /// Why the broker connection ended, once it has: a closed transport
    /// never reopens, so nothing more will arrive on this link.
    #[must_use]
    pub fn closed(&self) -> Option<String> {
        match self.manager.state() {
            ui::collaboration::CollaborationState::Offline(reason) => Some(reason),
            _ => None,
        }
    }

    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        let deadline = ui::time::now() + timeout;
        loop {
            self.clear_wakes();
            let polled = self.poll_intercepted();
            if let Some(event) = take_next_polled(&mut self.pending, polled) {
                return Ok(Some(event));
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
        #[cfg(test)]
        self.waits.set(self.waits.get().saturating_add(1));
        match &self.wake {
            Some(wake) => {
                let _ = wake.rx.recv_timeout(most);
            }
            None => std::thread::sleep(most.min(RECV_POLL)),
        }
    }

    /// Everything the transport delivered since the last poll, with what the broker asked to
    /// stop noted.
    fn poll_intercepted(&mut self) -> Vec<CollabEvent> {
        let polled = self.manager.poll();
        polled
            .into_iter()
            .inspect(|event| self.note(event))
            .collect()
    }

    /// Move whatever the transport delivered into the pending queue without
    /// waiting, noting what the broker asked to stop on the way.
    fn pump(&mut self) {
        self.clear_wakes();
        let polled = self.poll_intercepted();
        self.pending.extend(polled);
    }

    /// Note `event` when it asks this agent to stop an invocation: the session loop still reads
    /// it.
    fn note(&mut self, event: &CollabEvent) {
        if let CollabEvent::InvocationCancellation { id } = event {
            self.cancelled.insert(*id);
        }
    }

    /// Whether the broker asked this agent to stop the invocation it handed it as `id`, by
    /// everything the transport delivered by now.
    pub fn cancelled(&mut self, id: u64) -> bool {
        self.pump();
        self.cancelled.contains(&id)
    }

    /// Stop remembering that the broker asked to stop `id`: the invocation is over.
    pub fn forget_cancelled(&mut self, id: u64) {
        self.cancelled.remove(&id);
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

    /// Wait, at most to `deadline`, for the broker's next catalogue republication: on
    /// the transport's own wake, never on a timer. No other event ends the wait, and
    /// each one keeps its place for the session loop. `None` when the deadline passes
    /// first.
    pub fn next_catalogue(&mut self, deadline: ui::time::Instant) -> Option<CatalogueChange> {
        loop {
            if let Some(change) = self.take_catalogue() {
                return Some(change);
            }
            let now = ui::time::now();
            if now >= deadline {
                return None;
            }
            self.wait_for_wake(deadline - now);
        }
    }

    /// Waits this link has blocked in, as a test counts them.
    #[cfg(test)]
    pub(crate) fn waits(&self) -> u64 {
        self.waits.get()
    }

    /// The next catalogue republication already queued, without waiting, with every
    /// other event left in order.
    pub(crate) fn take_catalogue(&mut self) -> Option<CatalogueChange> {
        let mut taken =
            self.take_queued(|event| matches!(event, CollabEvent::ActionCatalogue { .. }));
        if taken.len() > 1 {
            self.defer(taken.split_off(1));
        }
        match taken.pop() {
            Some(CollabEvent::ActionCatalogue { change }) => Some(change),
            _ => None,
        }
    }

    /// Put events consumed while waiting for a correlated broker reply back
    /// in front of the normal session loop, preserving their order.
    pub fn defer(&mut self, events: Vec<CollabEvent>) {
        for event in events.into_iter().rev() {
            self.pending.push_front(event);
        }
    }
}

/// A test transport's wake: rung when the transport has more to deliver, so a link
/// waiting on it sleeps between deliveries instead of asking on a timer.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestWake(mpsc::Sender<()>);

#[cfg(test)]
impl TestWake {
    /// Tell the link something is waiting: its next receive polls the transport.
    pub(crate) fn ring(&self) {
        let _ = self.0.send(());
    }
}

/// A test transport's wake and the receive end a test link waits on. Keep the
/// wake for as long as the link: a dropped one leaves the link nothing to wait on.
#[cfg(test)]
pub(crate) fn test_wake() -> (TestWake, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    (TestWake(tx), rx)
}

#[cfg(test)]
pub(crate) fn test_link(
    transport: impl ui::collaboration::PeerTransport + 'static,
    wake: mpsc::Receiver<()>,
) -> Link {
    Link::with_manager(
        CollaborationManager::with_peer_transport(transport),
        Some(Wake {
            _scope: ui::wake::WakeScope::new(),
            rx: wake,
        }),
    )
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

    /// Hands over `events` on its first poll, then nothing.
    struct Courier {
        events: Vec<CollabEvent>,
    }

    impl ui::collaboration::PeerTransport for Courier {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> std::result::Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            std::mem::take(&mut self.events)
        }
    }

    tilo_test_env::scenario! {
        fn the_link_keeps_polled_events_in_order;

        phase typed_poll_batches_keep_every_event_in_order;
        {
            let mut pending = VecDeque::new();
            let first = take_next_polled(
                &mut pending,
                vec![
                    CollabEvent::RemoteCursorMoved {
                        member: ui::permissions::MemberId::person("one"),
                        x: 1.0,
                        y: 0.0,
                    },
                    CollabEvent::RemoteCursorMoved {
                        member: ui::permissions::MemberId::person("two"),
                        x: 2.0,
                        y: 0.0,
                    },
                    CollabEvent::RemoteCursorMoved {
                        member: ui::permissions::MemberId::person("three"),
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

        phase a_stop_is_noted_and_still_reaches_the_session_loop;
        // A stop the broker asks for is known at once wherever the agent looks, and the session
        // loop still reads it, so whatever serves the invocation stops it too.
        {
            let (_wake, rx) = test_wake();
            let mut link = test_link(
                Courier {
                    events: vec![CollabEvent::InvocationCancellation { id: 7 }],
                },
                rx,
            );
            assert!(!link.cancelled(8));
            assert!(link.cancelled(7));
            assert!(matches!(
                link.recv(Duration::ZERO).unwrap(),
                Some(CollabEvent::InvocationCancellation { id: 7 })
            ));
            link.forget_cancelled(7);
            assert!(!link.cancelled(7));
        }

        phase a_catalogue_wait_takes_the_republication_and_keeps_every_other_event;
        // A wait for the catalogue is ended by a republication, and only by one: a
        // republication behind another event still ends it, and that event keeps
        // its place for the session loop.
        {
            let change = CatalogueChange::snapshot(&[]);
            let (_wake, rx) = test_wake();
            let mut link = test_link(
                Courier {
                    events: vec![
                        CollabEvent::RemoteCursorMoved {
                            member: ui::permissions::MemberId::person("one"),
                            x: 1.0,
                            y: 0.0,
                        },
                        CollabEvent::ActionCatalogue {
                            change: change.clone(),
                        },
                    ],
                },
                rx,
            );
            assert_eq!(
                link.next_catalogue(ui::time::now()),
                Some(change),
                "the republication ends the wait"
            );
            assert!(
                matches!(
                    link.recv(Duration::ZERO).unwrap(),
                    Some(CollabEvent::RemoteCursorMoved { member: user_id, .. }) if user_id == ui::permissions::MemberId::person("one")
                ),
                "the other event keeps its place for the session loop"
            );
            assert!(link.recv(Duration::ZERO).unwrap().is_none());
        }
    }
}
