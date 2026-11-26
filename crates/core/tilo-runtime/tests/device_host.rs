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
use iroh::{Endpoint, EndpointAddr, EndpointId};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, formats,
};
use tilo_desktop_model::WorkspaceId;
use tilo_runtime::key_sessions::{KeySessions, SessionHost, Sessions};
use tilo_runtime::other_devices::Whereabouts;
use tilo_runtime::personal_devices::PersonalDevices;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_service::own_device;
use tilo_workspace_service::service::{ServiceConfig, WorkspaceService};

use super::actions_host::{Broker, NOTES, addr, endpoint};

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

    fn open(
        &self,
        workspace: &str,
        key: &DeviceCert,
        _whose: tilo_runtime::personal_devices::Whose,
    ) -> Result<(SessionHost, Caller), String> {
        if workspace != self.workspace {
            return Err(format!("no workspace {workspace} here"));
        }
        self.opened.lock().unwrap().push(key.clone());
        let caller =
            Caller::local_person("ada").delegated_to(tilo_mcp::devices::agent_id("agent", key));
        Ok((Arc::new(self.broker.clone()), caller))
    }

    fn dispatch(&self, workspace: &str) -> Option<SessionHost> {
        if workspace != self.workspace {
            return None;
        }
        Some(Arc::new(self.broker.clone()))
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
    _keys: tilo_workspace_service::own_device::KeyService,
    _state: tempfile::TempDir,
}

/// The workspace service of the device `key` and `cert` name, opened in `dir` as every device
/// opens its own: with its key service beside it, so the device takes part in its workspaces'
/// channels, keeps its person's own workspace and founds a workspace there. Its apps are answered
/// by `apps`, and it is reached on this
/// machine's own network, so opening waits for no address.
async fn as_its_device(
    dir: &std::path::Path,
    key: &SigningKey,
    cert: &DeviceCert,
    endpoint: &Endpoint,
    apps: Arc<dyn tilo_workspace_service::app_serving::AppActionServer>,
) -> (
    Arc<WorkspaceService>,
    tilo_workspace_service::own_device::KeyService,
) {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(key.clone());
    let own_device::Opened { service, keys } = own_device::open_in(
        &dir.join("device"),
        own_device::Device {
            cert,
            signer: &signer,
            itself: None,
        },
        own_device::vault_key(key),
        endpoint.clone(),
        Arc::new(tilo_workspace_keys::NoSealChecks),
        None,
        |config| ServiceConfig {
            apps: Some(apps),
            reaching: tilo_workspace_service::Reaching::OnItsOwn,
            ..config
        },
    )
    .await
    .unwrap();
    // The device keeps its person's own workspace, as every device of theirs does from its
    // setup: the list of their workspaces each one the device founds is named in.
    let person = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
    service
        .hold_own_workspace(
            &person,
            tilo_workspace_service::workspaces::person_workspace::NAME,
            cert,
        )
        .await
        .unwrap();
    service
        .workspaces()
        .enter(&person, (cert, key))
        .await
        .unwrap();
    (Arc::new(service), keys)
}

