//! A hosted workspace's Browser: opening it places an environment whose
//! session joins the workspace as a device and holds the window, and an
//! agent's page actions reach that window as the agent's — counted, and
//! refused with the host's typed refusal past the free allowance. A page
//! action the window asks the person about is put to them by the server
//! itself, and the caller waits for their answer.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::remote::server_account::{NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, ActionProvider, Caller,
    CataloguePage, CatalogueQuery, Confirmation, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    workspace_provider,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::actions::testing::duplex;
use tilo_peer_state::actions::{ActionService, RemoteDevice};
use tilo_peer_state::direct::{Heard, Replica};
use tilo_server::hosted_browser::{
    BrowserMeter, BrowserSessions, HostedBrowsers, Material, Window, no_payment_method,
};
use tilo_server::{Server, WorkspaceId};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationList, prepare_stand_in_cert,
    sign_device_cert,
};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT};

use crate::common::{Linked, TestDevice, TestServer, TestUser, spawn_server};
use tilo_server::hosted_browser::APP;
use tilo_workspace_service::agents::windows::PAGE_RESOURCE;

const WS: WorkspaceId = WorkspaceId::from_u128(0x1111_1111_1111_4111_8111_1111_1111_1111);
const OTHER_WS: WorkspaceId = WorkspaceId::from_u128(0x2222_2222_2222_4222_8222_2222_2222_2222);
const WS_NAME: &str = "11111111-1111-4111-8111-111111111111";
const WINDOW: &str = "browser@42";
/// What the host publishes for the workspace itself.
const HOST_PROVIDER: &str = "workspace:11111111-1111-4111-8111-111111111111";
/// The revision the window's own listing answers at.
const WINDOW_REVISION: &str = "window";
/// The record the window was opened on.
const WINDOW_RECORD: &str = "record-1";

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

/// `request` signed by `asker`, the device that asks it: what carries the
/// caller's identity to a window the host hands the action on to, since a
/// window's own link proves no device of its own.
fn as_asked(asker: &TestDevice, mut request: InvokeRequest) -> InvokeRequest {
    request.signed = None;
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            WS_NAME,
            &request,
            &asker.cert,
            &asker.sk,
            tilo_clock::unix_now_ns(),
        )
        .expect("the asking device signs its invocation"),
    ));
    request
}

/// The page action `action` of the window, asked by `asker`.
fn page(asker: &TestDevice, invocation: InvocationId, action: &str) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
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
        },
    )
}

/// A page action with `confirmation` attached.
fn page_with(
    asker: &TestDevice,
    invocation: InvocationId,
    action: &str,
    confirmation: Option<Confirmation>,
) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
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
            signed: None,
        },
    )
}

/// The submit the agent asks for, as a person's approving device is shown it:
/// the purpose it acts on, by the words the agent gave it.
fn submit(asker: &TestDevice, invocation: InvocationId) -> InvokeRequest {
    let mut request = page(asker, invocation, "submit-page");
    request.context = Some(tilo_actions::ActionContext {
        purpose: Some("Submit the application".into()),
        ..tilo_actions::ActionContext::default()
    });
    // The context is what a person reads, so it is signed with the rest.
    as_asked(asker, request)
}

/// The receipt `answer` carries.
fn receipt_of(answer: Option<RemoteActionMessage>) -> InvocationReceipt {
    match answer.expect("the caller is answered") {
        RemoteActionMessage::Receipt(receipt) => receipt,
        other => panic!("an invocation is answered with a receipt: {other:?}"),
    }
}

