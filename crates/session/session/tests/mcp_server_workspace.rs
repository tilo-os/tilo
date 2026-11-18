//! The MCP server at its bound, through the ways in a session really serves: its own socket for
//! the programs of this login, the socket of a confined environment for the programs inside it,
//! and this device's endpoint for the agents on the person's other devices. The server serves
//! sixteen connections at once and counts them as one whichever way they came. The one past them
//! is told what it can act on: that the server is full, that nothing it asked ran, and to connect
//! again once a connection has closed. One from another device is told so in place of the answer
//! that the server opened, and those that arrive between two passes of the session wait in the
//! server's own places, so no more of them wait than it serves.
//!
//! The environment here is the test's own: the test holds its socket's lease as `ush` does
//! ([`runtime_portal::environment_socket`]), and the programs inside it are processes the test
//! starts, as an environment's are its `ush`'s.

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

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use runtime_portal::environment_socket::{self, Lease, Request};
use serde_json::{Value, json};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_mcp::{ALPN, NotOpened, open};
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_runtime::personal_devices::PersonalDevices;
use tilo_service::Questions;
use tilo_session::{AppSocket, Options, Session};
use tilo_sheets::portal::PortalEffect;
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// The person at the shell, who owns its workspace.
const PERSON: &str = "ada";
/// The workspace in front of the person, which every agent here works in.
const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";
/// As many connections as the socket serves at once.
const SERVED: usize = 16;

/// What this test's own processes are, when it starts itself as one of them.
const ROLE: &str = "TILO_TEST_AGENT_WORKSPACE_ROLE";

/// A process of this test's, started as the role [`ROLE`] names: a program inside the test's
/// environment. Run as a test of its own, it does nothing.
#[test]
fn program() {
    if std::env::var(ROLE).as_deref() == Ok("inside") {
        inside();
    }
}

/// A coding agent's program inside an environment: connect to `TARGET`, the environment's socket,
/// introduce itself and say what the socket answered, then stay until told to end (this process's
/// input closes). With `AGAIN`, a socket that has no room is asked again until it serves this
/// program, as the socket's refusal says to, and the first refusal is said too.
fn inside() {
    let target = std::env::var("TARGET").expect("where to connect");
    let again = std::env::var("AGAIN").is_ok_and(|again| again == "1");
    let mut refused = false;
    let (answer, _stream) = tilo_test_env::wait_for("the socket to answer this program", || {
        let stream = UnixStream::connect(&target).expect("the environment's socket");
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        // A socket that refuses says so and closes at once, and can have closed before this
        // reaches it: the write then fails, and what the socket said is read all the same.
        let _ = (&stream).write_all(format!("{}\n", initialize()).as_bytes());
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .expect("the socket answers before it closes");
        let answer: Value = serde_json::from_str(&line).expect("one JSON reply a line");
        if again && let Some(told) = answer.pointer("/error/message").and_then(Value::as_str) {
            if !refused {
                say("REFUSED", told);
                refused = true;
            }
            return None;
        }
        Some((answer, stream))
    });
    say("ANSWER", &answer.to_string());
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
}

/// One line for the test that started this process: `what`, then the rest.
fn say(what: &str, rest: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{what} {rest}").unwrap();
    out.flush().unwrap();
}

/// A program inside the test's environment, and what it said.
struct Inside {
    child: Child,
    said: mpsc::Receiver<(String, String)>,
}

impl Inside {
    /// Start a program inside the environment whose socket is at `socket`; with `again`, one that
    /// connects again for as long as the socket has no room for it.
    fn start(socket: &Path, again: bool) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp_server_workspace::program",
                "--nocapture",
                "--test-threads",
                "1",
                "-q",
            ])
            .env(ROLE, "inside")
            .env("TARGET", socket)
            .env("AGAIN", if again { "1" } else { "0" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // The environment's program ends with the test, however the test ends.
            .dies_with_the_test()
            .spawn_locked()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tell, said) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((what, rest)) = line.split_once(' ')
                    && matches!(what, "REFUSED" | "ANSWER")
                {
                    let _ = tell.send((what.to_owned(), rest.to_owned()));
                }
            }
        });
        Self { child, said }
    }

    /// End it: what closing the agent does, and its connection goes with it.
    fn end(mut self) {
        drop(self.child.stdin.take());
        self.child.wait().expect("the program ends");
    }
}

