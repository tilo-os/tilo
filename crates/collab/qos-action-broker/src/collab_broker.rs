//! Production collaboration routing, application broker facade and lifetime.
use crate::collab_broker_capnp::CapnpBrokerState;
pub use crate::collab_core::{
    AuditEntry, BrokerChannels, RedactionRecord, apply_redaction_from_envelope,
};
pub use crate::route::CrdtRoute;
use collab_types::{ArtifactId, ChannelState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use ui::collaboration::CollabEvent;
/// Callback an in-process fanout subscriber registers to receive
/// delivered events. Aliased because the raw `Box<dyn Fn(...)>` shape
/// recurs across [`RoomSubscription`], [`FanoutState::subscribe_rooms`]
/// and [`BrokerHandle::subscribe_rooms`], and the last of those is public
/// API other modules call.
pub type FanoutSink = Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>;

thread_local! {
    static BROKER_FACADE_CALLS: std::cell::RefCell<Vec<usize>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

pub(crate) struct BrokerFacadeCallGuard {
    identity: usize,
}

impl BrokerFacadeCallGuard {
    pub(crate) fn enter(identity: usize) -> Self {
        BROKER_FACADE_CALLS.with(|calls| {
            calls.borrow_mut().push(identity);
        });
        Self { identity }
    }
}

impl Drop for BrokerFacadeCallGuard {
    fn drop(&mut self) {
        BROKER_FACADE_CALLS.with(|calls| {
            let popped = calls.borrow_mut().pop();
            debug_assert_eq!(popped, Some(self.identity));
        });
    }
}

fn broker_facade_call_active(identity: Option<usize>) -> bool {
    identity.is_some_and(|identity| {
        BROKER_FACADE_CALLS.with(|calls| calls.borrow().contains(&identity))
    })
}

/// Shared fanout registry for typed RPC subscribers and in-process shell consumers.
pub struct FanoutState {
    stopped: AtomicBool,
    peer_id_counter: AtomicU64,
    capnp: Mutex<Option<Arc<CapnpBrokerState>>>,
    local_subs: Mutex<Vec<Arc<RoomSubscription>>>,
}

/// A subscriber inside the shell process, addressed by room — how the
/// compositor reads presence its apps publish. Delivery goes through the
/// same [`CrdtRoute::delivers_to`] check as a typed subscriber, against
/// `CrdtRoute::cluster(room)`; since presence widens to cluster scope on
/// the way out, two apps in one room reach one subscription, so a cursor
/// crossing between them is one peer moving, not a second appearing.
/// Rooms are mutable — workspaces are created, and join a peer's room
/// when an invite is accepted.
pub struct RoomSubscription {
    rooms: Mutex<Vec<String>>,
    sink: FanoutSink,
}

impl RoomSubscription {
    /// Replace the set of rooms this subscription covers.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            *g = rooms;
        }
    }

    /// Whether `route` addresses any room this subscription holds. Uses
    /// the broker's own delivery predicate, so it never receives
    /// anything a socket peer in the same room wouldn't.
    fn wants(&self, route: &CrdtRoute) -> bool {
        let Ok(rooms) = self.rooms.lock() else {
            return false;
        };
        rooms
            .iter()
            .any(|room| route.delivers_to(&CrdtRoute::cluster(room.clone())))
    }
}

