//! A hosted workspace's Browser: opening it places an environment whose
//! session joins the workspace as a device and holds the window, and an
//! agent's page actions reach that window as the agent's — counted, and
//! refused with the host's typed refusal past the free allowance. A page
//! action the window holds for the person is refused stating what leaves and
//! where, and runs when the same call comes back carrying their approve.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::remote::server_account::{NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, Confirmation,
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server::hosted_browser::{
    BrowserMeter, BrowserSessions, HostedBrowsers, Material, Window, no_payment_method,
};
use tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_server::{AppActionServer, Envelope, ServerClient};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, HostApproval, RevocationList,
    prepare_stand_in_cert, sign_device_cert,
};

use crate::common::{
    TestDevice, TestServer, TestUser, client_endpoint, device_endpoint, spawn_server_serving_apps,
};
use tilo_server::WorkspaceId;
use tilo_workspace_service::agents::windows::PAGE_RESOURCE;

const WS: WorkspaceId = WorkspaceId::from_u128(0x1111_1111_1111_4111_8111_1111_1111_1111);
const OTHER_WS: WorkspaceId = WorkspaceId::from_u128(0x2222_2222_2222_4222_8222_2222_2222_2222);
const WS_NAME: &str = "11111111-1111-4111-8111-111111111111";
const WINDOW: &str = "browser@42";
/// What the host publishes for the workspace itself.
const HOST_PROVIDER: &str = "workspace:11111111-1111-4111-8111-111111111111";
/// The revision the window's own listing answers at.
const WINDOW_REVISION: &str = "window";

/// Places environments in-process: each "environment" is the device the
/// server certified, which the test's client then joins the workspace as.
#[derive(Default)]
struct InProcessEnvironments {
    starts: Mutex<Vec<Material>>,
    stops: Mutex<Vec<String>>,
    /// While set, a start waits until it is taken: a session whose VM is
    /// still waking.
    waking: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
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
        let waking = self.waking.lock().unwrap().take();
        if let Some(woken) = waking {
            let _ = woken.recv();
        }
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

/// The app action server of a server that routes its workspaces' Browser page actions to
/// the window holding them.
struct Routing(Arc<HostedBrowsers>);

impl AppActionServer for Routing {
    /// A listing naming no provider is the host's own: it publishes the
    /// workspace itself, and lists the Browser window its VM holds beside it.
    /// The host names no workspace on its own rows, as the workspace host does.
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        _caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<RemoteActionMessage>> {
        let RemoteActionMessage::CatalogueRequest { correlation, query } = message else {
            return None;
        };
        let own = CataloguePage {
            revision: "host".into(),
            providers: vec![CatalogueProvider {
                provider: HOST_PROVIDER.into(),
                app_id: String::new(),
                workspace: String::new(),
                document_id: String::new(),
                browsable: false,
            }],
            ..CataloguePage::default()
        };
        Some(tilo_actions::remote::Pending::ready(self.0.listed(
            workspace_id,
            &query,
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(own),
            },
        )))
    }

    fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<tilo_server::app_serving::Routing> {
        self.0.route(workspace_id, caller, message)
    }

    fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &DeviceCert,
        message: &RemoteActionMessage,
    ) -> Option<tilo_server::app_serving::Routing> {
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

    fn answers_for_host(&self, workspace_id: WorkspaceId, from: [u8; 32], to: [u8; 32]) -> bool {
        self.0.answers_for_host(workspace_id, from, to)
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
        signed: None,
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
        correlation: 0,
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
async fn receipt_for(caller: &ServerClient, invocation: InvocationId) -> InvocationReceipt {
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
    caller: &ServerClient,
    host: [u8; 32],
    invocation: InvocationId,
) -> tilo_actions::InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        caller
            .send(to_server(WS, host, frame(observe(invocation))))
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

fn to_server(workspace_id: WorkspaceId, host: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::RemoteAction {
        workspace_id,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: host,
        from_device_public_key: [0xEE; 32],
        from_user_public_key: [0xEE; 32],
        frame,
    }
}

async fn subscribe(
    server: &tilo_server::Server,
    user: &TestUser,
    device: &TestDevice,
) -> ServerClient {
    subscribe_in(server, user, device, WS).await
}

async fn subscribe_in(
    server: &tilo_server::Server,
    user: &TestUser,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> ServerClient {
    let client = ServerClient::connect(
        client_endpoint().await,
        server.node_addr(),
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    // A workspace held here hands a subscriber its state before it is ready.
    loop {
        match tilo_test_env::within("ready timeout", client.recv()).await {
            Some(Envelope::Ready { .. }) => return client,
            // The person's workspace list comes whenever their workspaces change.
            Some(
                Envelope::WorkspaceBootstrap { .. }
                | Envelope::Delta { .. }
                | Envelope::PersonalList { .. },
            ) => {}
            other => panic!("{other:?}"),
        }
    }
}

/// The next envelope `client` is sent past what the workspace sends every
/// member as its own state changes: a lease its server takes is one.
async fn next(client: &ServerClient, who: &str) -> Envelope {
    loop {
        match tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
            .await
            .unwrap_or_else(|_| panic!("{who} timed out"))
            .unwrap_or_else(|| panic!("{who} connection closed"))
        {
            Envelope::PersonalList { .. }
            | Envelope::WorkspaceBootstrap { .. }
            | Envelope::Delta { .. } => {}
            envelope => return envelope,
        }
    }
}

/// One page action a window was handed: who sent it, and whether it carried
/// the approved answer of the host that certified the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Handed {
    from: [u8; 32],
    approved: bool,
}

/// What a window does with a page action: run it, or — an action that asks
/// the person first — refuse it for want of their answer unless it carries
/// the answer of the host that certified the window, for that invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asks {
    /// Every page action runs at once.
    Never,
    /// Acting on the page asks the person: filling, selecting, clicking,
    /// scrolling and submitting are outside effects, and only their answer —
    /// or a key their person let operate the page — runs one. The window
    /// states the departure its run reached: the form the click or the submit
    /// sends, and the picture of the filled page, as it holds it.
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
            Self::Departing => !OBSERVES.contains(&action),
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
            Self::Never => Vec::new(),
        }
    }
}

/// The device holding the workspace's Browser window: it answers every
/// invocation with a receipt and keeps who sent it.
async fn window_session(
    server: &tilo_server::Server,
    user: &TestUser,
    material: &Material,
    asks: Asks,
) -> Arc<Mutex<Vec<Handed>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceIdentity::of(material);
    let client = subscribe(server, user, &device).await;
    let heard = Arc::clone(&seen);
    let holder = material.device.device_public_key;
    let certifier = material
        .device
        .issuer
        .as_ref()
        .expect("the server certified the environment's device")
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
            let request = match message {
                RemoteActionMessage::Invoke(request) => request,
                // What the window offers, read from the window.
                RemoteActionMessage::CatalogueRequest { correlation, .. } => {
                    let answer = RemoteActionFrame::new(RemoteActionMessage::CatalogueResponse {
                        correlation,
                        result: Ok(CataloguePage {
                            revision: WINDOW_REVISION.into(),
                            ..CataloguePage::default()
                        }),
                    })
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
                    continue;
                }
                _ => continue,
            };
            // Only the host that certified this window, for this very
            // invocation and action, answers for the person — or the dispatch,
            // admitting the same call again on their approve of its asked
            // record.
            let approved = match &request.confirmation {
                Some(Confirmation::Approved(token)) => {
                    HostApproval::decode(token).is_some_and(|approval| {
                        approval.host_device_public_key == certifier
                            && approval.invocation == *request.invocation.0.as_bytes()
                            && approval.provider == request.target.provider
                            && approval.action == request.target.address.action
                            && approval.verify(tilo_clock::unix_now_ns()).is_ok()
                    })
                }
                Some(Confirmation::Asked(_)) => true,
                _ => false,
            };
            heard.lock().unwrap().push(Handed {
                from: from_device_public_key,
                approved,
            });
            let status = if asks.asks(&request.target.address.action) && !approved {
                ReceiptStatus::Rejected {
                    error: ActionCallError::asks(tilo_actions::remote::browser::departure_asked(
                        tilo_actions::remote::browser::stated_facts(&asks.departure()),
                    )),
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
    server: TestServer,
    browsers: Arc<HostedBrowsers>,
    environments: Arc<InProcessEnvironments>,
    meter: Arc<Counting>,
}

async fn fixture(meter: Arc<Counting>) -> Fixture {
    fixture_with(meter, tilo_server::hosted_browser::RECONNECT).await
}

/// A fixture that gives a disconnected environment device `reconnect` to come
/// back before its environment stops.
async fn fixture_with(meter: Arc<Counting>, reconnect: Duration) -> Fixture {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home", DeviceCapabilities::member());
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let agent = user.enrol("agent", DeviceCapabilities::member());
    let home = host.device_pubkey();
    let host_key = host.sk.clone();
    let server_cert = {
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
        .with_reconnect(reconnect),
    );
    let server = Box::pin(spawn_server_serving_apps(
        &user,
        host,
        &[],
        &[],
        Arc::new(Routing(Arc::clone(&browsers))),
    ))
    .await;
    // The person made both workspaces on their server, and their agent's
    // device entered each.
    let workspaces = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    for workspace in [WS, OTHER_WS] {
        let workspace = workspace.to_string();
        workspaces
            .create_workspace("Workspace", Some(&workspace))
            .await
            .expect("the person makes the workspace on their server");
        workspaces
            .enter(&workspace, &agent.cert)
            .await
            .expect("the agent's device enters it");
    }
    browsers.serve_from(tilo_server::hosted_browser::Authority {
        user_public_key: user.user_pk,
        cert: server_cert,
        key: host_key,
        server: server.server.node_addr(),
        workspaces: server.server.workspaces().cloned(),
        environments: server.server.environments(),
        runtime: tokio::runtime::Handle::current(),
        certification: server.server.confirmations(),
        hand: server.server.hand(),
    });
    browsers
        .place(WS_NAME, WS, "record-1")
        .expect("the environment was placed");
    Fixture {
        user,
        home,
        agent,
        laptop,
        server,
        browsers,
        environments,
        meter,
    }
}

/// The environments a test started, behind the trait the server holds.
struct EnvironmentsHandle(Arc<InProcessEnvironments>);

impl BrowserSessions for EnvironmentsHandle {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.start(material)
    }

    fn stop(&self, workspace: &str) {
        self.0.stop(workspace);
    }
}

