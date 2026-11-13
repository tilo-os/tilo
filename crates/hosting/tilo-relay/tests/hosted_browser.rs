//! A hosted workspace's Browser: opening it places an environment whose
//! session joins the workspace as a device and holds the window, and an
//! agent's page actions reach that window as the agent's — counted, and
//! refused with the host's typed refusal past the free allowance. A page
//! action the window asks the person about is put to them by the relay
//! itself, and the caller waits for their answer.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(clippy::unwrap_used, reason = "in a test these are the assertions")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::remote::host_account::{NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    ActionHostError, Caller, Confirmation, ConfirmedInvocation, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay::certify::ConfirmationAnswer;
use tilo_relay::hosted_browser::{
    BrowserMeter, BrowserSessions, HostedBrowsers, Material, PAGE_RESOURCE, Window,
    no_payment_method,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::{AppServingPlane, Envelope, RelayClient};
use tilo_relay_client::Approver;
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, HostWitness, RevocationList, prepare_stand_in_cert,
    sign_device_cert,
};

use crate::common::{
    TestDevice, TestRelay, TestUser, admit_members, client_endpoint, spawn_relay_serving_apps,
};
use tilo_relay_protocol::WorkspaceId;

fn ws() -> tilo_relay_protocol::WorkspaceId {
    tilo_relay_protocol::WorkspaceId::from_u128(21)
}
const OTHER_WS: u64 = 22;
const WS_NAME: &str = "11111111-1111-4111-8111-111111111111";
const WINDOW: &str = "browser@42";

/// Places environments in-process: each "environment" is the device the
/// relay certified, which the test's client then joins the workspace as.
#[derive(Default)]
struct InProcessEnvironments {
    starts: Mutex<Vec<Material>>,
    stops: Mutex<Vec<String>>,
}

impl InProcessEnvironments {
    fn starts(&self) -> usize {
        self.starts.lock().unwrap().len()
    }

    fn stops(&self) -> usize {
        self.stops.lock().unwrap().len()
    }

    fn material(&self) -> Material {
        self.starts.lock().unwrap()[0].clone()
    }
}

impl BrowserSessions for InProcessEnvironments {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.starts.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, workspace: &str) {
        self.stops.lock().unwrap().push(workspace.to_owned());
    }
}

/// Counts what reached it, and refuses what it is told to.
#[derive(Default)]
struct Counting {
    recorded: Mutex<usize>,
    refusal: Mutex<Option<Refusal>>,
}

impl BrowserMeter for Counting {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        let refusal = self.refusal.lock().unwrap().clone();
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }

    fn record(&self, _: &str, _: &str) {
        let mut recorded = self.recorded.lock().unwrap();
        *recorded = recorded.saturating_add(1);
    }
}

/// The plane of a relay that routes its workspaces' Browser page actions to
/// the window holding them.
struct Routing(Arc<HostedBrowsers>);

impl AppServingPlane for Routing {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        _caller: &Caller,
        _message: RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<RemoteActionMessage>> {
        None
    }

    fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<tilo_relay::app_serving::Routing> {
        self.0.route(workspace_id, caller, message)
    }

    fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: &RemoteActionMessage,
    ) -> Option<tilo_relay::app_serving::Routing> {
        self.0
            .route_certified(workspace_id, caller, certificate, message)
    }

    fn relayed(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        self.0.relayed(workspace_id, from, message)
    }

    fn tracks(&self, workspace_id: WorkspaceId) -> bool {
        self.0.tracks(workspace_id)
    }
}

fn frame(message: RemoteActionMessage) -> Vec<u8> {
    RemoteActionFrame::new(message).encode().unwrap()
}

fn page(invocation: InvocationId, action: &str) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: WINDOW.into(),
            address: ActionAddress::new(PAGE_RESOURCE, action),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

/// A page action with `confirmation` attached.
fn page_with(
    invocation: InvocationId,
    action: &str,
    confirmation: Option<Confirmation>,
) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: WINDOW.into(),
            address: ActionAddress::new(PAGE_RESOURCE, action),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

/// The submit the agent asks for, as a person's approving device is shown it:
/// the purpose it acts on, by the words the agent gave it.
fn submit(invocation: InvocationId) -> RemoteActionMessage {
    let RemoteActionMessage::Invoke(mut request) = page(invocation, "submit-page") else {
        unreachable!("a page action is an invocation");
    };
    request.context = Some(tilo_actions::ActionContext {
        purpose: Some("Submit the application".into()),
        ..tilo_actions::ActionContext::default()
    });
    RemoteActionMessage::Invoke(request)
}

/// A caller asking how `invocation` went, waiting until it moves.
fn observe(invocation: InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Observe {
        invocation,
        wait_ms: Some(500),
        seen: None,
    }
}

/// The invocation `envelope` answers.
fn receipt_of(envelope: Envelope) -> InvocationReceipt {
    let Envelope::RemoteAction { frame, .. } = envelope else {
        panic!("the caller is answered: {envelope:?}");
    };
    match RemoteActionFrame::decode(&frame).unwrap().message {
        RemoteActionMessage::Receipt(receipt) => receipt,
        other => panic!("an invocation is answered with a receipt: {other:?}"),
    }
}

/// The receipt answering `invocation` of `caller`'s, past whatever answers an
/// earlier one of its own arrived first.
async fn receipt_for(caller: &RelayClient, invocation: InvocationId) -> InvocationReceipt {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let Envelope::RemoteAction { frame, .. } = next(caller, "caller").await else {
            panic!("the caller is answered");
        };
        match RemoteActionFrame::decode(&frame).unwrap().message {
            RemoteActionMessage::Receipt(receipt) if receipt.invocation == invocation => {
                return receipt;
            }
            // The state of an invocation that ended before it, which its
            // caller may already have seen.
            RemoteActionMessage::Update { .. } => {}
            other => panic!("{invocation:?} is answered with a receipt: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "{invocation:?} was never answered"
        );
    }
}

/// The state `envelope` answers a caller asking how an invocation went with.
fn update_of(envelope: Envelope) -> Option<tilo_actions::remote::InvocationState> {
    let Envelope::RemoteAction { frame, .. } = envelope else {
        panic!("the caller is answered: {envelope:?}");
    };
    match RemoteActionFrame::decode(&frame).unwrap().message {
        RemoteActionMessage::Update { state, .. } => state,
        other => panic!("an observation is answered with a state: {other:?}"),
    }
}

