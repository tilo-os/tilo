//! Coding agents in confined environments, and several at once: each environment `ush` opens
//! asks the session for a socket of its own, and a program inside it is served there as an agent
//! of that environment, working in the environment's workspace rather than the one in front of
//! the person. Every connection the person admits works as the agent member of its workspace,
//! under the name the workspace calls it by, with the environment it came in by beside it, and
//! "Always allow" is remembered per environment.
//!
//! The environments here are this test's own processes, as `ush` is the person's: a process asks
//! for the socket through the production request ([`runtime_portal::environment_socket`]) and holds
//! its lease, and the programs inside it are processes it starts. A program outside that process
//! tree, the test itself or a program of another environment, is refused at the socket.

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
use std::time::Duration;

use runtime_portal::environment_socket::{self, Request};
use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_session::{AppSocket, Options, Session};
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::common::{HEIGHT, WIDTH};

/// The person at the shell, who owns its workspaces.
const PERSON: &str = "ada";
/// The workspace an environment works in, and the one in front of the person.
const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";
const GARDEN: &str = "2c9d7a61-0e3b-4c8f-a5d2-7b6e5f4a3c2d";
/// A workspace on the shell the person is no member of.
const ELSEWHERE: &str = "f1e2d3c4-b5a6-4978-8a9b-0c1d2e3f4a5b";

/// One session at a time in this process: two headless sessions with MCP servers side by side in
/// one test process wait on each other (as `agent_control`'s do), so these take turns.
static ONE_SESSION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What this test's own processes are, when it starts itself as one of them.
const ROLE: &str = "TILO_TEST_AGENT_SESSION_ROLE";

/// A process of this test's, started as the role [`ROLE`] names: an environment holding its
/// socket, or a program inside one. Run as a test of its own, it does nothing.
#[test]
fn program() {
    match std::env::var(ROLE).as_deref() {
        Ok("environment") => environment(),
        Ok("client") => client(),
        _ => {}
    }
}

/// An environment, as `ush` opens one: ask the session for its socket, say where it is, start the
/// program inside it, and hold the lease until told to end (this process's input closes).
fn environment() {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let socket = PathBuf::from(var("SOCKET").expect("the session's socket"));
    let request = Request::new(
        &var("ENVIRONMENT").expect("the environment"),
        &var("ENVIRONMENT_NAME").expect("its name"),
        var("ENVIRONMENT_WORKSPACE").as_deref(),
    );
    let lease = match environment_socket::open(&socket, &request) {
        Ok(lease) => lease,
        Err(error) => {
            say("REFUSED", &error.to_string());
            ended();
            return;
        }
    };
    say("SOCKET", &lease.path().display().to_string());
    let target = match var("CLIENT_TARGET").as_deref() {
        None => {
            ended();
            return;
        }
        Some("own") => lease.path().to_path_buf(),
        Some(other) => PathBuf::from(other),
    };
    let mut inside = start("client", &[("TARGET", target.display().to_string())], true);
    inside.wait().expect("the program inside ends");
    drop(lease);
}

/// A coding agent's program: connect to `TARGET`, introduce itself as `CLIENT_NAME` (asking for
/// `ASK_WORKSPACE`, when named), say what the socket answered, list what it may do, and stay
/// until told to end.
fn client() {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let target = var("TARGET").expect("where to connect");
    let stream = match UnixStream::connect(&target) {
        Ok(stream) => stream,
        Err(error) => {
            say("UNREACHABLE", &error.to_string());
            ended();
            return;
        }
    };
    let workspace = var("ASK_WORKSPACE");
    let answers = introduce(
        &stream,
        &var("CLIENT_NAME").unwrap_or_else(|| "Claude Code".to_owned()),
        workspace.as_deref(),
    );
    for answer in answers {
        say("ANSWER", &answer.to_string());
    }
    ended();
}

/// Introduce a client named `name` on `stream`, asking for `workspace`; once served, list what it
/// may do. The answers it read.
fn introduce(stream: &UnixStream, name: &str, workspace: Option<&str>) -> Vec<Value> {
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut capabilities = json!({});
    if let Some(workspace) = workspace {
        capabilities = json!({"experimental": {"so.tilo": {"workspace": workspace}}});
    }
    let mut read = Vec::new();
    for ask in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "clientInfo": {"name": name, "version": "2.1"}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "tilo_ls", "arguments": {}}
        }),
    ] {
        // A socket that refuses closes at once, and can close before this ask reaches it: the
        // write then fails, and the reason it gave is already on the connection. What the socket
        // answered is read either way, so a refusal that came early is still the answer.
        let asked = writer.write_all(format!("{ask}\n").as_bytes()).is_ok();
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let answer: Value = serde_json::from_str(&line).unwrap();
                let refused = answer.get("error").is_some();
                read.push(answer);
                if refused || !asked {
                    break;
                }
            }
        }
    }
    read
}

