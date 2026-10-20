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
    certified_people: Mutex<Vec<std::sync::Weak<CertifiedPeopleSubscription>>>,
}

/// Host-local observer of complete, owner-certified room membership. This is
/// intentionally separate from ordinary app Context fanout: only the broker's
/// native membership refresh can publish it.
pub type CertifiedPeopleSink =
    Arc<dyn Fn(String, Vec<ui::collaboration::Collaborator>) + Send + Sync>;

pub struct CertifiedPeopleSubscription {
    sink: CertifiedPeopleSink,
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

impl Default for FanoutState {
    fn default() -> Self {
        Self::new()
    }
}

impl FanoutState {
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_peer_counter_for_test(&self, value: u64) {
        self.peer_id_counter.store(value, Ordering::Relaxed);
    }
    #[must_use]
    pub fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            peer_id_counter: AtomicU64::new(0),
            capnp: Mutex::new(None),
            local_subs: Mutex::new(Vec::new()),
            certified_people: Mutex::new(Vec::new()),
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

    pub(crate) fn subscribe_certified_people(
        &self,
        sink: CertifiedPeopleSink,
    ) -> Arc<CertifiedPeopleSubscription> {
        let subscription = Arc::new(CertifiedPeopleSubscription { sink });
        let state = self.capnp_state();
        if let Ok(mut observers) = self.certified_people.lock() {
            observers.push(Arc::downgrade(&subscription));
            if let Some(state) = state {
                for (room, people) in state.certified_people_for_shell() {
                    (subscription.sink)(room, people);
                }
            }
        }
        subscription
    }

    pub(crate) fn publish_certified_people(&self, room: &str) {
        let people = self
            .capnp_state()
            .map_or_else(Vec::new, |state| state.certified_people_in_room(room));
        if let Ok(mut observers) = self.certified_people.lock() {
            observers.retain(|observer| {
                let Some(observer) = observer.upgrade() else {
                    return false;
                };
                (observer.sink)(room.to_owned(), people.clone());
                true
            });
        }
    }

