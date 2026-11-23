//! A person's device hosts a workspace with the same host a server runs, and a
//! key of theirs uses it with the same calls it uses on a server: it opens a
//! session with the device, lists their workspaces there, enters the
//! workspace (the host invites it on the session and it redeems there), and
//! runs a record action addressed to the host's own device, which the
//! device's own action broker answers in a run of the key's
//! ([`tilo_runtime::key_sessions`]).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until it ends"
)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_desktop_model::WorkspaceId;
use tilo_runtime::key_sessions::{KeySessions, SessionHost, Sessions};
use tilo_runtime::personal_devices::PersonalDevices;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_service::service::{ServiceConfig, ServiceIdentity, WorkspaceService};

/// The app the device runs, which a key lists and acts on.
const NOTES: &str = "so.tilo.notes";

fn certified(
    root: &SigningKey,
    n: u8,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A localhost endpoint with no relay whose identity is the device `key`.
async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

fn notebook(action: Option<&str>) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: "1".into(),
        app_id: NOTES.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: "notes.notebook".into(),
            kind: "notes.notebook".into(),
            label: "Notebook".into(),
            presentation: None,
        },
        action: action.map(|action| tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("notes.notebook", action),
            resource_kind: "notes.notebook".into(),
            resource_label: "Notebook".into(),
            action_label: "Pin".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The device's broker as a key's run reaches it: Notes runs there, and every
/// caller and invocation is kept.
#[derive(Clone, Default)]
struct Broker {
    /// The workspace its apps run in.
    workspace: String,
    callers: Arc<Mutex<Vec<Caller>>>,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
}

impl ActionProvider for Broker {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let page = match query.provider.as_deref() {
            None if query.needle.is_empty() => CataloguePage {
                revision: "1".into(),
                providers: vec![CatalogueProvider {
                    provider: "1".into(),
                    app_id: NOTES.into(),
                    workspace: self.workspace.clone(),
                    document_id: String::new(),
                    browsable: true,
                }],
                ..CataloguePage::default()
            },
            // A search of the catalogue reaches every provider's rows.
            None | Some("1") => CataloguePage::from_rows(
                "1".into(),
                vec![notebook(None), notebook(Some("pin"))],
                &query,
            ),
            Some(_) => CataloguePage::from_rows("1".into(), Vec::new(), &query),
        };
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let ran = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.invocation == *id);
        ran.then(|| {
            InvocationState::new(
                *id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: tilo_actions::ActionOutput::default(),
                },
            )
        })
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// The device's runs: one workspace, and the keys a run was opened for.
struct DeviceRuns {
    workspace: String,
    broker: Broker,
    opened: Mutex<Vec<DeviceCert>>,
}

impl Sessions for DeviceRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        (tilo_desktop_model::WorkspaceId::parse(&self.workspace) == Some(workspace_id))
            .then(|| self.workspace.clone())
    }

    fn open(&self, workspace: &str, key: &DeviceCert) -> Result<(SessionHost, Caller), String> {
        if workspace != self.workspace {
            return Err(format!("no workspace {workspace} here"));
        }
        self.opened.lock().unwrap().push(key.clone());
        let caller =
            Caller::local_person("ada").delegated_to(tilo_mcp::devices::agent_id("agent", key));
        Ok((Arc::new(self.broker.clone()), caller))
    }

    fn dispatch(&self) -> SessionHost {
        Arc::new(self.broker.clone())
    }
}

/// The person's device, hosting a workspace for them on its endpoint.
struct Box_ {
    host: Arc<WorkspaceService>,
    cert: DeviceCert,
    endpoint: Endpoint,
    workspace: String,
    runs: Arc<DeviceRuns>,
    key_sessions: Arc<KeySessions>,
    _router: Router,
    _state: tempfile::TempDir,
}

