//! Actions between the devices of a workspace, over a [`Transport`]: an iroh
//! connection of its own, ALPN [`ACTIONS_ALPN`], or the link a page holds to
//! its host ([`tilo_server_protocol::web::LINK_ACTIONS_PATH`]).
//!
//! A request is one signed [`SignedDeviceAction`] on a stream of its own, and
//! its answer is the [`RemoteActionMessage`] the device's one dispatcher
//! ([`ActionProvider`](tilo_actions::remote::ActionProvider): the broker, or a
//! runtime's action host) gives, on the same bi-stream. The stream is the
//! correlation: a request in, one answer out, then the stream finishes, so
//! nothing has to be matched up afterwards and a lost answer is a lost
//! request, never another caller's.
//!
//! This serves a device, not a replica: one request names its workspace, and
//! own-device actions span the person. The device answering checks, in this
//! order, that
//!
//! 1. the transport's device is the action's `source`, where the transport
//!    names one, so a device may only ask as itself;
//! 2. the signature is the source's ([`SignedDeviceAction::verify`]);
//! 3. the invocation the request carries, if it carries one, is signed as
//!    that same device ([`verify_invocation`]), is fresh, and its id names
//!    no other request this device admitted within the window
//!    (`InvocationReplay`: [`ActionCallError::InvocationConflict`]
//!    otherwise);
//! 4. the addressed device is this one — a request addressed to another
//!    device this one holds a transport for is passed on there
//!    ([`ActionService::reach_device_at`]);
//! 5. the request is fresh and not one already admitted
//!    ([`ActionReplayWindow`]);
//! 6. this device holds the workspace it names;
//! 7. the source is a device of that workspace's roster: certified,
//!    unrevoked, unexpired, its person a member, its delegation live
//!    ([`StatePeer::permissions`]); or a key the workspace holds no certificate
//!    for at all, a visitor ([`Caller::visitor`]), which acts for nobody.
//!
//! A transport that names no device of its own (a page's session with its
//! host, which proves the page to the host and the host to nobody) admits a
//! request by what the request itself proves: its signature, and the
//! invocation it carries. Such a transport carries another device's request
//! only to the host whose session it is, which is the one its opener can
//! reach, so a request passed on to a page runs as the device that signed it
//! there, not as the host that carried it.
//!
//! A visitor (a page at a workspace's public entry, which made a key of its own
//! and signs each call with it) is admitted only to what the workspace's grants
//! open to any key ([`tilo_action_policy::EntryPolicy::visitor_grants`]),
//! decided here before any dispatcher meets it, and then handed to the
//! dispatcher like any caller.
//! Its calls are counted per client address before any provider runs
//! ([`visitors`]).
//!
//! One request is admitted before the device holds any certificate: a computer
//! being added to a person's devices asks the device that issued its ticket to
//! certify it ([`ActionService::admit_uncertified`]). Such a request names that
//! one scope instead of a workspace, checks 1 to 4 hold for it as for any
//! other, and it is answered by the provider that serves the scope alone, as
//! a [`Caller::visitor`].
//!
//! What is left is the [`Caller`] the local callers of the same dispatcher
//! get, and the dispatcher's own policy
//! ([`tilo_action_policy`](tilo_actions::remote)) is the only policy check.
//! Nothing is queued: a device that cannot be reached answers
//! [`ActionCallError::Unavailable`] at once, and the caller decides what to do
//! about it. Nothing is remembered here either: the dispatcher's durable
//! journal is the one log, keyed by invocation, so a retried request
//! reports the original invocation.

use crate::direct::Replica;
use crate::transport::{StreamRecv, StreamSend, Transport};
use collab_types::signer::DeviceSigner;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::actions::{
    ActionReplayWindow, ActionServiceFrame, DeviceAction, SignedDeviceAction,
};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_action_policy::{Actor, MemberId};
use tilo_actions::remote::{
    ActionProvider, Caller, ExecutionLease, InvocationReceipt, InvocationState, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, UpdateMark,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::invocation::InvocationReplay;
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

mod visitors;
pub use visitors::{VISITOR_CALLS_PER_DAY, VISITOR_CALLS_PER_TEN_MINUTES};

/// ALPN of the device-to-device action service, and of a page's session for
/// it ([`tilo_server_protocol::web::LINK_ACTIONS_PATH`]).
pub use tilo_server_protocol::transport::ACTIONS_ALPN;

/// Where a page opens its session to ask its host's actions: the same
/// streams [`ACTIONS_ALPN`] carries, over the browser's WebTransport.
pub use tilo_server_protocol::web::LINK_ACTIONS_PATH;

/// The largest frame either side writes.
const MAX_FRAME_BYTES: usize = neighbour_wire::actions::MAX_ACTION_BYTES + 8 * 1024;

/// How long a device is given to answer a request it was asked directly. A
/// device that is not reachable at all fails faster than this; one that is
/// reachable and does not answer is given up on, so the caller learns.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(30);

/// How long one long-poll asks the answering device to hold its answer;
/// well inside [`REPLY_DEADLINE`], so a held answer is never taken for a
/// lost one.
pub const CHANGE_WAIT_MS: u32 = 10_000;

/// How long an invocation is followed on another device before it is given
/// up as unanswered.
pub const FOLLOW_DEADLINE: Duration = Duration::from_secs(120);

/// How many changing requests one person may send per window, and how many
/// at once. Reading (a catalogue, an observation) spends none: a caller that
/// is following work must keep being able to look at it.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Changes refilled per window.
    pub changes_per_window: u32,
    /// The window, in seconds.
    pub window_secs: u32,
    /// Changes a person may send at once.
    pub burst: u32,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            changes_per_window: 20,
            window_secs: 10,
            burst: 40,
        }
    }
}

/// Buckets kept before idle, full ones are forgotten.
const KEPT_CALLERS: usize = 4096;

/// Most invocations one device follows at a time.
const MAX_FOLLOWED: usize = 1024;

/// The workspaces a device answers for, as the action service reads them.
///
/// A device whose workspaces are held in a store of its own (a session's
/// workspace owner) hands the action service this view of them instead of
/// [`Replica`]s: the action service then reads a workspace's state when a request names
/// it, so a revocation, a removal or a new certificate is seen as it
/// happens rather than as it was when the action service was handed the workspace.
///
/// A workspace a view keeps behind a lock of its own (a host holding its
/// workspaces for their members) is locked for the read, so what is read is
/// the state that holds then: a copy of the workspace taken out from under
/// the lock would stop seeing its roster change, because
/// [`StatePeer`](crate::StatePeer)'s maps are written copy-on-write.
pub trait Workspaces: Send + Sync {
    /// Read the state of `workspace`, if this device holds it: whether it
    /// did.
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&crate::StatePeer) + Send),
    ) -> ReadFuture<'a>;

    /// [`Self::read`] for a request the device `asker` signed, which the
    /// action service is deciding: a view that comes to hold a workspace on a
    /// device's request may do so here, and only here, for the devices it
    /// trusts to ask, from `placement`, the address the asker signed for the
    /// workspace when it sent one. Every other read only reads.
    fn read_for<'a>(
        &'a self,
        asker: [u8; 32],
        placement: Option<&'a crate::join::TransportHint>,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&crate::StatePeer) + Send),
    ) -> ReadFuture<'a> {
        let _ = (asker, placement);
        self.read(workspace, read)
    }
}

/// What a [`Workspaces`] view answers a read with: a future of its own,
/// because a view may wait for a workspace it keeps behind an async lock
/// before it can read it.
pub type ReadFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// The workspaces a device's own native enrollment holds, as the action service reads
/// them: the store a session opens its workspaces in is asked when a request
/// names one, so who may act there is what holds at that moment rather than as
/// it was when the action service started.
impl Workspaces for crate::owner::NativeWorkspaceOwner {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&crate::StatePeer) + Send),
    ) -> ReadFuture<'a> {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(workspace) else {
            return Box::pin(std::future::ready(false));
        };
        // The owner's own store: the read takes its lock for as long as it
        // runs, never across an await.
        Box::pin(async move {
            self.with_current(workspace, |view| {
                read(view.peer());
                Ok(())
            })
            .is_ok()
        })
    }
}

/// A weak handle to an [`ActionService`], for a provider registered on it that
/// also asks other devices through it: neither keeps the other alive.
#[derive(Clone)]
pub struct WeakActions(std::sync::Weak<Inner>);

impl WeakActions {
    /// The service, while it runs.
    #[must_use]
    pub fn upgrade(&self) -> Option<ActionService> {
        self.0.upgrade().map(ActionService)
    }
}

/// A device that answers no app actions on its action service: one built to ask its
/// workspaces' devices to take work, which serves the action service for the asks it
/// makes. An app's action reaches a device through
/// the dispatcher it serves; a request that arrives on such an action service anyway is
/// refused as one this device does not serve, so its caller learns at once
/// instead of waiting.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoActions;

/// What [`NoActions`] answers every request with.
fn not_served() -> tilo_actions::remote::ActionCallError {
    tilo_actions::remote::ActionCallError::Unavailable {
        reason: "this device answers no app actions on the devices' action service".into(),
    }
}

impl ActionProvider for NoActions {
    fn query(
        &self,
        _caller: &Caller,
        _query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(not_served()))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(not_served()))
    }
}

/// A device's action service: it serves the requests other devices of its
/// workspaces make, and asks them through [`Self::remote`].
#[derive(Clone)]
pub struct ActionService(Arc<Inner>);

