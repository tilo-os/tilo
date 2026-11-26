//! The control protocol a program speaks for the session's pointer, keys and frames is served to a
//! program inside an app this session started, and to nothing else: an agent the person admitted is
//! no exception, so no tool acts through pixels or synthetic input.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_session::{AppSocket, Options, Session};

use crate::common::publisher;
use crate::common::{control, private_home};

/// A headless session with the MCP server open, and the questions it asks the person.
fn session(
    root: &std::path::Path,
    socket: &std::path::Path,
) -> (Session, tilo_service::AskReceiver) {
    let (asks, questions) = crate::common::person_asked();
    let session = crate::common::headless(Options {
        asks: Some((asks, questions.clone())),
        collab: false,
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(socket.to_path_buf()),
        ..Options::default()
    });
    (session, questions)
}

/// An agent's own program at the session's socket: it introduces itself, reads its answer, and stays connected
/// — its presence is what an agent's state is served for.
struct Program {
    served: mpsc::Receiver<Value>,
    leave: mpsc::Sender<()>,
    ended: std::thread::JoinHandle<()>,
}

impl Program {
    /// Connect to `socket` and ask to work here.
    fn connect(socket: &std::path::Path) -> Self {
        let socket = socket.to_path_buf();
        let (served, answered) = mpsc::channel();
        let (leave, staying) = mpsc::channel();
        let ended = std::thread::spawn(move || {
            let stream = UnixStream::connect(&socket).expect("the MCP server's socket");
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .expect("the program waits for the person");
            let mut writer = stream.try_clone().expect("a second handle on the socket");
            let mut reader = BufReader::new(stream);
            let introduced = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "Claude Code", "version": "2.1"}
                }
            });
            writer
                .write_all(format!("{introduced}\n").as_bytes())
                .expect("the socket reads");
            let mut line = String::new();
            reader.read_line(&mut line).expect("an answer");
            let answer: Value = serde_json::from_str(&line).expect("a JSON-RPC answer");
            let _ = served.send(answer);
            let _ = staying.recv_timeout(tilo_test_env::hang_bound());
        });
        Self {
            served: answered,
            leave,
            ended,
        }
    }

    /// End the program's connection, as closing its editor does.
    fn end(self) {
        let _ = self.leave.send(());
        let _ = self.ended.join();
    }
}

/// Press `effect` on the sheet publishing it, as the person would.
#[track_caller]
fn press(session: &mut Session, effect: &str, args: Value) {
    let Value::Object(args) = args else {
        panic!("not an object: {args}");
    };
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// Let the session run until `done` holds.
#[track_caller]
fn pump(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, "what the test asked for", |session| {
        done(session).then_some(())
    });
}

/// Ask to work here and allow it for this session: the program is served as the agent the person
/// admitted.
fn admit(session: &mut Session, questions: &tilo_service::AskReceiver, program: &Program) -> Value {
    pump(session, |_| questions.pending().len() == 1);
    crate::common::person_answers(session, crate::common::allowed_for(1));
    crate::common::wait_for(session, "the program at the socket to be served", |_| {
        program.served.try_recv().ok()
    })
}

/// A program that speaks the control protocol is served nothing here, whether or not the person
/// admitted it: an agent reads and acts through the actions a resource publishes, never through the
/// session's pointer, keys or a picture of its screen.
#[test]
fn a_control_connection_is_refused_even_from_an_admitted_agent() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let (mut session, questions) = session(root.path(), &socket);

    refused_control(&mut session, &socket, "before the person admitted anything");

    // The program asks to work here; the person allows it for this session.
    let program = Program::connect(&socket);
    let served = admit(&mut session, &questions, &program);
    assert!(
        served.pointer("/result/serverInfo").is_some(),
        "the program is served as the agent it was admitted as: {served}"
    );

    refused_control(&mut session, &socket, "with the agent admitted");
    program.end();
}

/// A client the person admits is their agent member for that client: the workspace lists it, so a
/// person's search finds it like any member, and its next connection is the same agent.
#[test]
fn an_admitted_client_is_an_agent_member_people_find() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let (mut session, questions) = session(root.path(), &socket);
    let program = Program::connect(&socket);
    let served = admit(&mut session, &questions, &program);
    assert!(served.pointer("/result/serverInfo").is_some(), "{served}");
    session
        .invoke(
            "desktop:chrome",
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("open workspace settings");
    session.step(Duration::from_millis(1)).expect("step");
    press(&mut session, "show-tab", json!({"tab": "agents"}));
    // The workspace lists it as the person's agent, as its Agents tab shows the record.
    let listed = crate::common::wait_for(&mut session, "the agent to be listed", |session| {
        session
            .workspace_settings()?
            .settings()
            .agents
            .agents
            .into_iter()
            .find(|agent| agent.name == "Claude Code")
    });
    assert_eq!(listed.key, "claude-code");
    assert_eq!(
        listed.status, "Added by you",
        "kept by the person who admitted it"
    );
    program.end();
}

