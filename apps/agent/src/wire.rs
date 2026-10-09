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
    DocumentPublicationError, DocumentPublicationReceipt, RouteSpec,
};

/// One connection to the broker.
pub struct Link {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
    fresh: VecDeque<CollabEvent>,
    last_publication: LastPublication,
    /// Correlated requests sent without waiting in line, by request id.
    detached: HashMap<u64, Slot>,
    /// Refusals of detached reports, oldest first, until the session reads them.
    refused: Vec<String>,
    /// Wakes from the transport when an event arrives, so a receive blocks
    /// instead of polling. `None` for a transport that cannot wake.
    wake: Option<Wake>,
}

enum LastPublication {
    None,
    Pending(DocumentPublicationReceipt),
    Failed(DocumentPublicationError),
}

/// The broker's bounded answer to one managed Message RPC.
///
/// `Uncertain` means the request may have reached the owner, so a caller must
/// retain the original invocation and must not manufacture replacement work.
/// The current RPC's generic error is also uncertain; it is not an
/// authoritative non-effect receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentMessageRpcOutcome {
    Accepted,
    Uncertain,
    Disconnected(String),
}

/// The reply to a request sent with [`Link::send_detached`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detached {
    /// An enrolled task report; `Err` is the host's refusal.
    Report(Result<(), String>),
    /// A task cancellation poll; `Ok(true)` is cancelled.
    Cancellation(Result<bool, String>),
    /// The complete task control frontier, including pending directions.
    Control(Result<runtime_collab::TaskRunControl, String>),
}

enum Slot {
    /// Not answered yet. `keep` holds the answer for [`Link::take_detached`];
    /// otherwise an accepted report is dropped and a refused one recorded.
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
            let _ = tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(());
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
            fresh: VecDeque::new(),
            last_publication: LastPublication::None,
            detached: HashMap::new(),
            refused: Vec::new(),
            wake,
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

    /// Wait until every document mutation sent so far has been handed to the
    /// broker. [`Self::send`] only queues: dropping the link with a mutation
    /// still queued discards it, so a session that is about to end calls this
    /// first.
    ///
    /// # Errors
    ///
    /// When one of those mutations was not published, or the transport
    /// stopped before answering.
    pub fn flush_documents(&mut self) -> Result<()> {
        self.manager
            .begin_document_publication_barrier()?
            .wait(None)?;
        Ok(())
    }

    /// Subscribe to another typed route. This is used for cluster-wide
    /// discovery; mutations still require [`Self::send_to_route`].
    pub fn join_route(&mut self, route: RouteSpec) {
        self.manager.join_route(route);
    }