/// What a client found when it looked for the socket's next line.
enum Heard {
    /// One whole reply.
    Line(Value),
    /// The socket closed the connection.
    Closed,
}

/// One connection to a socket, as an MCP client holds it: a line each way. Nothing here waits on
/// the socket: the session's loop turns on the test's own thread, so what the socket wrote is read
/// when the test looks.
#[derive(Debug)]
struct Client {
    stream: UnixStream,
    read: Vec<u8>,
}

impl Client {
    fn over(stream: UnixStream) -> Self {
        stream.set_nonblocking(true).unwrap();
        Self {
            stream,
            read: Vec::new(),
        }
    }

    fn connect(socket: &Path) -> Self {
        Self::over(UnixStream::connect(socket).expect("the socket"))
    }

    /// Ask `request`. A socket that refused this connection may have closed it already: what it
    /// said is read all the same.
    fn send(&mut self, request: &Value) {
        let _ = self.stream.write_all(format!("{request}\n").as_bytes());
    }

    /// The next line the socket wrote, once one is whole, or that it closed the connection;
    /// `None` while it has said nothing more.
    fn heard(&mut self) -> Option<Heard> {
        loop {
            if let Some(end) = self.read.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.read.drain(..=end).collect();
                let reply = serde_json::from_slice(&line[..end]).expect("one JSON reply a line");
                return Some(Heard::Line(reply));
            }
            let mut buffer = [0_u8; 4096];
            match self.stream.read(&mut buffer) {
                Ok(0) => return Some(Heard::Closed),
                Ok(read) => self.read.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return None,
                // Closed while this side still had something unread by it.
                Err(_) => return Some(Heard::Closed),
            }
        }
    }
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"Claude Code","version":"2.1"}}})
}

/// What a connection past the bound is told: what is full, that nothing it asked ran, and when
/// to connect again.
#[track_caller]
fn says_no_room(told: &str) {
    assert!(
        told.contains(&format!("{SERVED} connections"))
            && told.contains("nothing it asked ran")
            && told.contains("connect again once one of them has closed"),
        "it says what is full, that nothing ran and when to come back: {told}"
    );
}

/// A device key `n` certified by `root`, called `label`.
fn certified(root: &SigningKey, n: u8, label: &str) -> (SigningKey, DeviceCert) {
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

/// The person: this box, where the shell is, and two more devices of theirs, where agents run.
/// And a device of somebody else's.
struct Person {
    root: SigningKey,
    this_box: (SigningKey, DeviceCert),
    laptop: (SigningKey, DeviceCert),
    tablet: (SigningKey, DeviceCert),
    stranger: (SigningKey, DeviceCert),
}

impl Person {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[0x11; 32]);
        Self {
            this_box: certified(&root, 2, "Build box"),
            laptop: certified(&root, 1, "Studio laptop"),
            tablet: certified(&root, 3, "Kitchen tablet"),
            stranger: certified(&SigningKey::from_bytes(&[0x22; 32]), 4, "Their laptop"),
            root,
        }
    }

    /// The person's own store as this box keeps it: their laptop and their tablet admitted.
    fn store(&self) -> PersonalSync {
        let store = PersonalStore::open(
            self.root.verifying_key().to_bytes(),
            self.this_box.1.clone(),
            SystemTime::now(),
        )
        .unwrap();
        let sync = PersonalSync::new(store, self.this_box.0.clone());
        sync.admit(|store, now| {
            store.admit_device(&self.laptop.1, now)?;
            store.admit_device(&self.tablet.1, now)?;
            Ok(true)
        })
        .unwrap();
        sync
    }
}

