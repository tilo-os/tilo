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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compat_catalogue::{ApplicationQuery, Catalogue, VmFallback};
use compat_service::{
    Assembly, CapsuleCommandPlan, CapsuleProgram, CapsuleStore, ExecutionError, LaunchLease,
    LaunchResourceBroker, LazyVmDelegate, PROTOCOL_VERSION, ProgramKind, ProtocolRequest,
    ResourceError, ResourceRequest, SandboxCommand, SandboxExecutor, SessionClient,
    SystemRuntimeRegistry,
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

/// Stands in for Bubblewrap and Wine: starts [`probe_program`] with `dir`, on the display the plan
/// binds into the sandbox (told it when `tell_display`; a keeper tells it its own), handing it a
/// descriptor at the number Bubblewrap reads its seccomp filter from.
struct Probe {
    dir: PathBuf,
    tell_display: bool,
    planned: Arc<Mutex<Option<PathBuf>>>,
}

/// What the probe finds at the descriptor it is handed.
const HANDED_DOWN: &[u8] = b"handed down";

impl SandboxExecutor for Probe {
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
        let display = plan
            .bwrap_argv
            .windows(3)
            .find(|bind| bind[0] == "--ro-bind" && bind[2].ends_with("/wayland-0"))
            .map(|bind| PathBuf::from(&bind[1]))
            .expect("the plan binds a display");
        *self.planned.lock().unwrap() = Some(display.clone());
        let mut args = vec![format!("{PROBE_DIR}={}", self.dir.display())];
        if self.tell_display {
            args.push(format!("WAYLAND_DISPLAY={}", display.display()));
        }
        args.extend([
            std::env::current_exe()
                .expect("this test binary")
                .to_string_lossy()
                .into_owned(),
            "--exact".to_owned(),
            "windows_compat::probe_program".to_owned(),
            "--nocapture".to_owned(),
        ]);
        let memfd = rustix::fs::memfd_create("probe", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|error| ExecutionError::Io(error.into()))?;
        rustix::io::write(&memfd, HANDED_DOWN).map_err(|error| ExecutionError::Io(error.into()))?;
        let above = rustix::io::fcntl_dupfd_cloexec(&memfd, 64)
            .map_err(|error| ExecutionError::Io(error.into()))?;
        let mut command = SandboxCommand::new("/usr/bin/env", args);
        command
            .inherited
            .push((above, compat_service::SECCOMP_CHILD_FD));
        Ok(command)
    }
}

/// What makes this test binary the stand-in for a Windows program: the directory it reports in.
const PROBE_DIR: &str = "TILO_TEST_WINDOWS_PROBE";

