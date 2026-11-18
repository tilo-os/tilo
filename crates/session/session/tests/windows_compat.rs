//! The Windows Apps manager reaches this session's compatibility service through the launcher's
//! handoff, and a Windows program it starts shows its window on the session's display.
//!
//! Real here: the session, its compositor, the service table that starts the compatibility service
//! before the display is bound, the launcher's handoff code, the manager's protocol client, the
//! service's authentication of the connecting process and its capsule planning. Stood in: the
//! launcher process (this test binary, started by the test as the session starts its apps), the
//! runtime payloads (taken as present), and the Bubblewrap executor, which opens a Wayland window
//! on the display the plan binds instead of starting Wine.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compat_catalogue::{ApplicationQuery, Catalogue, VmFallback};
use compat_service::{
    Assembly, CapsuleCommandPlan, CapsuleProgram, CapsuleStore, ExecutionError, ExecutionOutcome,
    LaunchLease, LaunchResourceBroker, LazyVmDelegate, PROTOCOL_VERSION, ProgramKind,
    ProtocolRequest, ResourceError, ResourceRequest, SandboxExecutor, SessionClient,
    StartedProgram, SystemRuntimeRegistry,
};
use serde_json::{Value, json};
use tilo_session::apps::Apps;
use tilo_session::{AppSocket, Launches, Options};
use tilo_test_env::SpawnUnderEnvLock as _;

/// The Windows Apps manager.
const MANAGER: &str = "so.tilo.windows";
/// The window the stand-in for a Windows program opens.
const PROGRAM_WINDOW: &str = "org.example.WindowsProbe";
/// What makes this test binary a launcher of the manager: the directory it reads its workspace and
/// entry from and writes the service's answers to.
const MANAGER_DIR: &str = "TILO_TEST_WINDOWS_MANAGER";

/// The answer to `request` on `client`, as JSON, or the error that ended the connection.
fn ask(client: &mut SessionClient, request: &ProtocolRequest) -> Value {
    match client.request(request) {
        Ok(response) => serde_json::to_value(response).expect("a response serializes"),
        Err(error) => json!({ "refused": error.to_string() }),
    }
}

/// A connection to the compatibility service made by the launcher's own handoff code, for the
/// manager in `workspace`.
fn connect(workspace: &str) -> SessionClient {
    let services = BTreeMap::from([(
        "compat".to_owned(),
        launcher::manifest::ServiceReq::parse("compat", "optional@31").expect("a service"),
    )]);
    let mut resolved = launcher::services::resolve_service_binds_for_launch(
        &services,
        false,
        launcher::services::ServiceLaunchContext {
            app_id: MANAGER,
            workspace_id: Some(workspace),
        },
    )
    .expect("the launcher hands the service off");
    assert_eq!(resolved.len(), 1, "the compatibility service is served");
    SessionClient::from_owned_fd(resolved.remove(0).host)
}