/// One line for the test that started this process: `what`, then the rest.
fn say(what: &str, rest: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{what} {rest}").unwrap();
    out.flush().unwrap();
}

/// Wait until the test closes this process's input.
fn ended() {
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
}

/// This test's program started as `role`, with `vars`; its output this process's own when
/// `inherit`, else read by the test.
fn start(role: &str, vars: &[(&str, String)], inherit: bool) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "agent_environments::program",
            "--nocapture",
            "--test-threads",
            "1",
            "-q",
        ])
        .env(ROLE, role)
        .stderr(Stdio::inherit())
        // The environment's program ends with the test, however the test ends: a program the
        // test's own output reaches must not outlive the run either.
        .dies_with_the_test();
    for (name, value) in vars {
        command.env(name, value);
    }
    if inherit {
        command.stdin(Stdio::inherit()).stdout(Stdio::inherit());
    } else {
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
    }
    command.spawn_locked().unwrap()
}

/// An environment of this test's: the process holding its socket, and what it and the program
/// inside it said.
struct Environment {
    child: Child,
    said: mpsc::Receiver<(String, String)>,
}

impl Environment {
    /// Open the environment `id` at `socket`, called `name`, in `workspace` (the one in front
    /// when `None`), with a program named `client` inside it connecting to `target` (`own`,
    /// its own socket, or another's path; none when `None`), asking for `asked`.
    fn open(
        socket: &Path,
        id: &str,
        workspace: Option<&str>,
        client: Option<(&str, &str)>,
    ) -> Self {
        Self::asking(socket, id, workspace, client, None)
    }

    fn asking(
        socket: &Path,
        id: &str,
        workspace: Option<&str>,
        client: Option<(&str, &str)>,
        asked: Option<&str>,
    ) -> Self {
        let mut vars = vec![
            ("SOCKET", socket.display().to_string()),
            ("ENVIRONMENT", id.to_owned()),
            (
                "ENVIRONMENT_NAME",
                format!("Env {}", id.rsplit('/').next().unwrap_or(id)),
            ),
            (
                "ENVIRONMENT_WORKSPACE",
                workspace.unwrap_or_default().to_owned(),
            ),
            ("ASK_WORKSPACE", asked.unwrap_or_default().to_owned()),
        ];
        if let Some((name, target)) = client {
            vars.push(("CLIENT_NAME", name.to_owned()));
            vars.push(("CLIENT_TARGET", target.to_owned()));
        }
        let mut child = start("environment", &vars, false);
        let stdout = child.stdout.take().unwrap();
        let (tell, said) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some((what, rest)) = line.split_once(' ')
                    && matches!(what, "SOCKET" | "REFUSED" | "ANSWER" | "UNREACHABLE")
                {
                    let _ = tell.send((what.to_owned(), rest.to_owned()));
                }
            }
        });
        Self { child, said }
    }

    /// The next thing it says, while the session runs.
    #[track_caller]
    fn next(&self, session: &mut Session) -> (String, String) {
        crate::common::wait_for(session, "the environment to say something", |_| {
            self.said.try_recv().ok()
        })
    }

    /// Where its socket is.
    #[track_caller]
    fn socket(&self, session: &mut Session) -> PathBuf {
        let (what, rest) = self.next(session);
        assert_eq!(what, "SOCKET", "the environment's socket opened: {rest}");
        PathBuf::from(rest)
    }

    /// The next answer its program read.
    #[track_caller]
    fn answer(&self, session: &mut Session) -> Value {
        let (what, rest) = self.next(session);
        assert_eq!(what, "ANSWER", "the program was answered: {rest}");
        serde_json::from_str(&rest).unwrap()
    }

    /// End it, and its program: what ending the environment does.
    fn end(mut self) {
        drop(self.child.stdin.take());
        self.child.wait().expect("the environment ends");
    }
}

