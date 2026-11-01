//! An ask goes in through the door and comes back as a run an app follows:
//! the composer queues what a person typed, watches the run it started, hears
//! what its agent says as it says it, steers it, and stops it.
//!
//! The test plays the agent's side of the run channel — the same calls the
//! broker makes when an agent accepts a run, reports its progress and ends —
//! while the ask, the steering and the stop go through the session's action
//! door, as an app's do.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::sync::{Arc, Mutex};

use runtime_collab::{CollabClient, TaskPresentation, TaskRunUpdate};
use serde_json::{Map, Value, json};
use tilo_agent_runs::AgentRuns;
use tilo_agent_runs::wire;
use tilo_compositor::OutputConfig;
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Options, Session};

/// What an app saw of one run when it changed.
#[derive(Clone, Debug, PartialEq)]
struct Observed {
    run: String,
    phase: String,
    agent: String,
    quick: bool,
    messages: u64,
    incorporated: u64,
    said: Option<String>,
    /// What the run streams of its model turn now.
    live: Option<String>,
}

impl Observed {
    fn of(run: &str, reading: &Value) -> Self {
        Self {
            run: run.to_owned(),
            phase: reading["phase"].as_str().unwrap_or_default().to_owned(),
            agent: reading["agent"].as_str().unwrap_or_default().to_owned(),
            quick: reading["quick"].as_bool().unwrap_or(false),
            messages: reading["messages"].as_u64().unwrap_or_default(),
            incorporated: reading["incorporated"].as_u64().unwrap_or_default(),
            said: reading["said"].as_str().map(ToOwned::to_owned),
            live: reading["live"]["text"].as_str().map(ToOwned::to_owned),
        }
    }
}

/// What the app asked for, and what the session answered.
struct Asked {
    /// The run of the ask it follows to its result.
    run: wire::Asked,
    /// The run of the ask it stops.
    stopped: wire::Asked,
    /// What the root read before anything was asked.
    before: Option<Value>,
    /// What stopping the second run answered.
    stopped_with: runtime_collab::wire::Receipt,
}

/// The session the app talks to: headless, with the runs this test plays the
/// agents' side of, and the plane on a socket of its own.
fn session(socket: &std::path::Path, runs: Arc<AgentRuns>) -> Session {
    let wake = LoopWake::default();
    // The app speaks from this test's own process, so the launch the session
    // would have recorded for it names that process: what the broker admits
    // the connection by.
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    launches.started(
        std::process::id(),
        "so.tilo.settings",
        "the-workspace",
        true,
    );
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            wake,
            launches,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            agent_runs: Some(runs),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// The app's side of the wire: the launcher's handoff, then the two asks, the
/// stop, and the runs watched until both ended.
fn app(
    socket: &std::path::Path,
    asked: &std::sync::mpsc::Sender<Asked>,
    seen: &Arc<Mutex<Vec<Observed>>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
        launcher::services::write_collab_handoff(&mut stream, "so.tilo.settings", "the-workspace")
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = stream.into_split();
        let (client, carrying) = CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let services = client.services();

        let before = services
            .snapshot(wire::RESOURCE)
            .await
            .expect("the session reads the runs it keeps")
            .reading
            .map(|reading| reading_json(&reading));
        let run = ask(&services, "Tidy the notes in the workspace", "agent:claude").await;
        let stopped = ask(&services, "Summarise the meeting", "claude").await;
        let following = services
            .watch(&run.run)
            .await
            .expect("the session watches the run it started");
        // The other run is stopped before anyone follows it: a watch begins
        // where its resource stands, so the app reads it already ended.
        let stopped_with = services
            .invoke(&stopped.run, "stop-run", &Map::new())
            .await
            .expect("the session stops the run");
        let ending = services
            .watch(&stopped.run)
            .await
            .expect("the session watches the other run");
        let _ = asked.send(Asked {
            run: run.clone(),
            stopped: stopped.clone(),
            before,
            stopped_with,
        });
        // What the stopped run stands at, read once: it never changes again.
        let stood = ending.read().await.expect("the reading of the run stopped");
        if let Some(reading) = stood.reading.as_deref() {
            observe(seen, &stopped.run, &reading_json(reading));
        }

        // The app follows the run going, telling the session what the person
        // typed to it, as the composer does, and reading what its agent says
        // until the run ended.
        let mut steered = false;
        for _ in 0..2_000 {
            let followed = following
                .read()
                .await
                .expect("the reading of the run followed");
            let Some(reading) = followed.reading.as_deref() else {
                continue;
            };
            let reading = reading_json(reading);
            observe(seen, &run.run, &reading);
            if !steered && reading["phase"] == json!("executing") {
                let mut arguments = Map::new();
                arguments.insert("text".into(), json!("Also check the calendar"));
                let receipt = services
                    .invoke(&run.run, "send-message", &arguments)
                    .await
                    .expect("the session steers the run");
                assert!(
                    matches!(receipt, runtime_collab::wire::Receipt::Done(_)),
                    "the run took the message: {receipt:?}"
                );
                steered = true;
            }
            if reading["phase"] == json!("done") {
                return;
            }
        }
        panic!("the run never ended");
    });
}