/// How `invocation` of `caller`'s went, followed until it is over: what a
/// caller that asked once and waits sees.
async fn followed(
    caller: &RelayClient,
    host: [u8; 32],
    invocation: InvocationId,
) -> tilo_actions::InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        caller
            .send(to_host(ws(), host, frame(observe(invocation))))
            .unwrap();
        let state =
            update_of(next(caller, "caller").await).expect("the invocation is the caller's");
        if state.update.is_terminal() {
            return state.update;
        }
        assert!(Instant::now() < deadline, "the invocation never ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The question the person's device sees waiting for them, once it is asked.
async fn asked(device: &mut Approver) -> tilo_relay::certify::PendingConfirmation {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut waiting = device.list_confirmations().await.unwrap();
        if let Some(question) = waiting.pop() {
            assert!(
                waiting.is_empty(),
                "one question per invocation: {waiting:?}"
            );
            return question;
        }
        assert!(Instant::now() < deadline, "the person was never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn to_host(workspace_id: WorkspaceId, host: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::RemoteAction {
        workspace_id,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: host,
        from_device_public_key: [0xEE; 32],
        from_user_public_key: [0xEE; 32],
        frame,
    }
}

async fn subscribe(relay: &tilo_relay::Relay, user: &TestUser, device: &TestDevice) -> RelayClient {
    subscribe_in(relay, user, device, ws()).await
}

async fn subscribe_in(
    relay: &tilo_relay::Relay,
    user: &TestUser,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> RelayClient {
    let client = RelayClient::connect(
        client_endpoint().await,
        relay.node_addr(),
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    let ready = tilo_test_env::within("ready timeout", client.recv()).await;
    assert!(matches!(ready, Some(Envelope::Ready { .. })), "{ready:?}");
    client
}

async fn next(client: &RelayClient, who: &str) -> Envelope {
    tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
        .await
        .unwrap_or_else(|_| panic!("{who} timed out"))
        .unwrap_or_else(|| panic!("{who} connection closed"))
}

/// One page action a window was handed: who sent it, and whether it carried
/// the witnessed answer of the host that certified the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Handed {
    from: [u8; 32],
    witnessed: bool,
}

/// What a window does with a page action: run it, or — an action that asks
/// the person first — refuse it for want of their answer unless it carries
/// the answer of the host that certified the window, for that invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asks {
    /// Every page action runs at once.
    Never,
    /// Submitting asks the person; reading and filling do not, as a page
    /// without their sign-ins lets an agent read and fill it.
    Submit,
    /// Acting on the page asks the person: filling, selecting, clicking,
    /// scrolling and submitting are outside effects, and only their answer —
    /// or a key their person let operate the page — runs one.
    Acting,
    /// [`Self::Acting`], and the window states the departure its run reached:
    /// the form the click or the submit sends, and the picture of the filled
    /// page, as the window itself holds it.
    Departing,
}

impl Asks {
    fn asks(self, action: &str) -> bool {
        /// What only looks at the page.
        const OBSERVES: [&str; 4] = [
            "read-page",
            "find-page-element",
            "capture-page",
            "wait-page-settled",
        ];
        match self {
            Self::Never => false,
            Self::Submit => action == "submit-page",
            Self::Acting | Self::Departing => !OBSERVES.contains(&action),
        }
    }

    /// What the window states about the departure it holds, when it states
    /// one: the facts a person's question reads, as the window states them on
    /// the hold it publishes.
    fn departure(self) -> Vec<tilo_actions::ActionFacet> {
        match self {
            Self::Departing => [
                ("page", "https://visa.example/apply"),
                ("kind", "submit"),
                ("method", "POST"),
                ("destination", "https://visa.example"),
                ("picture", "tilo.content:5f2b"),
            ]
            .into_iter()
            .map(|(name, value)| tilo_actions::remote::browser::departure_facet(name, value))
            .collect(),
            Self::Never | Self::Submit | Self::Acting => Vec::new(),
        }
    }
}

/// The device holding the workspace's Browser window: it answers every
/// invocation with a receipt and keeps who sent it.
async fn window_session(
    relay: &tilo_relay::Relay,
    user: &TestUser,
    material: &Material,
    asks: Asks,
) -> Arc<Mutex<Vec<Handed>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceIdentity::of(material);
    let client = subscribe(relay, user, &device).await;
    let heard = Arc::clone(&seen);
    let holder = material.device.device_public_key;
    let certifier = material
        .device
        .issuer
        .as_ref()
        .expect("the relay certified the environment's device")
        .device_public_key;
    let person = material.user_public_key;
    tokio::spawn(async move {
        while let Some(envelope) = client.recv().await {
            let Envelope::RemoteAction {
                workspace_id,
                from_device_public_key,
                frame,
                ..
            } = envelope
            else {
                continue;
            };
            let Ok(RemoteActionFrame { message, .. }) = RemoteActionFrame::decode(&frame) else {
                continue;
            };
            let RemoteActionMessage::Invoke(request) = message else {
                continue;
            };
            // Only the host that certified this window, for this very
            // invocation and action, answers for the person.
            let witnessed = match &request.confirmation {
                Some(Confirmation::Witnessed(token)) => {
                    HostWitness::decode(token).is_some_and(|witness| {
                        witness.host_device_public_key == certifier
                            && witness.invocation == *request.invocation.0.as_bytes()
                            && witness.provider == request.target.provider
                            && witness.action == request.target.address.action
                            && witness.verify(tilo_clock::unix_now_ns()).is_ok()
                    })
                }
                _ => false,
            };
            heard.lock().unwrap().push(Handed {
                from: from_device_public_key,
                witnessed,
            });
            let status = if asks.asks(&request.target.address.action) && !witnessed {
                ReceiptStatus::Rejected {
                    error: ActionHostError::ConfirmationRequired {
                        departure: asks.departure(),
                    },
                }
            } else {
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                }
            };
            let answer = RemoteActionFrame::new(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status,
            }))
            .encode()
            .unwrap();
            let _ = client.send(Envelope::RemoteAction {
                workspace_id,
                version: REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: from_device_public_key,
                from_device_public_key: holder,
                from_user_public_key: person,
                frame: answer,
            });
        }
    });
    seen
}

/// The test device a material names: what the session in the environment
/// joins as.
struct DeviceIdentity;

impl DeviceIdentity {
    fn of(material: &Material) -> TestDevice {
        TestDevice {
            sk: material.key().unwrap(),
            cert: material.device.clone(),
        }
    }
}

struct Fixture {
    user: TestUser,
    home: [u8; 32],
    agent: TestDevice,
    /// The person's own device, which answers what their agents ask them.
    laptop: TestDevice,
    relay: TestRelay,
    browsers: Arc<HostedBrowsers>,
    environments: Arc<InProcessEnvironments>,
    meter: Arc<Counting>,
}

