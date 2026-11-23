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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compat_catalogue::{ApplicationQuery, Catalogue, VmFallback};
use compat_service::{
    Assembly, CapsuleCommandPlan, CapsuleProgram, CapsuleStore, ExecutionError, LaunchLease,
    LaunchResourceBroker, LazyVmDelegate, NetworkShape, PROTOCOL_VERSION, ProgramKind,
    ProtocolRequest, ResourceError, ResourceRequest, SandboxCommand, SandboxExecutor,
    SessionClient, SystemRuntimeRegistry, VmAvailability, VmDelegate, VmInventory, VmPresentation,
    VmPublishedApplication, VmRouteRequest,
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

/// Write the manager's `answers` where the test waits for them: the file appears whole, so a
/// wait that sees it never reads a file the write has not filled yet.
fn write_answers(dir: &Path, answers: &Value) {
    let path = dir.join("answers");
    let staging = dir.join("answers.writing");
    fs::write(&staging, answers.to_string()).expect("the answers written");
    fs::rename(&staging, &path).expect("the answers put in place");
}

/// The answer to `request` on `client`, as JSON, or the error that ended the connection.
fn ask(client: &mut SessionClient, request: &ProtocolRequest) -> Value {
    match client.request(request) {
        Ok(response) => serde_json::to_value(response).expect("a response serializes"),
        Err(error) => json!({ "refused": error.to_string() }),
    }
}

/// The answer to `request` with the open `file` it hands over, as JSON, or the error that ended
/// the connection.
fn ask_with(client: &mut SessionClient, request: &ProtocolRequest, file: &fs::File) -> Value {
    match client.request_with_file(request, file) {
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
    write_answers(
        &dir,
        &json!({
            "listed": listed,
            "launched": launched,
            "running": running,
            "ended": ended,
            "again": again
        }),
    );
}

/// Grants nothing beyond what the plan carries.
struct NoResources;

impl LaunchResourceBroker for NoResources {
    fn materialize(&self, _: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
        Ok(LaunchLease::deny_all("lease"))
    }

    fn revoke(&self, _: &str) {}
}

/// Prepares a private network proxy for a filtered launch, as the computer's broker does: the
/// proxy itself is the launch's, started from this policy by the launcher.
struct NetResources {
    root: PathBuf,
}

/// Lease names no two launches in this test process share: a kept program's lease outlives the
/// desktop that made it, and a lease is never reused.
static NETWORK_LEASE: AtomicU64 = AtomicU64::new(1);

impl LaunchResourceBroker for NetResources {
    fn materialize(&self, request: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
        if NetworkShape::from_capabilities(request.capabilities) != NetworkShape::Filtered {
            return Ok(LaunchLease::deny_all("lease"));
        }
        let id = format!("net-{}", NETWORK_LEASE.fetch_add(1, Ordering::Relaxed));
        let dir = self.root.join(&id).join("netproxy");
        launcher::netproxy::prepare(
            &dir,
            &launcher::egress_proxy::EgressPolicy::from_grants(
                request.capabilities.net_hosts.iter().map(String::as_str),
            ),
        )
        .map_err(|error| ResourceError::Refused(error.to_string()))?;
        Ok(LaunchLease {
            id,
            audio: Vec::new(),
            audio_graph: None,
            egress_proxy: Some(launcher::sandbox::EgressProxyBind {
                host_socket: dir.join("proxy.sock"),
                sandbox_socket: PathBuf::from("/run/tilo/network/proxy.sock"),
                port: launcher::egress_proxy::RELAY_PORT,
            }),
        })
    }

    fn revoke(&self, _: &str) {}
}

/// Stands in for Bubblewrap and Wine: starts [`probe_program`] with `dir`, on the display the plan
/// binds into the sandbox (told it when `tell_display`; a keeper tells it its own), handing it a
/// descriptor at the number Bubblewrap reads its seccomp filter from.
struct Probe {
    dir: PathBuf,
    tell_display: bool,
    /// The display the last plan bound, and the plan's whole command line.
    planned: Arc<Mutex<Option<(PathBuf, Vec<String>)>>>,
    /// Where each launch's private audio graph is prepared, when the program has sound: the
    /// sandbox then starts as the compatibility service starts one with sound, after its graph
    /// ([`audio_step`]).
    sound: Option<PathBuf>,
    /// The real launcher: a launch whose network is filtered starts through its
    /// `windows-network`, as the computer's own launches do.
    launcher: PathBuf,
    /// The capsule store a setup installs into: a setup's sandbox writes what [`install_example`]
    /// writes, then shows and ends as any program does.
    setup: Option<PathBuf>,
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
        *self.planned.lock().unwrap() = Some((display.clone(), plan.bwrap_argv.clone()));
        if let Some(store) = &self.setup
            && plan
                .bwrap_argv
                .iter()
                .any(|arg| arg.contains("tilo-installers"))
        {
            install_example(store, &plan.capsule_id)?;
        }
        let mut args = vec![format!("{PROBE_DIR}={}", self.dir.display())];
        if self.tell_display {
            args.push(format!("WAYLAND_DISPLAY={}", display.display()));
        }
        let mut step = "windows_compat::probe_program";
        if let Some(sound) = &self.sound {
            let graph = sound.join(fs::read_dir(sound).map_or(0, Iterator::count).to_string());
            fs::create_dir_all(graph.join("audio/pulse"))?;
            args.push(format!("{AUDIO_GRAPH}={}", graph.display()));
            step = "windows_compat::audio_step";
        }
        if let Some(proxy) = &plan.network_proxy {
            args.push(format!(
                "{PROXY_SOCKET}={}",
                proxy.dir.join("proxy.sock").display()
            ));
        }
        args.extend([
            std::env::current_exe()
                .expect("this test binary")
                .to_string_lossy()
                .into_owned(),
            "--exact".to_owned(),
            step.to_owned(),
            "--nocapture".to_owned(),
        ]);
        // The test harness prints its own report on the stream a launch's output is read from.
        // The program gets that stream at descriptor 3 and the harness's on nothing, so what a
        // program writes is all the launch says, as a real program's is.
        let exe = args
            .iter()
            .position(|arg| !arg.contains('='))
            .unwrap_or(args.len());
        args.splice(
            exe..exe,
            [
                "sh".to_owned(),
                "-c".to_owned(),
                "exec \"$@\" 3>&1 1>/dev/null".to_owned(),
                "sh".to_owned(),
            ],
        );
        let memfd = rustix::fs::memfd_create("probe", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|error| ExecutionError::Io(error.into()))?;
        rustix::io::write(&memfd, HANDED_DOWN).map_err(|error| ExecutionError::Io(error.into()))?;
        let above = rustix::io::fcntl_dupfd_cloexec(&memfd, 64)
            .map_err(|error| ExecutionError::Io(error.into()))?;
        let program = PathBuf::from("/usr/bin/env");
        if let Some(proxy) = &plan.network_proxy {
            let wrapped: Vec<std::ffi::OsString> = [
                "windows-network".into(),
                "--dir".into(),
                proxy.dir.clone().into(),
                "--control".into(),
                proxy.control.clone().into(),
                "--".into(),
                program.into_os_string(),
            ]
            .into_iter()
            .chain(args.into_iter().map(Into::into))
            .collect();
            let mut command = SandboxCommand::new(self.launcher.clone(), wrapped);
            command
                .inherited
                .push((above, compat_service::SECCOMP_CHILD_FD));
            return Ok(command);
        }
        let mut command = SandboxCommand::new(program, args);
        command
            .inherited
            .push((above, compat_service::SECCOMP_CHILD_FD));
        Ok(command)
    }
}

/// What makes this test binary the stand-in for a Windows program: the directory it reports in.
const PROBE_DIR: &str = "TILO_TEST_WINDOWS_PROBE";
/// The directory of the private audio graph a program with sound starts after ([`audio_step`]).
const AUDIO_GRAPH: &str = "TILO_TEST_WINDOWS_AUDIO_GRAPH";
/// The command [`audio_step`] becomes, one argument a line, in place of [`probe_program`].
const AUDIO_COMMAND: &str = "TILO_TEST_WINDOWS_AUDIO_COMMAND";
/// What makes this test binary the stand-in for the RAIL wrapper and `FreeRDP`: its arguments, one
/// a line ([`rail_program`]).
const RAIL_ARGS: &str = "TILO_TEST_WINDOWS_RAIL";
/// The RDP password the appliance hands a `RemoteApp` launch.
const PASSWORD: &[u8] = b"a-one-time-rdp-password";
/// The proxy socket a program whose network is filtered reaches the network through.
const PROXY_SOCKET: &str = "TILO_TEST_WINDOWS_PROXY";
/// The file the test asks the program to reach a name through its proxy in.
const REQUEST: &str = "request";
/// The file the program answers a [`REQUEST`] in.
const ANSWER: &str = "answer";

/// Not a test: the launcher's `windows-audio` as a sandbox with sound starts behind it, through
/// the same call, with [`audio_daemon`] for both daemons: it starts the graph, then becomes the
/// program.
#[test]
fn audio_step() {
    let Ok(graph) = std::env::var(AUDIO_GRAPH) else {
        return;
    };
    let exe = std::env::current_exe().expect("this test binary");
    let daemon = PathBuf::from(&graph).join("daemon");
    fs::write(
        &daemon,
        format!(
            "#!/bin/sh\nexec '{}' --exact windows_compat::audio_daemon --nocapture\n",
            exe.display()
        ),
    )
    .expect("the stand-in daemon");
    fs::set_permissions(
        &daemon,
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("the stand-in daemon is executable");
    let daemons = launcher::audio_graph::Daemons {
        pipewire: daemon.clone(),
        pulse: daemon,
    };
    // The command `windows-audio` was handed, when the launcher's stand-in passed one on.
    let program: Vec<std::ffi::OsString> = match std::env::var(AUDIO_COMMAND) {
        Ok(command) => command.lines().map(Into::into).collect(),
        Err(_) => vec![
            exe.into_os_string(),
            "--exact".into(),
            "windows_compat::probe_program".into(),
            "--nocapture".into(),
        ],
    };
    // The keeper started this process to die with it, but this test is on a thread of the test
    // harness's, which a new thread never inherits that from: the launcher's own step is on its
    // main thread, so the program it becomes keeps it.
    // SAFETY: prctl with integer arguments only.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) },
        0,
        "the program dies with its keeper"
    );
    let Err(error) = launcher::audio_graph::exec_with(graph.into(), &daemons, &program);
    panic!("the program did not start: {error}");
}

/// Not a test: `PipeWire` or its Pulse server, as a graph starts them: it listens where the real
/// one would and adds its pid to the graph's `audio/daemons`, until it is ended.
#[test]
fn audio_daemon() {
    let Some(runtime) = std::env::var_os("PIPEWIRE_RUNTIME_DIR").map(PathBuf::from) else {
        return;
    };
    let socket = if std::env::var_os("PIPEWIRE_DAEMON").is_some() {
        runtime.join("pipewire-0")
    } else {
        PathBuf::from(std::env::var_os("PULSE_RUNTIME_PATH").expect("the Pulse directory"))
            .join("native")
    };
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the daemon's socket");
    let mut daemons = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime.join("daemons"))
        .expect("the daemons");
    std::io::Write::write_all(&mut daemons, format!("{}\n", std::process::id()).as_bytes())
        .expect("the daemon written down");
    for client in listener.incoming() {
        drop(client);
    }
}

