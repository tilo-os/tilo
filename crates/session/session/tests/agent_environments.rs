//! Coding agents in confined environments, and several at once: each environment `ush` opens
//! asks the session for a door of its own, and a program inside it is served there as an agent
//! of that environment, working in the environment's workspace rather than the one in front of
//! the person. Every connection the person admits is an agent of its own (`claude-code@dev#1`),
//! and "Always allow" is remembered per environment.
//!
//! The environments here are this test's own processes, as `ush` is the person's: a process asks
//! for the door through the production request ([`runtime_portal::environment_door`]) and holds
//! its lease, and the programs inside it are processes it starts. A program outside that process
//! tree, the test itself or a program of another environment, is refused at the door.

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

use runtime_portal::environment_door::{self, Request};
use serde_json::{Map, Value, json};
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::{Dialog, PortalEffect};
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::common::{HEIGHT, WIDTH};

/// The person at the desk, who owns its workspaces.
const PERSON: &str = "ada";
/// The workspace an environment works in, and the one in front of the person.
const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";
const GARDEN: &str = "2c9d7a61-0e3b-4c8f-a5d2-7b6e5f4a3c2d";
/// A workspace on the desk the person is no member of.
const ELSEWHERE: &str = "f1e2d3c4-b5a6-4978-8a9b-0c1d2e3f4a5b";

/// One session at a time in this process: two headless sessions with agent doors side by side in
/// one test process wait on each other (as `agent_control`'s do), so these take turns.
static ONE_SESSION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What this test's own processes are, when it starts itself as one of them.
const ROLE: &str = "TILO_TEST_AGENT_DOOR_ROLE";

/// A process of this test's, started as the role [`ROLE`] names: an environment holding its
/// door, or a program inside one. Run as a test of its own, it does nothing.
#[test]
fn program() {
    match std::env::var(ROLE).as_deref() {
        Ok("environment") => environment(),
        Ok("client") => client(),
        _ => {}
    }
}