/// How `invocation` of `caller`'s went, followed until it is over: what a
/// caller that asked once and waits sees.
async fn followed(
    remote: &RemoteDevice,
    caller: &Caller,
    invocation: InvocationId,
) -> tilo_actions::InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(state) = observed(remote, caller, invocation).await
            && state.update.is_terminal()
        {
            return state.update;
        }
        assert!(Instant::now() < deadline, "the invocation never ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What `caller` is told of `invocation` now, when the host holds it. The device is asked from
/// a thread of its own: its answer waits on the request it sends, which this test's one runtime
/// thread carries.
async fn observed(
    remote: &RemoteDevice,
    caller: &Caller,
    invocation: InvocationId,
) -> Option<InvocationState> {
    let (remote, caller) = (remote.clone(), caller.clone());
    tokio::task::spawn_blocking(move || ActionProvider::observe(&remote, &caller, &invocation))
        .await
        .expect("the observe finishes")
}

/// Every question that waits for the person, as the reading of the workspace's invocations names
/// them: each call that waits for their answer, with what they are asked.
async fn questions(
    remote: &RemoteDevice,
    caller: &Caller,
) -> Vec<tilo_actions::remote::asked::Asked> {
    use tilo_actions::remote::workspace_invocations::{self, Invocation, InvocationStatus};
    let read = tilo_actions::remote::ResourceReadRequest {
        workspace: WS_NAME.to_owned(),
        app: workspace_invocations::PROVIDER.to_owned(),
        record: String::new(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: tilo_actions::remote::ReadAnswer::Reading,
    };
    let answer = remote
        .ask_message(RemoteActionMessage::ResourceRead {
            correlation: 1,
            on_behalf_of: None,
            read,
        })
        .await
        .and_then(Result::ok);
    let Some(RemoteActionMessage::ResourceReading { result, .. }) = answer else {
        panic!("the host reads its invocations: {answer:?}");
    };
    let readings: serde_json::Value =
        serde_json::from_str(&result.expect("the person reads the workspace's invocations"))
            .expect("the readings are JSON");
    let items = readings["records"][0]["value"]["items"].clone();
    let person = caller.user_public_key().map(hex::encode);
    serde_json::from_value::<Vec<Invocation>>(items)
        .expect("the items read as invocations")
        .into_iter()
        .filter(|item| item.status == InvocationStatus::Waiting && item.person == person)
        .filter_map(|item| item.asked.map(|asked| *asked))
        .collect()
}

/// The question the person's device sees waiting for them, once it is asked: an asked
/// invocation record, read in the workspace's invocations.
async fn asked(remote: &RemoteDevice, caller: &Caller) -> tilo_actions::remote::asked::Asked {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(question) = questions(remote, caller).await.into_iter().next() {
            return question;
        }
        assert!(Instant::now() < deadline, "the person was never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The person answers `question` on their device, as `<record>#resolve`.
async fn answer(
    remote: &RemoteDevice,
    caller: &Caller,
    question: &tilo_actions::remote::asked::Asked,
    resolution: tilo_actions::remote::asked::Offer,
) {
    let resolution = match resolution {
        tilo_actions::remote::asked::Offer::Approve => "approve",
        tilo_actions::remote::asked::Offer::Decline => "decline",
        tilo_actions::remote::asked::Offer::ApproveAndGrant => "approve_and_grant",
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(
                &question.record.record,
                tilo_actions::remote::asked::RESOLVE,
            ),
        },
        argument: InvokeArgument::Json(serde_json::Map::from_iter([(
            "resolution".to_owned(),
            serde_json::Value::String(resolution.to_owned()),
        )])),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = ActionProvider::invoke(remote, caller, request)
        .await
        .expect("the host answers")
        .expect("the person's answer is admitted");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the record is answered: {receipt:?}"
    );
}

/// A device of the workspace, linked to `server`, and the host's actions asked
/// over the link it holds open.
async fn member_link(server: &Server, device: &TestDevice) -> Linked {
    Linked::open(server, device, WS)
        .await
        .expect("the workspace admits its device")
}

/// The host's actions `linked`'s device asks in `workspace`.
async fn asking(linked: &Linked, workspace: &str) -> RemoteDevice {
    linked
        .asking_in(workspace)
        .await
        .expect("the host answers its actions link")
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
#[derive(Clone)]
struct WindowActions {
    seen: Arc<Mutex<Vec<Handed>>>,
    asks: Asks,
    /// The host that placed this window: the only device whose word for the
    /// person a page action of this window carries.
    placement: tilo_actions::remote::browser::Placement,
}

impl ActionProvider for WindowActions {
    /// What the window offers, read from the window.
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage {
            revision: WINDOW_REVISION.into(),
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // Only the host that placed this window, over its own link, for the
        // workspace it placed it in, answers for the person; the dispatch that
        // admitted the same call again on the person's approve of its asked
        // record hands the window their own answer.
        let approved = match tilo_actions::remote::browser::host_answered(
            Some(&self.placement),
            caller,
            request.confirmation.as_ref(),
        ) {
            Ok(answered) => {
                answered || matches!(request.confirmation, Some(Confirmation::Asked(_)))
            }
            Err(refused) => return Pending::ready(Err(refused)),
        };
        self.seen.lock().unwrap().push(Handed {
            from: *caller
                .device_public_key()
                .expect("a page action names the device that asked it"),
            approved,
        });
        let status = if self.asks.asks(&request.target.address.action) && !approved {
            // What the person is shown: the departure the call itself states where it states
            // one, the window's own otherwise, and the page it acts on when it states neither.
            let stated = tilo_actions::remote::browser::departure_facts(request.context.as_ref());
            let stated = if stated.is_empty() {
                tilo_actions::remote::browser::stated_facts(&self.asks.departure())
            } else {
                stated
            };
            let mut facts = tilo_actions::remote::browser::departure_asked(stated);
            if facts.on.is_none() {
                facts.on = Some(PAGE_RESOURCE.to_owned());
            }
            if facts.words.is_empty()
                && let Some(purpose) = request.context.as_ref().and_then(|c| c.purpose.clone())
            {
                facts.words = purpose;
            }
            ReceiptStatus::Rejected {
                error: ActionCallError::asks(facts),
            }
        } else {
            ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            }
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}

/// The window's device, serving the page actions the host hands it over the
/// link it holds open to the host, and the page actions it was handed.
async fn window_actions(
    server: &Server,
    material: &Material,
    asks: Asks,
) -> Arc<Mutex<Vec<Handed>>> {
    window_served(server, material, asks).await.0
}

/// [`window_actions`], and the window's own action service, which serves any
/// other link a device holds open to it.
async fn window_served(
    server: &Server,
    material: &Material,
    asks: Asks,
) -> (Arc<Mutex<Vec<Handed>>>, ActionService) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceIdentity::of(material);
    let host = server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .device();
    let actions = ActionService::new(
        device.sk.clone(),
        Arc::new(WindowActions {
            seen: Arc::clone(&seen),
            asks,
            placement: tilo_actions::remote::browser::Placement {
                host,
                workspace: material.workspace.clone(),
            },
        }),
    );
    // What a window holds of the workspace, as the host holds it: who asks a
    // page action, and who may.
    let state = server
        .workspaces()
        .expect("the server holds workspaces")
        .state(WS_NAME)
        .await
        .expect("the server holds the workspace");
    actions.hold(Replica::new(state));
    // The link the window holds to the host, recorded with the host's action
    // service as the way a page action reaches the window.
    let (host_end, window_end) = duplex(Some(device.device_pubkey()), None);
    actions.serve_link(window_end);
    server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .reach_device_at(device.device_pubkey(), host_end);
    (seen, actions)
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
    /// The device the server serves its workspaces as.
    host_device: TestDevice,
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
    let host_device = host.clone();
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
            Arc::clone(&environments) as Arc<dyn BrowserSessions>,
            Box::new(SharedMeter(Arc::clone(&meter))),
            "acct_ada".into(),
        )
        .with_reconnect(reconnect),
    );
    let server = Box::pin(spawn_server(&user, host, &[], &[])).await;
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
            .enter(&workspace, (&agent.cert, &agent.sk))
            .await
            .expect("the agent's device enters it");
        workspaces
            .enter(&workspace, (&laptop.cert, &laptop.sk))
            .await
            .expect("the person's own device enters it");
    }
    // The host answers the workspace's open of Browser and the page actions of the window it
    // places through its one dispatch, as production registers it.
    browsers.provide_in(&workspaces.providers());
    browsers.serve_from(tilo_server::hosted_browser::Authority {
        user_public_key: user.user_pk,
        cert: server_cert,
        key: host_key,
        server: server.server.node_addr(),
        workspaces: server.server.workspaces().cloned(),
        environments: server.server.environments(),
        runtime: tokio::runtime::Handle::current(),
        hand: server.server.hand(),
    });
    // The server keeps one store for every call that waits for a person's yes here; the
    // Browser window's own questions wait in it, and the person's answer comes back to the
    // window that asked.
    let asked = workspaces
        .asked_store()
        .expect("the server keeps the calls that wait");
    browsers.keep_asked_in(&asked);
    browsers
        .place(WS_NAME, WS, WINDOW_RECORD)
        .expect("the environment was placed");
    Fixture {
        user,
        host_device,
        home,
        agent,
        laptop,
        server,
        browsers,
        environments,
        meter,
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
    /// The person's approving device, linked to the server: where a page action
    /// that asks first is put to them, and where they answer it.
    async fn approving_device(&self) -> Linked {
        member_link(&self.server.server, &self.laptop).await
    }

    /// A key the person made for an agent to act in their workspaces, which
    /// the workspace lists. It reaches all of them: the server admits a key to
    /// the workspaces its certificate reaches and no other, which is where a
    /// key for one workspace is confined to it.
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
    assert_eq!(material.record, WINDOW_RECORD);
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
    let again = fixture.browsers.place(WS_NAME, WS, WINDOW_RECORD).unwrap();
    assert_eq!(again, window);
    assert_eq!(fixture.environments.starts(), 1, "no second environment");
}

/// A host that runs no Browser places no window, and registers no provider for one: an open of
/// Browser is answered by the dispatch's own refusal, in words that say Browser does not run
/// here.
#[tokio::test]
async fn an_open_of_browser_is_refused_in_words_where_the_host_runs_no_browser() {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home", DeviceCapabilities::member());
    let agent = user.enrol("agent", DeviceCapabilities::member());
    let server = Box::pin(spawn_server(&user, host, &[WS], &[&agent])).await;
    let linked = member_link(&server.server, &agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(user.user_pk);
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_provider(WS_NAME),
            address: ActionAddress::new(
                format!("{APPLICATION_RESOURCE_PREFIX}{APP}"),
                APPLICATION_OPEN_ACTION,
            ),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let refused = ActionProvider::invoke(&remote, &caller, request)
        .await
        .expect("the host answers")
        .expect_err("no Browser runs here");
    let ActionCallError::Unavailable { reason } = refused else {
        panic!("the dispatch refuses an open of a Browser it does not run: {refused:?}");
    };
    assert!(
        reason.contains("Browser"),
        "the refusal says what does not run here: {reason}"
    );
}

impl Fixture {
    /// `key` enters the workspace, as a key its person made for it does.
    async fn list_environment_and(&self, key: &TestDevice) {
        self.server
            .server
            .workspaces()
            .expect("the server holds workspaces")
            .enter(WS_NAME, (&key.cert, &key.sk))
            .await
            .expect("the key lists itself");
    }
}

#[tokio::test]
async fn a_page_action_reaches_the_window_as_the_caller() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Never).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = tilo_test_env::within(
        "the window answers the page action",
        ActionProvider::invoke(&remote, &caller, page(&fixture.agent, id, "read-page")),
    )
    .await
    .expect("the host answers")
    .expect("the window runs it");
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(
        remote.device(),
        fixture.home,
        "the request was answered by the host the caller asked, which handed it to the window"
    );
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
    window_actions(&fixture.server.server, &material, Asks::Never).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let listing = |provider: Option<&str>| CatalogueQuery {
        provider: provider.map(str::to_owned),
        ..CatalogueQuery::default()
    };

    // Before the host handed it anything of theirs, a device that asks the
    // window itself hears the window.
    let to_window = linked
        .asking_device_in(WS_NAME, material.device.device_public_key)
        .await
        .expect("the host answers its actions link");
    let page = tilo_test_env::within(
        "the window answers its own listing",
        ActionProvider::query(&to_window, &caller, listing(Some(WINDOW))),
    )
    .await
    .expect("the host answers")
    .expect("the listing is answered");
    assert_eq!(page.revision, WINDOW_REVISION);
    assert_eq!(
        to_window.device(),
        material.device.device_public_key,
        "a device that asked the window hears the window"
    );

    let page = tilo_test_env::within(
        "the host lists the window",
        ActionProvider::query(&remote, &caller, listing(None)),
    )
    .await
    .expect("the host answers")
    .expect("the listing is answered");
    assert_eq!(
        remote.device(),
        fixture.home,
        "as the host the caller asked"
    );
    let window = page
        .providers
        .iter()
        .find(|listed| listed.provider == WINDOW)
        .unwrap_or_else(|| panic!("the window is listed: {:?}", page.providers));
    assert_eq!(window.app_id, "so.tilo.browser");
    assert_eq!(
        window.document_id, WINDOW_RECORD,
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

    let page = tilo_test_env::within(
        "the host hands the listing to the window",
        ActionProvider::query(&remote, &caller, listing(Some(WINDOW))),
    )
    .await
    .expect("the host answers")
    .expect("the listing is answered");
    assert_eq!(
        page.revision, WINDOW_REVISION,
        "the window answered its own listing"
    );
}

/// A session takes as long to start as its VM takes to wake: meanwhile the
/// windows already up, and the workspace's listing, are answered at once.
#[tokio::test]
async fn a_listing_is_answered_while_another_window_is_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    window_actions(&fixture.server.server, &material, Asks::Never).await;
    let (wake, waking) = std::sync::mpsc::channel();
    *fixture.environments.waking.lock().unwrap() = Some(waking);
    let browsers = Arc::clone(&fixture.browsers);
    let placing = std::thread::spawn(move || {
        browsers.place("22222222-2222-4222-8222-222222222222", OTHER_WS, "record-2")
    });
    while fixture.environments.waking.lock().unwrap().is_some() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    for provider in [None, Some(WINDOW)] {
        let query = CatalogueQuery {
            provider: provider.map(str::to_owned),
            ..CatalogueQuery::default()
        };
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            ActionProvider::query(&remote, &caller, query),
        )
        .await
        .expect("the listing is answered while the other window is placed")
        .expect("the host answers");
        assert!(answer.is_ok(), "the listing is answered: {answer:?}");
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
    let seen = window_actions(&fixture.server.server, &material, Asks::Never).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;

    let id = InvocationId::new();
    let receipt = receipt_of(
        remote
            .ask_message(RemoteActionMessage::Invoke(page(
                &fixture.agent,
                id,
                "fill-page",
            )))
            .await
            .and_then(Result::ok),
    );
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
    let seen = window_actions(&fixture.server.server, &material, Asks::Never).await;
    let environment = DeviceIdentity::of(&material);
    // A device of one workspace joins no other: the host refuses the link.
    let elsewhere = Linked::open(&fixture.server.server, &environment, OTHER_WS).await;
    assert!(
        elsewhere.is_err(),
        "a device of one workspace joins no other"
    );
    assert!(seen.lock().unwrap().is_empty());
}

/// The agent reads and fills the page a visitor may read and fill without
/// anyone being asked; the submit the same agent sends is put to the person
/// by the server itself — no caller-side confirmation, no entry of its own.
#[tokio::test]
async fn an_agents_reads_and_fills_run_at_once_and_its_submit_is_put_to_the_person() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Submit).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    for action in ["read-page", "find-page-element", "fill-page", "select-page"] {
        let id = InvocationId::new();
        let ran = ActionProvider::invoke(&remote, &caller, page(&fixture.agent, id, action))
            .await
            .expect("the host answers")
            .expect("the window runs it");
        assert!(
            matches!(ran.status, ReceiptStatus::Completed { .. }),
            "{action} ran without asking: {ran:?}"
        );
        assert!(
            questions(&approver_actions, &approver_caller)
                .await
                .is_empty(),
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
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Accepted,
        "the submit waits for the person rather than refused"
    );
    let question = asked(&approver_actions, &approver_caller).await;
    assert_eq!(question.facts.words, "Submit the application");
    assert_eq!(question.facts.on.as_deref(), Some(PAGE_RESOURCE));
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "a submit that waits for the person has run nothing"
    );
    assert_eq!(seen.lock().unwrap().len(), 5, "the window was asked once");
    assert!(
        !seen.lock().unwrap()[4].approved,
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
    let material = fixture.environments.material();
    window_actions(&fixture.server.server, &material, Asks::Departing).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Accepted,
        "the submission waits for the person rather than refused"
    );
    let question = asked(&approver_actions, &approver_caller).await;
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
    let material = fixture.environments.material();
    window_actions(&fixture.server.server, &material, Asks::Departing).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, page(&fixture.agent, id, "click-page"))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Accepted,
        "the click waits for the person rather than refused"
    );
    assert_leaves(&asked(&approver_actions, &approver_caller).await);
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "a click nobody allowed has sent nothing"
    );
}

