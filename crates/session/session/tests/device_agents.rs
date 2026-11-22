//! An agent on another of the person's devices works in the workspace this desktop holds over the
//! one action frame between devices: the laptop it runs on serves it MCP and sends each of its
//! requests here naming it ([`HandedAgent`]), and this real session takes each as that agent's,
//! for the person, never as the person. The desktop's setting decides whether such agents work
//! here at all, legibly refusing them while it is off; once in, the agent is the person's agent
//! member in a session of its own, listed with the laptop beside its name, until the person's Stop
//! here. Its steps are decided by its grants: what they do not allow is refused with nobody
//! asked, and where they say ask the person is asked on this desktop's sheet, unless the desktop's
//! second setting lets such agents' steps go unasked.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call \
              made into it"
)]

use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use neighbour_wire::actions::HandedAgent;
use serde_json::{Value, json};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::{IdentityPaths, persist};
use tilo_mcp::McpServer;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::owner::{self, testing};
use tilo_peer_state::{SeedState, StatePeer, TransportOrigin, seed_workspace};
use tilo_service::Questions;
use tilo_session::{AppSocket, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use crate::common::private_home;

/// What the agent's MCP client calls itself, and so what the shell calls its agent member here.
const CLIENT: &str = "Claude Code";

/// The app the agent's steps are taken in: Notes, as it publishes its notebook.
const NOTES_APP: &str = "so.tilo.notes";

/// The record of the person's Notes keeps in their own workspace, by its kind.
const NOTEBOOK: &str = "notes.notebook";

/// Notes' surface, as its package names it: the person's notebook; `open`, which reaches outside
/// it; and `archive`, which moderates it.
const NOTES_SURFACE: &[u8] = b"format = 2\n\n[[resources]]\nkind = \"notes.notebook\"\nlabel = \"Notebook\"\nversion = 1\nperson = true\n\n[[resources.verbs]]\nid = \"open\"\nkind = \"outside\"\n\n[[resources.verbs]]\nid = \"archive\"\nkind = \"undoable\"\n";

/// What a step's question to the person is read as on the sheet.
const STEP_QUESTION: &str = "session.step-question";

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

/// This computer: its own identity, and the workspace it holds.
struct ThisComputer {
    key: SigningKey,
    cert: DeviceCert,
    /// The person's root, signing here as it does at their own desktop.
    root: tilo_identity::store::SigningIdentity,
    workspace: String,
    state: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

impl ThisComputer {
    fn new() -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let system = home.root().join("system-identity");
        let state = tempfile::tempdir().expect("a state directory");
        home.env()
            .set("TILO_IDENTITY_ROOT", &system)
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        let paths = IdentityPaths::under(&system);
        let identity_dir = home.config().join("tilo/identity");
        tilo_identity::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("this computer has an identity of its own");
        let key = persist::load_device_key(&paths).expect("this device's key");
        let cert =
            persist::read_device_cert(&paths.device_cert).expect("this device's certificate");
        let record = tilo_identity::store::resolve_active_identity(&identity_dir)
            .expect("this identity's record");
        let root = tilo_identity::store::load_from_path(record)
            .expect("the person's root signs on this box");
        let workspace = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
        held_here(state.path(), &workspace, &cert, &key);
        Self {
            key,
            cert,
            root,
            workspace,
            state,
            _home: home,
        }
    }

    /// The person's laptop, which their root certified: where the agent runs.
    fn laptop(&self) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[0x55; 32]);
        let mut cert = DeviceCert {
            user_public_key: self.cert.user_public_key,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Studio laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        cert.signature = self
            .root
            .sign(&tilo_user_identity::device_cert_signing_digest(&cert).expect("a digest"))
            .expect("the person's root certifies the laptop");
        (key, cert)
    }

    /// The workspace's members with the laptop among them, as the action service decides who may ask.
    fn members_with(&self, laptop: &DeviceCert) -> StatePeer {
        let now = tilo_clock::wall();
        let members = vec![self.cert.clone(), laptop.clone()];
        let mut peer = StatePeer::new(
            &self.workspace,
            self.cert.clone(),
            members.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(&self.workspace, &self.cert, &members).unwrap(),
                documents: Vec::new(),
            },
            &self.key,
            now,
        )
        .unwrap();
        peer
    }
}

