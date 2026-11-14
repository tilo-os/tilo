//! An agent on another of the person's devices at this desktop's agent door,
//! over iroh: the door this session answers on (`Session::remote_door`, what
//! its endpoint serves on `tilo_runtime::remote_door::ALPN`) admits only the
//! person's own devices, and an agent from one is asked about on the same
//! sheet as a program of this login — named after the device it came from —
//! unless the person let agents from their other devices in on this device.
//! Nobody answering admits nothing, the agent leaving withdraws the question,
//! and the person's Stop on its seat ends its connection. Once in, a step its
//! run asks about is put to the person on the same sheet, unless they let such
//! agents act without asking on this device too.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_actions::remote::PendingPoll;
use tilo_actions::remote::workspace_activity as wire;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_peer_state::owner::{self, testing};
use tilo_peer_state::{PersonalStore, PersonalSync, TransportOrigin};
use tilo_runtime::remote_door::{ALPN, PersonalDevices, agent_id, open};
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, IdentityPaths, RevocationEntry, RevocationReason,
    persist, sign_device_cert, sign_revocation_entry,
};

use crate::common::private_home;

/// The run the agent names its calls with, as a client does: what `undo task` takes back.
const RUN: &str = "visa-run-1";
/// The run's name, as a person reads it where the run is shown.
const RUN_TITLE: &str = "Visa agent";
/// A later run of the same agent, which the person takes back.
const LATER_RUN: &str = "visa-run-2";

/// A device key `n` certified by `root` , called `label` .
fn device(root: &SigningKey, n: u8, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A localhost endpoint with no relay whose identity is the device `key` .
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

/// The person, this box (B) and their laptop (A), where the agent runs; and a phone of theirs they
/// removed.
struct Person {
    root: SigningKey,
    laptop: (SigningKey, DeviceCert),
    this_box: (SigningKey, DeviceCert),
    lost: (SigningKey, DeviceCert),
}

impl Person {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[0x11; 32]);
        Self {
            laptop: device(&root, 1, "Studio laptop"),
            this_box: device(&root, 2, "Build box"),
            lost: device(&root, 3, "Lost phone"),
            root,
        }
    }

    /// The person's own store as this box keeps it: the laptop admitted, the phone revoked.
    fn store(&self) -> PersonalSync {
        let store = PersonalStore::open(
            self.root.verifying_key().to_bytes(),
            self.this_box.1.clone(),
            SystemTime::now(),
        )
        .unwrap();
        let sync = PersonalSync::new(store, self.this_box.0.clone());
        let mut entry = RevocationEntry {
            user_public_key: self.root.verifying_key().to_bytes(),
            revoked_device_public_key: self.lost.1.device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &self.root).unwrap();
        sync.admit(|store, now| {
            store.admit_device(&self.laptop.1, now)?;
            store.revoke(entry)
        })
        .unwrap();
        sync
    }
}

/// This box: its headless session with the agent door open, the questions it asks the person, and
/// its endpoint answering the door toward the person's other devices.
struct ThisBox {
    session: Session,
    questions: Questions,
    socket: std::path::PathBuf,
    runtime: tokio::runtime::Runtime,
    addr: EndpointAddr,
    _endpoint: Endpoint,
    _router: Router,
    _root: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

impl ThisBox {
    fn start(person: &Person) -> Self {
        let home = tilo_test_env::TestHome::new().expect("a home");
        let root = private_home();
        let socket = root.path().join("agent.sock");
        let questions = Questions::new();
        let session = crate::common::headless(Options {
            questions: Some(questions.clone()),
            collab: false,
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(socket.clone()),
            state: Some(home.state().to_path_buf()),
            // The person's settings, as the session serves them: what the person sets this
            // box's agent door with.
            services: vec![std::boxed::Box::new(
                tilo_settings_service::SettingsService::new(),
            )],
            ..Options::default()
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let door = session.remote_door(Arc::new(PersonalDevices::new(person.store())));
        let (endpoint, router) = runtime.block_on(async {
            let endpoint = endpoint(&person.this_box.0).await;
            let router = Router::builder(endpoint.clone()).accept(ALPN, door).spawn();
            (endpoint, router)
        });
        Self {
            session,
            questions,
            socket,
            addr: addr(&endpoint),
            runtime,
            _endpoint: endpoint,
            _router: router,
            _root: root,
            _home: home,
        }
    }

    /// Dial this box's door from the device `key`, showing `certificate`,
    /// asking for `workspace`.
    fn dial(
        &self,
        (key, certificate): &(SigningKey, DeviceCert),
        workspace: Option<&str>,
    ) -> Result<Agent, String> {
        let endpoint = self.runtime.block_on(endpoint(key));
        let dialed = self.runtime.block_on(open(
            &endpoint,
            self.addr.clone(),
            certificate,
            workspace,
            false,
        ))?;
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs.set_nonblocking(true).unwrap();
        let theirs = {
            let _entered = self.runtime.enter();
            tokio::net::UnixStream::from_std(theirs).unwrap()
        };
        self.runtime.spawn(dialed.carry(theirs));
        Ok(Agent::over(ours, Some(endpoint)))
    }

    /// Run the session until `done` holds.
    #[track_caller]
    fn pump(&mut self, mut done: impl FnMut(&Self) -> bool) {
        tilo_test_env::wait_for("what the test asked for", || {
            self.session.step(Duration::from_millis(2)).expect("step");
            done(self).then_some(())
        });
    }

    /// Run the session for `steps` passes, whatever happens.
    fn idle(&mut self, steps: usize) {
        for _ in 0..steps {
            self.session.step(Duration::from_millis(2)).expect("step");
        }
    }

    /// The answer `agent` reads to what it asked, while the session runs.
    #[track_caller]
    fn answer(&mut self, agent: &Agent) -> Option<Value> {
        crate::common::wait_for(&mut self.session, "the agent's answer", |_| {
            agent.answers.try_recv().ok()
        })
    }

    /// Press `effect` on the sheet up, as the person would.
    fn press(&mut self, effect: &str, args: &Value) {
        crate::common::press_sheet(&mut self.session, effect, args);
    }

    /// The agents the desk seats now, by the key its presence names each.
    fn seated(&self) -> Vec<String> {
        self.session
            .read("session:agents")
            .and_then(|agents| agents.get("seated").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|agent| agent["seat"] == true)
            .map(|agent| agent["key"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// What the sheet up says: who asks, and what for.
    fn asked(&self) -> (String, String) {
        let dialog = self
            .session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .expect("a question is up");
        (dialog.heading, dialog.title)
    }
}

/// An agent's MCP client: what it sends goes out on its own thread, and each answer it reads comes
/// back — `None` once its connection ended.
struct Agent {
    asks: mpsc::Sender<Value>,
    answers: mpsc::Receiver<Option<Value>>,
    /// Its connection, which leaving shuts.
    stream: UnixStream,
    _endpoint: Option<Endpoint>,
}

impl Agent {
    fn over(stream: UnixStream, endpoint: Option<Endpoint>) -> Self {
        let (asks, asked) = mpsc::channel::<Value>();
        let (answered, answers) = mpsc::channel();
        let mut writer = stream.try_clone().unwrap();
        let reader = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            while let Ok(ask) = asked.recv() {
                if writer.write_all(format!("{ask}\n").as_bytes()).is_err() {
                    let _ = answered.send(None);
                    return;
                }
                let mut line = String::new();
                let answer = match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => serde_json::from_str(&line).ok(),
                };
                let ended = answer.is_none();
                if answered.send(answer).is_err() || ended {
                    return;
                }
            }
        });
        Self {
            asks,
            answers,
            stream,
            _endpoint: endpoint,
        }
    }

    fn send(&self, ask: Value) {
        self.asks.send(ask).unwrap();
    }

    /// Leave, as closing the agent does.
    fn leave(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"Claude Code","version":"2.1"}}})
}

