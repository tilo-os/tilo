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
    ActionService, FOLLOW_DEADLINE, Offering, ReadFuture, RemoteDevice, Workspaces,
};
use tilo_power_mgmt::InhibitToken;
use tilo_power_mgmt::inhibit::INHIBIT_SUSPEND;
use tilo_runtime::offload::{ActionServicePeers, offer_offload, own_offering};
use tilo_runtime::other_devices::{OtherDevices, Whereabouts};
use tilo_server_protocol::{Whose, WorkSetting};

use crate::device_agents::{HandedAgents, Host};
use crate::engine::Engine;
use neighbour_wire::offer::{Answer, Query};

/// This device's action service, as the session serves it and reaches the person's other devices
/// through it.
#[derive(Clone)]
pub(crate) struct DeviceActions(Arc<Inner>);

/// The workspaces whose devices this session heard of, and which of them it reaches now.
impl Whereabouts for DeviceActions {
    fn workspaces(&self) -> Vec<String> {
        self.0.here.lock_recover().keys().cloned().collect()
    }

    fn reachable(&self, workspace: &str, device: [u8; 32]) -> bool {
        Self::reachable(self, workspace, device)
    }

    /// The address this session was told, else the endpoint's own discovery of its key.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        self.0
            .addresses
            .lock_recover()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id))
    }
}

struct Inner {
    action_service: ActionService,
    /// The endpoint the action service asks from, as this device.
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
    /// The runtime the action service answers on, where its asks and runs are carried out.
    runtime: tokio::runtime::Handle,
    /// The person this device is, and its own key.
    whose: ([u8; 32], [u8; 32]),
}

impl DeviceActions {
    /// This device's action service on the session's `endpoint` , authorizing against the workspaces
    /// `engine` holds, answering through the dispatcher the session installs once its broker
    /// serves, and offering work with what it keeps.
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
        // Another of the person's devices may ask which of them would run a piece of work: this one
        // answers with what it knows of itself.
        let offering = SessionOffering::default();
        action_service.offering(offering.clone());
        Self(Arc::new(Inner {
            action_service,
            endpoint,
            addresses: Mutex::default(),
            here: Mutex::default(),
            dispatch,
            offering,
            runtime: tokio::runtime::Handle::current(),
            whose: (
                identity.device_cert.user_public_key,
                identity.device_cert.device_public_key,
            ),
        }))
    }

    /// Work with the agents the person's other devices hand requests to through the MCP server
    /// whose pass `wake` starts, as sessions of `sessions` ([`crate::device_agents`]).
    pub(crate) fn hear_agents(
        &self,
        wake: tilo_service::Wake,
        sessions: Arc<crate::agent_sessions::AgentSessions>,
    ) -> Arc<HandedAgents> {
        let handed = HandedAgents::new(
            wake,
            sessions,
            self.0.action_service.clone(),
            self.0.runtime.clone(),
        );
        *self.0.dispatch.0.handed.lock_recover() = Some(Arc::clone(&handed));
        handed
    }

    /// The person's other devices, as an agent of this login works on one of them.
    pub(crate) fn other_devices(&self) -> OtherDevices {
        OtherDevices::new(
            self.0.action_service.clone(),
            self.0.endpoint.clone(),
            self.0.runtime.clone(),
            self.0.whose,
            Arc::new(self.clone()),
        )
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

    /// The content this session keeps, as its offer says: another device weighs a job whose inputs
    /// are already here as moving nothing.
    pub(crate) fn attach_content(&self, content: Arc<crate::content::ContentService>) {
        self.0.offering.holding(content);
    }

    /// The route an offloadable invocation takes where a device of its workspace would run it
    /// sooner: the round asks them through this action service, the invocation runs on the device that wins,
    /// and the content it produced is kept in `content` — the store the app that invoked it reads
    /// its output from. `None` when offload is turned off on this device.
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
        offer_offload(media, Arc::new(peers))
    }

    /// Where `id` of the person's devices is reached.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        Whereabouts::addr(self, id)
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
    /// The agents the person's other devices hand requests to, once this session's MCP server
    /// works with them.
    handed: Mutex<Option<Arc<HandedAgents>>>,
}