struct Inner {
    /// The device whose signature every request it makes carries.
    device: Arc<dyn DeviceSigner>,
    /// The workspaces this device holds, by workspace id.
    replicas: Mutex<BTreeMap<String, Replica>>,
    /// Where this device keeps its workspaces, when they are not [`Replica`]s
    /// here ([`Workspaces`]).
    workspaces: Option<Arc<dyn Workspaces>>,
    /// The one place a request is handed to, wherever it came from.
    dispatcher: Arc<dyn ActionProvider + Send + Sync>,
    /// Providers this device answers beside its dispatcher, each the actions of the provider it
    /// names ([`ActionService::add_provider`]).
    providers: Mutex<Vec<Arc<dyn ActionProvider + Send + Sync>>>,
    /// The scopes a device with no certificate yet may call, and what answers each
    /// ([`ActionService::admit_uncertified`]).
    uncertified: Mutex<HashMap<String, Arc<dyn ActionProvider + Send + Sync>>>,
    /// What answers requests naming no workspace, the host's own operations
    /// ([`ActionService::admit_host_operations`]).
    host_operations: Mutex<Option<Arc<dyn ActionProvider + Send + Sync>>>,
    replay: Mutex<ActionReplayWindow>,
    /// The calls visitors made, per client address ([`visitors`]).
    visitors: Mutex<visitors::VisitorWindows>,
    /// The uploads callers keep open between the steps of their content puts.
    content_puts: tilo_actions::remote::HostedContentPuts,
    bounds: Bounds,
    callers: Mutex<HashMap<[u8; 32], Bucket>>,
    /// Endpoints keyed apart from the device they answer for, by endpoint
    /// id ([`ActionService::keyed_apart`]).
    endpoints: Mutex<HashMap<[u8; 32], [u8; 32]>>,
    /// The transports this device reaches other devices at, by device: the
    /// links they hold open to it, which it opens a bi-stream on as it dials a
    /// native device ([`ActionService::reach_device_at`]).
    held: Mutex<HashMap<[u8; 32], Arc<dyn Transport>>>,
    /// The invocation ids each workspace's requests admitted within their
    /// freshness window, checked before anything is dispatched.
    invocations: Mutex<HashMap<String, InvocationReplay>>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for ActionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionService")
            .field("device", &hex::encode(self.0.device.device_public_key()))
            .field("workspaces", &self.0.replicas.lock_recover().len())
            .finish_non_exhaustive()
    }
}