fn tool(id: u64, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}})
}

/// A tool call that names the run it is a step of, as a client does so what it changes is one
/// run's: `undo task` takes the whole run back.
fn tool_in_run(id: u64, run: &str, title: &str, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":name,"arguments":arguments,
        "_meta":{"so.tilo.mcp":{"run":{"id":run,"title":title}}}}})
}

#[test]
fn an_agent_from_another_device_waits_for_the_person_and_leaving_withdraws_it() {
    let _settled = ui::motion::testing::settled();
    let person = Person::new();
    let mut b = ThisBox::start(&person);
    // It asks for a workspace this box does not hold: the person is asked about the one in front,
    // as for any other client.
    let agent = b
        .dial(&person.laptop, Some("dddddddd-dddd-4ddd-8ddd-dddddddddddd"))
        .expect("this box's door opens for the person's laptop");
    agent.send(initialize());
    b.pump(|b| b.questions.pending().len() == 1);
    let (who, what) = b.asked();
    assert_eq!(who, "Claude Code, from your device Studio laptop");
    // Each connection the person admits is an agent of its own: the device's agent, first of it.
    let named = format!("{}#1", agent_id("claude-code", &person.laptop.1));
    // The desk keeps no name for a workspace nobody named: the question says the word for one.
    assert_eq!(what, format!("Work in “Workspace” as the agent {named}"));

    // Nobody is at this box: nothing is answered, and nothing is admitted.
    b.idle(200);
    assert!(
        agent.answers.try_recv().is_err(),
        "the agent reads nothing before the person answers"
    );
    assert!(b.seated().is_empty(), "nobody works here");

    // The agent leaves: its question is withdrawn.
    agent.leave();
    b.pump(|b| b.questions.pending().is_empty());
    assert!(b.seated().is_empty());
}

#[test]
fn an_agent_the_person_lets_in_works_here_as_their_agent_via_its_device_until_they_stop_it() {
    let _settled = ui::motion::testing::settled();
    let person = Person::new();
    let mut b = ThisBox::start(&person);
    let agent = b.dial(&person.laptop, None).unwrap();
    agent.send(initialize());
    b.pump(|b| b.questions.pending().len() == 1);
    // Allow for this session.
    b.press(PortalEffect::SELECT.id, &json!({"index": 1}));
    b.press(PortalEffect::CONFIRM.id, &json!({}));
    let introduced = b.answer(&agent).expect("the agent is served");
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );
    let named = format!("{}#1", agent_id("claude-code", &person.laptop.1));
    b.pump(|b| b.seated() == [named.clone()]);

    // The same tools as a program of this login, and the person's devices.
    agent.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = b.answer(&agent).expect("the tools");
    let offered: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for wanted in [
        "tilo_ls",
        "tilo_read",
        "tilo_do",
        "tilo_find",
        "tilo",
        "tilo_status",
        "tilo_devices",
    ] {
        assert!(offered.contains(&wanted), "{wanted} among {offered:?}");
    }
    agent.send(tool(3, "tilo_ls", &json!({"path": "session"})));
    let listed = b.answer(&agent).expect("a listing");
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    agent.send(tool(
        4,
        "tilo_do",
        &json!({"path": "session/Session diagnostics#profile-status"}),
    ));
    let done = b.answer(&agent).expect("an answer");
    assert_eq!(done["result"]["isError"], false, "{done}");

    // The person's Stop on its seat ends the connection on the other device.
    b.session
        .invoke(
            "session:agents",
            "stop-agent",
            json!({"agent": named}).as_object().unwrap(),
        )
        .expect("the person stops the agent");
    b.pump(|b| b.seated().is_empty());
    agent.send(tool(5, "tilo_ls", &json!({})));
    assert_eq!(b.answer(&agent), None, "the agent's connection ended");
}

