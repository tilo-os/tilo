//! A kept notification handed to an agent: the agent live in the workspace in
//! front takes it as the task it acts on, as the person handing it over from
//! the notification centre asks.

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
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_desktop_surface::notifications::Notification;
use tilo_service::Questions;
use tilo_session::{AGENT_RUNS_RESOURCE, ActionPlane, NOTIFICATIONS_RESOURCE, Options, Session};
use tilo_sheets::portal::PortalEffect;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;

/// A directory only this user reaches: the session refuses to serve its
/// sockets where anybody else could open them.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// Run the session until `done` says so.
#[track_caller]
fn pump(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, "what the test asked for", |session| {
        done(session).then_some(())
    });
}

/// What `done` reads, once the session reaches it.
#[track_caller]
fn until<T>(session: &mut Session, mut done: impl FnMut(&Session) -> Option<T>) -> T {
    common::wait_for(session, "what the test asked for", |session| done(session))
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
    let mut args: Map<String, Value> = match args {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    };
    if let Some(handle) = session
        .portal()
        .expect("the portal's sheet")
        .dialog()
        .map(|dialog| dialog.handle)
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// A program of this login at the agent door: it says who it is, and asks what
/// the session has for it. What it read comes back, however far it got, and it
/// stays seated until `leave` lets it go.
fn door_client(socket: &Path, answers: &mpsc::Sender<Vec<Value>>, leave: &mpsc::Receiver<()>) {
    let stream = UnixStream::connect(socket).expect("the agent door's socket");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("the client waits for the person");
    let mut writer = stream.try_clone().expect("a second handle on the door");
    let mut reader = BufReader::new(stream);
    let mut read = Vec::new();
    for ask in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "2.1"}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "tilo_ls", "arguments": {}}
        }),
    ] {
        if writer.write_all(format!("{ask}\n").as_bytes()).is_err() {
            break;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => read.push(
                serde_json::from_str(&line)
                    .unwrap_or_else(|_| panic!("not a JSON-RPC answer: {line:?}")),
            ),
        }
    }
    let _ = answers.send(read);
    let _ = leave.recv();
}

#[test]
fn a_notification_handed_to_a_live_agent_becomes_its_task() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");

    // The person lets the program work here as its agent: from then on the
    // desk's workspace holds it.
    let (answers, received) = mpsc::channel();
    let (leaving, leave) = mpsc::channel();
    let client = std::thread::spawn(move || door_client(&socket, &answers, &leave));
    pump(&mut session, |_| !questions.pending().is_empty());
    press(&mut session, PortalEffect::SELECT.id, json!({"index": 1}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let read = received
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the program at the door is served");
    assert!(
        read[1].get("error").is_none(),
        "it works here as the agent it was admitted as: {read:?}"
    );

    // A notice from an app, pushed off the banners by the ones after it: the
    // centre keeps it, and offers the agent that works here.
    let kept = session
        .notifications()
        .post(Notification::new("Invoice", "From Ada"));
    for title in [
        "Receipt",
        "Reminder",
        "Statement",
        "Notice",
        "Update",
        "Digest",
    ] {
        session
            .notifications()
            .post(Notification::new(title, "From Ada"));
    }
    let centre = until(&mut session, |session| {
        let count = session
            .read(NOTIFICATIONS_RESOURCE)?
            .get("count")?
            .as_u64()?;
        (count > 0).then_some(count)
    });
    assert!(centre > 0, "the centre kept the one pushed off the stack");

    // The person presses the agent on the kept one's row in the centre.
    session.notifications().hand_to_agent(kept);
    let run = until(&mut session, |session| {
        let runs = session.read(AGENT_RUNS_RESOURCE)?;
        let runs = runs.get("runs")?.as_array()?;
        runs.iter()
            .find(|run| {
                run.get("agent").and_then(Value::as_str) == Some("claude-code")
                    && run.get("title").and_then(Value::as_str) == Some("Invoice")
            })
            .cloned()
    });
    assert_eq!(
        run.get("instruction").and_then(Value::as_str),
        Some("Invoice\n\nFrom Ada"),
        "the agent reads the whole notification"
    );
    assert_eq!(
        run.get("quick").and_then(Value::as_bool),
        Some(true),
        "a hand-off is a quick task, answered once"
    );
    leaving.send(()).expect("the program leaves");
    client.join().expect("the program's thread");
}