async fn fixture(meter: Arc<Counting>) -> Fixture {
    fixture_with(
        meter,
        tilo_relay::hosted_browser::RECONNECT,
        tilo_relay::hosted_browser::ANSWER_WAIT,
    )
    .await
}

/// A fixture that gives a disconnected environment device `reconnect` to come
/// back before its environment stops, and the person `answer_wait` to answer
/// a page action's question.
async fn fixture_with(meter: Arc<Counting>, reconnect: Duration, answer_wait: Duration) -> Fixture {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home", DeviceCapabilities::host());
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let agent = user.enrol("agent", DeviceCapabilities::member());
    let home = host.device_pubkey();
    let host_key = host.sk.clone();
    let host_cert = {
        let now = tilo_clock::unix_now_ns();
        let mut stand_in = prepare_stand_in_cert(
            user.user_pk,
            host_key.verifying_key().to_bytes(),
            "Ada's host",
            now,
            now.saturating_add(24 * 60 * 60 * 1_000_000_000),
        );
        sign_device_cert(&mut stand_in, &user.user_sk).unwrap();
        stand_in
    };
    let environments = Arc::new(InProcessEnvironments::default());
    let browsers = Arc::new(
        HostedBrowsers::new(
            Box::new(EnvironmentsHandle(Arc::clone(&environments))),
            Box::new(SharedMeter(Arc::clone(&meter))),
            "acct_ada".into(),
        )
        .with_reconnect(reconnect)
        .with_answer_wait(answer_wait),
    );
    let relay = Box::pin(spawn_relay_serving_apps(
        &user,
        host,
        &[ws(), OTHER_WS],
        &[agent.device_pubkey()],
        Arc::new(Routing(Arc::clone(&browsers))),
    ))
    .await;
    browsers.serve_from(tilo_relay::hosted_browser::Authority {
        user_public_key: user.user_pk,
        cert: host_cert,
        key: host_key,
        relay: relay.relay.node_addr(),
        workspaces: None,
        environments: relay.relay.environments(),
        runtime: tokio::runtime::Handle::current(),
        certification: relay.relay.confirmations(),
        hand: relay.relay.hand(),
    });
    browsers
        .place(WS_NAME, ws(), "record-WorkspaceId::from_u128(1)")
        .expect("the environment was placed");
    Fixture {
        user,
        home,
        agent,
        laptop,
        relay,
        browsers,
        environments,
        meter,
    }
}

/// The environments a test started, behind the trait the relay holds.
struct EnvironmentsHandle(Arc<InProcessEnvironments>);

impl BrowserSessions for EnvironmentsHandle {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.start(material)
    }

    fn stop(&self, workspace: &str) {
        self.0.stop(workspace);
    }
}

/// The meter a test counts with, behind the trait the relay holds.
struct SharedMeter(Arc<Counting>);

impl BrowserMeter for SharedMeter {
    fn admit(&self, account: &str) -> Result<(), Refusal> {
        self.0.admit(account)
    }

    fn record(&self, workspace: &str, account: &str) {
        self.0.record(workspace, account);
    }
}

impl Fixture {
    /// The person's approving device, connected to the relay: where a page
    /// action that asks first is put to them.
    async fn approving_device(&self) -> (iroh::endpoint::Endpoint, Approver) {
        let endpoint = client_endpoint().await;
        let approver = Approver::connect(
            &endpoint,
            self.relay.relay.node_addr(),
            self.laptop.cert.clone(),
            &self.laptop.sk,
            &RevocationList::new(),
        )
        .await
        .expect("the person's device asks the relay what waits for them");
        (endpoint, approver)
    }

    /// The workspace lists the window's device, as the relay lists it on a
    /// host that governs the workspace.
    async fn list_environment(&self) {
        let device = self.browsers.window(ws()).unwrap().device;
        admit_members(
            &self.relay.relay,
            ws(),
            &[self.agent.device_pubkey(), device],
            &self.relay.state_root,
        )
        .await;
    }

    /// A key the person made for an agent to act in their workspaces, which
    /// the workspace lists. It reaches all of them: the relay admits a key to
    /// the workspaces its certificate reaches and no other, which is where a
    /// key for one workspace is held to it.
    fn key(&self, browser_pages: bool) -> TestDevice {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[0x4b; 32]);
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: self.user.user_pk,
            device_public_key: sk.verifying_key().to_bytes(),
            device_label: "Jane Doe's agent".into(),
            capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
                .with_browser_pages(browser_pages),
            issued_at_ns: now,
            expires_at_ns: Some(now.saturating_add(7 * 24 * 60 * 60 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &self.user.user_sk).expect("the person's root certifies it");
        TestDevice { sk, cert }
    }
}

#[tokio::test]
async fn opening_browser_places_an_environment_whose_session_holds_the_window() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let window = fixture.browsers.window(ws()).unwrap();
    assert_eq!(window.provider, WINDOW);
    assert_eq!(fixture.environments.starts(), 1, "one environment opened");
    let material = fixture.environments.material();
    assert_eq!(material.record, "record-1");
    assert_eq!(material.workspace, WS_NAME);
    assert_eq!(material.workspace_id, ws());
    assert_eq!(
        material.device.capabilities,
        DeviceCapabilities::member(),
        "the device is an ordinary member device of the workspace's person"
    );
    assert_eq!(
        material.device.user_public_key, fixture.user.user_pk,
        "of the person the workspace belongs to"
    );
    assert!(
        material.device.issuer.is_some(),
        "certified by the host's stand-in, not by the root"
    );
    assert_eq!(
        material.device.expires_at_ns.unwrap() - material.device.issued_at_ns,
        u64::try_from(tilo_relay::hosted_browser::DEVICE_LIFETIME.as_nanos()).unwrap(),
        "it lasts as long as the environment may run"
    );
    assert_eq!(
        material.device.device_public_key, window.device,
        "the window is the device the relay certified"
    );
    // A second open attaches to the window already up.
    let again = fixture.browsers.place(WS_NAME, ws(), "record-WorkspaceId::from_u128(1)").unwrap();
    assert_eq!(again, window);
    assert_eq!(fixture.environments.starts(), 1, "no second environment");
}

impl Fixture {
    /// The workspace lists the window's device and `key`, as a host that
    /// governs the workspace lists a key its person made for it.
    async fn list_environment_and(&self, key: &TestDevice) {
        let window = self.browsers.window(ws()).unwrap().device;
        admit_members(
            &self.relay.relay,
            ws(),
            &[key.device_pubkey(), window],
            &self.relay.state_root,
        )
        .await;
    }
}