/// Queue one ask through the door, and read what run it started.
async fn ask(services: &runtime_collab::ServicesHandle, title: &str, agent: &str) -> wire::Asked {
    let mut arguments = Map::new();
    arguments.insert("title".into(), json!(title));
    arguments.insert("agent".into(), json!(agent));
    if title.starts_with("Summarise") {
        arguments.insert("quick".into(), json!(true));
    }
    let receipt = services
        .invoke(wire::RESOURCE, "ask", &arguments)
        .await
        .expect("the session queues the ask");
    let runtime_collab::wire::Receipt::Done(output) = receipt else {
        panic!("the ask names the run it started: {receipt:?}");
    };
    let output: tilo_actions::ActionOutput =
        serde_json::from_slice(&output).expect("the action's output");
    let asked: wire::Asked = serde_json::from_value(output.value.expect("the run it started"))
        .expect("the ask's answer");
    assert_eq!(
        output.resources,
        vec![asked.run.clone()],
        "the answer names the run's own resource"
    );
    asked
}

fn reading_json(reading: &str) -> Value {
    serde_json::from_str(reading).expect("a reading is JSON")
}

/// Record what the app saw of `run`, once per change.
fn observe(seen: &Arc<Mutex<Vec<Observed>>>, run: &str, reading: &Value) {
    let observed = Observed::of(run, reading);
    let mut seen = seen.lock().unwrap();
    if seen.iter().rev().find(|held| held.run == run) == Some(&observed) {
        return;
    }
    seen.push(observed);
}

/// What the app last saw of `run`.
fn last(seen: &Arc<Mutex<Vec<Observed>>>, run: &str) -> Option<Observed> {
    seen.lock()
        .unwrap()
        .iter()
        .rev()
        .find(|held| held.run == run)
        .cloned()
}

/// The phases the app saw `run` in, in the order it saw them: where the run
/// stood each time what it read changed.
fn phases(seen: &Arc<Mutex<Vec<Observed>>>, run: &str) -> Vec<String> {
    let mut phases: Vec<String> = Vec::new();
    for held in seen.lock().unwrap().iter().filter(|held| held.run == run) {
        if phases.last() != Some(&held.phase) {
            phases.push(held.phase.clone());
        }
    }
    phases
}

/// The session's passes until `done` holds of what the app saw.
fn until(
    session: &mut Session,
    seen: &Arc<Mutex<Vec<Observed>>>,
    what: &str,
    done: impl Fn(&[Observed]) -> bool,
) {
    for _ in 0..2_000 {
        session.settle_services();
        session.step(std::time::Duration::from_millis(1)).unwrap();
        if done(&seen.lock().unwrap()) {
            return;
        }
    }
    panic!("the app never saw {what}: {:?}", seen.lock().unwrap());
}

/// A directory for the plane's socket that only this user reaches: the broker
/// refuses to serve one anybody else could open.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