async fn device_host(root: &SigningKey) -> Box_ {
    let (key, cert) = certified(root, 2, "Build box", DeviceCapabilities::primary());
    let endpoint = endpoint(&key).await;
    let state = tempfile::tempdir().unwrap();
    let key_sessions = KeySessions::new();
    let host = Arc::new(
        WorkspaceService::open(
            ServiceConfig {
                apps: Some(Arc::clone(&key_sessions)
                    as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>),
                // Reached on this machine's own network: nothing will name this
                // endpoint's address, so opening does not wait for one.
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                ..ServiceConfig::new(
                    state.path().join("workspace-host"),
                    tilo_server_protocol::WorkSetting::none(),
                )
            },
            ServiceIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            endpoint.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let made = host.workspaces().create("Shell").await.unwrap();
    let runs = Arc::new(DeviceRuns {
        workspace: made.workspace.clone(),
        broker: Broker {
            workspace: made.workspace.clone(),
            ..Broker::default()
        },
        opened: Mutex::default(),
    });
    key_sessions.serve(
        Arc::new(PersonalDevices::new(host.workspaces().clone(), &cert)),
        Arc::clone(&runs) as Arc<dyn Sessions>,
    );
    let router = host
        .protocols()
        .into_iter()
        .fold(
            Router::builder(endpoint.clone()),
            |router, (alpn, handler)| router.accept(alpn, handler),
        )
        .spawn();
    Box_ {
        host,
        cert,
        endpoint,
        workspace: made.workspace,
        runs,
        key_sessions,
        _router: router,
        _state: state,
    }
}

/// `key`'s session with the device, as it opens one with any host.
async fn session(device: &Box_, key: &(SigningKey, DeviceCert)) -> Device {
    let session = Device::over(
        PeerConfig {
            host: tilo_server_client::ServerAddress::at(addr(&device.endpoint)),
            cert: key.1.clone(),
            server_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(key.0.clone()),
        &key.1.device_label,
    );
    session.connected().await.unwrap();
    session
}

fn tool(id: u64, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}})
}