/// Not a test: a Windows program, as the stand-in for Bubblewrap starts it. It writes what it
/// finds at the descriptor Bubblewrap reads its filter from to `handed`, its pid to `started`, and
/// shows a window until a code appears in `exit`, then says what `say` holds on its error stream
/// and exits with it.
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
    // The stream a launch's output is read from was given to this program at descriptor 3, the
    // harness's own report on nothing: from here on this program's output is the launch's.
    // SAFETY: `dup2` and `close` take plain descriptors, and descriptor 3 is the harness script's.
    unsafe {
        if libc::dup2(3, libc::STDOUT_FILENO) != -1 {
            libc::close(3);
        }
    }
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
    let proxy = std::env::var_os(PROXY_SOCKET).map(PathBuf::from);
    let socket = std::env::var("WAYLAND_DISPLAY").expect("a display");
    let mut window = crate::common::window::Client::on(Path::new(&socket), PROGRAM_WINDOW, "Probe");
    loop {
        if let Some(proxy) = &proxy
            && let Ok(request) = fs::read_to_string(dir.join(REQUEST))
        {
            let _ = fs::remove_file(dir.join(REQUEST));
            let answer = proxy_answer(proxy, &dir, request.trim());
            fs::write(dir.join(ANSWER), answer).expect("the answer");
        }
        window.exchange();
        if let Ok(code) = fs::read_to_string(dir.join("exit")) {
            let _ = fs::remove_file(dir.join("exit"));
            if let Ok(said) = fs::read_to_string(dir.join("say")) {
                eprint!("{said}");
            }
            if let Ok(said) = fs::read_to_string(dir.join("say-out")) {
                print!("{said}");
            }
            std::process::exit(code.trim().parse().expect("an exit code"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Not a test: the RAIL wrapper and `FreeRDP` as one X11 client, started behind its keeper as the
/// appliance's presentation starts the real wrapper. It reads the password at the descriptor its
/// private argument names (writing only its digest, to `password`), then shows like the X11 probe.
#[test]
fn rail_program() {
    use std::os::fd::FromRawFd as _;
    let (Ok(dir), Ok(args)) = (std::env::var(PROBE_DIR), std::env::var(RAIL_ARGS)) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let fd: i32 = args
        .lines()
        .find_map(|arg| arg.strip_prefix("--tilo-password-fd="))
        .expect("the password's descriptor is named")
        .parse()
        .expect("a descriptor number");
    // SAFETY: the descriptor was handed to this process for this read alone.
    let mut password = unsafe { fs::File::from_raw_fd(fd) };
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut password, &mut read).expect("the password reads");
    drop(password);
    fs::write(
        dir.join("password"),
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&read)),
    )
    .expect("its digest written");
    let mut started = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("started"))
        .expect("the starts");
    std::io::Write::write_all(&mut started, format!("{}\n", std::process::id()).as_bytes())
        .expect("the start recorded");
    x11_probe(&dir);
}

/// The probe asking its proxy for `name`, as an app with a filtered network does: `allowed` or
/// `refused`, or for `hold` the answer `cut` when the connection ends, which a policy taken back
/// does.
fn proxy_answer(proxy: &Path, dir: &Path, request: &str) -> String {
    let answer = proxy_answer_traced(proxy, dir, request);
    probe_trace(dir, &format!("{request} -> {answer}"));
    answer
}

/// What the probe says of itself while it answers: the test reads it when an answer is not the
/// one it waits for.
fn probe_trace(dir: &Path, text: &str) {
    use std::io::Write as _;
    if let Ok(mut log) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("proxy.log"))
    {
        let _ = writeln!(log, "{text}");
    }
}

fn proxy_answer_traced(proxy: &Path, dir: &Path, request: &str) -> String {
    use std::io::{Read as _, Write as _};
    let mut parts = request.split_whitespace();
    let hold = parts.next() == Some("hold");
    let name = parts.next().unwrap_or_default();
    let port: u16 = parts.next().and_then(|port| port.parse().ok()).unwrap_or(0);
    probe_trace(dir, &format!("{request}: connecting"));
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(proxy) else {
        return "no proxy".to_owned();
    };
    if stream.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return "no proxy".to_owned();
    }
    let mut ack = [0u8; 2];
    if stream.read_exact(&mut ack).is_err() {
        return "no proxy".to_owned();
    }
    let Ok(len) = u8::try_from(name.len()) else {
        return "refused".to_owned();
    };
    let mut ask = vec![0x05, 0x01, 0x00, 0x03, len];
    ask.extend_from_slice(name.as_bytes());
    ask.extend_from_slice(&port.to_be_bytes());
    if stream.write_all(&ask).is_err() {
        return "no proxy".to_owned();
    }
    let mut head = [0u8; 4];
    if stream.read_exact(&mut head).is_err() {
        return "no proxy".to_owned();
    }
    probe_trace(dir, &format!("{request}: reply {}", head[1]));
    if head[1] != launcher::egress_proxy::reply::SUCCESS {
        return "refused".to_owned();
    }
    let rest = if head[3] == 0x01 { 6 } else { 18 };
    let mut drain = vec![0u8; rest];
    if stream.read_exact(&mut drain).is_err() {
        return "no proxy".to_owned();
    }
    if !hold {
        return "allowed".to_owned();
    }
    fs::write(dir.join(ANSWER), "holding").expect("the answer");
    let mut byte = [0u8; 1];
    while stream.read(&mut byte).is_ok_and(|read| read > 0) {}
    "cut".to_owned()
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
fn set_up_probe(data: &Path) -> (Catalogue, String, String) {
    set_up_probe_on(
        data,
        r#"{"layer": "wine", "runner": {"family": "wine", "version": "system"}}"#,
    )
}