/// This computer's copy of `workspace` under `state`, governed by this device.
fn held_here(state: &std::path::Path, workspace: &str, cert: &DeviceCert, key: &SigningKey) {
    let owner = owner::open_file(
        &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
    )
    .expect("an owner for this device's workspace");
    let governor = testing::enroll_interactive_governor_in(&owner, key, cert, workspace)
        .expect("this device governs the workspace it hosts");
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    let id = WorkspaceId::parse(workspace).expect("a workspace id");
    for frame in frames {
        owner
            .receive(
                id,
                TransportOrigin::RetainedServerDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The agent on the laptop: its MCP server there, whose every request goes to this computer
/// naming it.
struct Agent {
    server: Arc<McpServer<RemoteDevice>>,
    _endpoint: Endpoint,
}

/// This computer's shell with Notes running in the workspace it holds, its action service
/// answering the laptop: the session, the questions it puts to the person, what Notes took, and
/// the laptop.
struct ShellWithNotes {
    session: Session,
    questions: Questions,
    taken: mpsc::Receiver<runtime_collab::Invocation>,
    laptop: (SigningKey, DeviceCert),
    runtime: tokio::runtime::Runtime,
    this_box: EndpointAddr,
    _router: (Endpoint, Router),
    _root: tempfile::TempDir,
    computer: ThisComputer,
}

impl ShellWithNotes {
    fn start() -> Self {
        let computer = ThisComputer::new();
        ui::app_surface::register_package(NOTES_APP, NOTES_SURFACE);
        let person = hex::encode(computer.cert.user_public_key);
        let root = private_home();
        let questions = Questions::new();
        let app_wire = root.path().join("app_wire.sock");
        let launches = Arc::new(tilo_session::Launches::default());
        launches.started(std::process::id(), NOTES_APP, &computer.workspace, true);
        let mut session = crate::common::headless(Options {
            person: person.clone(),
            collab: true,
            state: Some(computer.state.path().to_path_buf()),
            questions: Some(questions.clone()),
            app_socket: AppSocket::At(app_wire.clone()),
            mcp_server: Some(root.path().join("agent.sock")),
            launches,
            // The person's settings, as the session serves them.
            services: vec![Box::new(tilo_settings_service::SettingsService::new())],
            ..Options::default()
        });
        session
            .handle()
            .replace_tree(layout(&computer.workspace, &person));
        crate::common::until(
            &mut session,
            "the workspace this computer holds",
            |session| session.confines_agents(&computer.workspace),
        );

        let (published, ready) = mpsc::channel();
        let (took, taken) = mpsc::channel();
        {
            let (workspace, key, cert) = (
                computer.workspace.clone(),
                computer.key.clone(),
                computer.cert.clone(),
            );
            std::thread::spawn(move || {
                notes(&app_wire, &workspace, (&key, &cert), &published, &took);
            });
        }
        crate::common::wait_for(
            &mut session,
            "Notes publishes its notebook",
            |_| match ready.try_recv() {
                Ok(()) => Some(()),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("Notes stopped before it published")
                }
            },
        );

        let laptop = computer.laptop();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let actions = session
            .device_actions()
            .expect("this computer answers the person's other devices");
        actions.hold(Replica::new(computer.members_with(&laptop.1)));
        let (this_box, router) = runtime.block_on(async {
            let endpoint = endpoint(&computer.key).await;
            let router = Router::builder(endpoint.clone())
                .accept(ACTIONS_ALPN, actions)
                .spawn();
            (endpoint, router)
        });
        Self {
            session,
            questions,
            taken,
            laptop,
            runtime,
            this_box: addr(&this_box),
            _router: (this_box, router),
            _root: root,
            computer,
        }
    }

    /// The agent on the laptop, in a session of its own there.
    fn agent(&self) -> Agent {
        let (endpoint, asking) = self.runtime.block_on(async {
            let endpoint = endpoint(&self.laptop.0).await;
            let asking = ActionService::new(self.laptop.0.clone(), Arc::new(NoActions));
            (endpoint, asking)
        });
        let remote = asking.remote_for_agent(
            &endpoint,
            &self.this_box,
            &self.computer.workspace,
            HandedAgent {
                client: CLIENT.into(),
                session: "laptop-session-1".into(),
            },
        );
        let person = hex::encode(self.computer.cert.user_public_key);
        Agent {
            server: Arc::new(McpServer::for_agent(
                remote,
                self.computer.workspace.clone(),
                "claude-code",
                &person,
            )),
            _endpoint: endpoint,
        }
    }

    /// Send `request` from the agent: its answer, while it waits on this computer.
    fn sent(&self, agent: &Agent, request: Value) -> tokio::task::JoinHandle<Option<Value>> {
        let server = Arc::clone(&agent.server);
        self.runtime
            .spawn(async move { server.handle(request).await })
    }

    /// The agent's answer to what it was sent, the session running while it waits.
    #[track_caller]
    fn answer(&mut self, sent: tokio::task::JoinHandle<Option<Value>>) -> Value {
        crate::common::wait_for(&mut self.session, "the agent's answer", |_| {
            sent.is_finished().then_some(())
        });
        self.runtime
            .block_on(sent)
            .unwrap()
            .expect("the agent is answered")
    }

    /// What the agent is answered to `request`.
    #[track_caller]
    fn ask(&mut self, agent: &Agent, request: Value) -> Value {
        let sent = self.sent(agent, request);
        self.answer(sent)
    }

    /// Turn on the setting `verb` of this computer's agents from the person's other devices, as
    /// Settings does.
    fn set(&mut self, verb: &str, holds: fn(&serde_json::Map<String, Value>) -> bool) {
        self.session
            .invoke(
                "settings:settings.mcp-server",
                verb,
                json!({"on": true}).as_object().unwrap(),
            )
            .expect("the setting is kept");
        crate::common::until(&mut self.session, "the setting kept", |_| {
            holds(&settings_store::load())
        });
    }

    /// The questions up now, by the kind each is read as.
    fn asked(&self) -> Vec<String> {
        self.questions
            .pending()
            .into_iter()
            .map(|question| question.kind)
            .collect()
    }
}

/// The agent session the shell calls `name`, with the device it says beside the name.
fn listed(session: &Session, name: &str) -> Option<(String, String, String)> {
    session
        .read("session:agents")
        .and_then(|agents| agents.get("sessions").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .find(|agent| agent["name"] == name)
        .map(|agent| {
            let said = |field: &str| agent[field].as_str().unwrap_or_default().to_owned();
            (said("key"), said("device"), said("device_name"))
        })
}

/// A shell showing `workspace`, owned by `person`.
fn layout(workspace: &str, person: &str) -> Layout {
    let id = WorkspaceId::parse(workspace).expect("a workspace id");
    let mut held = Workspace::new(id, "Shell");
    held.set_owner(person);
    Layout {
        workspaces: vec![held.clone()],
        active: held.id(),
        ..Layout::default()
    }
}

fn tool(id: u64, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}})
}