/// The question `question` is, as a person reads one about a departure: the
/// page it leaves from, what it does, the method, where it goes, and the
/// picture of the filled page.
fn assert_leaves(question: &tilo_actions::remote::asked::Asked) {
    assert_eq!(
        question.facts.on.as_deref(),
        Some("https://visa.example/apply"),
        "the question names the page it leaves from, not an internal record"
    );
    assert_eq!(
        question.facts.fact("kind"),
        Some("submit"),
        "the person is told what leaves and where"
    );
    assert_eq!(question.facts.fact("method"), Some("POST"));
    assert_eq!(
        question.facts.fact("destination"),
        Some("https://visa.example")
    );
    assert_eq!(
        question.facts.fact("picture"),
        Some("tilo.content:5f2b"),
        "and the picture of the filled page, by its content address"
    );
    assert!(
        question
            .offers
            .contains(&tilo_actions::remote::asked::Offer::Approve)
            && question
                .offers
                .contains(&tilo_actions::remote::asked::Offer::Decline),
        "the person allows it or declines it: {:?}",
        question.offers
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
    let seen = window_actions(&fixture.server.server, &material, Asks::Acting).await;
    let linked = member_link(&fixture.server.server, &key).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    for action in ["navigate", "fill-page", "select-page", "click-page"] {
        let id = InvocationId::new();
        let ran = ActionProvider::invoke(&remote, &caller, page(&key, id, action))
            .await
            .expect("the host answers")
            .expect("the window runs it");
        assert!(
            matches!(ran.status, ReceiptStatus::Completed { .. }),
            "{action} ran without asking: the page action was still a question ({ran:?})"
        );
        assert!(
            questions(&approver_actions, &approver_caller)
                .await
                .is_empty(),
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
            seen.iter().all(|handed| handed.approved),
            "the host answered for the person, as their answer does"
        );
    }

    // Submitting is not operating a page: it asks, whoever holds the key.
    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&key, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Accepted,
        "the submit waits for the person"
    );
    let question = asked(&approver_actions, &approver_caller).await;
    assert_eq!(question.facts.words, "Submit the application");
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        4,
        "a submit that waits for the person has run nothing"
    );
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    assert!(
        matches!(
            followed(&remote, &caller, id).await,
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
    let seen = window_actions(&fixture.server.server, &material, Asks::Acting).await;
    let linked = member_link(&fixture.server.server, &key).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    for action in ["fill-page", "select-page", "click-page"] {
        let id = InvocationId::new();
        let receipt = ActionProvider::invoke(&remote, &caller, page(&key, id, action))
            .await
            .expect("the host answers")
            .expect("the window holds it for the person");
        assert_eq!(
            receipt.status,
            ReceiptStatus::Accepted,
            "{action} waits for the person"
        );
        let question = asked(&approver_actions, &approver_caller).await;
        assert_eq!(
            question.name, action,
            "{action} is what they are asked about"
        );
        answer(
            &approver_actions,
            &approver_caller,
            &question,
            tilo_actions::remote::asked::Offer::Approve,
        )
        .await;
        assert!(
            matches!(
                followed(&remote, &caller, id).await,
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
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&key, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    assert_eq!(
        asked(&approver_actions, &approver_caller).await.facts.words,
        "Submit the application"
    );
}

/// The person allowing a submit runs it once, as the agent asked it, and the
/// page's receipt comes back to that same invocation; nothing but their
/// answer ever runs it.
#[tokio::test]
async fn a_page_action_the_person_allows_runs_once_and_the_page_answers_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Submit).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let question = asked(&approver_actions, &approver_caller).await;
    // While the person is asked, a caller following it is told so.
    let waiting = observed(&remote, &caller, id)
        .await
        .expect("the caller's invocation");
    assert!(
        matches!(
            &waiting.update,
            tilo_actions::InvocationUpdate::Progress { phase, message: Some(message), .. }
                if phase == tilo_actions::remote::asked::ASKED_PHASE
                    && message.contains('/')
        ),
        "a follower hears it waits for the person, in its record: {waiting:?}"
    );
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;

    let ended = followed(&remote, &caller, id).await;
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
    assert!(!seen[0].approved, "the first had no answer to carry");
    assert_eq!(
        seen[1].from,
        fixture.agent.device_pubkey(),
        "the run with the person's answer is the agent's, not the server's"
    );
    assert!(seen[1].approved, "the second carried their answer");
}

/// The person declining a page action ends it refused: it never runs, and
/// nothing is counted.
#[tokio::test]
async fn the_persons_denial_ends_a_page_action_refused_and_counts_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Submit).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let question = asked(&approver_actions, &approver_caller).await;
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Decline,
    )
    .await;
    let ended = followed(&remote, &caller, id).await;
    assert_eq!(
        ended,
        tilo_actions::InvocationUpdate::Cancelled {
            invocation: id,
            message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
        },
        "their decline ends it, and the page stays as it was"
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "the window saw it once");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// A question nobody answers waits in its record: the page stays as it was, the
/// invocation is still the person's to answer, and nothing is counted.
#[tokio::test]
async fn a_question_nobody_answers_waits_in_its_record() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Submit).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let waiting = observed(&remote, &caller, id)
        .await
        .expect("the caller's invocation");
    assert!(
        matches!(
            &waiting.update,
            tilo_actions::InvocationUpdate::Progress { phase, .. }
                if phase == tilo_actions::remote::asked::ASKED_PHASE
        ),
        "nobody answered it: it still waits for its person: {waiting:?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "the window saw it once");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// The key's own word is no answer for its person: the host refuses a page
/// action that carries one, and it reaches no window.
#[tokio::test]
async fn a_confirmation_the_caller_brings_runs_no_page_action() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(true);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Acting).await;
    let linked = member_link(&fixture.server.server, &key).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);

    let forged = ActionProvider::invoke(
        &remote,
        &caller,
        page_with(
            &key,
            InvocationId::new(),
            "click-page",
            Some(Confirmation::Confirmed),
        ),
    )
    .await
    .expect("the host answers");
    assert_eq!(
        forged.expect_err("a caller's own word is no answer here"),
        ActionCallError::NotPermitted
    );
    assert!(seen.lock().unwrap().is_empty(), "it reached no window");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// The frame the placing host hands its window, with its word for the