    /// Mint the next typed connection id, reserving zero for the broker root
    /// and `u64::MAX` for the current-connection sentinel. The counter itself
    /// remains the number of accepted connections, including before the first.
    ///
    /// # Panics
    ///
    /// Panics once `u64::MAX - 1` connections were accepted.
    #[expect(
        clippy::expect_used,
        reason = "exhausting a 64-bit connection counter cannot happen within one process"
    )]
    pub fn next_peer_id(&self) -> u64 {
        let previous = self
            .peer_id_counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1).filter(|next| *next < u64::MAX)
            })
            .expect("application peer id space exhausted");
        // `fetch_update` only stored `previous + 1` when it stayed below `u64::MAX`.
        previous.saturating_add(1)
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
        let certified_rooms = state
            .certified_people_for_shell()
            .into_iter()
            .map(|(room, _)| room)
            .collect::<Vec<_>>();
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
        match self.certified_people.lock() {
            Ok(mut observers) => {
                for observer in observers.iter().filter_map(std::sync::Weak::upgrade) {
                    for room in &certified_rooms {
                        (observer.sink)(room.clone(), Vec::new());
                    }
                }
                observers.clear();
            }
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
    pub fn capnp_state(&self) -> Option<Arc<CapnpBrokerState>> {
        self.capnp
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|state| state.is_accepting())
    }

    /// Whether a local app subscribes to exactly `route`'s documents.
    pub fn holds_route(&self, route: &CrdtRoute) -> bool {
        self.capnp_state()
            .is_some_and(|state| state.holds_route(route))
    }

    /// Ask the local apps holding records in `cluster_id` to publish their
    /// snapshots; see [`CapnpBrokerState::request_room_snapshots`].
    pub fn request_room_snapshots(&self, cluster_id: &str) -> usize {
        self.capnp_state()
            .map_or(0, |state| state.request_room_snapshots(cluster_id))
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

    /// Ask exactly one application instance, addressed by its primary route
    /// in `room`, for the next step of moving its window. Session-local like
    /// Follow; the answer arrives through the host's continuation services.
    pub fn request_handoff(
        &self,
        room: &str,
        app: &str,
        instance: u64,
        request: ui::collaboration::handoff::HandoffRequest,
    ) {
        self.fanout.fanout_to_route(
            &CollabEvent::Handoff { request },
            u64::MAX,
            &CrdtRoute::record(app, room, format!("continuation:{instance}")),
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

/// Session-owned broker discovery, without retaining its mutex/journal/process graph.
/// Pending desktop launch chrome upgrades only while performing an admitted request.
#[derive(Clone)]
pub struct WeakAppActionBroker(Arc<dyn BrokerDiscovery>);

// Desktop discovery does not expose the server's mutex-bearing graph. The
// session owns that graph; this concrete discovery implementation owns only Weak.
trait BrokerDiscovery: Send + Sync {
    fn current(&self) -> Result<AppActionBroker, String>;
}
struct WeakBrokerDiscovery(std::sync::Weak<CapnpBrokerState>);
impl BrokerDiscovery for WeakBrokerDiscovery {
    fn current(&self) -> Result<AppActionBroker, String> {
        let state = self
            .0
            .upgrade()
            .ok_or("native app opening broker retired")?;
        state.while_live_fenced(|_| Ok(()))?;
        Ok(AppActionBroker { state })
    }
}

impl WeakAppActionBroker {
    /// Acquire a temporary facade only while the owning session still accepts work.
    ///
    /// # Errors
    /// The session has dropped or stopped its actual broker.
    pub fn upgrade(&self) -> Result<AppActionBroker, String> {
        self.0.current()
    }
}

impl AppActionBroker {
    /// Exact current owner-backed agent identity while the session is live.
    pub fn certified_agent_identity(
        &self,
        room: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.state
            .while_live(|state| Ok(state.certified_agent_identity(room, actor)))
            .ok()
            .flatten()
    }

    /// Retain discovery without extending the owning session's lifetime.
    #[must_use]
    pub fn downgrade(&self) -> WeakAppActionBroker {
        WeakAppActionBroker(Arc::new(WeakBrokerDiscovery(Arc::downgrade(&self.state))))
    }
    /// Invoke one closed native workspace-membership command as the retained
    /// certified person. The owner still rechecks current Admin authority.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker, published action, or caller custody
    /// is unavailable.
    pub fn change_workspace_membership(
        &self,
        room: &str,
        command: &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<crate::collab_broker_capnp::InvocationJob, String> {
        self.state
            .while_live(|state| state.invoke_workspace_membership(room, command, certificate))
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
    /// `LocalSet`-owned application directory capabilities. Palette code must
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

    /// A local action door bound to an already-issued signed room admission and
    /// one host-issued execution. No device is enrolled and no grant is minted.
    ///
    /// # Errors
    ///
    /// Refuses unavailable/currently invalid admission, a missing registered
    /// policy clock, or any mismatch with the transport-owned caller.
    pub fn certified_local_action_host(
        &self,
        caller: &tilo_actions::remote::Caller,
        presentation: &[u8],
    ) -> Result<Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>, String> {
        self.state.certified_local_action_host(caller, presentation)
    }

    /// [`Self::certified_local_action_host`] handed to agent `agent` by the
    /// person's own device; see
    /// [`CapnpBrokerState::certified_local_agent_host`].
    ///
    /// # Errors
    ///
    /// As [`Self::certified_local_action_host`].
    pub fn certified_local_agent_host(
        &self,
        caller: &tilo_actions::remote::Caller,
        presentation: &[u8],
        agent: &str,
    ) -> Result<std::sync::Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>, String> {
        self.state
            .certified_local_agent_host(caller, presentation, agent)
    }

    /// Reuse the checked signed person-room admission for native owner checks.
    /// # Errors
    /// Refuses non-person, foreign, stale or unadmitted execution; grants none.
    pub fn certified_person_context(
        &self,
        caller: &tilo_actions::remote::Caller,
        presentation: &[u8],
    ) -> Result<crate::services::CertifiedPersonContext, String> {
        self.state.certified_person_context(caller, presentation)
    }

    /// Reusable native broker attachment for an ordinary standalone peer.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker stopped or has no invocation host.
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

    /// Read one current advertised action without cloning the whole inventory.
    pub fn published_action(
        &self,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(
        crate::collab_broker_capnp::PublishedSurface,
        crate::collab_broker_capnp::PublishedAction,
    )> {
        self.state
            .while_live(|state| Ok(state.published_action(route, resource, action)))
            .ok()
            .flatten()
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

    /// Current certified checkout of an attested local native task. This is
    /// inert provenance for the owner state door, never an invocation grant.
    #[must_use]
    pub fn native_checkout(
        &self,
        app: &str,
        room: &str,
        instance: u64,
        pid: u32,
        start: u64,
    ) -> Option<crate::collab_broker_capnp::ActionDirectoryRoot> {
        self.state
            .while_live(|state| Ok(state.native_checkout(app, room, instance, pid, start)))
            .ok()
            .flatten()
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

    /// The facade around a live broker state, for a host's action host.
    #[must_use]
    pub fn from_state(state: Arc<CapnpBrokerState>) -> Self {
        Self { state }
    }

    /// The broker state behind this facade, for the host's action host.
    #[must_use]
    pub fn state(&self) -> &Arc<CapnpBrokerState> {
        &self.state
    }

    /// Read what each application declared on the action door from
    /// `resolver` from now on.
    pub fn set_app_action_access(&self, resolver: crate::app_actions::AppActionAccessResolver) {
        self.state.set_app_action_access(resolver);
    }

    /// Ask `prompter` before an application uses another application's
    /// action it holds no grant for. Without one, such a request is refused.
    pub fn set_app_action_consent(
        &self,
        prompter: Arc<dyn crate::app_actions::AppActionConsentPrompter>,
    ) {
        self.state.set_app_action_consent(prompter);
    }

    /// Shortcut runs finished since the last call, oldest first, for the
    /// history recorder to record against the documents they changed.
    #[must_use]
    pub fn take_finished_shortcut_runs(&self) -> Vec<crate::automation::FinishedShortcutRun> {
        self.state.take_finished_shortcut_runs()
    }

    /// Whether a finished shortcut run is waiting to be taken, without
    /// taking it: a frame can ask every tick.
    #[must_use]
    pub fn has_finished_shortcut_runs(&self) -> bool {
        self.state.has_finished_shortcut_runs()
    }

    /// Agent runs of workspace tasks that ended since the last call, oldest
    /// first, for the history recorder to record against the documents they
    /// changed.
    #[must_use]
    pub fn take_ended_task_runs(&self) -> Vec<crate::task_runs::EndedTaskRun> {
        self.state.take_ended_task_runs()
    }

    /// Whether an ended task run is waiting to be taken, without taking it.
    #[must_use]
    pub fn has_ended_task_runs(&self) -> bool {
        self.state.has_ended_task_runs()
    }

    /// Run saved shortcut `shortcut` in `room` for `caller`, who is at the
    /// screen; its confirmations and questions wait for the compositor's
    /// sheet ([`Self::take_step_prompt`]). Answers with the broker job the
    /// run reports on.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`.
    pub fn start_interactive_shortcut_run(
        &self,
        caller: &tilo_actions::remote::Caller,
        room: &str,
        shortcut: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, tilo_actions::remote::ActionHostError> {
        self.state
            .start_interactive_shortcut_run(caller, room, shortcut, inputs)
    }

    /// The oldest step of an interactive shortcut run waiting for its
    /// person.
    #[must_use]
    pub fn take_step_prompt(&self) -> Option<crate::automation::PromptedStep> {
        self.state.take_step_prompt()
    }

    /// Start saved shortcut `shortcut` in `room` because its `trigger`
    /// fired, on behalf of `caller`, the person who saved it, with `inputs`
    /// the values the trigger gives the shortcut's inputs. Answers with
    /// the broker job the run reports on.
    ///
    /// # Errors
    ///
    /// Why the trigger may not start it: the shortcut is gone or lost the
    /// trigger, `caller` did not save it, or it now reaches past what
    /// `caller` may do.
    pub fn start_triggered_shortcut_run(
        &self,
        caller: &tilo_actions::remote::Caller,
        room: &str,
        shortcut: &str,
        trigger: &tilo_automation::TriggerSpec,
        run: tilo_automation::RunId,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, tilo_actions::remote::ActionHostError> {
        self.state
            .start_triggered_shortcut_run(caller, room, shortcut, trigger, run, inputs)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn record_completion_for_test(
        &self,
        id: u64,
        actor: &str,
        update: tilo_actions::InvocationUpdate,
    ) {
        self.state.record_completion_for_test(id, actor, update);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_invocations_for_test(
        &self,
        peer_id: u64,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::collab_broker_capnp::InvocationEnvelope> {
        self.state.capture_invocations_for_test(peer_id)
    }

    /// Changes whenever any invocation's status changes.
    #[must_use]
    pub fn statuses_generation(&self) -> u64 {
        self.state.invocations_generation()
    }

    /// Sees every invocation status change after this call — what
    /// [`Self::statuses_generation`] counts — so a waiter can sleep until
    /// one happens instead of polling for it.
    #[must_use]
    pub fn invocation_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.state.invocation_changes()
    }

    /// Sees every published-surface change after this call — what
    /// [`Self::surfaces_generation`] counts — so a waiter for a listing of
    /// these actions can sleep until one happens.
    #[must_use]
    pub fn surface_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.state.surface_changes()
    }

    /// One invocation's current status, only when `actor` owns it.
    #[must_use]
    pub fn status_for_actor(&self, id: u64, actor: &str) -> Option<tilo_actions::InvocationUpdate> {
        self.state
            .while_live(|state| Ok(state.invocation_status_for_actor(id, actor)))
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
    ///
    /// # Errors
    ///
    /// Returns a message when the live broker cannot evaluate the provenance.
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

    /// Connects one local host worker for `route` under `certificate`.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker is unavailable or refuses the worker.
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

    /// Connects one local host worker together with its published-invocation authorizer.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker is unavailable or refuses the worker.
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

    /// Observe complete native room membership after broker admission. The
    /// callback never receives ordinary application Context fanout.
    pub fn observe_certified_people(
        &self,
        sink: CertifiedPeopleSink,
    ) -> Arc<CertifiedPeopleSubscription> {
        self.fanout.subscribe_certified_people(sink)
    }

    /// Exact current owner-backed agent identity for a native participant.
    pub fn certified_agent_identity(
        &self,
        room: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.fanout
            .capnp_state()?
            .certified_agent_identity(room, actor)
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
}
impl Drop for BrokerHandle {
    fn drop(&mut self) {
        // Quit first so a wake from request_stop sees the flag and exits.
        self.capnp_quit.store(true, Ordering::Relaxed);
        self.fanout.request_shutdown();
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
///
/// # Errors
///
/// Returns an I/O error when the broker socket or thread cannot be set up.
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
///
/// # Errors
///
/// Returns an I/O error when the broker socket or thread cannot be set up.
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
        &socket,
        &fanout,
        crate::collab_broker_capnp::BrokerWiring {
            broker_channels: channels.clone(),
            publisher,
            membership,
            trusted_devices,
            services,
            config,
        },
        quit.clone(),
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
///
/// # Errors
///
/// Returns an I/O error when the publisher is not wired or the broker cannot start.
pub fn spawn_native_peer(
    socket: PathBuf,
    membership: crate::services::WorkspaceAuthorityHandle,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: crate::peer_actions::NativePeerBrokerConfig,
) -> std::io::Result<BrokerHandle> {
    spawn_native_peer_with_services(
        socket,
        membership,
        publisher,
        config,
        crate::services::HostServices::default(),
    )
}
/// [`spawn_native_peer`] with explicit host services.
///
/// # Errors
///
/// Returns an I/O error when the publisher is not wired or the broker cannot start.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public constructor contract: callers hand over their configuration"
)]
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

#[cfg(test)]
mod weak_facade_tests {
    use super::*;

    #[test]
    fn weak_native_discovery_does_not_retain_broker_or_accept_after_stop() {
        let broker = CapnpBrokerState::test_broker_with_idle_directory_queue();
        let weak = broker.downgrade();
        assert_eq!(Arc::strong_count(&broker.state), 1);
        let temporary = weak.upgrade().unwrap();
        assert_eq!(Arc::strong_count(&broker.state), 2);
        drop(temporary);
        broker.state.stop();
        assert!(weak.upgrade().is_err());
        drop(broker);
        assert!(weak.upgrade().is_err());
    }
}
