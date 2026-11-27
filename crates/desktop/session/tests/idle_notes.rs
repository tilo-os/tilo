//! An idle workspace wakes nobody.
//!
//! Notes runs in this device's own workspace: the device holds it, its records are sealed with
//! the keys the device holds, and the app reads and writes them through the session's
//! collaboration broker. With nothing typed and nothing arriving, no frame runs and the
//! collaboration threads sleep: the broker and the app's shovel stay asleep until someone writes.
//! A read re-checked on a clock, what the app publishes sent again every frame, or a write made by the app
//! itself and read again when it comes back shows here as a wake that no one asked for.
//!
//! The app runs on the process's own loop: frames while it owes any, then a wait on its wake, as
//! `tilolang-process` runs it. The window is the app process's own: it takes each batch the app
//! sends, composes the view again, and answers what its controls ask of the app, so a view that
//! sets its state on every render shows here as the write and the wake per frame it costs. What
//! is counted are the threads R09 counts: the compositor's collaboration broker and the app's
//! collaboration shovel, sampled by name over a settled idle window.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the app lives until its session ends"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tilo_compositor::OutputConfig;
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_session::{AppSocket, Options, Session};
use tilolang_process::TilolangProcess;
use tilolang_process::hosted::{Hosted, Kept, Spec};
use tilolang_process::phrases::Phrases;
use tilolang_process::wait::{Wake, wait};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::resources::ResourceStore;

use crate::common::NOTES;

/// The threads the idle window is counted over, as the kernel names them: a name is cut at
/// fifteen characters.
const BROKER: &str = "tilo-collab-bro";
const SHOVEL: &str = "ui-collab-shove";

/// How long the app is left alone before its wakes are counted, and over what window: the
/// workspace's enrolment and the notebook's first frames finish first, and an app that keeps
/// re-reading on a clock costs the same then as it did on its first frame.
const SETTLE: Duration = Duration::from_secs(2);
const WINDOW: Duration = Duration::from_secs(2);

/// Wakes one of those threads may take over the window, and frames the app may owe: an idle
/// workspace wakes nobody, so each is allowed a small count and no more.
const WAKES: u64 = 2;

static WOKEN: AtomicBool = AtomicBool::new(false);

/// Stops the app's thread when it is dropped, whatever ended the test's own: an assertion, a
/// panic, or the test's end.
struct Stopping(Arc<AtomicBool>);