/// person, runs only over that host's link and for the workspace it placed
/// the window in: the same frame from another of the workspace's devices,
/// from another host, or for another workspace, is refused, and the window
/// runs nothing.
#[tokio::test]
async fn only_the_placing_hosts_word_answers_for_the_person_at_its_window() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(true);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let (seen, window) = window_served(&fixture.server.server, &material, Asks::Acting).await;
    let window_device = material.device.device_public_key;
    let confirmed = || {
        RemoteActionMessage::Invoke(page_with(
            &key,
            InvocationId::new(),
            "click-page",
            Some(Confirmation::Confirmed),
        ))
    };

    let other_host = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    for forwarder in [fixture.agent.sk.clone(), other_host] {
        let forwarding = ActionService::new(
            forwarder,
            Arc::new(WindowActions {
                seen: Arc::new(Mutex::new(Vec::new())),
                asks: Asks::Never,
                placement: tilo_actions::remote::browser::Placement {
                    host: [0; 32],
                    workspace: String::new(),
                },
            }),
        );
        let (forwarder_end, window_end) = duplex(Some(window_device), None);
        window.serve_link(window_end);
        forwarding.reach_device_at(window_device, forwarder_end);
        let answer = forwarding
            .remote_held(window_device, WS_NAME)
            .expect("a link reaches the window")
            .ask_message(confirmed())
            .await;
        assert!(
            matches!(
                receipt_of(answer.and_then(Result::ok)).status,
                ReceiptStatus::Rejected {
                    error: ActionCallError::NotPermitted
                }
            ),
            "another device's word is no answer at the window"
        );
    }

    let host = fixture
        .server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service();
    let elsewhere = host
        .remote_held(window_device, &OTHER_WS.to_string())
        .expect("the host's link reaches the window")
        .ask_message(confirmed())
        .await;
    assert!(
        matches!(
            receipt_of(elsewhere.and_then(Result::ok)).status,
            ReceiptStatus::Rejected { .. }
        ),
        "the host's word for another workspace is no answer at this window"
    );
    assert!(seen.lock().unwrap().is_empty(), "the window ran nothing");

    let ran = host
        .remote_held(window_device, WS_NAME)
        .expect("the host's link reaches the window")
        .ask_message(confirmed())
        .await;
    assert!(
        matches!(
            receipt_of(ran.and_then(Result::ok)).status,
            ReceiptStatus::Completed { .. }
        ),
        "the placing host's word runs it"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].from, key.device_pubkey(), "it ran as the key's own");
    assert!(seen[0].approved, "as the person's answer");
}