#[test]
fn with_agents_from_other_devices_accepted_one_works_here_without_a_question() {
    let _settled = ui::motion::testing::settled();
    let person = Person::new();
    let mut b = ThisBox::start(&person);
    // The person lets agents from their other devices in on this box, as Settings, the palette or
    // an agent of theirs does: the setting's own verb.
    b.session
        .invoke(
            "settings:settings.agent-door",
            "set-accept-own-devices",
            json!({"on": true}).as_object().unwrap(),
        )
        .expect("the setting is kept");
    b.pump(|_| settings_store::agent_door::accepts_own_devices(&settings_store::load()));

    let agent = b.dial(&person.laptop, None).unwrap();
    agent.send(initialize());
    let introduced = b.answer(&agent).expect("the agent is served");
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );
    assert!(b.questions.pending().is_empty(), "nobody was asked");
    let named = format!("{}#1", agent_id("claude-code", &person.laptop.1));
    b.pump(|b| b.seated() == [named.clone()]);

    // The setting lets in the person's own devices and nothing else: a device of another person,
    // and one the person removed, are refused.
    let stranger = SigningKey::from_bytes(&[0x22; 32]);
    let theirs = device(&stranger, 4, "Their laptop");
    let refused = b.dial(&theirs, None).map(|_| ()).unwrap_err();
    assert!(refused.contains("someone else"), "{refused}");
    let refused = b.dial(&person.lost, None).map(|_| ()).unwrap_err();
    assert!(refused.contains("removed"), "{refused}");

    // Nor does it reach a program of this login, which is still asked about.
    let local = Agent::over(UnixStream::connect(&b.socket).unwrap(), None);
    local.send(initialize());
    b.pump(|b| b.questions.pending().len() == 1);
    assert!(
        b.asked().0.starts_with("Claude Code ("),
        "a program of this login: {:?}",
        b.asked()
    );
    local.leave();
    b.pump(|b| b.questions.pending().is_empty());
    assert_eq!(b.seated(), [named]);
}

/// This computer's own identity, as a desktop has one: a root in the person's record, the device
/// key and the certificate that root certified kept in a private directory of the test's, the
/// device enrolled in its identity's fleet, and the workspace its desk holds — the personal
/// cluster of that identity, which needs no other name to be found.
struct ThisComputer {
    /// The key this device signs its workspace with.
    key: SigningKey,
    /// The certificate this device presents.
    cert: DeviceCert,
    /// The person's root, signing here as it does at their own desktop.
    root: runtime_identity::SigningIdentity,
    /// The workspace this device holds, by id.
    workspace: String,
    /// Where the session keeps what it holds.
    state: tempfile::TempDir,
    /// The home whose environment names all of it; dropped last.
    _home: tilo_test_env::TestHome,
}

impl ThisComputer {
    fn new() -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let system = home.root().join("system-identity");
        let state = tempfile::tempdir().expect("a state directory");
        home.env()
            .set("TILO_IDENTITY_ROOT", &system)
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        let paths = IdentityPaths::under(&system);
        let identity_dir = home.config().join("tilo/identity");
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .expect("this computer has an identity of its own");
        let key = persist::load_device_key(&paths).expect("this device's key");
        let cert =
            persist::read_device_cert(&paths.device_cert).expect("this device's certificate");
        let record = runtime_identity::resolve_active_identity(&identity_dir)
            .expect("this identity's record");
        let root =
            runtime_identity::load_from_path(record).expect("the person's root signs on this box");
        let workspace = collab_types::personal_cluster(&cert.user_public_key).to_string();
        workspace_held_here(state.path(), &workspace, &cert, &key);
        Self {
            key,
            cert,
            root,
            workspace,
            state,
            _home: home,
        }
    }

    /// The person's own store as this box keeps it: the devices and keys the door admits.
    fn store(&self) -> PersonalSync {
        let store = PersonalStore::open(
            self.cert.user_public_key,
            self.cert.clone(),
            SystemTime::now(),
        )
        .expect("this box's store of the person's own");
        PersonalSync::new(store, self.key.clone())
    }
}

/// This computer's copy of the workspace `workspace` under `state`, held as `cert`'s device in a
/// workspace the same device governs: the state a session started on this directory finds waiting
/// for it.
fn workspace_held_here(
    state: &std::path::Path,
    workspace: &str,
    cert: &DeviceCert,
    key: &SigningKey,
) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
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
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// A desk showing the workspace `workspace`, known by its id, owned by `person`.
fn desk(workspace: &str, person: &str) -> Layout {
    let id = WorkspaceId::parse(workspace).expect("a workspace id");
    let mut held = Workspace::new(id, "Desk");
    held.set_owner(person);
    Layout {
        workspaces: vec![held.clone()],
        active: held.id(),
        ..Layout::default()
    }
}