/// An environment, as `ush` opens one: ask the session for its door, say where it is, start the
/// program inside it, and hold the door until told to end (this process's input closes).
fn environment() {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let door = PathBuf::from(var("DOOR").expect("the session's door"));
    let request = Request::new(
        &var("ENVIRONMENT").expect("the environment"),
        &var("ENVIRONMENT_NAME").expect("its name"),
        var("ENVIRONMENT_WORKSPACE").as_deref(),
    );
    let lease = match environment_door::open(&door, &request) {
        Ok(lease) => lease,
        Err(error) => {
            say("REFUSED", &error.to_string());
            ended();
            return;
        }
    };
    say("DOOR", &lease.path().display().to_string());
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
/// `ASK_WORKSPACE`, when named), say what the door answered, list what it may do, and stay
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
        // A door that refuses closes at once, and can close before this ask reaches it: the
        // write then fails, and the reason it gave is already on the connection. What the door
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
        .stderr(Stdio::inherit());
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

/// An environment of this test's: the process holding its door, and what it and the program
/// inside it said.
struct Environment {
    child: Child,
    said: mpsc::Receiver<(String, String)>,
}

impl Environment {
    /// Open the environment `id` at `door`, called `name`, in `workspace` (the one in front
    /// when `None`), with a program named `client` inside it connecting to `target` (`own`,
    /// its own door, or another's path; none when `None`), asking for `asked`.
    fn open(door: &Path, id: &str, workspace: Option<&str>, client: Option<(&str, &str)>) -> Self {
        Self::asking(door, id, workspace, client, None)
    }

    fn asking(
        door: &Path,
        id: &str,
        workspace: Option<&str>,
        client: Option<(&str, &str)>,
        asked: Option<&str>,
    ) -> Self {
        let mut vars = vec![
            ("DOOR", door.display().to_string()),
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
                    && matches!(what, "DOOR" | "REFUSED" | "ANSWER" | "UNREACHABLE")
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

    /// Where its door is.
    #[track_caller]
    fn door(&self, session: &mut Session) -> PathBuf {
        let (what, rest) = self.next(session);
        assert_eq!(what, "DOOR", "the environment's door opened: {rest}");
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

/// A desk with three workspaces: the person's Studio and Garden, Garden in front, and one they
/// are no member of.
fn desk() -> DesktopTree {
    let workspace = |id: &str, name: &str, owner: &str| {
        let id = WorkspaceId::parse(id).expect("a workspace id");
        let mut workspace = Workspace::in_workspace(WorkspaceSlot::of_workspace(id), id, name);
        workspace.set_owner(owner);
        workspace
    };
    let garden = workspace(GARDEN, "Garden", PERSON);
    DesktopTree {
        active: garden.slot,
        workspaces: vec![
            workspace(STUDIO, "Studio", PERSON),
            garden,
            workspace(ELSEWHERE, "Elsewhere", "someone-else"),
        ],
        ..DesktopTree::default()
    }
}

/// A headless session on [`desk`], its agent door at `root`, its questions asked on its sheets.
fn session(root: &Path) -> (Session, Questions, PathBuf) {
    let door = root.join("agent.sock");
    let questions = Questions::new();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            person: PERSON.to_owned(),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(door.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk());
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    (session, questions, door)
}

/// The resource publishing `action` among everything the session publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// Press `effect` on the sheet publishing it, as the person would.
#[track_caller]
fn press(session: &mut Session, effect: &str, args: Value) {
    let Value::Object(mut args) = args else {
        panic!("not an object: {args}");
    };
    if let Some(handle) = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .map(|dialog| dialog.handle)
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// The question on the sheet, once one stands.
#[track_caller]
fn asked(session: &mut Session, questions: &Questions) -> Dialog {
    crate::common::wait_for(session, "a question for the person", |session| {
        (questions.pending().len() == 1)
            .then(|| {
                session
                    .portal()
                    .and_then(tilo_sheets::portal::PortalSeat::dialog)
            })
            .flatten()
    })
}

/// The person allows the question up: once (0), for the session (1), or always (2).
#[track_caller]
fn allow(session: &mut Session, choice: usize) {
    press(session, PortalEffect::SELECT.id, json!({"index": choice}));
    press(session, PortalEffect::CONFIRM.id, json!({}));
}

/// The agents seated at the person's doors, as their desk lists them: each one's id, how the
/// desk names it, and the workspace it works in.
fn seated(session: &Session) -> Vec<(String, String, String)> {
    session
        .read(tilo_surface_host::shell_part::DESK_RESOURCE)
        .and_then(|desk| desk.get("agents").cloned())
        .and_then(|agents| {
            serde_json::from_value::<Vec<tilo_surface_host::shell_part::AgentReading>>(agents).ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| agent.seat)
        .map(|agent| (agent.key, agent.name, agent.doing))
        .collect()
}

/// Wait until `agent` is seated, and say how.
#[track_caller]
fn seated_as(session: &mut Session, agent: &str) -> (String, String, String) {
    crate::common::wait_for(session, format!("{agent} at the desk"), |session| {
        seated(session).into_iter().find(|(key, _, _)| key == agent)
    })
}

/// A program of the person's login, outside every environment, at `door`: its answers, read
/// while the session runs, and its leaving.
struct Program {
    answers: mpsc::Receiver<Vec<Value>>,
    leave: mpsc::Sender<()>,
    ended: std::thread::JoinHandle<()>,
}

impl Program {
    fn connect(door: &Path, workspace: Option<&str>) -> Self {
        let door = door.to_path_buf();
        let workspace = workspace.map(str::to_owned);
        let (tell, answers) = mpsc::channel();
        let (leave, staying) = mpsc::channel::<()>();
        let ended = std::thread::spawn(move || {
            let stream = UnixStream::connect(&door).expect("the door");
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
/// the person, and asking for another is refused. Its door goes when the environment ends,
/// and so does the agent it let in.
#[test]
fn an_agent_in_an_environment_works_in_its_workspace_alone() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, door) = session(root.path());

    let dev = Environment::open(
        &door,
        &format!("{STUDIO}/dev"),
        Some(STUDIO),
        Some(("Claude Code", "own")),
    );
    let path = dev.door(&mut session);
    assert!(
        path.starts_with(root.path().join("environments")),
        "the environment's door is beside the session's: {}",
        path.display()
    );
    assert!(path.exists(), "the door is there while the environment is");

    let shown = asked(&mut session, &questions);
    assert_eq!(
        shown.title, "Work in “Studio” as the agent claude-code@dev#1",
        "the environment's workspace, not Garden in front, and an agent of its own"
    );
    assert!(
        shown.heading.starts_with("Claude Code (")
            && shown.heading.ends_with(", in the environment “Env dev”"),
        "who asks, and from which environment: {}",
        shown.heading
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
    assert_eq!(
        seated_as(&mut session, "claude-code@dev#1"),
        (
            "claude-code@dev#1".to_owned(),
            "Claude Code in Env dev #1".to_owned(),
            "Studio".to_owned()
        ),
        "the desk shows the agent, from its environment, working in Studio"
    );

    // A program in an environment works in its environment's workspace only: asking for the
    // one in front is refused before the person is asked anything.
    let elsewhere = Environment::asking(
        &door,
        "dev-2",
        Some(STUDIO),
        Some(("Claude Code", "own")),
        Some(GARDEN),
    );
    elsewhere.door(&mut session);
    let refused = elsewhere.answer(&mut session);
    assert_eq!(
        refusal(&refused),
        Some("A program in an environment works in that environment’s workspace only."),
        "{refused}"
    );
    assert!(questions.pending().is_empty(), "nothing was asked");
    elsewhere.end();

    dev.end();
    crate::common::wait_for(&mut session, "the environment's door to go", |session| {
        (!path.exists()
            && !seated(session)
                .iter()
                .any(|(key, ..)| key == "claude-code@dev#1"))
        .then_some(())
    });
}

/// An environment's door serves the programs inside that environment and nothing else: not a
/// program of the login outside it, and not one inside another environment.
#[test]
fn an_environments_door_serves_its_own_programs_alone() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, door) = session(root.path());

    let dev = Environment::open(&door, "dev", Some(STUDIO), None);
    let path = dev.door(&mut session);

    // The test itself is a program of this login, and outside the environment.
    let outside = Program::connect(&path, None);
    let read = outside.answers(&mut session);
    assert_eq!(
        read.first().and_then(refusal),
        Some("this door serves only programs inside the environment it belongs to"),
        "{read:?}"
    );
    outside.end();

    // A program inside another environment reaches for it.
    let other = Environment::open(
        &door,
        "other",
        Some(STUDIO),
        Some(("Claude Code", &path.display().to_string())),
    );
    other.door(&mut session);
    let refused = other.answer(&mut session);
    assert_eq!(
        refusal(&refused),
        Some("this door serves only programs inside the environment it belongs to"),
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
    let (mut session, questions, door) = session(root.path());

    let first = Environment::open(&door, "dev", Some(STUDIO), Some(("Claude Code", "own")));
    first.door(&mut session);
    asked(&mut session, &questions);
    allow(&mut session, 2);
    assert!(first.answer(&mut session).get("result").is_some());
    seated_as(&mut session, "claude-code@dev#1");

    let again = Environment::open(&door, "dev", Some(STUDIO), Some(("Claude Code", "own")));
    again.door(&mut session);
    assert!(
        again.answer(&mut session).get("result").is_some(),
        "let in as the person said"
    );
    assert!(questions.pending().is_empty(), "without asking");
    seated_as(&mut session, "claude-code@dev#2");

    let other = Environment::open(&door, "other", Some(STUDIO), Some(("Claude Code", "own")));
    other.door(&mut session);
    let shown = asked(&mut session, &questions);
    assert_eq!(
        shown.title,
        "Work in “Studio” as the agent claude-code@other#1"
    );
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    assert_eq!(
        refusal(&other.answer(&mut session)),
        Some("You didn’t let this program work here as an agent.")
    );

    let host = Program::connect(&door, None);
    let shown = asked(&mut session, &questions);
    assert_eq!(
        shown.title, "Work in “Garden” as the agent claude-code@host#1",
        "outside every environment, in the workspace in front"
    );
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    let read = host.answers(&mut session);
    assert!(read.first().and_then(refusal).is_some(), "{read:?}");
    host.end();

    other.end();
    again.end();
    first.end();
}

/// Two Claude Codes at once are two agents: each its own id, at the desk as its own, and the
/// person's Stop ends the one they stop.
#[test]
fn two_programs_of_one_name_are_two_agents() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, door) = session(root.path());

    let first = Program::connect(&door, None);
    asked(&mut session, &questions);
    allow(&mut session, 1);
    assert!(first.answers(&mut session)[0].get("result").is_some());
    let second = Program::connect(&door, None);
    assert!(
        second.answers(&mut session)[0].get("result").is_some(),
        "the person let this program in for the session"
    );
    seated_as(&mut session, "claude-code@host#1");
    let (_, shown, doing) = seated_as(&mut session, "claude-code@host#2");
    assert_eq!(shown, "Claude Code #2", "the desk tells them apart");
    assert_eq!(doing, "Garden");

    let mut stop = Map::new();
    stop.insert("agent".to_owned(), json!("claude-code@host#1"));
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "stop-agent",
            &stop,
        )
        .expect("the person's Stop");
    crate::common::wait_for(&mut session, "the first agent to stop", |session| {
        let keys: Vec<String> = seated(session).into_iter().map(|(key, ..)| key).collect();
        (keys == ["claude-code@host#2".to_owned()]).then_some(())
    });
    first.end();
    second.end();
}

/// A program of the login may ask for a workspace by id: it is asked about there, and refused
/// one the person may not let an agent into, as one that is not on the desk at all.
#[test]
fn a_program_asks_for_a_workspace_the_person_may_grant() {
    let _one = ONE_SESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, questions, door) = session(root.path());

    let studio = Program::connect(&door, Some(STUDIO));
    let shown = asked(&mut session, &questions);
    assert_eq!(
        shown.title,
        "Work in “Studio” as the agent claude-code@host#1"
    );
    allow(&mut session, 0);
    assert!(studio.answers(&mut session)[0].get("result").is_some());
    let (_, _, doing) = seated_as(&mut session, "claude-code@host#1");
    assert_eq!(doing, "Studio", "where it asked, not Garden in front");

    for workspace in [ELSEWHERE, "00000000-0000-4000-8000-000000000000"] {
        let refused = Program::connect(&door, Some(workspace));
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