/// The catalogue routing `probe.exe` to the runtime `runtime` names (its layer and runner, and
/// any Proton recipe), and a capsule holding it; the manager's entry for it, and the app id a
/// grant names it by.
fn set_up_probe_on(data: &Path, runtime: &str) -> (Catalogue, String, String) {
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
                opens_with: None,
            },
        )
        .expect("the program set up");
    let program = store.registered_programs().expect("the programs").remove(0);
    (catalogue, program.entry_id, program.app_id)
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
    let (catalogue, entry, _app_id) = set_up_probe(home.data());
    let planned = Arc::new(Mutex::new(None));
    let probe = home.home().join("probe");
    fs::create_dir_all(&probe).unwrap();
    let assembly = Assembly::with({
        let planned = Arc::clone(&planned);
        let dir = probe.clone();
        let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
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
                    sound: None,
                    launcher,
                    setup: None,
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
    // A machine with no GPU: the compositor names its display's primary node, which is no render
    // node, and the program still starts, drawing in software.
    apps.set_render_node(Some(PathBuf::from("/dev/dri/card0")));
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
    let (bound, argv) = planned.lock().unwrap().clone().expect("a plan was made");
    assert_eq!(bound, display);
    assert!(
        !argv.iter().any(|arg| arg.starts_with("/dev/dri/card")),
        "a display's primary node is never given to a program: {argv:?}"
    );
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
/// directory says, one step a line (`list`, `launch`, `stop`, `ended`: list until an end is
/// told, or `install`: the directory's `setup.exe` into a private capsule, two answers), and
/// writes the answers, in order, to `answers`.
#[test]
fn manager_steps() {
    let Ok(dir) = std::env::var(STEPS_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let workspace = fs::read_to_string(dir.join("workspace")).expect("the workspace");
    let entry_id = fs::read_to_string(dir.join("entry")).unwrap_or_default();
    let mut client = connect(&workspace);
    let list = ProtocolRequest::List {
        version: PROTOCOL_VERSION,
    };
    let mut answers = Vec::new();
    for step in fs::read_to_string(dir.join("steps"))
        .expect("the steps")
        .lines()
    {
        let answer = match step {
            "list" => ask(&mut client, &list),
            "launch" => ask(
                &mut client,
                &ProtocolRequest::Launch {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                },
            ),
            "launch-vm" => ask(
                &mut client,
                &ProtocolRequest::LaunchVmFallback {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                },
            ),
            "install" => {
                // The installer the person chose, then the private capsule they approved for it,
                // as Windows Apps asks: the same open file both times.
                let installer = fs::File::open(dir.join("setup.exe")).expect("the installer");
                let asked = ask_with(
                    &mut client,
                    &ProtocolRequest::Install {
                        version: PROTOCOL_VERSION,
                        source_name: "setup.exe".to_owned(),
                    },
                    &installer,
                );
                answers.push(asked);
                ask_with(
                    &mut client,
                    &ProtocolRequest::InstallInCapsule {
                        version: PROTOCOL_VERSION,
                        source_name: "setup.exe".to_owned(),
                    },
                    &installer,
                )
            }
            // Open the program with Proton from now on, or as it is recommended to open.
            "proton" | "recommended" => ask(
                &mut client,
                &ProtocolRequest::SetOpensWith {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                    route: (step == "proton").then_some(compat_service::RouteKind::Proton),
                },
            ),
            "stop" => ask(
                &mut client,
                &ProtocolRequest::Stop {
                    version: PROTOCOL_VERSION,
                    entry_id: entry_id.clone(),
                },
            ),
            // The list once no setup is open any more.
            "settled" => {
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                loop {
                    let answer = ask(&mut client, &list);
                    if std::time::Instant::now() >= deadline
                        || answer["installations"].as_array().is_none_or(|all| {
                            all.iter().all(|one| {
                                one["status"] != "installing" && one["status"] != "uninstalling"
                            })
                        })
                    {
                        break answer;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
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
        };
        answers.push(answer);
    }
    write_answers(&dir, &Value::from(answers));
}

/// A session whose Windows programs are kept behind keepers, through restarts of the desktop.
struct KeptRig {
    home: tilo_test_env::TestHome,
    state: PathBuf,
    catalogue: Catalogue,
    entry: String,
    /// The app id a workspace grant names the program by.
    app_id: String,
    probe: PathBuf,
    keeper: PathBuf,
    root: PathBuf,
    /// The real launcher, which starts a filtered program's network proxy.
    launcher: PathBuf,
    /// Where the launches' network leases live, as the service leases in this test process.
    network_leases: PathBuf,
    /// Where the programs' private audio graphs are, when they have sound.
    sound: Option<PathBuf>,
    /// The Windows appliance is set up, with [`PUBLISHED`] in its catalogue.
    appliance: bool,
    /// A setup in a capsule installs the program "Example" ([`install_example`]).
    setup: bool,
    /// The desktop's person administers the computer.
    administrator: bool,
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
        fs::write(dir.join("setup.exe"), setup_exe()).unwrap();
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
        let (catalogue, entry, app_id) = set_up_probe_on(home.data(), runtime);
        let state = crate::common::device_in(&mut home);
        let probe = home.home().join("probe");
        fs::create_dir_all(&probe).unwrap();
        let keeper = home.home().join("keeper");
        tilo_app_launch::kept::testing::keeper_program(&keeper, "kept_apps::keeper_process")
            .unwrap();
        // The launcher's `windows-program`: the keeper, started with the command it was handed.
        tilo_app_launch::kept::testing::launcher_program(&home.home().join("tilo-launcher"), &[])
            .unwrap();
        let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
        let network_leases = compat_service::test_lease_root_at(home.runtime().join("leases"));
        let root = home.runtime().join("kept");
        Self {
            home,
            state,
            catalogue,
            entry,
            app_id,
            probe,
            keeper,
            root,
            launcher,
            network_leases,
            sound: None,
            appliance: false,
            setup: false,
            administrator: false,
        }
    }

    /// A rig whose Windows appliance publishes [`PUBLISHED`], shown through the real RAIL
    /// presentation: the stand-ins are the wrapper with `FreeRDP` ([`rail_program`]) and the
    /// launcher's `windows-audio` ([`audio_step`], handed the presentation's command).
    fn with_appliance() -> Self {
        let mut rig = Self::new();
        let exe = std::env::current_exe().expect("this test binary");
        write_program(
            &rig.home.home().join("rail"),
            &format!(
                "{PROBE_DIR}='{}' {RAIL_ARGS}=\"$(printf '%s\\n' \"$@\")\" exec '{}' --exact windows_compat::rail_program --nocapture",
                rig.probe.display(),
                exe.display()
            ),
        );
        write_program(
            &rig.home.home().join("audio-launcher"),
            &format!(
                "[ \"$1 $2 $4\" = 'windows-audio --graph --' ] || exit 2\ng=$3; shift 4\n\
                 {AUDIO_GRAPH}=\"$g\" {AUDIO_COMMAND}=\"$(printf '%s\\n' \"$@\")\" exec '{}' --exact windows_compat::audio_step --nocapture",
                exe.display()
            ),
        );
        rig.entry = compat_service::vm_entry_id(PUBLISHED);
        rig.appliance = true;
        rig
    }

    /// The daemons of the newest appliance presentation's audio graph, once both are there: it is
    /// in the presentation's keeper's directory.
    fn appliance_daemons(&self, desktop: &mut KeptDesktop) -> [u32; 2] {
        crate::common::wait_for(
            &mut desktop.session,
            "the presentation's audio daemons",
            |_| {
                let pids: Vec<u32> = entries(&self.root)
                    .into_iter()
                    .flat_map(|kept| entries(&kept.join("tilo-windows-vm")))
                    .filter_map(|graph| fs::read_to_string(graph.join("audio/daemons")).ok())
                    .flat_map(|daemons| {
                        daemons
                            .lines()
                            .filter_map(|line| line.parse().ok())
                            .collect::<Vec<u32>>()
                    })
                    .collect();
                <[u32; 2]>::try_from(pids).ok()
            },
        )
    }

    /// A rig whose programs have sound: each starts after a private audio graph of its own.
    fn with_sound() -> Self {
        let mut rig = Self::new();
        let sound = rig.home.runtime().join("sound");
        fs::create_dir_all(&sound).unwrap();
        rig.sound = Some(sound);
        rig
    }

    /// The daemons of the newest program's audio graph, once both are there.
    fn daemons(&self, desktop: &mut KeptDesktop) -> [u32; 2] {
        let sound = self.sound.as_ref().expect("a rig with sound");
        let newest = fs::read_dir(sound)
            .map_or(0, Iterator::count)
            .saturating_sub(1);
        let daemons = sound.join(newest.to_string()).join("audio/daemons");
        crate::common::wait_for(&mut desktop.session, "the audio daemons", |_| {
            let pids: Vec<u32> = fs::read_to_string(&daemons)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.parse().ok())
                .collect();
            <[u32; 2]>::try_from(pids).ok()
        })
    }

    /// Wait, with no desktop there, until each of `pids` ended.
    fn gone<const N: usize>(pids: [u32; N]) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while pids.iter().any(|pid| alive(*pid)) {
            assert!(std::time::Instant::now() < deadline, "{pids:?} did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until each of `daemons` ended.
    fn ended(desktop: &mut KeptDesktop, daemons: [u32; 2], why: &str) {
        crate::common::wait_for(&mut desktop.session, why, |_| {
            daemons.iter().all(|pid| !alive(*pid)).then_some(())
        });
    }

    /// A desktop, taking up what the keepers hold `following` an in-place restart.
    fn start(&self, following: bool) -> KeptDesktop {
        self.start_at(following, &self.state)
    }

    /// A desktop on the device keeping its state at `state`, taking up what the keepers hold
    /// `following` an in-place restart.
    fn start_at(&self, following: bool, state: &Path) -> KeptDesktop {
        let catalogue = self.catalogue.clone();
        let dir = self.probe.clone();
        let sound = self.sound.clone();
        let appliance = self.appliance.then(|| {
            (
                self.home.home().join("rail"),
                self.home.home().join("audio-launcher"),
            )
        });
        let launcher_bin = self.launcher.clone();
        let setup = self
            .setup
            .then(|| self.home.data().join("tilo/windows-compat"));
        let network_leases = NetResources {
            root: self.network_leases.clone(),
        };
        let administrator = self.administrator;
        let assembly = Assembly::with(move |mut side| {
            // The manager is this test binary, not the stand-in launcher the programs start
            // through.
            side.launcher = std::env::current_exe().expect("this test binary");
            side.administrator = Arc::new(move || administrator);
            let displays = Arc::clone(&side.displays);
            let vm = LazyVmDelegate::new(move || match &appliance {
                Some((rail, launcher)) => Ok(Arc::new(Appliance {
                    rail: compat_service::AudioIsolatedRailLauncher::new(Arc::clone(&displays))
                        .with_stand_ins(rail.clone(), launcher.clone(), stand_in_graph),
                }) as Arc<dyn VmDelegate>),
                None => Err("no appliance is set up here".to_owned()),
            });
            compat_service::serve(
                side,
                catalogue,
                SystemRuntimeRegistry::assuming_provider(),
                network_leases,
                Probe {
                    dir,
                    tell_display: false,
                    planned: Arc::default(),
                    sound,
                    launcher: launcher_bin,
                    setup,
                },
                vm,
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
            launch: Some(Rc::clone(&apps)),
            launches: Arc::clone(&launches),
            state: Some(state.to_path_buf()),
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

    /// End the program with `code`, saying `said` on its error stream first.
    fn exit_saying(&self, code: i32, said: &str) {
        fs::write(self.probe.join("say"), said).unwrap();
        self.exit(code);
    }

    /// End the program with `code`, saying `said` on its normal output first: a console tool
    /// that prints its usage and stops.
    fn exit_saying_out(&self, code: i32, said: &str) {
        fs::write(self.probe.join("say-out"), said).unwrap();
        self.exit(code);
    }
}

/// What the real launcher would do with `handed`, the command line the session started a launch
/// with: the keeper it names, and the arguments it would hand that keeper, read from the
/// launcher's own `--print-keeper` rather than a stand-in's idea of its arguments.
/// `--print-keeper` goes before the `--` that separates the launcher's options from the
/// sandbox's command.
fn real_launcher_keeper(handed: &[String]) -> (String, Vec<String>) {
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let mut args = handed.to_vec();
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    args.insert(separator, "--print-keeper".to_owned());
    let planned = std::process::Command::new(&launcher)
        .args(&args)
        .output_locked()
        .expect("the launcher answers");
    let said = String::from_utf8_lossy(&planned.stdout);
    assert!(
        planned.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let mut lines = said.lines();
    let keeper = lines
        .next()
        .and_then(|line| line.strip_prefix("keeper : "))
        .unwrap_or_else(|| panic!("the launcher named no keeper: {said}"))
        .to_owned();
    let keeper_args = lines
        .map(|line| line.trim_start_matches('\t').to_owned())
        .collect();
    (keeper, keeper_args)
}

fn alive(pid: u32) -> bool {
    tilo_process::start_time(pid).is_some()
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
    // The real launcher takes the session's command line: Wine needs no X11, so neither the
    // session's launch nor the keeper it names asks for one.
    let handed =
        tilo_app_launch::kept::testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
    assert!(
        !handed.contains(&"--x11".to_owned()),
        "the launch asks its keeper for no X11 display: {handed:?}"
    );
    let (keeper, keeper_args) = real_launcher_keeper(&handed);
    assert_eq!(keeper, rig.keeper.display().to_string(), "{keeper_args:?}");
    assert!(
        !keeper_args.contains(&"--x11".to_owned()),
        "the keeper is never asked for a display it would never serve: {keeper_args:?}"
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

    // A program that ends while no desktop is there is reported to the next desktop with its code,
    // and the tab it left closes rather than starting it again.
    let answers = second.manage(&rig, "third", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    second.shown();
    let tab = saved_window_tab(&rig, &mut second);
    second.end(tilo_app_launch::Teardown::InPlaceRestart);
    rig.exit_saying(5, "err:module:import_dll Library MSVCP140.dll not found\n");
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
    assert_eq!(
        answers[0]["ended"][0]["said"], "err:module:import_dll Library MSVCP140.dll not found",
        "what it said while no desktop was there comes with its end: {answers:?}"
    );
    closed_and_not_started(&rig, &mut third, tab, 2);

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

/// A Windows program that fails says why: the last of what it wrote, which went to its keeper's
/// log rather than the service, comes with its end, bounded to what a sentence holds. One that
/// says nothing ends with its status alone.
#[test]
fn a_failed_windows_program_says_why_with_its_end() {
    let rig = KeptRig::new();
    let mut desktop = rig.start(false);
    let answers = desktop.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    desktop.shown();
    let long = "x".repeat(4096);
    rig.exit_saying(
        3,
        &format!("fixme:heap:stub\n{long}\n\nwine: could not load ntdll.so\n"),
    );
    let answers = desktop.manage(&rig, "second", &["ended"]);
    let end = &answers[0]["ended"][0];
    assert_eq!(end["exit_code"], 3, "{answers:?}");
    let said = end["said"].as_str().expect("what it said");
    assert!(
        said.ends_with("x wine: could not load ntdll.so"),
        "its last lines: {said}"
    );
    assert!(
        said.starts_with('…') && said.chars().count() <= 200,
        "{said}"
    );

    fs::remove_file(rig.probe.join("say")).unwrap();
    let answers = desktop.manage(&rig, "third", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    desktop.shown();
    rig.exit(4);
    // A manager opened later hears every end kept: this one is the last, once it came.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let end = loop {
        let answers = desktop.manage(&rig, "fourth", &["ended"]);
        let last = answers[0]["ended"]
            .as_array()
            .and_then(|ended| ended.last())
            .cloned()
            .unwrap_or_default();
        if last["exit_code"] == 4 {
            break last;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the second end did not come: {answers:?}"
        );
    };
    assert_eq!(end["said"], Value::Null, "{end:?}");

    // A console tool prints its usage on its normal output and stops: that is its reason when its
    // error stream holds nothing.
    let answers = desktop.manage(&rig, "fifth", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    desktop.shown();
    rig.exit_saying_out(1, "Usage: plink [options] destination\n");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let end = loop {
        let answers = desktop.manage(&rig, "sixth", &["ended"]);
        let last = answers[0]["ended"]
            .as_array()
            .and_then(|ended| ended.last())
            .cloned()
            .unwrap_or_default();
        if last["exit_code"] == 1 {
            break last;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the usage was never its reason: {answers:?}"
        );
    };
    assert_eq!(
        end["said"], "Usage: plink [options] destination",
        "the last of its normal output is its reason"
    );
    desktop.end(tilo_app_launch::Teardown::Logout);
}

/// The parent of process `pid`.
fn parent(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// A program's private audio graph lives exactly as long as the program: its daemons are the
/// sandbox's own children (so in its launch's group), stay through an in-place restart, and end
/// when the program is quit (one a later desktop took back), ends, or the person logs out.
#[test]
fn a_windows_programs_sound_lives_as_long_as_the_program() {
    let rig = KeptRig::with_sound();
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };
    let daemons = rig.daemons(&mut first);
    for daemon in daemons {
        assert_eq!(
            parent(daemon),
            Some(pid),
            "the audio daemons are the program's sandbox's children"
        );
    }
    assert_eq!(
        fs::read(rig.probe.join("handed")).unwrap(),
        HANDED_DOWN,
        "the sandbox still gets what it is handed"
    );

    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    assert!(
        daemons.iter().all(|daemon| alive(*daemon)),
        "the sound outlives the desktop that started the program"
    );
    let mut second = rig.start(true);
    second.shown();
    assert!(daemons.iter().all(|daemon| alive(*daemon)));
    let answers = second.manage(&rig, "second", &["stop"]);
    assert_eq!(answers[0]["result"], "stopped", "{answers:?}");
    KeptRig::ended(
        &mut second,
        daemons,
        "quitting a program taken back ends its sound",
    );

    let answers = second.manage(&rig, "third", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    second.shown();
    let daemons = rig.daemons(&mut second);
    rig.exit(0);
    KeptRig::ended(
        &mut second,
        daemons,
        "a program that ends takes its sound along",
    );

    let answers = second.manage(&rig, "fourth", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    second.shown();
    let daemons = rig.daemons(&mut second);
    let pid = *rig.starts().last().unwrap();
    second.end(tilo_app_launch::Teardown::Logout);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while daemons.iter().any(|daemon| alive(*daemon)) {
        assert!(
            std::time::Instant::now() < deadline,
            "a logout leaves the sound playing (the program alive: {})",
            alive(pid)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Grant the program a filtered network for `name`, through the session's shell, and let the
/// session publish it before the compatibility service is asked to start it.
fn grant_network(desktop: &mut KeptDesktop, rig: &KeptRig, name: &str) {
    use collab_types::caps::{Access, CapOverride, Scope};
    use tilo_action_policy::Grantee;
    let handle = desktop.session.handle();
    let mut tree: tilo_desktop_model::Layout = handle.read(Clone::clone);
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id().to_string() == desktop.workspace)
        .expect("the workspace in front");
    workspace.sandbox.push(
        CapOverride::grant(
            Grantee::person("tester"),
            Scope::NetHost(name.to_owned()),
            Access::Write,
        )
        .through(rig.app_id.clone()),
    );
    handle.replace_tree(tree);
    for _ in 0..50 {
        desktop
            .session
            .step(Duration::from_millis(1))
            .expect("step");
    }
}

/// Take the program's network grants back, as the person revoking them in the workspace's grants
/// does.
fn revoke_network(desktop: &KeptDesktop) {
    let handle = desktop.session.handle();
    let mut tree: tilo_desktop_model::Layout = handle.read(Clone::clone);
    for workspace in &mut tree.workspaces {
        workspace
            .sandbox
            .retain(|grant| !matches!(grant.scope, collab_types::caps::Scope::NetHost(_)));
    }
    handle.replace_tree(tree);
}

/// Ask the program at `probe` to `request` through its network proxy, and wait for its answer,
/// stepping the session as a desktop does.
fn answered(desktop: &mut KeptDesktop, probe: &Path, request: &str) -> String {
    let _ = fs::remove_file(probe.join(ANSWER));
    fs::write(probe.join(REQUEST), request).expect("the request");
    crate::common::wait_for(&mut desktop.session, "the program's answer", |_| {
        fs::read_to_string(probe.join(ANSWER))
            .ok()
            .map(|answer| answer.trim().to_owned())
    })
}

/// The control socket of the kept program's network proxy, under `root`.
fn network_control(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let control = entry.path().join(launcher::netproxy::CONTROL);
        control.exists().then_some(control)
    })
}

/// A kept Windows program keeps its filtered network through an in-place restart, because the
/// proxy lives in the launch: it allows what the workspace granted before, while no desktop is
/// there and after one comes back. A grant taken back after adoption cuts the connection it
/// allowed, and the proxy ends with the program at logout.
#[test]
fn a_kept_program_keeps_its_filtered_network_through_a_restart_and_a_revoked_grant_cuts_it() {
    let rig = KeptRig::new();
    let mut first = rig.start(false);
    grant_network(&mut first, &rig, "localhost");

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a listener");
    let port = listener.local_addr().expect("its port").port();
    std::thread::spawn(move || {
        // Answer each connection and keep it open: a tunnel a grant allows is not one the
        // destination ends.
        for mut client in listener.incoming().flatten() {
            let _ = std::io::Write::write_all(&mut client, b"HTTP/1.0 200 OK\r\n\r\nallowed");
            std::thread::spawn(move || {
                use std::io::Read as _;
                let mut byte = [0u8; 64];
                while client.read(&mut byte).is_ok_and(|read| read > 0) {}
            });
        }
    });

    let answers = first.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };
    let control =
        crate::common::wait_for(&mut first.session, "the program's network proxy", |_| {
            network_control(&rig.root)
        });
    let proxy = crate::common::wait_for(&mut first.session, "the proxy's process", |_| {
        let children = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
        children.split_whitespace().next()?.parse::<u32>().ok()
    });

    assert_eq!(
        answered(&mut first, &rig.probe, &format!("connect localhost {port}")),
        "allowed",
        "the grant reaches the program before the restart"
    );
    assert_eq!(
        answered(&mut first, &rig.probe, "connect blocked.invalid 80"),
        "refused",
        "a name the grant does not name is refused before the restart"
    );
    first.end(tilo_app_launch::Teardown::InPlaceRestart);

    // No desktop is there: the proxy keeps enforcing the policy it has.
    fs::write(rig.probe.join(REQUEST), format!("connect localhost {port}")).expect("the request");
    tilo_test_env::wait_until("the program's answer while no desktop is there", || {
        fs::read_to_string(rig.probe.join(ANSWER)).is_ok_and(|answer| answer.trim() == "allowed")
    });
    fs::write(rig.probe.join(REQUEST), "connect blocked.invalid 80").expect("the request");
    tilo_test_env::wait_until(
        "an ungranted name is refused while no desktop is there",
        || {
            fs::read_to_string(rig.probe.join(ANSWER))
                .is_ok_and(|answer| answer.trim() == "refused")
        },
    );

    let mut second = rig.start(true);
    // The workspace's grants are the session's to publish: this desktop publishes them again,
    // as the workspace's grants are published after a restart.
    grant_network(&mut second, &rig, "localhost");
    second.shown();
    assert!(alive(pid), "the program outlived the desktop");
    assert!(alive(proxy), "so did its network proxy");
    assert_eq!(
        answered(
            &mut second,
            &rig.probe,
            &format!("connect localhost {port}")
        ),
        "allowed",
        "the adopted program still reaches what it was granted"
    );
    assert_eq!(
        answered(&mut second, &rig.probe, "connect blocked.invalid 80"),
        "refused",
        "what it was never granted is still refused after the restart"
    );

    // A connection opened now, then the grant taken back: the connection ends, and new ones are
    // refused.
    fs::write(rig.probe.join(REQUEST), format!("hold localhost {port}")).expect("the request");
    let holding = crate::common::steps_within(&mut second.session, Duration::from_secs(15), |_| {
        fs::read_to_string(rig.probe.join(ANSWER))
            .ok()
            .filter(|answer| answer.trim() == "holding")
            .map(|_| ())
    });
    assert!(
        holding.is_some(),
        "the tunnel did not open: answer {:?}, trace {:?}",
        fs::read_to_string(rig.probe.join(ANSWER)).ok(),
        fs::read_to_string(rig.probe.join("proxy.log")).ok()
    );
    revoke_network(&second);
    crate::common::wait_for(
        &mut second.session,
        "the revoked grant cuts the tunnel",
        |_| {
            fs::read_to_string(rig.probe.join(ANSWER))
                .ok()
                .filter(|answer| answer.trim() == "cut")
                .map(|_| ())
        },
    );
    assert_eq!(
        answered(
            &mut second,
            &rig.probe,
            &format!("connect localhost {port}")
        ),
        "refused",
        "nothing it was granted remains reachable"
    );

    // Logout ends the program and its proxy, and no control socket answers after.
    second.end(tilo_app_launch::Teardown::Logout);
    tilo_test_env::wait_until("the program ends", || !alive(pid));
    tilo_test_env::wait_until("its proxy ends with it", || !alive(proxy));
    assert!(
        std::os::unix::net::UnixStream::connect(&control).is_err(),
        "no proxy answers on its control socket"
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
    let handed =
        tilo_app_launch::kept::testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
    assert!(
        handed.contains(&"--x11".to_owned()),
        "the launch asks its keeper for an X11 display"
    );
    // The real launcher takes that command line and asks the keeper for the display itself: the
    // session's spelling and the launcher's are held together by the launcher, not by a stand-in.
    let (keeper, keeper_args) = real_launcher_keeper(&handed);
    assert_eq!(keeper, rig.keeper.display().to_string(), "{keeper_args:?}");
    let command = keeper_args
        .iter()
        .position(|arg| arg == "--")
        .expect("the keeper is handed the program's command");
    assert!(
        keeper_args[..command].contains(&"--x11".to_owned()),
        "the keeper is asked for a display: {keeper_args:?}"
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

/// A program its setup put in a Wine capsule opens with Proton once an administrator of the
/// computer chose so: the manager lists the choice as theirs, the program then shows through its
/// keeper's X11 display as Proton's Wine does, and stays open, the same program, through an
/// in-place restart, still listed as opening with Proton. While it is open, Wine does not open
/// its files beside it. Someone who does not administer the
/// computer is told so, and the program keeps opening with Wine.
#[test]
fn an_administrator_opens_a_wine_program_with_proton_and_it_stays_open() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X server");
    let mut rig = KeptRig::new();
    let mut someone = rig.start(false);
    let answers = someone.manage(&rig, "someone", &["list", "proton", "list"]);
    let program = &answers[0]["applications"][0];
    assert_eq!(program["opens_with_choice_available"], false, "{answers:?}");
    assert!(
        program["opens_with_unavailable_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("administrator")),
        "{answers:?}"
    );
    assert_eq!(
        answers[1]["error"]["code"], "not_administrator",
        "{answers:?}"
    );
    assert_eq!(
        answers[2]["applications"][0]["explanation"]["route"], "wine",
        "{answers:?}"
    );
    someone.end(tilo_app_launch::Teardown::Logout);

    rig.administrator = true;
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "administrator", &["list", "proton", "list", "launch"]);
    assert_eq!(
        answers[0]["applications"][0]["opens_with_choice_available"], true,
        "{answers:?}"
    );
    assert_eq!(answers[1]["result"], "opens_with_set", "{answers:?}");
    let program = &answers[2]["applications"][0];
    assert_eq!(program["explanation"]["route"], "proton", "{answers:?}");
    assert_eq!(program["recommended_route"], "wine", "{answers:?}");
    assert_eq!(answers[3]["result"], "launched", "{answers:?}");
    let handed =
        tilo_app_launch::kept::testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
    assert!(
        handed.contains(&"--x11".to_owned()),
        "a program opened with Proton asks its keeper for an X11 display: {handed:?}"
    );
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };

    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    let mut second = rig.start(true);
    second.shown();
    assert_eq!(rig.starts(), [pid], "the same program, not a new one");
    assert!(alive(pid), "still open");
    let answers = second.manage(
        &rig,
        "second",
        &["list", "recommended", "launch", "stop", "ended"],
    );
    let program = &answers[0]["applications"][0];
    assert_eq!(program["running"], true, "{answers:?}");
    assert_eq!(program["explanation"]["route"], "proton", "{answers:?}");
    assert_eq!(answers[1]["result"], "opens_with_set", "{answers:?}");
    // Its prefix is open with Proton (kept through the restart): Wine does not open it beside.
    assert_eq!(
        answers[2]["error"]["code"], "open_with_other_runner",
        "{answers:?}"
    );
    assert!(
        answers[2]["error"]["message"]
            .as_str()
            .is_some_and(|said| said.contains("is open with Proton")),
        "{answers:?}"
    );
    assert_eq!(rig.starts(), [pid], "nothing else started");
    assert_eq!(answers[4]["ended"][0]["entry_id"], rig.entry, "{answers:?}");
    assert_eq!(
        answers[4]["applications"][0]["explanation"]["route"], "wine",
        "back to the recommendation: {answers:?}"
    );
    second.end(tilo_app_launch::Teardown::Logout);
}

/// The tab a Windows program's window took, and the class it showed under, once it mapped.
/// The tab of the program's window, once the session kept the window's class for the next start
/// and wrote the tab into the workspace's layout.
fn saved_window_tab(rig: &KeptRig, desktop: &mut KeptDesktop) -> u64 {
    let (tab, _) = window_tab(&mut desktop.session);
    let workspace = desktop.workspace.clone();
    crate::common::wait_for(&mut desktop.session, "the window's class kept", |_| {
        kept_entry(rig, &workspace)
    });
    crate::layout::until(&mut desktop.session, "the tab is written", |session| {
        session.layout().borrow_mut().last_step().is_some()
    });
    tab
}

/// The tab `tab` of a program that ended while no desktop was there closes, and the program is
/// not started again: `starts` stays the number of its starts.
fn closed_and_not_started(rig: &KeptRig, desktop: &mut KeptDesktop, tab: u64, starts: usize) {
    crate::layout::until(
        &mut desktop.session,
        "the ended program's tab closes",
        |session| {
            !crate::layout::tabs(session)
                .iter()
                .any(|(other, _)| *other == tab)
        },
    );
    for _ in 0..200 {
        desktop
            .session
            .step(Duration::from_millis(1))
            .expect("step");
    }
    assert_eq!(
        rig.starts().len(),
        starts,
        "the ended program is not started again"
    );
}

fn window_tab(session: &mut tilo_session::Session) -> (u64, String) {
    crate::common::wait_for(session, "the program's tab", |session| {
        crate::layout::tabs(session)
            .into_iter()
            .find_map(|(tab, _)| {
                let (tilo_desktop_model::TileContent::Foreign { app_id, .. }
                | tilo_desktop_model::TileContent::LocalApp { app_id, .. }) =
                    crate::layout::tile_of(session, tab)?
                else {
                    return None;
                };
                (app_id == PROGRAM_WINDOW).then_some((tab, app_id))
            })
    })
}

/// The entry `rig`'s window class was seen in `workspace`, as the session keeps it for the start
/// after a reboot: the file the next session reads.
fn kept_entry(rig: &KeptRig, workspace: &str) -> Option<String> {
    let path = rig
        .home
        .data()
        .join("tilo/windows-compat/window-identities.json");
    let seen: Vec<Value> = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    seen.into_iter()
        .find(|seen| seen["workspace"] == workspace && seen["window"] == PROGRAM_WINDOW)
        .and_then(|seen| seen["entry"].as_str().map(str::to_owned))
}

#[test]
fn a_saved_tab_reopens_its_windows_program_after_a_reboot() {
    let rig = KeptRig::new();
    // A desktop that starts the program for the first time: its window takes a tab.
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    first.shown();
    let (tab, class) = window_tab(&mut first.session);
    assert_eq!(class, PROGRAM_WINDOW);
    // What the session saw of the window is kept with the entry it belongs to: the tab it stood
    // in opens the same program after a reboot.
    let workspace = first.workspace.clone();
    crate::common::wait_for(&mut first.session, "the window's class kept", |_| {
        kept_entry(&rig, &workspace)
    });
    assert_eq!(
        kept_entry(&rig, &workspace).as_deref(),
        Some(rig.entry.as_str())
    );
    // The tab the window took is the workspace's, written for the next start.
    crate::layout::until(&mut first.session, "the tab is written", |session| {
        session.layout().borrow_mut().last_step().is_some()
    });
    // The person logs out: the program ends, and nothing of it stays behind.
    first.end(tilo_app_launch::Teardown::Logout);

    // A fresh start, not an in-place restart, finds the tab and opens its program again: the
    // window takes the tab the person left it in.
    let mut second = rig.start(false);
    let reopened =
        crate::common::wait_for(&mut second.session, "the program's window", |session| {
            match crate::layout::tile_of(session, tab)? {
                tilo_desktop_model::TileContent::Foreign { app_id, .. }
                    if app_id == PROGRAM_WINDOW =>
                {
                    Some(app_id)
                }
                _ => None,
            }
        });
    assert_eq!(reopened, PROGRAM_WINDOW);
    assert_eq!(rig.starts().len(), 2, "the program started once more");
    second.end(tilo_app_launch::Teardown::Logout);
}

#[test]
fn a_saved_tab_whose_program_was_removed_says_so_and_starts_nothing() {
    let rig = KeptRig::new();
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    first.shown();
    let (tab, _) = window_tab(&mut first.session);
    crate::common::wait_for(&mut first.session, "the window's class kept", |_| {
        kept_entry(&rig, &first.workspace)
    });
    // The tab the window took is the workspace's, written for the next start.
    crate::layout::until(&mut first.session, "the tab is written", |session| {
        session.layout().borrow_mut().last_step().is_some()
    });
    first.end(tilo_app_launch::Teardown::Logout);

    // The program is removed from its capsule while the tab is closed.
    let store = CapsuleStore::new(rig.home.data().join("tilo/windows-compat")).expect("a store");
    let entry = store
        .registered_programs()
        .expect("the programs")
        .into_iter()
        .find(|entry| entry.entry_id == rig.entry)
        .expect("the program's entry");
    let mut capsule = store
        .load(&entry.capsule_id)
        .expect("the capsule")
        .expect("the capsule");
    capsule.programs.remove(&entry.program.id);
    store.save(&capsule).expect("the capsule saved");

    // The tab says the program is not installed, and nothing starts.
    let mut second = rig.start(false);
    let note = crate::common::wait_for(&mut second.session, "the tab's note", |session| {
        match crate::layout::tile_of(session, tab)? {
            tilo_desktop_model::TileContent::LocalApp { note, .. } if !note.is_empty() => {
                Some(note)
            }
            _ => None,
        }
    });
    assert!(note.contains("not installed"), "{note}");
    assert_eq!(
        rig.starts().len(),
        1,
        "nothing started for the removed program"
    );
    second.end(tilo_app_launch::Teardown::Logout);
}

/// A layout of two workspaces, the second holding a saved window of `app`.
fn two_workspaces_with(app: &str) -> tilo_desktop_model::Layout {
    use tilo_desktop_model::{
        Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };
    let tab = Tab {
        id: 100,
        title: "Probe".into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: "Probe".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut main = Strip::new(StripId(1), "Main");
    main.window_groups = vec![WindowGroup {
        id: 1,
        tabs: Vec::new(),
        active_tab: 0,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    let mut here = Workspace::new(WorkspaceId::from_u128(1), "Home");
    here.strips = vec![main];
    let mut away = Workspace::new(WorkspaceId::from_u128(2), "Away");
    let mut strip = Strip::new(StripId(2), "Later");
    strip.window_groups = vec![WindowGroup {
        id: 10,
        tabs: vec![tab],
        active_tab: 100,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    away.strips = vec![strip];
    let mut tree = Layout {
        active: here.id(),
        workspaces: vec![here, away],
        ..Layout::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(1));
    tree.set_active_group(StripId(1), 1);
    tree
}

#[test]
fn a_saved_tab_of_another_workspace_starts_nothing() {
    let mut rig = KeptRig::new();
    // The program's window was seen in the workspace in front; the saved tab stands in the
    // workspace behind, where the session never saw it.
    let (device, workspaces) =
        crate::layout::Device::with_layout(&mut rig.home, &two_workspaces_with(PROGRAM_WINDOW));
    let seen = serde_json::json!([{
        "workspace": workspaces[0].to_string(),
        "window": PROGRAM_WINDOW,
        "entry": rig.entry,
    }]);
    let path = rig
        .home
        .data()
        .join("tilo/windows-compat/window-identities.json");
    fs::create_dir_all(path.parent().expect("the directory")).unwrap();
    fs::write(&path, seen.to_string()).unwrap();

    let mut second = rig.start_at(false, &device.state);
    let note = crate::common::wait_for(&mut second.session, "the tab's note", |session| {
        crate::layout::tabs(session)
            .into_iter()
            .find_map(|(tab, _)| match crate::layout::tile_of(session, tab)? {
                tilo_desktop_model::TileContent::LocalApp { app_id, note, .. }
                    if app_id == PROGRAM_WINDOW && !note.is_empty() =>
                {
                    Some(note)
                }
                _ => None,
            })
    });
    assert!(note.contains("not installed"), "{note}");
    assert!(
        rig.starts().is_empty(),
        "nothing started in another workspace's place"
    );
    second.end(tilo_app_launch::Teardown::Logout);
}

/// The entries of `dir`, none when it cannot be read.
fn entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect()
}

/// A shell script at `path` running `body`.
fn write_program(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("the stand-in written");
    fs::set_permissions(
        path,
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("the stand-in is executable");
}

/// A private audio graph prepared where `PipeWire`'s configuration is not installed: its
/// directories, for [`audio_daemon`] to listen in.
fn stand_in_graph(
    root: PathBuf,
    _: u32,
    _: bool,
) -> Result<launcher::audio_graph::AudioGraph, launcher::audio_graph::AudioGraphError> {
    fs::create_dir_all(root.join("audio/pulse"))
        .map_err(launcher::audio_graph::AudioGraphError::Io)?;
    Ok(launcher::audio_graph::AudioGraph::at(root))
}

/// The application the stand-in appliance publishes.
const PUBLISHED: &str = "probe.published";

/// The Windows appliance with [`PUBLISHED`] in its catalogue, set up and running: a launch shows
/// it through the RAIL presentation the computer's appliance uses, with the password a guest
/// hands each launch.
struct Appliance {
    rail: compat_service::AudioIsolatedRailLauncher,
}

impl VmDelegate for Appliance {
    fn availability(&self) -> VmAvailability {
        VmAvailability::ready("set up")
    }

    fn inventory(&self) -> VmInventory {
        VmInventory {
            availability: self.availability(),
            applications: vec![VmPublishedApplication {
                entry_id: compat_service::vm_entry_id(PUBLISHED),
                app_id: compat_service::vm_app_id(PUBLISHED),
                published_application_id: PUBLISHED.to_owned(),
                display_name: "Probe".to_owned(),
                presentation: VmPresentation::NativeRail,
                uninstall_available: false,
            }],
        }
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        use windows_vm::RemoteAppLauncher as _;
        let guest = windows_vm::GuestLaunch {
            endpoint: windows_vm::RdpEndpoint {
                address: ([127, 0, 0, 1], 3389).into(),
                username: "tilo-remoteapp".to_owned(),
                certificate_sha256: "a".repeat(64),
            },
            password: windows_vm::Secret::new(PASSWORD.to_vec()),
            remote_app_alias: "tilo-probe".to_owned(),
            launch_nonce: "nonce".to_owned(),
            presentation: windows_vm::ApplicationPresentation::NativeRail,
        };
        let process = self.rail.launch(
            &windows_vm::Shown {
                workspace_id: request.workspace_id,
                application_id: request.published_application_id,
                display_name: "Probe",
            },
            &guest,
            &windows_vm::BrokeredLaunch::deny_all("lease"),
        )?;
        Ok(windows_vm::RunningRemoteApp {
            application_id: PUBLISHED.to_owned(),
            workspace_id: request.workspace_id.to_owned(),
            lease_id: "lease".to_owned(),
            process,
        })
    }
}

/// The application `entry` in the list `answer`.
fn listed<'a>(answer: &'a Value, entry: &str) -> &'a Value {
    answer["applications"]
        .as_array()
        .and_then(|applications| applications.iter().find(|listed| listed["id"] == entry))
        .unwrap_or_else(|| panic!("{entry} is not listed: {answer}"))
}

/// Whether `bytes` holds `needle`.
fn holds(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Every file under `dir` that holds `needle`.
fn files_holding(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in entries(dir) {
        let Ok(kind) = fs::symlink_metadata(&path).map(|metadata| metadata.file_type()) else {
            continue;
        };
        if kind.is_dir() {
            found.extend(files_holding(&path, needle));
        } else if kind.is_file() && fs::read(&path).is_ok_and(|bytes| holds(&bytes, needle)) {
            found.push(path);
        }
    }
    found
}

/// A `RemoteApp` window of the Windows appliance stays open through an in-place restart: its
/// presentation is a launch behind a keeper, on the keeper's X11 display, with its private audio
/// graph as its own children. The RDP password reaches it at a descriptor of its own and appears
/// in no argument, environment or file. After the restart the manager lists it open and cannot
/// quit it (it closes inside Windows), its end is reported even when no desktop was there, and a
/// logout ends it with its keeper and sound.
#[test]
fn a_remote_app_window_stays_open_through_an_in_place_restart() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X server");
    let rig = KeptRig::with_appliance();
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["launch-vm", "list"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    let launched =
        tilo_app_launch::kept::testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
    assert!(
        launched.contains(&"--x11".to_owned()),
        "the presentation asks its keeper for an X11 display: {launched:?}"
    );
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };
    assert!(
        fs::read_to_string(rig.probe.join("display")).is_ok_and(|display| display.starts_with(':')),
        "the presentation was served its keeper's X11 display"
    );
    assert_eq!(
        fs::read_to_string(rig.probe.join("password")).unwrap(),
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
            [PASSWORD, b"\n"].concat()
        )),
        "the password reaches the presentation through its descriptor"
    );
    let mut process = Some(pid);
    while let Some(at) = process.filter(|at| *at != std::process::id()) {
        for part in ["cmdline", "environ"] {
            let read = fs::read(format!("/proc/{at}/{part}")).unwrap_or_default();
            assert!(
                !holds(&read, PASSWORD),
                "process {at}'s {part} holds the password"
            );
        }
        process = parent(at);
    }
    for dir in [rig.home.home(), rig.home.runtime(), rig.home.data()] {
        assert_eq!(files_holding(dir, PASSWORD), Vec::<PathBuf>::new());
    }
    let daemons = rig.appliance_daemons(&mut first);
    for daemon in daemons {
        assert_eq!(
            parent(daemon),
            Some(pid),
            "the sound is the presentation's own"
        );
        assert!(
            !Path::new(&format!("/proc/{daemon}/fd/38")).exists(),
            "the audio daemons do not inherit the password's descriptor"
        );
    }

    // The desktop restarts in place: the presentation stays connected, and the next desktop shows
    // it and lists it open, without a way to quit it.
    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    assert!(alive(pid), "the presentation outlives the desktop");
    let mut second = rig.start(true);
    second.shown();
    assert_eq!(rig.starts(), [pid], "the same presentation, not a new one");
    let answers = second.manage(&rig, "second", &["list", "stop"]);
    assert_eq!(
        listed(&answers[0], &rig.entry)["running"],
        true,
        "{answers:?}"
    );
    assert_eq!(answers[1]["error"]["code"], "not_running", "{answers:?}");
    assert!(alive(pid), "on the same X11 connection, not quit");
    assert!(daemons.iter().all(|daemon| alive(*daemon)));

    // Another appliance program waits for it, as a second client would take its Windows session
    // over: once it closes, the next one opens.
    let answers = second.manage(&rig, "queued", &["launch-vm"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    for _ in 0..200 {
        second.session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(rig.starts(), [pid], "the next one waits for the open one");
    rig.exit(0);
    crate::common::wait_for(&mut second.session, "the next one opens", |_| {
        (rig.starts().len() == 2).then_some(())
    });
    second.shown();
    let tab = saved_window_tab(&rig, &mut second);
    let daemons = rig.appliance_daemons(&mut second);

    // It ends while no desktop is there (the program closed inside Windows): the next desktop
    // reports it, and the tab it left closes rather than opening the program again.
    second.end(tilo_app_launch::Teardown::InPlaceRestart);
    rig.exit(4);
    KeptRig::gone(daemons);
    let mut third = rig.start(true);
    let answers = third.manage(&rig, "third", &["ended"]);
    assert_eq!(answers[0]["ended"][0]["entry_id"], rig.entry, "{answers:?}");
    assert_eq!(answers[0]["ended"][0]["exit_code"], 4, "{answers:?}");
    closed_and_not_started(&rig, &mut third, tab, 2);

    // A logout ends a presentation, its keeper and its sound.
    let answers = third.manage(&rig, "fourth", &["launch-vm"]);
    assert_eq!(answers[0]["result"], "launched", "{answers:?}");
    third.shown();
    let pid = *rig.starts().last().unwrap();
    let daemons = rig.appliance_daemons(&mut third);
    third.end(tilo_app_launch::Teardown::Logout);
    KeptRig::gone([pid]);
    KeptRig::gone(daemons);
    assert!(
        tilo_app_launch::kept::testing::described(&rig.root).is_empty(),
        "no keeper is left"
    );
}

/// Stands in for Bubblewrap and Wine running a setup: in the prefix of the capsule the plan names
/// (in the store at `store`) it does what Wine and a real installer do there, Wine's own
/// placeholder programs beside the one installed.
struct SetUp {
    store: PathBuf,
}

impl SandboxExecutor for SetUp {
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
        install_example(&self.store, &plan.capsule_id)?;
        Ok(SandboxCommand::new(
            PathBuf::from("/usr/bin/true"),
            Vec::<String>::new(),
        ))
    }
}

/// What a setup of the program "Example" leaves in the prefix of `capsule` in `store`: the
/// program, its Start menu shortcut, and the placeholders Wine writes into every prefix.
fn install_example(store: &Path, capsule: &str) -> std::io::Result<()> {
    let prefix = CapsuleStore::new(store)
        .expect("the capsule store")
        .prefix_path(capsule);
    let program_files = prefix.join("drive_c/Program Files");
    let mut placeholder = b"MZ".to_vec();
    placeholder.resize(0x40, 0);
    placeholder.extend_from_slice(b"Wine builtin DLL");
    for (folder, name, bytes) in [
        ("Internet Explorer", "iexplore.exe", placeholder.clone()),
        ("Windows Media Player", "wmplayer.exe", placeholder),
        (
            "Example",
            "example.exe",
            b"MZ the installed program".to_vec(),
        ),
    ] {
        fs::create_dir_all(program_files.join(folder))?;
        fs::write(program_files.join(folder).join(name), bytes)?;
    }
    // The Start menu is what a person sees: the shortcut setup wrote for the program.
    let start_menu = prefix
        .join("drive_c/users/root/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Example");
    fs::create_dir_all(&start_menu)?;
    let mut shortcut = vec![0_u8; 0x4c];
    shortcut[0] = 0x4c;
    shortcut[0x14..0x18].copy_from_slice(&1_u32.to_le_bytes()); // HasLinkTargetIDList
    shortcut.extend_from_slice(&4_u16.to_le_bytes());
    shortcut.extend_from_slice(&[0; 4]);
    fs::write(start_menu.join("Example.lnk"), shortcut)
}

/// A setup program the person chose: the smallest file read as a 64-bit Windows program.
fn setup_exe() -> Vec<u8> {
    let mut installer = vec![0_u8; 0x200];
    installer[..2].copy_from_slice(b"MZ");
    installer[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
    installer[0x80..0x84].copy_from_slice(b"PE\0\0");
    installer[0x84..0x86].copy_from_slice(&0x8664_u16.to_le_bytes());
    installer
}

/// An installer the person chose and approved for a private capsule sets up there, and the
/// program it installed is listed, alone: not the placeholders Wine writes into every prefix.
#[test]
fn an_installer_approved_for_a_private_capsule_lists_what_it_installed() {
    let home = tilo_test_env::TestHome::new().expect("a home");
    // The catalogue knows the probe alone; the installer is routed as any unknown one is.
    let (catalogue, _, _) = set_up_probe(&home.home().join("elsewhere"));
    let store = home.data().join("tilo/windows-compat");
    let assembly = Assembly::with(move |side| {
        compat_service::serve(
            side,
            catalogue,
            SystemRuntimeRegistry::assuming_provider(),
            NoResources,
            SetUp { store },
            LazyVmDelegate::new(|| Err("no appliance is set up here".to_owned())),
        )
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
    let display = home
        .runtime()
        .join(session.compositor().listen().expect("the display binds"));
    apps.set_displays(display, None, None);
    apps.set_render_node(Some(PathBuf::from("/dev/dri/renderD128")));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace in front");
    let socket = launcher::services::socket_for(launcher::services::COMPAT_SERVICE);
    crate::common::wait_for(&mut session, "the compatibility service", |_| {
        socket.exists().then_some(())
    });

    let dir = home.home().join("manager");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workspace"), &workspace).unwrap();
    fs::write(dir.join("steps"), "install\nsettled").unwrap();
    fs::write(dir.join("setup.exe"), setup_exe()).unwrap();
    let mut manager = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "windows_compat::manager_steps", "--nocapture"])
        .env(STEPS_DIR, &dir)
        .spawn_locked()
        .expect("the manager's launcher starts");
    launches.started(manager.id(), MANAGER, &workspace, true);
    let answers = crate::common::wait_for(&mut session, "the manager's answers", |_| {
        fs::read_to_string(dir.join("answers")).ok()
    });
    manager.wait().expect("the launcher ends");
    let answers: Vec<Value> = serde_json::from_str(&answers).expect("answers");

    assert_eq!(
        answers[0]["result"], "vm_install_confirmation",
        "the service asks where the installer goes: {answers:?}"
    );
    assert_eq!(answers[1]["result"], "installed", "{answers:?}");
    assert_eq!(
        answers[2]["installations"][0]["status"], "installed",
        "{answers:?}"
    );
    let names: Vec<&str> = answers[2]["applications"]
        .as_array()
        .expect("the list")
        .iter()
        .filter_map(|application| application["display_name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["Example"],
        "only the installed program: {answers:?}"
    );
}

/// A Windows setup is one of the session's launches: it stays open through an in-place restart,
/// which never waits on it, and the desktop after the restart shows it and its setup in progress,
/// then lists what it installed once it ends. A logout ends it without waiting on it either.
#[test]
fn a_windows_setup_stays_open_through_an_in_place_restart_and_never_holds_it_up() {
    let mut rig = KeptRig::new();
    rig.setup = true;
    let mut first = rig.start(false);
    let answers = first.manage(&rig, "first", &["install", "list"]);
    assert_eq!(answers[1]["result"], "installed", "{answers:?}");
    assert_eq!(
        answers[1]["receipt"]["status"], "installing",
        "the manager is answered while the setup is open: {answers:?}"
    );
    assert_eq!(
        answers[2]["installations"][0]["status"], "installing",
        "{answers:?}"
    );
    first.shown();
    let [pid] = rig.starts()[..] else {
        panic!("one setup: {:?}", rig.starts());
    };

    let restart = std::time::Instant::now();
    first.end(tilo_app_launch::Teardown::InPlaceRestart);
    assert!(
        restart.elapsed() < Duration::from_secs(10),
        "a restart waited {:?} on the open setup",
        restart.elapsed()
    );
    assert!(alive(pid), "the setup outlives the desktop that started it");
    let mut second = rig.start(true);
    second.shown();
    assert_eq!(rig.starts(), [pid], "the same setup, not a new one");
    let answers = second.manage(&rig, "second", &["list"]);
    assert_eq!(
        answers[0]["installations"][0]["status"], "installing",
        "the setup is still in progress: {answers:?}"
    );
    rig.exit(0);
    let answers = second.manage(&rig, "third", &["settled"]);
    assert_eq!(
        answers[0]["installations"][0]["status"], "installed",
        "{answers:?}"
    );
    assert!(
        answers[0]["applications"]
            .as_array()
            .expect("the list")
            .iter()
            .any(|application| application["display_name"] == "Example"),
        "what it installed: {answers:?}"
    );

    // A logout ends an open setup, without waiting on it.
    let answers = second.manage(&rig, "fourth", &["install"]);
    assert_eq!(answers[1]["receipt"]["status"], "installing", "{answers:?}");
    second.shown();
    let pid = *rig.starts().last().unwrap();
    let logout = std::time::Instant::now();
    second.end(tilo_app_launch::Teardown::Logout);
    assert!(
        logout.elapsed() < Duration::from_secs(10),
        "a logout waited {:?} on the open setup",
        logout.elapsed()
    );
    KeptRig::gone([pid]);
    // The next desktop shows that setup as ended, so it can be retried or removed.
    let mut third = rig.start(false);
    let answers = third.manage(&rig, "fifth", &["list"]);
    let installations = answers[0]["installations"].as_array().expect("the list");
    assert!(
        installations
            .iter()
            .all(|installation| installation["status"] != "installing"),
        "no setup is left in progress: {answers:?}"
    );
    assert!(
        installations
            .iter()
            .any(|installation| installation["status"] == "failed"),
        "{answers:?}"
    );
}