/// The meter a test counts with, behind the trait the server holds.
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
    /// A key the person made for an agent to act in their workspaces, which
    /// the workspace lists. It reaches all of them: the server admits a key to
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
    let window = fixture.browsers.window(WS).unwrap();
    assert_eq!(window.provider, WINDOW);
    assert_eq!(fixture.environments.starts(), 1, "one environment opened");
    let material = fixture.environments.material();
    assert_eq!(material.record, "record-1");
    assert_eq!(material.workspace, WS_NAME);
    assert_eq!(material.workspace_id, WS);
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
        u64::try_from(tilo_server::hosted_browser::DEVICE_LIFETIME.as_nanos()).unwrap(),
        "it lasts as long as the environment may run"
    );
    assert_eq!(
        material.device.device_public_key, window.device,
        "the window is the device the server certified"
    );
    // A second open attaches to the window already up.
    let again = fixture.browsers.place(WS_NAME, WS, "record-1").unwrap();
    assert_eq!(again, window);
    assert_eq!(fixture.environments.starts(), 1, "no second environment");
}

impl Fixture {
    /// `key` enters the workspace, as a key its person made for it does.
    async fn list_environment_and(&self, key: &TestDevice) {
        tilo_workspace_service::workspaces::fixtures::enter_key(
            self.server
                .server
                .workspaces()
                .expect("the server holds workspaces"),
            WS_NAME,
            &key.cert,
        )
        .await;
    }
}

#[tokio::test]
async fn a_page_action_reaches_the_window_as_the_caller() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let caller = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_server(WS, fixture.home, frame(page(id, "read-page"))))
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
        from_device_public_key, fixture.home,
        "the answer comes from the host the caller asked, which handed it to the window"
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
        "the window ran it as the caller, not as the server: History records the agent's run"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1, "counted");
}

