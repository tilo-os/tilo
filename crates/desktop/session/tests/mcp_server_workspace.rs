//! The MCP server at its bound, through the ways in a session really serves: its own socket for
//! the programs of this login, and the socket of a confined environment for the programs inside
//! it. The server serves sixteen connections at once and counts them as one whichever way they
//! came. The one past them is told what it can act on: that the server is full, that nothing it
//! asked ran, and to connect again once a connection has closed.
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
use std::sync::mpsc;
use std::time::{Duration, Instant};

use runtime_portal::environment_socket::{self, Lease, Request};
use serde_json::{Value, json};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_session::{AppSocket, Options, Session};
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;

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

/// A shell with one workspace, the person's, in front.
fn tree() -> Layout {
    let id = WorkspaceId::parse(STUDIO).expect("a workspace id");
    let mut studio = Workspace::new(id, "Studio");
    studio.policy.set_owner(PERSON);
    Layout {
        active: studio.id(),
        workspaces: vec![studio],
        ..Layout::default()
    }
}

/// This box: its headless session with the MCP server open, and the questions it asks the
/// person.
struct Shell {
    session: Session,
    questions: tilo_service::AskReceiver,
    socket: PathBuf,
    _root: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

impl Shell {
    fn start() -> Self {
        let home = tilo_test_env::TestHome::new().expect("a home");
        let root = crate::common::private_home();
        let socket = root.path().join("agent.sock");
        let (asks, questions) = crate::common::person_asked();
        let mut session = crate::common::headless(Options {
            asks: Some((asks, questions.clone())),
            collab: false,
            person: PERSON.to_owned(),
            app_socket: AppSocket::At(root.path().join("app_wire.sock")),
            mcp_server: Some(socket.clone()),
            state: Some(home.state().to_path_buf()),
            ..Options::default()
        });
        session.handle().replace_tree(tree());
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        Self {
            session,
            questions,
            socket,
            _root: root,
            _home: home,
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
        crate::common::person_answers(&mut self.session, crate::common::allowed_for(1));
        assert!(
            self.questions.pending().is_empty(),
            "one question was asked"
        );
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
                    .len();
                (here == sessions).then_some(())
            },
        );
    }

    /// Connect again, as the socket's refusal says to, until `connect` is served: every refusal on
    /// the way is the socket's having no room, and nothing else.
    #[track_caller]
    fn once_there_is_room<T>(
        &mut self,
        mut connect: impl FnMut(&mut Self) -> Result<T, String>,
    ) -> T {
        let deadline = Instant::now() + tilo_test_env::hang_bound();
        loop {
            match connect(self) {
                Ok(served) => return served,
                Err(told) => says_no_room(&told),
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

/// The socket serves sixteen connections at once, whichever way each came in: thirteen programs
/// of this login and three programs inside an environment are all served, and the next is refused
/// by either way in, in the same words, before anything is read of it and without the person
/// being asked. Who may connect at all is decided before that, so a program outside the
/// environment is refused as that, not as one more than the socket serves. When a connection
/// closes, one that connects again is served, whichever way either came.
#[test]
fn the_server_serves_sixteen_connections_however_they_came_and_tells_the_next_there_is_no_room() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut shell = Shell::start();
    let dev = shell.environment("dev");

    // Thirteen programs of this login. The person is asked about the first, and lets the
    // program in for the session.
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
    for _ in 1..13 {
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

    // Who may connect is decided first: a full socket tells a program outside the environment
    // what it tells it when it has room.
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

    // A program inside the environment is turned away, and connects again as it was told to.
    // A program of this login leaves: the program inside is served, and the socket is as full
    // as it was.
    let waiting = Inside::start(dev.path(), true);
    assert_eq!(shell.said(&waiting), ("REFUSED".to_owned(), told.clone()));
    drop(programs.pop());
    shell.served_inside(&waiting);
    inside.push(waiting);
    let mut next = shell.program();
    let refused = shell
        .answer(&mut next)
        .expect("one past the bound is told why it is not served");
    assert_eq!(refusal(&refused), told);

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
            Err(refusal(&answer))
        }
    });
    programs.push(program);
    assert!(shell.questions.pending().is_empty());
    shell.at_shell(SERVED);

    for program in inside {
        program.end();
    }
}
