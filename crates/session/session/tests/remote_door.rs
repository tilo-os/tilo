//! An agent on another of the person's devices at this desktop's agent door,
//! over iroh: the door this session answers on (`Session::remote_door`, what
//! its endpoint serves on `tilo_runtime::remote_door::ALPN`) admits only the
//! person's own devices, and an agent from one is asked about on the same
//! sheet as a program of this login — named after the device it came from —
//! unless the person let agents from their other devices in on this device.
//! Nobody answering admits nothing, the agent leaving withdraws the question,
//! and the person's Stop on its seat ends its connection.

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

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_runtime::remote_door::{ALPN, PersonalDevices, agent_id, open};
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
    sign_revocation_entry,
};

use common::private_home;

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
        common::wait_for(&mut self.session, "the agent's answer", |_| {
            agent.answers.try_recv().ok()
        })
    }

    /// Press `effect` on the sheet up, as the person would.
    fn press(&mut self, effect: &str, args: &Value) {
        common::press_sheet(&mut self.session, effect, args);
    }

    /// The agents the desk seats now, by the key its presence names each.
    fn seated(&self) -> Vec<String> {
        self.session
            .read(tilo_surface_host::shell_part::DESK_RESOURCE)
            .and_then(|desk| desk.get("agents").cloned())
            .and_then(|agents| {
                serde_json::from_value::<Vec<tilo_surface_host::shell_part::AgentReading>>(agents)
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|agent| agent.seat)
            .map(|agent| agent.key)
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
    assert_eq!(what, format!("Work in “Desk” as the agent {named}"));

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
            tilo_surface_host::shell_part::DESK_RESOURCE,
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