impl FanoutState {
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_peer_counter_for_test(&self, value: u64) {
        self.peer_id_counter.store(value, Ordering::Relaxed);
    }
    pub fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            peer_id_counter: AtomicU64::new(0),
            capnp: Mutex::new(None),
            local_subs: Mutex::new(Vec::new()),
        }
    }

    /// Register an in-process subscriber, returning a handle the caller
    /// can use to keep its room set current.
    pub fn subscribe_rooms(&self, rooms: Vec<String>, sink: FanoutSink) -> Arc<RoomSubscription> {
        let sub = Arc::new(RoomSubscription {
            rooms: Mutex::new(rooms),
            sink,
        });
        if self.stopped.load(Ordering::Acquire) {
            return sub;
        }
        if let Ok(mut g) = self.local_subs.lock()
            && !self.stopped.load(Ordering::Acquire)
        {
            g.push(Arc::clone(&sub));
        }
        sub
    }

    /// Snapshot the in-process subscribers so the sinks run without
    /// the registry lock held.
    fn local_subs(&self) -> Vec<Arc<RoomSubscription>> {
        self.local_subs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Mint the next typed connection id, reserving zero for the broker root
    /// and `u64::MAX` for the current-connection sentinel. The counter itself
    /// remains the number of accepted connections, including before the first.
    pub fn next_peer_id(&self) -> u64 {
        let previous = self
            .peer_id_counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1).filter(|next| *next < u64::MAX)
            })
            .expect("application peer id space exhausted");
        previous + 1
    }

    /// How many typed connections this broker has ever accepted. Read as a
    /// generation, not a count: it only rises, so
    /// a value newer than the one recorded beside an announcement means
    /// someone connected who wasn't there to hear it — see
    /// `remote_participant_id` in the native adapter, which uses that to re-say a name.
    pub fn connections_seen(&self) -> u64 {
        self.peer_id_counter.load(Ordering::Relaxed)
    }

    /// Attach the capnp fanout sink. Called at most once, at
    /// broker-spawn time; repeat calls are silently ignored.
    pub fn attach_capnp(&self, state: Arc<CapnpBrokerState>) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut g) = self.capnp.lock()
            && g.is_none()
            && !self.stopped.load(Ordering::Acquire)
            && state.is_accepting()
        {
            *g = Some(state);
        }
    }

    /// Stop the exact attached broker while excluding new local and typed
    /// fanout. The broker releases its journal before this method detaches it,
    /// and a replacement can never be cleared by the old thread's guard.
    pub(crate) fn shutdown_capnp(&self, state: &Arc<CapnpBrokerState>) {
        self.request_shutdown();
        state.stop();
        let mut guard = match self.capnp.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(attached, state))
        {
            *guard = None;
        }
        match self.local_subs.lock() {
            Ok(mut subscribers) => subscribers.clear(),
            Err(poisoned) => poisoned.into_inner().clear(),
        }
    }

    pub(crate) fn request_shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let state = self.capnp.lock().ok().and_then(|state| state.clone());
        if let Some(state) = state {
            state.request_stop();
        }
    }

    /// Read the capnp fanout sink if one has been attached.
    pub(crate) fn capnp_state(&self) -> Option<Arc<CapnpBrokerState>> {
        self.capnp
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|state| state.is_accepting())
    }

    /// Deliver `event` to every typed or in-process subscriber addressed by
    /// `route`, skipping the originating typed peer id.
    pub fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        if route.cluster_id.is_empty() {
            tracing::debug!(
                "collab_broker: dropping event with an unaddressed route \
                 (sender never announced a cluster)"
            );
            return;
        }
        for sub in self.local_subs() {
            if self.stopped.load(Ordering::Acquire) {
                return;
            }
            if sub.wants(route) {
                (sub.sink)(event, route);
            }
        }
        if let Some(capnp) = self.capnp_state() {
            capnp.fanout_to_route(event, except, route);
        }
    }
}

/// Owns the typed broker and optional peer-network drain.
/// Cloneable, thread-safe slice of the broker used by shell chrome. Keeping
/// the join handles out of the closure avoids extending broker lifetime or
/// creating an ownership cycle with the root widget.
#[derive(Clone)]
pub struct SessionFollowDispatcher {
    fanout: Arc<FanoutState>,
}

impl SessionFollowDispatcher {
    pub fn new(fanout: Arc<FanoutState>) -> Self {
        Self { fanout }
    }
    pub fn set(&self, room: &str, peer_id: Option<String>) {
        self.fanout.fanout_to_route(
            &CollabEvent::FollowChanged { peer_id },
            u64::MAX,
            &CrdtRoute::cluster(room.to_string()),
        );
    }
}

/// Cloneable, transport-owning facade used by shell search providers. It
/// keeps only the shared action directory alive, never the broker threads or
/// socket owner.
#[derive(Clone)]
pub struct AppActionBroker {
    state: Arc<CapnpBrokerState>,
}

impl AppActionBroker {
    /// Reports the actual current enrolled subscription, without allocating or
    /// recovering any process/task authority.
    pub fn enrolled_recipient_ready(&self, channel: &str) -> bool {
        self.state
            .while_live(|state| Ok(state.enrolled_recipient_ready(channel)))
            .unwrap_or(false)
    }