/// Not a test: a launcher of the Windows Apps manager, as `tilo-launcher` connects it. It lists,
/// starts the entry it was given, lists while the program is open and until it ended, and connects a
/// second time, writing what it was answered.
#[test]
fn manager_process() {
    let Ok(dir) = std::env::var(MANAGER_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let workspace = fs::read_to_string(dir.join("workspace")).expect("the workspace");
    let entry_id = fs::read_to_string(dir.join("entry")).unwrap_or_default();
    let mut client = connect(&workspace);
    let listed = ask(
        &mut client,
        &ProtocolRequest::List {
            version: PROTOCOL_VERSION,
        },
    );
    let list = ProtocolRequest::List {
        version: PROTOCOL_VERSION,
    };
    let (mut launched, mut running, mut ended) = (Value::Null, Value::Null, Value::Null);
    if !entry_id.is_empty() {
        launched = ask(
            &mut client,
            &ProtocolRequest::Launch {
                version: PROTOCOL_VERSION,
                entry_id,
            },
        );
        running = ask(&mut client, &list);
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let answer = ask(&mut client, &list);
            if answer["ended"]
                .as_array()
                .is_none_or(|ended| !ended.is_empty())
            {
                ended = answer;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    drop(client);
    let again = ask(
        &mut connect(&workspace),
        &ProtocolRequest::List {
            version: PROTOCOL_VERSION,
        },
    );
    fs::write(
        dir.join("answers"),
        json!({
            "listed": listed,
            "launched": launched,
            "running": running,
            "ended": ended,
            "again": again
        })
        .to_string(),
    )
    .expect("the answers written");
}

/// Grants nothing beyond what the plan carries.
struct NoResources;

impl LaunchResourceBroker for NoResources {
    fn materialize(&self, _: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
        Ok(LaunchLease::deny_all("lease"))
    }

    fn revoke(&self, _: &str) {}
}

/// Stands in for Bubblewrap and Wine: opens a window on the display the plan binds into the
/// sandbox, and ends with status 7 once the test saw it.
struct WindowOnPlannedDisplay {
    planned: Arc<Mutex<Option<PathBuf>>>,
    seen: Arc<AtomicBool>,
}

/// The stand-in's program: it shows its window while the service waits for its end.
struct ProbeWindow {
    display: PathBuf,
    seen: Arc<AtomicBool>,
}

impl SandboxExecutor for WindowOnPlannedDisplay {
    fn start(&self, plan: &CapsuleCommandPlan) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        let display = plan
            .bwrap_argv
            .windows(3)
            .find(|bind| bind[0] == "--ro-bind" && bind[2].ends_with("/wayland-0"))
            .map(|bind| PathBuf::from(&bind[1]))
            .expect("the plan binds a display");
        *self.planned.lock().unwrap() = Some(display.clone());
        Ok(Box::new(ProbeWindow {
            display,
            seen: Arc::clone(&self.seen),
        }))
    }
}

impl StartedProgram for ProbeWindow {
    #[allow(
        clippy::panic_in_result_fn,
        reason = "a stand-in that fails its step is the test's assertion"
    )]
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        let mut window = crate::common::window::Client::on(&self.display, PROGRAM_WINDOW, "Probe");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !self.seen.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "the test saw no window"
            );
            window.exchange();
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(window);
        Ok(ExecutionOutcome {
            success: false,
            exit_code: Some(7),
        })
    }

    fn stop(&self) {}
}

/// The catalogue routing `probe.exe` to Wine, and a capsule holding it, set up under the login's
/// data directory; the manager's entry for it.
fn set_up_probe(data: &Path) -> (Catalogue, String) {
    let catalogue = Catalogue::from_json(
        r#"{
          "schema_version": 1,
          "records": [{
            "id": "probe",
            "version": 1,
            "provenance": "built_in",
            "signed": true,
            "matches": [{"executable": "probe.exe"}],
            "recipe": {
              "layer": "wine",
              "runner": {"family": "wine", "version": "system"},
              "settings": {},
              "security_profile": "windows-app"
            }
          }]
        }"#,
    )
    .expect("a catalogue");
    let query = ApplicationQuery::executable("probe.exe");
    let store = CapsuleStore::new(data.join("tilo/windows-compat")).expect("a capsule store");
    let route = catalogue
        .resolver()
        .expect("a resolver")
        .resolve(&query, VmFallback::Disabled);
    let capsule = store.ensure_for_plan(&route).expect("a capsule");
    store
        .register_program(
            &capsule.id,
            CapsuleProgram {
                id: "probe".to_owned(),
                display_name: "Probe".to_owned(),
                prefix_relative_executable: PathBuf::from("drive_c/probe.exe"),
                query,
                kind: ProgramKind::Application,
                arguments: Vec::new(),
            },
        )
        .expect("the program set up");
    let entry = store
        .registered_programs()
        .expect("the programs")
        .remove(0)
        .entry_id;
    (catalogue, entry)
}

/// Start this test binary as a launcher of the manager reading `dir`.
fn start_manager(dir: &Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "windows_compat::manager_process", "--nocapture"])
        .env(MANAGER_DIR, dir)
        .spawn_locked()
        .expect("the manager's launcher starts")
}

