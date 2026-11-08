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

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::portal::PortalEffect;

use common::{HEIGHT, WIDTH, control, private_home};

/// A headless session with the agent door open, and its questions asked on the session's sheets.
fn session(root: &std::path::Path, socket: &std::path::Path) -> (Session, Questions) {
    let questions = Questions::new();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, questions)
}

/// An agent's own program at the door: it introduces itself, reads its answer, and stays connected
/// — its presence is what an agent's seat is served for.
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
            let stream = UnixStream::connect(&socket).expect("the agent door's socket");
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .expect("the program waits for the person");
            let mut writer = stream.try_clone().expect("a second handle on the door");
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
                .expect("the door reads");
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
    let mut args = match args {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    };
    let handle = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .map(|dialog| dialog.handle);
    if let Some(handle) = handle
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// Let the session run until `done` holds.
#[track_caller]
fn pump(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, "what the test asked for", |session| {
        done(session).then_some(())
    });
}

/// Ask to work here and allow it for this session: the program is served as the agent the person
/// admitted.
fn admit(session: &mut Session, questions: &Questions, program: &Program) -> Value {
    pump(session, |_| questions.pending().len() == 1);
    press(session, PortalEffect::SELECT.id, json!({"index": 1}));
    press(session, PortalEffect::CONFIRM.id, json!({}));
    common::wait_for(session, "the program at the door to be served", |_| {
        program.served.try_recv().ok()
    })
}

/// A program that speaks the control protocol is served nothing here, whether or not the person
/// admitted it: an agent reads and acts through the actions a resource publishes, never through the
/// session's pointer, keys or a picture of its screen.
#[test]
fn a_control_connection_is_refused_even_from_an_admitted_agent() {
    let _settled = ui::motion::testing::settled();
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

/// Ask for a seat on the control protocol and read the refusal: it names the protocol as served to
/// a program inside an app this session started, and to nothing else.
#[track_caller]
fn refused_control(session: &mut Session, socket: &std::path::Path, when: &str) {
    for request in [
        json!({"op": "agent_seat"}),
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

/// What the desk says to the person now: the titles and bodies of its notices.
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
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    // What an older build kept: the client and the agent it asked, in a row this one does not read
    // (no program of theirs named).
    std::fs::write(
        root.path().join("agent-door-bindings.json"),
        br#"{"bindings": [{"agent": "claude-code", "client": "Claude Code"}]}"#,
    )
    .expect("the answers kept");
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions),
            collab: false,
            state: Some(root.path().to_path_buf()),
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(socket),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");

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
    // Told once, though the session's loop looks at the door every pass.
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert_eq!(about(&session).len(), 1, "said once");
}