    pub fn install_enrolled_connection(
        &self,
        connection: launcher::managed::channel::AdmittedManagedConnection,
        caller: tilo_user_identity::DeviceCert,
    ) -> Result<String, String> {
        self.state
            .while_live(|state| state.install_enrolled_connection(connection, caller))
    }
    pub fn dispatch_enrolled_task(
        &self,
        invocation: uuid::Uuid,
        caller: &tilo_user_identity::DeviceCert,
        envelope: runtime_collab::EnrolledTaskEnvelope,
    ) -> Result<crate::enrolled_dispatch::EnrolledDispatchResult, String> {
        self.state
            .while_live(|state| state.dispatch_enrolled_task(invocation, caller, envelope))
    }
    /// See [`crate::collab_broker_capnp::CapnpBrokerState::install_fixture_enrolled_connection`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_fixture_enrolled_connection(
        &self,
        process: crate::enrolled_dispatch::EnrolledProcess,
        stream: std::os::unix::net::UnixStream,
        caller: tilo_user_identity::DeviceCert,
    ) -> Result<String, String> {
        self.state
            .while_live(|state| state.install_fixture_enrolled_connection(process, stream, caller))
    }
    pub fn enrolled_child_binding(
        &self,
        dispatch: &crate::enrolled_dispatch::VerifiedEnrolledDispatch,
        peer: u64,
        resource: &str,
        action: &str,
    ) -> Result<(runtime_collab::PublishedInvocationBinding, String), String> {
        self.state
            .while_live(|state| state.enrolled_child_binding(dispatch, peer, resource, action))
    }
    pub fn invoke_enrolled_child(
        &self,
        dispatch: &crate::enrolled_dispatch::VerifiedEnrolledDispatch,
        request: &crate::collab_broker_capnp::EnrolledChildRequest,
    ) -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
        self.state
            .while_live(|state| state.invoke_enrolled_child(dispatch, request))
    }
    pub fn observe_enrolled_child(
        &self,
        owner: &crate::collab_broker_capnp::EnrolledChildOwner,
        host: [u8; 16],
        invocation: [u8; 16],
    ) -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
        self.state
            .while_live(|state| state.observe_enrolled_child(owner, host, invocation))
    }
    /// The live dispatch witness of an accepted enrolled task on `channel`.
    pub fn enrolled_task_dispatch(
        &self,
        channel: &str,
        invocation: uuid::Uuid,
    ) -> Result<crate::enrolled_dispatch::VerifiedEnrolledDispatch, String> {
        self.state
            .while_live(|state| state.enrolled_task_dispatch(channel, invocation))
    }

    /// Construct the shell-facing facade around a real broker state for
    /// `LocalSet` integration tests. Production hosts construct this facade
    /// through `BrokerHandle`; arbitrary state injection is test-only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_state_for_test(state: Arc<CapnpBrokerState>) -> Self {
        Self { state }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_action_directory_roots_for_test(
        &self,
        roots: Vec<crate::collab_broker_capnp::ActionDirectoryRoot>,
    ) {
        let _ = self.state.while_live(|state| {
            state.replace_action_directory_roots_for_test(roots);
            Ok(())
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_published_surfaces_for_test(
        &self,
        surfaces: Vec<crate::collab_broker_capnp::PublishedSurface>,
    ) {
        let _ = self.state.while_live(|state| {
            state.replace_published_surfaces_for_test(surfaces);
            Ok(())
        });
    }

    /// Bounded, nonblocking bridge from shell chrome into the broker's
    /// LocalSet-owned application directory capabilities. Palette code must
    /// poll the returned oneshot in its cache owner; it must never block a
    /// compositor frame on a Cap'n Proto RPC.
    #[must_use]
    pub fn action_directory_request_queue(
        &self,
    ) -> crate::collab_broker_capnp::ActionDirectoryRequestQueue {
        self.state
            .while_live(|state| Ok(state.action_directory_request_queue()))
            .unwrap_or_else(|_| {
                crate::collab_broker_capnp::ActionDirectoryRequestQueue::unavailable()
            })
    }

    /// Reusable native broker attachment for an ordinary standalone peer.
    pub fn peer_action_attachment(
        &self,
    ) -> Result<(crate::peer_actions::PeerActionQueue, uuid::Uuid), String> {
        self.state
            .while_live_fenced(|state| Ok((state.peer_action_queue(), state.invocation_host()?)))
    }

    pub fn surfaces(&self) -> Vec<crate::collab_broker_capnp::PublishedSurface> {
        self.state
            .while_live(|state| Ok(state.published_surfaces()))
            .unwrap_or_default()
    }

    /// Generation of the retained published-surface source.  Palette query
    /// edits use this inexpensive read before deciding to clone a new source
    /// snapshot.
    #[must_use]
    pub fn surfaces_generation(&self) -> u64 {
        self.state
            .while_live(|state| Ok(state.published_surfaces_generation()))
            .unwrap_or_default()
    }

    /// Small inert routes for currently connected `ActionDirectory` sources.
    /// Shell search uses these to fan out bounded pull queries; capabilities
    /// themselves remain owned by the broker `LocalSet`.
    pub fn action_directory_roots(&self) -> Vec<crate::collab_broker_capnp::ActionDirectoryRoot> {
        self.state
            .while_live(|state| Ok(state.action_directory_roots()))
            .unwrap_or_default()
    }

    /// Fetch one deterministic, bounded page of inert directory routes.
    /// Callers that expose root navigation retain `next_after_peer`; routine
    /// palette queries use the first page without rebuilding the registry.
    #[must_use]
    pub fn action_directory_root_page(
        &self,
        after_peer: Option<u64>,
        max_results: usize,
    ) -> crate::collab_broker_capnp::ActionDirectoryRootPage {
        self.state
            .while_live(|state| Ok(state.action_directory_root_page(after_peer, max_results)))
            .unwrap_or_default()
    }

    /// Invoke one published application action.
    ///
    /// # Errors
    ///
    /// Returns an error if the surface or action is unavailable or dispatch
    /// to its application connection fails.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<crate::collab_broker_capnp::InvocationCompletion>, String>
    {
        self.state
            .while_live(|state| state.invoke(peer_id, resource, action, argument, actor))
    }

    /// Start a published action and return its lifecycle job.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is unavailable or cannot be dispatched.
    pub fn start(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<crate::collab_broker_capnp::InvocationJob, String> {
        self.state
            .while_live(|state| state.start_invocation(peer_id, resource, action, argument, actor))
    }

    pub fn status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        self.state
            .while_live(|state| Ok(state.invocation_status(id)))
            .ok()
            .flatten()
    }

    pub fn statuses(&self, actor: &str) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.state
            .while_live(|state| Ok(state.invocation_statuses(actor)))
            .unwrap_or_default()
    }

    /// Request cooperative cancellation for an owned invocation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, foreign, or non-cancellable invocation.
    pub fn cancel(&self, id: u64, actor: &str) -> Result<(), String> {
        self.state
            .while_live(|state| state.request_cancellation(id, actor))
    }

    pub fn can_cancel(&self, id: u64, actor: &str) -> bool {
        self.state
            .while_live(|state| Ok(state.can_cancel(id, actor)))
            .unwrap_or(false)
    }

    /// True only while the named admitted agent has a live task subscription.
    pub fn agent_is_live_task_capable(&self, room: &str, agent: &str) -> bool {
        self.state
            .while_live(|state| Ok(state.agent_is_live_task_capable(room, agent)))
            .unwrap_or(false)
    }

    /// Dispatch a task to its assigned live agent. There is intentionally no
    /// offline queue or application launch fallback in this boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching admitted live agent can accept the
    /// task.
    pub fn dispatch_task(&self, room: &str, task_id: &str) -> Result<u64, String> {
        self.state
            .while_live(|state| state.dispatch_workspace_task(room, task_id))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Confirmation binds the complete invocation route and caller evidence in one operation."
    )]
    /// Prepare a one-use confirmation token for the exact invocation inputs.
    ///
    /// # Errors
    ///
    /// Returns an error when the action disappeared or confirmation evidence is invalid.
    pub fn prepare_confirmation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        phrase: Option<&str>,
    ) -> Result<String, String> {
        self.state.while_live(|state| {
            state.prepare_confirmation(peer_id, resource, action, argument, actor, phrase)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Starting a confirmed action must retain the exact invocation inputs used to issue its token."
    )]
    /// Start a published action using a previously prepared confirmation token.
    ///
    /// # Errors
    ///
    /// Returns an error when the action or token is unavailable or invalid.
    pub fn start_confirmed(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        token: &str,
    ) -> Result<crate::collab_broker_capnp::InvocationJob, String> {
        self.state.while_live(|state| {
            state.start_invocation_confirmed(
                peer_id,
                resource,
                action,
                argument,
                actor,
                Some(token),
            )
        })
    }
}

