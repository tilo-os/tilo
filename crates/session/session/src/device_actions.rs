//! This device's actions between the person's devices: what another of their devices asks this one
//! for, and what an app here asks of them.
//! The session's collaboration endpoint serves
//! [`ActionService`](tilo_peer_state::actions::ActionService): a request another device of one of this
//! person's workspaces sends is checked against that workspace's roster as it stands, and the
//! action it names runs here through the broker as any other caller's would. The other way
//! round, an application names another of the person's devices and one of its own actions;
//! [`OwnDeviceActions`] sends it there as an ordinary invocation of that action and follows it
//! until that device answers, so the app observes it like an invocation of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_actions::offload::ActionOffload;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    ConfirmationQuestion, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest,
    Pending, own_device, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_broker_core::services::{OwnDeviceActions, OwnDeviceOutcome};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{
    ActionService, FOLLOW_DEADLINE, ReadFuture, RemoteDevice, Workspaces,
};
use tilo_power_mgmt::InhibitToken;
use tilo_power_mgmt::inhibit::INHIBIT_SUSPEND;
use tilo_runtime::offload::{ActionServicePeers, turn_offload};

use crate::engine::Engine;

/// This device's action service, as the session serves it and reaches the person's other devices
/// through it.
#[derive(Clone)]
pub(crate) struct DeviceActions(Arc<Inner>);

struct Inner {
    action_service: ActionService,
    /// The endpoint the action service asks from, as this device.
    endpoint: Endpoint,
    /// Where this session was told each of the person's devices is
    /// ([`crate::sync::Client::connect`]), by its endpoint. A device it was told of is one it
    /// reaches now.
    addresses: Mutex<BTreeMap<EndpointId, EndpointAddr>>,
    /// The devices of each workspace this session reaches now, as its topic and its hosts say
    /// ([`Self::here`]): the ones asked to take a turn of work.
    here: Mutex<BTreeMap<String, BTreeSet<[u8; 32]>>>,
    /// What answers a request that arrived here.
    dispatch: DeviceDispatch,
    /// The runtime the action service answers on, where its asks and runs are carried out.
    runtime: tokio::runtime::Handle,
}