impl ActionService {
    /// An action service for the device whose signing key is `device` (and whose iroh
    /// endpoint's secret key it is), handing every request it answers to
    /// `dispatcher`.
    ///
    /// Call it where a runtime runs: answers are followed on that runtime,
    /// so a caller never blocks on one.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    #[must_use]
    pub fn new(
        device: impl DeviceSigner + 'static,
        dispatcher: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Self {
        Self::with_bounds(device, dispatcher, Bounds::default())
    }

    /// [`Self::new`] with per-person bounds of the caller's choosing.
    #[must_use]
    pub fn with_bounds(
        device: impl DeviceSigner + 'static,
        dispatcher: Arc<dyn ActionProvider + Send + Sync>,
        bounds: Bounds,
    ) -> Self {
        Self(Arc::new(Inner {
            device: Arc::new(device),
            replicas: Mutex::default(),
            workspaces: None,
            dispatcher,
            providers: Mutex::default(),
            uncertified: Mutex::default(),
            host_operations: Mutex::default(),
            replay: Mutex::default(),
            visitors: Mutex::new(visitors::VisitorWindows::new(rand_key())),
            content_puts: tilo_actions::remote::HostedContentPuts::default(),
            bounds,
            callers: Mutex::default(),
            endpoints: Mutex::default(),
            held: Mutex::default(),
            invocations: Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// [`Self::new`] for a device whose workspaces are held in `workspaces`
    /// rather than as [`Replica`]s here.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    #[must_use]
    pub fn over(
        device: impl DeviceSigner + 'static,
        dispatcher: Arc<dyn ActionProvider + Send + Sync>,
        workspaces: Arc<dyn Workspaces>,
    ) -> Self {
        Self(Arc::new(Inner {
            device: Arc::new(device),
            replicas: Mutex::default(),
            workspaces: Some(workspaces),
            dispatcher,
            providers: Mutex::default(),
            uncertified: Mutex::default(),
            host_operations: Mutex::default(),
            replay: Mutex::default(),
            visitors: Mutex::new(visitors::VisitorWindows::new(rand_key())),
            content_puts: tilo_actions::remote::HostedContentPuts::default(),
            bounds: Bounds::default(),
            callers: Mutex::default(),
            endpoints: Mutex::default(),
            held: Mutex::default(),
            invocations: Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// Record `transport`, the link the device `device` holds open to this
    /// one, as the way to reach that device: a request addressed to it is
    /// passed on there ([`Self::pass_on`]), and this device opens a bi-stream
    /// on it as it would dial a native device ([`Self::remote_held`]).
    pub fn reach_device_at(&self, device: [u8; 32], transport: Arc<dyn Transport>) {
        self.0.held.lock_recover().insert(device, transport);
    }

    /// Forget the way to `device` recorded by [`Self::reach_device_at`], when
    /// its link is the one that ended: a link this device kept before it is
    /// left as it was.
    pub fn forget_device(&self, device: [u8; 32], transport: &Arc<dyn Transport>) {
        let mut held = self.0.held.lock_recover();
        if held
            .get(&device)
            .is_some_and(|kept| Arc::ptr_eq(kept, transport))
        {
            held.remove(&device);
        }
    }

    /// The link this device reaches `device` at, when it holds one.
    #[must_use]
    pub fn reach(&self, device: [u8; 32]) -> Option<Arc<dyn Transport>> {
        self.0.held.lock_recover().get(&device).cloned()
    }

    /// A device whose endpoint is keyed apart from its device key, as a Tilo
    /// server's is, asks and answers at that endpoint as the device
    /// ([`Self::device_of`]). Every other endpoint is the device it is keyed
    /// by.
    pub fn keyed_apart(&self, device: [u8; 32], endpoint: [u8; 32]) {
        if device != endpoint {
            self.0.endpoints.lock_recover().insert(endpoint, device);
        }
    }

    /// The device a connection from `endpoint` asks as: on this device's other protocols
    /// too, for whoever serves them beside its actions.
    #[must_use]
    pub fn device_of(&self, endpoint: [u8; 32]) -> [u8; 32] {
        self.0
            .endpoints
            .lock_recover()
            .get(&endpoint)
            .copied()
            .unwrap_or(endpoint)
    }

    /// A handle that does not keep this service alive
    /// ([`WeakActions::upgrade`]): what a provider it answers through holds.
    #[must_use]
    pub fn weak(&self) -> WeakActions {
        WeakActions(Arc::downgrade(&self.0))
    }

    /// This device's public key, as the other devices see it.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.device.device_public_key()
    }

    /// Answer requests for `replica`'s workspace.
    pub fn hold(&self, replica: Replica) {
        self.0
            .replicas
            .lock_recover()
            .insert(replica.workspace(), replica);
    }

    /// The certificates on `workspace`'s roster as this device holds it, the one it decides who
    /// may ask by; `None` where it holds no such workspace.
    pub async fn certificates(
        &self,
        workspace: &str,
    ) -> Option<Vec<tilo_user_identity::DeviceCert>> {
        self.read_workspace_for(None, workspace, crate::StatePeer::certificates)
            .await
    }

    /// Answer requests for the workspace `sync` syncs.
    pub fn share(&self, sync: &crate::direct::DirectSync) {
        self.hold(sync.replica());
    }

    /// Answer requests naming `scope` (no workspace) from a device that holds no certificate
    /// yet, through `provider` alone: what a device issuing a ticket serves while a computer
    /// asks it to certify that computer. The provider sees each such caller as a
    /// [`Caller::visitor`] and answers only the call it serves.
    pub fn admit_uncertified(
        &self,
        scope: impl Into<String>,
        provider: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.0
            .uncertified
            .lock_recover()
            .insert(scope.into(), provider);
    }

    /// Answer requests that name no workspace, the host's own operations, through `provider`: a
    /// device of the host's person asks them, and proves who it is by the signed invocation it
    /// carries, since no workspace's roster names it here. A link admits them before any
    /// workspace is read, so a device that holds none yet still reaches the host that serves it.
    pub fn admit_host_operations(&self, provider: Arc<dyn ActionProvider + Send + Sync>) {
        *self.0.host_operations.lock_recover() = Some(provider);
    }

    /// Answer the actions published under the provider `provider` names
    /// ([`ActionProvider::serves`]) through it, beside what this device's dispatcher answers: a
    /// host holding a ticket for a page serves `tilo.device`'s actions on the device action
    /// service a certified device reaches, where its workspaces' provider answers its own.
    pub fn add_provider(&self, provider: Arc<dyn ActionProvider + Send + Sync>) {
        self.0.providers.lock_recover().push(provider);
    }

    /// What answers the actions published under `provider`: the dispatcher where it serves
    /// them, else the first provider this device serves beside it that does.
    fn provider_for(&self, provider: &str) -> Arc<dyn ActionProvider + Send + Sync> {
        if self.0.dispatcher.serves(provider) {
            return Arc::clone(&self.0.dispatcher);
        }
        self.0
            .providers
            .lock_recover()
            .iter()
            .find(|served| served.serves(provider))
            .map_or_else(|| Arc::clone(&self.0.dispatcher), Arc::clone)
    }

    /// The provider that alone answers a request [`Self::authorize`] handed to `dispatcher`: one
    /// admitted for a device with no certificate yet ([`Self::admit_uncertified`]) answers every
    /// request of its caller, and no provider beside it does.
    fn alone(
        &self,
        dispatcher: &Arc<dyn ActionProvider + Send + Sync>,
    ) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        (!Arc::ptr_eq(dispatcher, &self.0.dispatcher)).then(|| Arc::clone(dispatcher))
    }

    /// The content store a content put goes to: the one provider [`Self::authorize`] handed the
    /// request to, else the dispatcher's, else the first provider beside it that keeps one.
    fn content_puts(
        &self,
        dispatcher: &Arc<dyn ActionProvider + Send + Sync>,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        if let Some(alone) = self.alone(dispatcher) {
            return alone.content_puts();
        }
        self.0.dispatcher.content_puts().or_else(|| {
            self.0
                .providers
                .lock_recover()
                .iter()
                .find_map(tilo_actions::remote::ActionProvider::content_puts)
        })
    }

    /// One step of bytes a caller hands the content store `dispatcher` reaches. A step writes at
    /// most one piece: short enough to answer in place.
    fn put_content(
        &self,
        dispatcher: &Arc<dyn ActionProvider + Send + Sync>,
        caller: &Caller,
        step: &tilo_actions::remote::ContentPutStep,
    ) -> Result<tilo_actions::remote::ContentPutAnswer, tilo_actions::remote::ActionCallError> {
        let Some(puts) = self.content_puts(dispatcher) else {
            return Err(tilo_actions::remote::ActionCallError::Unavailable {
                reason: tilo_actions::remote::CONTENT_PUTS_UNAVAILABLE.into(),
            });
        };
        self.0
            .content_puts
            .step(puts.as_ref(), caller, step.clone())
    }

    /// What owns invocation `id` for `caller`: the dispatcher where it keeps the invocation,
    /// else the first provider beside it that does. A provider that observes an invocation is the
    /// one that ran it, so what follows or cancels it is asked there.
    fn owner(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Arc<dyn ActionProvider + Send + Sync> {
        if self.0.dispatcher.observe(caller, id).is_some() {
            return Arc::clone(&self.0.dispatcher);
        }
        self.0
            .providers
            .lock_recover()
            .iter()
            .find(|served| served.observe(caller, id).is_some())
            .map_or_else(|| Arc::clone(&self.0.dispatcher), Arc::clone)
    }

    /// Read one workspace's state, from wherever this device holds it: a
    /// replica handed to [`Self::hold`], else what [`Workspaces`] says of
    /// it.
    async fn read_workspace<R: Send>(
        &self,
        workspace: &str,
        read: impl FnMut(&crate::StatePeer) -> R + Send,
    ) -> Option<R> {
        self.read_workspace_for(None, workspace, read).await
    }

    /// [`Self::read_workspace`] while deciding a request `asker` signed, with the
    /// placement it carried ([`Workspaces::read_for`]).
    async fn read_workspace_for<R: Send>(
        &self,
        asker: Option<([u8; 32], Option<&crate::join::TransportHint>)>,
        workspace: &str,
        mut read: impl FnMut(&crate::StatePeer) -> R + Send,
    ) -> Option<R> {
        let held = self.0.replicas.lock_recover().get(workspace).cloned();
        if let Some(replica) = held {
            return Some(replica.read(|peer| read(peer)));
        }
        let workspaces = self.0.workspaces.as_ref()?;
        let mut answer = None;
        let mut reading = |peer: &crate::StatePeer| answer = Some(read(peer));
        let held = match asker {
            Some((asker, placement)) => {
                workspaces
                    .read_for(asker, placement, workspace, &mut reading)
                    .await
            }
            None => workspaces.read(workspace, &mut reading).await,
        };
        held.then_some(()).and(answer)
    }

    /// A host for another device's actions in `workspace`, asked as this
    /// device: every callers' [`ActionProvider`] methods work through it as
    /// through a local host.
    #[must_use]
    pub fn remote(
        &self,
        endpoint: &Endpoint,
        addr: &EndpointAddr,
        workspace: &str,
    ) -> RemoteDevice {
        RemoteDevice(self.device_at(endpoint, addr, workspace, None, None))
    }

    /// [`Self::remote`] for a device this device places `workspace`'s work on: every request
    /// carries `placement`, this device's signed address for the workspace, so a device that does
    /// not hold the workspace yet holds it from here ([`Workspaces::read_for`]).
    #[must_use]
    pub fn placing(
        &self,
        endpoint: &Endpoint,
        addr: &EndpointAddr,
        workspace: &str,
        placement: &crate::join::TransportHint,
    ) -> RemoteDevice {
        let placement = postcard::to_allocvec(placement).ok();
        RemoteDevice(self.device_at(endpoint, addr, workspace, placement, None))
    }

    /// The connection to one device, opened on first use and kept while it
    /// lasts.
    fn device_at(
        &self,
        endpoint: &Endpoint,
        addr: &EndpointAddr,
        workspace: &str,
        placement: Option<Vec<u8>>,
        target: Option<[u8; 32]>,
    ) -> Arc<Device> {
        let target = target.unwrap_or_else(|| self.device_of(*addr.id.as_bytes()));
        Arc::new(Device {
            way: Way::Dial {
                endpoint: endpoint.clone(),
                addr: addr.clone(),
            },
            workspace: workspace.to_owned(),
            placement,
            target,
            key: Arc::clone(&self.0.device),
            runtime: self.0.runtime.clone(),
            connection: Mutex::new(None),
            next_correlation: AtomicU64::new(1),
            follow: Following::default(),
        })
    }

    /// [`Self::remote`] for a device this one reaches over `transport`, the
    /// link it holds to it: a page's link to its host, or the link a device
    /// dialled it over ([`Self::remote_held`]).
    #[must_use]
    pub fn remote_over(
        &self,
        transport: Arc<dyn Transport>,
        device: [u8; 32],
        workspace: &str,
    ) -> RemoteDevice {
        RemoteDevice(Arc::new(Device {
            way: Way::Linked(transport),
            workspace: workspace.to_owned(),
            placement: None,
            target: device,
            key: Arc::clone(&self.0.device),
            runtime: self.0.runtime.clone(),
            connection: Mutex::new(None),
            next_correlation: AtomicU64::new(1),
            follow: Following::default(),
        }))
    }

    /// [`Self::remote_held`] for `device`, over the link this device holds to
    /// it ([`Self::reach_device_at`]); `None` where it holds none.
    #[must_use]
    pub fn remote_held(&self, device: [u8; 32], workspace: &str) -> Option<RemoteDevice> {
        Some(self.remote_over(self.reach(device)?, device, workspace))
    }

    /// [`Self::remote`] for a request addressed to `target` through the device
    /// at `addr`: a page asks its host for the actions of another device of its
    /// workspace, which the host passes the request on to.
    #[must_use]
    pub fn through(
        &self,
        endpoint: &Endpoint,
        addr: &EndpointAddr,
        workspace: &str,
        target: [u8; 32],
    ) -> RemoteDevice {
        RemoteDevice(self.device_at(endpoint, addr, workspace, None, Some(target)))
    }

    /// The devices of `workspace` that may be asked to take work, other than this
    /// one and any device's workspace service: certified, unrevoked and
    /// unexpired, its person a member, its delegation live ([`StatePeer::permissions`](crate::StatePeer::permissions)).
    pub async fn key_holders(&self, workspace: &str) -> Vec<[u8; 32]> {
        let device = self.device();
        let mut key_holders = self
            .read_workspace(workspace, |peer| {
                let now = tilo_clock::wall();
                // A device's workspace service takes on no work of its own: it is
                // reached as the device that certified it, which is asked.
                peer.certificates()
                    .into_iter()
                    .filter(|certificate| !certificate.capabilities.workspace_service)
                    .map(|certificate| certificate.device_public_key)
                    .filter(|source| peer.permissions(*source, now).is_ok())
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
        key_holders.retain(|source| *source != device);
        key_holders.sort_unstable();
        key_holders.dedup();
        key_holders
    }

    /// Answer one request from the client address `client` (where the connection says it): what
    /// the device's dispatcher made of it, or what the device it was addressed to made of it
    /// when this one passes it on.
    async fn answer(
        &self,
        remote: Option<[u8; 32]>,
        execution: &ExecutionLease,
        client: Option<std::net::IpAddr>,
        signed: SignedDeviceAction,
    ) -> Option<RemoteActionMessage> {
        let action = signed.action();
        let request = &action.message;
        match self.decide(remote, execution, &signed).await {
            Ok(Decided::Serve(admitted)) => {
                self.dispatch(&admitted.dispatcher, &admitted.caller, client, request)
                    .await
            }
            Ok(Decided::Passed(answer)) => Some(*answer),
            Err(error) => tilo_actions::remote::refuse(request, error),
        }
    }

    /// What a request comes to here.
    #[expect(
        clippy::too_many_lines,
        reason = "the admit order reads best in one place, in the order it is decided"
    )]
    async fn decide(
        &self,
        remote: Option<[u8; 32]>,
        execution: &ExecutionLease,
        signed: &SignedDeviceAction,
    ) -> Result<Decided, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::ActionCallError;
        let action = signed.action();
        let source = action.source;
        // Every request one link carries is one run: an agent's or a key's requests on it share
        // the execution the link minted, and the run ends when the link does.
        let carrying = |caller: Caller| caller.in_execution(execution.clone());
        // A computer with no certificate yet is admitted to the one scope the device that
        // certifies it serves, and holds no workspace to be read. Its endpoint is a key the
        // device delegated, which the host cannot know before it admits the device, so its
        // request stands on its signature and on what its provider checks.
        let uncertified = self
            .0
            .uncertified
            .lock_recover()
            .get(&action.workspace)
            .cloned();
        // A device asks as itself and nobody else: the transport already says
        // which device is there, where it says one at all.
        if uncertified.is_none()
            && let Some(remote) = remote
            && remote != source
        {
            return Err(not_permitted(
                source,
                "it came over another device's connection",
            ));
        }
        if signed.verify(&source).is_err() {
            return Err(not_permitted(source, "its signature does not verify"));
        }
        if let Some(provider) = uncertified {
            self.admit_replay(signed)?;
            return Ok(Decided::Serve(Box::new(Admitted {
                caller: carrying(Caller::visitor(source)),
                dispatcher: provider,
            })));
        }
        // A request naming no workspace asks the host itself, which no workspace's roster
        // decides: the provider it answers those through reads the signed invocation each one
        // carries, so a device proves which of the host's person's it is.
        if action.workspace.is_empty() {
            if action.target != self.device() {
                return Err(ActionCallError::Refused {
                    reason: "the request names another device".into(),
                });
            }
            let Some(provider) = self.0.host_operations.lock_recover().clone() else {
                return Err(not_permitted(
                    source,
                    "this device serves no operations of its own",
                ));
            };
            self.admit_replay(signed)?;
            if let RemoteActionMessage::Invoke(request) = &action.message {
                self.admit_invocation(&action.workspace, request)?;
            }
            return Ok(Decided::Serve(Box::new(Admitted {
                caller: carrying(Caller::visitor(source)),
                dispatcher: provider,
            })));
        }
        let now = tilo_clock::wall();
        // A placement that does not decode is no placement: the request is decided as any other.
        let placement = action.placement.as_deref().and_then(|bytes| {
            tilo_failure::ignored!(
                "a placement that does not decode places nothing, and the request is decided as any other",
                postcard::from_bytes::<crate::join::TransportHint>(bytes).ok()
            )
        });
        // The invocation the request carries, when it carries one, is checked against the
        // workspace's revocations and its own signature, here, before it names who asks.
        let decided = self
            .read_workspace_for(
                Some((source, placement.as_ref())),
                &action.workspace,
                |peer| -> Result<(_, _, _, _), ActionCallError> {
                    let revocations = revocations_of(peer);
                    let mut acting = source;
                    if let RemoteActionMessage::Invoke(request) = &action.message
                        && request.signed.is_some()
                    {
                        let certificate = invocation_signer(
                            &action.workspace,
                            request,
                            &revocations,
                            now_ns(),
                        )?;
                        if certificate.device_public_key != source {
                            // A transport that names no device of its own carries another
                            // device's request to the one its opener reaches: the request runs
                            // as the device that signed it there. One that names a device
                            // carries only that device's own.
                            if remote.is_some() {
                                // The answer would reach a device that never asked.
                                return Err(ActionCallError::Refused {
                                    reason: "the invocation is signed by another key than the device asking"
                                        .into(),
                                });
                            }
                            acting = certificate.device_public_key;
                        }
                    }
                    let authority = peer.permissions(acting, now);
                    let certificate = peer
                        .certificates()
                        .into_iter()
                        .find(|certificate| certificate.device_public_key == acting);
                    let opened = certificate.is_none()
                        && visitor_may(peer, &action.workspace, acting, &action.message);
                    Ok((acting, authority, certificate, opened))
                },
            )
            .await
            .ok_or_else(|| ActionCallError::Refused {
                reason: "this device does not hold that workspace".into(),
            })??;
        let (acting, authority, certificate, opened) = decided;
        // A request addressed to another device this one reaches: pass it on there, which decides
        // and answers it, and hand its answer back to the caller.
        if action.target != self.device() {
            let passed = self
                .pass_on(signed)
                .await
                .ok_or_else(|| ActionCallError::Refused {
                    reason: "the request names another device".into(),
                })?;
            return passed.map(|answer| Decided::Passed(Box::new(answer)));
        }
        self.admit_replay(signed)?;
        if let RemoteActionMessage::Invoke(request) = &action.message {
            self.admit_invocation(&action.workspace, request)?;
        }
        // A key the workspace holds no certificate for is a visitor: it acts for nobody, and is
        // admitted only to what the workspace's grants to any key open, decided here, so no
        // dispatcher behind this one ever meets a caller off the roster it did not open to.
        let Some(certificate) = certificate else {
            if !opened {
                return Err(not_permitted(
                    source,
                    "a key the workspace holds no certificate for asks what it opens to no visitor",
                ));
            }
            return Ok(Decided::Serve(Box::new(Admitted {
                caller: carrying(Caller::visitor(acting).in_workspace(action.workspace.clone())),
                dispatcher: Arc::clone(&self.0.dispatcher),
            })));
        };
        let authority = authority.map_err(|error| {
            tracing::warn!(
                device = %hex::encode(&acting[..4]),
                workspace = %action.workspace,
                %error,
                "a device's request is refused: the workspace gives it no authority"
            );
            refusal_of(&error)
        })?;
        // The device is its own actor when its person delegated it: an
        // agent is the agent, a bot the bot, never the person.
        let caller = match &authority.acting.by {
            Actor::Member(MemberId::Agent(agent)) => {
                Caller::delegated_agent(agent.clone(), certificate.user_public_key, acting)
            }
            Actor::Member(MemberId::Bot(installation)) => {
                Caller::bot(installation.clone(), certificate.user_public_key, acting)
            }
            _ => Caller::authenticated(certificate.user_public_key, acting),
        };
        Ok(Decided::Serve(Box::new(Admitted {
            caller: carrying(caller)
                .in_workspace(action.workspace.clone())
                .from_foreign_device(authority.acting.foreign_device),
            dispatcher: Arc::clone(&self.0.dispatcher),
        })))
    }

    /// Take `signed` into the replay window this device keeps for its requests,
    /// or say it is stale or a repeat.
    fn admit_replay(
        &self,
        signed: &SignedDeviceAction,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        self.0
            .replay
            .lock_recover()
            .admit(signed, now_ns())
            .map_err(|error| tilo_actions::remote::ActionCallError::Refused {
                reason: error.to_string(),
            })
    }

    /// Pass `signed`, a request addressed to another device, on to that
    /// device over the link this one holds to it
    /// ([`Self::reach_device_at`]): its answer, or `None` where no link
    /// reaches it.
    ///
    /// A request is passed on as it was signed, so the device that answers
    /// runs it as the device that signed it, never as the one that carried
    /// it. That is only sound over a link that names no device of its own —
    /// a page's session with its host — where a request stands on what it
    /// proves. A link that names a device carries only that device's own
    /// requests.
    async fn pass_on(
        &self,
        signed: &SignedDeviceAction,
    ) -> Option<Result<RemoteActionMessage, tilo_actions::remote::ActionCallError>> {
        let transport = self.reach(signed.action().target)?;
        Some(self.relay(&transport, signed).await)
    }

    /// Write `signed` on a stream of `transport` and read back what the device
    /// it reaches answered.
    async fn relay(
        &self,
        transport: &Arc<dyn Transport>,
        signed: &SignedDeviceAction,
    ) -> Result<RemoteActionMessage, tilo_actions::remote::ActionCallError> {
        let bytes = postcard::to_allocvec(&ActionServiceFrame::Action(Box::new(signed.clone())))
            .map_err(|error| unavailable(&error.to_string()))?;
        // A device that holds a link and does not listen on it answers nothing: the caller is
        // told so when the deadline passes, never left waiting on the stream.
        let answer = tokio::time::timeout(REPLY_DEADLINE, async {
            let (mut send, mut recv) = transport
                .open_bi()
                .await
                .map_err(|error| unavailable(&error))?;
            write_frame(&mut send, &bytes)
                .await
                .map_err(|error| unavailable(&error))?;
            tokio::io::AsyncWriteExt::shutdown(&mut send)
                .await
                .map_err(|error| unavailable(&error.to_string()))?;
            read_frame(&mut recv)
                .await
                .map_err(|error| unavailable(&error))?
                .ok_or_else(|| unavailable("the device closed without answering"))
        })
        .await
        .map_err(|_| unavailable("the device did not answer"))??;
        RemoteActionFrame::decode(&answer)
            .map(|frame| frame.message)
            .map_err(|error| unavailable(&error.to_string()))
    }

    /// Whether `signed`, a request carrying an invocation, names one this
    /// device admitted within its freshness window for another request: a
    /// conflict, answered as the invocation log answers one.
    fn admit_invocation(
        &self,
        workspace: &str,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        let Some(signed) = &request.signed else {
            return Ok(());
        };
        self.0
            .invocations
            .lock_recover()
            .entry(workspace.to_owned())
            .or_default()
            .admit(signed, now_ns())
            .map_err(|error| match error {
                tilo_server_protocol::invocation::InvocationError::Replayed => {
                    tilo_actions::remote::ActionCallError::InvocationConflict
                }
                other => tilo_actions::remote::ActionCallError::Refused {
                    reason: format!("{}: {other}", other.code()),
                },
            })
    }

    /// Hand one verified request to `dispatcher` and answer it.
    async fn dispatch(
        &self,
        dispatcher: &Arc<dyn ActionProvider + Send + Sync>,
        caller: &Caller,
        client: Option<std::net::IpAddr>,
        request: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        use tilo_actions::remote::ActionCallError;
        match request {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let result = await_answer(dispatcher.query(caller, query.clone())).await;
                Some(RemoteActionMessage::CatalogueResponse {
                    correlation: *correlation,
                    result,
                })
            }
            RemoteActionMessage::Invoke(invoke) => {
                let alone = self.alone(dispatcher);
                // The ticket admitted for a computer with no certificate yet answers its one
                // call as before; every other visitor is counted by its network.
                let spent = if alone.is_some() {
                    Ok(())
                } else {
                    self.spend(caller, client)
                };
                let provider = alone.unwrap_or_else(|| self.provider_for(&invoke.target.provider));
                let status = match spent {
                    Ok(()) => await_answer(provider.invoke(caller, invoke.clone()))
                        .await
                        .map_or_else(
                            |error| ReceiptStatus::Rejected { error },
                            |receipt| receipt.status,
                        ),
                    Err(error) => ReceiptStatus::Rejected { error },
                };
                Some(RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: invoke.invocation,
                    status,
                }))
            }
            RemoteActionMessage::Observe {
                correlation,
                invocation,
                wait_ms,
                seen,
            } => {
                let state = self
                    .observe(
                        &self
                            .alone(dispatcher)
                            .unwrap_or_else(|| self.owner(caller, invocation)),
                        caller,
                        invocation,
                        *wait_ms,
                        *seen,
                    )
                    .await;
                Some(RemoteActionMessage::Update {
                    invocation: *invocation,
                    state,
                    correlation: Some(*correlation),
                })
            }
            RemoteActionMessage::Cancel { invocation } => {
                let result = await_answer(
                    self.alone(dispatcher)
                        .unwrap_or_else(|| self.owner(caller, invocation))
                        .cancel(caller, invocation),
                )
                .await;
                Some(RemoteActionMessage::CancelResponse {
                    invocation: *invocation,
                    result,
                })
            }
            // Bytes a caller hands the device's content store, kept open between steps here.
            RemoteActionMessage::ContentPutRequest { correlation, step } => {
                Some(RemoteActionMessage::ContentPutResponse {
                    correlation: *correlation,
                    result: self.put_content(dispatcher, caller, step),
                })
            }
            // A request this device does not answer is said so, never left
            // for the caller to wait on.
            other if other.is_request() => tilo_actions::remote::refuse(
                other,
                ActionCallError::Unavailable {
                    reason: "this device does not answer that request".into(),
                },
            ),
            _ => None,
        }
    }

    /// The state of `invocation` as `caller` owns it, held up to `wait_ms`
    /// until it moves past `seen` or settles.
    async fn observe(
        &self,
        dispatcher: &Arc<dyn ActionProvider + Send + Sync>,
        caller: &Caller,
        invocation: &tilo_actions::InvocationId,
        wait_ms: Option<u32>,
        seen: Option<UpdateMark>,
    ) -> Option<InvocationState> {
        let mut state = dispatcher.observe(caller, invocation);
        let Some(wait_ms) = wait_ms else {
            return state;
        };
        let wait =
            Duration::from_millis(u64::from(wait_ms)).min(tilo_actions::remote::MAX_REMOTE_WAIT);
        let deadline = tilo_clock::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        loop {
            if let Some(current) = &state
                && (current.is_terminal()
                    || seen.is_none_or(|seen| UpdateMark::of(&current.update) != seen))
            {
                return state;
            }
            let Some(remaining) = deadline.checked_duration_since(tilo_clock::now()) else {
                return dispatcher.observe(caller, invocation);
            };
            match dispatcher.next_change(caller, invocation) {
                Some(changed) => {
                    let _ = tokio::time::timeout(remaining, changed).await;
                }
                // A dispatcher that cannot tell follows its own schedule.
                None => tokio::time::sleep(remaining.min(Duration::from_secs(1))).await,
            }
            let next = dispatcher.observe(caller, invocation);
            if next == state {
                // A wake that found nothing new costs a moment, so a
                // dispatcher that signals at once is not asked in a loop.
                tokio::time::sleep(remaining.min(Duration::from_millis(5))).await;
            }
            state = next;
        }
    }

    /// Spend one of `caller`'s changes, or say the caller has none left. A visitor spends one
    /// of the calls its client address (`client`) may make.
    fn spend(
        &self,
        caller: &Caller,
        client: Option<std::net::IpAddr>,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        if caller.acting().visitor_key().is_some() {
            let now_ms = now_ns() / 1_000_000;
            return self
                .0
                .visitors
                .lock_recover()
                .admit(client, now_ms)
                .map_err(|retry_after_secs| {
                    tilo_actions::remote::ActionCallError::Refused {
                        reason: format!(
                            "too many tries from your network just now; try again in {retry_after_secs} seconds"
                        ),
                    }
                });
        }
        let Some(person) = caller.user_public_key().copied() else {
            return Ok(());
        };
        let now = tilo_clock::now();
        let rate = self.rate();
        let mut callers = self.0.callers.lock_recover();
        if callers.len() >= KEPT_CALLERS && !callers.contains_key(&person) {
            callers.retain(|_, bucket| !bucket.full(rate, now));
        }
        let bucket = callers.entry(person).or_insert(Bucket {
            changes: rate.capacity,
            at: now,
        });
        let spent = bucket.take(rate, now);
        drop(callers);
        if spent {
            Ok(())
        } else {
            Err(tilo_actions::remote::ActionCallError::Refused {
                reason: "too many changes; slow down".into(),
            })
        }
    }

    fn rate(&self) -> Rate {
        let bounds = self.0.bounds;
        Rate {
            capacity: f64::from(bounds.burst),
            per_second: f64::from(bounds.changes_per_window) / f64::from(bounds.window_secs.max(1)),
        }
    }

    /// Serve `transport`, whatever carries it, until it ends, answering each
    /// stream on its own: an iroh connection (the action service's own ALPN),
    /// or a page's link to its host (the same service over WebTransport). The
    /// device it proves at its other end asks as itself; one that proves none
    /// — a page's session, whose host it dialled — is answered by what each
    /// request's own signature says ([`Self::decide`]). `client` is the
    /// address a visitor's calls are counted by, where the transport reaches
    /// the caller at one directly.
    ///
    /// `execution` is the run every request this link carries is: the caller that starts it
    /// mints one and closes it when the link ends, so an agent's requests on the link are one
    /// run, followed and stopped where they started, and the run ends with the link.
    pub async fn serve_transport(
        &self,
        transport: &dyn Transport,
        client: Option<std::net::IpAddr>,
        execution: &ExecutionLease,
    ) -> Result<(), String> {
        let named = transport.remote();
        let remote = (named != [0; 32]).then(|| self.device_of(named));
        loop {
            // The link ended, from either end.
            let Ok((send, recv)) = transport.accept_bi().await else {
                break;
            };
            let action_service = self.clone();
            let run = execution.clone();
            self.0.runtime.spawn(async move {
                if let Err(error) = action_service
                    .answer_stream(remote, &run, client, send, recv)
                    .await
                {
                    tracing::warn!(%error, "a remote action stream ended in error");
                }
            });
        }
        Ok(())
    }

    /// Serve one connection of the action service's own ALPN until it ends,
    /// answering each stream on its own.
    async fn serve(&self, connection: Connection) -> Result<(), String> {
        // The address the device reaches the caller at directly, when it does: a visitor's
        // calls are counted by it.
        let client = connection
            .paths()
            .iter()
            .find_map(|path| match path.remote_addr() {
                iroh::TransportAddr::Ip(address) => Some(address.ip()),
                _ => None,
            });
        // The connection a device opened to ask is a link both ways: the host asks it back
        // over the same one, so a request addressed to that device is passed on there
        // ([`Self::reach_device_at`]).
        let transport: Arc<dyn Transport> = Arc::new(connection.clone());
        let device = self.device_of(*connection.remote_id().as_bytes());
        self.reach_device_at(device, Arc::clone(&transport));
        // One connection is one run: its requests share an execution, which ends with the
        // connection.
        let execution = ExecutionLease::mint();
        let served = self.serve_transport(&connection, client, &execution).await;
        execution.close();
        self.forget_device(device, &transport);
        served
    }

    /// Serve a link a device holds open to this one until it ends, answering
    /// each stream it opens on its own: a page's session with its host
    /// ([`LINK_ACTIONS_PATH`]), or any other [`Transport`] the two share.
    ///
    /// A link that names the device at its other end is recorded as the way
    /// to reach it ([`Self::reach_device_at`]), so requests addressed to that
    /// device are passed on there and this device asks it as it dials any
    /// other. A link that names no device — a page's, whose host it dialled —
    /// proves nothing of its opener, and the requests it carries stand on
    /// their own signatures ([`Self::decide`]).
    pub fn serve_link(&self, transport: Arc<dyn Transport>) {
        let named = transport.remote();
        let device = (named != [0; 32]).then(|| self.device_of(named));
        if let Some(device) = device {
            self.reach_device_at(device, Arc::clone(&transport));
        }
        // One link is one run: every request it carries shares this execution, which ends when
        // the link does, so a run's invocations are followed and stopped on the link that
        // started them and the run leaves with it.
        let execution = ExecutionLease::mint();
        let action_service = self.clone();
        self.0.runtime.spawn(async move {
            if let Err(error) = action_service
                .serve_transport(transport.as_ref(), None, &execution)
                .await
            {
                tracing::warn!(%error, "a link's action service ended in error");
            }
            execution.close();
            if let Some(device) = device {
                action_service.forget_device(device, &transport);
            }
        });
    }

    /// Read one request from `recv`, answer it on `send`, and finish.
    async fn answer_stream(
        &self,
        remote: Option<[u8; 32]>,
        execution: &ExecutionLease,
        client: Option<std::net::IpAddr>,
        mut send: StreamSend,
        mut recv: StreamRecv,
    ) -> Result<(), String> {
        let Some(bytes) = read_frame(&mut recv).await? else {
            return tokio::io::AsyncWriteExt::shutdown(&mut send)
                .await
                .map_err(|error| error.to_string());
        };
        let frame: ActionServiceFrame =
            postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        let ActionServiceFrame::Action(signed) = frame;
        if let Some(reply) = self.answer(remote, execution, client, *signed).await {
            let bytes = RemoteActionFrame::new(reply)
                .encode()
                .map_err(|error| error.to_string())?;
            write_frame(&mut send, &bytes).await?;
        }
        tokio::io::AsyncWriteExt::shutdown(&mut send)
            .await
            .map_err(|error| error.to_string())
    }
}

/// What [`ActionService::decide`] came to.
enum Decided {
    /// This device answers the request, as [`Admitted::caller`].
    Serve(Box<Admitted>),
    /// The device it was addressed to answered it, and that answer is the
    /// caller's.
    Passed(Box<RemoteActionMessage>),
}

/// A request this device answers, and who asks it.
struct Admitted {
    caller: Caller,
    dispatcher: Arc<dyn ActionProvider + Send + Sync>,
}

impl ProtocolHandler for ActionService {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.serve(connection)
            .await
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    changes: f64,
    at: Instant,
}

/// A bucket's size and refill rate per second.
#[derive(Clone, Copy)]
struct Rate {
    capacity: f64,
    per_second: f64,
}

impl Bucket {
    fn take(&mut self, rate: Rate, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.changes = elapsed
            .mul_add(rate.per_second, self.changes)
            .min(rate.capacity);
        self.at = now;
        if self.changes >= 1.0 {
            self.changes -= 1.0;
            true
        } else {
            false
        }
    }

    fn full(&self, rate: Rate, now: Instant) -> bool {
        now.saturating_duration_since(self.at)
            .as_secs_f64()
            .mul_add(rate.per_second, self.changes)
            >= rate.capacity
    }
}

/// Whether the workspace `peer` holds opens `message` to the visitor `key`: an invocation the
/// grants to any key allow ([`tilo_action_policy::EntryPolicy::visitor_grants`]), or following
/// one (the dispatcher keeps each invocation for the caller that started it). A visitor browses
/// and cancels nothing.
fn visitor_may(
    peer: &crate::StatePeer,
    workspace: &str,
    key: [u8; 32],
    message: &RemoteActionMessage,
) -> bool {
    let invoke = match message {
        RemoteActionMessage::Invoke(invoke) => invoke,
        RemoteActionMessage::Observe { .. } => return true,
        _ => return false,
    };
    let Ok(policy) = peer.governance().governed_policy() else {
        return false;
    };
    let node = tilo_action_policy::TreePath::workspace(workspace)
        .app(invoke.target.provider.as_str())
        .resource(invoke.target.address.resource.as_str())
        .action(invoke.target.address.action.as_str());
    let visitor = tilo_action_policy::Acting::visitor(&key);
    tilo_action_policy::Authority::of_acting(&visitor, &policy, &[])
        .decide(
            &node,
            tilo_action_policy::ActionKind::Undoable,
            &tilo_action_policy::Context::at(now_ns() / 1_000_000),
        )
        .answer
        == tilo_action_policy::Answer::Allow
}

/// A key of the device's own choosing, which the client addresses visitors ask from are counted
/// under ([`visitors`]).
fn rand_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand_core::Rng::fill_bytes(
        &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
        &mut key,
    );
    key
}

/// A request from `source` refused for `why`, which the host's log keeps: the caller reads
/// only that it may not, as a stranger would.
fn not_permitted(source: [u8; 32], why: &str) -> tilo_actions::remote::ActionCallError {
    tracing::debug!(device = %hex::encode(&source[..4]), why, "a device's request is refused");
    tilo_actions::remote::ActionCallError::NotPermitted
}

/// The revocations the workspace `peer` holds, as an invocation is checked
/// against them.
fn revocations_of(peer: &crate::StatePeer) -> RevocationList {
    let mut revocations = RevocationList::new();
    for entry in peer.revocations() {
        // An entry this workspace cannot verify is one it does not keep.
        let _ = revocations.insert(entry);
    }
    revocations
}

/// The certificate the invocation `request` carries is signed under, checked
/// against `workspace`'s revocations as a host checks it before running what
/// the invocation asks: the signature, that it is this workspace's and the
/// request's own invocation, the argument against the signed hash, the chain,
/// validity and revocation, and the signer's grant's reach.
fn invocation_signer(
    workspace: &str,
    request: &tilo_actions::remote::InvokeRequest,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<tilo_user_identity::DeviceCert, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::ActionCallError;
    use tilo_server_protocol::invocation::{InvocationError, verify_invocation};
    verify_invocation(workspace, request, revocations, now_ns).map_err(|error| match error {
        InvocationError::Revoked => ActionCallError::NotPermitted,
        other => ActionCallError::Refused {
            reason: format!("{}: {other}", other.code()),
        },
    })
}

/// Why the workspace's roster leaves a device without authority, as the
/// refusal its caller reads.
fn refusal_of(error: &crate::MembershipError) -> tilo_actions::remote::ActionCallError {
    use crate::MembershipError;
    use tilo_actions::remote::ActionCallError;
    match error {
        MembershipError::Revoked => ActionCallError::Refused {
            reason: "the device is revoked".into(),
        },
        MembershipError::NoMembership => ActionCallError::Refused {
            reason: "the person is not a member of this workspace".into(),
        },
        MembershipError::UnreadableAccess => ActionCallError::Refused {
            reason: "your access to this workspace could not be read".into(),
        },
        MembershipError::Delegate(_) => ActionCallError::Refused {
            reason: "the device's delegation is expired or withdrawn".into(),
        },
        MembershipError::Certificate(_) => ActionCallError::Refused {
            reason: "the device's certificate has run out".into(),
        },
        MembershipError::UnknownDevice
        | MembershipError::Absent
        | MembershipError::MalformedIdentity(_)
        | MembershipError::CertificateMismatch
        | MembershipError::RulesMissing
        | MembershipError::RevocationRoot
        | MembershipError::Revocation(_) => ActionCallError::NotPermitted,
    }
}

/// The answer a dispatcher's pending gives, or the refusal of one it
/// abandoned.
async fn await_answer<T: Send + 'static>(
    answer: tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionCallError>>,
) -> Result<T, tilo_actions::remote::ActionCallError> {
    match answer.await {
        Some(result) => result,
        None => Err(tilo_actions::remote::ActionCallError::Unavailable {
            reason: "the device's action provider abandoned the request".into(),
        }),
    }
}

/// One other device, as an [`ActionProvider`]: its actions through the same
/// trait a local host's are used through.
#[derive(Clone)]
pub struct RemoteDevice(Arc<Device>);

struct Device {
    /// How this device is reached: dialled at its address, or over the link it
    /// holds open to this one ([`ActionService::remote_over`]).
    way: Way,
    workspace: String,
    /// What every request carries of where this device serves the workspace, when it places the
    /// workspace's work on the device ([`ActionService::placing`]).
    placement: Option<Vec<u8>>,
    target: [u8; 32],
    key: Arc<dyn DeviceSigner>,
    runtime: tokio::runtime::Handle,
    /// The connection to this device, kept while it lasts: one per pair, and
    /// only for a device dialled.
    connection: Mutex<Option<Connection>>,
    next_correlation: AtomicU64,
    follow: Following,
}

/// How this device reaches the one its requests go to.
enum Way {
    /// Dialled at its own endpoint.
    Dial {
        endpoint: Endpoint,
        addr: EndpointAddr,
    },
    /// Over the link it holds open to this device: a page's session with its
    /// host, or the link a device dialled this one over.
    Linked(Arc<dyn Transport>),
}

impl std::fmt::Debug for RemoteDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteDevice")
            .field("device", &hex::encode(self.0.target))
            .field("workspace", &self.0.workspace)
            .finish_non_exhaustive()
    }
}