pub struct BrokerHandle {
    capnp_socket_path: PathBuf,
    capnp_thread: Option<JoinHandle<()>>,
    capnp_identity: Option<usize>,
    capnp_quit: Arc<AtomicBool>,
    pub fanout: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
}
impl BrokerHandle {
    /// Observe current private host-worker provenance for a native publication.
    pub fn is_current_host_worker(
        &self,
        peer: u64,
        route: &CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<bool, String> {
        match self.fanout.capnp_state() {
            Some(state) => state.is_current_host_worker(peer, route, certificate),
            None => Ok(false),
        }
    }

    pub fn connect_local_worker(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.fanout
            .capnp_state()
            .ok_or("broker unavailable")?
            .connect_local_worker(route, certificate)
    }

    pub fn connect_local_worker_with_authorizer(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<
        (
            std::os::unix::net::UnixStream,
            ui::host_worker::PublishedInvocationAuthorizer,
        ),
        String,
    > {
        self.fanout
            .capnp_state()
            .ok_or("broker unavailable")?
            .connect_local_worker_with_authorizer(route, certificate)
    }

    /// Cloneable handle for the live resource/action search provider.
    pub fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.fanout
            .capnp_state()
            .map(|state| AppActionBroker { state })
    }

    /// Number of typed connections accepted since this broker started.
    pub fn peer_count(&self) -> usize {
        // Saturates rather than truncates on a 32-bit target — this is a
        // connection count, not an index, so clamping to `usize::MAX`
        // loses nothing a real desktop session would ever reach.
        usize::try_from(self.fanout.connections_seen()).unwrap_or(usize::MAX)
    }

    /// Every typed connection joins a channel as part of setup.
    pub fn announced_peer_count(&self) -> usize {
        self.peer_count()
    }

    /// Fan a `CollabEvent` to the peers `route` addresses. `except`
    /// skips one peer id, for the Iroh bridge. No "reach everyone"
    /// form exists — see [`FanoutState::fanout_to_route`].
    pub fn fanout_event(&self, event: &CollabEvent, except: Option<u64>, route: &CrdtRoute) {
        self.fanout
            .fanout_to_route(event, except.unwrap_or(u64::MAX), route);
    }

    /// Bind every local app in `room` to one participant's published view,
    /// or release the binding. This is session-local and never enters the
    /// peer publisher; each app already receives that participant's normal
    /// view-presence frames and applies them through `ui::App::follow`.
    pub fn set_follow_target(&self, room: &str, peer_id: Option<String>) {
        self.fanout_event(
            &CollabEvent::FollowChanged { peer_id },
            None,
            &CrdtRoute::cluster(room.to_string()),
        );
    }

    #[must_use]
    pub fn follow_dispatcher(&self) -> SessionFollowDispatcher {
        SessionFollowDispatcher {
            fanout: Arc::clone(&self.fanout),
        }
    }

    /// Subscribe this process to the rooms it hosts. The sink runs on
    /// the reader thread that decoded the frame: keep it short.
    pub fn subscribe_rooms(&self, rooms: Vec<String>, sink: FanoutSink) -> Arc<RoomSubscription> {
        self.fanout.subscribe_rooms(rooms, sink)
    }

    /// The bound Cap'n Proto socket.
    pub fn capnp_socket(&self) -> &std::path::Path {
        &self.capnp_socket_path
    }

    /// Shared handle to per-channel state + audit log. Tests use this
    /// to install a [`ChannelState`] before sending signed envelopes.
    pub fn channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.channels)
    }

    /// Snapshot the resource/action surfaces live applications published to
    /// this session. The values are inert; invoking is a separate operation.
    pub fn published_action_surfaces(&self) -> Vec<crate::collab_broker_capnp::PublishedSurface> {
        self.fanout
            .capnp_state()
            .map_or_else(Vec::new, |state| state.published_surfaces())
    }

    /// Check one live published route without cloning unrelated descriptors.
    pub fn has_live_published_action(
        &self,
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        resource: &str,
        action: &str,
    ) -> bool {
        self.fanout.capnp_state().is_some_and(|state| {
            state.has_live_published_action(app_id, cluster_id, record_id, resource, action)
        })
    }

    /// Direct one invocation to the exact application connection that
    /// published the addressed operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the action broker, surface, or action is
    /// unavailable or dispatch to its application connection fails.
    pub fn invoke_published_action(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<crate::collab_broker_capnp::InvocationCompletion>, String>
    {
        self.fanout
            .capnp_state()
            .ok_or_else(|| "the application action broker is unavailable".to_string())?
            .invoke(peer_id, resource, action, argument, actor)
    }

    /// Install a populated [`ChannelState`] for one artifact id.
    pub fn register_channel_state(&self, artifact: ArtifactId, state: ChannelState) {
        if let Ok(mut ch) = self.channels.lock() {
            ch.register_channel_state(artifact, state);
        }
    }

    /// Audit-log entries for one artifact.
    pub fn audit_for(&self, artifact: ArtifactId) -> Vec<AuditEntry> {
        self.channels
            .lock()
            .map(|g| g.audit.for_artifact(artifact))
            .unwrap_or_default()
    }

    /// Structured redaction records for one artifact, independent of
    /// `audit_for` so the compliance surface skips filtering drops.
    pub fn redactions_for(&self, artifact: ArtifactId) -> Vec<RedactionRecord> {
        self.channels
            .lock()
            .map(|g| g.redactions.for_artifact(artifact))
            .unwrap_or_default()
    }
}
impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.fanout.request_shutdown();
        self.capnp_quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.capnp_thread.take() {
            join_owned_thread(thread, self.capnp_identity);
        }
    }
}