    /// Publish on one explicit non-primary route. Naming the route at every
    /// call keeps a discovery subscription from becoming a mutation target.
    ///
    /// # Errors
    ///
    /// Returns an error if the request or its document completion marker cannot enqueue.
    pub fn send_to_route(&mut self, route: RouteSpec, req: &CollabRequest) -> Result<()> {
        let queued = self.manager.try_send_to_route(route, req.clone()).is_ok();
        self.record_enqueue(req, queued)
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
                Ok(receipt) => self.last_publication = LastPublication::Pending(receipt),
                Err(error) => {
                    // Invalidate the older prefix even if the caller ignores this error.
                    self.last_publication = LastPublication::Failed(error.clone());
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    /// Complete the latest document prefix while retaining the inbound receiver.
    /// Events polled around the wait remain available to the normal session loop.
    ///
    /// # Errors
    /// Returns a publication/transport error or exhaustion of the supplied deadline.
    pub fn complete_publications(&mut self, deadline: Option<ui::time::Instant>) -> Result<()> {
        let publication = std::mem::replace(&mut self.last_publication, LastPublication::None);
        let result = match publication {
            LastPublication::None => return Ok(()),
            LastPublication::Failed(error) => Err(error),
            LastPublication::Pending(receipt) => {
                self.pump();
                let result = receipt.wait(deadline);
                self.pump();
                result
            }
        };
        if let Err(error) = result {
            self.last_publication = LastPublication::Failed(error.clone());
            return Err(error.into());
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
    /// cannot wake is polled: a correlated step reply is on the critical path
    /// of every streamed chunk, so it is noticed within ~1 ms, not 5.
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
            CollabEvent::EnrolledTaskReply { request_id, result }
                if self.detached.contains_key(&request_id) =>
            {
                (request_id, Detached::Report(result))
            }
            CollabEvent::TaskRunCancellation { request_id, result }
                if self.detached.contains_key(&request_id) =>
            {
                (request_id, Detached::Cancellation(result))
            }
            CollabEvent::TaskRunControl { request_id, result }
                if self.detached.contains_key(&request_id) =>
            {
                (request_id, Detached::Control(result))
            }
            other => return Some(other),
        };
        match self.detached.remove(&request_id) {
            Some(Slot::Awaiting { keep: true }) => {
                self.detached.insert(request_id, Slot::Arrived(reply));
            }
            Some(Slot::Awaiting { keep: false }) => {
                if let Detached::Report(Err(error)) = reply {
                    self.refused.push(error);
                }
            }
            // A second answer to one request id: the first one stands.
            Some(arrived @ Slot::Arrived(_)) => {
                self.detached.insert(request_id, arrived);
            }
            None => {}
        }
        None
    }

    /// Send a correlated request without waiting for its answer: the answer is
    /// taken off the event stream wherever it arrives, so no wait loop has to
    /// know about it. The transport dispatches requests in the order they are
    /// sent, so a later request still reaches the broker after this one.
    ///
    /// An enrolled report's acceptance is dropped and its refusal kept for
    /// [`Self::take_refusals`]; a cancellation poll's answer is kept for
    /// [`Self::take_detached`].
    ///
    /// # Errors
    ///
    /// When `request` has no correlated answer to detach, or cannot enqueue.
    pub fn send_detached(&mut self, request: &CollabRequest) -> Result<()> {
        let (request_id, keep) = match request {
            CollabRequest::ReportEnrolledTask { request_id, .. } => (*request_id, false),
            CollabRequest::TaskRunCancellation { request_id, .. }
            | CollabRequest::TaskRunControl { request_id, .. } => (*request_id, true),
            _ => anyhow::bail!("only enrolled reports and cancellation polls are detached"),
        };
        self.detached.insert(request_id, Slot::Awaiting { keep });
        if let Err(error) = self.send(request) {
            self.detached.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    /// Whether the detached request `request_id` is still unanswered.
    pub fn awaiting(&mut self, request_id: u64) -> bool {
        self.pump();
        matches!(self.detached.get(&request_id), Some(Slot::Awaiting { .. }))
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
            if !matches!(self.detached.get(&request_id), Some(Slot::Awaiting { .. })) {
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

    /// The refusals of detached reports that arrived since the last call.
    pub fn take_refusals(&mut self) -> Vec<String> {
        self.pump();
        std::mem::take(&mut self.refused)
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

    /// Queue newly-polled unrelated events after older deferred work.
    pub(crate) fn defer_fresh(&mut self, events: Vec<CollabEvent>) {
        self.pending.extend(events);
    }

    /// Read one newly arrived event without rescanning events that a
    /// selective session pump already deferred for its normal consumer.
    pub(crate) fn recv_fresh(&mut self) -> Option<CollabEvent> {
        if self.fresh.is_empty() {
            let polled = self.poll_intercepted();
            self.fresh.extend(polled);
        }
        self.fresh.pop_front()
    }
}

impl Link {
    /// Queueing is not acceptance. Wait for the actual correlated RPC result.
    ///
    /// # Errors
    ///
    /// Returns an error when the broker refuses the request, the link fails,
    /// or no correlated reply arrives before the deadline.
    pub fn wait_enrolled_reply(&mut self, request_id: u64) -> Result<()> {
        let deadline = ui::time::now() + Duration::from_secs(10);
        let mut deferred = Vec::new();
        let result = loop {
            if ui::time::now() >= deadline {
                break Err(anyhow::anyhow!("enrolled RPC acknowledgement uncertain"));
            }
            match self.recv(Duration::from_millis(20))? {
                Some(CollabEvent::EnrolledTaskReply {
                    request_id: received,
                    result,
                }) if received == request_id => break result.map_err(anyhow::Error::msg),
                Some(CollabEvent::EnrolledTaskReply {
                    request_id: received,
                    result: Ok(()),
                }) if received < request_id => {
                    // Typed phase milestones do not block the model or Stop.
                    // Their ordered acknowledgements are consumed by the
                    // next correlated owner report instead of becoming an
                    // unbounded deferred-event backlog.
                }
                Some(CollabEvent::EnrolledTaskReply {
                    request_id: received,
                    result: Err(error),
                }) if received < request_id => {
                    break Err(anyhow::anyhow!(
                        "an earlier enrolled report was refused: {error}"
                    ));
                }
                Some(CollabEvent::EnrolledTaskDisconnected { detail }) => {
                    break Err(anyhow::anyhow!(detail));
                }
                Some(event) => {
                    if deferred.len() >= 128 {
                        break Err(anyhow::anyhow!("enrolled reply queue exhausted"));
                    }
                    deferred.push(event);
                }
                None => {}
            }
        };
        self.defer(deferred);
        result
    }

    /// Wait for the exact managed Message acknowledgement without consuming
    /// another message, task, catalogue, or document event.
    ///
    /// # Errors
    /// Returns a transport error if receiving an event fails. A timeout or
    /// broker disconnect remains a typed outcome so the caller retains the
    /// original invocation instead of treating uncertainty as refusal.
    pub(crate) fn wait_agent_message_reply(
        &mut self,
        request_id: u64,
    ) -> Result<AgentMessageRpcOutcome> {
        let deadline = ui::time::now() + Duration::from_secs(10);
        let mut deferred = Vec::new();
        let outcome = loop {
            if ui::time::now() >= deadline {
                break AgentMessageRpcOutcome::Uncertain;
            }
            match self.recv(Duration::from_millis(20))? {
                Some(CollabEvent::AgentMessageReply {
                    request_id: received,
                    result,
                }) if received == request_id => {
                    break match result {
                        Ok(()) => AgentMessageRpcOutcome::Accepted,
                        // The current protocol's generic error carries no
                        // authoritative proof that the owner did not retain
                        // or perform the request.
                        Err(_) => AgentMessageRpcOutcome::Uncertain,
                    };
                }
                Some(CollabEvent::AgentMessageDisconnected { detail }) => {
                    break AgentMessageRpcOutcome::Disconnected(detail);
                }
                Some(event) => {
                    deferred.push(event);
                    if deferred.len() >= 128 {
                        break AgentMessageRpcOutcome::Uncertain;
                    }
                }
                None => {}
            }
        };
        self.defer(deferred);
        Ok(outcome)
    }
}

#[cfg(test)]
pub(crate) fn test_link(transport: impl ui::collaboration::PeerTransport + 'static) -> Link {
    Link::with_manager(CollaborationManager::with_peer_transport(transport), None)
}

#[cfg(test)]
impl Link {
    pub(crate) fn test_fresh(&mut self, events: impl IntoIterator<Item = CollabEvent>) {
        self.fresh.extend(events);
    }

    pub(crate) fn test_fresh_len(&self) -> usize {
        self.fresh.len()
    }
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

    struct EnrolledReplies(Vec<CollabEvent>);

    impl ui::collaboration::PeerTransport for EnrolledReplies {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> std::result::Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            std::mem::take(&mut self.0)
        }
    }

    fn peer_link() -> Link {
        Link::with_manager(
            CollaborationManager::with_peer_transport(AcceptedPeer),
            None,
        )
    }

    #[test]
    fn idle_link_needs_no_completion_protocol_or_extra_deadline() {
        let mut link = peer_link();
        link.complete_publications(Some(ui::time::now())).unwrap();
    }

    #[test]
    fn accepted_document_without_completion_support_is_sticky_failure() {
        let mut link = peer_link();
        assert!(
            link.send(&CollabRequest::document_incremental(vec![1]))
                .is_err()
        );
        assert!(link.complete_publications(None).is_err());
        assert!(link.complete_publications(None).is_err());
    }

    #[test]
    fn correlated_enrolled_wait_consumes_earlier_phase_acknowledgements() {
        let replies = vec![
            CollabEvent::EnrolledTaskReply {
                request_id: 4,
                result: Ok(()),
            },
            CollabEvent::EnrolledTaskReply {
                request_id: 5,
                result: Ok(()),
            },
        ];
        let mut link = test_link(EnrolledReplies(replies));
        link.wait_enrolled_reply(5).unwrap();
        assert!(link.pending.is_empty());
    }

    #[test]
    fn managed_message_wait_defers_every_unrelated_event_in_original_order() {
        let replies = vec![
            CollabEvent::RemoteCursorMoved {
                user_id: "before".into(),
                x: 1.0,
                y: 0.0,
            },
            CollabEvent::AgentMessageReply {
                request_id: 7,
                result: Ok(()),
            },
            CollabEvent::RemoteCursorMoved {
                user_id: "after".into(),
                x: 2.0,
                y: 0.0,
            },
        ];
        let mut link = test_link(EnrolledReplies(replies));
        assert_eq!(
            link.wait_agent_message_reply(7).unwrap(),
            AgentMessageRpcOutcome::Accepted
        );
        assert!(matches!(
            link.recv(Duration::ZERO).unwrap(),
            Some(CollabEvent::RemoteCursorMoved { user_id, .. }) if user_id == "before"
        ));
        assert!(matches!(
            link.recv(Duration::ZERO).unwrap(),
            Some(CollabEvent::RemoteCursorMoved { user_id, .. }) if user_id == "after"
        ));
    }

    #[test]
    fn managed_message_wait_bound_retains_the_event_that_exhausted_it() {
        let mut events = (0..128)
            .map(|index| CollabEvent::RemoteCursorMoved {
                user_id: format!("user-{index}"),
                x: index as f32,
                y: 0.0,
            })
            .collect::<Vec<_>>();
        events.push(CollabEvent::AgentMessageReply {
            request_id: 9,
            result: Ok(()),
        });
        let mut link = test_link(EnrolledReplies(events));
        assert_eq!(
            link.wait_agent_message_reply(9).unwrap(),
            AgentMessageRpcOutcome::Uncertain
        );
        assert_eq!(link.pending.len() + link.fresh.len(), 129);
        for index in 0..128 {
            assert!(matches!(
                link.recv(Duration::ZERO).unwrap(),
                Some(CollabEvent::RemoteCursorMoved { user_id, .. })
                    if user_id == format!("user-{index}")
            ));
        }
        assert!(matches!(
            link.recv(Duration::ZERO).unwrap(),
            Some(CollabEvent::AgentMessageReply { request_id: 9, .. })
        ));
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

    /// Answers every report and cancellation poll at once, each followed by
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
            let (request_id, reply) = match request {
                CollabRequest::ReportEnrolledTask {
                    request_id,
                    sequence,
                    ..
                } => (
                    request_id,
                    CollabEvent::EnrolledTaskReply {
                        request_id,
                        result: if sequence == 2 {
                            Err("sequence 2 refused".into())
                        } else {
                            Ok(())
                        },
                    },
                ),
                CollabRequest::TaskRunCancellation { request_id, .. } => (
                    request_id,
                    CollabEvent::TaskRunCancellation {
                        request_id,
                        result: Ok(true),
                    },
                ),
                CollabRequest::TaskRunControl { request_id, .. } => (
                    request_id,
                    CollabEvent::TaskRunControl {
                        request_id,
                        result: Ok(runtime_collab::TaskRunControl {
                            version: 1,
                            run_epoch: 9,
                            applied_revision: 3,
                            directions: vec![runtime_collab::TaskRunDirection {
                                revision: 4,
                                text: "Use the saved direction".into(),
                            }],
                            cancel_requested: false,
                        }),
                    },
                ),
                _ => return Ok(()),
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

    fn report(request_id: u64, sequence: u64) -> CollabRequest {
        CollabRequest::ReportEnrolledTask {
            request_id,
            acceptance: runtime_collab::EnrolledTaskAcceptance {
                version: 1,
                message_id: "message-1".into(),
                task_id: "task-1".into(),
                run_id: "run-1".into(),
                launch_id: "launch-1".into(),
                runtime_epoch: "epoch-1".into(),
                recipient_agent: "agent-1".into(),
                channel_lease: "channel-1".into(),
                acceptance_sequence: 1,
            },
            sequence,
            update: runtime_collab::TaskRunUpdate::Progress {
                message: format!("update {sequence}"),
            },
        }
    }

    /// Detached answers never reach the session loop: an accepted report is
    /// dropped, a refused one is kept for the session, a cancellation answer
    /// waits for its owner — and every other event keeps its order, including
    /// an answer nobody detached (an in-line wait still sees it).
    #[test]
    fn detached_answers_leave_the_event_stream_without_reordering_it() {
        let mut link = test_link(AnsweringHost { events: Vec::new() });
        link.send_detached(&report(1, 1)).unwrap();
        link.send_detached(&report(2, 2)).unwrap();
        link.send_detached(&CollabRequest::TaskRunCancellation {
            request_id: 3,
            run_id: 7,
            lease: "lease".into(),
        })
        .unwrap();
        // Sent in line: its answer is the caller's to read.
        link.send(&report(4, 3)).unwrap();
        assert!(
            link.send_detached(&CollabRequest::RequestDocumentSnapshot)
                .is_err(),
            "only requests with a correlated answer can be detached"
        );

        let mut seen = Vec::new();
        while let Some(event) = link.recv(Duration::ZERO).unwrap() {
            seen.push(match event {
                CollabEvent::RemoteCursorMoved { user_id, .. } => user_id,
                CollabEvent::EnrolledTaskReply { request_id, .. } => format!("reply-{request_id}"),
                other => panic!("unexpected event {other:?}"),
            });
        }
        assert_eq!(
            seen,
            ["after-1", "after-2", "after-3", "reply-4", "after-4"],
            "detached answers are taken out; the rest keep their order"
        );
        assert!(!link.awaiting(1) && !link.awaiting(2) && !link.awaiting(3));
        assert_eq!(link.take_refusals(), ["sequence 2 refused".to_string()]);
        assert!(link.take_refusals().is_empty(), "a refusal is read once");
        assert_eq!(
            link.take_detached(3),
            Some(Detached::Cancellation(Ok(true)))
        );
        assert_eq!(link.take_detached(3), None, "an answer is taken once");
    }

    /// A forgotten poll's answer is dropped when it arrives, and waiting for
    /// an answer that is already in returns at once.
    #[test]
    fn a_forgotten_poll_is_dropped_and_an_arrived_one_needs_no_wait() {
        let mut link = test_link(AnsweringHost { events: Vec::new() });
        let poll = |request_id| CollabRequest::TaskRunCancellation {
            request_id,
            run_id: 7,
            lease: "lease".into(),
        };
        link.send_detached(&poll(1)).unwrap();
        link.forget(1);
        link.send_detached(&poll(2)).unwrap();
        let began = ui::time::now();
        assert_eq!(
            link.wait_detached(2, began + Duration::from_secs(5)),
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

    #[test]
    fn detached_control_retains_the_exact_owner_frontier() {
        let mut link = test_link(AnsweringHost { events: Vec::new() });
        link.send_detached(&CollabRequest::TaskRunControl {
            request_id: 8,
            run_id: 7,
            lease: "lease".into(),
        })
        .unwrap();

        let Some(Detached::Control(Ok(control))) =
            link.wait_detached(8, ui::time::now() + Duration::from_secs(5))
        else {
            panic!("the exact task-control reply must remain correlated");
        };
        assert_eq!(control.run_epoch, 9);
        assert_eq!(control.applied_revision, 3);
        assert_eq!(control.directions.len(), 1);
        assert_eq!(control.directions[0].revision, 4);
        assert!(!control.cancel_requested);
        assert!(matches!(
            link.recv(Duration::ZERO).unwrap(),
            Some(CollabEvent::RemoteCursorMoved { user_id, .. }) if user_id == "after-8"
        ));
    }
}
