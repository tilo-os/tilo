//! This device's actions between the person's devices: what another of
//! their devices asks this one for, and what an app here asks of them.
//!
//! The session's collaboration endpoint serves
//! [`ActionPlane`](tilo_peer_state::actions::ActionPlane): a request another
//! device of one of this person's workspaces sends is checked against that
//! room's roster as it stands, and the action it names runs here through the
//! broker's door as any other caller's would. The other way round, an
//! application names another of the person's devices and one of its own
//! actions; [`OwnDeviceActions`] sends it there as an ordinary invocation of
//! that action and follows it until that device answers, so the app observes
//! it like an invocation of its own.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_action_broker::services::{OwnDeviceActions, OwnDeviceOutcome};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Confirmation,
    ConfirmationQuestion, ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest,
    Pending, own_device, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_desktop_model::RoomId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ActionPlane, FOLLOW_DEADLINE, RemoteDevice, Workspaces};

use crate::engine::Engine;

/// This device's action plane, as the session serves it and reaches the
/// person's other devices through it.
#[derive(Clone)]
pub(crate) struct DeviceActions(Arc<Inner>);

struct Inner {
    plane: ActionPlane,
    /// The endpoint the plane asks from, as this device.
    endpoint: Endpoint,
    /// Where this session was told each of the person's devices is
    /// ([`crate::sync::Client::connect`]), by its endpoint.
    addresses: Mutex<BTreeMap<EndpointId, EndpointAddr>>,
    /// What answers a request that arrived here.
    dispatch: DeviceDispatch,
}

impl DeviceActions {
    /// This device's plane on the session's `endpoint`, authorizing against
    /// the rooms `engine` holds, and answering through the dispatcher the
    /// session installs once its broker serves.
    pub(crate) fn start(
        identity: &tilo_network_actor::HostIdentity,
        endpoint: Endpoint,
        engine: Option<Arc<Engine>>,
    ) -> Self {
        let dispatch = DeviceDispatch::default();
        let plane = ActionPlane::over(
            identity.device_signing_key.clone(),
            Arc::new(dispatch.clone()),
            Arc::new(Rooms(engine)),
        );
        Self(Arc::new(Inner {
            plane,
            endpoint,
            addresses: Mutex::default(),
            dispatch,
        }))
    }

    /// The plane the collaboration endpoint serves.
    pub(crate) fn plane(&self) -> ActionPlane {
        self.0.plane.clone()
    }

    /// Answer arriving requests through `host` from now on: the broker's
    /// own door, once it serves.
    pub(crate) fn install(&self, host: Arc<dyn ActionHost + Send + Sync>) {
        self.0.dispatch.install(host);
    }

    /// Where one of the person's devices is, as this session was told.
    pub(crate) fn learn(&self, addr: EndpointAddr) {
        self.0.addresses.lock_recover().insert(addr.id, addr);
    }