#[tokio::test]
async fn a_page_action_reaches_the_window_as_the_caller() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(page(id, "read-page"))))
        .unwrap();
    let answer = next(&caller, "caller").await;
    let Envelope::RemoteAction {
        to_device_public_key,
        from_device_public_key,
        frame: answer,
        ..
    } = answer
    else {
        panic!("the window answered: {answer:?}");
    };
    assert_eq!(to_device_public_key, fixture.agent.device_pubkey());
    assert_eq!(
        from_device_public_key, material.device.device_public_key,
        "the answer comes from the window's device"
    );
    let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(&answer).unwrap().message
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .map(|handed| handed.from)
            .collect::<Vec<_>>(),
        [fixture.agent.device_pubkey()],
        "the window ran it as the caller, not as the relay: History records the agent's run"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1, "counted");
}

#[tokio::test]
async fn a_page_action_past_the_allowance_is_refused_and_says_how_to_continue() {
    let meter = Arc::new(Counting::default());
    *meter.refusal.lock().unwrap() = Some(no_payment_method(3));
    let fixture = fixture(Arc::clone(&meter)).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(page(id, "fill-page"))))
        .unwrap();
    let Envelope::RemoteAction { frame: answer, .. } = next(&caller, "caller").await else {
        panic!("the caller is answered");
    };
    let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(&answer).unwrap().message
    else {
        panic!("a refusal is a receipt");
    };
    assert_eq!(receipt.invocation, id);
    let ReceiptStatus::Rejected {
        error: ActionHostError::Refused { reason },
    } = receipt.status
    else {
        panic!("refused: {receipt:?}");
    };
    let refusal = Refusal::from_reason(&reason).expect("the refusal is the host's typed one");
    assert_eq!(refusal.reason, Reason::NoPaymentMethod);
    assert!(
        refusal.step(NextStepKind::AddPaymentMethod).is_some(),
        "one next step: add a payment method"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refused action never reaches the window"
    );
    assert_eq!(*meter.recorded.lock().unwrap(), 0, "nothing ran to count");
}

#[tokio::test]
async fn an_environment_serves_the_workspace_it_was_placed_for_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
    let environment = DeviceIdentity::of(&material);
    let client = RelayClient::connect(
        client_endpoint().await,
        fixture.relay.relay.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the environment's device dials the host");
    client
        .send(Envelope::Subscribe {
            workspace_id: OTHER_WS,
        })
        .unwrap();
    let answer = next(&client, "environment").await;
    assert!(
        !matches!(answer, Envelope::Ready { .. }),
        "a device of one workspace joins no other: {answer:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}

/// The agent reads and fills the page a visitor may read and fill without
/// anyone being asked; the submit the same agent sends is put to the person
/// by the relay itself — no caller-side confirmation, no door of its own.
#[tokio::test]
async fn an_agents_reads_and_fills_run_at_once_and_its_submit_is_put_to_the_person() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Submit).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    for action in ["read-page", "find-page-element", "fill-page", "select-page"] {
        let id = InvocationId::new();
        caller
            .send(to_host(ws(), fixture.home, frame(page(id, action))))
            .unwrap();
        let ran = receipt_of(next(&caller, "caller").await);
        assert!(
            matches!(ran.status, ReceiptStatus::Completed { .. }),
            "{action} ran without asking: {ran:?}"
        );
        assert!(
            approver.list_confirmations().await.unwrap().is_empty(),
            "{action} asked the person, and a page an agent may read and fill asks nothing"
        );
    }
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "every page action that ran is counted"
    );

    // The submit asks the person, and the caller waits for them.
    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted,
        "the submit waits for the person rather than refused"
    );
    let question = asked(&mut approver).await;
    assert_eq!(question.action_label, "Submit the application");
    assert_eq!(question.resource_label, PAGE_RESOURCE);
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "a submit that waits for the person has run nothing"
    );
    assert_eq!(seen.lock().unwrap().len(), 5, "the window was asked once");
    assert!(
        !seen.lock().unwrap()[4].witnessed,
        "the agent carried no confirmation of its own"
    );
}

/// A submission a page action's own run reached: the window answers that it
/// waits for the person and states the departure it holds, so the question
/// names what leaves — the page it leaves from, the method, where it goes and
/// the picture of the filled page — rather than a label for the action.
#[tokio::test]
async fn a_submission_a_page_action_reached_is_asked_about_by_what_leaves() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    window_session(
        &fixture.relay.relay,
        &fixture.user,
        &material,
        Asks::Departing,
    )
    .await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted,
        "the submission waits for the person rather than refused"
    );
    let question = asked(&mut approver).await;
    assert_leaves(&question);
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "a submission nobody allowed has run nothing"
    );
}

/// The same, for a click on the page's own submit button: the click's run
/// reaches the departure the page sends, and the person is asked what leaves.
#[tokio::test]
async fn a_click_on_a_submit_button_is_asked_about_by_what_leaves() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    window_session(
        &fixture.relay.relay,
        &fixture.user,
        &material,
        Asks::Departing,
    )
    .await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(page(id, "click-page"))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted,
        "the click waits for the person rather than refused"
    );
    assert_leaves(&asked(&mut approver).await);
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "a click nobody allowed has sent nothing"
    );
}

/// The question `question` is, as a person reads one about a departure: the
/// page it leaves from, what it does, the method, where it goes, and the
/// picture of the filled page.
fn assert_leaves(question: &tilo_relay::certify::PendingConfirmation) {
    assert_eq!(
        question.action_label, "send the form to https://visa.example (POST)",
        "the person is told what leaves, where, and how"
    );
    assert_eq!(question.resource_label, PAGE_RESOURCE);
    let page = question
        .page
        .as_ref()
        .expect("the question names the page it leaves from");
    assert_eq!(page.page, "https://visa.example/apply");
    assert_eq!(page.kind, "submit");
    assert_eq!(page.method, "POST");
    assert_eq!(page.destination, "https://visa.example");
    assert_eq!(
        page.picture.as_deref(),
        Some("tilo.content:5f2b"),
        "and the picture of the filled page, by its content address"
    );
}

