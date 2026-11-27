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

use tilo_action_services::server_account::{NextStepKind, Reason, Refusal};
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
use tilo_runtime::device::keep::hosted_browser::{
    BrowserMeter, BrowserSessions, HostedBrowsers, Material, Started, Window, no_payment_method,
};
use tilo_runtime::device::keep::server::Server;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert,
};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT};
use collab_types::CommitNamed as _;

use crate::keep::common::{
    Linked, TestDevice, TestServer, TestUser, spawn_server, spawn_server_serving_apps,
};
use tilo_app_host::agents::windows::{PageWindow, Release, Releasing, TurnFill};
use tilo_runtime::device::keep::hosted_browser::{APP, FILL_PAGE_SECRET, LET_LEAVE};
use tilo_workspace_service::environment::vm_placed_records;
use tilo_workspace_service::workspaces::person_workspace::devices;

const WS: WorkspaceId = WorkspaceId::from_u128(0x1111_1111_1111_4111_8111_1111_1111_1111);
const OTHER_WS: WorkspaceId = WorkspaceId::from_u128(0x2222_2222_2222_4222_8222_2222_2222_2222);
const WS_NAME: &str = "11111111-1111-4111-8111-111111111111";
const WINDOW: &str = "42";
/// What the host publishes for the workspace itself.
const HOST_PROVIDER: &str = "workspace:11111111-1111-4111-8111-111111111111";
/// The revision the window's own listing answers at.
const WINDOW_REVISION: &str = "window";
/// The record the window was opened on: the workspace's one Browser record, which its VM is
/// confined to.
const WINDOW_RECORD: &str = tilo_workspace_service::environment::VM_RECORD;

/// Places environments in-process: each "environment" is the workspace's VM daemon, which
/// makes its own key and logs in with the ticket the server served for it, as the daemon in the
/// VM does; the test's client then joins the workspace as that device.
#[derive(Default)]
struct InProcessEnvironments {
    starts: Mutex<Vec<Material>>,
    stops: Mutex<Vec<String>>,
    /// While set, a start waits until it is taken: a session whose VM is
    /// still waking.
    waking: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// The server's tickets, where a VM's daemon logs in.
    tickets: std::sync::OnceLock<tilo_peer_state::new_device::Tickets>,
    /// The server's register of the workspaces' VM devices: where the certificate a VM's
    /// daemon was given is read back.
    registry: std::sync::OnceLock<tilo_workspace_service::environment::EnvironmentRegistry>,
    /// The certificates handed to the workspaces' VMs as they were certified again.
    renewals: Mutex<Vec<(String, DeviceCert)>>,
}

/// A workspace's VM as it joined: what it was started with, and the device its daemon is.
#[derive(Clone)]
struct Joined {
    workspace: String,
    workspace_id: WorkspaceId,
    record: String,
    ticket: tilo_device_identity::login::Ticket,
    /// The certificate the person's root gave the daemon's key.
    device: DeviceCert,
    key: ed25519_dalek::SigningKey,
}