/// A shell with three workspaces: the person's Studio and Garden, Garden in front, and one they
/// are no member of.
fn shell() -> Layout {
    let workspace = |id: &str, name: &str, owner: &str| {
        let id = WorkspaceId::parse(id).expect("a workspace id");
        let mut workspace = Workspace::new(id, name);
        workspace.set_owner(owner);
        workspace
    };
    let garden = workspace(GARDEN, "Garden", PERSON);
    Layout {
        active: garden.id(),
        workspaces: vec![
            workspace(STUDIO, "Studio", PERSON),
            garden,
            workspace(ELSEWHERE, "Elsewhere", "someone-else"),
        ],
        ..Layout::default()
    }
}

/// A headless session on [`shell`], its MCP server at `root`, and the questions it asks the person.
fn session(root: &Path) -> (Session, tilo_service::AskReceiver, PathBuf) {
    let socket = root.join("agent.sock");
    let (asks, questions) = crate::common::person_asked();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            asks: Some((asks, questions.clone())),
            collab: false,
            person: PERSON.to_owned(),
            app_socket: AppSocket::At(root.join("app_wire.sock")),
            mcp_server: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(shell());
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    (session, questions, socket)
}

/// The question the person is asked, once exactly one waits.
#[track_caller]
fn asked(session: &mut Session, questions: &tilo_service::AskReceiver) -> tilo_service::Question {
    crate::common::wait_for(session, "a question for the person", |_| {
        let pending = questions.pending();
        (pending.len() == 1)
            .then(|| pending.into_iter().next())
            .flatten()
    })
}

/// What the person is asked to let in: the workspace and the agent, as the question says them.
fn admission(question: &tilo_service::Question) -> String {
    let said = |field: &str| {
        question.details[field]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    format!(
        "Work in “{}” as the agent {}",
        said("workspace"),
        said("agent")
    )
}

/// The person allows the question up: once (0), for the session (1), or always (2).
#[track_caller]
fn allow(session: &mut Session, choice: usize) {
    crate::common::person_answers(session, crate::common::allowed_for(choice));
}

/// The person declines the question up.
#[track_caller]
fn decline(
    session: &mut Session,
    questions: &tilo_service::AskReceiver,
    shown: &tilo_service::Question,
) {
    assert!(questions.decline(&shown.handle), "the question waits");
    session.step(Duration::from_millis(1)).expect("step");
}

/// The agent sessions at the person's MCP servers, as their shell lists them: each one's id, how
/// the shell names it, the workspace it works in, and the environment it came in by (empty for a
/// program of the session's own server).
fn agent_sessions(session: &Session) -> Vec<(String, String, String, String)> {
    session
        .read("session:agents")
        .and_then(|agents| agents.get("sessions").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|agent| {
            let text = |field: &str| agent[field].as_str().unwrap_or_default().to_owned();
            (
                text("key"),
                text("name"),
                text("doing"),
                text("environment"),
            )
        })
        .collect()
}

/// The sessions the shell calls `name`, from the environment `environment` (empty for a program
/// of the session's own server): every connection of one client is called the same.
fn sessions_named(
    session: &Session,
    name: &str,
    environment: &str,
) -> Vec<(String, String, String, String)> {
    agent_sessions(session)
        .into_iter()
        .filter(|(_, shown, _, from)| shown.as_str() == name && from.as_str() == environment)
        .collect()
}

/// Wait until a session the shell calls `name` is here, from `environment` (empty for a program
/// of the session's own server), and say how.
#[track_caller]
fn session_for(
    session: &mut Session,
    name: &str,
    environment: &str,
) -> (String, String, String, String) {
    crate::common::wait_for(session, format!("{name} at the shell"), |session| {
        sessions_named(session, name, environment)
            .into_iter()
            .next()
    })
}

/// A program of the person's login, outside every environment, at `socket`: its answers, read
/// while the session runs, and its leaving.
struct Program {
    answers: mpsc::Receiver<Vec<Value>>,
    leave: mpsc::Sender<()>,
    ended: std::thread::JoinHandle<()>,
}

impl Program {
    fn connect(socket: &Path, workspace: Option<&str>) -> Self {
        let socket = socket.to_path_buf();
        let workspace = workspace.map(str::to_owned);
        let (tell, answers) = mpsc::channel();
        let (leave, staying) = mpsc::channel::<()>();
        let ended = std::thread::spawn(move || {
            let stream = UnixStream::connect(&socket).expect("the socket");
            let read = introduce(&stream, "Claude Code", workspace.as_deref());
            let _ = tell.send(read);
            let _ = staying.recv_timeout(tilo_test_env::hang_bound());
        });
        Self {
            answers,
            leave,
            ended,
        }
    }

    #[track_caller]
    fn answers(&self, session: &mut Session) -> Vec<Value> {
        crate::common::wait_for(session, "the program's answers", |_| {
            self.answers.try_recv().ok()
        })
    }

    fn end(self) {
        let _ = self.leave.send(());
        let _ = self.ended.join();
    }
}

/// The message a refused client read.
fn refusal(answer: &Value) -> Option<&str> {
    answer.pointer("/error/message").and_then(Value::as_str)
}

/// An agent in an environment works in that environment's workspace, not the one in front of
/// the person, and asking for another is refused. Its socket goes when the environment ends,
/// and so does the agent it let in.
#[test]
fn an_agent_in_an_environment_works_in_its_workspace_alone() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, socket) = session(root.path());

    let dev = Environment::open(
        &socket,
        &format!("{STUDIO}/dev"),
        Some(STUDIO),
        Some(("Claude Code", "own")),
    );
    let path = dev.socket(&mut session);
    assert!(
        path.starts_with(root.path().join("environments")),
        "the environment's socket is beside the session's: {}",
        path.display()
    );
    assert!(
        path.exists(),
        "the socket is there while the environment is"
    );

    let shown = asked(&mut session, &questions);
    assert_eq!(
        admission(&shown),
        "Work in “Studio” as the agent Claude Code",
        "the environment's workspace, not Garden in front, and the agent the workspace names"
    );
    assert!(
        shown.label.starts_with("Claude Code (")
            && shown.label.ends_with(", in the environment “Env dev”"),
        "who asks, and from which environment: {}",
        shown.label
    );
    allow(&mut session, 1);
    let initialized = dev.answer(&mut session);
    assert!(
        initialized.pointer("/result/serverInfo").is_some(),
        "the program is served: {initialized}"
    );
    let listed = dev.answer(&mut session);
    assert!(
        listed.get("error").is_none(),
        "it reaches the desktop's actions: {listed}"
    );
    let (dev_session, _, doing, environment) = session_for(&mut session, "Claude Code", "Env dev");
    assert_eq!(
        doing, "Studio",
        "the shell shows the agent, from its environment, working in Studio"
    );
    assert_eq!(
        environment, "Env dev",
        "the shell says the environment it came in by beside its name"
    );

    // A program in an environment works in its environment's workspace only: asking for the
    // one in front is refused before the person is asked anything.
    let elsewhere = Environment::asking(
        &socket,
        "dev-2",
        Some(STUDIO),
        Some(("Claude Code", "own")),
        Some(GARDEN),
    );
    elsewhere.socket(&mut session);
    let refused = elsewhere.answer(&mut session);
    assert_eq!(
        refusal(&refused),
        Some("A program in an environment works in that environment’s workspace only."),
        "{refused}"
    );
    assert!(questions.pending().is_empty(), "nothing was asked");
    elsewhere.end();

    dev.end();
    crate::common::wait_for(&mut session, "the environment's socket to go", |session| {
        (!path.exists()
            && !agent_sessions(session)
                .iter()
                .any(|(key, ..)| *key == dev_session))
        .then_some(())
    });
}