/// A key whose person let it operate web pages fills, selects and clicks in
/// the hosted Browser without anyone being asked; the submit the same key
/// sends still asks them, because submitting is not operating the page.
#[tokio::test]
async fn a_keys_page_actions_run_at_once_where_its_person_let_it_operate_them() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(true);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Acting).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &key).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    for action in ["navigate", "fill-page", "select-page", "click-page"] {
        let id = InvocationId::new();
        caller
            .send(to_host(ws(), fixture.home, frame(page(id, action))))
            .unwrap();
        let ran = receipt_for(&caller, id).await;
        assert!(
            matches!(ran.status, ReceiptStatus::Completed { .. }),
            "{action} ran without asking: the page action was still a question ({ran:?})"
        );
        assert!(
            approver.list_confirmations().await.unwrap().is_empty(),
            "{action} asked the person, whose key may operate the page"
        );
    }
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "every page action that ran is counted, and once"
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(
            seen.iter().all(|handed| handed.from == key.device_pubkey()),
            "the window ran them as the key's own agent"
        );
        assert!(
            seen.iter().all(|handed| handed.witnessed),
            "the host answered for the person, as their answer does"
        );
    }

    // Submitting is not operating a page: it asks, whoever holds the key.
    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted,
        "the submit waits for the person"
    );
    let question = asked(&mut approver).await;
    assert_eq!(question.action_label, "Submit the application");
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "a submit that waits for the person has run nothing"
    );
    approver
        .answer(question.id, ConfirmationAnswer::AllowOnce)
        .await
        .expect("the person answers");
    assert!(
        matches!(
            followed(&caller, fixture.home, id).await,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "their answer runs the submit they were asked about"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        5,
        "the submit they allowed is counted once"
    );
}

/// A key whose person did not let it operate web pages is asked about every
/// action on the page, one at a time, and its submit too.
#[tokio::test]
async fn a_keys_page_actions_ask_first_where_it_may_not_operate_them() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(false);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Acting).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &key).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    for action in ["fill-page", "select-page", "click-page"] {
        let id = InvocationId::new();
        caller
            .send(to_host(ws(), fixture.home, frame(page(id, action))))
            .unwrap();
        assert_eq!(
            receipt_for(&caller, id).await.status,
            ReceiptStatus::Accepted,
            "{action} waits for the person"
        );
        let question = asked(&mut approver).await;
        assert_eq!(
            question.action_label, action,
            "{action} is what they are asked about"
        );
        approver
            .answer(question.id, ConfirmationAnswer::AllowOnce)
            .await
            .expect("the person answers");
        assert!(
            matches!(
                followed(&caller, fixture.home, id).await,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ),
            "{action} ran once they allowed it"
        );
    }
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        3,
        "each allowed action is counted once"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|handed| handed.from == key.device_pubkey()),
        "the window ran them as the key's agent"
    );
    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_for(&caller, id).await.status,
        ReceiptStatus::Accepted
    );
    assert_eq!(
        asked(&mut approver).await.action_label,
        "Submit the application"
    );
}

/// The person allowing a submit runs it once, as the agent asked it, and the
/// page's receipt comes back to that same invocation; nothing but their
/// answer ever runs it.
#[tokio::test]
async fn a_page_action_the_person_allows_runs_once_and_the_page_answers_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Submit).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted
    );
    let question = asked(&mut approver).await;
    approver
        .answer(question.id, ConfirmationAnswer::AllowOnce)
        .await
        .expect("the person answers");

    let ended = followed(&caller, fixture.home, id).await;
    assert!(
        matches!(ended, tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the page's own answer comes back to the invocation that asked: {ended:?}"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        1,
        "it ran once, and is counted once"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "the window was handed it twice");
    assert_eq!(seen[0].from, fixture.agent.device_pubkey());
    assert!(!seen[0].witnessed, "the first had no answer to carry");
    assert_eq!(
        seen[1].from,
        fixture.agent.device_pubkey(),
        "the run with the person's answer is the agent's, not the relay's"
    );
    assert!(seen[1].witnessed, "the second carried their answer");
}

/// The person declining a page action ends it refused: it never runs, and
/// nothing is counted.
#[tokio::test]
async fn the_persons_denial_ends_a_page_action_refused_and_counts_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Submit).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted
    );
    let question = asked(&mut approver).await;
    approver
        .answer(question.id, ConfirmationAnswer::Deny)
        .await
        .expect("the person answers");
    let ended = followed(&caller, fixture.home, id).await;
    assert_eq!(
        ended,
        tilo_actions::InvocationUpdate::Failed {
            invocation: id,
            error: tilo_actions::ActionFailure {
                code: ActionHostError::NotPermitted.code().to_owned(),
                message: ActionHostError::NotPermitted.to_string(),
                details: None,
                retryable: false,
            },
        }
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "the window saw it once");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// A question nobody answers ends the invocation refused, as an action that
/// asks does, and nothing is counted.
#[tokio::test]
async fn a_question_nobody_answers_ends_a_page_action_refused() {
    let fixture = fixture_with(
        Arc::new(Counting::default()),
        tilo_relay::hosted_browser::RECONNECT,
        Duration::from_millis(100),
    )
    .await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Submit).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_host(ws(), fixture.home, frame(submit(id))))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Accepted
    );
    let ended = followed(&caller, fixture.home, id).await;
    let tilo_actions::InvocationUpdate::Failed { error, .. } = ended else {
        panic!("a question nobody answers ends it failed: {ended:?}");
    };
    assert_eq!(
        error.code,
        ActionHostError::confirmation_required().code(),
        "the refusal says the person's answer is needed"
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "the window saw it once");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// An answer of any other kind is no answer: a word that is not this host's,
/// for this invocation, runs nothing.
#[tokio::test]
async fn only_this_hosts_answer_for_this_invocation_runs_a_page_action() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Submit).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    let elsewhere = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let mut witness = HostWitness::unsigned(
        tilo_user_identity::Parties {
            person: fixture.user.user_pk,
            host: elsewhere.verifying_key().to_bytes(),
            caller: fixture.agent.device_pubkey(),
            target: material.device.device_public_key,
        },
        tilo_user_identity::Answered {
            invocation: *id.0.as_bytes(),
            provider: WINDOW.to_owned(),
            resource: PAGE_RESOURCE.to_owned(),
            action: "submit-page".to_owned(),
            argument: tilo_actions::remote::argument_digest(None),
        },
        (
            tilo_clock::unix_now_ns(),
            tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
        ),
    );
    witness.sign(&elsewhere).expect("the other host signs it");
    caller
        .send(to_host(
            ws(),
            fixture.home,
            frame(page_with(
                id,
                "submit-page",
                Some(Confirmation::Witnessed(witness.encode())),
            )),
        ))
        .unwrap();
    assert_eq!(
        receipt_of(next(&caller, "caller").await).status,
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        },
        "another host's word is no answer here"
    );
    assert!(seen.lock().unwrap().is_empty(), "it reached no window");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// The record a customer wrote before Browser opened: another app's.
const NOTES: &str = "so.tilo.notes";
const NOTES_RECORD: &str = "notes-1";

/// The channel the window's own record lives on, and the one a customer's
/// document lives on: what every plane of the workspace tells a record by.
fn window_channel() -> [u8; 32] {
    tilo_relay::environment::record_channel(tilo_relay::hosted_browser::APP, "record-1")
}

fn notes_channel() -> [u8; 32] {
    tilo_relay::environment::record_channel(NOTES, NOTES_RECORD)
}