/// The record a customer wrote before Browser opened: another app's.
const NOTES: &str = "so.tilo.notes";
const NOTES_RECORD: &str = "notes-1";

/// The channel the window's own record lives on, and the one a customer's
/// document lives on: what every protocol of the workspace tells a record by.
fn window_channel() -> [u8; 32] {
    tilo_server::environment::document_channel(APP, WINDOW_RECORD)
}

fn notes_channel() -> [u8; 32] {
    tilo_server::environment::document_channel(NOTES, NOTES_RECORD)
}

/// The channel each record frame a link brought belongs to.
fn changed(frames: &[Heard]) -> Vec<[u8; 32]> {
    frames
        .iter()
        .filter_map(|heard| {
            let neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            } = &heard.message
            else {
                return None;
            };
            Some(neighbour_wire::app_document_group(app_id, document_id))
        })
        .collect()
}

/// Every frame `linked` hears within `within`.
async fn heard(linked: &mut Linked, within: std::time::Duration) -> Vec<Heard> {
    let mut out = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(within, linked.next()).await {
        out.push(frame);
    }
    out
}

/// `linked`'s device writes `marker` into `record` of `app`, through the link.
fn publish_record(linked: &Linked, device: &TestDevice, app: &str, record: &str, marker: &str) {
    let mut document = AutoCommit::new();
    document.put(ROOT, "marker", marker).unwrap();
    let bytes = document.save();
    linked
        .publish(move |peer| {
            peer.publication_mut().publish_document(
                app,
                record,
                bytes,
                &device.sk,
                std::time::SystemTime::now(),
            )
        })
        .unwrap();
}

/// What the host made of `linked`'s next own write.
async fn next_answer(
    answers: &mut tokio::sync::broadcast::Receiver<tilo_peer_state::direct::Answered>,
) -> tilo_peer_state::direct::Answered {
    tilo_test_env::within("the host answers the write", answers.recv())
        .await
        .expect("the link stays open")
}

/// A device's copy of the workspace as the host holds it, without any of its
/// records: what a device links with when it holds none of the workspace yet.
async fn workspace_seed(fixture: &Fixture, device: &TestDevice) -> tilo_peer_state::StatePeer {
    let workspaces = fixture
        .server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let authority = workspaces.device_cert().clone();
    let state = workspaces
        .state(WS_NAME)
        .await
        .expect("the server holds the workspace");
    let mut roster = state.certificates();
    if !roster
        .iter()
        .any(|cert| cert.device_public_key == device.device_pubkey())
    {
        roster.push(device.cert.clone());
    }
    let now = tilo_clock::wall();
    let mut peer = tilo_peer_state::StatePeer::new(
        WS_NAME,
        authority.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .expect("the workspace opens");
    peer.select_exact_admissions()
        .expect("its admissions stand");
    peer.install_seed(
        tilo_peer_state::SeedState {
            workspace_changes: tilo_peer_state::seed_workspace(WS_NAME, &authority, &roster)
                .expect("the workspace seeds"),
            documents: Vec::new(),
        },
        &fixture.host_device.sk,
        now,
    )
    .expect("the seed lands");
    peer
}

impl Fixture {
    /// The environment's own link: the device the server certified, joining
    /// the one workspace it was placed in.
    async fn environment_session(&self) -> Linked {
        let material = self.environments.material();
        let environment = DeviceIdentity::of(&material);
        Linked::open(&self.server.server, &environment, WS)
            .await
            .expect("the workspace admits the device it placed")
    }
}

#[tokio::test]
async fn the_server_confines_an_environments_device_to_the_record_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    assert_eq!(
        tilo_server::environment::environment_of(&fixture.server.server.environments(), &device),
        Some(tilo_server::environment::Environment::Placed(
            tilo_server::environment::EnvironmentScope::for_resource(WS, APP, WINDOW_RECORD)
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
            tilo_server::environment::EnvironmentScope::for_resource(WS, APP, WINDOW_RECORD)
        )),
        "a placed environment is written down before its device is listed, so a \
         restart confines it rather than leaving it a member"
    );
}

