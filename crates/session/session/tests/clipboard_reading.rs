//! What the person copied is the session's own reading: the device panel
//! reads it over the wire, and an app the workspace does not grant is refused it
//! as it is refused an action.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use runtime_collab::CollabClient;
use tilo_compositor::clipboard_history::TEXT_MIME;
use tilo_compositor::{OutputConfig, Selection};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, CLIPBOARD_RESOURCE, Launches, Options, Session};

/// The app the person's device shows the session's own state in, and an app
/// of its own: what the workspace grants apart.
const PANEL: &str = "so.tilo.quick-settings";
const APP: &str = "so.tilo.settings";

/// A session serving the plane on `socket`, having started `app` in its one
/// workspace, which this test's own process runs.
fn session(socket: &std::path::Path, app: &str) -> (Session, String) {
    let wake = LoopWake::default();
    let launches = Arc::new(Launches::default());
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            wake,
            launches: Arc::clone(&launches),
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");
    // The app runs in the workspace the session opened its desk on, which is
    // the workspace it reads the session's resources in.
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });
    // The app speaks from this test's own process, so the launch the session
    // would have recorded for it names that process: what the broker admits
    // the connection by.
    launches.started(std::process::id(), app, &workspace, true);
    (session, workspace)
}

/// What the app read of the clipboard: its reading now, and the one its
/// watch began at, each the refusal when the session refused it.
#[derive(Debug)]
struct Seen {
    snapshot: Result<Option<String>, String>,
    watched: Result<Option<String>, String>,
}

/// The app's side of the wire: the launcher's handoff on a connection to
/// `socket`, then a read and a watch of the clipboard.
fn app(socket: &std::path::Path, id: &str, workspace: &str, seen: &std::sync::mpsc::Sender<Seen>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
        launcher::services::write_collab_handoff(&mut stream, id, workspace)
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = stream.into_split();
        let (client, carrying) = CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let services = client.services();
        let snapshot = match services.snapshot(CLIPBOARD_RESOURCE).await {
            Ok(snapshot) => Ok(snapshot.reading),
            Err(error) => Err(error.to_string()),
        };
        let watched = match services.watch(CLIPBOARD_RESOURCE).await {
            Ok(readings) => match readings.read().await {
                Ok(reading) => Ok(reading.reading),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        };
        let _ = seen.send(Seen { snapshot, watched });
    });
}

/// The session's pass until the app is done, as the session runs its loop.
fn until_the_app_is_done(session: &mut Session, seen: &std::sync::mpsc::Receiver<Seen>) -> Seen {
    common::wait_for(session, "the app to finish its calls", |session| {
        session.settle_services();
        session.step(std::time::Duration::from_millis(1)).unwrap();
        seen.try_recv().ok()
    })
}