/// The window runs in the workspace's VM, yet a caller asking the host what
/// runs in the workspace finds it there, and a listing of the window is read
/// from the window: both answered as the host's, the device the caller asked.
/// A device that addresses the window itself hears the window.
#[tokio::test]
async fn the_host_lists_the_window_and_hands_its_listing_to_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let caller = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;
    let listing = |correlation, provider: Option<&str>| {
        frame(RemoteActionMessage::CatalogueRequest {
            correlation,
            query: CatalogueQuery {
                provider: provider.map(str::to_owned),
                ..CatalogueQuery::default()
            },
        })
    };
    let answered = |envelope: Envelope| {
        let Envelope::RemoteAction {
            from_device_public_key,
            frame,
            ..
        } = envelope
        else {
            panic!("a listing is answered: {envelope:?}");
        };
        let RemoteActionMessage::CatalogueResponse {
            result: Ok(page), ..
        } = RemoteActionFrame::decode(&frame).unwrap().message
        else {
            panic!("a listing is answered with a page");
        };
        (from_device_public_key, page)
    };

    // Before the host handed it anything of theirs, a device that asks the
    // window itself hears the window.
    caller
        .send(to_server(
            WS,
            material.device.device_public_key,
            listing(3, Some(WINDOW)),
        ))
        .unwrap();
    let (from, _) = answered(next(&caller, "caller").await);
    assert_eq!(
        from, material.device.device_public_key,
        "a device that asked the window hears the window"
    );

    caller
        .send(to_server(WS, fixture.home, listing(1, None)))
        .unwrap();
    let (from, page) = answered(next(&caller, "caller").await);
    assert_eq!(from, fixture.home);
    let window = page
        .providers
        .iter()
        .find(|listed| listed.provider == WINDOW)
        .unwrap_or_else(|| panic!("the window is listed: {:?}", page.providers));
    assert_eq!(window.app_id, "so.tilo.browser");
    assert_eq!(
        window.document_id, "record-1",
        "on the record it was opened on"
    );
    assert_eq!(
        window.workspace, WS_NAME,
        "named by the workspace it was opened in, so a caller holding what runs \
         in its workspace to that workspace keeps it"
    );
    assert!(window.browsable);
    assert!(
        page.providers
            .iter()
            .any(|listed| listed.provider == HOST_PROVIDER),
        "beside what the host publishes itself"
    );

    caller
        .send(to_server(WS, fixture.home, listing(2, Some(WINDOW))))
        .unwrap();
    let (from, page) = answered(next(&caller, "caller").await);
    assert_eq!(
        page.revision, WINDOW_REVISION,
        "the window answered its own listing"
    );
    assert_eq!(from, fixture.home, "as the host the caller asked");
}

/// A session takes as long to start as its VM takes to wake: meanwhile the
/// windows already up, and the workspace's listing, are answered at once.
#[tokio::test]
async fn a_listing_is_answered_while_another_window_is_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let (wake, waking) = std::sync::mpsc::channel();
    *fixture.environments.waking.lock().unwrap() = Some(waking);
    let browsers = Arc::clone(&fixture.browsers);
    let placing = std::thread::spawn(move || {
        browsers.place("22222222-2222-4222-8222-222222222222", OTHER_WS, "record-2")
    });
    while fixture.environments.waking.lock().unwrap().is_some() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let caller = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;
    for (correlation, provider) in [(1, None), (2, Some(WINDOW))] {
        caller
            .send(to_server(
                WS,
                fixture.home,
                frame(RemoteActionMessage::CatalogueRequest {
                    correlation,
                    query: CatalogueQuery {
                        provider: provider.map(str::to_owned),
                        ..CatalogueQuery::default()
                    },
                }),
            ))
            .unwrap();
    }
    for _ in 0..2 {
        // The person's list of workspaces is pushed to every session of theirs as it changes;
        // it answers nothing asked here.
        let answer = loop {
            let answer = tokio::time::timeout(Duration::from_secs(5), caller.recv())
                .await
                .expect("the listing is answered while the other window is placed");
            if !matches!(answer, Some(Envelope::PersonalList { .. })) {
                break answer;
            }
        };
        assert!(
            matches!(answer, Some(Envelope::RemoteAction { .. })),
            "{answer:?}"
        );
    }

    wake.send(()).unwrap();
    placing
        .join()
        .unwrap()
        .expect("the other window was placed");
}