#[tokio::test]
async fn an_environment_hears_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let mut environment = fixture.environment_session().await;
    let writer = member_link(&fixture.server.server, &fixture.agent).await;
    publish_record(
        &writer,
        &fixture.agent,
        NOTES,
        NOTES_RECORD,
        "a customer's document",
    );
    publish_record(
        &writer,
        &fixture.agent,
        APP,
        WINDOW_RECORD,
        "the window's own state",
    );
    let heard = heard(&mut environment, std::time::Duration::from_millis(500)).await;
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "a page that escapes Chromium reads no other record of the workspace"
    );
    assert!(
        environment.read(|peer| peer.document(NOTES, NOTES_RECORD).is_none()),
        "no other record of the workspace is its to read"
    );
}

/// What the workspace's store keeps that `device` wrote, as each write's
/// channel and encoded message. The workspace keeps its own governance beside
/// it, which is not counted.
async fn written_by(server: &tilo_server::Server, device: [u8; 32]) -> Vec<([u8; 32], Vec<u8>)> {
    server
        .workspace_documents()
        .retained(WS)
        .await
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
    let environment_device = DeviceIdentity::of(&fixture.environments.material());
    let device = environment_device.device_pubkey();
    // The link's scope is the one record it placed: a write of another record
    // is not admitted, and the session stays, as for any frame its device may
    // not send.
    publish_record(
        &environment,
        &environment_device,
        NOTES,
        NOTES_RECORD,
        "overwritten by a page",
    );
    // Its own record is its to write, on the same link.
    publish_record(
        &environment,
        &environment_device,
        APP,
        WINDOW_RECORD,
        "the page clicked",
    );
    // The server keeps a write as it takes it in, and says nothing back to its writer: the
    // store is read until the write is there, however long a loaded server takes.
    let retained = tilo_test_env::within("the window's write is kept", async {
        loop {
            let kept = written_by(&fixture.server.server, device).await;
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
        .sync
        .open_record(notes_channel())
        .await
        .expect("the link answers an open it keeps nothing of");
    assert!(
        environment.read(|peer| peer.document(NOTES, NOTES_RECORD).is_none()),
        "another record does not open for it"
    );
    environment
        .sync
        .open_record(window_channel())
        .await
        .expect("its own record opens");
}

#[tokio::test]
async fn an_environments_replay_is_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let writer = member_link(&fixture.server.server, &fixture.agent).await;
    publish_record(
        &writer,
        &fixture.agent,
        NOTES,
        NOTES_RECORD,
        "a customer's document",
    );
    publish_record(
        &writer,
        &fixture.agent,
        APP,
        WINDOW_RECORD,
        "the window's own state",
    );
    // It joins after both records were written: what its link is sent — the
    // replay before its copy, then what is written behind it — is its own
    // record.
    let material = fixture.environments.material();
    let environment_device = DeviceIdentity::of(&material);
    let seed = workspace_seed(&fixture, &environment_device).await;
    let mut environment =
        Linked::open_seeded(&fixture.server.server, &environment_device, WS, seed)
            .await
            .expect("the environment's link opens");
    let heard = heard(&mut environment, std::time::Duration::from_millis(700)).await;
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "the replay of another record stays out"
    );
    assert!(
        environment.read(|peer| peer.document(NOTES, NOTES_RECORD).is_none()),
        "no other record of the workspace reaches it"
    );
}

#[tokio::test]
async fn an_environment_answers_a_page_action_and_never_calls_one() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Never).await;
    let environment_device = DeviceIdentity::of(&material);
    let environment = member_link(&fixture.server.server, &environment_device).await;
    let remote = asking(&environment, WS_NAME).await;
    let caller = environment.caller(fixture.user.user_pk);
    // The window's device calls the workspace's entry itself: not its to do.
    let refused = ActionProvider::invoke(
        &remote,
        &caller,
        as_asked(
            &environment_device,
            InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: NOTES.into(),
                    address: ActionAddress::new("notes", "delete"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        ),
    )
    .await
    .expect("the host answers")
    .expect_err("a window's device calls no entry of the workspace");
    assert!(
        matches!(refused, ActionCallError::Unavailable { .. }),
        "a window's device calls no entry of the workspace: {refused:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stopped_environment_reaches_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    // The link that holds the window is here, and stays up over the stop.
    let mut environment = fixture.environment_session().await;
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
    // Its link here is cut: the protocols it carried refuse it.
    let closed = tokio::time::timeout(tilo_test_env::hang_bound(), environment.closed()).await;
    assert!(
        closed.is_ok(),
        "a stopped environment's device has no link left"
    );
    assert!(
        closed.unwrap().is_err(),
        "the host's refusal ends it: the device is no device of the workspace"
    );
    // It dials again: refused, not open.
    let environment = DeviceIdentity::of(&material);
    let refused = Linked::open(&fixture.server.server, &environment, WS).await;
    assert!(
        refused.is_err(),
        "a stopped environment's device is no device of the workspace"
    );
}

#[tokio::test]
async fn an_environment_stops_when_the_session_that_held_it_ends() {
    let fixture = fixture_with(Arc::new(Counting::default()), Duration::from_millis(20)).await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    // The session that holds the window here, and ends: the server hears its
    // last connection go and stops its environment.
    let (endpoint, live) = live_session(&fixture.server.server, &material).await;
    endpoint.close().await;
    drop(live);
    // One signal, and no protocol admits it after.
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

/// The device holding the workspace's window, linked: what the session in
/// the VM is, reporting the departures its page holds to the server. Its
/// actions are asked over its link, as every device asks its host's.
async fn window_device(fixture: &Fixture, material: &Material) -> (Linked, RemoteDevice) {
    let device = DeviceIdentity::of(material);
    let linked = member_link(&fixture.server.server, &device).await;
    let remote = asking(&linked, WS_NAME).await;
    (linked, remote)
}

/// A departure the window holds, reported as the session in the VM reports
/// one: the call that would release it, named by the id the window published it
/// with, and the departure itself in its context — the page it leaves from,
/// what it sends and where — as the window states it.
fn reported_departure(asker: &TestDevice, invocation: InvocationId, id: u32) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
            on_behalf_of: None,
            invocation,
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
            signed: None,
        },
    )
}

