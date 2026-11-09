//! This device's actions between the person's devices: what another of their devices asks this one
//! for, and what an app here asks of them.
//! The session's collaboration endpoint serves
//! [`ActionPlane`](tilo_peer_state::actions::ActionPlane): a request another device of one of this
//! person's workspaces sends is checked against that workspace's roster as it stands, and the
//! action it names runs here through the broker's door as any other caller's would. The other way
//! round, an application names another of the person's devices and one of its own actions;
//! [`OwnDeviceActions`] sends it there as an ordinary invocation of that action and follows it
//! until that device answers, so the app observes it like an invocation of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_action_broker::services::{OwnDeviceActions, OwnDeviceOutcome};
use tilo_actions::offload::ActionOffload;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Confirmation,
    ConfirmationQuestion, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest,
    Pending, own_device, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{
    ActionPlane, FOLLOW_DEADLINE, Offering, ReadFuture, RemoteDevice, Workspaces,
};
use tilo_power_mgmt::InhibitToken;
use tilo_power_mgmt::inhibit::INHIBIT_SUSPEND;
use tilo_runtime::offload::{PlanePeers, offer_offload, own_offering};

use crate::engine::Engine;
use neighbour_wire::offer::{Answer, Query};

/// This device's action plane, as the session serves it and reaches the person's other devices
/// through it.
#[derive(Clone)]
pub(crate) struct DeviceActions(Arc<Inner>);

struct Inner {
    plane: ActionPlane,
    /// The endpoint the plane asks from, as this device.
    endpoint: Endpoint,
    /// Where this session was told each of the person's devices is
    /// ([`crate::sync::Client::connect`]), by its endpoint. A device it was told of is one it
    /// reaches now.
    addresses: Mutex<BTreeMap<EndpointId, EndpointAddr>>,
    /// The devices of each workspace this session reaches now, as its topic and its hosts say
    /// ([`Self::here`]): the ones an offer round asks.
    here: Mutex<BTreeMap<String, BTreeSet<[u8; 32]>>>,
    /// What answers a request that arrived here.
    dispatch: DeviceDispatch,
    /// What this session says of itself when another of the person's devices offers it work.
    offering: SessionOffering,
    /// The runtime the plane answers on, where its asks and runs are carried out.
    runtime: tokio::runtime::Handle,
}

impl DeviceActions {
    /// This device's plane on the session's `endpoint` , authorizing against the workspaces
    /// `engine` holds, answering through the dispatcher the session installs once its broker
    /// serves, and offering work with what it keeps.
    pub(crate) fn start(
        identity: &tilo_network_actor::HostIdentity,
        endpoint: Endpoint,
        engine: Option<Arc<Engine>>,
    ) -> Self {
        let dispatch = DeviceDispatch::for_user(identity.device_cert.user_public_key);
        let plane = ActionPlane::over(
            identity.device_signing_key.clone(),
            Arc::new(dispatch.clone()),
            Arc::new(HeldWorkspaces(engine)),
        );
        // Another of the person's devices may ask which of them would run a piece of work: this one
        // answers with what it knows of itself.
        let offering = SessionOffering::default();
        plane.offering(offering.clone());
        Self(Arc::new(Inner {
            plane,
            endpoint,
            addresses: Mutex::default(),
            here: Mutex::default(),
            dispatch,
            offering,
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// The plane the collaboration endpoint serves.
    pub(crate) fn plane(&self) -> ActionPlane {
        self.0.plane.clone()
    }

    /// Answer arriving requests through `host` from now on: the broker's own door, once it serves.
    pub(crate) fn install(&self, host: Arc<dyn ActionHost + Send + Sync>) {
        self.0.dispatch.install(host);
    }

    /// Hold this machine awake through `power` , the holds its idle timer reads, while another of
    /// the person's devices uses it: requests from there renew a suspend-only lease, so the machine
    /// does not fall asleep mid-task while the screen still blanks, the session still locks, and
    /// lid close and critical battery still suspend.
    pub(crate) fn attach_power(&self, power: tilo_session_power::PowerLink) {
        self.0.dispatch.attach_power(power);
    }

    /// Where one of the person's devices is, as this session was told.
    pub(crate) fn learn(&self, addr: EndpointAddr) {
        self.0.addresses.lock_recover().insert(addr.id, addr);
    }

    /// The devices of `workspace` this session reaches now, replacing what it knew: those heard on
    /// the workspace's topic and its hosts, never this device.
    pub(crate) fn here(&self, workspace: &str, devices: BTreeSet<[u8; 32]>) {
        self.0
            .here
            .lock_recover()
            .insert(workspace.to_owned(), devices);
    }

    /// Whether this session reaches the device `device` of `workspace` now: the workspace's topic
    /// or its hosts reported it connected here. A device that is away is not asked to run work, and
    /// the round does not wait for it, however well this session knows where it lives.
    pub(crate) fn reachable(&self, workspace: &str, device: [u8; 32]) -> bool {
        self.0
            .here
            .lock_recover()
            .get(workspace)
            .is_some_and(|devices| devices.contains(&device))
    }

    /// The content this session keeps, as its offer says: another device weighs a job whose inputs
    /// are already here as moving nothing.
    pub(crate) fn attach_content(&self, content: Arc<crate::content::Plane>) {
        self.0.offering.holding(content);
    }

    /// The route an offloadable invocation takes where a device of its workspace would run it
    /// sooner: the round asks them through this plane, the invocation runs on the device that wins,
    /// and the content it produced is kept in `content` — the store the app that invoked it reads
    /// its output from. `None` when offload is turned off on this device.
    pub(crate) fn offload_route(
        &self,
        content: Arc<crate::content::Plane>,
    ) -> Option<Arc<dyn ActionOffload>> {
        let actions = self.clone();
        let media = content.media();
        let outputs: Arc<dyn tilo_runtime::offload::OutputContent> = content;
        let asking = self.clone();
        let peers = PlanePeers::new(
            self.0.plane.clone(),
            self.0.endpoint.clone(),
            move |id| actions.addr(id),
            outputs,
            self.0.runtime.clone(),
        )
        .reachable(move |workspace, device| asking.reachable(workspace, device));
        offer_offload(media, Arc::new(peers))
    }

    /// Where `id` of the person's devices is reached: the address this session was told, else the
    /// endpoint's own discovery of its key.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        self.0
            .addresses
            .lock_recover()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id))
    }

    /// The device an app's action is for, with where it is reached.
    fn device(&self, key: [u8; 32], workspace: &str) -> Result<Arc<RemoteDevice>, ActionHostError> {
        let id = EndpointId::from_bytes(&key).map_err(|_| ActionHostError::InvalidRequest {
            reason: "that is not a device key".into(),
        })?;
        Ok(Arc::new(self.0.plane.remote(
            &self.0.endpoint,
            &self.addr(id),
            workspace,
        )))
    }
}

impl OwnDeviceActions for DeviceActions {
    fn invoke(
        &self,
        caller: &Caller,
        checked: &own_device::Checked,
        request: &InvokeRequest,
    ) -> Result<Pending<OwnDeviceOutcome>, ActionHostError> {
        // The workspace the two devices are to share: the one the app acts in, which is where the
        // other device decides whether this one may act at all.
        let Some(workspace) = caller.workspace() else {
            return Err(ActionHostError::NotPermitted);
        };
        let device = self.device(checked.device, workspace)?;
        let forwarded = checked.forwarded(workspace, request);
        let (complete, outcome) = pending();
        device
            .invoke_until_settled(forwarded, FOLLOW_DEADLINE)
            .then(move |answer| {
                complete.complete(outcome_of(answer));
            });
        Ok(outcome)
    }