/// The steps of the record the notebook `book` is the root of, as History lists them to the
/// person reading it, oldest first: each with the run it is a step of, carried from the step
/// before where History leaves it out.
fn history(session: &mut Session, book: &str) -> Vec<Value> {
    let read = session
        .invoke(book, "history", &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("the notebook's history: {error:?}"));
    let ui::host_operation::ResourceOperationStart::Completed(read) = read else {
        panic!("History answers at once");
    };
    let read = read.value.expect("History lists the record's steps");
    let mut carried = serde_json::Map::new();
    read["versions"]
        .as_array()
        .expect("the record's steps")
        .iter()
        .map(|version| {
            let mut version = version.as_object().cloned().unwrap_or_default();
            for key in ["run", "run_id", "by"] {
                match version.get(key) {
                    Some(value) => {
                        carried.insert(key.to_owned(), value.clone());
                    }
                    None => {
                        if let Some(value) = carried.get(key) {
                            version.insert(key.to_owned(), value.clone());
                        }
                    }
                }
            }
            Value::Object(version)
        })
        .collect()
}

/// The run the newest step that retitled a note to `title` is one of, as History names it: its
/// title and its id.
fn run_of_retitle(session: &mut Session, book: &str, title: &str) -> (String, String) {
    let steps = history(session, book);
    let step = steps
        .iter()
        .rev()
        .find(|step| step.pointer("/what/args/title").and_then(Value::as_str) == Some(title))
        .unwrap_or_else(|| panic!("History lists the retitle to {title}: {steps:?}"));
    let named = |key: &str| {
        step.get(key)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the retitle to {title} is a run's step: {step}"))
            .to_owned()
    };
    (named("run"), named("run_id"))
}