impl DeviceDispatch {
    fn for_user(user: [u8; 32]) -> Self {
        Self(Arc::new(DispatchInner {
            provider: Mutex::default(),
            governance: Mutex::default(),
            user,
            power: Mutex::default(),
            remote_use: Mutex::default(),
            handed: Mutex::default(),
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
    /// Run `act` on what answers `caller` here, as whom: an agent another of the person's devices
    /// handed its requests to as that agent's session here, through the broker connection its
    /// session has ([`crate::device_agents`]), refused where this session works with no such
    /// agents; anyone else as they are, through the broker.
    fn through<T: Send + 'static>(
        &self,
        caller: &Caller,
        act: impl FnOnce(&Host, &Caller) -> Pending<Result<T, ActionCallError>> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
        if !crate::device_agents::handed(caller) {
            return match self.provider() {
                Ok(provider) => act(&provider, caller),
                Err(error) => Pending::ready(Err(error)),
            };
        }
        let handed = self.0.handed.lock_recover().clone();
        match handed {
            Some(handed) => handed.through(caller, act),
            None => Pending::ready(Err(crate::device_agents::not_accepted())),
        }
    }

    /// What answers `caller` here already, as whom, for a read that does not wait
    /// ([`Self::through`]): nothing for a handed agent whose session does not work here yet.
    fn known(&self, caller: &Caller) -> Option<(Caller, Host)> {
        if !crate::device_agents::handed(caller) {
            return Some((caller.clone(), self.provider().ok()?));
        }
        self.0.handed.lock_recover().as_ref()?.known(caller)
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
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            && !crate::device_agents::handed(caller)
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
        let (caller, host) = self.known(caller)?;
        let governance = self.0.governance.lock_recover().clone();
        host.observe(&caller, id)
            .or_else(|| governance?.observe(&caller, id))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let (caller, host) = self.known(caller)?;
        host.next_change(&caller, id)
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

/// What this session says of itself when another of the person's devices offers it work: what the
/// device's work setting takes, and which of the job's inputs it keeps — a device that has them is
/// not weighed as receiving them, and one that keeps no content weighs every input as moving here.
#[derive(Clone)]
struct SessionOffering {
    /// The work this device takes. A session runs on a machine someone is at, so it takes its own
    /// person's work where the setting says the machine has room for it.
    work: WorkSetting,
    content: Arc<Mutex<Option<Arc<crate::content::ContentService>>>>,
}

impl Default for SessionOffering {
    fn default() -> Self {
        Self {
            work: WorkSetting::own_person(),
            content: Arc::default(),
        }
    }
}

impl SessionOffering {
    /// The content this session keeps, from now on: what its answer reports holding.
    fn holding(&self, content: Arc<crate::content::ContentService>) {
        *self.content.lock_recover() = Some(content);
    }
}

impl Offering for SessionOffering {
    fn report(&self, _from: [u8; 32], whose: &Whose, offer: &Query) -> Answer {
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
        own_offering(&self.work, whose, &offer.route.app, holds)
    }
}

#[cfg(test)]
mod tests {
    //! Two of the person's devices on this machine's loopback: an action an app invokes for the
    //! other device runs there through the session's own action service and answers here, a device the
    //! workspace does not hold is refused, a piece of work runs on the device that offers and holds
    //! its inputs, and one nobody offers for runs here.

    tilo_automerge_isolate::test_worker!();

    use std::future::Future;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::pin::Pin;
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use neighbour_wire::offer::{Decline, Power, Rank, Route, Work};
    use tilo_actions::offload::{OffloadRequest, Offloaded};
    use tilo_actions::remote::{
        InvokeArgument, TestProvider, application_action_target, application_provider, own_device,
    };
    use tilo_actions::{ActionAddress, ActionOutput, CONTENT_RESOURCE_KIND, Offload};
    use tilo_broker_core::route::CrdtRoute;
    use tilo_content::hash_bytes;
    use tilo_network_actor::{DeviceIdentity, RecordContentDelivery};
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
    use tilo_user_identity::DeviceCert;
    use tilo_workspace_service::content::ContentState;

    use super::*;
    use crate::sync::Cmd;

    const WORKSPACE: &str = testing::WORKSPACE;
    /// How long the device asked has to answer before the test fails.
    const PATIENCE: Duration = Duration::from_secs(20);
    const APP: &str = "so.tilo.notes";
    /// The package whose transform a test's action declares.
    const PACKAGE: &str = "so.tilo.images";
    const ENTRYPOINT: &str = "thumbnail";
    /// How long the transform last took on the device that asked: far longer than moving its
    /// photograph to another device takes, so that a device holding the photograph is one that
    /// would run it sooner however slow the day's round trip is.
    const LOCAL_RUN: Duration = Duration::from_secs(60);

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
    async fn the_session_says_which_of_the_persons_devices_would_take_work() {
        let root = SigningKey::from_bytes(&[0x51; 32]);
        let here = device(&root, 0x52, "This device");
        let there = device(&root, 0x53, "The other device");
        let provider = Arc::new(TestProvider::new());
        let (there_endpoint, router, _) =
            answering(&there.0, &there.1, Some(&here), provider).await;
        let (here_endpoint, actions) = asking(&here.0, &here.1, addr(&there_endpoint)).await;
        let route = Route::new("so.tilo.nowhere", "notes.root", "insert");
        let work = Work {
            local_wall_ms: Some(10_000),
            ..Work::none()
        };

        let choice = tokio::time::timeout(
            PATIENCE,
            actions.action_service().offers(
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

    /// A device's content, and what it said about it, waiting to be carried to the other device as
    /// the workspace's hosts carry it.
    struct Content {
        action_service: Arc<crate::content::ContentService>,
        address: EndpointId,
        said: tokio::sync::mpsc::UnboundedReceiver<crate::sync::HostCmd>,
    }

    impl Content {
        /// The content store of the session whose endpoint is `key` 's, as the workspace's topic
        /// names it.
        fn open(key: &SigningKey) -> Self {
            let address = SecretKey::from_bytes(&key.to_bytes()).public();
            let action_service = crate::content::ContentService::open(None);
            let (say, said) = tokio::sync::mpsc::unbounded_channel::<crate::sync::HostCmd>();
            action_service.bind(crate::sync::HostOutbox::of(say), address);
            Self {
                action_service,
                address,
                said,
            }
        }

        /// The bytes an app keeps as `app` 's content in `WORKSPACE` .
        fn keep(&self, app: &str, bytes: &[u8]) -> String {
            let hash = hash_bytes(bytes);
            self.action_service
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
        said: &mut tokio::sync::mpsc::UnboundedReceiver<crate::sync::HostCmd>,
        to: &crate::content::ContentService,
    ) {
        while let Ok(outgoing) = said.try_recv() {
            match outgoing {
                crate::sync::HostCmd::Network(Cmd::Publish { ws, msg }) => {
                    to.heard(ws, WORKSPACE, from, &msg, &|_| true);
                }
                crate::sync::HostCmd::Content(RecordContentDelivery { ws, msg, .. }) => {
                    to.heard(ws, WORKSPACE, from, &msg, &|_| true);
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

    /// The session's own answer, for a device that has the app installed and is on mains power:
    /// what one of the person's devices that would take the work says, without a test depending on
    /// what this machine has installed or how it is powered.
    struct Willing(SessionOffering);

    impl Offering for Willing {
        fn report(&self, from: [u8; 32], whose: &Whose, offer: &Query) -> Answer {
            let answer = self.0.report(from, whose, offer);
            Answer::taking(
                Rank {
                    power: Power::External,
                    load_permille: 0,
                    latency_ms: 0,
                },
                answer.holds,
            )
        }
    }

    /// What each of two devices' content says, carried to the other as the workspace's hosts carry
    /// it, until the returned task is aborted: the two action services back, for the test to read.
    fn carrying(
        here: Content,
        there: Content,
    ) -> (
        Arc<crate::content::ContentService>,
        Arc<crate::content::ContentService>,
        tokio::task::JoinHandle<()>,
    ) {
        let addresses = [here.address, there.address];
        let mut said = [here.said, there.said];
        let action_services = [
            Arc::clone(&here.action_service),
            Arc::clone(&there.action_service),
        ];
        let pump = tokio::spawn(async move {
            loop {
                carry(addresses[0], &mut said[0], &action_services[1]);
                carry(addresses[1], &mut said[1], &action_services[0]);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        (here.action_service, there.action_service, pump)
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
            let kept = Arc::clone(&there_content.action_service);
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
        let provider = Arc::new(TestProvider::new().completing(written));
        let (there_endpoint, there_router, there_actions) =
            answering(&there.0, &there.1, Some(&here), Arc::clone(&provider)).await;
        // What that device says when offered the work: it has the app, and the photograph is
        // content it already keeps.
        let offering = SessionOffering::default();
        offering.holding(Arc::clone(&there_content.action_service));
        there_actions.action_service().offering(Willing(offering));

        let answer_with = Arc::new(TestProvider::new());
        let (here_endpoint, here_router, actions) =
            answering(&here.0, &here.1, Some(&there), answer_with).await;
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
        let (here_action_service, _, pump) = carrying(here_content, there_content);

        let route = actions
            .offload_route(Arc::clone(&here_action_service))
            .expect("offload is on");

        // Nothing has measured how long the transform takes here, so nothing is quicker than
        // running it here: the app's own handler runs it.
        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = route.offload_invocation(request.clone(), sent).await;
        assert!(
            matches!(placed, Offloaded::RunHere { .. }),
            "the first run is here: {placed:?}"
        );
        route.ran_here(&request, LOCAL_RUN);

        // The other device has the app and already keeps the photograph, and the run here took
        // longer than moving it: it offers, and the round picks it.
        let (request, sent) = offloadable(vec![input]);
        let placed =
            tokio::time::timeout(PATIENCE, route.offload_invocation(request, sent.clone()))
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
        let asked = provider.asked();
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
        let seen = provider.callers();
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
        let mut news = here_action_service.route().subscribe(&route);
        here_action_service.route().want(&route, &made);
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

        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = route.offload_invocation(request.clone(), sent).await;
        assert!(matches!(placed, Offloaded::RunHere { .. }), "{placed:?}");
        route.ran_here(&request, LOCAL_RUN);

        let (request, sent) = offloadable(vec![input]);
        let placed = tokio::time::timeout(PATIENCE, route.offload_invocation(request, sent))
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
        let content = crate::content::ContentService::open(None);
        let photograph = photograph();
        let input = hash_bytes(&photograph);
        content
            .route()
            .offer(&CrdtRoute::app(APP, WORKSPACE), &input, &photograph)
            .expect("the app's photograph is kept here");
        let route = actions.offload_route(content).expect("offload is on");

        // The app's own handler runs it here, and how long that took is what another device would
        // have to beat.
        let (request, sent) = offloadable(vec![input.clone()]);
        let placed = route.offload_invocation(request.clone(), sent).await;
        assert!(
            matches!(placed, Offloaded::RunHere { .. }),
            "the first run is here: {placed:?}"
        );
        route.ran_here(&request, LOCAL_RUN);

        // Nobody is in the workspace to offer, so it runs here again, and says why.
        let (request, sent) = offloadable(vec![input]);
        let placed = route.offload_invocation(request, sent).await;
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