/// One change of `channel`'s record, as a device writes it.
fn change(channel: [u8; 32], marker: &[u8]) -> Vec<u8> {
    tilo_relay::coedit::CoeditPayload::RetainedDelta {
        channel,
        body: marker.to_vec(),
    }
    .encode()
    .unwrap()
}

/// Every envelope a session hears in `within`, the ones it already holds first.
async fn heard(client: &RelayClient, within: std::time::Duration) -> Vec<Envelope> {
    let mut out = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(within, client.recv()).await {
        out.push(envelope);
    }
    out
}

/// The record channel each change a session heard belongs to.
fn changed(envelopes: &[Envelope]) -> Vec<[u8; 32]> {
    envelopes
        .iter()
        .filter_map(|envelope| {
            let Envelope::Delta { delta, .. } = envelope else {
                return None;
            };
            match tilo_relay::coedit::CoeditPayload::decode(delta)? {
                tilo_relay::coedit::CoeditPayload::Checkpoint { channel, .. }
                | tilo_relay::coedit::CoeditPayload::RetainedDelta { channel, .. } => Some(channel),
            }
        })
        .collect()
}

impl Fixture {
    /// The environment's own session: the device the relay certified, joining
    /// the one workspace it was placed in.
    async fn environment_session(&self) -> RelayClient {
        let material = self.environments.material();
        let environment = DeviceIdentity::of(&material);
        subscribe(&self.relay.relay, &self.user, &environment).await
    }
}

#[tokio::test]
async fn the_relay_confines_an_environments_device_to_the_record_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    assert_eq!(
        tilo_relay::environment::environment_of(&fixture.relay.relay.environments(), &device),
        Some(tilo_relay::environment::Environment::Placed(
            tilo_relay::environment::EnvironmentScope::for_record(
                ws(),
                tilo_relay::hosted_browser::APP,
                "record-1"
            )
        )),
        "the placement names the one record its device reaches"
    );
}

#[tokio::test]
async fn a_relay_that_restarts_still_confines_the_environment_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let device = fixture.environments.material().device.device_public_key;
    // What a relay that starts again reads: its own register, not the
    // workspace's listing, which still has the device in it.
    let restarted = tilo_relay::environment::EnvironmentRegistry::at(
        fixture.relay.state_root.join("environments.json"),
    );
    assert_eq!(
        tilo_relay::environment::environment_of(&restarted, &device),
        Some(tilo_relay::environment::Environment::Placed(
            tilo_relay::environment::EnvironmentScope::for_record(
                ws(),
                tilo_relay::hosted_browser::APP,
                "record-1"
            )
        )),
        "a placed environment is written down before its device is listed, so a \
         restart confines it rather than leaving it a member"
    );
}

#[tokio::test]
async fn an_environment_hears_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let writer = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: ws(),
                author_device_public_key: fixture.agent.device_pubkey(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: change(channel, marker),
                signature: [0; 64],
            })
            .unwrap();
    }
    let heard = heard(&environment, std::time::Duration::from_millis(500)).await;
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "a page that escapes Chromium reads no other record of the workspace: {heard:?}"
    );
}

#[tokio::test]
async fn an_environment_writes_its_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let device = fixture.environments.material().device.device_public_key;
    environment
        .send(Envelope::Delta {
            workspace_id: ws(),
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(notes_channel(), b"overwritten by a page"),
            signature: [0; 64],
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    let Envelope::NotAuthorised {
        workspace_id,
        reason,
    } = refusal
    else {
        panic!("a write to another record is refused: {refusal:?}");
    };
    assert_eq!(workspace_id, ws());
    assert!(
        reason.contains("environment"),
        "the refusal names the rule: {reason}"
    );

    environment
        .send(Envelope::Delta {
            workspace_id: ws(),
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(window_channel(), b"the page clicked"),
            signature: [0; 64],
        })
        .unwrap();
    let tolerance = heard(&environment, std::time::Duration::from_millis(300)).await;
    assert!(
        !tolerance
            .iter()
            .any(|envelope| matches!(envelope, Envelope::NotAuthorised { .. })),
        "its own record is its to write: {tolerance:?}"
    );
    let retained: Vec<Envelope> =
        tilo_relay::store::WorkspaceStore::open(&fixture.relay.state_root, ws(), true)
            .unwrap()
            .load_retained()
            .unwrap()
            .into_iter()
            .filter_map(|frame| {
                tilo_relay::frame::decode(frame.get(4..)?)
                    .ok()
                    .filter(|envelope| matches!(envelope, Envelope::Delta { .. }))
            })
            .collect();
    assert_eq!(
        changed(&retained),
        [window_channel()],
        "the window's own write is the only record this workspace keeps from it"
    );
}

#[tokio::test]
async fn an_environment_opens_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    environment
        .send(Envelope::OpenRecord {
            workspace_id: ws(),
            channel: notes_channel(),
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    assert!(
        matches!(&refusal, Envelope::NotAuthorised { reason, .. } if reason.contains("environment")),
        "another record does not open for it: {refusal:?}"
    );
    environment
        .send(Envelope::OpenRecord {
            workspace_id: ws(),
            channel: window_channel(),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::RecordOpened { channel, .. } if channel == window_channel()),
        "its own record opens: {answer:?}"
    );
}

#[tokio::test]
async fn an_environments_replay_is_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let writer = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: ws(),
                author_device_public_key: fixture.agent.device_pubkey(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: change(channel, marker),
                signature: [0; 64],
            })
            .unwrap();
    }
    // It joins after both records were written: what reaches it — queued
    // before its subscription, then replayed behind it — is its own record.
    let material = fixture.environments.material();
    let environment = DeviceIdentity::of(&material);
    let client = RelayClient::connect(
        client_endpoint().await,
        fixture.relay.relay.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client
        .send(Envelope::Subscribe { workspace_id: ws() })
        .unwrap();
    let mut heard = Vec::new();
    let mut ready = false;
    while let Ok(Some(envelope)) =
        tokio::time::timeout(std::time::Duration::from_millis(700), client.recv()).await
    {
        ready |= matches!(envelope, Envelope::Ready { .. });
        heard.push(envelope);
    }
    assert!(ready, "the environment's subscription is answered");
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "the replay of another record stays out: {heard:?}"
    );
}

