//! Production collaboration routing, application broker facade and lifetime.
use crate::collab_app_broker::AppBrokerState;
pub use crate::collab_core::{
    AuditEntry, BrokerChannels, RedactionRecord, apply_redaction_from_envelope,
};
pub use crate::route::CrdtRoute;
use collab_types::{ArtifactId, ChannelState};
use runtime_collab::event::CollabEvent;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
/// Callback an in-process fanout subscriber registers to receive
/// delivered events. Aliased because the raw `Box<dyn Fn(...)>` shape
/// recurs across [`WorkspaceSubscription`], [`FanoutState::subscribe_workspaces`]
/// and [`BrokerHandle::subscribe_workspaces`], and the last of those is public
/// API other modules call.
pub type FanoutSink = Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>;

/// Explicit inputs for an ordinary native broker host. The owner supplies its
/// real publisher and current room tree separately.
#[derive(Clone)]
pub struct NativePeerBrokerConfig {
    /// Where the durable invocation journal lives.
    pub journal_root: PathBuf,
    /// The owner's current certificate and revocation authority.
    pub certificate_authority: Arc<dyn crate::certificate_authority::CertificateAuthority>,
}

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
    app_broker: Mutex<Option<Arc<AppBrokerState>>>,
    local_subs: Mutex<Vec<Arc<WorkspaceSubscription>>>,
}

/// A subscriber inside the shell process, addressed by workspace — how the
/// compositor reads presence its apps publish. Delivery goes through the
/// same [`CrdtRoute::delivers_to`] check as a typed subscriber, against
/// `CrdtRoute::cluster(workspace)`; since presence widens to cluster scope on
/// the way out, two apps in one workspace reach one subscription, so a cursor
/// crossing between them is one peer moving, not a second appearing.
/// Workspaces are mutable — workspaces are created, and join a peer's workspace
/// when an invite is accepted.
pub struct WorkspaceSubscription {
    workspaces: Mutex<Vec<String>>,
    sink: FanoutSink,
}

impl WorkspaceSubscription {
    /// Replace the set of workspaces this subscription covers.
    pub fn set_workspaces(&self, workspaces: Vec<String>) {
        if let Ok(mut g) = self.workspaces.lock() {
            *g = workspaces;
        }
    }

    /// Whether `route` addresses any workspace this subscription holds. Uses
    /// the broker's own delivery predicate, so it never receives
    /// anything a socket peer in the same workspace wouldn't.
    fn wants(&self, route: &CrdtRoute) -> bool {
        let Ok(workspaces) = self.workspaces.lock() else {
            return false;
        };
        workspaces
            .iter()
            .any(|workspace| route.delivers_to(&CrdtRoute::cluster(workspace.clone())))
    }
}

impl Default for FanoutState {
    fn default() -> Self {
        Self::new()
    }
}