fn text(answer: &Value) -> &str {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_lists_enters_and_acts_in_a_workspace_its_persons_device_hosts() {
    let root = SigningKey::from_bytes(&[0x11; 32]);
    let device = device_host(&root).await;
    let agent = certified(
        &root,
        9,
        "Visa backend #1",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let key = session(&device, &agent).await;

    // The person's workspaces, as the host lists them to a key of theirs.
    let listed = key
        .personal_list(
            &tilo_server_session::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == device.workspace),
        "the device lists the workspace it hosts: {listed:?}"
    );

    // It enters the workspace as every key enters one on any host.
    let workspace_id = key
        .enter_workspace(&device.workspace, "Visa backend #1")
        .await
        .unwrap();
    assert!(
        device
            .host
            .is_subscribed(workspace_id, &agent.1.device_public_key)
            .await
    );

    // Its agent surface acts there, on the device's broker, in a run of its own.
    let entered = tilo_mcp::McpServer::new(
        key.actions(workspace_id),
        device.workspace.clone(),
        hex::encode(agent.1.user_public_key),
    );
    let apps = entered
        .handle(tool(2, "tilo_ls", &json!({})))
        .await
        .unwrap();
    assert_eq!(apps["result"]["isError"], false, "{apps}");
    assert!(text(&apps).contains("notes"), "{apps}");
    let done = entered
        .handle(tool(3, "tilo_do", &json!({"path": "notes/Notebook#pin"})))
        .await
        .unwrap();
    assert_eq!(done["result"]["isError"], false, "{done}");
    assert_eq!(
        device.runs.broker.invoked.lock().unwrap().len(),
        1,
        "it ran on the device"
    );
    let opened = device.runs.opened.lock().unwrap().clone();
    assert_eq!(
        opened,
        vec![agent.1.clone()],
        "one run, for the key that came in"
    );
    key.close();
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_reaching_another_workspace_enters_nothing_and_runs_nothing_here() {
    let root = SigningKey::from_bytes(&[0x12; 32]);
    let device = device_host(&root).await;
    let elsewhere = certified(
        &root,
        10,
        "elsewhere",
        DeviceCapabilities::agent_grant(
            AgentScope::parse("22222222-2222-4222-8222-222222222222").unwrap(),
        ),
    );
    let key = session(&device, &elsewhere).await;
    let refused = key
        .enter_workspace(&device.workspace, "elsewhere")
        .await
        .unwrap_err();
    // The device's host said so itself, and why: a session that never reached
    // it, or an answer that never came, is no refusal of this key.
    assert_eq!(
        tilo_server_session::workspaces::Refusal::from_reason(&refused.message),
        Some(tilo_server_session::workspaces::Refusal::NotYourOwn),
        "{refused:?}"
    );
    assert!(
        !device
            .host
            .is_subscribed(
                device.workspace.parse().unwrap(),
                &elsewhere.1.device_public_key
            )
            .await,
        "the key entered nothing"
    );
    assert!(device.runs.opened.lock().unwrap().is_empty());
}

/// A window a device shows on a page, as its window process answers: the page whole, and what
/// each act did, said back in the markup of the element it changed.
struct ShownWindow {
    /// Told when a window's device let go of it.
    closed: std::sync::mpsc::Sender<()>,
}

impl tilo_runtime::key_sessions::PageViews for ShownWindow {
    fn open(
        &self,
        workspace: &str,
        app: &str,
        caller: &Caller,
    ) -> Result<
        tilo_server_protocol::page::window::WindowLink,
        tilo_server_protocol::page::PageRefusal,
    > {
        use tilo_server_protocol::page::window::{WindowAnswer, WindowAsk, receive, send};
        if app != "notes" {
            return Err(tilo_server_protocol::page::PageRefusal::NoSuchApp);
        }
        // The window acts in the page's workspace: a caller of another is refused.
        if caller.workspace() != Some(workspace) {
            return Err(tilo_server_protocol::page::PageRefusal::Elsewhere);
        }
        let (device, mut window) = std::os::unix::net::UnixStream::pair().unwrap();
        let closed = self.closed.clone();
        std::thread::spawn(move || {
            while let Some(ask) = receive::<WindowAsk>(&mut window).unwrap() {
                let answer = match ask {
                    WindowAsk::Open { show } => WindowAnswer::Shown {
                        title: "Notes".into(),
                        css: ".n{}".into(),
                        body: format!("<main data-p=\"0\">{}</main>", show.unwrap_or_default()),
                    },
                    WindowAsk::Act { id, action, .. } => {
                        WindowAnswer::Changed(vec![tilo_server_protocol::page::PagePatch {
                            id,
                            html: format!("{action:?}"),
                        }])
                    }
                    WindowAsk::Changed => WindowAnswer::Changed(Vec::new()),
                };
                send(&mut window, &answer).unwrap();
            }
            closed.send(()).unwrap();
        });
        Ok(tilo_server_protocol::page::window::WindowLink::over(device))
    }
}

/// A member opens the workspace's pages on the device that holds it: the front page lists the
/// apps its dispatch offers them, the search runs an action the catalogue offers as them, and an
/// app's page is its window, which takes what they did and goes when the page closes.
#[tokio::test(flavor = "current_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one member's visit, read top to bottom"
)]
async fn a_member_opens_searches_and_acts_on_pages_the_device_serves() {
    use tilo_server_protocol::page::{PageAction, PageAddress};
    use tilo_workspace_service::app_serving::AppActionServer as _;
    let root = SigningKey::from_bytes(&[0x13; 32]);
    let device = device_host(&root).await;
    let (closed, window_closed) = std::sync::mpsc::channel();
    device
        .key_sessions
        .pages_served()
        .show_through(Arc::new(ShownWindow { closed }));
    let pages = device.key_sessions.pages().unwrap();
    let workspace_id = WorkspaceId::parse(&device.workspace).unwrap();
    let ada = Caller::local_person("ada").in_workspace(device.workspace.clone());
    let act = |id, action| tilo_workspace_service::app_serving::PageAct {
        id,
        target: None,
        action,
        signed: None,
    };
    let address = |app: &str, show: Option<&str>| PageAddress {
        workspace: device.workspace.clone(),
        app: app.into(),
        show: show.map(str::to_owned),
    };

    let home = pages
        .open(workspace_id, &ada, address("", None))
        .await
        .unwrap()
        .unwrap();
    assert!(
        home.body
            .contains(&format!("href=\"/w/{workspace_id}/notes\"")),
        "the front page opens Notes: {}",
        home.body
    );
    assert!(
        home.body.contains("/search\""),
        "and the workspace's search"
    );

    let search = pages
        .open(workspace_id, &ada, address("search", Some("pin")))
        .await
        .unwrap()
        .unwrap();
    let pin = search
        .body
        .split("data-p=\"")
        .skip(1)
        .find(|part| part.contains("Pin"))
        .and_then(|part| part.split('"').next())
        .and_then(|id| id.parse::<u32>().ok())
        .expect("the search offers the notebook's Pin");
    let ran = pages
        .act(
            workspace_id,
            search.page,
            Some(act(pin, PageAction::Activate)),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        ran.iter().any(|patch| patch.html.contains("Pin: Done.")),
        "the row's action ran and said so: {ran:?}"
    );
    let invoked = device.runs.broker.invoked.lock().unwrap().clone();
    assert!(
        invoked
            .iter()
            .any(|request| request.target.address.action == "pin"),
        "the dispatch ran Pin"
    );
    assert!(
        device
            .runs
            .broker
            .callers
            .lock()
            .unwrap()
            .iter()
            .all(|caller| caller.workspace() == Some(device.workspace.as_str())),
        "every call went as the page's person in its workspace"
    );

    let notes = pages
        .open(workspace_id, &ada, address("notes", Some("groceries")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notes.title, "Notes");
    assert!(
        notes.body.contains("groceries"),
        "the window opened what the address names"
    );
    let toggled = pages
        .act(workspace_id, notes.page, Some(act(7, PageAction::Toggle)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        toggled.first().map(|patch| (patch.id, patch.html.as_str())),
        Some((7, "Toggle"))
    );
    assert_eq!(
        pages
            .open(workspace_id, &ada, address("mail", None))
            .await
            .unwrap(),
        Err(tilo_server_protocol::page::PageRefusal::NoSuchApp),
        "an app the workspace has no window of has no page"
    );
    pages.close(workspace_id, notes.page);
    window_closed
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the window goes with its page");
}