#[tokio::test]
async fn an_environment_answers_a_page_action_and_never_calls_one() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
    let environment = fixture.environment_session().await;
    // The window's device calls the workspace's door itself: not its to do.
    environment
        .send(Envelope::RemoteAction {
            workspace_id: ws(),
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: fixture.home,
            from_device_public_key: material.device.device_public_key,
            from_user_public_key: material.user_public_key,
            frame: frame(RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: "so.tilo.notes".into(),
                    address: ActionAddress::new("notes", "delete"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            })),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::RemoteActionUndeliverable { .. }),
        "a window's device calls no door of the workspace: {answer:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stopped_environment_reaches_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    // The session that holds the window is here, and stays connected over the
    // stop.
    let environment = fixture.environment_session().await;
    fixture.browsers.stopped(ws());
    assert_eq!(
        tilo_relay::environment::environment_of(&fixture.relay.relay.environments(), &device),
        Some(tilo_relay::environment::Environment::Stopped {
            until_ns: material.device.expires_at_ns.unwrap()
        }),
        "a stopped environment's device is refused until its certificate expires"
    );
    assert_eq!(
        fixture.environments.stops(),
        1,
        "the session in the VM was ended"
    );
    // Its connection here is cut: the planes it carried refuse it.
    assert!(
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            while environment.recv().await.is_some() {}
        })
        .await
        .is_ok(),
        "a stopped environment's device has no connection left"
    );
    // The workspace still lists it — this fixture's host delists through
    // `Workspaces`, which it has none of, so the delisting fails and the
    // refusal is all that is left. It is refused, not open.
    admit_members(
        &fixture.relay.relay,
        ws(),
        &[fixture.agent.device_pubkey(), device],
        &fixture.relay.state_root,
    )
    .await;
    let environment = DeviceIdentity::of(&material);
    let refused = RelayClient::connect(
        client_endpoint().await,
        fixture.relay.relay.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the environment's device dials the host");
    refused
        .send(Envelope::Subscribe { workspace_id: ws() })
        .unwrap();
    // Refused at the handshake or at its subscribe, or closed: never ready.
    let answer = tokio::time::timeout(tilo_test_env::hang_bound(), refused.recv()).await;
    assert!(
        !matches!(answer, Ok(Some(Envelope::Ready { .. }))),
        "a stopped environment's device is no device of the workspace: {answer:?}"
    );
}

#[tokio::test]
async fn an_environment_stops_when_the_session_that_held_it_ends() {
    let fixture = fixture_with(
        Arc::new(Counting::default()),
        Duration::from_millis(20),
        tilo_relay::hosted_browser::ANSWER_WAIT,
    )
    .await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    let environment = fixture.environment_session().await;
    drop(environment);
    // The relay hears that the device's session ended and stops its
    // environment: one signal, and no plane admits it after.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture
            .relay
            .relay
            .environments()
            .of(&device, tilo_clock::unix_now_ns())
            == Some(tilo_relay::environment::Environment::Stopped {
                until_ns: material.device.expires_at_ns.unwrap(),
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the environment whose session ended never stopped"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        fixture.environments.stops(),
        1,
        "the session in the VM was ended"
    );
}

/// The device holding the workspace's window, connected: what the session in
/// the VM is, reporting the departures its page holds to the relay.
async fn window_device(fixture: &Fixture, material: &Material) -> RelayClient {
    let device = DeviceIdentity::of(material);
    subscribe(&fixture.relay.relay, &fixture.user, &device).await
}

/// A departure the window holds, reported as the session in the VM reports
/// one: the invocation that would answer it, named by the id the window
/// published it with, and the departure itself in its context — the page it
/// leaves from, what it sends and where — as the window states it.
fn held(correlation: u64, id: u32) -> RemoteActionMessage {
    RemoteActionMessage::Confirm {
        correlation,
        request: Box::new(InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: WINDOW.into(),
                address: ActionAddress::new(PAGE_RESOURCE, "send-held-request"),
            },
            argument: InvokeArgument::Json(serde_json::Map::from_iter([(
                "id".to_owned(),
                id.to_string().into(),
            )])),
            context: Some(tilo_actions::ActionContext {
                purpose: Some("POST https://orders.example".into()),
                facets: [
                    ("page", "https://orders.example"),
                    ("kind", "submit"),
                    ("method", "POST"),
                    ("destination", "https://orders.example"),
                    ("picture", "tilo.content:5f2b"),
                ]
                .into_iter()
                .map(|(name, value)| tilo_actions::remote::browser::departure_facet(name, value))
                .collect(),
                ..tilo_actions::ActionContext::default()
            }),
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }),
    }
}

/// What the relay answered the report `correlation` names, once it did.
async fn hold_answer(
    window: &RelayClient,
    correlation: u64,
) -> Result<Box<ConfirmedInvocation>, ActionHostError> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let Envelope::RemoteAction { frame, .. } = next(window, "window").await else {
            panic!("the window is answered");
        };
        match RemoteActionFrame::decode(&frame).unwrap().message {
            RemoteActionMessage::ConfirmResponse {
                correlation: answered,
                result,
            } if answered == correlation => return result,
            _ => {}
        }
        assert!(Instant::now() < deadline, "the hold was never answered");
    }
}

/// A departure the page started on its own waits on nobody's invocation, so
/// the session holding the window reports it: the relay puts it to the person
/// once, and their allow is the witness that releases that one request.
#[tokio::test]
async fn a_departure_nothing_asked_for_is_put_to_the_person_and_their_allow_names_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let window = window_device(&fixture, &material).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    window
        .send(to_host(ws(), fixture.home, frame(held(1, 2))))
        .unwrap();
    let question = asked(&mut approver).await;
    assert_eq!(
        question.action_label, "send the form to https://orders.example (POST)",
        "the person is told what leaves, where, and how"
    );
    assert_eq!(question.resource_label, PAGE_RESOURCE);
    let page = question
        .page
        .as_ref()
        .expect("the question names the page it leaves from");
    assert_eq!(page.page, "https://orders.example");
    assert_eq!(page.kind, "submit");
    assert_eq!(page.method, "POST");
    assert_eq!(page.destination, "https://orders.example");
    assert_eq!(
        page.picture.as_deref(),
        Some("tilo.content:5f2b"),
        "and the picture of the filled page, by its content address"
    );

    // The same departure reported again is not put to them twice.
    window
        .send(to_host(ws(), fixture.home, frame(held(2, 2))))
        .unwrap();
    assert!(
        hold_answer(&window, 2).await.is_err(),
        "the second report is told it is already asked"
    );
    assert_eq!(
        approver.list_confirmations().await.unwrap().len(),
        1,
        "one departure is one question"
    );

    approver
        .answer(question.id, ConfirmationAnswer::AllowOnce)
        .await
        .expect("the person answers");
    let confirmed = hold_answer(&window, 1)
        .await
        .expect("their answer is the confirmation the request runs with");
    let Some(Confirmation::Witnessed(token)) = Some(confirmed.confirmation) else {
        panic!("the person's answer travels as the host's witness");
    };
    let witness = HostWitness::decode(&token).expect("the witness is the relay's");
    assert_eq!(
        witness.host_device_public_key, fixture.home,
        "signed by the host that certified the window"
    );
    assert_eq!(
        witness.target_device_public_key, material.device.device_public_key,
        "for the window the request runs on"
    );
    assert_eq!(witness.action, "send-held-request");
    assert!(
        witness.verify(tilo_clock::unix_now_ns()).is_ok(),
        "the witness is good now"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "a departure the person releases is no page action of their allowance"
    );
}