/// The key the daemon in `workspace_id`'s VM makes for itself: one per workspace.
fn vm_key(workspace_id: WorkspaceId) -> ed25519_dalek::SigningKey {
    let mut seed = [0x5e; 32];
    seed[..16].copy_from_slice(workspace_id.as_uuid().as_bytes());
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

impl InProcessEnvironments {
    fn starts(&self) -> usize {
        self.starts.lock().unwrap().len()
    }

    fn stops(&self) -> usize {
        self.stops.lock().unwrap().len()
    }

    /// The first VM placed, as it joined.
    fn material(&self) -> Joined {
        let material = self.starts.lock().unwrap()[0].clone();
        let key = vm_key(material.workspace_id);
        let device = self
            .registry
            .get()
            .expect("the fixture names the server's register")
            .certificate(&key.verifying_key().to_bytes())
            .expect("the VM's daemon was certified and confined");
        Joined {
            workspace: material.workspace,
            workspace_id: material.workspace_id,
            record: material.record,
            ticket: material.ticket,
            device,
            key,
        }
    }

    /// Log the daemon of the VM `material` starts in with its ticket, as `tilo login` does.
    fn log_in(&self, material: &Material) -> [u8; 32] {
        use tilo_device_identity::login::{CERTIFY, CertifyInput, PROVIDER, RESOURCE};
        let key = vm_key(material.workspace_id);
        let device = key.verifying_key().to_bytes();
        let input = CertifyInput {
            secret: material.ticket.secret_hex(),
            device_public_key: hex::encode(device),
            agent: "tilo".into(),
            label: Some("Workspace computer".into()),
            tilo_os: true,
        };
        let serde_json::Value::Object(argument) = serde_json::to_value(&input).unwrap() else {
            panic!("the input is an object");
        };
        let request = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(RESOURCE, CERTIFY),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        // The certify call is taken at once; the person's root answers it in its own time.
        drop(ActionProvider::invoke(
            &*self
                .tickets
                .get()
                .expect("the fixture names the server's tickets")
                .uncertified(),
            &Caller::authenticated([0; 32], device),
            request,
        ));
        device
    }
}

impl BrowserSessions for InProcessEnvironments {
    fn start(&self, material: &Material) -> Result<Started, String> {
        let waking = self.waking.lock().unwrap().take();
        if let Some(woken) = waking {
            let _ = woken.recv();
        }
        self.starts.lock().unwrap().push(material.clone());
        self.log_in(material);
        Ok(Started {
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, workspace: &str) {
        self.stops.lock().unwrap().push(workspace.to_owned());
    }

    fn renewed(&self, workspace: &str, cert: &DeviceCert) -> Result<(), String> {
        self.renewals
            .lock()
            .unwrap()
            .push((workspace.to_owned(), cert.clone()));
        Ok(())
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

/// The window's page, as its page actions name it: the Browser's page record the open answered.
fn the_page() -> PageWindow {
    PageWindow::new(WINDOW, WINDOW_RECORD)
}

/// The page action `action` of the window, asked by `asker`.
fn page(asker: &TestDevice, invocation: InvocationId, action: &str) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation,
            target: the_page().target(action),
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
            asks: false,
            on_behalf_of: None,
            invocation,
            target: the_page().target(action),
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
    let mut request = page(asker, invocation, "submit_page");
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
    use tilo_action_services::workspace_invocations::{self, Invocation, InvocationStatus};
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
        .unwrap_or_else(|error| panic!("the items read as invocations: {error}: {readings}"))
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
        asks: false,
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
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
            "read_page",
            "find_page_element",
            "capture_page",
            "wait_page_settled",
        ];
        match self {
            Self::Never => false,
            Self::Submit => action == "submit_page",
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
            .map(|(name, value)| tilo_action_services::browser::departure_facet(name, value))
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
    placement: tilo_action_services::browser::Placement,
    /// The fills it was handed, each waiting for its machine to be lent the item.
    fills: Arc<HeldFills>,
}

/// The page actions of the Browser's `Page` (`apps/tl/browser/app.tl`) a window here answers,
/// each with the parameters the Browser declares for it.
const PAGE_ACTIONS: &[(&str, &[&str])] = &[
    ("read_page", &["from", "frame"]),
    ("read_page_table", &["element", "from", "frame"]),
    ("find_page_text", &["text", "frame", "frames"]),
    (
        "find_page_element",
        &["role", "name", "nth", "frame", "frames"],
    ),
    (
        "wait_page_settled",
        &["quiet_ms", "timeout_ms", "text", "gone", "frame"],
    ),
    ("capture_page", &["element", "max_width", "frame"]),
    ("navigate", &["url"]),
    ("click_page", &["element", "frame"]),
    ("hover_page", &["element", "x", "y"]),
    (
        "drag_page",
        &["from", "from_x", "from_y", "to", "to_x", "to_y", "steps"],
    ),
    ("fill_page", &["element", "text", "frame"]),
    ("fill_page_secret", &["element", "item"]),
    ("select_page", &["element", "values", "frame"]),
    ("press_page", &["key", "element", "frame"]),
    (
        "scroll_page",
        &["element", "to", "by", "into_view", "frame"],
    ),
    ("pointer_page", &["steps", "button"]),
    ("submit_page", &["element", "cross_origin", "frame"]),
    ("let_leave", &["id", "origin", "method", "leaving"]),
];

/// What a window's page answers once the fill it was handed ends.
type FillAnswer = tilo_actions::remote::Completer<Result<InvocationReceipt, ActionCallError>>;

/// The fills a window was handed and has not ended: its page fills a field once its machine
/// was lent the item's text, which a test asks for while the fill waits.
#[derive(Default)]
struct HeldFills {
    held: Mutex<Vec<(InvokeRequest, FillAnswer)>>,
    told: tokio::sync::Notify,
    /// How each fill that ended ended, as the window answers whoever observes it.
    ended: Mutex<std::collections::HashMap<InvocationId, InvocationState>>,
}

impl HeldFills {
    /// The fill `invocation`, whose page answers through `answer`, ended as `update`.
    fn end(
        &self,
        answer: FillAnswer,
        invocation: InvocationId,
        update: tilo_actions::InvocationUpdate,
    ) {
        let state = InvocationState::new(invocation, update);
        answer.complete(Ok(state.receipt()));
        self.ended.lock().unwrap().insert(invocation, state);
    }

    fn hold(&self, request: InvokeRequest, answer: FillAnswer) {
        self.held.lock().unwrap().push((request, answer));
        self.told.notify_one();
    }

    /// The next fill the window was handed, once it was handed one.
    async fn next(&self) -> (InvokeRequest, FillAnswer) {
        loop {
            let next = {
                let mut held = self.held.lock().unwrap();
                (!held.is_empty()).then(|| held.remove(0))
            };
            if let Some(next) = next {
                return next;
            }
            self.told.notified().await;
        }
    }
}

/// The window's page as its listing declares it: each of [`PAGE_ACTIONS`] on the page record.
fn declared_page() -> Vec<tilo_actions::remote::CatalogueRow> {
    let page = the_page();
    PAGE_ACTIONS
        .iter()
        .map(|(action, params)| {
            tilo_actions::remote::CatalogueRow::for_action(
                &page.provider,
                APP,
                tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: ActionAddress::new(&page.page, *action),
                    resource_kind: "tl.so.tilo.browser.Page".to_owned(),
                    resource_label: "Page".to_owned(),
                    action_label: (*action).to_owned(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ServerPreferred,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: tilo_actions::ActionContract::default(),
                    input: tilo_actions::InputSpec::None,
                    params: params
                        .iter()
                        .map(|name| {
                            tilo_actions::ActionParam::optional(
                                name,
                                tilo_actions::ParamType::Text,
                                "",
                            )
                        })
                        .collect(),
                    view: None,
                },
            )
        })
        .collect()
}

impl ActionProvider for WindowActions {
    /// What the window offers, read from the window: the Browser's page actions on its page.
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage {
            revision: WINDOW_REVISION.into(),
            rows: declared_page(),
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // The window answers what its page declares, on its page, and nothing else.
        let action = request.target.address.action.as_str();
        let declared = PAGE_ACTIONS.iter().any(|(named, _)| *named == action);
        if !the_page().names(&request.target) || !declared {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        // Only the host that placed this window, over its own link, for the
        // workspace it placed it in, answers for the person; the dispatch that
        // admitted the same call again on the person's approve of its asked
        // record hands the window their own answer.
        let approved = match tilo_action_services::browser::host_answered(
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
        if action == FILL_PAGE_SECRET {
            let (answer, answered) = tilo_actions::remote::pending();
            self.fills.hold(request, answer);
            return answered;
        }
        let status = if self.asks.asks(&request.target.address.action) && !approved {
            // What the person is shown: the departure the call itself states where it states
            // one, the window's own otherwise, and the page it acts on when it states neither.
            let stated = tilo_action_services::browser::departure_facts(request.context.as_ref());
            let stated = if stated.is_empty() {
                tilo_action_services::browser::stated_facts(&self.asks.departure())
            } else {
                stated
            };
            let mut facts = tilo_action_services::browser::departure_asked(stated);
            if facts.on.is_none() {
                facts.on = Some(the_page().page);
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

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.fills.ended.lock().unwrap().get(id).cloned()
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}

/// The window's device, serving the page actions the host hands it over the
/// link it holds open to the host, and the page actions it was handed.
async fn window_actions(server: &Server, material: &Joined, asks: Asks) -> Arc<Mutex<Vec<Handed>>> {
    window_served(server, material, asks).await.0
}

/// [`window_actions`], the host's actions asked from the window's own session
/// link: the link it holds open to the host, which it serves itself, so a page
/// action the host hands it reaches its window.
async fn window_session(
    server: &Server,
    material: &Joined,
    asks: Asks,
) -> (Arc<Mutex<Vec<Handed>>>, RemoteDevice) {
    let (seen, _, remote) = window_served(server, material, asks).await;
    (seen, remote)
}

/// [`window_actions`], and the window's own action service, which serves the
/// link its session dialled to the host: the host asks it back over that one.
async fn window_served(
    server: &Server,
    material: &Joined,
    asks: Asks,
) -> (Arc<Mutex<Vec<Handed>>>, ActionService, RemoteDevice) {
    window_served_with(server, material, asks, Arc::default()).await
}

/// [`window_session`], and the fills its page was handed, each waiting for the item.
async fn window_filling(server: &Server, material: &Joined) -> (Arc<HeldFills>, RemoteDevice) {
    let fills = Arc::new(HeldFills::default());
    let (_, _, remote) =
        window_served_with(server, material, Asks::Never, Arc::clone(&fills)).await;
    (fills, remote)
}

/// [`window_served`], keeping the fills it is handed in `fills`.
async fn window_served_with(
    server: &Server,
    material: &Joined,
    asks: Asks,
    fills: Arc<HeldFills>,
) -> (Arc<Mutex<Vec<Handed>>>, ActionService, RemoteDevice) {
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
            placement: tilo_action_services::browser::Placement {
                host,
                workspace: material.workspace.clone(),
            },
            fills,
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
    // What the device answers under, as a device's service does: the certificate the host
    // certified it with, which the host reads to take it in as placed.
    actions.answer_under(material.device.clone());
    // The session's own link: the window's side names no device, as a page's
    // session does, and the host's names the window, so the host hands a page
    // action back over this one and the window asks over it.
    let (host_end, window_end) = duplex(Some(device.device_pubkey()), None);
    actions.serve_link(Arc::clone(&window_end));
    let remote = actions.remote_over(window_end, host, WS_NAME);
    server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .serve_link(host_end);
    (seen, actions, remote)
}

/// The test device a VM joined as: what the daemon in the environment is.
struct DeviceIdentity;

impl DeviceIdentity {
    fn of(material: &Joined) -> TestDevice {
        TestDevice {
            sk: material.key.clone(),
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
    server: Box<TestServer>,
    browsers: Arc<HostedBrowsers>,
    environments: Arc<InProcessEnvironments>,
    meter: Arc<Counting>,
    /// The device this host places workspace VMs as, which signs their certificates: the host's
    /// own, one of the person's devices, unless the fixture runs as an operator's.
    placing: TestDevice,
}

async fn fixture(meter: Arc<Counting>) -> Fixture {
    fixture_with(meter, tilo_runtime::device::keep::hosted_browser::RECONNECT).await
}

/// A fixture that gives a disconnected environment device `reconnect` to come
/// back before its environment stops.
async fn fixture_with(meter: Arc<Counting>, reconnect: Duration) -> Fixture {
    let fixture = unplaced(meter, reconnect, false).await;
    fixture.place(WS).await.expect("the environment was placed");
    fixture
}

/// A host whose Browser places workspace VMs as a device of an operator's, none of the person's:
/// it places one only under a grant the person gave it. Nothing is placed yet.
async fn operated() -> Fixture {
    unplaced(
        Arc::new(Counting::default()),
        tilo_runtime::device::keep::hosted_browser::RECONNECT,
        true,
    )
    .await
}

/// The fixture before it places anything: its host places as the person's own device, or, where
/// `operated`, as a device of an operator's.
fn unplaced(
    meter: Arc<Counting>,
    reconnect: Duration,
    operated: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture> + Send>> {
    Box::pin(async move {
        let user = TestUser::fresh("Ada");
        let host = user.enrol("home", DeviceCapabilities::member());
        let laptop = user.enrol("laptop", DeviceCapabilities::primary());
        let agent = user.enrol("agent", DeviceCapabilities::member());
        let home = host.device_pubkey();
        let host_device = host.clone();
        let placing = if operated {
            TestUser::fresh("Operator").enrol("hosting", DeviceCapabilities::member())
        } else {
            host.clone()
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
        // The host starts as a hosting server does: its Browser, composed with what else it
        // serves, is the app action server its workspaces answer their devices' app frames with.
        let server = Box::new(
            Box::pin(spawn_server_serving_apps(
                &user,
                host,
                &[],
                &[],
                tilo_runtime::device::keep::hosted_browser::app_serving(
                    Some(Arc::clone(&browsers)),
                    None,
                )
                .expect("a host with a Browser serves it"),
            ))
            .await,
        );
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
        // The person's own workspace, where their devices record is, is on their server too.
        if !tilo_workspace_service::workspaces::person_workspace::holds_person_workspace(workspaces)
            .await
        {
            tilo_workspace_service::workspaces::person_workspace::found(workspaces)
                .await
                .expect("the person's own workspace is founded on their server");
        }
        // The host answers the workspace's open of Browser and the page actions of the window it
        // places through its one dispatch, as production registers it.
        browsers.provide_in(&workspaces.providers());
        let _ = environments
            .tickets
            .set(workspaces.action_service().tickets());
        let _ = environments.registry.set(server.server.environments());
        browsers.serve_from(tilo_runtime::device::keep::hosted_browser::Authority {
            user_public_key: user.user_pk,
            host: placing.cert.clone(),
            host_key: placing.sk.clone(),
            server: server.server.node_addr(),
            workspaces: server
                .server
                .workspaces()
                .map(tilo_workspace_service::workspaces::Workspaces::downgrade),
            environments: server.server.environments(),
            runtime: tokio::runtime::Handle::current(),
            hand: server.server.hand(),
            actions: workspaces.action_service(),
        });
        // The server keeps one store for every call that waits for a person's yes here; the
        // Browser window's own questions wait in it, and the person's answer comes back to the
        // window that asked.
        let asked = workspaces
            .asked_store()
            .expect("the server keeps the calls that wait");
        browsers.keep_asked_in(&asked);
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
            placing,
        }
    })
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
    /// Place `workspace`'s VM, opened on its Browser record, as an open of Browser does: it
    /// waits for the VM's daemon to be certified, which the runtime carries meanwhile.
    async fn place(&self, workspace: WorkspaceId) -> Result<Window, String> {
        let browsers = Arc::clone(&self.browsers);
        tokio::task::spawn_blocking(move || {
            browsers.place(&workspace.to_string(), workspace, WINDOW_RECORD)
        })
        .await
        .unwrap()
    }

    /// Certify `workspace`'s VM device again, as this host does before its certificate ends.
    async fn renew(&self, workspace: WorkspaceId) -> Result<DeviceCert, String> {
        let browsers = Arc::clone(&self.browsers);
        tokio::task::spawn_blocking(move || browsers.renew_placed(workspace))
            .await
            .unwrap()
    }

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
        material.ticket.issuer.endpoint,
        *fixture.server.server.node_addr().id.as_bytes(),
        "the VM joins with a ticket the host serves, and is handed no key"
    );
    assert_eq!(
        material.device.capabilities,
        DeviceCapabilities::placed(
            vm_placed_records(WS).expect("the workspace's two VM records are well formed")
        ),
        "the device is certified for its VM's records and nothing else, no member device"
    );
    assert_eq!(
        material.device.user_public_key, fixture.user.user_pk,
        "of the person the workspace belongs to"
    );
    assert_eq!(
        material.device.issuer.as_deref(),
        Some(&fixture.host_device.cert),
        "certified by this host, one of the person's devices, with its own key"
    );
    assert!(
        material.device.expires_at_ns.is_some_and(|ends| ends
            <= material.device.issued_at_ns + tilo_user_identity::MAX_PLACED_DEVICE_LIFETIME_NS),
        "it lasts two hours at most, and the host certifies it again meanwhile"
    );
    assert_eq!(
        material.device.device_public_key, window.device,
        "the window is the device the host certified"
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
        asks: false,
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
        ActionProvider::invoke(&remote, &caller, page(&fixture.agent, id, "read_page")),
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
    // What it lists is the Browser's page, by the record the open answered: the page actions an
    // agent calls, under the Browser's own names.
    let fill = page
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.address.action == "fill_page_secret")
        .expect("the window lists the Browser's fill of a secret");
    assert_eq!(
        fill.address.resource,
        format!("tl:so.tilo.browser:{WINDOW_RECORD}")
    );
    let params: Vec<&str> = fill.params.iter().map(|param| &*param.name).collect();
    assert_eq!(params, ["element", "item"]);
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
    // Placing waits for the VM's daemon to be certified, which this runtime carries meanwhile.
    tokio::task::spawn_blocking(move || placing.join())
        .await
        .unwrap()
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
                "fill_page",
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

    for action in ["read_page", "find_page_element", "fill_page", "select_page"] {
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
    assert_eq!(question.facts.on.as_deref(), Some(the_page().page.as_str()));
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
    let receipt = ActionProvider::invoke(&remote, &caller, page(&fixture.agent, id, "click_page"))
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

    for action in ["navigate", "fill_page", "select_page", "click_page"] {
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

    for action in ["fill_page", "select_page", "click_page"] {
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
            "click_page",
            Some(Confirmation::Operated),
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
    let (seen, window, _remote) =
        window_served(&fixture.server.server, &material, Asks::Acting).await;
    let window_device = material.device.device_public_key;
    let confirmed = || {
        RemoteActionMessage::Invoke(page_with(
            &key,
            InvocationId::new(),
            "click_page",
            Some(Confirmation::Operated),
        ))
    };

    let other_host = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    for forwarder in [fixture.agent.sk.clone(), other_host] {
        let forwarding = ActionService::new(
            forwarder,
            Arc::new(WindowActions {
                seen: Arc::new(Mutex::new(Vec::new())),
                asks: Asks::Never,
                placement: tilo_action_services::browser::Placement {
                    host: [0; 32],
                    workspace: String::new(),
                },
                fills: Arc::default(),
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
    tilo_workspace_service::environment::document_channel(APP, WINDOW_RECORD)
}

fn notes_channel() -> [u8; 32] {
    tilo_workspace_service::environment::document_channel(NOTES, NOTES_RECORD)
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

/// `linked`'s device writes `marker` into `record` of `app`, through the link, as the window's
/// edit its person makes: a placed device acts for no one of its own.
fn publish_record(linked: &Linked, device: &TestDevice, app: &str, record: &str, marker: &str) {
    linked
        .write(&device.sk, (app, record), |document| {
            document.put(ROOT, "marker", marker).unwrap();
            document.commit_edit("window", "marker", &device.cert.user_public_key);
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

/// A device's copy of the workspace as the host keeps it, without any of its
/// records: what a device links with when it has none of the workspace yet. A
/// device the roster does not list proves itself by the certificate it was placed
/// with ([`tilo_peer_state::direct::DirectSync::placed`]), never by a row of its own.
async fn workspace_seed(fixture: &Fixture) -> tilo_peer_state::StatePeer {
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
    let roster = state.certificates();
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

/// `device`'s first write to `record` of `app`, over the record as `linked` reads it and signed by
/// hand as a page that escaped its browser signs one: taking it in is decided by admission alone.
fn forged_in(
    linked: &Linked,
    device: &TestDevice,
    app: &str,
    record: &str,
    marker: &str,
) -> neighbour_wire::NeighbourMsg {
    let workspace = linked.read(|peer| peer.workspace_id().to_owned());
    let held = linked.opened((app, record));
    let mut document = held
        .as_deref()
        .map_or_else(AutoCommit::new, |bytes| AutoCommit::load(bytes).unwrap());
    let heads = document.get_heads();
    document.put(ROOT, "marker", marker).unwrap();
    document.commit_edit("window", "marker", &device.cert.user_public_key);
    let mutation = match held {
        Some(_) => collab_types::DocumentMutation::Incremental(document.save_after(&heads)),
        None => collab_types::DocumentMutation::Snapshot(document.save()),
    };
    crate::keep::common::signed_by_hand(
        &device.sk,
        (&workspace, app, record),
        &mutation,
        1,
        None,
        Vec::new(),
    )
}

/// The hello a device opens each link with: its certificate.
fn hello_of(device: &TestDevice) -> neighbour_wire::NeighbourMsg {
    neighbour_wire::NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(device.cert.clone()),
        revocations: Vec::new(),
    }
}

/// The marker `record` of `app` has in `linked`'s copy, if that copy has the record.
fn marker_in(linked: &Linked, app: &str, record: &str) -> Option<String> {
    let bytes = linked.opened((app, record))?;
    let document = AutoCommit::load(&bytes).ok()?;
    match automerge::ReadDoc::get(&document, ROOT, "marker").ok()?? {
        (automerge::Value::Scalar(scalar), _) => match scalar.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
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
        tilo_workspace_service::environment::environment_of(
            &fixture.server.server.environments(),
            &device
        ),
        Some(tilo_workspace_service::environment::Environment::Placed(
            tilo_workspace_service::environment::EnvironmentScope::for_resource(
                WS,
                APP,
                WINDOW_RECORD
            )
        )),
        "the placement names the one record its device reaches"
    );
}

#[tokio::test]
async fn a_server_that_restarts_still_confines_the_environment_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    // What a server that starts again reads: its own register, kept with its
    // workspace service's state, not the workspace's listing.
    let restarted = tilo_workspace_service::environment::EnvironmentRegistry::at(
        fixture
            .server
            .state_root
            .join("service")
            .join("environments.json"),
    );
    assert_eq!(
        tilo_workspace_service::environment::environment_of(&restarted, &device),
        Some(tilo_workspace_service::environment::Environment::Placed(
            tilo_workspace_service::environment::EnvironmentScope::for_resource(
                WS,
                APP,
                WINDOW_RECORD
            )
        )),
        "a placed environment is written down before its device is listed, so a \
         restart confines it rather than leaving it a member"
    );
}

#[tokio::test]
async fn an_environment_hears_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let writer = member_link(&fixture.server.server, &fixture.agent).await;
    // Written before the environment joins: what its link is sent of the
    // workspace's history.
    publish_record(
        &writer,
        &fixture.agent,
        NOTES,
        NOTES_RECORD,
        "before it joined",
    );
    publish_record(
        &writer,
        &fixture.agent,
        APP,
        WINDOW_RECORD,
        "the window's first state",
    );
    let material = fixture.environments.material();
    let environment_device = DeviceIdentity::of(&material);
    let seed = workspace_seed(&fixture).await;
    let mut environment =
        Linked::open_seeded(&fixture.server.server, &environment_device, WS, seed)
            .await
            .expect("the host that placed the environment admits its link");
    // Written while it is linked: what its link is sent live.
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
    let heard = heard(&mut environment, std::time::Duration::from_millis(700)).await;
    let records = changed(&heard);
    assert!(
        records.contains(&window_channel()),
        "its own record reaches it: {records:?}"
    );
    let notes: Vec<_> = heard
        .iter()
        .filter(|heard| changed(std::slice::from_ref(heard)) == [notes_channel()])
        .map(|heard| (heard.retained, hex::encode(heard.author)))
        .collect();
    assert!(
        notes.is_empty(),
        "a page that escapes Chromium reads no other record of the workspace, from its history \
         or live; it was sent (retained, author): {notes:?}"
    );
    assert!(
        records.iter().all(|record| *record == window_channel()),
        "only its own record reaches it: {records:?}"
    );
    let keys: Vec<_> = heard
        .iter()
        .filter(|heard| {
            matches!(
                heard.message,
                neighbour_wire::NeighbourMsg::MlsGovernance { .. }
                    | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
                    | neighbour_wire::NeighbourMsg::MlsGovernanceWanted { .. }
            )
        })
        .map(|heard| format!("{:?}", heard.message))
        .collect();
    assert!(
        keys.is_empty(),
        "no key of the workspace's, nor a share of one, goes to its link: {keys:?}"
    );
    assert!(
        environment.opened((NOTES, NOTES_RECORD)).is_none(),
        "no other record of the workspace is its to read"
    );
    named_by_no_roster(&fixture, &environment_device).await;
}

/// Neither the person's laptop nor the host knows the environment's device: it is in no
/// certificate list and no member list, so none of them admits it or hands it a key.
async fn named_by_no_roster(fixture: &Fixture, environment_device: &TestDevice) {
    let laptop = member_link(&fixture.server.server, &fixture.laptop).await;
    let device = environment_device.device_pubkey();
    let now = tilo_clock::wall();
    for (whose, (certified, member)) in [
        (
            "the person's laptop",
            laptop.read(|peer| {
                (
                    peer.certificates()
                        .iter()
                        .any(|cert| cert.device_public_key == device),
                    peer.governance()
                        .member_list_access(now)
                        .expect("the member list reads")
                        .member_devices
                        .contains(&device),
                )
            }),
        ),
        ("the host", {
            let state = fixture
                .server
                .server
                .workspaces()
                .expect("the server keeps workspaces")
                .state(WS_NAME)
                .await
                .expect("the server keeps the workspace");
            (
                state
                    .certificates()
                    .iter()
                    .any(|cert| cert.device_public_key == device),
                state
                    .governance()
                    .member_list_access(now)
                    .expect("the member list reads")
                    .member_devices
                    .contains(&device),
            )
        }),
    ] {
        assert!(
            !certified,
            "{whose} knows no certificate of the environment's device"
        );
        assert!(
            !member,
            "{whose} lists the environment's device as no member's"
        );
    }
}

/// The write `device` made, as the host's store keeps it, once it is kept: a server says
/// nothing back to a write's writer, so the store is read until the write is there.
async fn kept_write(
    server: &tilo_runtime::device::keep::server::Server,
    device: [u8; 32],
) -> Vec<([u8; 32], Vec<u8>)> {
    tilo_test_env::within("the window's write is kept", async {
        loop {
            let kept = written_by(server, device).await;
            if !kept.is_empty() {
                return kept;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
}

/// What the workspace's store keeps that `device` wrote, as each write's
/// channel and encoded message. The workspace keeps its own governance beside
/// it, which is not counted.
async fn written_by(
    server: &tilo_runtime::device::keep::server::Server,
    device: [u8; 32],
) -> Vec<([u8; 32], Vec<u8>)> {
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
    let environment_device = DeviceIdentity::of(&fixture.environments.material());
    let device = environment_device.device_pubkey();
    // A page that escapes Chromium signs writes its copy would refuse. They are signed in a copy
    // of the person's laptop made before the environment's link said who it is, so that copy
    // checks nothing of them, and the laptop lacks nothing either builds on.
    let laptop = member_link(&fixture.server.server, &fixture.laptop).await;
    let forged_notes = forged_in(
        &laptop,
        &environment_device,
        NOTES,
        NOTES_RECORD,
        "overwritten by a page",
    );
    let own = forged_in(
        &laptop,
        &environment_device,
        APP,
        WINDOW_RECORD,
        "the window, as the laptop hears it",
    );
    let environment = fixture.environment_session().await;
    // Its own copy makes no write of another record: its certificate places it for its window's
    // record alone, there as on every device.
    assert!(
        environment
            .write(&environment_device.sk, (NOTES, NOTES_RECORD), |document| {
                document
                    .put(ROOT, "marker", "overwritten by a page")
                    .unwrap();
            })
            .is_err(),
        "the environment's own copy refuses to write another record"
    );
    // The page sends its forged write anyway: the link's scope is the one record it was placed
    // for, so the host does not admit it, and the session stays, as for any frame its device may
    // not send.
    environment
        .send(forged_notes.clone())
        .expect("the link carries what the page sends");
    // A member device that hears of the environment refuses the same write by its certificate:
    // the person's laptop, handed the device's hello and the forged write directly.
    let now = tilo_clock::wall();
    let origin = tilo_peer_state::TransportOrigin::ServerDelta(device);
    laptop
        .sync
        .replica()
        .write(|peer| peer.receive_from(origin, hello_of(&environment_device), now))
        .expect("the laptop takes in the placed device's certificate");
    let refused = laptop
        .sync
        .replica()
        .write(|peer| peer.receive_from(origin, forged_notes, now));
    assert!(
        refused.is_err(),
        "the laptop refuses the environment's write of a record it was not placed for"
    );
    assert_ne!(
        marker_in(&laptop, NOTES, NOTES_RECORD).as_deref(),
        Some("overwritten by a page"),
        "the laptop's copy of the other record is untouched"
    );
    let admitted = laptop
        .sync
        .replica()
        .write(|peer| peer.receive_from(origin, own, now))
        .expect("the laptop admits the environment's write of its own record");
    assert_eq!(
        marker_in(&laptop, APP, WINDOW_RECORD).as_deref(),
        Some("the window, as the laptop hears it"),
        "the laptop's copy shows the environment's write: {admitted:?}"
    );
    assert!(
        laptop.read(|peer| {
            peer.placed_device(&device, now).is_some_and(|placed| {
                placed.is_ok_and(|placed| placed.person == fixture.user.user_pk)
            })
        }),
        "the laptop names the person for what the environment writes"
    );
    // Its own record is its to write, on the same link.
    publish_record(
        &environment,
        &environment_device,
        APP,
        WINDOW_RECORD,
        "the page clicked",
    );
    let retained = kept_write(&fixture.server.server, device).await;
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
    // A record is opened on a link that caught up with the host's history.
    let mut caught_up = environment.sync.caught_up();
    tilo_test_env::within(
        "the environment's link catches up",
        caught_up.wait_for(|in_hand| *in_hand),
    )
    .await
    .expect("the link stays up");
    environment
        .sync
        .open_record(notes_channel())
        .await
        .expect("the link answers an open it keeps nothing of");
    assert!(
        environment.opened((NOTES, NOTES_RECORD)).is_none(),
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
    let seed = workspace_seed(&fixture).await;
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
        environment.opened((NOTES, NOTES_RECORD)).is_none(),
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
                asks: false,
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
    assert_eq!(
        refused,
        ActionCallError::NotPermitted,
        "a window's device calls no entry of the workspace: the host refuses it before any app \
         is asked"
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
        tilo_workspace_service::environment::environment_of(
            &fixture.server.server.environments(),
            &device
        ),
        Some(tilo_workspace_service::environment::Environment::Stopped {
            until_ns: material.device.expires_at_ns.unwrap_or(u64::MAX)
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
            == Some(tilo_workspace_service::environment::Environment::Stopped {
                until_ns: material.device.expires_at_ns.unwrap_or(u64::MAX),
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

/// A request the window's page holds, let leave: the Browser's `let_leave` of the page, naming
/// the request by the id the page holds it under, and the departure itself in its context — the
/// page it leaves from, what it sends and where — as the window states it. The session in the
/// VM raises it over its own link for what its page sent by itself, which proves the device.
fn reported_departure(invocation: InvocationId, id: u32) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation,
        target: the_page().target(LET_LEAVE),
        argument: InvokeArgument::Json(serde_json::Map::from_iter([
            ("id".to_owned(), id.into()),
            ("origin".to_owned(), "https://orders.example".into()),
            ("method".to_owned(), "POST".into()),
            ("leaving".to_owned(), "the order form".into()),
        ])),
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
            .map(|(name, value)| tilo_action_services::browser::departure_facet(name, value))
            .collect(),
            ..tilo_actions::ActionContext::default()
        }),
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A departure the page started on its own waits on nobody's invocation, so
/// the session holding the window calls it: the host keeps it as its own asked
/// record, the person is asked once, and their allow releases that one request.
#[tokio::test]
async fn a_departure_nothing_asked_for_is_put_to_the_person_and_their_allow_releases_it() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let (seen, remote) = window_session(&fixture.server.server, &material, Asks::Departing).await;
    let asker = DeviceIdentity::of(&material);
    // The window asks as the device the host placed it as.
    let caller = Caller::authenticated(fixture.user.user_pk, asker.device_pubkey());
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, reported_departure(id, 2))
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
    let again = ActionProvider::invoke(&remote, &caller, reported_departure(id, 2))
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
        1,
        "nothing counted what the page sent by itself, so its release counts once"
    );
}

/// A departure the person denies stays kept: the page stays as it was filled,
/// nothing leaves, and the window never runs it.
#[tokio::test]
async fn a_departure_the_person_does_not_allow_is_answered_kept() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let (seen, remote) = window_session(&fixture.server.server, &material, Asks::Departing).await;
    let asker = DeviceIdentity::of(&material);
    // The window asks as the device the host placed it as.
    let caller = Caller::authenticated(fixture.user.user_pk, asker.device_pubkey());
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    let id = InvocationId::new();
    let receipt = ActionProvider::invoke(&remote, &caller, reported_departure(id, 3))
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

/// Any device of the person's own lets a request their window holds leave, their agent's among
/// them, and each is put to the person first: a key that may operate web pages included. A
/// device of another member of the workspace is refused, and nobody is asked.
#[tokio::test]
async fn each_of_the_persons_devices_lets_a_held_request_leave_and_another_members_is_refused() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let key = fixture.key(true);
    fixture.list_environment_and(&key).await;
    let material = fixture.environments.material();
    let seen = window_actions(&fixture.server.server, &material, Asks::Never).await;
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    // Another member of the workspace, on a device of their own.
    let bea = TestUser::fresh("Bea");
    let beas = bea.enrol("laptop", DeviceCapabilities::primary());
    let workspaces = fixture
        .server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let issued = crate::keep::common::invitation(
        workspaces,
        &fixture.host_device,
        WS_NAME,
        tilo_peer_state::join::Role::Editor,
        &beas,
    )
    .await;
    crate::keep::common::redeem(&beas, &issued)
        .await
        .expect("Bea joins the workspace");
    let theirs = member_link(&fixture.server.server, &beas).await;
    let refused = ActionProvider::invoke(
        &asking(&theirs, WS_NAME).await,
        &theirs.caller(bea.user_pk),
        as_asked(&beas, reported_departure(InvocationId::new(), 4)),
    )
    .await
    .expect("the host answers")
    .expect_err("the host refuses it");
    assert_eq!(
        refused,
        ActionCallError::NotPermitted,
        "another member does not let the person's request leave"
    );
    assert!(
        questions(&approver_actions, &approver_caller)
            .await
            .is_empty(),
        "nobody is asked about a release another member asked for"
    );

    // The person's agent, on a key that operates web pages, and their own laptop: each asks the
    // person, and the window runs nothing meanwhile.
    for (at, asker) in [(5, &key), (6, &fixture.laptop)] {
        let linked = member_link(&fixture.server.server, asker).await;
        let receipt = ActionProvider::invoke(
            &asking(&linked, WS_NAME).await,
            &linked.caller(fixture.user.user_pk),
            as_asked(asker, reported_departure(InvocationId::new(), at)),
        )
        .await
        .expect("the host answers")
        .expect("the release waits for the person");
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
    }
    assert_eq!(
        questions(&approver_actions, &approver_caller).await.len(),
        2,
        "the person is asked about each release"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing leaves before the person answers"
    );
}

/// A request the page holds because an agent's click sent it was counted with the click: the
/// person letting it leave counts nothing more. One held-then-released request moves the
/// allowance by exactly one.
#[tokio::test]
async fn a_held_request_let_leave_moves_the_allowance_by_exactly_one() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let (seen, window) = window_session(&fixture.server.server, &material, Asks::Never).await;
    let window_caller = Caller::authenticated(
        fixture.user.user_pk,
        DeviceIdentity::of(&material).device_pubkey(),
    );
    let linked = member_link(&fixture.server.server, &fixture.agent).await;
    let remote = asking(&linked, WS_NAME).await;
    let caller = linked.caller(fixture.user.user_pk);
    let approver = fixture.approving_device().await;
    let approver_actions = asking(&approver, WS_NAME).await;
    let approver_caller = approver.caller(fixture.user.user_pk);

    // The agent's click sends a form, which the page holds: the click ran, and is counted.
    let click = InvocationId::new();
    let receipt =
        ActionProvider::invoke(&remote, &caller, page(&fixture.agent, click, "click_page"))
            .await
            .expect("the host answers")
            .expect("the click runs");
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1);

    // The window raises the release of what the click sent, naming the click.
    let release = InvocationId::new();
    let mut leave = reported_departure(release, 7);
    leave.on_behalf_of = Some(click);
    let receipt = ActionProvider::invoke(&window, &window_caller, leave)
        .await
        .expect("the host answers")
        .expect("the release waits for the person");
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    let question = asked(&approver_actions, &approver_caller).await;
    answer(
        &approver_actions,
        &approver_caller,
        &question,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    assert!(
        matches!(
            followed(&window, &window_caller, release).await,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the release the person allowed ran"
    );
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|handed| handed.approved)
            .count(),
        1,
        "the window let it leave once"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        1,
        "the click's count covers what it sent: never twice"
    );
}

/// What an environment's device asks of the host about itself
/// ([`tilo_action_services::environment_device`]): `action` with `argument`.
fn asks_about_itself(
    asker: &TestDevice,
    invocation: InvocationId,
    action: &str,
    argument: InvokeArgument,
) -> InvokeRequest {
    as_asked(
        asker,
        InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation,
            target: tilo_action_services::environment_device::target(action),
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
        serde_json::to_value(tilo_action_services::environment_device::WindowShare {
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
            tilo_action_services::environment_device::MEDIA_KEYS,
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
            tilo_action_services::environment_device::MEDIA_KEYS,
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
        tilo_action_services::environment_device::SHARE_WINDOW,
        tilo_action_services::environment_device::END_WINDOW_SHARE,
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
    let environment = fixture.environment_session().await;
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
    server: &tilo_runtime::device::keep::server::Server,
    material: &Joined,
) -> (iroh::endpoint::Endpoint, tilo_live::admit::Connected) {
    let endpoint = crate::keep::common::device_endpoint(&DeviceIdentity::of(material)).await;
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
    // A workspace this host serves: an open placed from its run names the workspace by its id.
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
    let observe: Arc<tilo_runtime::device::keep::hosted_browser::ObserveInvocation> = {
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
    let answer = tilo_runtime::device::keep::hosted_browser::placed(
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
    use tilo_workspace_service::workspaces::{
        ENTER_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    };
    let endpoint = crate::keep::common::device_endpoint(device).await;
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
        asks: false,
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
                .member_list_access(now)
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
                .member_list_access(now)
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
    let refusal = tilo_workspace_service::environment::JoinsNothing.to_string();
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
    let entered: tilo_workspace_service::workspaces::EnteredWorkspace =
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
    let endpoint = crate::keep::common::device_endpoint(&environment).await;
    let redeemed = tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            signed,
            environment.cert.clone(),
            &environment.sk,
            [9; 32],
        )
        .await;
    endpoint.close().await;
    let refusal = tilo_workspace_service::environment::JoinsNothing.to_string();
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

impl Fixture {
    /// The server's workspaces, where the person's devices record is.
    fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.server
            .server
            .workspaces()
            .expect("the server holds workspaces")
    }

    /// The grant the person gives this host to place `workspace`'s VM, kept among their devices
    /// as the one record every host of theirs reads it from.
    async fn grant_placing(&self, workspace: WorkspaceId) -> DeviceCert {
        let mut grant = tilo_user_identity::prepare_placing_grant(
            self.user.user_pk,
            self.placing.device_pubkey(),
            vm_placed_records(workspace).expect("the workspace's two VM records are well formed"),
            "Hosting",
            tilo_clock::unix_now_ns(),
            None,
        );
        sign_device_cert(&mut grant, &self.user.user_sk).expect("the person's root signs it");
        devices::keep(self.workspaces(), &grant)
            .await
            .expect("the person's devices record keeps the grant");
        grant
    }

    /// The person takes back the grant `grant`, as they revoke a device of theirs.
    async fn revoke(&self, grant: &DeviceCert) {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: self.user.user_pk,
            revoked_device_public_key: grant.device_public_key,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &self.user.user_sk)
            .expect("the person's root signs the revocation");
        devices::revoke(self.workspaces(), &self.user.user_pk, &entry)
            .await
            .expect("the person's devices record keeps the revocation");
    }
}

/// A host that is one of the person's devices certifies a workspace's VM with its own key at
/// once, for the VM's records alone, and places it in its register, which confines it there; the
/// person's devices record lists it nowhere, since a workspace's computer is none of theirs.
#[tokio::test]
async fn a_host_that_is_the_persons_device_places_the_vm_at_once_and_lists_it_among_none() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    let person = fixture.user.user_pk;
    assert_eq!(
        material.device.capabilities,
        DeviceCapabilities::placed(
            vm_placed_records(WS).expect("the workspace's two VM records are well formed")
        ),
        "its certificate reaches the VM's records of the workspace it was placed for alone"
    );
    let scope = tilo_workspace_service::environment::EnvironmentScope::of_workspace_device(WS);
    assert_eq!(
        tilo_workspace_service::environment::environment_of(
            &fixture.server.server.environments(),
            &device
        ),
        Some(tilo_workspace_service::environment::Environment::Placed(
            scope
        )),
        "the server confines it to its workspace's records"
    );
    let record = devices::of(fixture.workspaces(), &person).await;
    assert!(
        record
            .listed(&person)
            .iter()
            .chain(record.placing_grants(&person).iter())
            .all(|listed| listed.cert.device_public_key != device),
        "the person's devices record does not list the workspace's computer"
    );
}

/// A host that is none of the person's devices, and holds no grant of theirs, refuses to place
/// their workspace's VM in words that say what lets it, and starts nothing.
#[tokio::test]
async fn a_host_that_is_neither_the_persons_device_nor_granted_refuses_in_words() {
    let fixture = operated().await;
    let refused = fixture
        .place(WS)
        .await
        .expect_err("a host the person never allowed places nothing");
    assert_eq!(
        refused,
        tilo_runtime::device::keep::hosted_browser::NOT_ALLOWED_TO_PLACE
    );
    assert_eq!(fixture.environments.starts(), 0, "no VM was started");
    assert!(fixture.browsers.window(WS).is_none());
}

/// A host the person granted placement for a workspace's VM records places that VM under the
/// grant, and certifies it again before it ends; once the person revokes the grant, the host
/// neither places another nor certifies this one again.
#[tokio::test]
async fn a_granted_host_places_and_renews_until_the_grant_is_revoked() {
    let fixture = operated().await;
    let grant = fixture.grant_placing(WS).await;
    let window = fixture
        .place(WS)
        .await
        .expect("the grant lets the host place it");
    let material = fixture.environments.material();
    assert_eq!(
        material.device.issuer.as_deref(),
        Some(&grant),
        "the host certified it under the person's grant"
    );
    assert_eq!(
        material.device.capabilities,
        DeviceCapabilities::placed(
            vm_placed_records(WS).expect("the workspace's two VM records are well formed")
        )
    );
    assert_eq!(material.device.device_public_key, window.device);
    let renewed = fixture
        .renew(WS)
        .await
        .expect("the host certifies it again");
    assert_eq!(renewed.device_public_key, window.device, "the same device");
    assert!(renewed.issued_at_ns >= material.device.issued_at_ns);
    assert_eq!(
        fixture
            .server
            .server
            .environments()
            .certificate(&window.device),
        Some(renewed),
        "the register holds the renewed certificate"
    );

    fixture.revoke(&grant).await;
    assert_eq!(
        fixture
            .renew(WS)
            .await
            .expect_err("a revoked grant renews nothing"),
        tilo_runtime::device::keep::hosted_browser::NOT_ALLOWED_TO_PLACE
    );
    let other = fixture.grant_placing(OTHER_WS).await;
    fixture.revoke(&other).await;
    assert_eq!(
        fixture
            .place(OTHER_WS)
            .await
            .expect_err("a revoked grant places nothing"),
        tilo_runtime::device::keep::hosted_browser::NOT_ALLOWED_TO_PLACE
    );
    assert_eq!(fixture.environments.starts(), 1, "no second VM was started");
}

/// The host certifies a placed VM again before its certificate ends, while the placement is up,
/// and no more once it stopped.
#[tokio::test]
async fn the_vm_is_certified_again_before_it_ends_and_no_more_once_stopped() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let renewed = fixture
        .renew(WS)
        .await
        .expect("the host certifies it again");
    assert_eq!(renewed.device_public_key, material.device.device_public_key);
    assert_eq!(renewed.capabilities, material.device.capabilities);
    assert!(
        renewed.expires_at_ns >= material.device.expires_at_ns,
        "it lasts from now on"
    );
    assert_eq!(
        fixture
            .environments
            .renewals
            .lock()
            .unwrap()
            .iter()
            .map(|(_, cert)| cert.clone())
            .collect::<Vec<_>>(),
        [renewed],
        "the renewed certificate is handed to the VM"
    );
    fixture.browsers.stopped(WS);
    assert!(
        fixture.renew(WS).await.is_err(),
        "a stopped placement is certified no more"
    );
    assert_eq!(
        fixture.environments.renewals.lock().unwrap().len(),
        1,
        "nothing is handed to a stopped placement's VM"
    );
}

/// The login a turn fills, and the site it belongs to.
const LOGIN: &str = "vault.item:0123456789abcdef0123456789abcdef";
const LOGIN_SITE: &str = "https://login.example.test";

/// What the window's machine (`asker`, asking as `caller` over `window`) is answered when it asks
/// the host for the item of the fill `fill` at `origin`.
async fn item_use(
    window: &RemoteDevice,
    (asker, caller): (&TestDevice, &Caller),
    fill: InvocationId,
    (item, origin): (&str, &str),
) -> Result<InvocationReceipt, ActionCallError> {
    let serde_json::Value::Object(used) =
        serde_json::to_value(tilo_action_services::environment_device::ItemUse {
            invocation: fill,
            item: item.to_owned(),
            origin: origin.to_owned(),
        })
        .unwrap()
    else {
        unreachable!("an item's use is an object");
    };
    ActionProvider::invoke(
        window,
        caller,
        asks_about_itself(
            asker,
            InvocationId::new(),
            tilo_action_services::environment_device::USE_ITEM,
            InvokeArgument::Json(used),
        ),
    )
    .await
    .expect("the host answers")
}

/// What decides a turn's fill, as the vault does for the login it names: released for the
/// login's own site, refused for any other. Each origin it is asked about is kept in `asked`.
fn deciding(asked: &Arc<Mutex<Vec<String>>>) -> Releasing {
    let asked = Arc::clone(asked);
    Box::new(move |origin: String| {
        let decided: std::pin::Pin<Box<dyn std::future::Future<Output = Release> + Send>> =
            Box::pin(async move {
                asked.lock().unwrap().push(origin.clone());
                if origin == LOGIN_SITE {
                    Release::Text {
                        text: zeroize::Zeroizing::new("correct horse".to_owned()),
                        into: "password",
                    }
                } else {
                    Release::Refused("This login belongs to another site.".to_owned())
                }
            });
        decided
    })
}

/// A fill of `LOGIN` into the field `e7`, as a turn names it.
fn turn_fill(invocation: InvocationId) -> TurnFill {
    TurnFill {
        invocation,
        element: "e7".to_owned(),
        item: LOGIN.to_owned(),
    }
}

/// A turn's fill reaches the window as the Browser's own `fill_page_secret` of its page, with the
/// parameters the Browser declares, and the window's machine is lent the login only for that
/// fill: sealed to its key for the site it names, and once. Another member's agent is lent
/// nothing, neither of the turn's fill nor of a fill it hands the window itself; a site the
/// login does not belong to is refused with the vault's reason, and nothing is sealed for it.
#[tokio::test]
async fn a_turns_fill_reaches_the_window_as_fill_page_secret_and_lends_its_login_once() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let material = fixture.environments.material();
    let window_device = DeviceIdentity::of(&material);
    let (fills, window) = window_filling(&fixture.server.server, &material).await;
    let window_caller = Caller::authenticated(fixture.user.user_pk, window_device.device_pubkey());
    let asked = Arc::new(Mutex::new(Vec::new()));

    let turn = InvocationId::new();
    let browsers = Arc::clone(&fixture.browsers);
    let releasing = deciding(&asked);
    let filled =
        tokio::spawn(async move { browsers.fill_for_turn(WS, turn_fill(turn), releasing).await });
    let (handed, fill_ends) = fills.next().await;
    assert_eq!(handed.invocation, turn);
    assert_eq!(handed.target, the_page().target(FILL_PAGE_SECRET));
    let serde_json::Value::Object(named) = serde_json::json!({"element": "e7", "item": LOGIN})
    else {
        unreachable!("an object");
    };
    assert_eq!(
        handed.argument,
        InvokeArgument::Json(named.clone()),
        "the fill names what the Browser's fill_page_secret declares, and nothing else"
    );

    // Another member's agent, on a device of theirs, is lent nothing of the turn's fill.
    let bea = TestUser::fresh("Bea");
    let beas_agent = bea.enrol("agent", DeviceCapabilities::member());
    let workspaces = fixture
        .server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let issued = crate::keep::common::invitation(
        workspaces,
        &fixture.host_device,
        WS_NAME,
        tilo_peer_state::join::Role::Editor,
        &beas_agent,
    )
    .await;
    crate::keep::common::redeem(&beas_agent, &issued)
        .await
        .expect("Bea joins the workspace");
    let theirs = member_link(&fixture.server.server, &beas_agent).await;
    let their_remote = asking(&theirs, WS_NAME).await;
    let their_caller = theirs.caller(bea.user_pk);
    assert_eq!(
        item_use(
            &their_remote,
            (&beas_agent, &their_caller),
            turn,
            (LOGIN, LOGIN_SITE)
        )
        .await,
        Err(ActionCallError::NotPermitted),
        "only the window's own machine is lent an item"
    );

    // Nor is a fill of another login, nor one the host never handed, lent anything.
    for (fill, item) in [
        (turn, "vault.item:fedcba9876543210fedcba9876543210"),
        (InvocationId::new(), LOGIN),
    ] {
        let refused = item_use(
            &window,
            (&window_device, &window_caller),
            fill,
            (item, LOGIN_SITE),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { .. })),
            "{refused:?}"
        );
    }
    assert!(asked.lock().unwrap().is_empty(), "nothing was decided yet");

    // The window's machine names the site its engine committed: the login is sealed to its key
    // for this fill at that site, and opens for no other site.
    let lent = item_use(
        &window,
        (&window_device, &window_caller),
        turn,
        (LOGIN, LOGIN_SITE),
    )
    .await
    .expect("the host lends the window its fill's login");
    let ReceiptStatus::Completed { output } = lent.status else {
        panic!("the login is lent: {lent:?}");
    };
    let used: tilo_action_services::environment_device::ItemUsed =
        serde_json::from_value(output.value.unwrap()).unwrap();
    let tilo_action_services::environment_device::ItemUsed::Released { sealed, into } = used else {
        panic!("the login is released");
    };
    assert_eq!(into, "password");
    let sealed = hex::decode(sealed).unwrap();
    let bound = |origin| key_group::ReleasedFor {
        workspace: WS_NAME,
        invocation: *turn.0.as_bytes(),
        origin,
    };
    let opened = key_group::open_released_text(&window_device.sk, &bound(LOGIN_SITE), &sealed)
        .expect("the window's machine opens what was sealed to it");
    assert_eq!(opened.as_str(), "correct horse");
    assert!(
        key_group::open_released_text(&window_device.sk, &bound("https://elsewhere.test"), &sealed)
            .is_err(),
        "what was lent for one site opens for no other"
    );
    assert_eq!(*asked.lock().unwrap(), [LOGIN_SITE]);

    // A fill is lent its login once.
    let again = item_use(
        &window,
        (&window_device, &window_caller),
        turn,
        (LOGIN, LOGIN_SITE),
    )
    .await;
    assert!(
        matches!(&again, Err(ActionCallError::Refused { .. })),
        "{again:?}"
    );
    fills.end(
        fill_ends,
        turn,
        tilo_actions::InvocationUpdate::Succeeded {
            invocation: turn,
            output: tilo_actions::ActionOutput::default(),
        },
    );
    let ended = filled
        .await
        .unwrap()
        .expect("the turn hears how the fill ended");
    assert!(
        matches!(ended, tilo_actions::InvocationUpdate::Succeeded { .. }),
        "{ended:?}"
    );

    // A page at a site the login does not belong to is filled nothing: the vault's reason is
    // what the window's machine is told.
    let elsewhere = InvocationId::new();
    let browsers = Arc::clone(&fixture.browsers);
    let releasing = deciding(&asked);
    let filled = tokio::spawn(async move {
        browsers
            .fill_for_turn(WS, turn_fill(elsewhere), releasing)
            .await
    });
    let (_, fill_ends) = fills.next().await;
    assert_eq!(
        item_use(
            &window,
            (&window_device, &window_caller),
            elsewhere,
            (LOGIN, "https://phish.example.test"),
        )
        .await,
        Err(ActionCallError::Refused {
            reason: "This login belongs to another site.".to_owned()
        })
    );
    fills.end(
        fill_ends,
        elsewhere,
        tilo_actions::InvocationUpdate::Cancelled {
            invocation: elsewhere,
            message: None,
        },
    );
    drop(filled.await.unwrap());

    // Bea's agent asking the window itself for a fill is lent nothing either: the host lends
    // only for a fill it handed for a turn it carries out.
    let hers = InvocationId::new();
    let request = as_asked(
        &beas_agent,
        InvokeRequest {
            argument: InvokeArgument::Json(named),
            ..page(&beas_agent, hers, FILL_PAGE_SECRET)
        },
    );
    let asking_window =
        tokio::spawn(
            async move { ActionProvider::invoke(&their_remote, &their_caller, request).await },
        );
    tokio::select! {
        answered = asking_window => {
            // Refused before it reached the window: nothing was handed, so nothing is lent.
            let answered = answered.unwrap().expect("the host answers");
            assert!(
                !matches!(
                    &answered,
                    Ok(InvocationReceipt { status: ReceiptStatus::Completed { .. }, .. })
                ),
                "{answered:?}"
            );
        }
        (handed, her_fill) = fills.next() => {
            assert_eq!(handed.invocation, hers);
            let refused = item_use(
                &window,
                (&window_device, &window_caller),
                hers,
                (LOGIN, LOGIN_SITE),
            )
            .await;
            assert!(
                matches!(&refused, Err(ActionCallError::Refused { .. })),
                "{refused:?}"
            );
            drop(her_fill);
        }
    }
    assert_eq!(asked.lock().unwrap().len(), 2, "nothing decided Bea's fill");
}