impl DeviceActions {
    /// This device's action service on the session's `endpoint` , authorizing against the workspaces
    /// `engine` holds, answering through the dispatcher the session installs once its broker
    /// serves.
    pub(crate) fn start(
        identity: &tilo_network_actor::DeviceIdentity,
        endpoint: Endpoint,
        engine: Option<Arc<Engine>>,
    ) -> Self {
        let dispatch = DeviceDispatch::for_user(identity.device_cert.user_public_key);
        let action_service = ActionService::over(
            Arc::clone(&identity.signer),
            Arc::new(dispatch.clone()),
            Arc::new(HeldWorkspaces(engine)),
        );
        Self(Arc::new(Inner {
            action_service,
            endpoint,
            addresses: Mutex::default(),
            here: Mutex::default(),
            dispatch,
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// The action service the collaboration endpoint serves.
    pub(crate) fn action_service(&self) -> ActionService {
        self.0.action_service.clone()
    }

    /// Answer arriving requests through `host` from now on: the broker's own, once it serves.
    pub(crate) fn install(&self, provider: Arc<dyn ActionProvider + Send + Sync>) {
        self.0.dispatch.install(provider);
    }

    /// Answer the governance actions of the workspaces this device holds through `governance`
    /// from now on: its own workspace service's, which this action service answers in place of.
    pub(crate) fn govern_with(&self, governance: Arc<dyn ActionProvider + Send + Sync>) {
        *self.0.dispatch.0.governance.lock_recover() = Some(governance);
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

    /// The route an offloadable invocation takes: the workspace's devices connected here are asked
    /// through this action service to take its turn, the one that does runs it, and the content it
    /// produced is kept in `content`, the store the app that invoked it reads its output from.
    /// `None` when offload is turned off on this device.
    pub(crate) fn offload_route(
        &self,
        content: Arc<crate::content::ContentService>,
    ) -> Option<Arc<dyn ActionOffload>> {
        let actions = self.clone();
        let media = content.media();
        let outputs: Arc<dyn tilo_runtime::offload::OutputContent> = content;
        let asking = self.clone();
        let peers = ActionServicePeers::new(
            self.0.action_service.clone(),
            self.0.endpoint.clone(),
            move |id| actions.addr(id),
            outputs,
            self.0.runtime.clone(),
        )
        .reachable(move |workspace, device| asking.reachable(workspace, device));
        turn_offload(media, Arc::new(peers))
    }

    /// Where `id` of the person's devices is reached: the address this session was told, else
    /// the endpoint's own discovery of its key.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        self.0
            .addresses
            .lock_recover()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id))
    }

    /// The device an app's action is for, with where it is reached.
    fn device(&self, key: [u8; 32], workspace: &str) -> Result<Arc<RemoteDevice>, ActionCallError> {
        let id = EndpointId::from_bytes(&key).map_err(|_| ActionCallError::InvalidRequest {
            reason: "that is not a device key".into(),
        })?;
        Ok(Arc::new(self.0.action_service.remote(
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
    ) -> Result<Pending<OwnDeviceOutcome>, ActionCallError> {
        // The workspace the two devices are to share: the one the app acts in, which is where the
        // other device decides whether this one may act at all.
        let Some(workspace) = caller.workspace() else {
            return Err(ActionCallError::NotPermitted);
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
    ) -> Result<Pending<Result<Confirmation, ActionCallError>>, ActionCallError> {
        // A person's own word is asked of them where they are, by the device in front of them.
        // Reaching another of their devices to ask there is no part of this action service: nothing carries
        // a question to it.
        Err(ActionCallError::Unavailable {
            reason: "this device does not ask the person on another of their devices".into(),
        })
    }
}

/// What the other device's answer comes to: the outcome the app on this device observes as its own
/// invocation's.
fn outcome_of(answer: Option<Result<InvocationUpdate, ActionCallError>>) -> OwnDeviceOutcome {
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

/// The one place a request that arrived on this device is handed to: the broker, once it
/// serves. Until then a request is answered as unavailable rather than left waiting, and so after
/// the broker retires.
/// It also counts the requests from the person's other devices as use of this one, holding the
/// machine awake for a lease after each.
#[derive(Clone)]
struct DeviceDispatch(Arc<DispatchInner>);

struct DispatchInner {
    /// What answers a request that arrived here.
    provider: Mutex<Option<Arc<dyn ActionProvider + Send + Sync>>>,
    /// What answers a member's ask of the governance of a workspace this device holds.
    governance: Mutex<Option<Arc<dyn ActionProvider + Send + Sync>>>,
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
            provider: Mutex::default(),
            governance: Mutex::default(),
            user,
            power: Mutex::default(),
            remote_use: Mutex::default(),
        }))
    }

    fn install(&self, provider: Arc<dyn ActionProvider + Send + Sync>) {
        *self.0.provider.lock_recover() = Some(provider);
    }

    fn attach_power(&self, power: tilo_session_power::PowerLink) {
        *self.0.power.lock_recover() = Some(power);
    }

    fn provider(&self) -> Result<Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        self.0
            .provider
            .lock_recover()
            .clone()
            .ok_or_else(|| ActionCallError::Unavailable {
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

impl DeviceDispatch {
    /// Run `act` on what answers a request here: the broker, once it serves.
    fn through<T: Send + 'static>(
        &self,
        caller: &Caller,
        act: impl FnOnce(
            &Arc<dyn ActionProvider + Send + Sync>,
            &Caller,
        ) -> Pending<Result<T, ActionCallError>>,
    ) -> Pending<Result<T, ActionCallError>> {
        match self.provider() {
            Ok(provider) => act(&provider, caller),
            Err(error) => Pending::ready(Err(error)),
        }
    }
}

impl ActionProvider for DeviceDispatch {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.through(caller, move |host, caller| host.query(caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.note_remote_use(caller);
        // The workspaces' governance, and the turns of work this device takes, are its workspace
        // service's to answer.
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            || request.target.provider == tilo_workspace_service::work::WORK_PROVIDER
        {
            let governance = self.0.governance.lock_recover().clone();
            return match governance {
                Some(governance) => governance.invoke(caller, request),
                None => Pending::ready(Err(ActionCallError::UnknownAction)),
            };
        }
        self.through(caller, move |host, caller| host.invoke(caller, request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let governance = self.0.governance.lock_recover().clone();
        self.provider()
            .ok()?
            .observe(caller, id)
            .or_else(|| governance?.observe(caller, id))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.provider().ok()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let id = *id;
        self.through(caller, move |host, caller| host.cancel(caller, &id))
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        let request = request.clone();
        self.through(caller, move |host, caller| host.confirm(caller, &request))
    }
}

/// The workspaces this device holds, as the action service reads them: this session's own store, asked when
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

#[cfg(test)]
mod tests {
    //! Two of the person's devices on this machine's loopback: an action an app invokes for the
    //! other device runs there through the session's own action service and answers here, a device the
    //! workspace does not hold is refused, and a piece of work no device connected here takes runs
    //! here.

    tilo_automerge_isolate::test_worker!();

    use std::future::Future;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::pin::Pin;
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use tilo_actions::offload::{OffloadRequest, Offloaded};
    use tilo_actions::remote::{
        InvokeArgument, TestProvider, application_action_target, own_device,
    };
    use tilo_actions::{ActionAddress, ActionOutput, Offload};
    use tilo_broker_core::route::CrdtRoute;
    use tilo_content::hash_bytes;
    use tilo_network_actor::DeviceIdentity;
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
    use tilo_user_identity::DeviceCert;

    use super::*;

    const WORKSPACE: &str = testing::WORKSPACE;
    /// How long the device asked has to answer before the test fails.
    const PATIENCE: Duration = Duration::from_secs(20);
    const APP: &str = "so.tilo.notes";
    /// The package whose transform a test's action declares.
    const PACKAGE: &str = "so.tilo.images";
    const ENTRYPOINT: &str = "thumbnail";

    /// A device key certified by `root` as one of its person's devices.
    fn device(root: &SigningKey, seed: u8, label: &str) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let cert = testing::certificate(root, &key, label);
        (key, cert)
    }

    fn identity(cert: DeviceCert, key: &SigningKey) -> DeviceIdentity {
        DeviceIdentity::kept(cert, key.clone())
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
    ///
    /// Boxed: setting one up holds an engine, a workspace and another device's admission of it,
    /// so the future is larger than a test's stack frame should carry.
    fn answering<'a>(
        key: &'a SigningKey,
        cert: &'a DeviceCert,
        asking: Option<&'a (SigningKey, DeviceCert)>,
        provider: Arc<TestProvider>,
    ) -> Pin<Box<dyn Future<Output = (Endpoint, Router, DeviceActions)> + 'a>> {
        Box::pin(async move {
            let directory = tempfile::TempDir::new().expect("a state directory");
            let id = identity(cert.clone(), key);
            let engine = Arc::new(
                Engine::open(
                    id.clone(),
                    Some(directory.path()),
                    std::iter::empty(),
                    crate::platform::membership_check::of_this_session(),
                )
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
                            tilo_peer_state::TransportOrigin::RetainedServerDelta(
                                cert.device_public_key,
                            ),
                            postcard::from_bytes(bytes).expect("a workspace change"),
                            now,
                        )
                        .expect("this device takes the workspace as its governor has it");
                }
            }
            let endpoint = endpoint(key).await;
            let actions = DeviceActions::start(&id, endpoint.clone(), Some(engine));
            actions.install(provider);
            let router = Router::builder(endpoint.clone())
                .accept(
                    tilo_peer_state::actions::ACTIONS_ALPN,
                    actions.action_service(),
                )
                .spawn();
            (endpoint, router, actions)
        })
    }

    /// The session of the device `key` that asks: its own action service, nothing to answer, and where the
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
            signed: None,
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
        let provider = Arc::new(TestProvider::new());
        let (there_endpoint, router, _) =
            answering(&there.0, &there.1, Some(&here), provider.clone()).await;
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
        let asked = provider.asked();
        assert_eq!(asked.len(), 1, "one request reached the other device");
        assert_eq!(asked[0].invocation, request.invocation, "under its own id");
        assert_eq!(asked[0].target.address.action, "insert");
        assert_eq!(
            tilo_actions::remote::application_provider(&asked[0].target.provider),
            Some((WORKSPACE, APP)),
            "the app's action, in the workspace the two devices share"
        );
        let seen = provider.callers();
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
    async fn a_device_the_workspace_does_not_hold_is_refused() {
        let root = SigningKey::from_bytes(&[0x41; 32]);
        let there = device(&root, 0x42, "The other device");
        let stranger = device(
            &root,
            0x43,
            "A device of the same person, not in the workspace",
        );
        let provider = Arc::new(TestProvider::new());
        let (there_endpoint, router, _) =
            answering(&there.0, &there.1, None, provider.clone()).await;
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
            provider.asked().is_empty(),
            "the action never ran on the device it named"
        );

        router.shutdown().await.expect("the router stops");
        stranger_endpoint.close().await;
    }

    /// The photograph the transform reads.
    fn photograph() -> Vec<u8> {
        (0..8 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 251).expect("a byte"))
            .collect()
    }

    /// An offloadable invocation of `APP` 's action, reading `inputs` : the request the broker
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
            signed: None,
        };
        (request, sent)
    }

