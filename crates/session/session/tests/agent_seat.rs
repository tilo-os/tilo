//! An agent the person admitted drives a seat of its own through the agent door, in the
//! control protocol its computer tools speak: its pointer, the frames it sees, the window it
//! aims at — and nothing of the person's desk.

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

/// A headless session with the agent door open, and its questions asked on the session's
/// sheets.
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

/// An agent's own program at the door: it introduces itself, reads its answer, and stays
/// connected — its presence is what an agent's seat is served for.
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
                .set_read_timeout(Some(Duration::from_secs(30)))
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
            let _ = staying.recv_timeout(Duration::from_secs(30));
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
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// The agent's own seat, as its control connection reads it.
fn seat_of(session: &mut Session, socket: &std::path::Path) -> Value {
    control(
        session,
        socket,
        &json!({"op": "agent_seat", "agent": "claude-code"}),
    )
}

/// Ask to work here and allow it for this session: the program is served as
/// the agent the person admitted.
fn admit(session: &mut Session, questions: &Questions, program: &Program) -> Value {
    pump(session, |_| questions.pending().len() == 1);
    press(session, PortalEffect::SELECT.id, json!({"index": 1}));
    press(session, PortalEffect::CONFIRM.id, json!({}));
    let mut served = None;
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if let Ok(answer) = program.served.try_recv() {
            served = Some(answer);
            break;
        }
    }
    served.expect("the program at the door is served")
}

/// A frame of the seat's viewport, written as a PNG at `path`.
fn frame(session: &mut Session, socket: &std::path::Path, path: &std::path::Path) -> (u32, u32) {
    let shot = control(
        session,
        socket,
        &json!({
            "op": "snapshot",
            "agent": "claude-code",
            "path": path.to_str().expect("a path"),
        }),
    );
    assert_eq!(shot, json!({"ok": {"frame": 0}}), "the seat sees: {shot}");
    let image = image::open(path).expect("a PNG").into_rgba8();
    (image.width(), image.height())
}

/// Before the person admits anyone, a program that speaks the control protocol
/// is served nothing here: no seat is its own.
fn nothing_before_admission(session: &mut Session, socket: &std::path::Path) {
    let refused = control(session, socket, &json!({"op": "agent_seat"}));
    let code = refused.pointer("/err/code").and_then(Value::as_str);
    assert_eq!(
        code,
        Some("not_allowed"),
        "a program outside the door's agents has no seat: {refused}"
    );
    let message = refused.pointer("/err/message").and_then(Value::as_str);
    assert!(
        message.is_some_and(|message| message.contains("an agent the person admitted")),
        "it is told a seat is an admitted agent's: {refused}"
    );
}

#[test]
fn an_admitted_agents_control_connection_is_served_as_its_own_seat() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let (mut session, questions) = session(root.path(), &socket);

    nothing_before_admission(&mut session, &socket);

    // The program asks to work here; the person allows it for this session.
    let program = Program::connect(&socket);
    let served = admit(&mut session, &questions, &program);
    assert!(
        served.pointer("/result/serverInfo").is_some(),
        "the program is served as the agent it was admitted as: {served}"
    );

    // Its computer tools drive a seat of their own: a viewport on the screen it acts in.
    let seat = seat_of(&mut session, &socket);
    assert_eq!(
        seat["answer"]["viewport"],
        json!([0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)]),
        "the seat sees the whole screen it acts on: {seat}"
    );
    assert_eq!(seat["answer"]["agent"], "claude-code", "{seat}");

    // Its pointer moves, and the frames it sees come back as a PNG of that viewport.
    let moved = control(
        &mut session,
        &socket,
        &json!({
            "op": "pointer_move",
            "agent": "claude-code",
            "window": null,
            "x": 40.0,
            "y": 24.0,
        }),
    );
    assert_eq!(
        moved,
        json!({"ok": {}}),
        "the seat's pointer moves: {moved}"
    );

    let png = root.path().join("seat.png");
    assert_eq!(
        frame(&mut session, &socket, &png),
        (
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        ),
        "the frames are the viewport the seat was given"
    );

    // The seat has its own hands and nothing of the person's desk: the ops the person's doors
    // answer are refused here, by name.
    for (op, request) in [
        (
            "click_at",
            json!({"op": "click_at", "agent": "claude-code", "x": 2.0, "y": 2.0}),
        ),
        ("windows", json!({"op": "windows", "agent": "claude-code"})),
    ] {
        let refused = control(&mut session, &socket, &request);
        assert_eq!(
            refused["err"]["code"], "not_permitted",
            "a seat is not the person's desk: {op} {refused}"
        );
        assert!(
            refused["err"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(op)),
            "{op} is named in its refusal: {refused}"
        );
    }

    // The program leaves: the seat is no longer served on this pid.
    program.end();
    let mut refused = json!(null);
    for _ in 0..300 {
        session.step(Duration::from_millis(2)).expect("step");
        refused = seat_of(&mut session, &socket);
        if refused["err"]["code"] == "not_allowed" {
            break;
        }
    }
    assert_eq!(
        refused["err"]["code"], "not_allowed",
        "an ended agent's program has no seat: {refused}"
    );
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

/// An answer this build cannot read costs the agent that answer — it is asked
/// about again — and the person is told once, naming it, instead of the agent
/// silently losing what it was allowed.
#[test]
fn an_answer_this_build_cannot_read_is_asked_again_and_told_once() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    // What an older build kept: the client and the agent it asked, in a row
    // this one does not read (no program of theirs named).
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