impl RemoteDevice {
    /// The device these actions are asked of.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.target
    }

    /// Send one message to the device and hand back its one answer, as the
    /// provider's calls build it: what a page, or a host handing a request on,
    /// asks with.
    #[must_use]
    pub fn ask_message(
        &self,
        message: RemoteActionMessage,
    ) -> tilo_actions::remote::Pending<Option<RemoteActionMessage>> {
        let (completer, result) = tilo_actions::remote::pending();
        let device = Arc::clone(&self.0);
        self.0.runtime.spawn(async move {
            completer.complete(device.request(message).await.ok());
        });
        result
    }

    /// Send one message and hand back what `answer` makes of its reply.
    fn ask<T: Send + 'static>(
        &self,
        message: RemoteActionMessage,
        answer: impl FnOnce(RemoteActionMessage) -> Result<T, tilo_actions::remote::ActionCallError>
        + Send
        + 'static,
    ) -> tilo_actions::remote::Pending<Result<T, tilo_actions::remote::ActionCallError>> {
        let (completer, result) = tilo_actions::remote::pending();
        let device = Arc::clone(&self.0);
        self.0.runtime.spawn(async move {
            let reply = device.request(message).await;
            completer.complete(reply.and_then(answer));
        });
        result
    }

    /// One step of a content put into the device's content store: what a caller hands a device
    /// bytes by before it invokes an action that names them by hash.
    pub fn content_put(
        &self,
        step: tilo_actions::remote::ContentPutStep,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::ContentPutAnswer, tilo_actions::remote::ActionCallError>,
    > {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.ask(
            RemoteActionMessage::ContentPutRequest { correlation, step },
            |answer| match answer {
                RemoteActionMessage::ContentPutResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }

    /// Invoke `request` on the device and follow it until it settles there:
    /// its terminal update, or why it was refused or lost. No thread waits:
    /// the receipt is held by the device until the invocation moves, and each
    /// answer sends the next long-poll.
    pub fn invoke_until_settled(
        self: Arc<Self>,
        mut request: tilo_actions::remote::InvokeRequest,
        follow_for: Duration,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionCallError>,
    > {
        let now = tilo_clock::now();
        let budget = FollowBudget {
            until: now.checked_add(follow_for).unwrap_or(now),
            rounds: MAX_FOLLOW_ROUNDS,
        };
        let invocation = request.invocation;
        request.wait_ms = Some(CHANGE_WAIT_MS);
        let (done, answer) = tilo_actions::remote::pending();
        let provider = Arc::clone(&self);
        self.ask(
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => Ok(receipt),
                _ => Err(unexpected()),
            },
        )
        .then(move |receipt| match receipt {
            Some(Ok(receipt)) => match settled_receipt(invocation, receipt.status) {
                Ok(Some(update)) => done.complete(Ok(update)),
                Ok(None) => follow(
                    &provider,
                    invocation,
                    Some(UpdateMark::Accepted),
                    budget,
                    done,
                ),
                Err(error) => done.complete(Err(error)),
            },
            Some(Err(error)) => done.complete(Err(error)),
            None => done.complete(Err(unanswered())),
        });
        answer
    }
}

impl Device {
    /// The connection to this device, opened once and kept while it lasts.
    async fn connection(&self) -> Result<Connection, tilo_actions::remote::ActionCallError> {
        if let Some(open) = self
            .connection
            .lock_recover()
            .as_ref()
            .filter(|connection| connection.close_reason().is_none())
        {
            return Ok(open.clone());
        }
        let Way::Dial { endpoint, addr } = &self.way else {
            return Err(unavailable(
                "the device is reached over a link, not dialled",
            ));
        };
        let connection =
            tokio::time::timeout(REPLY_DEADLINE, endpoint.connect(addr.clone(), ACTIONS_ALPN))
                .await
                .map_err(|_| unavailable("the device did not answer"))?
                .map_err(|error| unavailable(&error.to_string()))?;
        *self.connection.lock_recover() = Some(connection.clone());
        Ok(connection)
    }

    /// One signed request and its one answer.
    async fn request(
        &self,
        message: RemoteActionMessage,
    ) -> Result<RemoteActionMessage, tilo_actions::remote::ActionCallError> {
        let signed = SignedDeviceAction::sign(
            &DeviceAction {
                workspace: self.workspace.clone(),
                request_id: uuid::Uuid::new_v4(),
                source: self.key.device_public_key(),
                target: self.target,
                message,
                placement: self.placement.clone(),
            },
            now_ns(),
            self.key.as_ref(),
        )
        .map_err(|error| tilo_actions::remote::ActionCallError::unavailable(error.to_string()))?;
        let bytes = postcard::to_allocvec(&ActionServiceFrame::Action(Box::new(signed))).map_err(
            |error| tilo_actions::remote::ActionCallError::unavailable(error.to_string()),
        )?;
        let answer = self.exchange(&bytes).await?;
        RemoteActionFrame::decode(&answer)
            .map(|frame| frame.message)
            .map_err(|error| tilo_actions::remote::ActionCallError::unavailable(error.to_string()))
    }

    /// Write `bytes` on a stream of this device's own, and read the one
    /// answer back: over the connection to its endpoint, or over the link it
    /// holds open to this one.
    async fn exchange(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<u8>, tilo_actions::remote::ActionCallError> {
        let (mut send, mut recv): (StreamSend, StreamRecv) = match &self.way {
            Way::Dial { .. } => {
                let connection = self.connection().await?;
                let (send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|error| unavailable(&error.to_string()))?;
                (Box::new(send), Box::new(recv))
            }
            Way::Linked(link) => link.open_bi().await.map_err(|error| unavailable(&error))?,
        };
        write_frame(&mut send, bytes)
            .await
            .map_err(|error| unavailable(&error))?;
        tokio::io::AsyncWriteExt::shutdown(&mut send)
            .await
            .map_err(|error| unavailable(&error.to_string()))?;
        tokio::time::timeout(REPLY_DEADLINE, read_frame(&mut recv))
            .await
            .map_err(|_| unavailable("the device did not answer"))?
            .map_err(|error| unavailable(&error))?
            .ok_or_else(|| unavailable("the device closed without answering"))
    }
}

/// What a peer did not answer, as the caller reads it.
fn unavailable(reason: &str) -> tilo_actions::remote::ActionCallError {
    tilo_actions::remote::ActionCallError::Unavailable {
        reason: reason.to_owned(),
    }
}

fn unexpected() -> tilo_actions::remote::ActionCallError {
    tilo_actions::remote::ActionCallError::unavailable(
        "the device answered with an unexpected message",
    )
}

const fn unanswered() -> tilo_actions::remote::ActionCallError {
    tilo_actions::remote::ActionCallError::Unanswered
}

/// The terminal update a receipt amounts to; `None` while it runs.
fn settled_receipt(
    invocation: tilo_actions::InvocationId,
    status: ReceiptStatus,
) -> Result<Option<tilo_actions::InvocationUpdate>, tilo_actions::remote::ActionCallError> {
    use tilo_actions::InvocationUpdate;
    match status {
        ReceiptStatus::Accepted => Ok(None),
        ReceiptStatus::Completed { output } => {
            Ok(Some(InvocationUpdate::Succeeded { invocation, output }))
        }
        ReceiptStatus::Failed { error } => Ok(Some(InvocationUpdate::Failed { invocation, error })),
        ReceiptStatus::Cancelled { message } => Ok(Some(InvocationUpdate::Cancelled {
            invocation,
            message,
        })),
        ReceiptStatus::Rejected { error } => Err(error),
    }
}

/// What following one invocation on another device has left.
#[derive(Clone, Copy)]
struct FollowBudget {
    until: Instant,
    rounds: u32,
}

/// Most long-polls one followed invocation sends. A device holds each for up
/// to [`CHANGE_WAIT_MS`] unless the invocation moves, so this bounds only a
/// device that answers at once with nothing new.
const MAX_FOLLOW_ROUNDS: u32 = 64;

/// Ask the device for `invocation`'s next move past `seen`, and keep asking
/// until it settles or `budget` runs out.
fn follow(
    provider: &Arc<RemoteDevice>,
    invocation: tilo_actions::InvocationId,
    seen: Option<UpdateMark>,
    budget: FollowBudget,
    done: tilo_actions::remote::Completer<
        Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionCallError>,
    >,
) {
    if budget.rounds == 0 || tilo_clock::now() >= budget.until {
        done.complete(Err(tilo_actions::remote::ActionCallError::unavailable(
            "the device did not finish the action in time",
        )));
        return;
    }
    let next = FollowBudget {
        rounds: budget.rounds.saturating_sub(1),
        ..budget
    };
    let again = Arc::clone(provider);
    provider
        .ask(
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: Some(CHANGE_WAIT_MS),
                seen,
            },
            |answer| match answer {
                RemoteActionMessage::Update { state, .. } => Ok(state),
                _ => Err(unexpected()),
            },
        )
        .then(move |answer| match answer {
            Some(Ok(Some(state))) if state.is_terminal() => done.complete(Ok(state.update)),
            Some(Ok(Some(state))) => {
                let seen = Some(UpdateMark::of(&state.update));
                follow(&again, invocation, seen, next, done);
            }
            Some(Ok(None)) => done.complete(Err(
                tilo_actions::remote::ActionCallError::UnknownInvocation,
            )),
            Some(Err(error)) => done.complete(Err(error)),
            None => done.complete(Err(unanswered())),
        });
}

/// What following an invocation on another device knows of it.
#[derive(Default)]
struct Following {
    /// How far each running invocation was last seen to get: what a
    /// long-poll asks the device to move past.
    seen: Mutex<HashMap<tilo_actions::InvocationId, UpdateMark>>,
    /// The state a long-poll brought back, for the observe that follows its
    /// wake, so one change costs one round trip, not two.
    fresh: Mutex<HashMap<tilo_actions::InvocationId, InvocationState>>,
    /// Set once the device answered a long-poll at once with nothing new: it
    /// predates waits, so its callers keep their own schedule.
    unheld: std::sync::atomic::AtomicBool,
}

impl Following {
    fn saw(&self, id: tilo_actions::InvocationId, state: Option<&InvocationState>) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        match state.filter(|state| !state.is_terminal()) {
            Some(state) => {
                if seen.len() >= MAX_FOLLOWED && !seen.contains_key(&id) {
                    seen.clear();
                }
                seen.insert(id, UpdateMark::of(&state.update));
            }
            None => {
                seen.remove(&id);
            }
        }
    }

    fn mark(&self, id: &tilo_actions::InvocationId) -> Option<UpdateMark> {
        self.seen.lock().ok()?.get(id).copied()
    }

    /// Keep what a long-poll brought back. A `None` is not kept: the observe
    /// that follows asks again, and hears the same refusal.
    fn keep_fresh(&self, id: tilo_actions::InvocationId, state: Option<InvocationState>) {
        if let (Some(state), Ok(mut fresh)) = (state, self.fresh.lock()) {
            if fresh.len() >= MAX_FOLLOWED && !fresh.contains_key(&id) {
                fresh.clear();
            }
            fresh.insert(id, state);
        }
    }

    fn take_fresh(&self, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.fresh.lock().ok()?.remove(id)
    }
}

impl ActionProvider for RemoteDevice {
    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.ask(
            RemoteActionMessage::CatalogueRequest { correlation, query },
            |answer| match answer {
                RemoteActionMessage::CatalogueResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        self.ask(
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => match receipt.status {
                    ReceiptStatus::Rejected { error } => Err(error),
                    _ => Ok(receipt),
                },
                _ => Err(unexpected()),
            },
        )
    }

    /// The state the long-poll that just woke its caller brought back, or
    /// else the device's answer to asking, waited for as the trait's
    /// synchronous shape requires.
    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let state = self.0.follow.take_fresh(id).or_else(|| {
            let answer = self
                .ask(
                    RemoteActionMessage::Observe {
                        correlation: 0,
                        invocation: *id,
                        wait_ms: None,
                        seen: None,
                    },
                    |answer| match answer {
                        RemoteActionMessage::Update { state, .. } => Ok(state),
                        _ => Err(unexpected()),
                    },
                )
                .wait_timeout(REPLY_DEADLINE);
            match answer {
                Ok(Some(Ok(state))) => state,
                _ => None,
            }
        });
        self.0.follow.saw(*id, state.as_ref());
        state
    }

    /// A long-poll: one waiting observe the device answers when the
    /// invocation moves past what was last seen of it, or after
    /// [`CHANGE_WAIT_MS`]. No thread waits for it. `None` for a device that
    /// answered one at once with nothing new, so its callers keep their own
    /// schedule instead of asking as fast as it answers.
    fn next_change(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Pending<()>> {
        if self.0.follow.unheld.load(Ordering::Relaxed) {
            return None;
        }
        let seen = self.0.follow.mark(id);
        let sent = tilo_clock::now();
        let device = Arc::clone(&self.0);
        let invocation = *id;
        let (changed, change) = tilo_actions::remote::pending();
        self.ask(
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: Some(CHANGE_WAIT_MS),
                seen,
            },
            move |answer| match answer {
                RemoteActionMessage::Update { state, .. } => {
                    let same = state.as_ref().map(|state| UpdateMark::of(&state.update)) == seen
                        && state.is_some();
                    let early =
                        sent.elapsed() < Duration::from_millis(u64::from(CHANGE_WAIT_MS / 2));
                    if same && early {
                        device.follow.unheld.store(true, Ordering::Relaxed);
                    }
                    device.follow.keep_fresh(invocation, state);
                    Ok(())
                }
                _ => Err(unexpected()),
            },
        )
        .then(move |_| changed.complete(()));
        Some(change)
    }

    fn cancel(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        self.ask(
            RemoteActionMessage::Cancel { invocation: *id },
            |answer| match answer {
                RemoteActionMessage::CancelResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }
}

pub(crate) fn now_ns() -> u64 {
    tilo_clock::wall()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or(u64::MAX)
}

async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    send: &mut W,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    crate::frames::write_frame(send, bytes).await
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    recv: &mut R,
) -> Result<Option<Vec<u8>>, String> {
    crate::frames::read_frame(recv, MAX_FRAME_BYTES).await
}

/// A link between two devices with no network between them, for tests of
/// both ends of it: what a page's session with its host carries, and what a
/// host opens a bi-stream on to reach the page.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::{StreamRecv, StreamSend, Transport};
    use crate::transport::StreamFuture;
    use std::sync::Arc;
    use tilo_server_protocol::refusal::Refusal;
    use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

    /// What one stream of the link carries at once.
    const BUFFER: usize = 1 << 20;

    /// Two ends of one link. Each is told which device the other end is,
    /// where it proves one: `None` leaves that end a key no device holds, as
    /// a page's session with its host has.
    #[must_use]
    pub fn duplex(
        left_remote: Option<[u8; 32]>,
        right_remote: Option<[u8; 32]>,
    ) -> (Arc<dyn Transport>, Arc<dyn Transport>) {
        let (to_left_bi, left_in_bi) = mpsc::channel(64);
        let (to_right_bi, right_in_bi) = mpsc::channel(64);
        let (to_left_uni, left_in_uni) = mpsc::channel(64);
        let (to_right_uni, right_in_uni) = mpsc::channel(64);
        let (closed, _) = watch::channel(false);
        let left = Side {
            remote: left_remote.unwrap_or([0; 32]),
            to_peer_bi: to_right_bi,
            to_peer_uni: to_right_uni,
            in_bi: AsyncMutex::new(left_in_bi),
            in_uni: AsyncMutex::new(left_in_uni),
            closed: closed.clone(),
        };
        let right = Side {
            remote: right_remote.unwrap_or([0; 32]),
            to_peer_bi: to_left_bi,
            to_peer_uni: to_left_uni,
            in_bi: AsyncMutex::new(right_in_bi),
            in_uni: AsyncMutex::new(right_in_uni),
            closed,
        };
        (Arc::new(left), Arc::new(right))
    }

    struct Side {
        remote: [u8; 32],
        to_peer_bi: mpsc::Sender<(StreamSend, StreamRecv)>,
        to_peer_uni: mpsc::Sender<StreamRecv>,
        in_bi: AsyncMutex<mpsc::Receiver<(StreamSend, StreamRecv)>>,
        in_uni: AsyncMutex<mpsc::Receiver<StreamRecv>>,
        closed: watch::Sender<bool>,
    }

    impl Side {
        fn gone(&self) -> bool {
            *self.closed.borrow()
        }
    }

    impl Transport for Side {
        fn open_uni(&self) -> StreamFuture<'_, StreamSend> {
            Box::pin(async move {
                if self.gone() {
                    return Err("the link closed".into());
                }
                let (mine, theirs) = tokio::io::duplex(BUFFER);
                self.to_peer_uni
                    .send(Box::new(theirs) as StreamRecv)
                    .await
                    .map_err(|_| "the link closed".to_string())?;
                Ok(Box::new(mine) as StreamSend)
            })
        }

        fn accept_uni(&self) -> StreamFuture<'_, StreamRecv> {
            Box::pin(async move {
                if self.gone() {
                    return Err("the link closed".into());
                }
                self.in_uni
                    .lock()
                    .await
                    .recv()
                    .await
                    .ok_or_else(|| "the link closed".to_string())
            })
        }

        fn open_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
            Box::pin(async move {
                if self.gone() {
                    return Err("the link closed".into());
                }
                let (mine, theirs) = tokio::io::duplex(BUFFER);
                let (mine_read, mine_write) = tokio::io::split(mine);
                let (their_read, their_write) = tokio::io::split(theirs);
                let theirs: (StreamSend, StreamRecv) =
                    (Box::new(their_write), Box::new(their_read));
                self.to_peer_bi
                    .send(theirs)
                    .await
                    .map_err(|_| "the link closed".to_string())?;
                let ours: (StreamSend, StreamRecv) = (Box::new(mine_write), Box::new(mine_read));
                Ok(ours)
            })
        }

        fn accept_bi(&self) -> StreamFuture<'_, (StreamSend, StreamRecv)> {
            Box::pin(async move {
                if self.gone() {
                    return Err("the link closed".into());
                }
                self.in_bi
                    .lock()
                    .await
                    .recv()
                    .await
                    .ok_or_else(|| "the link closed".to_string())
            })
        }

        fn remote(&self) -> [u8; 32] {
            self.remote
        }

        fn close(&self, _code: u32, _reason: &[u8]) {
            self.closed.send_replace(true);
        }

        fn close_reason(&self) -> Option<Refusal> {
            None
        }
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::refusal_of;
    use crate::MembershipError;
    use tilo_actions::remote::ActionCallError;

    /// A person whose access rows do not read back reads that on the action
    /// they took, instead of being told they are no member.
    #[test]
    fn an_unreadable_access_row_is_refused_with_that_reason() {
        assert_eq!(
            refusal_of(&MembershipError::UnreadableAccess),
            ActionCallError::Refused {
                reason: "your access to this workspace could not be read".into(),
            }
        );
    }
}

