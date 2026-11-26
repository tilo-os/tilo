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

/// The file store this process keeps its secrets in: a process opens the store
/// `TILO_SECRETS_DIR` names once, so every session here keeps its channel keys in the one store.
fn secrets() -> &'static std::path::Path {
    static STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-test-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the secret store's directory");
        dir
    })
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

/// A session that founds its workspace on its own service and holds it, its records sealed with
/// the channel keys the device has, and the workspace it shows.
fn session(
    home: &mut tilo_test_env::TestHome,
    root: &std::path::Path,
    launches: Arc<tilo_session::Launches>,
) -> (Session, String) {
    // The account's own store below the identity directory, where every reader looks, and the
    // person's devices a certificate is checked against before anything signs with it.
    tilo_identity::sources::test_trust::install();
    let identity = tilo_identity::store::account_identity_dir(&home.config().join("tilo/identity"));
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let system = home.root().join("system");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set("TILO_SECRETS_DIR", secrets());
    // This process stands in for the machine's daemon: it takes the device key first, so the
    // identity is sealed with it.
    crate::common::take_device_key(&tilo_identity::provisioning::collaboration_identity_paths());
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    let mut session = crate::common::headless(Options {
        person: crate::common::PERSON.to_owned(),
        collab: true,
        state: Some(home.state().to_path_buf()),
        services: Vec::new(),
        asks: Some(tilo_service::asks::channel()),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(root.join("agent.sock")),
        api_socket: Some(root.join("api.sock")),
        launches,
        ..Options::default()
    });
    let workspace =
        crate::common::wait_for(&mut session, "the workspace the shell shows", |session| {
            session.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        });
    (session, workspace)
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
            let mut home = tilo_test_env::TestHome::new().expect("a home");
            let launches = Arc::new(tilo_session::Launches::default());
            let (mut session, workspace) = session(&mut home, root.path(), Arc::clone(&launches));
            let socket = root.path().join("app_wire.sock");
            let workspace_id = uuid::Uuid::parse_str(&workspace).expect("a workspace id");
            let kind = format!("tl.{NOTES}.Notebook");
            // The package and the app data store are read here, on the thread that holds the
            // test environment: the app's thread must not reach for them.
            let bundle = notes_bundle();
            let (ready, frames) = std::sync::mpsc::channel::<u64>();
            let stop = Arc::new(AtomicBool::new(false));
            let app_stop = Arc::clone(&stop);
            launches.started(std::process::id(), NOTES, &workspace, true);
            std::thread::scope(|scope| {
                let app = scope.spawn(move || {
                    // The workspace's founding membership reaches the identity service a few
                    // turns after the shell shows the workspace: until then the window's join is
                    // refused, and it joins again.
                    let until = Instant::now() + Duration::from_secs(120);
                    let manager = loop {
                        match join(&socket, &workspace, workspace_id, &kind) {
                            Ok(manager) => break manager,
                            Err(why) if why.contains("not a member") && Instant::now() < until => {
                                std::thread::park_timeout(Duration::from_millis(100));
                            }
                            Err(why) => panic!("{why}"),
                        }
                    };
                    let connection =
                        tilolang_process::connection::Connection::new(manager).expect("joined");
                    let wake = std::rc::Rc::new(Wake::new().expect("a wake"));
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
                    let spec = Spec::new(
                        NOTES.to_owned(),
                        workspace.clone(),
                        1,
                        crate::common::PERSON,
                    );
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
                    // The notebook a person reads: a note whose body says its sections, as the
                    // app's own scenario seeds one. The window shows the tree of notes and the
                    // outline of the one open beside them.
                    for instance in host.instances_mut() {
                        instance
                            .stage("the notebook and a note's outline")
                            .unwrap_or_else(|why| panic!("{why}"));
                    }
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
                crate::common::steps_within(&mut session, Duration::from_secs(900), |_| {
                    frames.try_recv().ok()
                })
                .expect("the notebook opens");
                let pid = std::process::id();
                // The session's own loop serves the app's store as it steps, as a running
                // session's does: the session is stepped through the settle and the window alike,
                // so what is counted is what the app and the broker do beside it.
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