/// Copy `text` in the app in front, and let the clipboard keep it.
fn copy(session: &mut Session, text: &str) {
    session
        .compositor()
        .set_selection(
            Selection::Clipboard,
            vec![(TEXT_MIME.to_owned(), text.as_bytes().to_vec())],
        )
        .expect("the app copies");
    for _ in 0..5 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// What the person copied reaches the panel's launch, and an app the workspace
/// grants nothing reads nothing.
#[test]
fn the_panel_reads_what_the_person_copied_and_an_app_is_refused_it() {
    let root = common::private_home();
    let panel_socket = root.path().join("panel.sock");
    let app_socket = root.path().join("app.sock");
    // Both apps speak from this process, and a session admits one app of it,
    // so each has a session of its own.
    let (mut panel_session, panel_room) = session(&panel_socket, PANEL);
    let (_app_session, app_room) = session(&app_socket, APP);
    copy(&mut panel_session, "the copied line");

    let (panel_sent, panel_received) = std::sync::mpsc::channel();
    let panel = std::thread::spawn(move || app(&panel_socket, PANEL, &panel_room, &panel_sent));
    let panel_seen = until_the_app_is_done(&mut panel_session, &panel_received);
    panel.join().expect("the panel's thread");

    // The app that was granted nothing is refused as it asks: its connection
    // arrives as the session starts, and the workspace's grants decide it before
    // the session's loop turns to serve anything of its own.
    let (sent, received) = std::sync::mpsc::channel();
    let ordinary = std::thread::spawn(move || app(&app_socket, APP, &app_room, &sent));
    let app_seen = received
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the app's calls are answered before the session's loop runs a pass");
    ordinary.join().expect("the app's thread");

    let reading = panel_seen
        .snapshot
        .as_ref()
        .expect("the panel reads what the person copied")
        .as_deref()
        .expect("what the person copied is a reading");
    let copies: serde_json::Value = serde_json::from_str(reading).expect("a reading is JSON");
    let listed = copies
        .as_array()
        .expect("the copies are a list")
        .iter()
        .any(|copy| copy["text"] == serde_json::json!("the copied line"));
    assert!(listed, "the line the person copied is the panel's to list");
    let watched = panel_seen
        .watched
        .as_ref()
        .expect("the panel watches the clipboard")
        .as_deref()
        .expect("the watch begins at a reading");
    assert_eq!(
        watched, reading,
        "the panel's watch begins where the reading stands"
    );
    for refused in [&app_seen.snapshot, &app_seen.watched] {
        let Err(refusal) = refused.as_ref() else {
            panic!("an ordinary app is refused the reading");
        };
        assert!(
            refusal.contains("not granted"),
            "refused as a reading nobody granted it: {refusal}"
        );
    }
}

/// A session with nobody attached: what the verbs of the clipboard can be
/// invoked on directly.
fn plain_session() -> Session {
    Session::headless(
        Options::default(),
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// The history as its reading lists it: each copy's key, its text and
/// whether the person keeps it, newest first.
fn listed(session: &Session) -> Vec<(String, Option<String>, bool)> {
    session
        .read(CLIPBOARD_RESOURCE)
        .expect("the session publishes the clipboard")
        .as_array()
        .expect("the history is a list")
        .iter()
        .map(|copy| {
            (
                copy["key"].as_str().expect("a key").to_owned(),
                copy["text"].as_str().map(str::to_owned),
                copy["pinned"].as_bool().expect("a pin"),
            )
        })
        .collect()
}

/// The key of the copy whose text is `text`.
fn key_of(session: &Session, text: &str) -> String {
    listed(session)
        .into_iter()
        .find(|(_, copied, _)| copied.as_deref() == Some(text))
        .map_or_else(|| panic!("{text} is not in the history"), |(key, _, _)| key)
}

fn invoke(session: &mut Session, action: &str, arguments: serde_json::Value) {
    session
        .invoke(
            CLIPBOARD_RESOURCE,
            action,
            &serde_json::from_value(arguments).expect("an object of arguments"),
        )
        .unwrap_or_else(|error| panic!("{action}: {error}"));
}

/// What the person does with the history is what the clipboard resource
/// offers: keeping a copy, dropping one, and clearing the rest.
#[test]
fn the_history_is_pinned_removed_and_cleared_through_its_published_verbs() {
    let mut session = plain_session();
    // The history is the device's, kept across sessions: what this test
    // copied is named so a run never reads another's copies.
    let kept_text = format!("keep me {}", std::process::id());
    let dropped_text = format!("let me go {}", std::process::id());
    copy(&mut session, &kept_text);
    copy(&mut session, &dropped_text);
    let kept = key_of(&session, &kept_text);
    let dropped = key_of(&session, &dropped_text);

    invoke(
        &mut session,
        "pin",
        serde_json::json!({ "copy": kept, "pinned": true }),
    );
    assert!(
        listed(&session).contains(&(kept.clone(), Some(kept_text.clone()), true)),
        "the copy the person kept is pinned: {:?}",
        listed(&session)
    );

    invoke(
        &mut session,
        "remove",
        serde_json::json!({ "copy": dropped }),
    );
    assert!(
        !listed(&session).iter().any(|(key, _, _)| *key == dropped),
        "the copy they dropped is gone: {:?}",
        listed(&session)
    );

    invoke(&mut session, "clear", serde_json::json!({}));
    let left = listed(&session);
    assert!(
        left.contains(&(kept, Some(kept_text), true)),
        "clearing the history keeps what the person pinned: {left:?}"
    );
    assert!(
        left.iter().all(|(_, _, pinned)| *pinned),
        "nothing the person did not pin is kept: {left:?}"
    );
}