impl FanoutState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            peer_id_counter: AtomicU64::new(0),
            app_broker: Mutex::new(None),
            local_subs: Mutex::new(Vec::new()),
        }
    }

    /// Register an in-process subscriber, returning a handle the caller
    /// can use to keep its workspace set current.
    pub fn subscribe_workspaces(
        &self,
        workspaces: Vec<String>,
        sink: FanoutSink,
    ) -> Arc<WorkspaceSubscription> {
        let sub = Arc::new(WorkspaceSubscription {
            workspaces: Mutex::new(workspaces),
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
    fn local_subs(&self) -> Vec<Arc<WorkspaceSubscription>> {
        self.local_subs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
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

    /// Attach the app broker's fanout sink. Called at most once, at
    /// broker-spawn time; repeat calls are silently ignored.
    pub fn attach_app_broker(&self, state: Arc<AppBrokerState>) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut g) = self.app_broker.lock()
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
    pub(crate) fn shutdown_app_broker(&self, state: &Arc<AppBrokerState>) {
        self.request_shutdown();
        state.stop();
        {
            let mut guard = match self.app_broker.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if guard
                .as_ref()
                .is_some_and(|attached| Arc::ptr_eq(attached, state))
            {
                *guard = None;
            }
        }
        match self.local_subs.lock() {
            Ok(mut subscribers) => subscribers.clear(),
            Err(poisoned) => poisoned.into_inner().clear(),
        }
    }

    pub(crate) fn request_shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let state = self.app_broker.lock().ok().and_then(|state| state.clone());
        if let Some(state) = state {
            state.request_stop();
        }
    }

    /// Read the app broker's fanout sink if one has been attached.
    pub fn app_broker(&self) -> Option<Arc<AppBrokerState>> {
        self.app_broker
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .filter(|state| state.is_accepting())
    }

    /// Whether a local app subscribes to exactly `route`'s documents.
    pub fn holds_route(&self, route: &CrdtRoute) -> bool {
        self.app_broker()
            .is_some_and(|state| state.holds_route(route))
    }

    /// The records local app windows hold, by the window instance the shell
    /// launched each for; see [`AppBrokerState::window_records`].
    pub fn window_records(&self) -> Vec<(u64, CrdtRoute)> {
        self.app_broker()
            .map(|state| state.window_records())
            .unwrap_or_default()
    }

    /// Ask the local apps holding records in `cluster_id` to publish their
    /// snapshots; see [`AppBrokerState::request_workspace_snapshots`].
    pub fn request_workspace_snapshots(&self, cluster_id: &str) -> usize {
        self.app_broker()
            .map_or(0, |state| state.request_workspace_snapshots(cluster_id))
    }

    /// [`Self::fanout_to_route`] for a local app's presence: delivered to
    /// whoever `scope` (the workspace) addresses, while in-process subscribers
    /// are told the `origin` route it came from — which app, which record —
    /// as the shell needs to know what a window under the pointer shows.
    pub fn fanout_presence(
        &self,
        event: &CollabEvent,
        except: u64,
        scope: &CrdtRoute,
        origin: &CrdtRoute,
    ) {
        if self.stopped.load(Ordering::Acquire) || scope.cluster_id.is_empty() {
            return;
        }
        for sub in self.local_subs() {
            if sub.wants(scope) {
                (sub.sink)(event, origin);
            }
        }
        if let Some(app_broker) = self.app_broker() {
            app_broker.fanout_to_route(event, except, scope);
        }
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
        if let Some(app_broker) = self.app_broker() {
            app_broker.fanout_to_route(event, except, route);
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
    pub fn set(&self, workspace: &str, peer_id: Option<String>) {
        self.fanout.fanout_to_route(
            &CollabEvent::FollowChanged { peer_id },
            u64::MAX,
            &CrdtRoute::cluster(workspace.to_string()),
        );
    }

    /// Ask exactly one application instance, addressed by its primary route
    /// in `workspace`, for the next step of moving its window. Session-local like
    /// Follow; the answer arrives through the host's continuation services.
    pub fn request_handoff(
        &self,
        workspace: &str,
        app: &str,
        instance: u64,
        request: collab_types::handoff::HandoffRequest,
    ) {
        self.fanout.fanout_to_route(
            &CollabEvent::Handoff { request },
            u64::MAX,
            &CrdtRoute::record(app, workspace, format!("continuation:{instance}")),
        );
    }
}

/// Cloneable, transport-owning facade used by shell search providers. It
/// keeps only the shared action directory alive, never the broker threads or
/// socket owner.
#[derive(Clone)]
pub struct AppActionBroker {
    state: Arc<AppBrokerState>,
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
struct WeakBrokerDiscovery(std::sync::Weak<AppBrokerState>);
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
    pub fn upgrade(&self) -> Result<AppActionBroker, String> {
        self.0.current()
    }
}

/// Why an app opening in a workspace was not let author there
/// ([`AppActionBroker::grant_app_authorship`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAuthorshipError {
    /// The grant was not asked: the broker is shutting down, or this device
    /// may not ask it in the workspace.
    Unasked(String),
    /// The workspace's owner refused the grant or could not commit it.
    Refused(String),
}

impl std::fmt::Display for AppAuthorshipError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unasked(why) => formatter.write_str(why),
            Self::Refused(why) => write!(formatter, "native app authorship refused: {why}"),
        }
    }
}

impl AppActionBroker {
    /// The host this person's other devices reach this device's actions
    /// through: every request admitted for the caller its own device's
    /// signature named, and followed here like any other invocation.
    #[must_use]
    pub fn device_host(&self) -> Arc<dyn tilo_actions::remote::ActionHost + Send + Sync> {
        crate::collab_app_broker::device_host(Arc::clone(&self.state))
    }