#[test]
fn the_windows_apps_manager_starts_a_program_on_the_session_display() {
    let home = tilo_test_env::TestHome::new().expect("a home");
    let (catalogue, entry) = set_up_probe(home.data());
    let planned = Arc::new(Mutex::new(None));
    let seen = Arc::new(AtomicBool::new(false));
    let assembly = Assembly::with({
        let planned = Arc::clone(&planned);
        let seen = Arc::clone(&seen);
        move |side| {
            compat_service::serve(
                side,
                catalogue,
                SystemRuntimeRegistry::assuming_provider(),
                NoResources,
                WindowOnPlannedDisplay { planned, seen },
                LazyVmDelegate::new(|| Err("no appliance is set up here".to_owned())),
            )
        }
    });
    let apps = Rc::new(Apps::with_launcher(
        tilo_app_launch::SessionEnvironment::from_process(),
        None,
        Some(std::env::current_exe().expect("this test binary")),
    ));
    let launches = Arc::new(Launches::default());
    let mut session = crate::common::headless(Options {
        collab: false,
        launch: Some(Rc::clone(&apps)),
        launches: Arc::clone(&launches),
        app_socket: AppSocket::At(home.runtime().join("app_wire.sock")),
        windows: assembly,
        ..Options::default()
    });
    // The service started with the session; the display is bound after it, as `main` binds it.
    let display = home
        .runtime()
        .join(session.compositor().listen().expect("the display binds"));
    apps.set_displays(display.clone(), None, None);
    apps.set_render_node(Some(PathBuf::from("/dev/dri/renderD128")));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace in front");

    // The service starts on a thread of its own.
    let socket = launcher::services::socket_for(launcher::services::COMPAT_SERVICE);
    crate::common::wait_for(&mut session, "the compatibility service", |_| {
        socket.exists().then_some(())
    });

    // A launcher the session started for the manager: it is answered, and its program shows.
    let dir = home.home().join("manager");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workspace"), &workspace).unwrap();
    fs::write(dir.join("entry"), &entry).unwrap();
    let mut manager = start_manager(&dir);
    launches.started(manager.id(), MANAGER, &workspace, true);
    let answers = crate::common::wait_for(&mut session, "the manager's answers", |session| {
        if crate::common::window::window_of(session.compositor(), PROGRAM_WINDOW).is_some() {
            seen.store(true, Ordering::Release);
        }
        fs::read_to_string(dir.join("answers")).ok()
    });
    manager.wait().expect("the launcher ends");
    let answers: Value = serde_json::from_str(&answers).expect("answers");
    assert_eq!(answers["listed"]["result"], "listed", "{answers}");
    assert_eq!(
        answers["listed"]["applications"][0]["id"], entry,
        "the manager lists the program: {answers}"
    );
    // The launch is answered while the program is open, which the next list says; its end, with
    // the program's own status, is reported once it came.
    assert_eq!(answers["launched"]["result"], "launched", "{answers}");
    assert_eq!(
        answers["running"]["applications"][0]["running"], true,
        "{answers}"
    );
    assert_eq!(answers["ended"]["ended"][0]["entry_id"], entry, "{answers}");
    assert_eq!(answers["ended"]["ended"][0]["exit_code"], 7, "{answers}");
    assert_eq!(
        answers["ended"]["applications"][0]["running"], false,
        "{answers}"
    );
    assert!(
        seen.load(Ordering::Acquire),
        "the program's window was shown on the session's display"
    );
    assert_eq!(planned.lock().unwrap().as_deref(), Some(display.as_path()));
    // The same launch connecting again is refused: each launch hands off once.
    assert!(answers["again"]["refused"].is_string(), "{answers}");

    // A launcher process the session did not start is refused.
    let stranger = home.home().join("stranger");
    fs::create_dir_all(&stranger).unwrap();
    fs::write(stranger.join("workspace"), &workspace).unwrap();
    let mut started = start_manager(&stranger);
    let answers = crate::common::wait_for(&mut session, "the stranger's answers", |_| {
        fs::read_to_string(stranger.join("answers")).ok()
    });
    started.wait().expect("the stranger ends");
    let answers: Value = serde_json::from_str(&answers).expect("answers");
    assert!(answers["listed"]["refused"].is_string(), "{answers}");
}