#[cfg(test)]
mod provider_routing_tests {
    use super::*;
    use tilo_actions::remote::{
        ActionCallError, CataloguePage, CatalogueQuery, InvocationReceipt, InvocationState,
        InvokeRequest, Pending, TestProvider,
    };

    /// A provider that says which provider's actions it answers, so one device answers two.
    struct Serving {
        name: &'static str,
        inner: TestProvider,
    }

    impl ActionProvider for Serving {
        fn serves(&self, provider: &str) -> bool {
            provider == self.name
        }

        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.inner.query(caller, query)
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.inner.invoke(caller, request)
        }

        fn observe(
            &self,
            caller: &Caller,
            id: &tilo_actions::InvocationId,
        ) -> Option<InvocationState> {
            self.inner.observe(caller, id)
        }

        fn cancel(
            &self,
            caller: &Caller,
            id: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            self.inner.cancel(caller, id)
        }
    }

    fn invoked(provider: &Arc<dyn ActionProvider + Send + Sync>, name: &str) {
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: name.to_owned(),
                address: tilo_actions::ActionAddress::new("resource", "action"),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let caller = Caller::authenticated([2; 32], [3; 32]);
        let _ = provider
            .invoke(&caller, request)
            .wait_timeout(std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_request_reaches_the_provider_it_names_beside_the_dispatchers_own() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let own = Arc::new(Serving {
            name: "own",
            inner: TestProvider::new(),
        });
        let extra = Arc::new(Serving {
            name: "extra",
            inner: TestProvider::new(),
        });
        let service = ActionService::new(key, own.clone());
        service.add_provider(extra.clone());
        assert!(extra.serves("extra"), "the extra answers extra");
        assert!(!own.serves("extra"), "the dispatcher answers not extra");
        let chosen = service.provider_for("extra");
        assert!(chosen.serves("extra"), "the provider chosen answers extra");
        assert!(
            !chosen.serves("own"),
            "the provider chosen is not the dispatcher"
        );
        invoked(&chosen, "extra");
        assert_eq!(
            extra.inner.asked().len(),
            1,
            "the provider the device serves beside its own is the one asked"
        );
        assert!(
            own.inner.asked().is_empty(),
            "the dispatcher is not asked for another provider's action"
        );
        invoked(&service.provider_for("own"), "own");
        assert_eq!(own.inner.asked().len(), 1, "the dispatcher answers its own");
    }
}
