//! Actions between the devices of a workspace, over an iroh connection with
//! no host, store or relay in between.
//!
//! A request is one signed [`SignedDeviceAction`] on a connection of its
//! own, ALPN [`ACTIONS_ALPN`], and its answer is the [`RemoteActionMessage`]
//! the device's one dispatcher
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
//! 1. the connection id is the action's `source`, so a device may only ask
//!    as itself;
//! 2. the signature is the source's ([`SignedDeviceAction::verify`]);
//! 3. the addressed device is this one;
//! 4. the request is fresh and not one already admitted
//!    ([`ActionReplayWindow`]);
//! 5. this device holds the workspace it names;
//! 6. the source is a device of that workspace's roster: certified,
//!    unrevoked, unexpired, its person a member, its delegation live
//!    ([`StatePeer::permissions`]); or a key the workspace holds no certificate
//!    for at all, a visitor ([`Caller::visitor`]), which acts for nobody.
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
use collab_types::signer::DeviceSigner;
use iroh::endpoint::{Connection, RecvStream, SendStream};
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
    ActionProvider, Caller, InvocationReceipt, InvocationState, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, UpdateMark,
};
use tilo_lock::MutexRecover as _;
use web_time::SystemTime;

mod visitors;
pub use visitors::{VISITOR_CALLS_PER_DAY, VISITOR_CALLS_PER_TEN_MINUTES};