/// An environment's socket serves the programs inside that environment and nothing else: not a
/// program of the login outside it, and not one inside another environment.
#[test]
fn an_environments_socket_serves_its_own_programs_alone() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, socket) = session(root.path());

    let dev = Environment::open(&socket, "dev", Some(STUDIO), None);
    let path = dev.socket(&mut session);

    // The test itself is a program of this login, and outside the environment.
    let outside = Program::connect(&path, None);
    let read = outside.answers(&mut session);
    assert_eq!(
        read.first().and_then(refusal),
        Some("this socket serves only programs inside the environment it belongs to"),
        "{read:?}"
    );
    outside.end();

    // A program inside another environment reaches for it.
    let other = Environment::open(
        &socket,
        "other",
        Some(STUDIO),
        Some(("Claude Code", &path.display().to_string())),
    );
    other.socket(&mut session);
    let refused = other.answer(&mut session);
    assert_eq!(
        refusal(&refused),
        Some("this socket serves only programs inside the environment it belongs to"),
        "{refused}"
    );
    assert!(
        questions.pending().is_empty(),
        "nobody was asked about either"
    );

    other.end();
    dev.end();
}

/// "Always allow" is the person's answer about a program in one environment: a later session
/// of it there is let in without asking, as an agent of its own, while the same program in
/// another environment, or outside every environment, is asked about.
#[test]
fn always_allow_is_remembered_per_environment() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, socket) = session(root.path());

    let first = Environment::open(&socket, "dev", Some(STUDIO), Some(("Claude Code", "own")));
    first.socket(&mut session);
    asked(&mut session, &questions);
    allow(&mut session, 2);
    assert!(first.answer(&mut session).get("result").is_some());
    session_for(&mut session, "Claude Code", "Env dev");

    let again = Environment::open(&socket, "dev", Some(STUDIO), Some(("Claude Code", "own")));
    again.socket(&mut session);
    assert!(
        again.answer(&mut session).get("result").is_some(),
        "let in as the person said"
    );
    assert!(questions.pending().is_empty(), "without asking");
    crate::common::wait_for(
        &mut session,
        "the second session in the environment",
        |session| (sessions_named(session, "Claude Code", "Env dev").len() == 2).then_some(()),
    );

    let other = Environment::open(&socket, "other", Some(STUDIO), Some(("Claude Code", "own")));
    other.socket(&mut session);
    let shown = asked(&mut session, &questions);
    assert_eq!(
        admission(&shown),
        "Work in “Studio” as the agent Claude Code"
    );
    decline(&mut session, &questions, &shown);
    assert_eq!(
        refusal(&other.answer(&mut session)),
        Some("You didn’t let this program work here as an agent.")
    );

    let host = Program::connect(&socket, None);
    let shown = asked(&mut session, &questions);
    assert_eq!(
        admission(&shown),
        "Work in “Garden” as the agent Claude Code",
        "outside every environment, in the workspace in front"
    );
    decline(&mut session, &questions, &shown);
    let read = host.answers(&mut session);
    assert!(read.first().and_then(refusal).is_some(), "{read:?}");
    host.end();

    other.end();
    again.end();
    first.end();
}