    /// The device an app's action is for, with where it is reached: the
    /// address this session was told, else the endpoint's own discovery of
    /// the device's key.
    fn device(&self, key: [u8; 32], workspace: &str) -> Result<Arc<RemoteDevice>, ActionHostError> {
        let id = EndpointId::from_bytes(&key).map_err(|_| ActionHostError::InvalidRequest {
            reason: "that is not a device key".into(),
        })?;
        let addr = self
            .0
            .addresses
            .lock_recover()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id));
        Ok(Arc::new(self.0.plane.remote(
            self.0.endpoint.clone(),
            addr,
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
        // The workspace the two devices are to share: the one the app acts
        // in, which is where the other device decides whether this one may
        // act at all.
        let Some(workspace) = caller.room() else {
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
        // A person's own word is asked of them where they are, by the device
        // in front of them. Reaching another of their devices to ask there is
        // no part of this plane: nothing carries a question to it.
        Err(ActionHostError::Unavailable {
            reason: "this device does not ask the person on another of their devices".into(),
        })
    }
}

/// What the other device's answer comes to: the outcome the app on this
/// device observes as its own invocation's.
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
        // Following ends on a terminal update: anything else is a device
        // answering out of turn.
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

/// The one place a request that arrived on this device is handed to: the
/// broker's door, once it serves. Until then a request is answered as
/// unavailable rather than left waiting, and so after the broker retires.
#[derive(Clone, Default)]
struct DeviceDispatch(Arc<Mutex<Option<Arc<dyn ActionHost + Send + Sync>>>>);

impl DeviceDispatch {
    fn install(&self, host: Arc<dyn ActionHost + Send + Sync>) {
        *self.0.lock_recover() = Some(host);
    }

    fn host(&self) -> Result<Arc<dyn ActionHost + Send + Sync>, ActionHostError> {
        self.0
            .lock_recover()
            .clone()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this device is not serving its actions".into(),
            })
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

/// The rooms this device holds, as the plane reads them: this session's own
/// store, asked when a request names a workspace, so who may act there is
/// what holds at that moment rather than as it was when the session started.
struct Rooms(Option<Arc<Engine>>);

impl Workspaces for Rooms {
    fn read(&self, workspace: &str, read: &mut dyn FnMut(&StatePeer)) -> bool {
        let (Some(engine), Some(room)) = (&self.0, RoomId::parse(workspace)) else {
            return false;
        };
        engine
            .owner()
            .with_current(room, |view| {
                read(view.peer());
                Ok(())
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    //! Two of the person's devices on this machine's loopback: an action an
    //! app invokes for the other device runs there through the session's own
    //! plane and answers here, and a device the room does not hold is
    //! refused.

    tilo_automerge_isolate::test_worker!();

    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use tilo_actions::remote::{TestHost, own_device};
    use tilo_network_actor::HostIdentity;
    use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
    use tilo_user_identity::DeviceCert;

    use super::*;

    const ROOM: &str = testing::ROOM;
    /// How long the device asked has to answer before the test fails.
    const PATIENCE: Duration = Duration::from_secs(20);
    const APP: &str = "so.tilo.notes";

    /// A device key certified by `root` as one of its person's devices.
    fn device(root: &SigningKey, seed: u8, label: &str) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let cert = testing::certificate(root, &key, label);
        (key, cert)
    }

    fn identity(cert: DeviceCert, key: &SigningKey) -> HostIdentity {
        HostIdentity {
            device_cert: cert,
            device_signing_key: key.clone(),
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        }
    }

    /// A loopback endpoint whose identity is the device `key`, as the
    /// session's own endpoint is.
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

    /// The session of the device `key` that answers requests, holding the
    /// room the person's device `asking` is in too, and answering through
    /// `host`.
    async fn answering(
        key: &SigningKey,
        cert: &DeviceCert,
        asking: Option<&(SigningKey, DeviceCert)>,
        host: Arc<TestHost>,
    ) -> (Endpoint, Router) {
        let directory = tempfile::TempDir::new().expect("a state directory");
        let id = identity(cert.clone(), key);
        let engine = Arc::new(
            Engine::open(id.clone(), Some(directory.path()), std::iter::empty())
                .expect("this device's engine opens"),
        );
        // This device makes the room, and the other device of the person
        // joins it: the device certified in the room is the one that acts.
        let governor = TestGovernor::empty_interactive(cert.clone(), key.clone(), ROOM)
            .expect("this device governs its room");
        let governor = testing::enroll_into(governor, engine.owner(), key, cert)
            .expect("this device's room is its own");
        if let Some(asking) = asking {
            let joining = tempfile::TempDir::new().expect("the other device's directory");
            let other = tilo_peer_state::owner::open_file(&joining.path().join("owner.json"))
                .expect("the other device's owner opens");
            let governor = testing::enroll_into(governor, &other, &asking.0, &asking.1)
                .expect("the room admits the other device");
            // What the room's admission of the other device changed reaches
            // this device's own copy of the room, as it would over the
            // person's devices.
            let room = RoomId::parse(ROOM).expect("a room");
            let now = std::time::SystemTime::now();
            for (bytes, _) in governor.outgoing() {
                engine
                    .owner()
                    .receive(
                        room,
                        tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                            cert.device_public_key,
                        ),
                        postcard::from_bytes(bytes).expect("a room change"),
                        now,
                    )
                    .expect("this device takes the room as its governor has it");
            }
        }
        let endpoint = endpoint(key).await;
        let actions = DeviceActions::start(&id, endpoint.clone(), Some(engine));
        actions.install(host);
        let router = Router::builder(endpoint.clone())
            .accept(tilo_peer_state::actions::ACTIONS_ALPN, actions.plane())
            .spawn();
        (endpoint, router)
    }

    /// The session of the device `key` that asks: its own plane, nothing to
    /// answer, and where the other device is.
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

    /// What an app on this device asks: `action` of its own on `device`, in
    /// `ROOM`.
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

    /// The outcome `actions` observes for `request`, once the other device
    /// answers: `None` when it never does.
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
        let (there_endpoint, router) =
            answering(&there.0, &there.1, Some(&here), host.clone()).await;
        let (here_endpoint, actions) = asking(&here.0, &here.1, addr(&there_endpoint)).await;
        let caller =
            Caller::authenticated(root.verifying_key().to_bytes(), here.1.device_public_key)
                .in_room(ROOM);
        let (checked, request) = invocation(there.1.device_public_key, "insert");

        // The other device runs the app's action and this one hears its
        // outcome as its own.
        let outcome = answered(&actions, &caller, &checked, &request)
            .expect("the other device answers within the patience");
        assert!(
            outcome.is_ok(),
            "the other device ran the action: {outcome:?}"
        );

        // It ran there, as this device, in the room both are in.
        let asked = host.asked();
        assert_eq!(asked.len(), 1, "one request reached the other device");
        assert_eq!(asked[0].invocation, request.invocation, "under its own id");
        assert_eq!(asked[0].target.address.action, "insert");
        assert_eq!(
            tilo_actions::remote::application_provider(&asked[0].target.provider),
            Some((ROOM, APP)),
            "the app's action, in the room the two devices share"
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
        assert_eq!(seen[0].room(), Some(ROOM));

        router.shutdown().await.expect("the router stops");
        here_endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_the_room_does_not_hold_is_refused() {
        let root = SigningKey::from_bytes(&[0x41; 32]);
        let there = device(&root, 0x42, "The other device");
        let stranger = device(&root, 0x43, "A device of the same person, not in the room");
        let host = Arc::new(TestHost::new());
        let (there_endpoint, router) = answering(&there.0, &there.1, None, host.clone()).await;
        let (stranger_endpoint, actions) =
            asking(&stranger.0, &stranger.1, addr(&there_endpoint)).await;
        let caller = Caller::authenticated(
            root.verifying_key().to_bytes(),
            stranger.1.device_public_key,
        )
        .in_room(ROOM);
        let (checked, request) = invocation(there.1.device_public_key, "insert");

        let refused = answered(&actions, &caller, &checked, &request)
            .expect("the other device answers within the patience");
        assert!(
            refused.is_err(),
            "a device the room has no part for acts nowhere in it: {refused:?}"
        );
        assert!(
            host.asked().is_empty(),
            "the action never ran on the device it named"
        );

        router.shutdown().await.expect("the router stops");
        stranger_endpoint.close().await;
    }
}