#[tokio::test]
async fn a_page_action_past_the_allowance_is_refused_and_says_how_to_continue() {
    let meter = Arc::new(Counting::default());
    *meter.refusal.lock().unwrap() = Some(no_payment_method(3));
    let fixture = fixture(Arc::clone(&meter)).await;
    let material = fixture.environments.material();
    let seen = window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let caller = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_server(WS, fixture.home, frame(page(id, "fill-page"))))
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
        error: ActionCallError::Refused { reason },
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
    let material = fixture.environments.material();
    let seen = window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let environment = DeviceIdentity::of(&material);
    let client = ServerClient::connect(
        client_endpoint().await,
        fixture.server.server.node_addr(),
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

/// A key whose person let it operate web pages fills, selects and clicks in
/// the hosted Browser without anyone being asked; a submit the same key sends
/// is refused for want of the person's yes, stating what leaves and where, and
/// the dispatch that carried the call keeps it as its asked record — the page
/// runs it only when the same call comes back carrying their approve.
#[tokio::test]
async fn a_keys_page_actions_run_at_once_where_its_person_let_it_operate_them() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(true);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let seen = window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Departing,
    )
    .await;
    let caller = subscribe(&fixture.server.server, &fixture.user, &key).await;

    for action in ["navigate", "fill-page", "select-page", "click-page"] {
        let id = InvocationId::new();
        caller
            .send(to_server(WS, fixture.home, frame(page(id, action))))
            .unwrap();
        let ran = receipt_for(&caller, id).await;
        assert!(
            matches!(ran.status, ReceiptStatus::Completed { .. }),
            "{action} ran without asking: the page action was still a question ({ran:?})"
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
            seen.iter().all(|handed| handed.approved),
            "the host answered for the person, as their answer does"
        );
    }

    // Submitting is not operating a page: it is refused for want of the
    // person's yes, and states what leaves and where, for the record the
    // dispatch keeps of it.
    let id = InvocationId::new();
    caller
        .send(to_server(WS, fixture.home, frame(submit(id))))
        .unwrap();
    let ReceiptStatus::Rejected {
        error: ActionCallError::ConfirmationRequired(facts),
    } = receipt_of(next(&caller, "caller").await).status
    else {
        panic!("the submit waits for the person");
    };
    assert_eq!(facts.on.as_deref(), Some("https://visa.example/apply"));
    assert_eq!(facts.fact("kind"), Some("submit"));
    assert_eq!(facts.fact("method"), Some("POST"));
    assert_eq!(facts.fact("destination"), Some("https://visa.example"));
    assert_eq!(facts.fact("picture"), Some("tilo.content:5f2b"));
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "a submit nobody allowed has run nothing"
    );

    // The person allowed it: the same call comes back carrying their approve
    // of its asked record, and the window runs it once.
    caller
        .send(to_server(
            WS,
            fixture.home,
            frame(RemoteActionMessage::Invoke(answered_submit(id))),
        ))
        .unwrap();
    assert!(
        matches!(
            followed(&caller, fixture.home, id).await,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "their approve runs the submit they were asked about"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        5,
        "the submit they allowed is counted once"
    );
}

/// [`submit`], as the dispatch admits the call again on the person's approve
/// of the asked record it was kept as.
fn answered_submit(invocation: InvocationId) -> InvokeRequest {
    let RemoteActionMessage::Invoke(mut request) = submit(invocation) else {
        panic!("a submit is an invocation");
    };
    request.confirmation = Some(Confirmation::Asked(tilo_actions::remote::asked::Approval {
        record: tilo_actions::remote::asked::RecordRef {
            workspace: WS.to_string(),
            record: format!("invocations/{}", invocation.0),
        },
        choice: None,
    }));
    request
}

/// The record a customer wrote before Browser opened: another app's.
const NOTES: &str = "so.tilo.notes";
const NOTES_RECORD: &str = "notes-1";

/// The channel the window's own record lives on, and the one a customer's
/// document lives on: what every protocol of the workspace tells a record by.
fn window_channel() -> [u8; 32] {
    tilo_server::environment::document_channel(tilo_server::hosted_browser::APP, "record-1")
}

fn notes_channel() -> [u8; 32] {
    tilo_server::environment::document_channel(NOTES, NOTES_RECORD)
}

/// One change of `channel`'s record, as a device writes it.
fn change(channel: [u8; 32], marker: &[u8]) -> Vec<u8> {
    tilo_server::coedit::CoeditPayload::RetainedDelta {
        channel,
        body: marker.to_vec(),
    }
    .encode()
    .unwrap()
}

/// Every envelope a session hears in `within`, the ones it already holds first.
async fn heard(client: &ServerClient, within: std::time::Duration) -> Vec<Envelope> {
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
            match tilo_server::coedit::CoeditPayload::decode(delta)? {
                tilo_server::coedit::CoeditPayload::Checkpoint { channel, .. }
                | tilo_server::coedit::CoeditPayload::RetainedDelta { channel, .. } => {
                    Some(channel)
                }
            }
        })
        .collect()
}