fn join_owned_thread(thread: JoinHandle<()>, identity: Option<usize>) {
    if thread.thread().id() != std::thread::current().id() && !broker_facade_call_active(identity) {
        let _ = thread.join();
    }
}
/// Start the production broker with explicit host-owned authority and services.
pub fn spawn_with_services(
    socket: PathBuf,
    membership: Option<crate::services::WorkspaceAuthorityHandle>,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: Option<&crate::peer_actions::NativePeerBrokerConfig>,
    services: crate::services::HostServices,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> std::io::Result<BrokerHandle> {
    spawn_inner(
        socket, membership, publisher, config, services, None, channels, fanout,
    )
}

/// Native compatibility fixtures only; production callers inject current authority.
#[cfg(any(test, feature = "test-support"))]
pub fn spawn_with_test_services(
    socket: PathBuf,
    membership: Option<crate::services::WorkspaceAuthorityHandle>,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: Option<&crate::peer_actions::NativePeerBrokerConfig>,
    services: crate::services::HostServices,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> std::io::Result<BrokerHandle> {
    spawn_inner(
        socket,
        membership,
        publisher,
        config,
        services,
        trusted_devices,
        channels,
        fanout,
    )
}

fn spawn_inner(
    socket: PathBuf,
    membership: Option<crate::services::WorkspaceAuthorityHandle>,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: Option<&crate::peer_actions::NativePeerBrokerConfig>,
    services: crate::services::HostServices,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> std::io::Result<BrokerHandle> {
    let quit = Arc::new(AtomicBool::new(false));
    let thread = crate::collab_broker_capnp::spawn_configured(
        socket.clone(),
        &fanout,
        channels.clone(),
        publisher,
        membership,
        trusted_devices,
        services,
        quit.clone(),
        config,
    )?;
    let capnp_identity = fanout.capnp_state().map(|state| Arc::as_ptr(&state).addr());
    Ok(BrokerHandle {
        capnp_socket_path: socket,
        capnp_thread: Some(thread),
        capnp_identity,
        capnp_quit: quit,
        fanout,
        channels,
    })
}

/// Ordinary peer production constructor. All trust and durability belong to the caller's owner.
pub fn spawn_native_peer(
    socket: PathBuf,
    membership: crate::services::WorkspaceAuthorityHandle,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: crate::peer_actions::NativePeerBrokerConfig,
) -> std::io::Result<BrokerHandle> {
    spawn_native_peer_with_services(socket, membership, publisher, config, Default::default())
}
pub fn spawn_native_peer_with_services(
    socket: PathBuf,
    membership: crate::services::WorkspaceAuthorityHandle,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: crate::peer_actions::NativePeerBrokerConfig,
    services: crate::services::HostServices,
) -> std::io::Result<BrokerHandle> {
    if !publisher.is_wired() {
        return Err(std::io::Error::other(
            "peer broker requires an attached signed-document publisher",
        ));
    }
    spawn_with_services(
        socket,
        Some(membership),
        publisher,
        Some(&config),
        services,
        Arc::new(Mutex::new(BrokerChannels::default())),
        Arc::new(FanoutState::new()),
    )
}