    fn ask_person(
        &self,
        _caller: &Caller,
        _question: &ConfirmationQuestion,
    ) -> Result<Pending<Result<Confirmation, ActionHostError>>, ActionHostError> {
        // A person's own word is asked of them where they are, by the device in front of them.
        // Reaching another of their devices to ask there is no part of this plane: nothing carries
        // a question to it.
        Err(ActionHostError::Unavailable {
            reason: "this device does not ask the person on another of their devices".into(),
        })
    }
}

/// What the other device's answer comes to: the outcome the app on this device observes as its own
/// invocation's.
fn outcome_of(answer: Option<Result<InvocationUpdate, ActionHostError>>) -> OwnDeviceOutcome {
    let failure = |code: &str, message: String| {
        Box::new(ActionFailure {
            code: code.to_owned(),
            message,
            details: None,
            retryable: true,
        })
    };
    match answer {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => Ok(output),
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(Box::new(error)),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => Err(failure(
            "cancelled",
            message.unwrap_or_else(|| "the action was cancelled on that device".into()),
        )),
        // Following ends on a terminal update: anything else is a device answering out of turn.
        Some(Ok(update)) => Err(failure(
            "unsettled",
            format!("the action on that device did not settle: {update:?}"),
        )),
        Some(Err(error)) => Err(failure("own_device_refused", error.to_string())),
        None => Err(failure(
            "own_device_abandoned",
            "the device's answer was dropped before it arrived".into(),
        )),
    }
}

/// How long one request from another of the person's devices keeps this machine from
/// idle-suspending.
const REMOTE_USE_LEASE: Duration = Duration::from_secs(600);

/// The one place a request that arrived on this device is handed to: the broker's door, once it
/// serves. Until then a request is answered as unavailable rather than left waiting, and so after
/// the broker retires.
/// It also counts the requests from the person's other devices as use of this one, holding the
/// machine awake for a lease after each.
#[derive(Clone)]
struct DeviceDispatch(Arc<DispatchInner>);

struct DispatchInner {
    /// What answers a request that arrived here.
    host: Mutex<Option<Arc<dyn ActionHost + Send + Sync>>>,
    /// The person this device belongs to: what arrives from another of their devices is them using
    /// this one.
    user: [u8; 32],
    /// This machine's holds, once the session's power link serves.
    power: Mutex<Option<tilo_session_power::PowerLink>>,
    /// The suspend lease the person's other devices hold, renewed in place.
    remote_use: Mutex<Option<InhibitToken>>,
}

impl DeviceDispatch {
    fn for_user(user: [u8; 32]) -> Self {
        Self(Arc::new(DispatchInner {
            host: Mutex::default(),
            user,
            power: Mutex::default(),
            remote_use: Mutex::default(),
        }))
    }