/// A box that sends a message lists the workspace's agents and the people here: the client the
/// person admitted is an agent they can message, and the person at this session is a recipient of
/// their own workspace.
#[test]
fn a_send_lists_the_workspaces_agents_and_people() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let (mut session, questions) = session(root.path(), &socket);
    let program = Program::connect(&socket);
    let served = admit(&mut session, &questions, &program);
    assert!(served.pointer("/result/serverInfo").is_some(), "{served}");

    // The admission reaches the workspace's members after the program is served: the send lists
    // the agent once it is one.
    let recipients = crate::common::wait_for(
        &mut session,
        "the admitted client among the send's recipients",
        |session| {
            let reading = session.read("session:send-to")?;
            let recipients = reading.get("recipients")?.as_array()?.clone();
            recipients
                .iter()
                .any(|recipient| recipient["name"] == "Claude Code" && recipient["kind"] == "agent")
                .then_some(recipients)
        },
    );
    let mut seen: Vec<(serde_json::Value, serde_json::Value)> = Vec::new();
    for recipient in &recipients {
        let key = (recipient["kind"].clone(), recipient["id"].clone());
        assert!(!seen.contains(&key), "one row each: {recipients:?}");
        seen.push(key);
    }
    program.end();
}

/// Ask for a state on the control protocol and read the refusal: it names the protocol as served to
/// a program inside an app this session started, and to nothing else.
#[track_caller]
fn refused_control(session: &mut Session, socket: &std::path::Path, when: &str) {
    for request in [
        json!({"op": "agent_place"}),
        json!({"op": "pointer_move", "x": 40.0, "y": 24.0}),
        json!({"op": "click_at", "x": 2.0, "y": 2.0}),
        json!({"op": "type", "text": "hello"}),
    ] {
        let refused = control(session, socket, &request);
        assert_eq!(
            refused.pointer("/err/code").and_then(Value::as_str),
            Some("not_allowed"),
            "{when}, the control protocol answers no `{}`: {refused}",
            request
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        let message = refused
            .pointer("/err/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("a program running inside an app"),
            "it is told what the protocol is served to: {refused}"
        );
    }
}

/// What the shell says to the person now: the titles and bodies of its notices.
fn said(session: &Session) -> Vec<(String, String)> {
    session
        .notifications()
        .log()
        .active()
        .iter()
        .map(|notice| (notice.title.clone(), notice.body.clone()))
        .collect()
}

/// An answer this build cannot read costs the agent that answer — it is asked about again — and the
/// person is told once, naming it, instead of the agent silently losing what it was allowed.
#[test]
fn an_answer_this_build_cannot_read_is_asked_again_and_told_once() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    // What an older build kept: the client and the agent it asked, in a row this one does not read
    // (no program of theirs named).
    std::fs::write(
        root.path().join("mcp-server-bindings.json"),
        br#"{"bindings": [{"agent": "claude-code", "client": "Claude Code"}]}"#,
    )
    .expect("the answers kept");
    let socket = root.path().join("agent.sock");
    let mut session = crate::common::headless(Options {
        asks: Some(crate::common::person_asked()),
        collab: false,
        state: Some(root.path().to_path_buf()),
        app_socket: AppSocket::At(root.path().join("app_wire.sock")),
        mcp_server: Some(socket),
        ..Options::default()
    });

    pump(&mut session, |session| {
        said(session)
            .iter()
            .any(|(title, _)| title == "claude-code is asked again")
    });
    let about = |session: &Session| {
        said(session)
            .into_iter()
            .filter(|(title, _)| title == "claude-code is asked again")
            .collect::<Vec<_>>()
    };
    assert_eq!(
        about(&session),
        [(
            "claude-code is asked again".to_owned(),
            "This computer could not read that you let claude-code work here, so it \
             asks you again the next time claude-code connects. Nothing else was \
             removed."
                .to_owned()
        )]
    );
    // Told once, though the session's loop looks at the API every pass.
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert_eq!(about(&session).len(), 1, "said once");
}