    /// A device of the workspace that is not connected here now is not asked,
    /// though this session knows where it lives: nobody is asked, so the action
    /// runs here without waiting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_of_the_workspace_that_is_away_is_not_asked() {
        let root = SigningKey::from_bytes(&[0x91; 32]);
        let here = device(&root, 0x92, "This device");
        let away = device(&root, 0x93, "A device that is away");
        let (away_endpoint, away_router, _) =
            answering(&away.0, &away.1, Some(&here), Arc::new(TestProvider::new())).await;
        let (here_endpoint, here_router, actions) =
            answering(&here.0, &here.1, Some(&away), Arc::new(TestProvider::new())).await;
        // Where the other device is, and nothing saying it is connected.
        actions.learn(addr(&away_endpoint));

        let content = crate::content::ContentService::open(None);
        let photograph = photograph();
        let input = hash_bytes(&photograph);
        content
            .route()
            .offer(&CrdtRoute::app(APP, WORKSPACE), &input, &photograph)
            .expect("the app's photograph is kept here");
        let route = actions
            .offload_route(Arc::clone(&content))
            .expect("offload is on");

        let (request, sent) = offloadable(vec![input]);
        let placed = tokio::time::timeout(PATIENCE, route.offload_invocation(request, sent))
            .await
            .expect("a device that is away is not waited for");
        let Offloaded::RunHere { reason } = placed else {
            panic!("nobody connected was asked: {placed:?}");
        };
        assert_eq!(reason, "no other device of the workspace is reachable");

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
        // This session holds no workspace, so it asks nobody.
        let actions =
            DeviceActions::start(&identity(here.1.clone(), &here.0), endpoint.clone(), None);
        let content = crate::content::ContentService::open(None);
        let photograph = photograph();
        let input = hash_bytes(&photograph);
        content
            .route()
            .offer(&CrdtRoute::app(APP, WORKSPACE), &input, &photograph)
            .expect("the app's photograph is kept here");
        let route = actions.offload_route(content).expect("offload is on");