#[test]
fn an_ask_queued_through_the_door_is_a_run_the_session_follows_to_its_result() {
    let directory = private_home();
    let socket = directory.path().join("collab.sock");
    let runs = Arc::new(AgentRuns::new(Some(LoopWake::default().handle())));
    let mut session = session(&socket, Arc::clone(&runs));
    let room = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
        })
        .expect("the session is in a workspace");

    let seen: Arc<Mutex<Vec<Observed>>> = Arc::default();
    let (asked, received) = std::sync::mpsc::channel();
    let app = {
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || app(&socket, &asked, &seen))
    };
    let mut asked = None;
    for _ in 0..4000 {
        session.settle_services();
        session.step(std::time::Duration::from_millis(1)).unwrap();
        if let Ok(asked_now) = received.try_recv() {
            asked = Some(asked_now);
            break;
        }
    }
    let asked = asked.expect("the app queued its asks");

    // Nothing was asked before: the runs are empty, and both asks started one.
    assert_eq!(
        asked.before,
        Some(json!({ "runs": [] })),
        "a session nobody asked anything of keeps no runs"
    );
    assert!(
        asked.run.run.starts_with(wire::RUN_PREFIX),
        "an ask answers the run it started: {}",
        asked.run.run
    );
    assert_eq!(
        asked.run.issue,
        wire::issue(asked.run.run.trim_start_matches(wire::RUN_PREFIX)),
        "the run channel names the run by the work it does"
    );

    // The agent takes it up, and the app following it sees that.
    runs.absorb_report(&room, &asked.run.issue, 1, &TaskRunUpdate::Accepted);
    until(&mut session, &seen, "the run accepted", |seen| {
        seen.iter().any(|run| run.phase == "accepted")
    });

    // It works, streaming what it says, and the app sees both.
    let showing = TaskPresentation {
        run: format!("{}/0", asked.run.issue),
        sequence: 0,
        received_at: None,
        text: "Writing the summary".into(),
        reasoning: String::new(),
        finish: None,
        detail: String::new(),
    };
    runs.absorb_presentation(&room, &asked.run.issue, 1, Some(&showing));
    runs.absorb_report(
        &room,
        &asked.run.issue,
        1,
        &TaskRunUpdate::Progress {
            message: "read three notes".into(),
            presentation: None,
        },
    );
    until(&mut session, &seen, "the run working", |seen| {
        seen.iter().any(|run| {
            run.phase == "executing" && run.live.as_deref() == Some("Writing the summary")
        })
    });

    // The person steers it from the composer, and its agent takes the message
    // in: the app reads both from the run it is following.
    until(&mut session, &seen, "the message sent to the run", |seen| {
        seen.iter().any(|run| run.messages == 1)
    });
    runs.absorb_report(
        &room,
        &asked.run.issue,
        1,
        &TaskRunUpdate::DirectionsApplied { revision: 1 },
    );
    until(&mut session, &seen, "the message taken in", |seen| {
        seen.iter().any(|run| run.incorporated == 1)
    });

    // It ends its turn, the turn's last snapshot included.
    runs.absorb_report(
        &room,
        &asked.run.issue,
        1,
        &TaskRunUpdate::Succeeded {
            message: "tidied three notes".into(),
        },
    );
    runs.absorb_presentation(&room, &asked.run.issue, 1, None);
    until(&mut session, &seen, "the run done", |seen| {
        seen.iter().any(|run| run.phase == "done")
    });

    // The other run was stopped through the door, and stayed stopped: its
    // agent was never delivered it.
    until(&mut session, &seen, "the run stopped", |seen| {
        seen.iter().any(|run| run.phase == "stopped")
    });
    app.join().expect("the app's thread");

    let followed = phases(&seen, &asked.run.run);
    assert_eq!(
        followed,
        vec!["approved", "accepted", "executing", "done"],
        "the app followed the run from the ask to its result"
    );
    assert_eq!(
        phases(&seen, &asked.stopped.run),
        vec!["stopped"],
        "the stopped run never went anywhere"
    );
    assert!(
        matches!(asked.stopped_with, runtime_collab::wire::Receipt::Done(_)),
        "stopping it is an action that finished: {:?}",
        asked.stopped_with
    );

    let ended = last(&seen, &asked.run.run).expect("what the app last saw");
    assert_eq!(
        ended.agent, "claude",
        "the agent is named by its bare id, however it was typed"
    );
    assert_eq!(
        ended.said.as_deref(),
        Some("tidied three notes"),
        "the app reads what its agent said last"
    );
    assert_eq!(ended.live, None, "the turn ended, so nothing streams");
    let stopped = last(&seen, &asked.stopped.run).expect("the stopped run");
    assert!(stopped.quick, "a quick ask is a question");
    assert_eq!(stopped.messages, 0, "nobody steered the stopped run");

    let published = session
        .read(&asked.run.run)
        .expect("the session publishes the run");
    assert_eq!(published["phase"], json!("done"));
    assert_eq!(
        published["steps"],
        json!(["read three notes", "tidied three notes"]),
        "the app's own steps are read back from the run"
    );
    assert_eq!(
        published["title"],
        json!("Tidy the notes in the workspace"),
        "the run is what was asked"
    );
}