async fn device_host(root: &SigningKey) -> Box_ {
    let (key, cert) = certified(root, 2, "Build box", DeviceCapabilities::primary());
    let endpoint = endpoint(&key).await;
    let state = tempfile::tempdir().unwrap();
    let key_sessions = KeySessions::new();
    let apps =
        Arc::clone(&key_sessions) as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>;
    // Reached on this machine's own network: nothing will name this endpoint's address, so
    // opening does not wait for one.
    let (host, keys) = as_its_device(state.path(), &key, &cert, &endpoint, apps).await;
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
        _keys: keys,
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
        .person_workspaces(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
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
    let entered = tilo_mcp::McpServer::for_agent(
        key.actions(workspace_id),
        device.workspace.clone(),
        &tilo_mcp::devices::agent_id("agent", &agent.1),
        &hex::encode(agent.1.user_public_key),
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
    // The workspace's link refuses the call before the host's own operations see it, and says
    // why: the key's grant names another workspace, so the request it signs proves no device that
    // reaches this one. A session that never reached the host, or an answer that never came, is no
    // refusal of this key.
    assert_eq!(refused.code, "refused", "{refused:?}");
    assert!(
        refused
            .message
            .contains("grant does not reach this workspace"),
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

/// An agent's grant, dialing the device that hosts the person's workspace, asks as the agent it
/// is: what only a person on a device of theirs reads — their own region and formats — is
/// refused it, which the person's server key reads as they do.
#[tokio::test(flavor = "current_thread")]
async fn an_agents_grant_reads_no_persons_formats_where_a_server_key_does() {
    let root = SigningKey::from_bytes(&[0x16; 32]);
    let device = device_host(&root).await;

    let agent = certified(
        &root,
        16,
        "Visa backend #3",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let agent_key = session(&device, &agent).await;
    let entered = agent_key
        .enter_workspace(&device.workspace, "Visa backend #3")
        .await
        .unwrap();
    let refused = agent_key
        .ask_host(
            entered,
            formats::target(formats::READ),
            serde_json::Map::new(),
        )
        .await
        .expect_err("an agent reads no person's formats");
    assert_eq!(
        refused.code,
        ActionCallError::UnknownAction.code(),
        "the host answers, and answers no: {refused:?}"
    );
    agent_key.close();

    let server = certified(
        &root,
        17,
        "Visa backend #4",
        DeviceCapabilities::own_key_grant(AgentScope::AllWorkspaces),
    );
    let server_key = session(&device, &server).await;
    let entered = server_key
        .enter_workspace(&device.workspace, "Visa backend #4")
        .await
        .unwrap();
    let formats = server_key
        .ask_host(
            entered,
            formats::target(formats::READ),
            serde_json::Map::new(),
        )
        .await
        .expect("a server key reads its person's formats");
    assert!(
        formats.is_object(),
        "the formats a person on their device reads: {formats}"
    );
    server_key.close();
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_key_reads_the_persons_workspace_no_more() {
    let root = SigningKey::from_bytes(&[0x14; 32]);
    let device = device_host(&root).await;
    let agent = certified(
        &root,
        12,
        "Visa backend #2",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let key = session(&device, &agent).await;
    let labels = tilo_server_protocol::workspaces::WorkspaceLabels::new();

    // The key enters the person's workspace at its first read, and reads their list there.
    let listed = key.person_workspaces(&labels, None).await.unwrap();
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == device.workspace),
        "{listed:?}"
    );

    // The person revokes it, and the revocation reaches the device that keeps their workspace.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: agent.1.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    device
        .host
        .workspaces()
        .announce_revocations(vec![entry])
        .await
        .unwrap();

    // From then on the workspace refuses it: the host answers, and answers no.
    let refused = key.person_workspaces(&labels, None).await.unwrap_err();
    assert_ne!(
        refused.code,
        tilo_session_client::device::DeviceRefusal::UNANSWERED,
        "{refused:?}"
    );
    key.close();
}

#[tokio::test(flavor = "current_thread")]
async fn a_grant_for_one_workspace_is_invited_into_that_one_alone() {
    let root = SigningKey::from_bytes(&[0x13; 32]);
    let device = device_host(&root).await;
    let second = device.host.workspaces().create("Other").await.unwrap();
    let grant = certified(
        &root,
        11,
        "one workspace",
        DeviceCapabilities::agent_grant(AgentScope::parse(&device.workspace).unwrap()),
    );
    let key = session(&device, &grant).await;

    // The invitation into the workspace its grant reaches is signed for that workspace, and the
    // host admits it by the scope the person's root signed.
    let entered = key
        .enter_workspace(&device.workspace, "one workspace")
        .await
        .unwrap();
    assert!(
        device
            .host
            .is_subscribed(entered, &grant.1.device_public_key)
            .await
    );

    // The same grant asking for another workspace's invitation is refused: its scope reaches
    // the one workspace, and the invitation it asks for is signed for the other.
    let refused = key
        .enter_workspace(&second.workspace, "one workspace")
        .await
        .unwrap_err();
    assert_eq!(refused.code, "refused", "{refused:?}");
    assert!(
        refused
            .message
            .contains("grant does not reach this workspace"),
        "{refused:?}"
    );
    assert!(
        !device
            .host
            .is_subscribed(
                second.workspace.parse().unwrap(),
                &grant.1.device_public_key
            )
            .await,
        "the key entered nothing else"
    );
    key.close();
}

/// A window a device shows on a page, as its window process answers: the page whole, and what
/// each act did, said back in the markup of the element it changed.
struct ShownWindow {
    /// Told when a window's device let go of it.
    closed: std::sync::mpsc::Sender<()>,
    /// The pointer events the page carried into the window.
    pointers: Arc<std::sync::Mutex<Vec<tilo_server_protocol::page::PointerEvent>>>,
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
        if app != NOTES {
            return Err(tilo_server_protocol::page::PageRefusal::NoSuchApp);
        }
        // The window acts in the page's workspace: a caller of another is refused.
        if caller.workspace() != Some(workspace) {
            return Err(tilo_server_protocol::page::PageRefusal::Elsewhere);
        }
        let (device, mut window) = std::os::unix::net::UnixStream::pair().unwrap();
        let closed = self.closed.clone();
        let pointers = Arc::clone(&self.pointers);
        std::thread::spawn(move || {
            while let Some(ask) = receive::<WindowAsk>(&mut window).unwrap() {
                let answer = match ask {
                    WindowAsk::Open { show } => WindowAnswer::Shown {
                        title: "Notes".into(),
                        css: ".n{}".into(),
                        body: format!("<main data-p=\"0\">{}</main>", show.unwrap_or_default()),
                    },
                    WindowAsk::Act { id, action, .. } => WindowAnswer::Changed(vec![
                        tilo_server_protocol::page::PagePatch::Replace {
                            id,
                            html: format!("{action:?}"),
                        },
                    ]),
                    WindowAsk::Changed | WindowAsk::Wells(_) => WindowAnswer::Changed(Vec::new()),
                    WindowAsk::Pointer(event) => {
                        pointers.lock().unwrap().push(event);
                        WindowAnswer::Changed(vec![
                            tilo_server_protocol::page::PagePatch::Replace {
                                id: 0,
                                html: "pointer".into(),
                            },
                        ])
                    }
                    WindowAsk::Appearance { dark } => WindowAnswer::Shown {
                        title: if dark { "Notes, dark" } else { "Notes, light" }.into(),
                        css: String::new(),
                        body: "<main></main>".into(),
                    },
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
    let pointers = Arc::new(std::sync::Mutex::new(Vec::new()));
    device
        .key_sessions
        .pages_served()
        .show_through(Arc::new(ShownWindow {
            closed,
            pointers: Arc::clone(&pointers),
        }));
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
        ran.iter()
            .any(|patch| patch.html().is_some_and(|html| html.contains("Pin: Done."))),
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
        toggled.first(),
        Some(&tilo_server_protocol::page::PagePatch::Replace {
            id: 7,
            html: "Toggle".into()
        })
    );
    // The pointer on the page reaches the window that shows it, and its answer reaches the page.
    let carried = pages
        .pointer(
            workspace_id,
            notes.page,
            tilo_server_protocol::page::PointerEvent {
                kind: tilo_server_protocol::page::PointerKind::Down,
                x: 40,
                y: 12,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        carried.first(),
        Some(&tilo_server_protocol::page::PagePatch::Replace {
            id: 0,
            html: "pointer".into()
        }),
        "the window's answer reaches the page"
    );
    assert_eq!(
        pointers.lock().unwrap().as_slice(),
        [tilo_server_protocol::page::PointerEvent {
            kind: tilo_server_protocol::page::PointerKind::Down,
            x: 40,
            y: 12,
        }],
        "the pointer reached the window's process"
    );
    assert_eq!(
        pages
            .open(workspace_id, &ada, address("mail", None))
            .await
            .unwrap(),
        Err(tilo_server_protocol::page::PageRefusal::NoSuchApp),
        "an app the catalogue does not list them has no page"
    );
    pages.close(workspace_id, notes.page);
    window_closed
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the window goes with its page");
}

/// The agent keys a test makes from the person's root it holds: the same key for the same agent
/// in the same workspace, certified as an agent's grant, exactly as the device does in production.
#[derive(Default)]
struct TestKeys {
    root: Option<SigningKey>,
    made: Mutex<Vec<([u8; 32], DeviceCert, SigningKey)>>,
}

impl tilo_app_host::agents::agent_keys::AgentKeys for TestKeys {
    fn grant(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        agent: &str,
        _name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        let root = self.root.as_ref().ok_or("this device holds no root")?;
        let mut seed = [0x5a; 32];
        seed[0] = u8::try_from(agent.len()).unwrap_or(0);
        let key = SigningKey::from_bytes(&seed);
        let public = key.verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: public,
            device_label: agent.into(),
            capabilities: DeviceCapabilities::agent_grant(AgentScope::agent_in(
                *workspace.as_uuid().as_bytes(),
                agent,
            )),
            issued_at_ns: now.saturating_sub(1_000_000_000),
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).map_err(|error| error.to_string())?;
        self.made.lock().unwrap().push((public, cert, key));
        Ok(public)
    }

    fn sign(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let made = self.made.lock().unwrap();
        let (_, cert, signing) = made
            .iter()
            .find(|(public, ..)| public == key)
            .ok_or("the agent's key was not made here")?;
        request.signed = None;
        let invocation = tilo_server_protocol::invocation::sign_invocation_now(
            &workspace.to_string(),
            request,
            cert,
            signing,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| error.to_string())?;
        request.signed = Some(Box::new(invocation));
        Ok(())
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.made
            .lock()
            .unwrap()
            .iter()
            .find(|(public, ..)| public == key)
            .map(|(_, cert, signing)| (cert.clone(), signing.clone()))
    }
}

/// What the device that keeps the workspace answers a forwarded client with: Notes runs there,
/// and the caller of each request is kept, as the connection admitted it.
#[derive(Default)]
struct Answers {
    broker: Mutex<Broker>,
    /// Every request that reached the host, and the callers the invocations came as.
    invokers: Arc<Mutex<Vec<Caller>>>,
}

impl Answers {
    fn answer(&self, caller: &Caller, message: RemoteActionMessage) -> RemoteActionMessage {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let result = self
                    .broker
                    .lock()
                    .unwrap()
                    .query(caller, query)
                    .wait_timeout(std::time::Duration::from_secs(5))
                    .ok()
                    .flatten()
                    .unwrap_or(Err(ActionCallError::Unanswered));
                RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result,
                }
            }
            RemoteActionMessage::Invoke(request) => {
                self.invokers.lock().unwrap().push(caller.clone());
                let invocation = request.invocation;
                let receipt = {
                    let broker = self.broker.lock().unwrap();
                    broker
                        .invoke(caller, request)
                        .wait_timeout(std::time::Duration::from_secs(5))
                        .ok()
                        .flatten()
                };
                let status = match receipt {
                    Some(Ok(receipt)) => receipt.status,
                    Some(Err(error)) => ReceiptStatus::Rejected { error },
                    None => ReceiptStatus::Rejected {
                        error: ActionCallError::Unanswered,
                    },
                };
                RemoteActionMessage::Receipt(InvocationReceipt { invocation, status })
            }
            RemoteActionMessage::Observe { invocation, .. } => RemoteActionMessage::Update {
                invocation,
                state: self.broker.lock().unwrap().observe(caller, &invocation),
                correlation: None,
            },
            RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
                invocation,
                result: Ok(()),
            },
            other => tilo_actions::remote::refuse(
                &other,
                ActionCallError::Unavailable {
                    reason: "not here".into(),
                },
            )
            .unwrap_or(other),
        }
    }
}

impl tilo_workspace_service::app_serving::AppActionServer for Answers {
    fn serve(
        &self,
        _workspace: tilo_server_protocol::WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        Some(Pending::ready(self.answer(caller, message)))
    }

    fn serve_certified(
        &self,
        _workspace: tilo_server_protocol::WorkspaceId,
        caller: &Caller,
        _certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        Some(Pending::ready(self.answer(caller, message)))
    }
}

/// The person's devices where this laptop finds them: the workspace one device keeps, its roster,
/// and where that device is.
struct TestPlaces {
    workspace: String,
    host: EndpointAddr,
    devices: Vec<DeviceCert>,
}

impl Whereabouts for TestPlaces {
    fn workspaces(&self) -> Vec<String> {
        vec![self.workspace.clone()]
    }

    fn addr(&self, _id: EndpointId) -> EndpointAddr {
        self.host.clone()
    }

    fn devices(&self, _workspace: &str) -> Vec<DeviceCert> {
        self.devices.clone()
    }
}

/// A forwarded client, speaking MCP on its own line each.
struct ForwardedClient {
    stream: std::os::unix::net::UnixStream,
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    next: u64,
}

impl ForwardedClient {
    fn over(stream: std::os::unix::net::UnixStream) -> Self {
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        stream
            .set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut client = Self {
            stream,
            reader,
            next: 1,
        };
        // The client's hello, as every MCP client opens.
        let hello = client.send(&json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "1"}
            }
        }));
        assert!(hello.get("result").is_some(), "{hello}");
        client
    }

    fn send(&mut self, request: &Value) -> Value {
        use std::io::{BufRead as _, Write as _};
        writeln!(self.stream, "{request}").unwrap();
        let id = request["id"].clone();
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).unwrap();
            assert_ne!(read, 0, "the forward closed without answering {request}");
            let answer: Value = match serde_json::from_str(&line) {
                Ok(answer) => answer,
                Err(_) => continue,
            };
            if answer.get("id") == Some(&id) && answer.get("method").is_none() {
                return answer;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Value {
        let id = self.next;
        self.next = id.saturating_add(1);
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
    }
}

/// Two of a person's devices: one keeps a workspace, the other's daemon carries an agent's MCP
/// client to it under the agent's own key. Returns the forward along with what the test flips.
struct Forwarded {
    client: ForwardedClient,
    host: Arc<WorkspaceService>,
    answers: Arc<Answers>,
    host_endpoint: Endpoint,
    laptop_endpoint: Endpoint,
    router: Router,
    served: Option<std::thread::JoinHandle<()>>,
    _keys: tilo_workspace_service::own_device::KeyService,
    _state: tempfile::TempDir,
}

impl Forwarded {
    async fn start() -> Self {
        let root = SigningKey::from_bytes(&[0x21; 32]);
        let person = root.verifying_key().to_bytes();
        let (host_key, host_cert) = certified(&root, 2, "Build box", DeviceCapabilities::primary());
        let host_endpoint = endpoint(&host_key).await;
        let answers = Arc::new(Answers::default());
        let state = tempfile::tempdir().unwrap();
        let apps =
            Arc::clone(&answers) as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>;
        let (host, key_service) =
            as_its_device(state.path(), &host_key, &host_cert, &host_endpoint, apps).await;
        let made = host.workspaces().create("Shell").await.unwrap();
        let workspace = made.workspace.clone();
        *answers.broker.lock().unwrap() = Broker {
            workspace: workspace.clone(),
            ..Broker::default()
        };
        let router = host.protocols().into_iter().fold(
            Router::builder(host_endpoint.clone()),
            |router, (alpn, handler)| router.accept(alpn, handler),
        );
        let router = router.spawn();

        // The laptop: another device of the person's, holding their root, which makes the
        // agent's key as the person's device does.
        let (laptop_key, laptop_cert) =
            certified(&root, 3, "Studio laptop", DeviceCapabilities::primary());
        let laptop_endpoint = endpoint(&laptop_key).await;
        let keys = Arc::new(TestKeys {
            root: Some(root.clone()),
            made: Mutex::default(),
        });
        let places = Arc::new(TestPlaces {
            workspace: workspace.clone(),
            host: addr(&host_endpoint),
            devices: vec![host_cert.clone(), laptop_cert.clone()],
        });
        let other = tilo_runtime::other_devices::OtherDevices::new(
            tokio::runtime::Handle::current(),
            (person, laptop_cert.device_public_key),
            tilo_user_identity::RevocationList::new(),
            keys,
            places,
        );
        let (local, remote) = std::os::unix::net::UnixStream::pair().unwrap();
        let forward = tilo_mcp::devices::Forward {
            device: "Build box".into(),
            workspace: Some(workspace.clone()),
        };
        let served = std::thread::spawn(move || other.serve_forward(remote, &forward));
        Self {
            client: ForwardedClient::over(local),
            host,
            answers,
            host_endpoint,
            laptop_endpoint,
            router,
            served: Some(served),
            _keys: key_service,
            _state: state,
        }
    }

    /// The caller the host last ran an invocation as.
    fn last_invoker(&self) -> Caller {
        self.answers
            .invokers
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("an invocation reached the host")
    }

    async fn finish(mut self) {
        drop(self.client);
        if let Some(served) = self.served.take() {
            served.join().unwrap();
        }
        self.router.shutdown().await.unwrap();
        self.host_endpoint.close().await;
        self.laptop_endpoint.close().await;
        self.host.shutdown().await;
    }
}

tilo_test_env::scenario! {
    fn an_agent_on_the_persons_laptop_works_on_another_device_through_the_forward;

    phase the_forward_lists_the_persons_devices_and_its_agent_acts_with_no_setup;
    {
        runtime().block_on(async {
            let mut forwarded = Forwarded::start().await;
            // What the other device runs, listed there through the forward.
            let listed = forwarded.client.call("tilo_ls", &json!({}));
            assert_eq!(listed["result"]["isError"], false, "{listed}");
            assert!(text(&listed).contains("notes"), "{listed}");
            // The rows the host's broker answers, by their names: Notes' notebook.
            let notes = forwarded.client.call("tilo_ls", &json!({"path": "notes"}));
            assert_eq!(notes["result"]["isError"], false, "{notes}");
            assert!(text(&notes).contains("Notebook"), "{notes}");
            // Its agent acts here under the workspace's grants naming it: no device setting
            // lets it in, and no switch is turned on for it.
            let done = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            assert_eq!(done["result"]["isError"], false, "{done}");
            let caller = forwarded.last_invoker();
            assert!(caller.acting().is_agent(), "it acts as the agent: {caller:?}");
            forwarded.finish().await;
        });
    }
}

/// A runtime for one phase: the forward's session and the host's peer session live on it.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