/// A departure the page started on its own waits on nobody's invocation, so
/// the session holding the window calls it: the host keeps it as its own asked
/// record, the person is asked once, and their allow releases that one request.
#[tokio::test]
async fn a_departure_nothing_asked_for_is_put_to_the_person_and_their_allow_releases_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Departing).await;
    let (window, remote) = window_device(&fixture, &material).await;
    let asker = DeviceIdentity::of(&material);
    let caller = window.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, reported_departure(&asker, id, 2))
        .await
        .expect("the host answers")
        .expect("the departure waits for the person");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Accepted,
        "the departure waits for the person rather than refused"
    );
    let question = asked(&approver_actions, &approver_caller).await;
    assert_eq!(
        question.facts.on.as_deref(),
        Some("https://orders.example"),
        "the person is told the page it leaves from"
    );
    assert_eq!(question.facts.fact("kind"), Some("submit"));
    assert_eq!(question.facts.fact("method"), Some("POST"));
    assert_eq!(
        question.facts.fact("destination"),
        Some("https://orders.example")
    );
    assert_eq!(
        question.facts.fact("picture"),
        Some("tilo.content:5f2b"),
        "and the picture of the filled page, by its content address"
    );

    // The same call asked again under its id is kept as the same record: one departure is
    // one question.
    let again = ActionProvider::invoke(&remote, &caller, reported_departure(&asker, id, 2))
        .await
        .expect("the host answers")
        .expect("the departure still waits for the person");
    assert_eq!(again.status, ReceiptStatus::Accepted);
    assert_eq!(
        questions(&approver_actions, &approver_caller).await.len(),
        1,
        "one departure is one question"
    );

    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    // Their approve hands the same call to the window again, carrying it, and the window runs
    // it once.
    tilo_test_env::within("the window runs the departure the person released", async {
        loop {
            if seen.lock().unwrap().iter().any(|handed| handed.approved) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        matches!(
            followed(&remote, &caller, id).await,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the release the person allowed ran"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "a departure the person releases is no page action of their allowance"
    );
}

/// A departure the person denies stays kept: the page stays as it was filled,
/// nothing leaves, and the window never runs it.
#[tokio::test]
async fn a_departure_the_person_does_not_allow_is_answered_kept() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Departing).await;
    let (window, remote) = window_device(&fixture, &material).await;
    let asker = DeviceIdentity::of(&material);
    let caller = window.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, reported_departure(&asker, id, 3))
        .await
        .expect("the host answers")
        .expect("the departure waits for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let question = asked(&approver_actions, &approver_caller).await;
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Decline,
    )
    .await;
    assert_eq!(
        followed(&remote, &caller, id).await,
        tilo_actions::InvocationUpdate::Cancelled {
            invocation: id,
            message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
        },
        "kept: the session drops the request it reported and nothing leaves"
    );
    assert!(
        !seen.lock().unwrap().iter().any(|handed| handed.approved),
        "the window never ran the departure the person declined"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0);
}

/// Only the environment's own device releases the requests its own window
/// holds: another device of the workspace is refused, and nobody is asked.
#[tokio::test]
async fn only_the_device_of_the_window_releases_what_it_holds() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let refused = ActionProvider::invoke(
        &remote,
        &caller,
        reported_departure(&fixture.agent, InvocationId::new(), 4),
    )
    .await
    .expect("the host answers")
    .expect_err("the host refuses it");
    assert_eq!(
        refused,
        ActionCallError::NotPermitted,
        "another device does not answer for the window's page"
    );
    assert!(
        questions(&approver_actions, &approver_caller)
            .await
            .is_empty(),
        "nobody is asked about a hold another device reports"
    );
}

/// What an environment's device asks of the host about itself
/// ([`tilo_actions::remote::environment_device`]): `action` with `argument`.
fn asks_about_itself(
    asker: &TestDevice,
    invocation: InvocationId,
    action: &str,
    argument: InvokeArgument,
) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
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
        },
    )
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
    let material = fixture.environments.material();
    let environment_device = DeviceIdentity::of(&material);
    let environment = member_link(&fixture.server.server, &environment_device).await;
    let remote = asking(&environment, WS_NAME).await;
    let caller = environment.caller(fixture.user.user_pk);
    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(
        &remote,
        &caller,
        asks_about_itself(
            &environment_device,
            id,
            tilo_actions::remote::environment_device::MEDIA_KEYS,
            InvokeArgument::None,
        ),
    )
    .await
    .expect("the host answers")
    .expect("the host answers its environment");
    // This server keeps the workspace here, so it answers with its keys sealed
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

    let agent = member_link(&fixture.server.server, &fixture.agent).await;
    let agent_remote = asking(&agent, WS_NAME).await;
    let agent_caller = agent.caller(fixture.user.user_pk);
    let id = InvocationId::new();
    let refused = ActionProvider::invoke(
        &agent_remote,
        &agent_caller,
        asks_about_itself(
            &fixture.agent,
            id,
            tilo_actions::remote::environment_device::MEDIA_KEYS,
            InvokeArgument::None,
        ),
    )
    .await
    .expect("the host answers")
    .expect_err("a member that is no environment is lent nothing");
    assert_eq!(
        refused,
        ActionCallError::NotPermitted,
        "a member that is no environment of the workspace is lent nothing"
    );
}

#[tokio::test]
async fn an_environment_shares_no_window_it_did_not_mint() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let environment_device = DeviceIdentity::of(&material);
    let environment = member_link(&fixture.server.server, &environment_device).await;
    let remote = asking(&environment, WS_NAME).await;
    let caller = environment.caller(fixture.user.user_pk);
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
        let refused = ActionProvider::invoke(
            &remote,
            &caller,
            asks_about_itself(
                &environment_device,
                id,
                action,
                window_named(NOTES, foreign),
            ),
        )
        .await
        .expect("the host answers")
        .expect_err("another device's window is refused");
        assert_eq!(
            refused,
            ActionCallError::NotPermitted,
            "{action} of another device's window is refused"
        );
    }
}