/// A device that dials this box's socket: its certificate, and the endpoint whose key it names.
struct Device {
    cert: DeviceCert,
    endpoint: Endpoint,
}

/// A shell with one workspace, the person's, in front.
fn tree() -> Layout {
    let id = WorkspaceId::parse(STUDIO).expect("a workspace id");
    let mut studio = Workspace::new(id, "Studio");
    studio.set_owner(PERSON);
    Layout {
        active: studio.id(),
        workspaces: vec![studio],
        ..Layout::default()
    }
}

/// This box: its headless session with the MCP server open, the questions it asks the person,
/// and its endpoint answering the socket toward the person's other devices.
struct Shell {
    session: Session,
    questions: Questions,
    socket: PathBuf,
    runtime: tokio::runtime::Runtime,
    addr: EndpointAddr,
    _endpoint: Endpoint,
    _router: Router,
    _root: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

impl Shell {
    fn start(person: &Person) -> Self {
        let home = tilo_test_env::TestHome::new().expect("a home");
        let root = crate::common::private_home();
        let socket = root.path().join("agent.sock");
        let questions = Questions::new();
        let mut session = crate::common::headless(Options {
            questions: Some(questions.clone()),
            collab: false,
            person: PERSON.to_owned(),
            app_socket: AppSocket::At(root.path().join("app_wire.sock")),
            mcp_server: Some(socket.clone()),
            state: Some(home.state().to_path_buf()),
            // The person's settings, as the session serves them: what the person sets this
            // box's MCP server with.
            services: vec![std::boxed::Box::new(
                tilo_settings_service::SettingsService::new(),
            )],
            ..Options::default()
        });
        session.handle().replace_tree(tree());
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let remote = session.device_mcp(Arc::new(PersonalDevices::new(person.store())));
        let (endpoint, router) = runtime.block_on(async {
            let endpoint = endpoint(&person.this_box.0).await;
            let router = Router::builder(endpoint.clone())
                .accept(ALPN, remote)
                .spawn();
            (endpoint, router)
        });
        let addr = EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            endpoint.bound_sockets().first().unwrap().port(),
        )));
        Self {
            session,
            questions,
            socket,
            runtime,
            addr,
            _endpoint: endpoint,
            _router: router,
            _root: root,
            _home: home,
        }
    }

    /// The person lets agents from their other devices in on this box without being asked, as
    /// Settings does: a box nobody sits at.
    fn accept_own_devices(&mut self) {
        self.session
            .invoke(
                "settings:settings.mcp-server",
                "set-accept-own-devices",
                json!({"on": true}).as_object().unwrap(),
            )
            .expect("the setting is kept");
        crate::common::wait_for(&mut self.session, "the setting to be kept", |_| {
            settings_store::mcp_server::accepts_own_devices(&settings_store::load()).then_some(())
        });
    }

    /// One of the devices that dial this box, by its key and certificate.
    fn device(&self, (key, cert): &(SigningKey, DeviceCert)) -> Device {
        Device {
            cert: cert.clone(),
            endpoint: self.runtime.block_on(endpoint(key)),
        }
    }

    /// Open an environment's socket as its `ush` does, this test holding its lease: the programs
    /// the test starts are the programs inside it.
    fn environment(&mut self, record: &str) -> Lease {
        let socket = self.socket.clone();
        let request = Request::new(
            &format!("{STUDIO}/{record}"),
            &format!("Env {record}"),
            Some(STUDIO),
        );
        let (tell, opened) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tell.send(environment_socket::open(&socket, &request));
        });
        crate::common::wait_for(&mut self.session, "the environment's socket", |_| {
            opened.try_recv().ok()
        })
        .expect("the environment's socket opens")
    }

    /// A program of this login at the session's own socket.
    fn program(&self) -> Client {
        Client::connect(&self.socket)
    }

    /// Dial this box's socket from `device`: its connection once the socket said it opened, or the
    /// reason it said instead.
    fn dial(&self, device: &Device) -> Result<Client, NotOpened> {
        let dialed = self.runtime.block_on(open(
            &device.endpoint,
            self.addr.clone(),
            &device.cert,
            None,
            false,
        ))?;
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs.set_nonblocking(true).unwrap();
        let theirs = {
            let _entered = self.runtime.enter();
            tokio::net::UnixStream::from_std(theirs).unwrap()
        };
        self.runtime.spawn(dialed.carry(theirs));
        Ok(Client::over(ours))
    }

    /// The next line `client` reads while the session runs, or `None` once its connection is
    /// closed.
    #[track_caller]
    fn answer(&mut self, client: &mut Client) -> Option<Value> {
        let heard =
            crate::common::wait_for(&mut self.session, "the socket's answer", |_| client.heard());
        match heard {
            Heard::Line(reply) => Some(reply),
            Heard::Closed => None,
        }
    }

    /// Introduce `client` as an MCP client does, and read that the socket serves it.
    #[track_caller]
    fn served(&mut self, client: &mut Client) {
        client.send(&initialize());
        self.introduced(client);
    }

    /// Read that the socket serves `client`, which introduced itself.
    #[track_caller]
    fn introduced(&mut self, client: &mut Client) {
        let introduced = self.answer(client).expect("the socket answers it");
        assert!(
            introduced.pointer("/result/serverInfo").is_some(),
            "it is served: {introduced}"
        );
    }

    /// The next thing `inside` says, while the session runs.
    #[track_caller]
    fn said(&mut self, inside: &Inside) -> (String, String) {
        crate::common::wait_for(&mut self.session, "the program to say something", |_| {
            inside.said.try_recv().ok()
        })
    }

    /// Read that the socket serves the program `inside`.
    #[track_caller]
    fn served_inside(&mut self, inside: &Inside) {
        let (what, rest) = self.said(inside);
        assert_eq!(what, "ANSWER", "{rest}");
        let introduced: Value = serde_json::from_str(&rest).unwrap();
        assert!(
            introduced.pointer("/result/serverInfo").is_some(),
            "it is served: {introduced}"
        );
    }

    /// The person is asked about one client, and lets its program in for the session.
    #[track_caller]
    fn allow_for_the_session(&mut self) {
        let questions = self.questions.clone();
        crate::common::wait_for(&mut self.session, "a question for the person", |session| {
            (questions.pending().len() == 1)
                .then(|| {
                    session
                        .portal()
                        .and_then(tilo_sheets::portal::PortalState::dialog)
                })
                .flatten()
        });
        crate::common::press_sheet(
            &mut self.session,
            PortalEffect::SELECT.id,
            &json!({"index": 1}),
        );
        crate::common::press_sheet(&mut self.session, PortalEffect::CONFIRM.id, &json!({}));
    }

    /// Wait until `sessions` agent sessions are at the shell.
    #[track_caller]
    fn at_shell(&mut self, sessions: usize) {
        crate::common::wait_for(
            &mut self.session,
            format!("{sessions} agent sessions at the shell"),
            |session| {
                let here = session
                    .read("session:agents")
                    .and_then(|agents| agents.get("sessions").and_then(Value::as_array).cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|agent| agent["stoppable"] == true)
                    .count();
                (here == sessions).then_some(())
            },
        );
    }

    /// Connect again, as the socket's refusal says to, until `connect` is served: every refusal on
    /// the way is the socket's having no room, and nothing else.
    #[track_caller]
    fn once_there_is_room<T>(
        &mut self,
        mut connect: impl FnMut(&mut Self) -> Result<T, NotOpened>,
    ) -> T {
        let deadline = Instant::now() + tilo_test_env::hang_bound();
        loop {
            match connect(self) {
                Ok(served) => return served,
                Err(told) => says_no_room(told.reason()),
            }
            assert!(
                Instant::now() < deadline,
                "the socket has room once a connection closed"
            );
            self.session.step(Duration::from_millis(1)).expect("step");
        }
    }
}