/// A grant for an agent the person's root certified: reaching all their workspaces, ending in an
/// hour, and on no device of theirs — the root made it, and that is all. Its key and certificate.
fn agent_grant(computer: &ThisComputer) -> (SigningKey, DeviceCert) {
    let agent_key = SigningKey::from_bytes(&[0x44; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut grant = DeviceCert {
        user_public_key: computer.cert.user_public_key,
        device_public_key: agent_key.verifying_key().to_bytes(),
        device_label: "Visa script".into(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    grant.signature = computer
        .root
        .sign(&tilo_user_identity::device_cert_signing_digest(&grant).expect("the grant's digest"))
        .expect("the person's root signs the grant");
    tilo_user_identity::verify_device_cert(&grant).expect("a certified grant");
    (agent_key, grant)
}

/// The agent holding `computer`'s grant, dialing this box at `this_box` over iroh on `runtime`
/// and naming the workspace it enters: a key for all the person's workspaces names one.
fn dial_in(
    runtime: &tokio::runtime::Runtime,
    computer: &ThisComputer,
    this_box: &Endpoint,
) -> Agent {
    let (agent_key, grant) = agent_grant(computer);
    let dialing = runtime.block_on(endpoint(&agent_key));
    let dialed = runtime
        .block_on(open(
            &dialing,
            addr(this_box),
            &grant,
            Some(&computer.workspace),
            false,
        ))
        .expect("the desktop's door opens for the person's key");
    let (ours, theirs) = UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    let theirs = {
        let _entered = runtime.enter();
        tokio::net::UnixStream::from_std(theirs).unwrap()
    };
    runtime.spawn(dialed.carry(theirs));
    Agent::over(ours, Some(dialing))
}

/// The agent's next answer, the session running while it waits.
#[track_caller]
fn answer(session: &mut Session, agent: &Agent) -> Value {
    crate::common::wait_for(session, "the agent's answer", |_| {
        agent.answers.try_recv().ok()
    })
    .expect("an answer")
}

/// The text of a tool's answer.
fn text(answer: &Value) -> String {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// What the note `note` reads as, as the agent's own listing names it: the app's own title.
fn listed(session: &mut Session, agent: &Agent, note: &str) -> String {
    agent.send(tool(
        9,
        "tilo_ls",
        &json!({"path": format!("session/{note}")}),
    ));
    text(&answer(session, agent))
}

/// The agent retitles `note` to `title` through `rename` (the note's own action, as its listing
/// names it) as a step of the run `run`, and the listing reads the new title.
fn retitle(
    session: &mut Session,
    agent: &Agent,
    (note, rename): (&str, &str),
    run: &str,
    title: &str,
) {
    agent.send(tool_in_run(
        3,
        run,
        RUN_TITLE,
        "tilo_do",
        &json!({"path": format!("session/{note}{rename}"), "args": {"title": title}}),
    ));
    let renamed = answer(session, agent);
    assert_eq!(renamed["result"]["isError"], false, "{renamed}");
    crate::common::until(session, "the note retitled", |session| {
        listed(session, agent, note).starts_with(&format!("session/Notes/{title} "))
    });
}

/// The note `note` reads as it did before the agent came: its first title.
fn back_to_its_title(session: &mut Session, agent: &Agent, note: &str) {
    crate::common::until(session, "the note's title back", |session| {
        listed(session, agent, note)
            .starts_with(&format!("session/Notes/{} ", crate::common::NOTE_TITLE))
    });
}

/// A key elsewhere — an agent's grant the person's root certified, on no device of theirs —
/// enters the workspace this desktop holds over iroh, through the same door: the person lets it
/// in, and it renames a note as a step of its run, on this real session. The notes are the
/// session's own records, which no app's process holds, and still offer their History and their
/// undo: History names the run, the agent's `undo task` takes its step back, and the person's
/// undo of a later run of the agent's takes that run back through the record's own history.
#[test]
fn an_agents_step_from_elsewhere_is_undone_in_the_workspace_this_desktop_holds() {
    let _settled = ui::motion::testing::settled();
    let computer = ThisComputer::new();
    let seated = hex::encode(computer.cert.user_public_key);
    let root = private_home();
    let questions = Questions::new();
    // The notes this person keeps here, one of them the test renames and reads back.
    let (notebook, notes) = crate::common::notebook_of(&seated, &[crate::common::NOTE_TITLE]);
    let note = notes.into_iter().next().expect("a note");
    let book = tilo_service::Service::resources(&notebook)
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{}.Notebook", crate::common::NOTES))
        .expect("the notebook")
        .id
        .as_str()
        .to_owned();
    let mut session = crate::common::headless(Options {
        person: seated.clone(),
        collab: true,
        state: Some(computer.state.path().to_path_buf()),
        questions: Some(questions.clone()),
        action_plane: ActionPlane::At(root.path().join("plane.sock")),
        agent_door: Some(root.path().join("agent.sock")),
        services: vec![Box::new(notebook)],
        ..Options::default()
    });
    // The desk shows the workspace this computer holds, as its own desktop does.
    session
        .handle()
        .replace_tree(desk(&computer.workspace, &seated));
    crate::common::until(
        &mut session,
        "the workspace this computer holds",
        |session| {
            session
                .conversation_runs()
                .requires_workspace(&computer.workspace)
        },
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let door = session.remote_door(Arc::new(PersonalDevices::new(computer.store())));
    let (this_box, _router) = runtime.block_on(async {
        let endpoint = endpoint(&computer.key).await;
        let router = Router::builder(endpoint.clone()).accept(ALPN, door).spawn();
        (endpoint, router)
    });
    let agent = dial_in(&runtime, &computer, &this_box);

    // The person lets it in for this session.
    agent.send(initialize());
    crate::common::until(&mut session, "the person asked", |_| {
        questions.pending().len() == 1
    });
    crate::common::press_sheet(&mut session, PortalEffect::SELECT.id, &json!({"index": 1}));
    crate::common::press_sheet(&mut session, PortalEffect::CONFIRM.id, &json!({}));
    let introduced = answer(&mut session, &agent);
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );

    // It renames the note through the note's own action, as the listing names it.
    let actions = listed(&mut session, &agent, &note);
    let rename = actions
        .lines()
        .find(|line| line.trim_end().ends_with("Rename…"))
        .and_then(|line| line.split('(').next())
        .unwrap_or_else(|| panic!("the note's rename: {actions}"))
        .trim()
        .to_owned();
    retitle(&mut session, &agent, (&note, &rename), RUN, "Shopping");

    // History names the run: the notes' record lists the rename as a step of the agent's run,
    // though no app's process holds the record. The run is named by the agent the person let
    // in, as this desktop knows it, whatever the client calls its run.
    let (title, _) = run_of_retitle(&mut session, &book, "Shopping");
    assert!(
        title.starts_with("Claude Code"),
        "History names the run by the agent let in: {title}"
    );

    // `undo task` takes the run's step back in the record it changed.
    agent.send(tool_in_run(
        4,
        RUN,
        RUN_TITLE,
        "tilo",
        &json!({"program": "undo task"}),
    ));
    let undone = answer(&mut session, &agent);
    assert_eq!(undone["result"]["isError"], false, "{undone}");
    back_to_its_title(&mut session, &agent, &note);

    // A later run renames it again, and the person takes that run back from the Activity: the run
    // is listed there although no app's process holds the note, and its Undo run asks each record
    // the run changed for its own undo of it — the session keeps that record, so the session's
    // provider is what answers.
    retitle(&mut session, &agent, (&note, &rename), LATER_RUN, "Errands");
    let (title, run) = run_of_retitle(&mut session, &book, "Errands");
    assert!(
        title.starts_with("Claude Code"),
        "History names the later run: {title}"
    );
    let listed = listed_run(&mut session, &computer.workspace, &seated, &run);
    undo_from_activity(&mut session, &listed);
    back_to_its_title(&mut session, &agent, &note);
    agent.leave();
}

/// The run the Activity lists as `named` (the id History names it by), as the range its Undo run
/// takes back: read through the model a window of the Activity follows.
fn listed_run(session: &mut Session, workspace: &str, person: &str, named: &str) -> wire::Range {
    let requests = session.activity();
    let caller = tilo_actions::remote::Caller::local_person(person).in_workspace(workspace);
    let following = tilo_actions::InvocationId::new();
    requests
        .follow(
            &caller,
            following,
            wire::RUNS,
            query(&wire::RunsQuery::default()),
        )
        .expect("the Activity follows its runs");
    let listed: wire::Runs = activity_read(session, &requests, &following, "the runs are listed");
    let run = listed
        .runs
        .iter()
        .find(|run| run.id == named)
        .unwrap_or_else(|| {
            panic!("the Activity lists the run History names ({named}): {listed:?}")
        });
    assert_eq!(
        run.undo_blocked, None,
        "the run is undone from the Activity: {run:?}"
    );
    run.range.clone()
}

/// The person presses Undo run on `range` in the Activity, and the model takes it.
fn undo_from_activity(session: &mut Session, range: &wire::Range) {
    let mut asked = session.activity().act(wire::UNDO_RUN, query(range));
    crate::common::wait_for(session, "the Activity takes the undo", |_| {
        match asked.poll_ready() {
            PendingPoll::Ready(Ok(())) => Some(()),
            PendingPoll::Ready(Err(error)) => panic!("the Activity refuses the undo: {error}"),
            PendingPoll::Waiting => None,
            PendingPoll::Abandoned => panic!("the Activity never took the undo"),
        }
    });
}

/// What the model serves the follow `following` for, once it has read it.
fn activity_read<T: serde::de::DeserializeOwned>(
    session: &mut Session,
    requests: &tilo_desktop_surface::session::ActivityRequests,
    following: &tilo_actions::InvocationId,
    what: &str,
) -> T {
    crate::common::wait_for(session, what, |_| {
        let read = requests.served(following)?.ok()?;
        serde_json::from_value(read).ok()
    })
}

/// `what` as the arguments of an Activity action: a query or a range.
fn query(what: &impl serde::Serialize) -> serde_json::Map<String, serde_json::Value> {
    serde_json::to_value(what)
        .expect("an argument")
        .as_object()
        .cloned()
        .expect("an argument object")
}

/// The app the agent's steps are taken in: Notes, as it publishes its notebook.
const NOTES_APP: &str = "so.tilo.notes";

/// The record of the person's Notes keeps in their own workspace, by its kind.
const NOTEBOOK: &str = "notes.notebook";

/// Notes' surface, as its package names it: the person's notebook, and `open`, which reaches
/// outside it.
const NOTES_SURFACE: &[u8] = b"format = 2\n\n[[resources]]\nkind = \"notes.notebook\"\nlabel = \"Notebook\"\nversion = 1\nperson = true\n\n[[resources.verbs]]\nid = \"open\"\nkind = \"outside\"\n";

/// What the run's question to the person is read as on the sheet.
const RUN_QUESTION: &str = "session.run-question";

/// Another device of the person's computer's: a laptop their root certified, which their own store
/// on this computer lists.
fn laptop_of(computer: &ThisComputer) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[0x55; 32]);
    let mut cert = DeviceCert {
        user_public_key: computer.cert.user_public_key,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Studio laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    cert.signature = computer
        .root
        .sign(&tilo_user_identity::device_cert_signing_digest(&cert).expect("the cert's digest"))
        .expect("the person's root certifies the laptop");
    tilo_user_identity::verify_device_cert(&cert).expect("a certified laptop");
    (key, cert)
}

/// This computer's desk with Notes running in the workspace it holds, and the door toward the
/// person's other devices answered on its endpoint: the session, the questions it puts to the
/// person, what the app took, and the laptop the agent runs on.
struct DeskWithNotes {
    session: Session,
    questions: Questions,
    /// Each invocation Notes was handed, as it took it.
    taken: mpsc::Receiver<runtime_collab::Invocation>,
    laptop: (SigningKey, DeviceCert),
    runtime: tokio::runtime::Runtime,
    this_box: Endpoint,
    /// The door of this login's own programs.
    socket: std::path::PathBuf,
    _router: Router,
    _root: tempfile::TempDir,
    /// Its identity and state, which the session reads while it runs; dropped last.
    computer: ThisComputer,
}

impl DeskWithNotes {
    fn start() -> Self {
        let computer = ThisComputer::new();
        // Notes' package names its surface, as the launcher does for an app it starts.
        ui::app_surface::register_package(NOTES_APP, NOTES_SURFACE);
        let seated = hex::encode(computer.cert.user_public_key);
        let root = private_home();
        let questions = Questions::new();
        let plane = root.path().join("plane.sock");
        let socket = root.path().join("agent.sock");
        // Notes speaks from this test's own process, so the launch the session would have recorded
        // for it names that process.
        let launches = Arc::new(tilo_session::Launches::default());
        launches.started(std::process::id(), NOTES_APP, &computer.workspace, true);
        let mut session = crate::common::headless(Options {
            person: seated.clone(),
            collab: true,
            state: Some(computer.state.path().to_path_buf()),
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(plane.clone()),
            agent_door: Some(socket.clone()),
            launches,
            // The person's settings, as the session serves them: what they set this computer's
            // agent door with.
            services: vec![Box::new(tilo_settings_service::SettingsService::new())],
            ..Options::default()
        });
        session
            .handle()
            .replace_tree(desk(&computer.workspace, &seated));
        crate::common::until(
            &mut session,
            "the workspace this computer holds",
            |session| {
                session
                    .conversation_runs()
                    .requires_workspace(&computer.workspace)
            },
        );

        let (published, ready) = mpsc::channel();
        let (took, taken) = mpsc::channel();
        {
            let (workspace, key, cert) = (
                computer.workspace.clone(),
                computer.key.clone(),
                computer.cert.clone(),
            );
            std::thread::spawn(move || notes(&plane, &workspace, (&key, &cert), &published, &took));
        }
        crate::common::wait_for(&mut session, "Notes publishes its notebook", |_| {
            ready.try_recv().ok()
        });

        let laptop = laptop_of(&computer);
        let store = computer.store();
        store
            .admit(|store, now| store.admit_device(&laptop.1, now))
            .expect("the person's laptop is one of their devices");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let door = session.remote_door(Arc::new(PersonalDevices::new(store)));
        let (this_box, router) = runtime.block_on(async {
            let endpoint = endpoint(&computer.key).await;
            let router = Router::builder(endpoint.clone()).accept(ALPN, door).spawn();
            (endpoint, router)
        });
        Self {
            session,
            questions,
            taken,
            laptop,
            runtime,
            this_box,
            socket,
            _router: router,
            _root: root,
            computer,
        }
    }

    /// Set the switch `verb` of this computer's agent door, as Settings, the palette or an agent of
    /// the person's does: the setting's own verb.
    fn set(&mut self, verb: &str, holds: fn(&serde_json::Map<String, Value>) -> bool) {
        self.session
            .invoke(
                "settings:settings.agent-door",
                verb,
                json!({"on": true}).as_object().unwrap(),
            )
            .expect("the setting is kept");
        crate::common::until(&mut self.session, "the setting kept", |_| {
            holds(&settings_store::load())
        });
    }

    /// The agent on `device`, dialing this computer's door and naming the workspace it holds.
    fn dial(&self, (key, certificate): &(SigningKey, DeviceCert)) -> Result<Agent, String> {
        let dialing = self.runtime.block_on(endpoint(key));
        let dialed = self.runtime.block_on(open(
            &dialing,
            addr(&self.this_box),
            certificate,
            Some(&self.computer.workspace),
            false,
        ))?;
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs.set_nonblocking(true).unwrap();
        let theirs = {
            let _entered = self.runtime.enter();
            tokio::net::UnixStream::from_std(theirs).unwrap()
        };
        self.runtime.spawn(dialed.carry(theirs));
        Ok(Agent::over(ours, Some(dialing)))
    }

    /// The questions up now, by the kind each is read as.
    fn asked(&self) -> Vec<String> {
        self.questions
            .pending()
            .into_iter()
            .map(|question| question.kind)
            .collect()
    }

    /// The run the agent seated as `seated` works in, in this desk's workspace.
    fn run_of(&self, seated: &str) -> tilo_broker_core::services::TrackerRun {
        let runs = self
            .session
            .conversation_runs()
            .runs(&self.computer.workspace)
            .unwrap_or_default();
        runs.iter()
            .find(|run| run.title == seated)
            .cloned()
            .unwrap_or_else(|| panic!("no run of {seated}: {runs:?}"))
    }
}

/// Whether Notes took `taken` as a step of `run`: what the run's history, its activity and its undo
/// are read from.
fn a_step_of(
    taken: &runtime_collab::Invocation,
    run: &tilo_broker_core::services::TrackerRun,
) -> bool {
    taken.context_json.contains(&run.issue)
}

/// Notes' side: join the workspace as the app the session launched, publish its notebook with the
/// one action that reaches outside it (`open`), say so on `published`, then take each invocation
/// the session hands it and hand it on to `took`.
fn notes(
    plane: &std::path::Path,
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
        let mut stream = UnixStream::connect(plane).expect("the plane's socket");
        launcher::services::write_collab_handoff(&mut stream, NOTES_APP, workspace)
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = tokio::io::split(stream);
        let (client, carrying) = runtime_collab::CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.to_owned(),
            device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("a presentation encodes");
        // In the person's own workspace an app takes part only in their records.
        let channel = tilo_action_broker::route::CrdtRoute {
            app_id: NOTES_APP.into(),
            cluster_id: workspace.into(),
            record_id: collab_types::person_record_id(NOTEBOOK),
        }
        .channel_key();
        let mine = client
            .join_channel(&channel, &presentation)
            .await
            .expect("the app the session launched joins the workspace's channel");
        let invocations = mine
            .subscribe_stable_invocations()
            .await
            .expect("the app listens for the invocations it runs");
        mine.publish_actions(&[open_action()])
            .await
            .expect("Notes publishes its notebook");
        // Its directory, which an agent lists and names the action by.
        let directory: runtime_collab::wire::client::Answerer =
            Arc::new(|ask| Box::pin(std::future::ready(directory_answer(ask))));
        mine.publish_action_directory(directory)
            .await
            .expect("Notes publishes its directory");
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

/// Notes' directory, as it answers the host: its notebook, with `open` on it.
fn directory_answer(
    ask: runtime_collab::wire::Ask,
) -> Result<runtime_collab::wire::Answer, String> {
    use runtime_collab::wire::{Answer, Ask};
    use runtime_collab::{ActionDirectoryPage, ActionDirectoryResolution, ActionDirectoryRow};
    let open = open_action();
    let row = ActionDirectoryRow {
        parent_resource: String::new(),
        resource: NOTEBOOK.into(),
        resource_kind: NOTEBOOK.into(),
        resource_label: "Notebook".into(),
        presentation: None,
        action: Some("open".into()),
        visibility: Some(runtime_collab::RowVisibility {
            action: "open".into(),
            contract: open.contract.clone(),
            governing_kind: Some(open.operation_kind),
        }),
        descriptor: Some(open),
        has_children: false,
    };
    match ask {
        Ask::Query { query, .. } => {
            // The notebook is all there is, at the top, and holds nothing listed further down; a
            // page holds as many rows as the host asks for, and the cursor says where the next
            // one starts.
            let rows = if query.parent_resource.is_empty()
                && [String::new(), NOTEBOOK.into()].contains(&query.resource_name)
            {
                vec![
                    row.clone(),
                    ActionDirectoryRow {
                        action: None,
                        descriptor: None,
                        ..row
                    },
                ]
            } else {
                Vec::new()
            };
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
            let found = if resource == NOTEBOOK && action == "open" {
                ActionDirectoryResolution::Found {
                    row: Box::new(row),
                    revision: Some("r1".into()),
                }
            } else {
                ActionDirectoryResolution::NotFound
            };
            serde_json::to_string(&found)
                .map(Answer::Resolution)
                .map_err(|error| error.to_string())
        }
    }
}

/// Notes' `open` on its notebook: a step that reaches outside the notebook, which an agent asks
/// its person about before it takes it.
fn open_action() -> runtime_collab::ExportedAction {
    use runtime_collab::{ExportedAction, ExportedInput};
    use tilo_actions::{ActionContract, ActionKind, OperationKind};
    ExportedAction {
        resource: NOTEBOOK.into(),
        semantic_identity: None,
        action: "open".into(),
        resource_kind: NOTEBOOK.into(),
        resource_label: "Notebook".into(),
        action_label: "Open".into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        state: tilo_actions::ActionState::default(),
        operation_kind: OperationKind::Effect,
        params: Vec::new(),
        contract: ActionContract {
            kind: ActionKind::Outside,
            ..ActionContract::default()
        },
        view: None,
        input: ExportedInput::None,
    }
}

/// With only agents from their other devices let in unasked, a step such an agent's run asks about
/// is still put to the person: the agent waits while the sheet is up, and Allow once lets the step
/// go on as a step of the agent's run.
#[test]
fn with_only_agents_from_other_devices_let_in_their_step_still_asks_the_person() {
    let _settled = ui::motion::testing::settled();
    let mut desk = DeskWithNotes::start();
    desk.set(
        "set-accept-own-devices",
        settings_store::agent_door::accepts_own_devices,
    );
    let agent = desk
        .dial(&desk.laptop)
        .expect("the door opens for the laptop");
    agent.send(initialize());
    let introduced = answer(&mut desk.session, &agent);
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );
    assert!(desk.asked().is_empty(), "let in without a question");

    agent.send(tool(2, "tilo_do", &json!({"path": "notes#open"})));
    crate::common::until(&mut desk.session, "the person asked about the step", |_| {
        !desk.questions.pending().is_empty()
    });
    assert_eq!(desk.asked(), [RUN_QUESTION], "the run's question is up");
    for _ in 0..200 {
        desk.session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(
        agent.answers.try_recv().is_err(),
        "the agent waits while the person is asked"
    );
    assert!(desk.taken.try_recv().is_err(), "nothing was opened yet");

    // Allow once.
    crate::common::press_sheet(&mut desk.session, PortalEffect::CONFIRM.id, &json!({}));
    let opened = answer(&mut desk.session, &agent);
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    let taken = desk
        .taken
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("Notes took the step");
    assert_eq!(taken.action, "open");
    let run = desk.run_of("Claude Code #1");
    assert!(
        a_step_of(&taken, &run),
        "the step is the run's: {} in {}",
        taken.context_json,
        run.issue
    );
    assert!(desk.asked().is_empty());
    agent.leave();
}

/// With their steps let go unasked as well, a step an agent from another of the person's devices
/// would ask about is allowed by the setting: no sheet goes up, and the step is taken as a step of
/// the agent's run, as one the person allowed is. A program of this login is still asked about
/// its step, and a device of someone else's is still refused at the door.
#[test]
fn with_their_steps_let_go_unasked_an_agent_from_another_device_acts_without_a_question() {
    let _settled = ui::motion::testing::settled();
    let mut desk = DeskWithNotes::start();
    desk.set(
        "set-accept-own-devices",
        settings_store::agent_door::accepts_own_devices,
    );
    desk.set(
        "set-act-without-asking",
        settings_store::agent_door::acts_without_asking,
    );
    let agent = desk
        .dial(&desk.laptop)
        .expect("the door opens for the laptop");
    agent.send(initialize());
    let introduced = answer(&mut desk.session, &agent);
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );

    agent.send(tool(2, "tilo_do", &json!({"path": "notes#open"})));
    let opened = answer(&mut desk.session, &agent);
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    assert!(desk.asked().is_empty(), "nobody was asked");
    let taken = desk
        .taken
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("Notes took the step");
    assert_eq!(taken.action, "open");
    let run = desk.run_of("Claude Code #1");
    assert!(
        a_step_of(&taken, &run),
        "the step is the run's, as one the person allowed is: {} in {}",
        taken.context_json,
        run.issue
    );

    // A program of this login is asked about, at the door and for its step.
    let local = Agent::over(UnixStream::connect(&desk.socket).unwrap(), None);
    local.send(initialize());
    crate::common::until(
        &mut desk.session,
        "the person asked about the program",
        |_| desk.questions.pending().len() == 1,
    );
    // Allow for this session.
    crate::common::press_sheet(
        &mut desk.session,
        PortalEffect::SELECT.id,
        &json!({"index": 1}),
    );
    crate::common::press_sheet(&mut desk.session, PortalEffect::CONFIRM.id, &json!({}));
    let introduced = answer(&mut desk.session, &local);
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );
    local.send(tool(2, "tilo_do", &json!({"path": "notes#open"})));
    crate::common::until(&mut desk.session, "the person asked about its step", |_| {
        !desk.questions.pending().is_empty()
    });
    assert_eq!(desk.asked(), [RUN_QUESTION], "its step is asked about");
    assert!(
        local.answers.try_recv().is_err(),
        "the program waits for the person"
    );
    // Deny.
    crate::common::press_sheet(&mut desk.session, PortalEffect::CANCEL.id, &json!({}));
    let denied = answer(&mut desk.session, &local);
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert!(desk.asked().is_empty());
    local.leave();

    // A device of someone else's is refused at the door, the settings notwithstanding.
    let stranger = SigningKey::from_bytes(&[0x22; 32]);
    let theirs = device(&stranger, 4, "Their laptop");
    let refused = desk.dial(&theirs).map(|_| ()).unwrap_err();
    assert!(refused.contains("someone else"), "{refused}");
    agent.leave();
}