    /// Exact current owner-backed agent identity while the session is live.
    pub fn certified_agent_identity(
        &self,
        workspace: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.state
            .while_live(|state| Ok(state.certified_agent_identity(workspace, actor)))
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
    /// Told whether `workspace`'s membership actions are published, once the
    /// broker's loop has looked (see
    /// `AppBrokerState::await_membership_surface`). Ask before
    /// [`Self::change_workspace_membership`] in a workspace that may have
    /// appeared since the broker last published its workspaces' actions.
    pub fn await_membership_surface(
        &self,
        workspace: &str,
    ) -> Result<std::sync::mpsc::Receiver<bool>, String> {
        self.state
            .while_live(|state| Ok(state.await_membership_surface(workspace)))
    }

    /// Let `app` author in `workspace`, as `certificate`'s person, this device,
    /// opening it there: checked as their membership invocation is, and
    /// committed by the workspace's owner before this returns
    /// (`AppBrokerState::app_authorship_commit`). Blocks on the owner's
    /// commit: call it off the broker's loop and the frame loop.
    ///
    /// #
    /// Errors [`AppAuthorshipError::Unasked`]: the broker is shutting down, or the
    /// device may not ask it in this workspace; [`AppAuthorshipError::Refused`]:
    /// the workspace's owner refused or could not commit it.
    pub fn grant_app_authorship(
        &self,
        workspace: &str,
        app: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), AppAuthorshipError> {
        use AppAuthorshipError::{Refused, Unasked};
        // A workspace this device just made or joined holds its membership here
        // once its replica is projected: told so by the refresh that
        // publishes the workspace's membership, never on a clock.
        let projected = self
            .state
            .while_live(|state| Ok(state.membership_projection(workspace)))
            .map_err(Unasked)?;
        if let Some(published) = projected
            && !published.recv().map_err(|_| {
                Unasked(
                    "the session's broker ended before the workspace's membership was held here"
                        .to_owned(),
                )
            })?
        {
            return Err(Unasked(
                "the workspace's membership is not held on this device, and it neither holds, \
                 joins nor places the workspace"
                    .into(),
            ));
        }
        let (service, request) = self
            .state
            .while_live(|state| state.app_authorship_commit(workspace, app, certificate))
            .map_err(Unasked)?;
        let receipt =
            futures::executor::block_on(service.execute(request.clone())).map_err(Refused)?;
        receipt.validate_for(&request).map_err(Refused)
    }

    pub fn change_workspace_membership(
        &self,
        workspace: &str,
        command: &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<crate::collab_app_broker::InvocationJob, String> {
        self.state
            .while_live(|state| state.invoke_workspace_membership(workspace, command, certificate))
    }

    /// Construct the shell-facing facade around a real broker state for
    /// `LocalSet` integration tests. Production hosts construct this facade
    /// through `BrokerHandle`; arbitrary state injection is test-only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_state_for_test(state: Arc<AppBrokerState>) -> Self {
        Self { state }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_action_directory_roots_for_test(
        &self,
        roots: Vec<crate::collab_app_broker::ActionDirectoryRoot>,
    ) {
        let _ = self.state.while_live(|state| {
            state.replace_action_directory_roots_for_test(roots);
            Ok(())
        });
    }

    /// Publish `surface` for its connection as an app's `publishActions`
    /// does: an unchanged surface moves nothing.
    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_surface_for_test(&self, surface: crate::collab_app_broker::PublishedSurface) {
        let _ = self.state.while_live(|state| {
            state.publish_surface_for_test(surface);
            Ok(())
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_published_surfaces_for_test(
        &self,
        surfaces: Vec<crate::collab_app_broker::PublishedSurface>,
    ) {
        let _ = self.state.while_live(|state| {
            state.replace_published_surfaces_for_test(surfaces);
            Ok(())
        });
    }

    /// Bounded, nonblocking bridge from shell chrome into the broker's
    /// `LocalSet`-owned application directory capabilities. Palette code must
    /// poll the returned oneshot in its cache owner; it must never block a
    /// compositor frame on a broker round trip.
    #[must_use]
    pub fn action_directory_request_queue(
        &self,
    ) -> crate::collab_app_broker::ActionDirectoryRequestQueue {
        self.state
            .while_live(|state| Ok(state.action_directory_request_queue()))
            .unwrap_or_else(|_| {
                crate::collab_app_broker::ActionDirectoryRequestQueue::unavailable()
            })
    }

    /// A local action door bound to an already-issued signed workspace admission and
    /// one host-issued execution. No device is enrolled and no grant is minted.
    pub fn certified_local_action_host(
        &self,
        caller: &tilo_actions::remote::Caller,
        presentation: &[u8],
    ) -> Result<Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>, String> {
        self.state.certified_local_action_host(caller, presentation)
    }

    /// [`Self::certified_local_action_host`] handed to agent `agent` by the
    /// person's own device; see
    /// [`AppBrokerState::certified_local_agent_host`].
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

    /// Reuse the checked signed personal-cluster admission for native owner checks.
    pub fn certified_person_context(
        &self,
        caller: &tilo_actions::remote::Caller,
        presentation: &[u8],
    ) -> Result<crate::services::CertifiedPersonContext, String> {
        self.state.certified_person_context(caller, presentation)
    }

    /// The durable invocation host this broker serves, while it is live.
    pub fn invocation_host(&self) -> Result<uuid::Uuid, String> {
        self.state
            .while_live_fenced(AppBrokerState::invocation_host)
    }

    pub fn surfaces(&self) -> Vec<crate::collab_app_broker::PublishedSurface> {
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
        crate::collab_app_broker::PublishedSurface,
        crate::collab_app_broker::PublishedAction,
    )> {
        self.state
            .while_live(|state| Ok(state.published_action(route, resource, action)))
            .ok()
            .flatten()
    }

    /// The connection (`PublishedSurface::peer_id`) of the local app window
    /// the shell launched for window instance `instance`, while it is
    /// connected: where an action meant for that one window goes.
    #[must_use]
    pub fn window_peer(&self, instance: u64) -> Option<u64> {
        self.state
            .while_live(|state| Ok(state.window_peer(instance)))
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
    pub fn action_directory_roots(&self) -> Vec<crate::collab_app_broker::ActionDirectoryRoot> {
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
        workspace: &str,
        instance: u64,
        pid: u32,
        start: u64,
    ) -> Option<crate::collab_app_broker::ActionDirectoryRoot> {
        self.state
            .while_live(|state| Ok(state.native_checkout(app, workspace, instance, pid, start)))
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
    ) -> crate::collab_app_broker::ActionDirectoryRootPage {
        self.state
            .while_live(|state| Ok(state.action_directory_root_page(after_peer, max_results)))
            .unwrap_or_default()
    }

    /// Invoke one published application action.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<crate::collab_app_broker::InvocationCompletion>, String>
    {
        self.state
            .while_live(|state| state.invoke(peer_id, resource, action, argument, actor))
    }

    /// Start a published action and return its lifecycle job.
    pub fn start(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<crate::collab_app_broker::InvocationJob, String> {
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
    pub fn from_state(state: Arc<AppBrokerState>) -> Self {
        Self { state }
    }

    /// The broker state behind this facade, for the host's action host.
    #[must_use]
    pub fn state(&self) -> &Arc<AppBrokerState> {
        &self.state
    }

    /// Admit app `caller_app`'s request to open `target` in `workspace` for the
    /// running invocation `parent` the broker delivered to it
    /// ([`AppBrokerState::admit_app_open`]).
    ///
    /// # Errors
    ///
    /// As [`AppBrokerState::admit_app_open`].
    pub fn admit_app_open(
        &self,
        caller_app: &str,
        workspace: &str,
        parent: tilo_actions::InvocationId,
        target: &str,
    ) -> Result<crate::collab_app_broker::AppOpen, String> {
        self.state
            .admit_app_open(caller_app, workspace, parent, target)
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

    /// The shortcut runs going in `workspace`: what the workspace says is
    /// running, and what can be stopped.
    #[must_use]
    pub fn running_shortcut_runs(&self, workspace: &str) -> Vec<crate::automation::RunningRun> {
        self.state.running_shortcut_runs(workspace)
    }

    /// Stop the shortcut runs `request` names in `workspace`, before their next
    /// step. What they already did stands and stays undoable as one step.
    #[must_use]
    pub fn stop_shortcut_runs(
        &self,
        workspace: &str,
        request: &crate::automation::StopRequest,
        reason: &str,
    ) -> Vec<crate::automation::RunningRun> {
        self.state.stop_shortcut_runs(workspace, request, reason)
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

    /// Run saved shortcut `shortcut` in `workspace` for `caller`, who is at the
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
        workspace: &str,
        shortcut: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, tilo_actions::remote::ActionHostError> {
        self.state
            .start_interactive_shortcut_run(caller, workspace, shortcut, inputs)
    }

    /// The oldest step of an interactive shortcut run waiting for its
    /// person.
    #[must_use]
    pub fn take_step_prompt(&self) -> Option<crate::automation::PromptedStep> {
        self.state.take_step_prompt()
    }

    /// Start saved shortcut `shortcut` in `workspace` because its `trigger`
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
        workspace: &str,
        shortcut: &str,
        trigger: &tilo_automation::TriggerSpec,
        run: tilo_automation::RunId,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, tilo_actions::remote::ActionHostError> {
        self.state
            .start_triggered_shortcut_run(caller, workspace, shortcut, trigger, run, inputs)
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

    /// Name `peer_id` the connection of the window launched as `instance`,
    /// as the launcher's handoff does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn bind_window_instance_for_test(&self, peer_id: u64, instance: u64) {
        self.state.bind_window_instance_for_test(peer_id, instance);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_invocations_for_test(
        &self,
        peer_id: u64,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::collab_app_broker::InvocationEnvelope> {
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

    pub fn statuses(&self, actor: &str) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.state
            .while_live(|state| Ok(state.invocation_statuses(actor)))
            .unwrap_or_default()
    }

    /// Request cooperative cancellation for an owned invocation, answered
    /// once the request is durable. Blocks the calling thread for that
    /// write, so never call it on the broker's loop.
    pub fn cancel(&self, id: u64, actor: &str) -> Result<(), String> {
        self.state
            .while_live(|state| state.request_cancellation(id, actor))?
            .wait_blocking()
    }

    pub fn can_cancel(&self, id: u64, actor: &str) -> bool {
        self.state
            .while_live(|state| Ok(state.can_cancel(id, actor)))
            .unwrap_or(false)
    }

    /// True only while the named admitted agent has a live task subscription.
    pub fn agent_is_live_task_capable(&self, workspace: &str, agent: &str) -> bool {
        self.state
            .while_live(|state| Ok(state.agent_is_live_task_capable(workspace, agent)))
            .unwrap_or(false)
    }

    /// Prepare a one-use confirmation token for the exact invocation inputs.
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
    pub fn start_confirmed(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        token: &str,
    ) -> Result<crate::collab_app_broker::InvocationJob, String> {
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
    app_socket_path: PathBuf,
    app_thread: Option<JoinHandle<()>>,
    app_identity: Option<usize>,
    app_quit: Arc<AtomicBool>,
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
        match self.fanout.app_broker() {
            Some(state) => state.is_current_host_worker(peer, route, certificate),
            None => Ok(false),
        }
    }

    /// Connects one local host worker for `route` under `certificate`.
    pub fn connect_local_worker(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.fanout
            .app_broker()
            .ok_or("broker unavailable")?
            .connect_local_worker(route, certificate)
    }

    /// Connects one local host worker together with its published-invocation authorizer.
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
            .app_broker()
            .ok_or("broker unavailable")?
            .connect_local_worker_with_authorizer(route, certificate)
    }

    /// Cloneable handle for the live resource/action search provider.
    pub fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.fanout
            .app_broker()
            .map(|state| AppActionBroker { state })
    }

    #[must_use]
    pub fn follow_dispatcher(&self) -> SessionFollowDispatcher {
        SessionFollowDispatcher {
            fanout: Arc::clone(&self.fanout),
        }
    }

    /// Subscribe this process to the workspaces it hosts. The sink runs on
    /// the reader thread that decoded the frame: keep it short.
    pub fn subscribe_workspaces(
        &self,
        workspaces: Vec<String>,
        sink: FanoutSink,
    ) -> Arc<WorkspaceSubscription> {
        self.fanout.subscribe_workspaces(workspaces, sink)
    }

    /// Exact current owner-backed agent identity for a native participant.
    pub fn certified_agent_identity(
        &self,
        workspace: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.fanout
            .app_broker()?
            .certified_agent_identity(workspace, actor)
    }

    /// The bound collaboration socket.
    pub fn app_socket(&self) -> &std::path::Path {
        &self.app_socket_path
    }

    /// Shared handle to per-channel state + audit log. Tests use this
    /// to install a [`ChannelState`] before sending signed envelopes.
    pub fn channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.channels)
    }

    /// Snapshot the resource/action surfaces live applications published to
    /// this session. The values are inert; invoking is a separate operation.
    pub fn published_action_surfaces(&self) -> Vec<crate::collab_app_broker::PublishedSurface> {
        self.fanout
            .app_broker()
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
        self.fanout.app_broker().is_some_and(|state| {
            state.has_live_published_action(app_id, cluster_id, record_id, resource, action)
        })
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
        self.app_quit.store(true, Ordering::Relaxed);
        self.fanout.request_shutdown();
        if let Some(thread) = self.app_thread.take() {
            join_owned_thread(thread, self.app_identity);
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
    config: Option<&crate::NativePeerBrokerConfig>,
    services: crate::services::HostServices,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> std::io::Result<BrokerHandle> {
    spawn_inner(
        socket,
        membership,
        publisher,
        config,
        services,
        HarnessAdmission::default(),
        channels,
        fanout,
    )
}

/// How a harness broker admits devices instead of a session's identity
/// store: its enrolled devices, and the authority that admits them.
#[derive(Default)]
struct HarnessAdmission {
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    certificate_authority: Option<Arc<dyn crate::certificate_authority::CertificateAuthority>>,
}

/// Native compatibility fixtures only; production callers inject current authority.
#[cfg(any(test, feature = "test-support"))]
pub fn spawn_with_test_services(
    socket: PathBuf,
    membership: Option<crate::services::WorkspaceAuthorityHandle>,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: Option<&crate::NativePeerBrokerConfig>,
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
        HarnessAdmission {
            trusted_devices,
            certificate_authority: None,
        },
        channels,
        fanout,
    )
}

fn spawn_inner(
    socket: PathBuf,
    membership: Option<crate::services::WorkspaceAuthorityHandle>,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: Option<&crate::NativePeerBrokerConfig>,
    services: crate::services::HostServices,
    harness: HarnessAdmission,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> std::io::Result<BrokerHandle> {
    let HarnessAdmission {
        trusted_devices,
        certificate_authority,
    } = harness;
    let quit = Arc::new(AtomicBool::new(false));
    let thread = crate::collab_app_broker::spawn_configured(
        &socket,
        &fanout,
        crate::collab_app_broker::BrokerWiring {
            broker_channels: channels.clone(),
            publisher,
            membership,
            trusted_devices,
            certificate_authority,
            services,
            config,
        },
        quit.clone(),
    )?;
    let app_identity = fanout.app_broker().map(|state| Arc::as_ptr(&state).addr());
    Ok(BrokerHandle {
        app_socket_path: socket,
        app_thread: Some(thread),
        app_identity,
        app_quit: quit,
        fanout,
        channels,
    })
}

/// Ordinary peer production constructor. All trust and durability belong to the caller's owner.
pub fn spawn_native_peer(
    socket: PathBuf,
    membership: crate::services::WorkspaceAuthorityHandle,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: crate::NativePeerBrokerConfig,
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
#[expect(
    clippy::needless_pass_by_value,
    reason = "public constructor contract: callers hand over their configuration"
)]
pub fn spawn_native_peer_with_services(
    socket: PathBuf,
    membership: crate::services::WorkspaceAuthorityHandle,
    publisher: Arc<crate::collab_outbound::PeerPublisher>,
    config: crate::NativePeerBrokerConfig,
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
        let broker = AppBrokerState::test_broker_with_idle_directory_queue();
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

#[cfg(test)]
mod presence_origin_tests {
    use super::*;

    /// The shell learns which record a window shows from its app pointing
    /// in it: cursor chat over that window is sealed for and placed in that
    /// record. A local app's presence is delivered workspace-wide, and the
    /// shell's subscription is told the app and record it came from.
    #[test]
    fn a_local_apps_presence_names_the_app_and_record_it_came_from() {
        let fanout = FanoutState::new();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let _shell = fanout.subscribe_workspaces(
            vec!["workspace".into()],
            Box::new(move |_, route| sink.lock().unwrap().push(route.clone())),
        );
        let origin = CrdtRoute::record("so.tilo.chat", "workspace", "derived:main");
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: "peer@1".into(),
            key: ui::presence::PresenceKey::pointer_default(),
            payload: ui::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
        };
        fanout.fanout_presence(&event, 0, &origin.to_cluster_scope(), &origin);
        drop(fanout);
        let heard = heard.lock().unwrap().clone();
        assert_eq!(heard, vec![origin]);
    }
}