#[tokio::test]
async fn an_environment_writes_nothing_of_the_workspaces_own_document_and_stays() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let mut environment = fixture.environment_session().await;
    let environment_device = DeviceIdentity::of(&fixture.environments.material());
    let device = environment_device.device_pubkey();
    environment
        .send(neighbour_wire::NeighbourMsg::SyncResp {
            workspace: WS_NAME.into(),
            changes: vec![b"a shell layout and a grant of its own making".to_vec()],
        })
        .unwrap();
    // What the host makes of that frame is between it and the frame: let it
    // reach this device before the write that follows is asked about.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut answers = environment.answers();
    // Neither refused nor kept: the link stays, and writes its own record.
    publish_record(
        &environment,
        &environment_device,
        APP,
        WINDOW_RECORD,
        "the page clicked",
    );
    assert_eq!(
        next_answer(&mut answers).await.kept,
        tilo_peer_state::direct::Kept::Durable,
        "a session that writes its shell as any session does is not cut for it"
    );
    let retained = tilo_test_env::within("the window's write is kept", async {
        loop {
            let kept = written_by(&fixture.server.server, device).await;
            if !kept.is_empty() {
                return kept;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
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
    let fixture = fixture_with(Arc::new(Counting::default()), reconnect).await;
    // The workspace lists the window's device, as the server lists it.
    let material = fixture.environments.material();

    // The join: the session's device enters the workspace, and that
    // connection ends at once, as the invitation connection in the VM does.
    let joined = member_link(&fixture.server.server, &DeviceIdentity::of(&material)).await;
    // The lasting connection the session keeps: the live session it publishes
    // its window on.
    let (endpoint, live) = live_session(&fixture.server.server, &material).await;
    // The join connection ends the way the session's own does: its endpoint
    // closes, having told the server it is going.
    drop(joined);

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
/// [`Connected`](tilo_live::admit::Connected) are kept alive.
async fn live_session(
    server: &tilo_server::Server,
    material: &Material,
) -> (iroh::endpoint::Endpoint, tilo_live::admit::Connected) {
    let endpoint = crate::common::device_endpoint(&DeviceIdentity::of(material)).await;
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

/// A workspace's page submission the person approves is carried out once: the host keeps the
/// call as its asked record, the person's approve hands the same call to the window carrying
/// their answer, and the window runs it once — counted once, and handed the answer once.
#[tokio::test]
async fn an_approved_submission_is_performed_once() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Submit).await;
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, submit(&fixture.agent, id))
        .await
        .expect("the host answers")
        .expect("the window holds it for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let question = asked(&approver_actions, &approver_caller).await;
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    let ended = followed(&remote, &caller, id).await;
    assert!(
        matches!(ended, tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the host that kept it hands it over: {ended:?}"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1, "it ran once");
    let handed = seen.lock().unwrap().iter().filter(|h| h.approved).count();
    assert_eq!(handed, 1, "the window was handed the person's answer once");
    assert!(
        questions(&approver_actions, &approver_caller)
            .await
            .is_empty(),
        "the person is not asked again"
    );
}

/// What the host answers `device` asking to enter the workspace, asked as a member device asks
/// it: on a link of its own to the host's actions, naming no workspace, the call signed by the
/// device it proves.
async fn entering(
    server: &Server,
    device: &TestDevice,
) -> Result<tilo_actions::InvocationUpdate, ActionCallError> {
    use tilo_server::workspaces::{ENTER_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE};
    let endpoint = crate::common::device_endpoint(device).await;
    let remote = Arc::new(
        ActionService::new(
            device.sk.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(&endpoint, &server.node_addr(), ""),
    );
    let arguments = serde_json::Map::from_iter([
        ("workspace".to_owned(), WS_NAME.into()),
        ("expires_in_seconds".to_owned(), 600.into()),
    ]);
    let mut request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), ENTER_WORKSPACE),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            "",
            &request,
            &device.cert,
            &device.sk,
            tilo_clock::unix_now_ns(),
        )
        .expect("the device signs its call"),
    ));
    let answer = remote
        .invoke_until_settled(request, Duration::from_secs(60))
        .await
        .expect("the host answers the entry");
    endpoint.close().await;
    answer
}

/// Whether the workspace lists `device`, as the host and a device of the person's own read it:
/// a certificate in its roster, or a device in its access.
async fn listed_by_workspace(fixture: &Fixture, device: [u8; 32]) -> Vec<&'static str> {
    let now = tilo_clock::wall();
    let state = fixture
        .server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .state(WS_NAME)
        .await
        .expect("the server holds the workspace");
    let laptop = member_link(&fixture.server.server, &fixture.laptop).await;
    let (laptop_roster, laptop_access) = laptop.read(|peer| {
        (
            peer.certificates()
                .iter()
                .any(|cert| cert.device_public_key == device),
            peer.governance()
                .governed_access(now)
                .expect("the laptop reads the workspace's access")
                .member_devices
                .contains(&device),
        )
    });
    [
        (
            state
                .certificates()
                .iter()
                .any(|cert| cert.device_public_key == device),
            "the host's roster",
        ),
        (
            state
                .governance()
                .governed_access(now)
                .expect("the host reads the workspace's access")
                .member_devices
                .contains(&device),
            "the host's access",
        ),
        (laptop_roster, "the laptop's roster"),
        (laptop_access, "the laptop's access"),
    ]
    .into_iter()
    .filter_map(|(listed, by)| listed.then_some(by))
    .collect()
}

#[tokio::test]
async fn an_environment_asking_to_enter_the_workspace_is_refused() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let environment = DeviceIdentity::of(&material);
    let answer = entering(&fixture.server.server, &environment).await;
    let refusal = tilo_server::environment::JoinsNothing.to_string();
    match &answer {
        Err(ActionCallError::Refused { reason }) => assert_eq!(
            reason, &refusal,
            "the host says why it lets the environment into nothing"
        ),
        other => panic!(
            "the host answered the environment's entry with {other:?}; the workspace lists it in {:?}",
            listed_by_workspace(&fixture, material.device.device_public_key).await
        ),
    }
    assert_eq!(
        listed_by_workspace(&fixture, material.device.device_public_key).await,
        Vec::<&str>::new(),
        "no member row, on the host or on another device of the person's"
    );
}

#[tokio::test]
async fn an_environment_redeeming_an_invitation_is_refused() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let environment = DeviceIdentity::of(&material);
    // An invitation for the person, which a device of theirs redeems: the agent's own entry
    // answers one.
    let Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        entering(&fixture.server.server, &fixture.agent).await
    else {
        panic!("a device of the person's enters the workspace");
    };
    let entered: tilo_server::workspaces::EnteredWorkspace =
        serde_json::from_value(output.value.unwrap_or_default())
            .expect("the entry answers an invitation");
    let signed = entered
        .invitation
        .decode()
        .expect("the invitation decodes")
        .signed_invitation;
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &signed,
        Some(fixture.user.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .expect("the invitation is the person's");
    let endpoint = crate::common::device_endpoint(&environment).await;
    let redeemed = tilo_server::workspace_join::WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            signed,
            environment.cert.clone(),
            &environment.sk,
            [9; 32],
        )
        .await;
    endpoint.close().await;
    let refusal = tilo_server::environment::JoinsNothing.to_string();
    match &redeemed {
        Err(error) => assert!(
            error.to_string().contains(&refusal),
            "the host says why it lets the environment into nothing: {error}"
        ),
        Ok(_) => panic!(
            "the host admitted the environment by an invitation; the workspace lists it in {:?}",
            listed_by_workspace(&fixture, material.device.device_public_key).await
        ),
    }
    assert_eq!(
        listed_by_workspace(&fixture, material.device.device_public_key).await,
        Vec::<&str>::new(),
        "no member row, on the host or on another device of the person's"
    );
}