        // Nobody is in the workspace to take it, so it runs here, and says why.
        let (request, sent) = offloadable(vec![input]);
        let placed = route.offload_invocation(request, sent).await;
        let Offloaded::RunHere { reason } = placed else {
            panic!("nobody took it, so it runs here: {placed:?}");
        };
        assert_eq!(reason, "no other device of the workspace is reachable");
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

    /// A failure this device names for an answer of the other device's that brought no outcome of
    /// the action's own: one the app may try again.
    fn ours(code: &str, message: &str) -> OwnDeviceOutcome {
        Err(Box::new(ActionFailure {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
            retryable: true,
        }))
    }

    /// The failure this device names for `update`, an answer that ended the following without
    /// settling the action.
    fn unsettled(update: &InvocationUpdate) -> OwnDeviceOutcome {
        ours(
            "unsettled",
            &format!("the action on that device did not settle: {update:?}"),
        )
    }

    /// What the app here observes for each way the other device's answer can end: the action's
    /// output or its own failure exactly as that device gave them, and a failure this device names
    /// for an answer that was cancelled, came out of turn, was refused, or never arrived.
    #[test]
    fn every_answer_of_the_other_device_is_the_outcome_the_app_observes() {
        let invocation = InvocationId::new();
        let output = ActionOutput {
            value: Some(serde_json::json!({"appended": 3})),
            resources: vec!["notes.note:1".to_owned()],
        };
        let theirs = ActionFailure {
            code: "note_locked".to_owned(),
            message: "the note is locked there".to_owned(),
            details: Some(serde_json::json!({"note": "1"})),
            retryable: false,
        };
        let stopped = Some("the person stopped it".to_owned());
        let accepted = InvocationUpdate::Accepted { invocation };
        let item = InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!("a line"),
        };
        let unavailable = ActionCallError::Unavailable {
            reason: "this device is not serving its actions".to_owned(),
        };
        let rows = [
            (
                "succeeded",
                Some(Ok(InvocationUpdate::Succeeded {
                    invocation,
                    output: output.clone(),
                })),
                Ok(output),
            ),
            (
                "failed there",
                Some(Ok(InvocationUpdate::Failed {
                    invocation,
                    error: theirs.clone(),
                })),
                Err(Box::new(theirs)),
            ),
            (
                "cancelled with a word",
                Some(Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message: stopped,
                })),
                ours("cancelled", "the person stopped it"),
            ),
            (
                "cancelled without one",
                Some(Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                })),
                ours("cancelled", "the action was cancelled on that device"),
            ),
            (
                "accepted, and no more",
                Some(Ok(accepted.clone())),
                unsettled(&accepted),
            ),
            (
                "an item, and no more",
                Some(Ok(item.clone())),
                unsettled(&item),
            ),
            (
                "not permitted there",
                Some(Err(ActionCallError::NotPermitted)),
                ours(
                    "own_device_refused",
                    "the caller may not invoke this action",
                ),
            ),
            (
                "not serving there",
                Some(Err(unavailable)),
                ours(
                    "own_device_refused",
                    "remote actions are unavailable: this device is not serving its actions",
                ),
            ),
            (
                "never answered",
                None,
                ours(
                    "own_device_abandoned",
                    "the device's answer was dropped before it arrived",
                ),
            ),
        ];
        for (row, answer, expected) in rows {
            assert_eq!(outcome_of(answer), expected, "{row}");
        }
    }

    /// A device is reached in the workspace that reports it connected and in no other, and only
    /// while that report stands: what one workspace says of a device is nothing another may ask
    /// it by, and knowing where a device lives is not reaching it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_is_reached_only_in_the_workspace_that_reports_it_and_while_it_does() {
        const ELSEWHERE: &str = "another workspace of the person's";
        let root = SigningKey::from_bytes(&[0xa1; 32]);
        let here = device(&root, 0xa2, "This device");
        let there = device(&root, 0xa3, "Their other device");
        let other = there.0.verifying_key().to_bytes();
        let stranger = [0xa4; 32];
        let lives = EndpointAddr::new(
            EndpointId::from_bytes(&other).expect("the other device's key is an endpoint's"),
        );
        let (endpoint, actions) = asking(&here.0, &here.1, lives).await;
        assert!(
            !actions.reachable(WORKSPACE, other),
            "told where the device lives, and by no workspace that it is connected"
        );

        actions.here(WORKSPACE, BTreeSet::from([other]));
        assert!(actions.reachable(WORKSPACE, other));
        assert!(
            !actions.reachable(ELSEWHERE, other),
            "a workspace that did not report the device does not reach it"
        );
        assert!(
            !actions.reachable(WORKSPACE, stranger),
            "nor does the workspace reach a device it did not report"
        );

        actions.here(ELSEWHERE, BTreeSet::from([stranger]));
        assert!(
            actions.reachable(WORKSPACE, other) && !actions.reachable(WORKSPACE, stranger),
            "what another workspace reports changes nothing of this one"
        );
        assert!(actions.reachable(ELSEWHERE, stranger));

        // A workspace's next report replaces what was known of it, and of it alone.
        actions.here(WORKSPACE, BTreeSet::new());
        assert!(
            !actions.reachable(WORKSPACE, other),
            "a device that left is not reached"
        );
        assert!(actions.reachable(ELSEWHERE, stranger));
        endpoint.close().await;
    }
}