/// A departure the person denies is answered as refused, and one they never
/// answer ends the same way: the page stays as it was filled.
#[tokio::test]
async fn a_departure_the_person_does_not_allow_is_answered_kept() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let window = window_device(&fixture, &material).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    window
        .send(to_host(ws(), fixture.home, frame(held(1, 3))))
        .unwrap();
    let question = asked(&mut approver).await;
    approver
        .answer(question.id, ConfirmationAnswer::Deny)
        .await
        .expect("the person answers");
    assert_eq!(
        hold_answer(&window, 1).await,
        Err(ActionHostError::NotPermitted),
        "kept: the session drops the held request and nothing leaves"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0);
}

/// Only the environment's own device reports the departures of its own
/// window: another device of the workspace is answered nothing, and nobody is
/// asked.
#[tokio::test]
async fn only_the_windows_own_device_reports_what_it_holds() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    caller
        .send(to_host(ws(), fixture.home, frame(held(1, 4))))
        .unwrap();
    let answer = next(&caller, "caller").await;
    if let Envelope::RemoteAction { frame, .. } = answer
        && let Ok(frame) = RemoteActionFrame::decode(&frame)
    {
        assert!(
            !matches!(frame.message, RemoteActionMessage::ConfirmResponse { .. }),
            "another device does not answer for the window's page"
        );
    }
    assert!(
        approver.list_confirmations().await.unwrap().is_empty(),
        "nobody is asked about a hold another device reports"
    );
}

/// What an environment's device asks of the host about itself
/// ([`tilo_actions::remote::environment_device`]): `action` with `argument`.
fn asks_about_itself(
    invocation: InvocationId,
    action: &str,
    argument: InvokeArgument,
) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: tilo_actions::remote::environment_device::target(action),
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

/// The window `tab` of `app`, as [`asks_about_itself`] names it.
fn window_named(app: &str, tab: u64) -> InvokeArgument {
    let serde_json::Value::Object(map) =
        serde_json::to_value(tilo_actions::remote::environment_device::WindowShare {
            app: app.to_owned(),
            tab,
        })
        .unwrap()
    else {
        unreachable!("a window is an object");
    };
    InvokeArgument::Json(map)
}

#[tokio::test]
async fn an_environment_asks_the_host_for_its_own_media_keys_and_nobody_else_may() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let id = InvocationId::new();
    environment
        .send(to_host(
            ws(),
            fixture.home,
            frame(asks_about_itself(
                id,
                tilo_actions::remote::environment_device::MEDIA_KEYS,
                InvokeArgument::None,
            )),
        ))
        .unwrap();
    let receipt = receipt_for(&environment, id).await;
    // This host governs no workspace's keys here, so it says so: the ask
    // reached it, where every other action an environment starts is refused.
    assert!(
        matches!(
            &receipt.status,
            ReceiptStatus::Rejected {
                error: ActionHostError::Unavailable { reason }
            } if reason.contains("keys")
        ),
        "the environment's own ask is the host's to answer: {receipt:?}"
    );

    let agent = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let id = InvocationId::new();
    agent
        .send(to_host(
            ws(),
            fixture.home,
            frame(asks_about_itself(
                id,
                tilo_actions::remote::environment_device::MEDIA_KEYS,
                InvokeArgument::None,
            )),
        ))
        .unwrap();
    let receipt = receipt_for(&agent, id).await;
    assert!(
        matches!(
            receipt.status,
            ReceiptStatus::Rejected {
                error: ActionHostError::NotPermitted
            }
        ),
        "a member that is no environment of the workspace is lent nothing: {receipt:?}"
    );
}

#[tokio::test]
async fn an_environment_shares_no_window_it_did_not_mint() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    // A window the agent's device minted: its id carries that device's salt.
    let foreign = tilo_desktop_model::ids::IdMinter::new(
        tilo_desktop_model::ids::device_salt_of_key(&fixture.agent.device_pubkey()),
    )
    .mint();
    for action in [
        tilo_actions::remote::environment_device::SHARE_WINDOW,
        tilo_actions::remote::environment_device::END_WINDOW_SHARE,
    ] {
        let id = InvocationId::new();
        environment
            .send(to_host(
                ws(),
                fixture.home,
                frame(asks_about_itself(
                    id,
                    action,
                    window_named("so.tilo.notes", foreign),
                )),
            ))
            .unwrap();
        let receipt = receipt_for(&environment, id).await;
        assert!(
            matches!(
                receipt.status,
                ReceiptStatus::Rejected {
                    error: ActionHostError::NotPermitted
                }
            ),
            "{action} of another device's window is refused: {receipt:?}"
        );
    }
}

#[tokio::test]
async fn an_environment_writes_nothing_of_the_workspaces_own_document_and_stays() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let device = fixture.environments.material().device.device_public_key;
    environment
        .send(Envelope::Delta {
            workspace_id: ws(),
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: b"a desk layout and a grant of its own making".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    // Neither refused nor kept: the session stays, and writes its own record.
    environment
        .send(Envelope::Delta {
            workspace_id: ws(),
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(window_channel(), b"the page clicked"),
            signature: [0; 64],
        })
        .unwrap();
    let after = heard(&environment, std::time::Duration::from_millis(300)).await;
    assert!(
        !after
            .iter()
            .any(|envelope| matches!(envelope, Envelope::NotAuthorised { .. })),
        "a session that writes its desk as any session does is not cut for it: {after:?}"
    );
    let retained: Vec<Vec<u8>> =
        tilo_relay::store::WorkspaceStore::open(&fixture.relay.state_root, ws(), true)
            .unwrap()
            .load_retained()
            .unwrap()
            .into_iter()
            .filter_map(
                |frame| match tilo_relay::frame::decode(frame.get(4..)?).ok()? {
                    Envelope::Delta { delta, .. } => Some(delta),
                    _ => None,
                },
            )
            .collect();
    assert!(
        !retained
            .iter()
            .any(|delta| delta.starts_with(b"a desk layout")),
        "the workspace keeps nothing of its document from the environment's device"
    );
    assert_eq!(
        retained.len(),
        1,
        "its own record's write is kept: {retained:?}"
    );
}