    fn install(&self, host: Arc<dyn ActionHost + Send + Sync>) {
        *self.0.host.lock_recover() = Some(host);
    }

    fn attach_power(&self, power: tilo_session_power::PowerLink) {
        *self.0.power.lock_recover() = Some(power);
    }

    fn host(&self) -> Result<Arc<dyn ActionHost + Send + Sync>, ActionHostError> {
        self.0
            .host
            .lock_recover()
            .clone()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this device is not serving its actions".into(),
            })
    }

    /// A request from another of the person's own devices is use of this one: renew the
    /// suspend-only lease. The reason names what holds the machine in quick settings and in
    /// logind's inhibitor list.
    fn note_remote_use(&self, caller: &Caller) {
        if caller.is_foreign_device()
            || caller.user_public_key() != Some(&self.0.user)
            || caller.device_public_key().is_none()
        {
            return;
        }
        let Some(power) = self.0.power.lock_recover().clone() else {
            return;
        };
        let Some(until) = Instant::now().checked_add(REMOTE_USE_LEASE) else {
            return;
        };
        let reason = crate::i18n::t("remote-use-hold");
        let mut lease = self.0.remote_use.lock_recover();
        let renewed = lease
            .as_ref()
            .is_some_and(|lease| lease.renew_until(reason.clone(), until));
        if !renewed {
            *lease = Some(
                power
                    .registry()
                    .acquire_until(reason, INHIBIT_SUSPEND, until),
            );
        }
    }
}