impl Drop for Stopping {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Where the app's thread stands, for the test's own failure: what it was doing when the notebook
/// did not open.
fn at(stage: &std::sync::Mutex<String>, text: &str) {
    let mut stage = stage
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    stage.clear();
    stage.push_str(text);
}

/// The Notes package, as installed.
fn notes_bundle() -> Bundle {
    let catalog = package_store::Catalog::at(crate::common::app_store(), None);
    let installed = app_catalogue::apps::read(&catalog).expect("the installed apps");
    let notes = installed
        .iter()
        .find(|app| app.app_id == NOTES)
        .expect("Notes is installed");
    Bundle::read(&notes.payload_root).expect("Notes' payload")
}

/// Join Notes' route as the launcher hands a window its collaboration connection and identity: a
/// manager joined as that window of the app in the workspace.
fn join(
    socket: &std::path::Path,
    workspace: &str,
    workspace_id: uuid::Uuid,
    kind: &str,
) -> Result<collaboration::CollaborationManager, String> {
    let mut app_wire =
        std::os::unix::net::UnixStream::connect(socket).map_err(|error| error.to_string())?;
    launcher::services::write_collab_handoff(&mut app_wire, NOTES, workspace)
        .map_err(|error| error.to_string())?;
    let mut identity =
        std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
            .map_err(|error| error.to_string())?;
    launcher::services::write_identity_handshake(&mut identity, NOTES, workspace)
        .map_err(|error| error.to_string())?;
    collaboration::CollaborationManager::join_instance(
        &collab_types::document::ResourceType::new(kind.to_owned()),
        std::os::fd::OwnedFd::from(app_wire),
        &std::os::fd::OwnedFd::from(identity),
        collaboration::RouteSpec::app(NOTES.to_owned(), workspace_id),
    )
    .map_err(|error| format!("the window joins: {error:?}"))?
    .finish_join()
    .map_err(|error| format!("the window's route is joined: {error:?}"))
}

/// A session as a computer starts one: the shell of a device daemon of its own
/// ([`crate::common::on_its_daemon`]), which holds the person's own workspace and the keys its
/// records are sealed with; that workspace; and the person, as the app's window names them.
fn session(
    root: &std::path::Path,
    launches: Arc<tilo_session::Launches>,
) -> (Session, String, String) {
    let person = std::cell::RefCell::new(String::new());
    let mut session = crate::common::on_its_daemon_with(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| {
            person.replace(device.person.clone());
            Options {
                collab: true,
                state: Some(root.join("state")),
                // The development apps' records, kept where the daemon serves them: what the
                // shell and the app's own package read through the device, as a seeded session
                // has them.
                services: crate::common::seeded_apps_of(device),
                asks: Some(tilo_service::asks::channel()),
                app_socket: AppSocket::At(root.join("app_wire.sock")),
                mcp_server: Some(root.join("agent.sock")),
                api_socket: Some(root.join("api.sock")),
                launches,
                ..Options::default()
            }
        },
    );
    // The shell shows its workspace, and the session's sockets and services are up with it.
    crate::common::wait_for(&mut session, "the workspace the shell shows", |session| {
        session.handle().read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    });
    // The apps' records stand in the workspace the daemon serves before an app reads them.
    crate::common::kept_in_workspace(&mut session);
    // The person's own workspace, which this device keeps and their daemon hosts: the one an app
    // that keeps its records there founds on a computer that has none yet. A workspace the daemon
    // only lists, at a host this device is not, answers an app's records nothing.
    let person = person.into_inner();
    let workspace = collab_types::person_workspace_of(&person)
        .map(|workspace| workspace.to_string())
        .expect("the person's own workspace, from their root key");
    (session, workspace, person)
}

/// Run Notes in a workspace this device holds and seals, with nothing typed, and count what wakes
/// over a settled idle window: no frame of the app, no wake of the collaboration broker, and none
/// of the app's shovel.
#[test]
fn an_idle_notes_window_in_a_workspace_this_device_holds_wakes_nothing() {
    tilo_test_env::in_own_process(
        module_path!(),
        "an_idle_notes_window_in_a_workspace_this_device_holds_wakes_nothing",
        || {
            let _settled = tilo_ui_base::motion::testing::settled();
            let root = crate::common::private_home();
            // The apps this computer installed, before its daemon starts: the daemon serves an
            // app's records from the package it reads there.
            crate::common::app_store();
            let launches = Arc::new(tilo_session::Launches::default());
            let (mut session, workspace, person) = session(root.path(), Arc::clone(&launches));
            // The notebook a person reads: the notes their seed wrote, and a note whose body says
            // its sections, which they import themselves through the app's own action. The window
            // shows the tree of notes and the outline of the one open beside them.
            let notebook = session
                .resources()
                .into_iter()
                .map(|spec| spec.id.as_str().to_owned())
                .find(|id| id.starts_with(&format!("tl:{NOTES}:root-")))
                .expect("the Notes root is published");
            let imported = session
                .invoke(
                    &notebook,
                    "import_text",
                    serde_json::json!({
                        "title": "Autumn plan",
                        "markdown": "## Ship the editor\n\nOffline first, then sharing.\n\n\
                                     ## Grow the garden\n\nTomatoes along the south wall.",
                    })
                    .as_object()
                    .expect("an object"),
                )
                .unwrap_or_else(|error| panic!("the person imports a note: {error}"));
            assert!(
                imported.completed().is_some(),
                "the note is in the workspace before its window opens"
            );
            let socket = root.path().join("app_wire.sock");
            let workspace_id = uuid::Uuid::parse_str(&workspace).expect("a workspace id");
            let kind = format!("tl.{NOTES}.Notebook");
            // The package and the app data store are read here, on the thread that holds the
            // test environment: the app's thread must not reach for them.
            let bundle = notes_bundle();
            let (ready, frames) = std::sync::mpsc::channel::<u64>();
            let stop = Arc::new(AtomicBool::new(false));
            let app_stop = Arc::clone(&stop);
            let stage = Arc::new(std::sync::Mutex::new("not started".to_owned()));
            let app_stage = Arc::clone(&stage);
            launches.started(std::process::id(), NOTES, &workspace, true);
            std::thread::scope(|scope| {
                // The app's thread ends with `stop`, which this lets go of even when an assertion
                // below fails: a failed test reports instead of waiting on an app nobody stops.
                let _stopping = Stopping(Arc::clone(&stop));
                let app = scope.spawn(move || {
                    // The workspace's founding membership reaches the identity service a few
                    // turns after the shell shows the workspace, and the daemon's holder takes
                    // the workspace's records a moment after it starts: until either lands, the
                    // window's join is refused, and it joins again.
                    at(&app_stage, "joining the window");
                    // The connection wakes the app's loop and its store's waits whenever it has
                    // something new, as the process joins it: inside the wake scope bound to the
                    // loop's wake.
                    let wake = std::rc::Rc::new(Wake::new().expect("a wake"));
                    let mut wakes = tilo_rt::wake::WakeScope::new();
                    let woken = wake.waker();
                    wakes
                        .bind(move |_| woken())
                        .expect("the app's wake scope binds");
                    let until = tilo_test_env::hang_deadline();
                    let mut last = String::new();
                    let manager = loop {
                        let joined = {
                            let _in_scope = wakes.enter();
                            join(&socket, &workspace, workspace_id, &kind)
                        };
                        match joined {
                            Ok(manager) => break manager,
                            Err(why)
                                if Instant::now() < until
                                    && (why.contains("not a member")
                                        || why.contains("not available on this device")
                                        || why.contains("no identity to sign in with")) =>
                            {
                                if why != last {
                                    eprintln!("the window's join is refused for now: {why}");
                                    last = why;
                                }
                                std::thread::park_timeout(Duration::from_millis(250));
                            }
                            Err(why) => {
                                at(&app_stage, &format!("the window's join is refused: {why}"));
                                panic!("{why}");
                            }
                        }
                    };
                    at(&app_stage, "the window joined");
                    let connection =
                        tilolang_process::connection::Connection::new(manager).expect("joined");
                    let store = ResourceStore::new(std::rc::Rc::new(
                        connection.port(Some(std::rc::Rc::clone(&wake))),
                    ));
                    let content = tilolang_process::hosted::memory_content();
                    let mut host = TilolangProcess::new(wake.waker());
                    let kept = Kept {
                        content: Some(content.clone()),
                        ..Kept::default()
                    };
                    let (ours, theirs) = Channel::pair().expect("a window channel");
                    let watched = std::os::fd::AsFd::as_fd(&ours)
                        .try_clone_to_owned()
                        .expect("the window's end");
                    let spec = Spec::new(NOTES.to_owned(), workspace.clone(), 1, person);
                    host.add(
                        Hosted::new(
                            spec,
                            connection,
                            (store, content),
                            Arc::new(bundle),
                            &kept,
                            Box::new(ChannelTransport::new(ours).expect("the window")),
                            (&std::rc::Rc::new(Phrases::new("en-US")), host.formats()),
                            wake.waker(),
                        ),
                        Some(watched),
                    );
                    // The window is built from what the app sends, as the app process builds it:
                    // each batch it takes composes the view again, and what that asks of the app
                    // — a selection, the rows a list shows — goes back on the channel.
                    let window_stop = Arc::clone(&app_stop);
                    std::thread::spawn(move || {
                        let mut window = tilo_app_window::AppWindow::new(
                            Box::new(ChannelTransport::new(theirs).expect("the window")),
                            Vec::new(),
                        );
                        while !window_stop.load(Ordering::Relaxed) {
                            window.receive();
                            if window.owes() {
                                window.flush();
                            }
                            std::thread::park_timeout(Duration::from_millis(1));
                        }
                    });
                    // Opening the notebook is work the process owes; it is done before the idle
                    // window opens, so what is counted is what an idle app costs. A host that
                    // never stops owing one is measured too: it is the loop this test is about.
                    let settle_by = Instant::now() + Duration::from_secs(300);
                    at(&app_stage, "opening the notebook");
                    loop {
                        let turned = host.turn();
                        if Instant::now() > settle_by {
                            break;
                        }
                        if turned || host.owes_work() {
                            continue;
                        }
                        // Quiet: the window has taken the frames it was sent, and what it asks
                        // back of the app — the rows a list shows, the focus it moved — has
                        // reached it.
                        std::thread::park_timeout(Duration::from_millis(50));
                        if !host.turn() && !host.owes_work() {
                            break;
                        }
                    }
                    // Frames an idle app owes over the window: none, unless something wakes it.
                    let mut ran = 0_u64;
                    at(&app_stage, "settled");
                    ready.send(ran).expect("the sample");
                    while !app_stop.load(Ordering::Relaxed) {
                        let mut turned = host.turn();
                        while turned && !app_stop.load(Ordering::Relaxed) {
                            ran = ran.saturating_add(1);
                            turned = host.turn();
                        }
                        if app_stop.load(Ordering::Relaxed) || host.owes_work() {
                            continue;
                        }
                        let timeout = host
                            .wakes_in()
                            .map_or(50, |at| i32::try_from(at.as_millis().min(50)).unwrap_or(50));
                        WOKEN.store(false, Ordering::SeqCst);
                        let _ = wait(None, &wake, None, &host, timeout);
                        let _ = wake.consume();
                    }
                    let _ = ready.send(ran);
                });
                // The app's records and frames are the device daemon's, served on the daemon's
                // own threads: the session's loop steps again over the window this test counts,
                // so what is counted is what the app and the broker do beside it.
                let opens_within = tilo_test_env::hang_bound();
                if let Err(why) = frames.recv_timeout(opens_within) {
                    let stage = stage
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    match why {
                        std::sync::mpsc::RecvTimeoutError::Timeout => panic!(
                            "the notebook opens: the app's thread has been at {stage:?} for \
                             {opens_within:?}"
                        ),
                        std::sync::mpsc::RecvTimeoutError::Disconnected => panic!(
                            "the app's thread ended before the notebook opened: it was at {stage:?}"
                        ),
                    }
                }
                let pid = std::process::id();
                let settling = Instant::now();
                while settling.elapsed() < SETTLE {
                    session.step(Duration::from_millis(5)).expect("a frame");
                }
                let before =
                    tilo_test_env::idle::sample_tree(pid).expect("a sample of this process");
                let started = Instant::now();
                while started.elapsed() < WINDOW {
                    session.step(Duration::from_millis(5)).expect("a frame");
                }
                let after =
                    tilo_test_env::idle::sample_tree(pid).expect("a sample of this process");
                let idle = tilo_test_env::idle::Wakeups::between(before, after, started.elapsed());
                stop.store(true, Ordering::Relaxed);
                let ran = frames
                    .recv_timeout(Duration::from_secs(30))
                    .expect("the idle window closes");
                app.join().expect("the app's thread");
                let woke = |thread: &str| -> u64 {
                    idle.rows
                        .iter()
                        .filter(|row| row.thread == thread)
                        .map(tilo_test_env::idle::WakeupRow::wakeups)
                        .sum()
                };
                assert!(
                    woke(BROKER) <= WAKES,
                    "the collaboration broker woke {} times while the app idled:\n{idle}",
                    woke(BROKER)
                );
                assert!(
                    woke(SHOVEL) <= WAKES,
                    "the app's collaboration shovel woke {} times while it idled:\n{idle}",
                    woke(SHOVEL)
                );
                assert!(
                    ran <= WAKES,
                    "an idle Notes ran {ran} frames over {} s, waking nothing:\n{idle}",
                    WINDOW.as_secs()
                );
            });
        },
    );
}