impl Fixture {
    /// The environment's own session: the device the server certified, joining
    /// the one workspace it was placed in.
    async fn environment_session(&self) -> ServerClient {
        let material = self.environments.material();
        let environment = DeviceIdentity::of(&material);
        subscribe(&self.server.server, &self.user, &environment).await
    }
}

#[tokio::test]
async fn the_server_confines_an_environments_device_to_the_record_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    assert_eq!(
        tilo_server::environment::environment_of(&fixture.server.server.environments(), &device),
        Some(tilo_server::environment::Environment::Placed(
            tilo_server::environment::EnvironmentScope::for_resource(
                WS,
                tilo_server::hosted_browser::APP,
                "record-1"
            )
        )),
        "the placement names the one record its device reaches"
    );
}

#[tokio::test]
async fn a_server_that_restarts_still_confines_the_environment_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    // What a server that starts again reads: its own register, not the
    // workspace's listing, which still has the device in it.
    let restarted = tilo_server::environment::EnvironmentRegistry::at(
        fixture.server.state_root.join("environments.json"),
    );
    assert_eq!(
        tilo_server::environment::environment_of(&restarted, &device),
        Some(tilo_server::environment::Environment::Placed(
            tilo_server::environment::EnvironmentScope::for_resource(
                WS,
                tilo_server::hosted_browser::APP,
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
    let environment = fixture.environment_session().await;
    let writer = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: WS,
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

/// What the workspace's store keeps that `device` wrote, as each write's
/// channel and encoded message. The workspace keeps its own governance beside
/// it, which is not counted.
fn written_by(server: &tilo_server::Server, device: [u8; 32]) -> Vec<([u8; 32], Vec<u8>)> {
    server
        .workspace_documents()
        .retained(WS)
        .unwrap_or_default()
        .into_iter()
        .filter(|kept| kept.author == device)
        .filter_map(|kept| {
            let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(&kept.body).ok()?;
            let (channel, _) = neighbour_wire::retention(&message, false)?;
            Some((channel, kept.body))
        })
        .collect()
}

#[tokio::test]
async fn an_environment_writes_its_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let environment = fixture.environment_session().await;
    let device = fixture.environments.material().device.device_public_key;
    environment
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(notes_channel(), b"overwritten by a page"),
            signature: [0; 64],
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    let Envelope::NotAuthorised {
        workspace_id,
        refusal,
    } = refusal
    else {
        panic!("a write to another record is refused: {refusal:?}");
    };
    assert_eq!(workspace_id, WS);
    assert!(
        matches!(&refusal, tilo_server_protocol::refusal::Refusal::NotAllowed(rule) if rule.contains("environment")),
        "the refusal names the rule: {refusal}"
    );

    environment
        .send(Envelope::Delta {
            workspace_id: WS,
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
    // The server keeps a write as it takes it in, and says nothing back to its writer: the
    // store is read until the write is there, however long a loaded server takes.
    let retained = tilo_test_env::within("the window's write is kept", async {
        loop {
            let kept = written_by(&fixture.server.server, device);
            if !kept.is_empty() {
                return kept;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        retained
            .iter()
            .map(|(channel, _)| *channel)
            .collect::<Vec<_>>(),
        [window_channel()],
        "the window's own write is the only record this workspace keeps from it"
    );
}

#[tokio::test]
async fn an_environment_opens_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let environment = fixture.environment_session().await;
    environment
        .send(Envelope::OpenDocument {
            workspace_id: WS,
            channel: notes_channel(),
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    assert!(
        matches!(&refusal, Envelope::NotAuthorised { refusal, .. } if refusal.to_string().contains("environment")),
        "another record does not open for it: {refusal:?}"
    );
    environment
        .send(Envelope::OpenDocument {
            workspace_id: WS,
            channel: window_channel(),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::DocumentOpened { channel, .. } if channel == window_channel()),
        "its own record opens: {answer:?}"
    );
}

#[tokio::test]
async fn an_environments_replay_is_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let writer = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: WS,
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
    let client = ServerClient::connect(
        client_endpoint().await,
        fixture.server.server.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client
        .send(Envelope::Subscribe { workspace_id: WS })
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
    let material = fixture.environments.material();
    let seen = window_session(
        &fixture.server.server,
        &fixture.user,
        &material,
        Asks::Never,
    )
    .await;
    let environment = fixture.environment_session().await;
    // The window's device calls the workspace's entry itself: not its to do.
    environment
        .send(Envelope::RemoteAction {
            workspace_id: WS,
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
                signed: None,
            })),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::RemoteActionUndeliverable { .. }),
        "a window's device calls no entry of the workspace: {answer:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stopped_environment_reaches_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    // The session that holds the window is here, and stays connected over the
    // stop.
    let environment = fixture.environment_session().await;
    fixture.browsers.stopped(WS);
    assert_eq!(
        tilo_server::environment::environment_of(&fixture.server.server.environments(), &device),
        Some(tilo_server::environment::Environment::Stopped {
            until_ns: material.device.expires_at_ns.unwrap()
        }),
        "a stopped environment's device is refused until its certificate expires"
    );
    assert_eq!(
        fixture.environments.stops(),
        1,
        "the session in the VM was ended"
    );
    // Its connection here is cut: the protocols it carried refuse it.
    assert!(
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            while environment.recv().await.is_some() {}
        })
        .await
        .is_ok(),
        "a stopped environment's device has no connection left"
    );
    // It dials again: refused, not open.
    let environment = DeviceIdentity::of(&material);
    let refused = ServerClient::connect(
        client_endpoint().await,
        fixture.server.server.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the environment's device dials the host");
    refused
        .send(Envelope::Subscribe { workspace_id: WS })
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
        tilo_server::hosted_browser::ANSWER_WAIT,
    )
    .await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    let environment = fixture.environment_session().await;
    drop(environment);
    // The server hears that the device's session ended and stops its
    // environment: one signal, and no protocol admits it after.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture
            .server
            .server
            .environments()
            .of(&device, tilo_clock::unix_now_ns())
            == Some(tilo_server::environment::Environment::Stopped {
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
        signed: None,
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
    let environment = fixture.environment_session().await;
    let id = InvocationId::new();
    environment
        .send(to_server(
            WS,
            fixture.home,
            frame(asks_about_itself(
                id,
                tilo_actions::remote::environment_device::MEDIA_KEYS,
                InvokeArgument::None,
            )),
        ))
        .unwrap();
    let receipt = receipt_for(&environment, id).await;
    // The workspace is held here, so this server answers with its keys sealed
    // to the environment: the ask reached it, where every other action an
    // environment starts is refused.
    assert!(
        matches!(
            &receipt.status,
            ReceiptStatus::Completed { output }
                if output.value.as_ref().is_some_and(|value| value.get("sealed").is_some())
        ),
        "the environment's own ask is the host's to answer: {receipt:?}"
    );

    let agent = subscribe(&fixture.server.server, &fixture.user, &fixture.agent).await;
    let id = InvocationId::new();
    agent
        .send(to_server(
            WS,
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
                error: ActionCallError::NotPermitted
            }
        ),
        "a member that is no environment of the workspace is lent nothing: {receipt:?}"
    );
}

#[tokio::test]
async fn an_environment_shares_no_window_it_did_not_mint() {
    let fixture = fixture(Arc::new(Counting::default())).await;
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
            .send(to_server(
                WS,
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
                    error: ActionCallError::NotPermitted
                }
            ),
            "{action} of another device's window is refused: {receipt:?}"
        );
    }
}

#[tokio::test]
async fn an_environment_writes_nothing_of_the_workspaces_own_document_and_stays() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let environment = fixture.environment_session().await;
    let device = fixture.environments.material().device.device_public_key;
    environment
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: b"a shell layout and a grant of its own making".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    // Neither refused nor kept: the session stays, and writes its own record.
    environment
        .send(Envelope::Delta {
            workspace_id: WS,
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
        "a session that writes its shell as any session does is not cut for it: {after:?}"
    );
    let retained = written_by(&fixture.server.server, device);
    assert!(
        !retained.iter().any(|(_, body)| body
            .windows(b"a shell layout".len())
            .any(|window| window == b"a shell layout")),
        "the workspace keeps nothing of its document from the environment's device"
    );
    assert_eq!(
        retained.len(),
        1,
        "its own record's write is kept: {retained:?}"
    );
}

/// The session in a workspace's VM joins the workspace, and the connection it
/// joins over ends right after: after that the session takes part over its own
/// lasting link and the live session it publishes its window on. The live session
/// is therefore the server's only sight of the device being here, and it must
/// count as such — otherwise the window stops a reconnect-wait after the join
/// ends, as it did before any protocol counted the device.
#[tokio::test]
async fn a_live_session_keeps_the_window_running_past_the_reconnect_wait() {
    // The wait the deployment gives a dropped session is 30 s; the same wait,
    // short, proves the same answer here without a minute of sleeping.
    let reconnect = Duration::from_millis(600);
    let fixture = fixture_with(
        Arc::new(Counting::default()),
        reconnect,
        tilo_server::hosted_browser::ANSWER_WAIT,
    )
    .await;
    // The workspace lists the window's device, as the server lists it.
    let material = fixture.environments.material();

    // The join: the session's device enters the workspace, and that
    // connection ends at once, as the invitation connection in the VM does.
    let joined = subscribe(
        &fixture.server.server,
        &fixture.user,
        &DeviceIdentity::of(&material),
    )
    .await;
    // The lasting connection the session keeps: the live session it publishes
    // its window on.
    let (endpoint, live) = live_session(&fixture.server.server, &material).await;
    // The join connection ends the way the session's own does: its endpoint
    // closes, having told the server it is going.
    joined.close().await;

    tokio::time::sleep(reconnect * 4).await;
    assert_eq!(
        fixture.environments.stops(),
        0,
        "a device with a live session is still here, though its join connection ended"
    );

    // The session went away for good: its last connection ending stops it,
    // and with it the window. The connection's teardown reaches the live session a
    // moment after it is dropped, so the stop is watched for.
    endpoint.close().await;
    drop(live);
    let deadline = Instant::now() + Duration::from_secs(15);
    while fixture.environments.stops() == 0 && Instant::now() < deadline {
        tokio::time::sleep(reconnect).await;
    }
    assert_eq!(
        fixture.environments.stops(),
        1,
        "the window stops once its session's last connection ended"
    );
}

/// The environment's device joins the workspace's live session as the session
/// in the VM does: over its own endpoint key, admitted by the certificate the
/// server issued it. The session stays open while the returned endpoint and
/// [`Connected`](tilo_live::admit::Connected) are held.
async fn live_session(
    server: &tilo_server::Server,
    material: &Material,
) -> (iroh::endpoint::Endpoint, tilo_live::admit::Connected) {
    let endpoint = device_endpoint(&DeviceIdentity::of(material)).await;
    let scope = tilo_live::scope::Scope::workspace(&material.workspace);
    let grant = tilo_live::grant::Native::new(
        *material.workspace_id.as_uuid().as_bytes(),
        &scope.id(),
        0,
        serde_json::to_vec(&material.device).unwrap(),
        Vec::new(),
    )
    .unwrap()
    .encode();
    let dial = tilo_live::native::Iroh::new(endpoint.clone(), server.node_addr());
    let joined = tilo_live::admit::Dial::dial(&dial, &grant, None, None, Duration::from_secs(20))
        .await
        .expect("the live session admits the environment's device");
    (endpoint, joined)
}

/// An app's open answers with the run it started, not with the record it made: the server follows
/// that run and places the window when the record exists, so an agent's open still runs where its
/// record is. The caller's own answer is unchanged — it follows its run as it does any other.
#[tokio::test]
async fn an_open_that_answers_with_its_run_places_the_window_when_the_record_exists() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    // A workspace this host governs: an open placed from its run names the workspace by its id.
    let second = fixture
        .server
        .server
        .workspaces()
        .unwrap()
        .create("Second")
        .await
        .unwrap();
    let workspace = second.workspace_id;
    let invocation = InvocationId::new();
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observe: Arc<tilo_server::hosted_browser::ObserveInvocation> = {
        let polls = Arc::clone(&polls);
        Arc::new(move |_, _| {
            // The run is admitted first, then makes the record.
            let update = if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                tilo_actions::outcome::InvocationUpdate::Accepted { invocation }
            } else {
                tilo_actions::outcome::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"record": "record-2"})),
                        resources: Vec::new(),
                    },
                }
            };
            Some(tilo_actions::remote::InvocationState::new(
                update.invocation(),
                update,
            ))
        })
    };
    let answer = tilo_server::hosted_browser::placed(
        &fixture.browsers,
        workspace,
        invocation,
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        }),
        Some(observe),
        &Caller::local_app("test"),
    )
    .await;
    assert!(
        matches!(&answer, RemoteActionMessage::Receipt(receipt) if matches!(receipt.status, ReceiptStatus::Accepted)),
        "the caller is answered its run, as it was"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.browsers.window(workspace).is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let window = fixture
        .browsers
        .window(workspace)
        .expect("the run's record placed the workspace's window");
    assert_eq!(window.provider, WINDOW);
}