/// What a local connection past the bound reads: a JSON-RPC error naming no request, since
/// none of its requests was read. The words it carries.
#[track_caller]
fn refusal(refused: &Value) -> String {
    assert_eq!(refused["error"]["code"], -32001, "{refused}");
    assert!(
        refused["id"].is_null(),
        "no request of it was read: {refused}"
    );
    refused["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The socket serves sixteen connections at once, whichever way each came in: seven programs of
/// this login, three programs inside an environment and six agents on two of the person's other
/// devices are all served, and the next is refused by every way in, in the same words, before
/// anything is read of it and without the person being asked. One from another device is not
/// told the socket opened first. Who may connect at all is decided before that, so a device of
/// somebody else's and a program outside the environment are refused as that, not as one more
/// than the socket serves. When a connection closes, one that connects again is served, whichever
/// way either came.
#[test]
fn the_server_serves_sixteen_connections_however_they_came_and_tells_the_next_there_is_no_room() {
    let _settled = ui::motion::testing::settled();
    let person = Person::new();
    let mut shell = Shell::start(&person);
    shell.accept_own_devices();
    let laptop = shell.device(&person.laptop);
    let tablet = shell.device(&person.tablet);
    let stranger = shell.device(&person.stranger);
    let dev = shell.environment("dev");

    // Seven programs of this login. The person is asked about the first, and lets the program
    // in for the session.
    let mut programs = Vec::new();
    let mut first = shell.program();
    first.send(&initialize());
    shell.allow_for_the_session();
    let introduced = shell.answer(&mut first).expect("the socket answers it");
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
    );
    programs.push(first);
    for _ in 1..7 {
        let mut program = shell.program();
        shell.served(&mut program);
        programs.push(program);
    }

    // Three programs inside an environment, asked about once there.
    let mut inside = vec![Inside::start(dev.path(), false)];
    shell.allow_for_the_session();
    shell.served_inside(&inside[0]);
    for _ in 1..3 {
        let program = Inside::start(dev.path(), false);
        shell.served_inside(&program);
        inside.push(program);
    }

    // Six agents on two of the person's other devices, which this box lets in unasked.
    let mut agents = Vec::new();
    for device in [&laptop, &tablet, &laptop, &tablet, &laptop, &tablet] {
        let mut agent = shell
            .dial(device)
            .expect("the socket opens for the person's device");
        shell.served(&mut agent);
        agents.push(agent);
    }
    assert!(
        shell.questions.pending().is_empty(),
        "nobody is waiting on the person"
    );
    shell.at_shell(SERVED);

    // The seventeenth, at the session's own socket: told before anything is read of it, and
    // closed.
    let mut next = shell.program();
    let refused = shell
        .answer(&mut next)
        .expect("one past the bound is told why it is not served");
    let told = refusal(&refused);
    says_no_room(&told);
    assert_eq!(
        shell.answer(&mut next),
        None,
        "and its connection is closed"
    );

    // From another device: refused in place of the answer that the socket opened, in the same
    // words.
    let unopened = shell
        .dial(&laptop)
        .expect_err("one past the bound is not told the socket opened");
    assert_eq!(
        unopened.reason(),
        told,
        "one refusal, whichever way it came"
    );

    // Inside the environment: the same.
    let turned_away = Inside::start(dev.path(), false);
    let (what, rest) = shell.said(&turned_away);
    assert_eq!(what, "ANSWER", "{rest}");
    let refused: Value = serde_json::from_str(&rest).unwrap();
    assert_eq!(
        refusal(&refused),
        told,
        "one refusal, whichever way it came"
    );
    turned_away.end();
    assert!(
        shell.questions.pending().is_empty(),
        "the person was asked about none of them"
    );

    // Who may connect is decided first: a full socket tells a device of somebody else's, and a
    // program outside the environment, what it tells them when it has room.
    let theirs = shell
        .dial(&stranger)
        .expect_err("somebody else's device is refused");
    assert!(theirs.reason().contains("someone else"), "{theirs}");
    let mut outside = Client::connect(dev.path());
    let refused = shell
        .answer(&mut outside)
        .expect("a program outside the environment is told why");
    assert_eq!(
        refused["error"]["message"],
        "this socket serves only programs inside the environment it belongs to",
        "{refused}"
    );

    // The sixteen being served are not disturbed by any of it.
    programs[0].send(&json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    let still = shell.answer(&mut programs[0]).expect("it still answers");
    assert!(still["result"].is_object(), "{still}");
    agents[0].send(&json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    let still = shell.answer(&mut agents[0]).expect("it still answers");
    assert!(still["result"].is_object(), "{still}");

    // A program of this login leaves: an agent on another device that connects again is
    // served, and the socket is as full as it was.
    drop(programs.pop());
    let mut arrived = shell.once_there_is_room(|shell| shell.dial(&tablet));
    shell.served(&mut arrived);
    agents.push(arrived);
    let mut next = shell.program();
    let refused = shell
        .answer(&mut next)
        .expect("one past the bound is told why it is not served");
    assert_eq!(refusal(&refused), told);

    // A program inside the environment is turned away, and connects again as it was told to.
    // An agent on another device leaves: the program is served.
    let waiting = Inside::start(dev.path(), true);
    assert_eq!(shell.said(&waiting), ("REFUSED".to_owned(), told));
    drop(agents.pop());
    shell.served_inside(&waiting);
    inside.push(waiting);

    // A program inside the environment leaves: a program of this login that connects again is
    // served, without the person being asked again.
    inside.remove(0).end();
    let program = shell.once_there_is_room(|shell| {
        let mut program = shell.program();
        program.send(&initialize());
        let answer = shell
            .answer(&mut program)
            .expect("the socket answers before it closes");
        if answer.pointer("/result/serverInfo").is_some() {
            Ok(program)
        } else {
            Err(NotOpened::Refused(refusal(&answer)))
        }
    });
    programs.push(program);
    assert!(shell.questions.pending().is_empty());
    shell.at_shell(SERVED);

    for program in inside {
        program.end();
    }
}

/// Connections from the person's other devices arrive on this device's endpoint, between the
/// passes of the session that serve them. Each takes one of the socket's places as it arrives and
/// waits in it: while the session's loop does not turn, sixteen are told the socket opened and
/// the next is refused, so no more wait than the socket serves. A program of this login that
/// connected meanwhile finds the socket full on the pass that follows, since the places are the
/// same ones. The sixteen that waited are then each served, and once one of them has closed, a
/// device that connects again is served.
#[test]
fn connections_from_other_devices_wait_for_the_pass_in_the_servers_places_and_no_more() {
    let _settled = ui::motion::testing::settled();
    let person = Person::new();
    let mut shell = Shell::start(&person);
    shell.accept_own_devices();
    let laptop = shell.device(&person.laptop);
    let tablet = shell.device(&person.tablet);

    // The session's loop does not turn from here until the test reads an answer: nothing
    // that arrives is served yet.
    let mut waiting = Vec::new();
    for place in 0..SERVED {
        let device = if place % 2 == 0 { &laptop } else { &tablet };
        let mut agent = shell
            .dial(device)
            .unwrap_or_else(|reason| panic!("connection {place} has a place to wait in: {reason}"));
        // What it opens with waits with it.
        agent.send(&initialize());
        waiting.push(agent);
    }
    let unopened = shell
        .dial(&laptop)
        .expect_err("one more than the socket serves is not told the socket opened");
    says_no_room(unopened.reason());
    let mut program = shell.program();

    // The loop turns. The places are taken, so the program of this login is told so.
    let refused = shell
        .answer(&mut program)
        .expect("one past the bound is told why it is not served");
    assert_eq!(refusal(&refused), unopened.reason());
    for agent in &mut waiting {
        shell.introduced(agent);
    }
    assert!(shell.questions.pending().is_empty(), "nobody was asked");
    shell.at_shell(SERVED);

    // One of them leaves: a device that connects again is served.
    drop(waiting.pop());
    let mut again = shell.once_there_is_room(|shell| shell.dial(&tablet));
    shell.served(&mut again);
}