/// ALPN of the device-to-device action service.
pub const ACTIONS_ALPN: &[u8] = b"tilo-actions/1";

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
    /// The scope a device with no certificate yet may call, and what answers it there
    /// ([`ActionService::admit_uncertified`]).
    uncertified: Mutex<Option<(String, Arc<dyn ActionProvider + Send + Sync>)>>,
    replay: Mutex<ActionReplayWindow>,
    /// The calls visitors made, per client address ([`visitors`]).
    visitors: Mutex<visitors::VisitorWindows>,
    /// The uploads callers keep open between the steps of their content puts.
    content_puts: tilo_actions::remote::HostedContentPuts,
    bounds: Bounds,
    callers: Mutex<HashMap<[u8; 32], Bucket>>,
    /// Endpoints keyed apart from the device they answer for, by endpoint
    /// id ([`ActionService::reach_device_at`]).
    endpoints: Mutex<HashMap<[u8; 32], [u8; 32]>>,
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
            replay: Mutex::default(),
            visitors: Mutex::new(visitors::VisitorWindows::new(rand_key())),
            content_puts: tilo_actions::remote::HostedContentPuts::default(),
            bounds,
            callers: Mutex::default(),
            endpoints: Mutex::default(),
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
            replay: Mutex::default(),
            visitors: Mutex::new(visitors::VisitorWindows::new(rand_key())),
            content_puts: tilo_actions::remote::HostedContentPuts::default(),
            bounds: Bounds::default(),
            callers: Mutex::default(),
            endpoints: Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// Take a connection from the endpoint `endpoint` as one from the device
    /// `device`, and address what this device asks there to `device`: a
    /// device whose endpoint is keyed apart from its device key, as a Tilo
    /// server's is, asks and answers at that endpoint as the device. Every
    /// other endpoint is the device it is keyed by.
    pub fn reach_device_at(&self, device: [u8; 32], endpoint: [u8; 32]) {
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
        *self.0.uncertified.lock_recover() = Some((scope.into(), provider));
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

    /// [`Self::remote`] for `agent`, which this device's person handed the requests to: every
    /// request names it, and the device that answers acts on each as that agent, for this
    /// device's person, under the agent's grants there.
    #[must_use]
    pub fn remote_for_agent(
        &self,
        endpoint: &Endpoint,
        addr: &EndpointAddr,
        workspace: &str,
        agent: neighbour_wire::actions::HandedAgent,
    ) -> RemoteDevice {
        RemoteDevice(self.device_at(endpoint, addr, workspace, None, Some(agent)))
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
        agent: Option<neighbour_wire::actions::HandedAgent>,
    ) -> Arc<Device> {
        Arc::new(Device {
            endpoint: endpoint.clone(),
            addr: addr.clone(),
            workspace: workspace.to_owned(),
            placement,
            agent,
            target: self.device_of(*addr.id.as_bytes()),
            key: Arc::clone(&self.0.device),
            runtime: self.0.runtime.clone(),
            connection: Mutex::new(None),
            next_correlation: AtomicU64::new(1),
            follow: Following::default(),
        })
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
    /// the device's dispatcher made of it.
    async fn answer(
        &self,
        remote: [u8; 32],
        client: Option<std::net::IpAddr>,
        signed: SignedDeviceAction,
    ) -> Option<RemoteActionMessage> {
        let action = signed.action();
        let request = &action.message;
        match self.authorize(remote, &signed).await {
            Ok((caller, dispatcher)) => self.dispatch(&dispatcher, &caller, client, request).await,
            Err(error) => tilo_actions::remote::refuse(request, error),
        }
    }

    /// The caller a verified request acts as and what answers it, or why it is refused.
    async fn authorize(
        &self,
        remote: [u8; 32],
        signed: &SignedDeviceAction,
    ) -> Result<
        (Caller, Arc<dyn ActionProvider + Send + Sync>),
        tilo_actions::remote::ActionCallError,
    > {
        use tilo_actions::remote::ActionCallError;
        let action = signed.action();
        let source = action.source;
        // A device asks as itself and nobody else: the connection already
        // says which device is there.
        if remote != source {
            return Err(not_permitted(
                source,
                "it came over another device's connection",
            ));
        }
        if signed.verify(&source).is_err() {
            return Err(not_permitted(source, "its signature does not verify"));
        }
        if action.target != self.device() {
            return Err(ActionCallError::Refused {
                reason: "the request names another device".into(),
            });
        }
        self.0
            .replay
            .lock_recover()
            .admit(signed, now_ns())
            .map_err(|error| ActionCallError::Refused {
                reason: error.to_string(),
            })?;
        let uncertified = self.0.uncertified.lock_recover().clone();
        if let Some((scope, provider)) = uncertified
            && action.workspace == scope
        {
            return Ok((Caller::visitor(source), provider));
        }
        let now = tilo_clock::wall();
        // A placement that does not decode is no placement: the request is decided as any other.
        let placement = action.placement.as_deref().and_then(|bytes| {
            tilo_failure::ignored!(
                "a placement that does not decode places nothing, and the request is decided as any other",
                postcard::from_bytes::<crate::join::TransportHint>(bytes).ok()
            )
        });
        let (authority, certificate, opened) = self
            .read_workspace_for(
                Some((source, placement.as_ref())),
                &action.workspace,
                |peer| {
                    let authority = peer.permissions(source, now);
                    let certificate = peer
                        .certificates()
                        .into_iter()
                        .find(|certificate| certificate.device_public_key == source);
                    let opened = certificate.is_none()
                        && visitor_may(peer, &action.workspace, source, &action.message);
                    (authority, certificate, opened)
                },
            )
            .await
            .ok_or_else(|| ActionCallError::Refused {
                reason: "this device does not hold that workspace".into(),
            })?;
        // A key the workspace holds no certificate for is a visitor: it acts for nobody, and is
        // admitted only to what the workspace's grants to any key open, decided here, so no
        // dispatcher behind this one ever meets a caller off the roster it did not open to.
        let Some(certificate) = certificate else {
            if action.agent.is_some() || !opened {
                return Err(not_permitted(
                    source,
                    "a key the workspace holds no certificate for asks what it opens to no visitor",
                ));
            }
            return Ok((
                Caller::visitor(source).in_workspace(action.workspace.clone()),
                Arc::clone(&self.0.dispatcher),
            ));
        };
        let authority = authority.map_err(|error| {
            tracing::warn!(
                device = %hex::encode(&source[..4]),
                workspace = %action.workspace,
                %error,
                "a device's request is refused: the workspace gives it no authority"
            );
            refusal_of(&error)
        })?;
        // The device is its own actor when its person delegated it: an
        // agent is the agent, never the person.
        let caller = match &authority.acting.by {
            Actor::Member(MemberId::Agent(agent)) => {
                Caller::delegated_agent(agent.clone(), certificate.user_public_key, source)
            }
            _ => Caller::authenticated(certificate.user_public_key, source),
        };
        let caller = match &action.agent {
            None => caller,
            Some(agent) => handed(caller, agent)?,
        };
        Ok((
            caller
                .in_workspace(action.workspace.clone())
                .from_foreign_device(authority.acting.foreign_device),
            Arc::clone(&self.0.dispatcher),
        ))
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
            // An agent the person's device handed its requests to asks its person here, where its
            // session is, as an agent of this device does; a device itself confirms nothing here.
            RemoteActionMessage::Confirm {
                correlation,
                request,
            } if caller.is_session_delegated() && caller.acting().is_agent() => {
                let result = await_answer(dispatcher.confirm(caller, request)).await;
                Some(RemoteActionMessage::ConfirmResponse {
                    correlation: *correlation,
                    result: result.map(Box::new),
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

    /// Serve `connection` until it ends, answering each stream on its own.
    async fn serve(&self, connection: Connection) -> Result<(), String> {
        let remote = self.device_of(*connection.remote_id().as_bytes());
        // The address the device reaches the caller at directly, when it does: a visitor's
        // calls are counted by it.
        let client = connection
            .paths()
            .iter()
            .find_map(|path| match path.remote_addr() {
                iroh::TransportAddr::Ip(address) => Some(address.ip()),
                _ => None,
            });
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(stream) => stream,
                Err(iroh::endpoint::ConnectionError::ApplicationClosed(_)) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            };
            let action_service = self.clone();
            self.0.runtime.spawn(async move {
                if let Err(error) = action_service
                    .answer_stream(remote, client, send, recv)
                    .await
                {
                    tracing::warn!(%error, "a remote action stream ended in error");
                }
            });
        }
    }

    /// Read one request from `recv`, answer it on `send`, and finish.
    async fn answer_stream(
        &self,
        remote: [u8; 32],
        client: Option<std::net::IpAddr>,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), String> {
        let Some(bytes) = read_frame(&mut recv).await? else {
            return send.finish().map_err(|error| error.to_string());
        };
        let frame: ActionServiceFrame =
            postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        let ActionServiceFrame::Action(signed) = frame;
        if let Some(reply) = self.answer(remote, client, *signed).await {
            let bytes = RemoteActionFrame::new(reply)
                .encode()
                .map_err(|error| error.to_string())?;
            write_frame(&mut send, &bytes).await?;
        }
        send.finish().map_err(|error| error.to_string())
    }
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

/// `caller`, a person at their own device, as the agent the device says it handed the request
/// to: that agent, for the person, held to what both the person and the agent's grants allow,
/// under the id the person's device gives an agent of that client
/// ([`tilo_actions::remote::device_agent_id`]). The claim only narrows, so the device's word is
/// taken for it; a caller that is already not the person (an agent's own device)
/// hands nothing on.
fn handed(
    caller: Caller,
    agent: &neighbour_wire::actions::HandedAgent,
) -> Result<Caller, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::ActionCallError;
    if !caller.acting().is_person() {
        return Err(ActionCallError::NotPermitted);
    }
    let said = |text: &str| !text.trim().is_empty() && text.len() <= MAX_HANDED;
    if !(said(&agent.client) && said(&agent.session)) {
        return Err(ActionCallError::InvalidRequest {
            reason: "the agent the request names has no client or session".into(),
        });
    }
    let person = caller
        .user_public_key()
        .copied()
        .ok_or(ActionCallError::NotPermitted)?;
    let id = tilo_actions::remote::device_agent_id(&person, &agent.client);
    Ok(caller
        .delegated_to(id)
        .named(&agent.client)
        .in_agent_session(agent.session.clone()))
}

/// The longest client or session a handed agent is named by.
const MAX_HANDED: usize = 256;

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
    endpoint: Endpoint,
    addr: EndpointAddr,
    workspace: String,
    /// What every request carries of where this device serves the workspace, when it places the
    /// workspace's work on the device ([`ActionService::placing`]).
    placement: Option<Vec<u8>>,
    /// The agent this device's person handed every request to, when it is one
    /// ([`ActionService::remote_for_agent`]).
    agent: Option<neighbour_wire::actions::HandedAgent>,
    target: [u8; 32],
    key: Arc<dyn DeviceSigner>,
    runtime: tokio::runtime::Handle,
    /// The connection to this device, kept while it lasts: one per pair.
    connection: Mutex<Option<Connection>>,
    next_correlation: AtomicU64,
    follow: Following,
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
        let connection = tokio::time::timeout(
            REPLY_DEADLINE,
            self.endpoint.connect(self.addr.clone(), ACTIONS_ALPN),
        )
        .await
        .map_err(|_| {
            tilo_actions::remote::ActionCallError::unavailable("the device did not answer")
        })?
        .map_err(|error| tilo_actions::remote::ActionCallError::unavailable(error.to_string()))?;
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
                agent: self.agent.clone(),
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
    /// answer back.
    async fn exchange(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<u8>, tilo_actions::remote::ActionCallError> {
        let connection = self.connection().await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(|error| {
            tilo_actions::remote::ActionCallError::unavailable(error.to_string())
        })?;
        write_frame(&mut send, bytes)
            .await
            .map_err(tilo_actions::remote::ActionCallError::unavailable)?;
        send.finish().map_err(|error| {
            tilo_actions::remote::ActionCallError::unavailable(error.to_string())
        })?;
        read_frame(&mut recv)
            .await
            .map_err(tilo_actions::remote::ActionCallError::unavailable)?
            .ok_or_else(|| {
                tilo_actions::remote::ActionCallError::unavailable(
                    "the device closed without answering",
                )
            })
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

    /// The person put `request` to on the device, where an agent this device handed its requests
    /// to works: that device asks them as it asks about any of its agents' steps.
    fn confirm(
        &self,
        _caller: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::ConfirmedInvocation, tilo_actions::remote::ActionCallError>,
    > {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.ask(
            RemoteActionMessage::Confirm {
                correlation,
                request: Box::new(request.clone()),
            },
            |answer| match answer {
                RemoteActionMessage::ConfirmResponse { result, .. } => result.map(|answer| *answer),
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

async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    crate::frames::write_frame(send, bytes).await
}

async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, String> {
    crate::frames::read_frame(recv, MAX_FRAME_BYTES).await
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