/// Two Claude Codes at once are two sessions of the one agent member: the shell calls both alike,
/// each row its own session, and the person's Stop ends the one they stop.
#[test]
fn two_programs_of_one_name_are_two_agents() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, socket) = session(root.path());

    let first = Program::connect(&socket, None);
    asked(&mut session, &questions);
    allow(&mut session, 1);
    assert!(first.answers(&mut session)[0].get("result").is_some());
    let second = Program::connect(&socket, None);
    assert!(
        second.answers(&mut session)[0].get("result").is_some(),
        "the person let this program in for the session"
    );
    let listed = crate::common::wait_for(&mut session, "two sessions at the shell", |session| {
        let listed = sessions_named(session, "Claude Code", "");
        (listed.len() == 2).then_some(listed)
    });
    let (first_session, _, doing, _) = listed[0].clone();
    let (second_session, ..) = listed[1].clone();
    assert_ne!(first_session, second_session, "two sessions");
    assert_eq!(doing, "Garden");

    let mut stop = Map::new();
    stop.insert("agent".to_owned(), json!(first_session));
    session
        .invoke("session:agents", "stop-agent", &stop)
        .expect("the person's Stop");
    crate::common::wait_for(&mut session, "the first agent to stop", |session| {
        let keys: Vec<String> = agent_sessions(session)
            .into_iter()
            .map(|(key, ..)| key)
            .collect();
        (keys == [second_session.clone()]).then_some(())
    });
    first.end();
    second.end();
}

/// A program of the login may ask for a workspace by id: it is asked about there, and refused
/// one the person may not let an agent into, as one that is not on the shell at all.
#[test]
fn a_program_asks_for_a_workspace_the_person_may_grant() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, socket) = session(root.path());

    let studio = Program::connect(&socket, Some(STUDIO));
    let shown = asked(&mut session, &questions);
    assert_eq!(
        admission(&shown),
        "Work in “Studio” as the agent Claude Code"
    );
    allow(&mut session, 0);
    assert!(studio.answers(&mut session)[0].get("result").is_some());
    let (_, _, doing, _) = session_for(&mut session, "Claude Code", "");
    assert_eq!(doing, "Studio", "where it asked, not Garden in front");

    for workspace in [ELSEWHERE, "00000000-0000-4000-8000-000000000000"] {
        let refused = Program::connect(&socket, Some(workspace));
        let read = refused.answers(&mut session);
        assert_eq!(
            read.first().and_then(refusal),
            Some("You can’t let an agent work in that workspace from here."),
            "{workspace}: {read:?}"
        );
        assert!(questions.pending().is_empty(), "nobody was asked");
        refused.end();
    }
    studio.end();
}