/// Not a test: a Windows program, as the stand-in for Bubblewrap starts it. It writes what it
/// finds at the descriptor Bubblewrap reads its filter from to `handed`, its pid to `started`, and
/// shows a window until a code appears in `exit`, then exits with it.
#[test]
#[expect(
    clippy::exit,
    reason = "a Windows program ends with its own status, which the test reads"
)]
fn probe_program() {
    let Ok(dir) = std::env::var(PROBE_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let handed = fs::read(format!(
        "/proc/self/fd/{}",
        compat_service::SECCOMP_CHILD_FD
    ))
    .unwrap_or_default();
    fs::write(dir.join("handed"), handed).expect("what it was handed");
    let mut started = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("started"))
        .expect("the starts");
    std::io::Write::write_all(&mut started, format!("{}\n", std::process::id()).as_bytes())
        .expect("the start recorded");
    if std::env::var_os("DISPLAY").is_some() {
        x11_probe(&dir);
    }
    let socket = std::env::var("WAYLAND_DISPLAY").expect("a display");
    let mut window = crate::common::window::Client::on(Path::new(&socket), PROGRAM_WINDOW, "Probe");
    loop {
        window.exchange();
        if let Ok(code) = fs::read_to_string(dir.join("exit")) {
            let _ = fs::remove_file(dir.join("exit"));
            std::process::exit(code.trim().parse().expect("an exit code"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The probe as a program that shows through X11, as Proton's Wine does: it writes the display
/// its keeper served it to `display`, maps a window of [`PROGRAM_WINDOW`]'s class on it, and holds
/// that one connection until a code appears in `exit`, then exits with it. A connection lost (the
/// X server went away with a desktop) ends it with 99.
#[expect(
    clippy::exit,
    reason = "a Windows program ends with its own status, which the test reads"
)]
fn x11_probe(dir: &Path) -> ! {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;

    let display = std::env::var("DISPLAY").expect("an X11 display");
    fs::write(dir.join("display"), &display).expect("the display written");
    let (x11, screen) = x11rb::rust_connection::RustConnection::connect(None)
        .expect("the program reaches its X11 display with the cookie it was handed");
    let root = x11.setup().roots[screen].root;
    let window = x11.generate_id().expect("an id");
    x11.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        200,
        150,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().background_pixel(0x0000_00ff),
    )
    .expect("a window");
    x11.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        format!("probe\0{PROGRAM_WINDOW}\0").as_bytes(),
    )
    .expect("its class");
    x11.map_window(window).expect("mapped");
    x11.flush().expect("sent");
    loop {
        if x11
            .get_input_focus()
            .ok()
            .and_then(|asked| asked.reply().ok())
            .is_none()
        {
            std::process::exit(99);
        }
        if let Ok(code) = fs::read_to_string(dir.join("exit")) {
            let _ = fs::remove_file(dir.join("exit"));
            std::process::exit(code.trim().parse().expect("an exit code"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The catalogue routing `probe.exe` to Wine, and a capsule holding it, set up under the login's
/// data directory; the manager's entry for it.
fn set_up_probe(data: &Path) -> (Catalogue, String) {
    set_up_probe_on(
        data,
        r#"{"layer": "wine", "runner": {"family": "wine", "version": "system"}}"#,
    )
}

/// The catalogue routing `probe.exe` to the runtime `runtime` names (its layer and runner, and
/// any Proton recipe), and a capsule holding it; the manager's entry for it.
fn set_up_probe_on(data: &Path, runtime: &str) -> (Catalogue, String) {
    let mut recipe: Value = serde_json::from_str(runtime).expect("a runtime");
    recipe["settings"] = json!({});
    recipe["security_profile"] = json!("windows-app");
    let catalogue = Catalogue::from_json(
        &json!({
          "schema_version": 1,
          "records": [{
            "id": "probe",
            "version": 1,
            "provenance": "built_in",
            "signed": true,
            "matches": [{"executable": "probe.exe"}],
            "recipe": recipe
          }]
        })
        .to_string(),
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
    let probe = home.home().join("probe");
    fs::create_dir_all(&probe).unwrap();
    let assembly = Assembly::with({
        let planned = Arc::clone(&planned);
        let dir = probe.clone();
        move |side| {
            compat_service::serve(
                side,
                catalogue,
                SystemRuntimeRegistry::assuming_provider(),
                NoResources,
                Probe {
                    dir,
                    tell_display: true,
                    planned,
                },
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
    let mut seen = false;
    let answers = crate::common::wait_for(&mut session, "the manager's answers", |session| {
        if crate::common::window::window_of(session.compositor(), PROGRAM_WINDOW).is_some() && !seen
        {
            seen = true;
            fs::write(probe.join("exit"), "7").unwrap();
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
        seen,
        "the program's window was shown on the session's display"
    );
    assert_eq!(
        fs::read(probe.join("handed")).unwrap(),
        HANDED_DOWN,
        "the program inherits what its sandbox is handed"
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

/// What makes this test binary a manager that follows the steps in its directory.
const STEPS_DIR: &str = "TILO_TEST_WINDOWS_STEPS";

/// Not a test: a launcher of the Windows Apps manager that asks the service what `steps` in its
/// directory says, one step a line (`list`, `launch`, `stop`, or `ended`: list until an end is
/// told), and writes the answers, in order, to `answers`.
#[test]
fn manager_steps() {
    let Ok(dir) = std::env::var(STEPS_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let workspace = fs::read_to_string(dir.join("workspace")).expect("the workspace");
    let entry_id = fs::read_to_string(dir.join("entry")).expect("the entry");
    let mut client = connect(&workspace);
    let list = ProtocolRequest::List {
        version: PROTOCOL_VERSION,
    };
    let mut answers = Vec::new();
    for step in fs::read_to_string(dir.join("steps"))
        .expect("the steps")
        .lines()
    {
        answers.push(match step {
            "list" => ask(&mut client, &list),
            "launch" => ask(
                &mut client,
                &ProtocolRequest::Launch {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                },
            ),
            "stop" => ask(
                &mut client,
                &ProtocolRequest::Stop {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                },
            ),
            "ended" => {
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                loop {
                    let answer = ask(&mut client, &list);
                    if std::time::Instant::now() >= deadline
                        || answer["ended"]
                            .as_array()
                            .is_none_or(|ended| !ended.is_empty())
                    {
                        break answer;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            other => panic!("no step {other}"),
        });
    }
    fs::write(dir.join("answers"), Value::from(answers).to_string()).expect("the answers");
}

/// A session whose Windows programs are kept behind keepers, through restarts of the desktop.
struct KeptRig {
    home: tilo_test_env::TestHome,
    state: PathBuf,
    catalogue: Catalogue,
    entry: String,
    probe: PathBuf,
    keeper: PathBuf,
    root: PathBuf,
}

/// One desktop, until it ends.
struct KeptDesktop {
    session: tilo_session::Session,
    apps: Rc<Apps>,
    launches: Arc<Launches>,
    ending: Rc<std::cell::Cell<tilo_app_launch::Teardown>>,
    workspace: String,
}

impl KeptDesktop {
    fn end(self, teardown: tilo_app_launch::Teardown) {
        self.ending.set(teardown);
        let Self { session, apps, .. } = self;
        drop(session);
        drop(apps);
    }

    /// Ask the service `steps` as the manager would, and its answers.
    fn manage(&mut self, rig: &KeptRig, name: &str, steps: &[&str]) -> Vec<Value> {
        let dir = rig.home.home().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace"), &self.workspace).unwrap();
        fs::write(dir.join("entry"), &rig.entry).unwrap();
        fs::write(dir.join("steps"), steps.join("\n")).unwrap();
        let mut manager = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows_compat::manager_steps", "--nocapture"])
            .env(STEPS_DIR, &dir)
            .spawn_locked()
            .expect("the manager's launcher starts");
        self.launches
            .started(manager.id(), MANAGER, &self.workspace, true);
        let answers = crate::common::wait_for(&mut self.session, "the manager's answers", |_| {
            fs::read_to_string(dir.join("answers")).ok()
        });
        manager.wait().expect("the launcher ends");
        fs::remove_dir_all(&dir).unwrap();
        serde_json::from_str::<Vec<Value>>(&answers).expect("answers")
    }

    /// Wait until the program's window is on the desktop.
    fn shown(&mut self) {
        crate::common::wait_for(&mut self.session, "the program's window", |session| {
            crate::common::window::window_of(session.compositor(), PROGRAM_WINDOW)
        });
    }
}

impl KeptRig {
    fn new() -> Self {
        Self::on(r#"{"layer": "wine", "runner": {"family": "wine", "version": "system"}}"#)
    }

    /// A rig whose program is routed to the runtime `runtime` names ([`set_up_probe_on`]).
    fn on(runtime: &str) -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let (catalogue, entry) = set_up_probe_on(home.data(), runtime);
        let state = crate::common::device_in(&mut home);
        let probe = home.home().join("probe");
        fs::create_dir_all(&probe).unwrap();
        let keeper = home.home().join("keeper");
        tilo_app_launch::kept::testing::keeper_program(&keeper, "kept_apps::keeper_process")
            .unwrap();
        // The launcher's `windows-program`: the keeper, started with the command it was handed.
        tilo_app_launch::kept::testing::launcher_program(&home.home().join("tilo-launcher"), &[])
            .unwrap();
        let root = home.runtime().join("kept");
        Self {
            home,
            state,
            catalogue,
            entry,
            probe,
            keeper,
            root,
        }
    }

    /// A desktop, taking up what the keepers hold `following` an in-place restart.
    fn start(&self, following: bool) -> KeptDesktop {
        let catalogue = self.catalogue.clone();
        let dir = self.probe.clone();
        let assembly = Assembly::with(move |mut side| {
            // The manager is this test binary, not the stand-in launcher the programs start
            // through.
            side.launcher = std::env::current_exe().expect("this test binary");
            compat_service::serve(
                side,
                catalogue,
                SystemRuntimeRegistry::assuming_provider(),
                NoResources,
                Probe {
                    dir,
                    tell_display: false,
                    planned: Arc::default(),
                },
                LazyVmDelegate::new(|| Err("no appliance is set up here".to_owned())),
            )
        });
        let apps = Rc::new(Apps::with_launcher(
            tilo_app_launch::SessionEnvironment::from_process(),
            None,
            Some(self.home.home().join("tilo-launcher")),
        ));
        let launches = Arc::new(Launches::default());
        let ending = Rc::new(std::cell::Cell::new(tilo_app_launch::Teardown::Logout));
        let asked = Rc::clone(&ending);
        let mut session = crate::common::headless(Options {
            collab: false,
            launch: Some(Rc::clone(&apps)),
            launches: Arc::clone(&launches),
            state: Some(self.state.clone()),
            app_socket: AppSocket::At(self.home.runtime().join("app_wire.sock")),
            windows: assembly,
            teardown: Box::new(move || asked.get()),
            ..Options::default()
        });
        let display = self
            .home
            .runtime()
            .join(session.compositor().listen().expect("the display binds"));
        apps.set_displays(display, None, None);
        apps.set_render_node(Some(PathBuf::from("/dev/dri/renderD128")));
        let connector = session.compositor().client_connector();
        apps.keep_behind(tilo_app_launch::kept::Keeping {
            keeper: self.keeper.clone(),
            root: self.root.clone(),
            desktop: Arc::new(move |pid| connector.connect_for(pid)),
        });
        apps.take_up_kept(following);
        let workspace = session
            .handle()
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
            .expect("a workspace in front");
        let socket = launcher::services::socket_for(launcher::services::COMPAT_SERVICE);
        crate::common::wait_for(&mut session, "the compatibility service", |_| {
            socket.exists().then_some(())
        });
        KeptDesktop {
            session,
            apps,
            launches,
            ending,
            workspace,
        }
    }

    /// The pids of the program's starts.
    fn starts(&self) -> Vec<u32> {
        fs::read_to_string(self.probe.join("started"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.parse().ok())
            .collect()
    }

    /// End the program with `code`.
    fn exit(&self, code: i32) {
        fs::write(self.probe.join("exit"), code.to_string()).unwrap();
    }
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        && !fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
}

#[test]
fn a_windows_program_stays_open_through_an_in_place_restart() {
    let rig = KeptRig::new();
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch", "list"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    assert_eq!(
        answers[1]["applications"][0]["running"], true,
        "{answers:?}"
    );
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };
    assert_eq!(
        fs::read(rig.probe.join("handed")).unwrap(),
        HANDED_DOWN,
        "the program inherits what its sandbox is handed, through its keeper"
    );

    // The desktop restarts in place: the program goes on, and the next desktop shows it and its
    // service lists it open; Quit ends it.
    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    assert!(
        alive(pid),
        "the program outlives the desktop that started it"
    );
    let mut second = rig.start(true);
    second.shown();
    assert_eq!(rig.starts(), [pid], "the same program, not a new one");
    let answers = second.manage(&rig, "second", &["list", "stop", "ended"]);
    assert_eq!(
        answers[0]["applications"][0]["running"], true,
        "{answers:?}"
    );
    assert_eq!(answers[1]["result"], "stopped", "{answers:?}");
    assert_eq!(answers[2]["ended"][0]["entry_id"], rig.entry, "{answers:?}");
    assert_eq!(
        answers[2]["applications"][0]["running"], false,
        "{answers:?}"
    );
    crate::common::wait_for(&mut second.session, "the program ends", |_| {
        (!alive(pid)).then_some(())
    });

    // A program that ends while no desktop is there is reported to the next desktop with its code.
    let answers = second.manage(&rig, "third", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    second.shown();
    second.end(tilo_app_launch::Teardown::InPlaceRestart);
    rig.exit(5);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let ended = || {
        fs::read_dir(&rig.root)
            .into_iter()
            .flatten()
            .flatten()
            .any(|dir| dir.path().join("ended").exists())
    };
    while !ended() {
        assert!(
            std::time::Instant::now() < deadline,
            "the program did not end: {:?} {:?}",
            rig.starts(),
            fs::read_dir(&rig.root).map(|dirs| dirs
                .flatten()
                .map(|dir| fs::read_dir(dir.path()).map(|files| files
                    .flatten()
                    .map(|file| file.file_name())
                    .collect::<Vec<_>>()))
                .collect::<Vec<_>>())
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut third = rig.start(true);
    let answers = third.manage(&rig, "fourth", &["ended"]);
    assert_eq!(answers[0]["ended"][0]["entry_id"], rig.entry, "{answers:?}");
    assert_eq!(answers[0]["ended"][0]["exit_code"], 5, "{answers:?}");

    // A logout ends a program and its keeper.
    let answers = third.manage(&rig, "fifth", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    third.shown();
    let pid = *rig.starts().last().unwrap();
    third.end(tilo_app_launch::Teardown::Logout);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while alive(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "a logout leaves the program running"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        tilo_app_launch::kept::testing::described(&rig.root).is_empty(),
        "no keeper is left"
    );
}

/// Whether the X server a keeper serves an X11 display with is installed here.
fn x_servers() -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for program in ["xwayland-satellite", "Xwayland"] {
        if !std::env::split_paths(&path).any(|dir| dir.join(program).is_file()) {
            return Err(format!("{program} is not on PATH"));
        }
    }
    Ok(())
}

/// A Proton program shows through X11 (the pinned Proton has no Wayland driver): its launch asks
/// its keeper for an X11 display, the program's window on it reaches the desktop, and program, X
/// server and connection stay through an in-place restart.
#[test]
fn a_proton_program_shows_through_its_keepers_x11_display_and_stays_open() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X server");
    let rig = KeptRig::on(
        r#"{"layer": "proton", "runner": {"family": "umu-proton", "version": "UMU-Proton-10.0-4"},
            "proton_recipe": "umu-probe"}"#,
    );
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    assert!(
        tilo_app_launch::kept::testing::launcher_handed(&rig.home.home().join("tilo-launcher"))
            .contains(&"--x11".to_owned()),
        "the launch asks its keeper for an X11 display"
    );
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };
    assert!(
        fs::read_to_string(rig.probe.join("display")).is_ok_and(|display| display.starts_with(':')),
        "the program was served an X11 display"
    );

    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    let mut second = rig.start(true);
    second.shown();
    assert_eq!(rig.starts(), [pid], "the same program, not a new one");
    assert!(alive(pid), "on the same X11 connection");
    let answers = second.manage(&rig, "second", &["list", "stop", "ended"]);
    assert_eq!(
        answers[0]["applications"][0]["running"], true,
        "{answers:?}"
    );
    assert_eq!(answers[2]["ended"][0]["entry_id"], rig.entry, "{answers:?}");
    second.end(tilo_app_launch::Teardown::Logout);
}