/// Whether a tool's answer is an error, and what it says.
fn said(answer: &Value) -> (bool, String) {
    (
        answer["result"]["isError"] == true,
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

/// The device the shell says beside an agent that came from `cert`'s: its key and its name.
fn device_of(cert: &DeviceCert) -> (String, String) {
    (
        hex::encode(cert.device_public_key),
        cert.device_label.clone(),
    )
}

/// Whether Notes took `taken` as a step of the agent session `session`.
fn a_step_of(taken: &runtime_collab::Invocation, session: &str) -> bool {
    let context: Value = serde_json::from_str(&taken.context_json).unwrap_or_default();
    context["group"]["kind"].as_str() == Some(tilo_actions::context::AGENT_SESSION_GROUP)
        && context["group"]["id"].as_str() == Some(session)
}

/// The device Notes was told the agent that took `taken` works from.
fn told_device(taken: &runtime_collab::Invocation) -> (String, String) {
    let context: Value = serde_json::from_str(&taken.context_json).unwrap_or_default();
    let said = |at: &str| {
        context
            .pointer(at)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (said("/invoker/device/id"), said("/invoker/device/name"))
}

/// Notes' side: join the workspace as the app the session launched, publish its notebook, say so
/// on `published`, then hand each invocation it takes on to `took`.
fn notes(
    app_wire: &std::path::Path,
    workspace: &str,
    (key, cert): (&SigningKey, &DeviceCert),
    published: &mpsc::Sender<()>,
    took: &mpsc::Sender<runtime_collab::Invocation>,
) {
    use ed25519_dalek::Signer as _;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream = UnixStream::connect(app_wire).expect("the app wire's socket");
        launcher::services::write_collab_handoff(&mut stream, NOTES_APP, workspace)
            .expect("the launcher names the app on the connection");
        stream.set_nonblocking(true).unwrap();
        let stream = tokio::net::UnixStream::from_std(stream).unwrap();
        let (reader, writer) = tokio::io::split(stream);
        let (client, carrying) = runtime_collab::CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.to_owned(),
            device_certificate: serde_json::to_vec(cert).unwrap(),
            proof: key
                .sign(&tilo_identity_client::collaboration_join_digest(workspace).unwrap())
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        let channel = tilo_workspace_service::apps::route::CrdtRoute {
            app_id: NOTES_APP.into(),
            cluster_id: workspace.into(),
            document_id: collab_types::person_document_id(NOTEBOOK),
        }
        .channel_key();
        let mine = client
            .join_channel(&channel, &presentation)
            .await
            .expect("the app the session launched joins the workspace's channel");
        let invocations = mine.subscribe_stable_invocations().await.unwrap();
        mine.publish_actions(&actions()).await.unwrap();
        let directory: runtime_collab::wire::client::Answerer =
            Arc::new(|ask| Box::pin(std::future::ready(directory_answer(ask))));
        mine.publish_action_directory(directory).await.unwrap();
        let _ = published.send(());
        while let Ok(invocation) = invocations.read().await {
            if mine
                .complete_invocation(invocation.id, "", "")
                .await
                .is_err()
                || took.send(invocation).is_err()
            {
                return;
            }
        }
    });
}

/// Notes' actions on its notebook: `open`, which reaches outside it, and `archive`, a change to its
/// moderation, which an agent's grants never allow.
fn actions() -> [runtime_collab::ExportedAction; 2] {
    use runtime_collab::{ExportedAction, ExportedInput};
    use tilo_actions::{ActionContract, ActionKind, OperationKind};
    let action = |id: &str, label: &str, contract: ActionContract| ExportedAction {
        resource: NOTEBOOK.into(),
        semantic_identity: None,
        action: id.into(),
        resource_kind: NOTEBOOK.into(),
        resource_label: "Notebook".into(),
        action_label: label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        state: tilo_actions::ActionState::default(),
        operation_kind: OperationKind::Effect,
        params: Vec::new(),
        contract,
        view: None,
        input: ExportedInput::None,
        did: None,
    };
    [
        action(
            "open",
            "Open",
            ActionContract {
                kind: ActionKind::Outside,
                ..ActionContract::default()
            },
        ),
        action(
            "archive",
            "Archive",
            ActionContract {
                kind: ActionKind::Undoable,
                field: Some(tilo_action_policy::roles::MODERATION_FIELD.into()),
                ..ActionContract::default()
            },
        ),
    ]
}

/// Notes' directory, as it answers the host: its notebook, with its actions on it.
fn directory_answer(
    ask: runtime_collab::wire::Ask,
) -> Result<runtime_collab::wire::Answer, String> {
    use runtime_collab::wire::{Answer, Ask};
    use runtime_collab::{ActionDirectoryPage, ActionDirectoryResolution, ActionDirectoryRow};
    let row = |action: runtime_collab::ExportedAction| ActionDirectoryRow {
        parent_resource: String::new(),
        resource: NOTEBOOK.into(),
        resource_kind: NOTEBOOK.into(),
        resource_label: "Notebook".into(),
        presentation: None,
        action: Some(action.action.clone()),
        visibility: Some(runtime_collab::RowVisibility {
            action: action.action.clone(),
            contract: action.contract.clone(),
            governing_kind: Some(action.operation_kind),
        }),
        descriptor: Some(action),
        has_children: false,
    };
    match ask {
        Ask::Query { query, .. } => {
            let mut rows: Vec<ActionDirectoryRow> = Vec::new();
            if query.parent_resource.is_empty()
                && [String::new(), NOTEBOOK.into()].contains(&query.resource_name)
            {
                let [open, archive] = actions();
                let bare = ActionDirectoryRow {
                    action: None,
                    descriptor: None,
                    ..row(open.clone())
                };
                rows = vec![row(open), row(archive), bare];
            }
            let from = std::str::from_utf8(&query.cursor)
                .ok()
                .and_then(|cursor| cursor.parse::<usize>().ok())
                .unwrap_or(0);
            let to = rows
                .len()
                .min(from.saturating_add(usize::try_from(query.max_results).unwrap_or(1)));
            let more = to < rows.len();
            serde_json::to_string(&ActionDirectoryPage {
                revision: "r1".into(),
                rows: rows.get(from..to).unwrap_or_default().to_vec(),
                next_cursor: if more {
                    to.to_string().into_bytes()
                } else {
                    Vec::new()
                },
                exhausted: !more,
                stale_cursor: false,
                budget_exhausted: false,
            })
            .map(Answer::Page)
            .map_err(|error| error.to_string())
        }
        Ask::Resolve {
            resource, action, ..
        } => {
            let found = actions()
                .into_iter()
                .find(|published| resource == NOTEBOOK && published.action == action)
                .map_or(ActionDirectoryResolution::NotFound, |published| {
                    ActionDirectoryResolution::Found {
                        row: Box::new(row(published)),
                        revision: Some("r1".into()),
                    }
                });
            serde_json::to_string(&found)
                .map(Answer::Resolution)
                .map_err(|error| error.to_string())
        }
    }
}

/// While this computer does not accept agents from the person's other devices, the laptop's agent
/// is refused, saying which setting would let it in, with nobody asked and nothing listed. Let in,
/// it works here as the person's agent member, listed with the laptop beside its name, until the
/// person stops it here: from then on it is refused, saying so.
#[test]
fn an_agent_from_another_device_works_here_only_while_let_in_and_until_stopped() {
    let _settled = ui::motion::testing::settled();
    let mut shell = ShellWithNotes::start();
    let agent = shell.agent();

    let refused = shell.ask(&agent, tool(1, "tilo_ls", &json!({})));
    let (error, why) = said(&refused);
    assert!(error, "{refused}");
    assert!(
        why.contains("accept agents from your other devices"),
        "the refusal names the setting: {why}"
    );
    assert!(shell.asked().is_empty(), "nobody is asked");
    assert_eq!(listed(&shell.session, CLIENT), None, "nobody works here");

    shell.set(
        "set-accept-own-devices",
        settings_store::mcp_server::accepts_own_devices,
    );
    let apps = shell.ask(&agent, tool(2, "tilo_ls", &json!({})));
    assert!(!said(&apps).0, "{apps}");
    assert!(shell.asked().is_empty(), "let in without a question");
    let (session, device, device_name) =
        listed(&shell.session, CLIENT).expect("the agent works here");
    assert_eq!((device, device_name), device_of(&shell.laptop.1));

    shell
        .session
        .invoke(
            "session:agents",
            "stop-agent",
            json!({"agent": session}).as_object().unwrap(),
        )
        .expect("the person stops the agent");
    crate::common::until(&mut shell.session, "the agent gone", |session| {
        listed(session, CLIENT).is_none()
    });
    let stopped = shell.ask(&agent, tool(3, "tilo_ls", &json!({})));
    let (error, why) = said(&stopped);
    assert!(error, "{stopped}");
    assert!(why.contains("You stopped this agent"), "{why}");
}

/// Let in, the laptop's agent is held to its grants here: a step they do not allow is refused with
/// nobody asked, and a step they leave to the person is put to the person on this computer's
/// sheet while the agent waits; Allow once lets it go on under the agent's session here, and Notes
/// is told the laptop is where the agent works from.
#[test]
fn an_agent_from_another_device_is_held_to_its_grants_and_its_person_asked_here() {
    let _settled = ui::motion::testing::settled();
    let mut shell = ShellWithNotes::start();
    shell.set(
        "set-accept-own-devices",
        settings_store::mcp_server::accepts_own_devices,
    );
    let agent = shell.agent();

    let archived = shell.ask(
        &agent,
        tool(1, "tilo_do", &json!({"path": "notes/Notebook#archive"})),
    );
    assert!(said(&archived).0, "an agent moderates nothing: {archived}");
    assert!(shell.asked().is_empty(), "nobody is asked");
    assert!(shell.taken.try_recv().is_err(), "nothing was archived");

    let sent = shell.sent(
        &agent,
        tool(2, "tilo_do", &json!({"path": "notes/Notebook#open"})),
    );
    crate::common::until(
        &mut shell.session,
        "the person asked about the step",
        |_| !shell.questions.pending().is_empty(),
    );
    assert_eq!(shell.asked(), [STEP_QUESTION]);
    for _ in 0..200 {
        shell.session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(
        !sent.is_finished(),
        "the agent waits while the person is asked"
    );
    assert!(shell.taken.try_recv().is_err(), "nothing was opened yet");

    // Allow once.
    crate::common::press_sheet(&mut shell.session, PortalEffect::CONFIRM.id, &json!({}));
    let opened = shell.answer(sent);
    assert!(!said(&opened).0, "{opened}");
    let taken = shell
        .taken
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("Notes took the step");
    assert_eq!(taken.action, "open");
    let (session, ..) = listed(&shell.session, CLIENT).expect("the agent works here");
    assert!(a_step_of(&taken, &session), "{}", taken.context_json);
    assert_eq!(told_device(&taken), device_of(&shell.laptop.1));
    assert!(shell.asked().is_empty());
}

/// With their steps let go unasked as well, a step the laptop's agent would ask the person about
/// is allowed by the setting: no sheet goes up, and the step is the agent's session's.
#[test]
fn with_their_steps_let_go_unasked_an_agent_from_another_device_acts_without_a_question() {
    let _settled = ui::motion::testing::settled();
    let mut shell = ShellWithNotes::start();
    shell.set(
        "set-accept-own-devices",
        settings_store::mcp_server::accepts_own_devices,
    );
    shell.set(
        "set-act-without-asking",
        settings_store::mcp_server::acts_without_asking,
    );
    let agent = shell.agent();
    let opened = shell.ask(
        &agent,
        tool(1, "tilo_do", &json!({"path": "notes/Notebook#open"})),
    );
    assert!(!said(&opened).0, "{opened}");
    assert!(shell.asked().is_empty(), "nobody was asked");
    let taken = shell
        .taken
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("Notes took the step");
    assert_eq!(taken.action, "open");
    let (session, ..) = listed(&shell.session, CLIENT).expect("the agent works here");
    assert!(a_step_of(&taken, &session), "{}", taken.context_json);
}