impl ActionHost for DeviceDispatch {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        match self.host() {
            Ok(host) => host.query(caller, query),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.note_remote_use(caller);
        match self.host() {
            Ok(host) => host.invoke(caller, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host().ok()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.host().ok()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        match self.host() {
            Ok(host) => host.cancel(caller, id),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        match self.host() {
            Ok(host) => host.confirm(caller, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }
}

/// The workspaces this device holds, as the plane reads them: this session's own store, asked when
/// a request names one, so who may act there is what holds at that moment rather than as it was
/// when the session started.
struct HeldWorkspaces(Option<Arc<Engine>>);

impl Workspaces for HeldWorkspaces {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        let (Some(engine), Some(workspace_id)) = (&self.0, WorkspaceId::parse(workspace)) else {
            return Box::pin(std::future::ready(false));
        };
        // This session's workspace is its own store: the read takes the engine's lock for as long
        // as it runs, never across an await.
        Box::pin(async move {
            engine
                .owner()
                .with_current(workspace_id, |view| {
                    read(view.peer());
                    Ok(())
                })
                .is_ok()
        })
    }
}

/// What this session says of itself when another of the person's devices offers it work: whether it
/// has the app, how it is powered, how much work it already has in hand, and which of the job's
/// inputs it keeps — a device that has them is not weighed as receiving them, and one that keeps no
/// content weighs every input as moving here.
#[derive(Clone, Default)]
struct SessionOffering {
    content: Arc<Mutex<Option<Arc<crate::content::Plane>>>>,
}

impl SessionOffering {
    /// The content this session keeps, from now on: what its answer reports holding.
    fn holding(&self, content: Arc<crate::content::Plane>) {
        *self.content.lock_recover() = Some(content);
    }
}

impl Offering for SessionOffering {
    fn report(&self, _from: [u8; 32], offer: &Query) -> Answer {
        let content = self.content.lock_recover().clone();
        let holds = content.map_or_else(Vec::new, |content| {
            offer
                .work
                .inputs
                .iter()
                .map(|(digest, _)| *digest)
                .filter(|digest| content.holds(&offer.workspace, &offer.route.app, *digest))
                .collect()
        });
        own_offering(&offer.route.app, holds)
    }
}

#[cfg(test)]
mod tests {
    //! Two of the person's devices on this machine's loopback: an action an app invokes for the
    //! other device runs there through the session's own plane and answers here, a device the
    //! workspace does not hold is refused, a piece of work runs on the device that offers and holds
    //! its inputs, and one nobody offers for runs here.

    tilo_automerge_isolate::test_worker!();

    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use neighbour_wire::offer::{Decline, Power, Route, Work};
    use tilo_action_broker::record_content::ContentState;
    use tilo_action_broker::route::CrdtRoute;
    use tilo_actions::offload::{OffloadRequest, Offloaded};
    use tilo_actions::remote::{
        InvokeArgument, TestHost, application_action_target, application_provider, own_device,
    };
    use tilo_actions::{ActionAddress, ActionOutput, CONTENT_RESOURCE_KIND, Offload};
    use tilo_content::hash_bytes;
    use tilo_network_actor::{HostIdentity, RecordContentDelivery};
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
    use tilo_user_identity::DeviceCert;

    use super::*;
    use crate::platform::plane::Cmd;

    const WORKSPACE: &str = testing::WORKSPACE;
    /// How long the device asked has to answer before the test fails.
    const PATIENCE: Duration = Duration::from_secs(20);
    const APP: &str = "so.tilo.notes";
    /// The package whose transform a test's action declares.
    const PACKAGE: &str = "so.tilo.images";
    const ENTRYPOINT: &str = "thumbnail";
    /// How long the transform last took on the device that asked: longer than moving its photograph
    /// to another device takes, so a device that holds the photograph is one that would run it
    /// sooner.
    const LOCAL_RUN: Duration = Duration::from_millis(500);

    /// A device key certified by `root` as one of its person's devices.
    fn device(root: &SigningKey, seed: u8, label: &str) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let cert = testing::certificate(root, &key, label);
        (key, cert)
    }

    fn identity(cert: DeviceCert, key: &SigningKey) -> HostIdentity {
        HostIdentity::kept(cert, key.clone())
    }

    /// A loopback endpoint whose identity is the device `key` , as the session's own endpoint is.
    async fn endpoint(key: &SigningKey) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback endpoint binds")
            .bind()
            .await
            .expect("a loopback endpoint binds")
    }

    fn addr(endpoint: &Endpoint) -> EndpointAddr {
        let port = endpoint
            .bound_sockets()
            .first()
            .expect("a bound socket")
            .port();
        EndpointAddr::new(endpoint.id()).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    /// The session of the device `key` that answers requests, holding the workspace the person's
    /// device `asking` is in too, and answering through `host` .
    async fn answering(
        key: &SigningKey,
        cert: &DeviceCert,
        asking: Option<&(SigningKey, DeviceCert)>,
        host: Arc<TestHost>,
    ) -> (Endpoint, Router, DeviceActions) {
        let directory = tempfile::TempDir::new().expect("a state directory");
        let id = identity(cert.clone(), key);
        let engine = Arc::new(
            Engine::open(id.clone(), Some(directory.path()), std::iter::empty())
                .expect("this device's engine opens"),
        );
        // This device makes the workspace, and the other device of the person joins it: the device
        // certified in the workspace is the one that acts.
        let governor = TestGovernor::empty_interactive(cert.clone(), key.clone(), WORKSPACE)
            .expect("this device governs its workspace");
        let governor = testing::enroll_into(governor, engine.owner(), key, cert)
            .expect("this device's workspace is its own");
        if let Some(asking) = asking {
            let joining = tempfile::TempDir::new().expect("the other device's directory");
            let other = tilo_peer_state::owner::open_file(&joining.path().join("owner.json"))
                .expect("the other device's owner opens");
            let governor = testing::enroll_into(governor, &other, &asking.0, &asking.1)
                .expect("the workspace admits the other device");
            // What the workspace's admission of the other device changed reaches this device's own
            // copy of the workspace, as it would over the person's devices.
            let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace");
            let now = std::time::SystemTime::now();
            for (bytes, _) in governor.outgoing() {
                engine
                    .owner()
                    .receive(
                        workspace_id,
                        tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                            cert.device_public_key,
                        ),
                        postcard::from_bytes(bytes).expect("a workspace change"),
                        Some(key),
                        now,
                    )
                    .expect("this device takes the workspace as its governor has it");
            }
        }
        let endpoint = endpoint(key).await;
        let actions = DeviceActions::start(&id, endpoint.clone(), Some(engine));
        actions.install(host);
        let router = Router::builder(endpoint.clone())
            .accept(tilo_peer_state::actions::ACTIONS_ALPN, actions.plane())
            .spawn();
        (endpoint, router, actions)
    }

    /// The session of the device `key` that asks: its own plane, nothing to answer, and where the
    /// other device is.
    async fn asking(
        key: &SigningKey,
        cert: &DeviceCert,
        there: EndpointAddr,
    ) -> (Endpoint, DeviceActions) {
        let endpoint = endpoint(key).await;
        let actions = DeviceActions::start(&identity(cert.clone(), key), endpoint.clone(), None);
        actions.learn(there);
        (endpoint, actions)
    }

    /// What an app on this device asks: `action` of its own on `device` , in `WORKSPACE` .
    fn invocation(device: [u8; 32], action: &str) -> (own_device::Checked, InvokeRequest) {
        let argument = own_device::Invocation {
            device: hex::encode(device),
            app: APP.into(),
            resource: "notes.root".into(),
            action: action.into(),
            arguments: None,
        }
        .argument();
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: own_device::target(),
            argument: argument.clone(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        (
            own_device::Invocation::from_argument(&argument)
                .expect("an own-device invocation")
                .check()
                .expect("a valid own-device invocation"),
            request,
        )
    }

    /// The outcome `actions` observes for `request` , once the other device answers: `None` when it
    /// never does.
    fn answered(
        actions: &DeviceActions,
        caller: &Caller,
        checked: &own_device::Checked,
        request: &InvokeRequest,
    ) -> Option<OwnDeviceOutcome> {
        let pending = OwnDeviceActions::invoke(actions, caller, checked, request)
            .expect("the person here may act on their other device");
        pending.wait_timeout(PATIENCE).ok().flatten()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_action_for_another_of_the_persons_devices_runs_there_and_answers() {
        let root = SigningKey::from_bytes(&[0x31; 32]);
        let here = device(&root, 0x32, "This device");
        let there = device(&root, 0x33, "The other device");
        let host = Arc::new(TestHost::new());
        let (there_endpoint, router, _) =
            answering(&there.0, &there.1, Some(&here), host.clone()).await;
        let (here_endpoint, actions) = asking(&here.0, &here.1, addr(&there_endpoint)).await;
        let caller =
            Caller::authenticated(root.verifying_key().to_bytes(), here.1.device_public_key)
                .in_workspace(WORKSPACE);
        let (checked, request) = invocation(there.1.device_public_key, "insert");

        // The other device runs the app's action and this one hears its outcome as its own.
        let outcome = answered(&actions, &caller, &checked, &request)
            .expect("the other device answers within the patience");
        assert!(
            outcome.is_ok(),
            "the other device ran the action: {outcome:?}"
        );

        // It ran there, as this device, in the workspace both are in.
        let asked = host.asked();
        assert_eq!(asked.len(), 1, "one request reached the other device");
        assert_eq!(asked[0].invocation, request.invocation, "under its own id");
        assert_eq!(asked[0].target.address.action, "insert");
        assert_eq!(
            tilo_actions::remote::application_provider(&asked[0].target.provider),
            Some((WORKSPACE, APP)),
            "the app's action, in the workspace the two devices share"
        );
        let seen = host.callers();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].user_public_key(),
            Some(&root.verifying_key().to_bytes())
        );
        assert_eq!(
            seen[0].device_public_key(),
            Some(&here.1.device_public_key),
            "the device that asked, not the one that answered"
        );
        assert_eq!(seen[0].workspace(), Some(WORKSPACE));

        router.shutdown().await.expect("the router stops");
        here_endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_session_says_which_of_the_persons_devices_would_take_work() {
        let root = SigningKey::from_bytes(&[0x51; 32]);
        let here = device(&root, 0x52, "This device");
        let there = device(&root, 0x53, "The other device");
        let host = Arc::new(TestHost::new());
        let (there_endpoint, router, _) = answering(&there.0, &there.1, Some(&here), host).await;
        let (here_endpoint, actions) = asking(&here.0, &here.1, addr(&there_endpoint)).await;
        let route = Route::new("so.tilo.nowhere", "notes.root", "insert");
        let work = Work {
            local_wall_ms: Some(10_000),
            ..Work::none()
        };

        let choice = tokio::time::timeout(
            PATIENCE,
            actions.plane().offers(
                &here_endpoint,
                [addr(&there_endpoint)],
                WORKSPACE,
                &route,
                &work,
                tilo_actions::InvocationId::new(),
                PATIENCE,
            ),
        )
        .await
        .expect("the round finishes")
        .expect("the round completes");

        assert_eq!(choice.considered.len(), 1, "the one device asked: {choice}");
        // It answered with what it knows of itself: only a device that never answered is declined
        // for being away.
        assert_eq!(
            choice.considered[0].verdict,
            Err(Decline::AppNotInstalled),
            "the other session answered: {choice}"
        );
        assert_eq!(choice.worker, None, "no device here can run that app");

        router.shutdown().await.expect("the router stops");
        here_endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_the_room_does_not_hold_is_refused() {
        let root = SigningKey::from_bytes(&[0x41; 32]);
        let there = device(&root, 0x42, "The other device");
        let stranger = device(
            &root,
            0x43,
            "A device of the same person, not in the workspace",
        );
        let host = Arc::new(TestHost::new());
        let (there_endpoint, router, _) = answering(&there.0, &there.1, None, host.clone()).await;
        let (stranger_endpoint, actions) =
            asking(&stranger.0, &stranger.1, addr(&there_endpoint)).await;
        let caller = Caller::authenticated(
            root.verifying_key().to_bytes(),
            stranger.1.device_public_key,
        )
        .in_workspace(WORKSPACE);
        let (checked, request) = invocation(there.1.device_public_key, "insert");

        let refused = answered(&actions, &caller, &checked, &request)
            .expect("the other device answers within the patience");
        assert!(
            refused.is_err(),
            "a device the workspace has no part for acts nowhere in it: {refused:?}"
        );
        assert!(
            host.asked().is_empty(),
            "the action never ran on the device it named"
        );

        router.shutdown().await.expect("the router stops");
        stranger_endpoint.close().await;
    }

    /// A device's content, and what it said about it, waiting to be carried to the other device as
    /// the workspace's hosts carry it.
    struct Content {
        plane: Arc<crate::content::Plane>,
        address: EndpointId,
        said: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    }

    impl Content {
        /// The content plane of the session whose endpoint is `key` 's, as the workspace's topic
        /// names it.
        fn open(key: &SigningKey) -> Self {
            let address = SecretKey::from_bytes(&key.to_bytes()).public();
            let plane = crate::content::Plane::open(None);
            let (say, said) = tokio::sync::mpsc::unbounded_channel();
            plane.bind(say, address);
            Self {
                plane,
                address,
                said,
            }
        }

        /// The bytes an app keeps as `app` 's content in `WORKSPACE` .
        fn keep(&self, app: &str, bytes: &[u8]) -> String {
            let hash = hash_bytes(bytes);
            self.plane
                .route()
                .offer(&CrdtRoute::app(app, WORKSPACE), &hash, bytes)
                .expect("the app's content is kept");
            hash
        }
    }

    /// Hand what `said` about content to `to` , stamped with the device it came from: a want on the
    /// workspace's topic, and an answer to the one device that asked.
    fn carry(
        from: EndpointId,
        said: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        to: &crate::content::Plane,
    ) {
        while let Ok(outgoing) = said.try_recv() {
            match outgoing {
                Cmd::Publish { ws, msg } => {
                    to.heard(ws, WORKSPACE, from, &msg);
                }
                Cmd::Content(RecordContentDelivery { ws, msg, .. }) => {
                    to.heard(ws, WORKSPACE, from, &msg);
                }
                _ => {}
            }
        }
    }

    /// The photograph the transform reads: large enough that moving it to another device takes
    /// longer than the run here did.
    fn photograph() -> Vec<u8> {
        (0..8 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 251).expect("a byte"))
            .collect()
    }

    /// What the transform makes of it.
    fn thumbnail() -> Vec<u8> {
        (0..240_000_u32)
            .map(|index| u8::try_from(index % 241).expect("a byte"))
            .collect()
    }

    /// An offloadable invocation of `APP` 's action, reading `inputs` : the request the door
    /// weighs, and the invocation the app made.
    fn offloadable(inputs: Vec<String>) -> (OffloadRequest, InvokeRequest) {
        let request = OffloadRequest {
            workspace: WORKSPACE.into(),
            app_id: APP.into(),
            address: ActionAddress::new("photos", "thumbnail"),
            offload: Offload {
                app: PACKAGE.into(),
                entrypoint: ENTRYPOINT.into(),
                gpu: false,
                cpu_time_ms: 5_000,
                memory_bytes: 256 * 1024 * 1024,
                output_bytes: 8 * 1024 * 1024,
            },
            inputs,
        };
        let sent = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: application_action_target(WORKSPACE, APP, "photos", "thumbnail"),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        (request, sent)
    }

    /// The session's own answer, for a device that has the app installed and is on mains power:
    /// what one of the person's devices that would take the work says, without a test depending on
    /// what this machine has installed or how it is powered.
    struct Willing(SessionOffering);

    impl Offering for Willing {
        fn report(&self, from: [u8; 32], offer: &Query) -> Answer {
            let mut answer = self.0.report(from, offer);
            answer.state.app_installed = true;
            answer.state.critical_battery = false;
            answer.state.power = Power::External;
            answer.state.load_permille = 0;
            answer
        }
    }

    /// What each of two devices' content says, carried to the other as the workspace's hosts carry
    /// it, until the returned task is aborted: the two planes back, for the test to read.
    fn carrying(
        here: Content,
        there: Content,
    ) -> (
        Arc<crate::content::Plane>,
        Arc<crate::content::Plane>,
        tokio::task::JoinHandle<()>,
    ) {
        let addresses = [here.address, there.address];
        let mut said = [here.said, there.said];
        let planes = [Arc::clone(&here.plane), Arc::clone(&there.plane)];
        let pump = tokio::spawn(async move {
            loop {
                carry(addresses[0], &mut said[0], &planes[1]);
                carry(addresses[1], &mut said[1], &planes[0]);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        (here.plane, there.plane, pump)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_offloadable_action_runs_where_it_was_offered_and_its_inputs_are_kept() {
        let root = SigningKey::from_bytes(&[0x71; 32]);
        let here = device(&root, 0x72, "This device");
        let there = device(&root, 0x73, "The other device");
        // What the transform makes of the photograph, and where the device that ran it keeps it: on
        // that device's own content, in the app's scope in the workspace, as a run there leaves it.
        let produced = thumbnail();
        let made = hash_bytes(&produced);
        let there_content = Content::open(&there.0);
        let written = {
            let kept = Arc::clone(&there_content.plane);
            let (bytes, hash) = (produced.clone(), made.clone());
            move |_: &InvokeRequest| {
                kept.route()
                    .offer(&CrdtRoute::app(APP, WORKSPACE), &hash, &bytes)
                    .expect("the device that ran it keeps its output");
                ActionOutput {
                    value: None,
                    resources: vec![format!("{CONTENT_RESOURCE_KIND}:{hash}")],
                }
            }
        };
        let host = Arc::new(TestHost::new().completing(written));
        let (there_endpoint, there_router, there_actions) =
            answering(&there.0, &there.1, Some(&here), Arc::clone(&host)).await;
        // What that device says when offered the work: it has the app, and the photograph is
        // content it already keeps.
        let offering = SessionOffering::default();
        offering.holding(Arc::clone(&there_content.plane));
        there_actions.plane().offering(Willing(offering));

        let (here_endpoint, here_router, actions) =
            answering(&here.0, &here.1, Some(&there), Arc::new(TestHost::new())).await;
        actions.learn(addr(&there_endpoint));
        // The workspace's topic reports the other device connected here: it is one of the devices
        // the round asks.
        actions.here(
            WORKSPACE,
            std::collections::BTreeSet::from([there.1.device_public_key]),
        );
        let here_content = Content::open(&here.0);
        // The app's photograph is here, and on the other device too: what the round weighs as
        // moving nothing.
        let photograph = photograph();
        let input = here_content.keep(APP, &photograph);
        there_content.keep(APP, &photograph);

        // The two devices' content hears what the other says, as the workspace's hosts carry it.
        let (here_plane, _, pump) = carrying(here_content, there_content);

        let door = actions
            .offload_route(Arc::clone(&here_plane))
            .expect("offload is on");

        // Nothing has measured how long the transform takes here, so nothing is quicker than
        // running it here: the app's own handler runs it.
        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = door.offload_invocation(request.clone(), sent).await;
        assert!(
            matches!(placed, Offloaded::RunHere { .. }),
            "the first run is here: {placed:?}"
        );
        door.ran_here(&request, LOCAL_RUN);

        // The other device has the app and already keeps the photograph, and the run here took
        // longer than moving it: it offers, and the round picks it.
        let (request, sent) = offloadable(vec![input]);
        let placed = tokio::time::timeout(PATIENCE, door.offload_invocation(request, sent.clone()))
            .await
            .expect("the round finishes");
        let Offloaded::Ran { device, output, .. } = placed else {
            panic!("the other device runs it: {placed:?}");
        };
        assert_eq!(
            device,
            hex::encode(there.1.device_public_key),
            "the device that offered and holds the photograph"
        );

        // It ran there, as the invocation the app made, in the workspace both devices are in.
        let asked = host.asked();
        assert_eq!(asked.len(), 1, "one invocation reached the other device");
        assert_eq!(
            asked[0].invocation, sent.invocation,
            "under the id the app invoked it with"
        );
        assert_eq!(
            application_provider(&asked[0].target.provider),
            Some((WORKSPACE, APP)),
            "the app's action, in the workspace the two devices share"
        );
        let seen = host.callers();
        assert_eq!(
            seen[0].device_public_key(),
            Some(&here.1.device_public_key),
            "the device that invoked it, not the one that ran it"
        );

        // What the other device produced is content here, read back by the address the run
        // returned, exactly as a run here would leave it.
        assert_eq!(
            output.resources,
            vec![format!("{CONTENT_RESOURCE_KIND}:{made}")]
        );
        let route = CrdtRoute::app(APP, WORKSPACE);
        let mut news = here_plane.route().subscribe(&route);
        here_plane.route().want(&route, &made);
        let arrived = news
            .try_recv()
            .expect("this device keeps what the run produced");
        assert_eq!(arrived.state, ContentState::Resolved);
        assert_eq!(
            arrived.bytes, produced,
            "the output the app reads, in its own scope in the workspace"
        );

        pump.abort();
        here_router.shutdown().await.expect("the router stops");
        there_router.shutdown().await.expect("the router stops");
        here_endpoint.close().await;
        there_endpoint.close().await;
    }

    /// A device of the workspace that is not connected here now is not asked,
    /// though this session knows where it lives: the round has nobody to ask,
    /// so the action runs here without waiting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_of_the_workspace_that_is_away_is_not_asked() {
        let root = SigningKey::from_bytes(&[0x91; 32]);
        let here = device(&root, 0x92, "This device");
        let away = device(&root, 0x93, "A device that is away");
        let (away_endpoint, away_router, _) =
            answering(&away.0, &away.1, Some(&here), Arc::new(TestHost::new())).await;
        let (here_endpoint, here_router, actions) =
            answering(&here.0, &here.1, Some(&away), Arc::new(TestHost::new())).await;
        // Where the other device is, and nothing saying it is connected.
        actions.learn(addr(&away_endpoint));

        let content = crate::content::Plane::open(None);
        let photograph = photograph();
        let input = hash_bytes(&photograph);
        content
            .route()
            .offer(&CrdtRoute::app(APP, WORKSPACE), &input, &photograph)
            .expect("the app's photograph is kept here");
        let door = actions
            .offload_route(Arc::clone(&content))
            .expect("offload is on");

        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = door.offload_invocation(request.clone(), sent).await;
        assert!(matches!(placed, Offloaded::RunHere { .. }), "{placed:?}");
        door.ran_here(&request, LOCAL_RUN);

        let (request, sent) = offloadable(vec![input]);
        let placed = tokio::time::timeout(PATIENCE, door.offload_invocation(request, sent))
            .await
            .expect("a device that is away is not waited for");
        let Offloaded::RunHere { reason } = placed else {
            panic!("nobody connected was asked: {placed:?}");
        };
        assert!(reason.contains("no device offered to run it"), "{reason}");

        here_router.shutdown().await.expect("the router stops");
        away_router.shutdown().await.expect("the router stops");
        here_endpoint.close().await;
        away_endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_offloadable_action_nobody_offers_for_runs_here() {
        let root = SigningKey::from_bytes(&[0x81; 32]);
        let here = device(&root, 0x82, "This device");
        let endpoint = endpoint(&here.0).await;
        // This session holds no workspace, so its round asks nobody.
        let actions =
            DeviceActions::start(&identity(here.1.clone(), &here.0), endpoint.clone(), None);
        let content = crate::content::Plane::open(None);
        let photograph = photograph();
        let input = hash_bytes(&photograph);
        content
            .route()
            .offer(&CrdtRoute::app(APP, WORKSPACE), &input, &photograph)
            .expect("the app's photograph is kept here");
        let door = actions.offload_route(content).expect("offload is on");

        // The app's own handler runs it here, and how long that took is what another device would
        // have to beat.
        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = door.offload_invocation(request.clone(), sent).await;
        assert!(
            matches!(placed, Offloaded::RunHere { .. }),
            "the first run is here: {placed:?}"
        );
        door.ran_here(&request, LOCAL_RUN);

        // Nobody is in the workspace to offer, so it runs here again, and says why.
        let (request, sent) = offloadable(vec![input]);
        let placed = door.offload_invocation(request, sent).await;
        let Offloaded::RunHere { reason } = placed else {
            panic!("nobody offered, so it runs here: {placed:?}");
        };
        assert_eq!(reason, "no device offered to run it");
        endpoint.close().await;
    }

    /// When a lease taken at `now` lapses, and the moments either side of it.
    fn lease_ends(now: Instant) -> Instant {
        now.checked_add(REMOTE_USE_LEASE).expect("a lease deadline")
    }

    /// A request from another of the person's own devices is them using this machine: suspend is
    /// held off for a lease after it, nothing else is.
    #[test]
    fn a_request_from_another_of_the_persons_devices_holds_suspend_off() {
        use tilo_actions::remote::PendingPoll;
        use tilo_power_mgmt::inhibit::INHIBIT_IDLE;

        let user = [0x51; 32];
        let dispatch = DeviceDispatch::for_user(user);
        let power = tilo_session_power::PowerLink::default();
        dispatch.attach_power(power.clone());
        let registry = power.registry();
        let (_, request) = invocation([0x52; 32], "notes.append");
        let theirs = Caller::authenticated(user, [0x53; 32]);
        let now = Instant::now();
        let lapse = lease_ends(now);
        let mut answer = dispatch.invoke(&theirs, request.clone());
        assert!(
            matches!(answer.poll_ready(), PendingPoll::Ready(Err(_))),
            "no broker serves here yet"
        );
        assert!(registry.is_inhibited_for_at(INHIBIT_SUSPEND, now));
        assert!(
            !registry.is_inhibited_for_at(INHIBIT_IDLE, now),
            "the screen still blanks and locks"
        );
        assert_eq!(
            registry.active_reasons_for(INHIBIT_SUSPEND),
            vec![crate::i18n::t("remote-use-hold")],
            "quick settings and logind name what holds the machine"
        );
        assert!(
            registry.is_inhibited_for_at(
                INHIBIT_SUSPEND,
                lapse
                    .checked_sub(Duration::from_secs(1))
                    .expect("just before the lease lapses")
            ),
            "the lease is long enough for the next request"
        );
        assert!(
            !registry.is_inhibited_for_at(
                INHIBIT_SUSPEND,
                lapse
                    .checked_add(Duration::from_secs(1))
                    .expect("just after the lease lapses")
            ),
            "and lapses once they stop"
        );

        // The next request through the same device renews that one hold.
        let later = Instant::now();
        let _pending = dispatch.invoke(&theirs, request.clone());
        assert_eq!(registry.count(), 1, "one lease, renewed in place");
        assert!(
            registry.is_inhibited_for_at(
                INHIBIT_SUSPEND,
                lease_ends(later)
                    .checked_sub(Duration::from_secs(1))
                    .expect("just before the renewed lease lapses")
            ),
            "the renewal counts from the later request"
        );

        // Someone else's device, a foreign device, or a caller admitted here is not the person
        // using this machine from elsewhere.
        let nobody = DeviceDispatch::for_user(user);
        let power = tilo_session_power::PowerLink::default();
        nobody.attach_power(power.clone());
        let _pending = nobody.invoke(
            &Caller::authenticated([0x54; 32], [0x55; 32]),
            request.clone(),
        );
        let _pending = nobody.invoke(
            &Caller::authenticated(user, [0x56; 32]).from_foreign_device(true),
            request.clone(),
        );
        let _pending = nobody.invoke(&Caller::local_person("person"), request);
        assert!(!power.suspend_held(), "nothing of theirs holds the machine");
        assert!(nobody.0.remote_use.lock_recover().is_none());
    }
}
