//! The native session: `tilo-session [--backend drm|headless|nested]
//! [--size WxH] [--shell bar|screen] [--host-desktop]
//! [--firstboot|--firstboot-only]
//! [--provision-identity|--verify-collaboration-identity] [--no-x11]
//! [--session-bus ADDRESS|none] [--system-bus ADDRESS|none]
//! [--admit-repl PATH]... [--test-control PATH] [--seed-apps]
//! [-- command args...]`. Listens on a fresh `wayland-N` socket, starts
//! Xwayland for X11 clients (unless `--no-x11`), prints both displays and the
//! agent door it serves (`TILO_AGENT_DOOR=`, what `tilolang repl --door`
//! takes) and starts `command` with `WAYLAND_DISPLAY` and `DISPLAY` set to
//! them; with a command, the session lasts as long as it does. `--shell`
//! shows the tilolang shell (`apps/tl/shell`): its bar over the desktop
//! (`bar`), or the whole screen (`screen`), its bar, dock and the person's
//! arrangement as their preset shows it, every window in its place's well. The desktop services serve
//! on the buses the environment names unless a flag names another (or none);
//! a nested session, which runs in a window of a desktop it did not start,
//! serves no bus unless a flag names one and exports nothing to that
//! desktop's systemd user manager and D-Bus activation. A command started in
//! the session is given the session bus a flag named, so the portal and the
//! notifications it reaches are the session's own. The two identity commands
//! open no display: they establish the machine's identity or prove that the
//! one it holds verifies, and the process stops with their answer.
//! `--test-control PATH`
//! opens the development test-control socket there (`tilo-control
//! snapshot`), in development builds only, and prints it
//! (`TILO_TEST_CONTROL=`); `--seed-apps` publishes the tilolang notes,
//! tasks, chat and mail apps filled with their seed data
//! ([`tilo_session::seeded_apps`]), also in development builds only.
//! SIGTERM, SIGINT and SIGHUP end the session as ending on its own does, so
//! what it mounted and bound goes with it.

#[cfg(target_os = "linux")]
use tilo_session::desktop_services::{Bus, Buses};
#[cfg(target_os = "linux")]
use tilo_session::{Backend, Reach, SessionEnvironment};

/// Byte string stamped into a session built with `--features dev-session`, so
/// the package's developer entry can tell a deliberately built session from
/// any other executable before it starts one as the login session. A release
/// build never carries it.
///
/// `#[used]` keeps the linker from discarding a static nothing reads.
#[cfg(all(feature = "dev-session", debug_assertions))]
#[used]
static DEV_SESSION_BUILD_MARKER: [u8; 32] = *b"tilo-app-session-build-marker-v1";

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (ours, command) = split_command(&args);
    // `--firstboot` is the host package's wrapper hook, which goes on to the
    // session it was starting.
    let host_desktop = ours.iter().any(|arg| arg == "--host-desktop");
    if host_desktop {
        // A session installed on another distribution keeps its device
        // identity where that login can write, and every child it starts —
        // the launcher, the brokers — reads the same tree. This runs before
        // anything here starts a thread, as it requires.
        match tilo_onboarding_host::identity_provisioning::configure_host_identity_root() {
            Ok(root) => eprintln!(
                "Tilo host session: device identity will be stored under {}",
                root.display()
            ),
            Err(error) => {
                eprintln!("tilo-session: cannot resolve host-session identity storage: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    // The desktop's tilolang surfaces start compiled once Cranelift compiled them in an
    // earlier session.
    if let Some(cache) = tilo_paths::xdg::cache_home()
        && let Err(error) = tilolang_tilo::runtime::engine::keep_compiled_in(
            cache.join("tilo").join("tilolang-compiled"),
        )
    {
        eprintln!("tilo-session: compiled tilolang modules are not kept: {error}");
    }
    if let Some(done) = first_boot(ours, host_desktop) {
        return done;
    }
    match tilo_session::startup::command(ours) {
        Err(said) => {
            eprintln!("tilo-session: {said}");
            return ExitCode::from(2);
        }
        Ok(Some(command)) => return tilo_session::startup::run(command, host_desktop),
        Ok(None) => {}
    }
    let identity = tilo_session::startup::identity();
    match &identity {
        tilo_session::startup::Identity::Desk
        | tilo_session::startup::Identity::WaitForClock { .. } => {}
        tilo_session::startup::Identity::Recovery(problem) => {
            eprintln!(
                "tilo-session: this device's Tilo identity needs its owner: {}",
                problem.as_str()
            );
        }
        tilo_session::startup::Identity::Refused(said) => {
            eprintln!("tilo-session: this device's identity did not verify: {said}");
            return ExitCode::FAILURE;
        }
    }
    let Some(line) = command_line(ours) else {
        return ExitCode::FAILURE;
    };
    start(ours, command, line, host_desktop, &identity)
}

/// The session's own arguments and the command it starts, split at `--`.
#[cfg(target_os = "linux")]
fn split_command(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|arg| arg == "--") {
        Some(split) => (
            args.get(..split).unwrap_or_default(),
            args.get(split.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (args, &[][..]),
    }
}

/// The machine's first boot: setup runs and the process stops there, before
/// any GPU, font or session state is opened. What it returns is the code the
/// process exits with when it stops here.
#[cfg(target_os = "linux")]
fn first_boot(ours: &[String], host_desktop: bool) -> Option<std::process::ExitCode> {
    use std::process::ExitCode;
    use tilo_session::mount::firstboot::{self, OneShot};
    match firstboot::one_shot(ours) {
        Ok(Some(OneShot::SetupOnly)) => Some(match firstboot::run_setup(host_desktop) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilo-session: first boot setup failed: {error}");
                ExitCode::FAILURE
            }
        }),
        Ok(Some(OneShot::SetupThenSession)) => {
            if let Err(error) = firstboot::run_setup(host_desktop) {
                // A machine that cannot be prepared still boots: the person
                // sees what is missing rather than a login that never comes.
                eprintln!("tilo-session: first boot setup failed: {error}; starting anyway");
            }
            None
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            Some(ExitCode::from(2))
        }
    }
}

/// What the command line asked of the session: the backend it runs on, the
/// size of a headless or nested one, and what it reaches of the desktop it
/// runs in.
#[cfg(target_os = "linux")]
struct CommandLine {
    backend: Backend,
    size: (u16, u16),
    reach: Reach,
    /// What the tilolang shell shows, when the command line asked for it.
    shell: Option<tilo_surface_host::shell_part::ShellLayer>,
    /// The programs the person admits to act as them at the agent door
    /// (`--admit-repl PATH`, as often as they name one): their decision,
    /// made as they start the session.
    admit_repl: Vec<std::path::PathBuf>,
    /// Where the development test-control socket opens, when the command
    /// line named one and this build opens it.
    test_control: Option<std::path::PathBuf>,
    /// Whether the session publishes the seeded tilolang apps
    /// (`--seed-apps`, in a development build).
    seed_apps: bool,
}

/// The command line `ours` parsed, or `None` when it names a backend there
/// is not.
#[cfg(target_os = "linux")]
fn command_line(ours: &[String]) -> Option<CommandLine> {
    let value = |flag: &str| {
        ours.iter()
            .position(|arg| arg == flag)
            .and_then(|index| ours.get(index.saturating_add(1)))
    };
    let name = value("--backend").map_or("drm", String::as_str);
    let Some(backend) = Backend::named(name) else {
        eprintln!("tilo-session: unknown backend {name} (drm, headless, nested)");
        return None;
    };
    let shell = match value("--shell").map(String::as_str) {
        None => None,
        Some("bar") => Some(tilo_surface_host::shell_part::ShellLayer::Bar),
        Some("screen") => Some(tilo_surface_host::shell_part::ShellLayer::Screen),
        Some(other) => {
            eprintln!("tilo-session: unknown shell {other} (bar, screen)");
            return None;
        }
    };
    let size: (u16, u16) = value("--size")
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));
    let bus = |flag: &str| match value(flag).map(String::as_str) {
        None => Bus::Default,
        Some("none") => Bus::None,
        Some(address) => Bus::Address(address.to_owned()),
    };
    // What a session in a window of another desktop reaches of it: no bus of
    // it unless a flag names one, and no displays exported to it.
    let reach = Reach::new(
        backend,
        Buses {
            session: bus("--session-bus"),
            system: bus("--system-bus"),
        },
    );
    let admit_repl = ours
        .iter()
        .zip(ours.iter().skip(1))
        .filter(|(flag, _)| *flag == "--admit-repl")
        .map(|(_, program)| std::path::PathBuf::from(program))
        .collect();
    let named = value("--test-control").map(String::as_str);
    let test_control =
        tilo_session::test_control::path(tilo_session::test_control::OPENED_BY_THIS_BUILD, named);
    if named.is_some() && test_control.is_none() {
        eprintln!("tilo-session: --test-control is honoured by development builds only");
    }
    let asked_seed = ours.iter().any(|arg| arg == "--seed-apps");
    let seed_apps = asked_seed && tilo_session::seeded_apps::SEEDED_BY_THIS_BUILD;
    if asked_seed && !seed_apps {
        eprintln!("tilo-session: --seed-apps is honoured by development builds only");
    }
    Some(CommandLine {
        backend,
        size,
        reach,
        shell,
        admit_repl,
        test_control,
        seed_apps,
    })
}

/// The session `line` describes, run until `command` exits.
#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_lines,
    reason = "the session's startup reads in one place, from its services to the doors it opens"
)]
fn start(
    ours: &[String],
    command: &[String],
    line: CommandLine,
    host_desktop: bool,
    identity: &tilo_session::startup::Identity,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    use std::sync::Arc;
    use tilo_session::Options;

    let CommandLine {
        backend,
        size,
        reach,
        shell,
        admit_repl,
        test_control,
        seed_apps,
    } = line;
    let buses = reach.buses.clone();
    record_panics(backend);
    // Who sits here, read now and kept current by workers: nothing the loop
    // asks of the identity directory waits on the disk.
    tilo_session::identity::install();
    let wake = tilo_service::LoopWake::default();
    // The person's settings, read once here: the power policy they resolve is
    // part of them.
    let config = tilo_session::SessionConfig::load();
    // The machine's power: that policy, applied by the session's loop and
    // acted on over logind by the service started beside the others. A
    // headless session has no lid, keys or person at a seat: it would act on
    // the battery and idle of a machine whose own session does that.
    let power =
        (backend != Backend::Headless).then(|| tilo_session_power::PowerLink::new(config.power));
    // A session in a window of another desktop runs on that desktop's
    // machine: its services stay off the host's daemons, its device stays
    // the host's, and no session action it takes reaches the host.
    let machine = if matches!(backend, Backend::Nested) {
        tilo_session::Machine::Window
    } else {
        tilo_session::Machine::Owned
    };
    let (mut services, _following) = services(&wake, power.clone(), machine);
    let (capture, captures) =
        tilo_session::capture::channel_to(tilo_session::capture::directory(), wake.handle());
    // The portal's side of the desktop, which the session answers.
    let (portal_side, portal_link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = desktop_services(
        &buses,
        &wake,
        Arc::new(capture),
        &portal_side,
        &mut services,
        power
            .as_ref()
            .map(|power_link| power_link.registry().clone()),
    );
    // Where input captures and the shared screens are is the portal's: its
    // capture broker tells them what the session's seat and outputs are.
    let portal = desktop
        .as_ref()
        .and_then(|desktop| desktop.portal())
        .map(|serving| portal_link.with_captures(serving.input_captures().clone()));
    tell_the_person(backend, desktop.as_deref());
    // The apps this session opens start from its own displays and the
    // person's variables, once the compositor has bound its socket.
    let launch = std::rc::Rc::new(tilo_session::mount::apps::Apps::new(
        tilo_app_launch::SessionEnvironment::from_process(),
        Some(wake.handle()),
    ));
    // A drive the person plugs in is announced like any other news, and the
    // eject the notice offers goes over the system bus the desktop services
    // already hold.
    let drives = drives(backend, desktop.as_ref());
    let mut options = Options {
        services,
        wake,
        setup: tilo_session::startup::before_desk(identity, host_desktop),
        captures: Some(captures),
        questions: desktop.as_ref().map(|desktop| desktop.questions().clone()),
        apps: Some(std::rc::Rc::clone(&launch) as std::rc::Rc<dyn tilo_session::AppActions>),
        launch: Some(std::rc::Rc::clone(&launch)),
        drives,
        desktop,
        portal,
        power,
        machine,
        shell,
        admit_repl,
        test_control,
        ..device_options(machine)
    };
    // A headless session has no input device: nothing the person does can
    // ever reset its idle timers, so they would only blank and lock it.
    if backend == Backend::Headless {
        options.config.idle = tilo_compositor::IdleConfig::never();
    }
    if seed_apps {
        match tilo_session::seeded_apps::seeded(&tilo_session::identity::seated_person(&options)) {
            Ok(apps) => options.services.push(Box::new(apps)),
            Err(error) => eprintln!("tilo-session: the seeded apps did not load: {error}"),
        }
    }
    // A signal ends the loop rather than the process, so the session drops
    // what it holds as it does when it ends on its own.
    if let Err(error) = tilo_session::termination::install(options.wake.handle()) {
        eprintln!("tilo-session: a signal ends this session without its cleanup: {error}");
    }
    let agent_door = options.agent_door.clone();
    let test_control = options.test_control.clone();
    let mut session = match start_session(backend, options, size) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    watch_the_loop(backend, &mut session);
    let session_bus = match &buses.session {
        Bus::Address(address) => Some(address.as_str()),
        _ => None,
    };
    run(
        session,
        ours,
        command,
        Some(&launch),
        session_bus,
        reach.export_displays,
        host_desktop,
        &Doors {
            agent: agent_door.as_deref(),
            test_control: test_control.as_deref(),
        },
    )
}

/// The sockets the session bound as it started, printed for what drives it.
#[cfg(target_os = "linux")]
struct Doors<'a> {
    /// The agent door (`TILO_AGENT_DOOR=`).
    agent: Option<&'a std::path::Path>,
    /// The development test-control socket (`TILO_TEST_CONTROL=`).
    test_control: Option<&'a std::path::Path>,
}

/// The session on `backend`'s displays, at `size` where the backend runs in
/// one.
fn start_session(
    backend: Backend,
    options: tilo_session::Options,
    size: (u16, u16),
) -> Result<tilo_session::Session, tilo_compositor::Error> {
    use tilo_compositor::{DisplayConfig, NestedConfig, OutputConfig};
    use tilo_session::Session;
    match backend {
        Backend::Headless => Session::headless(
            options,
            &[OutputConfig::new(
                "HEADLESS-1",
                i32::from(size.0),
                i32::from(size.1),
            )],
        ),
        Backend::Drm => Session::drm(options, DisplayConfig::default()),
        Backend::Nested => Session::nested(
            options,
            &NestedConfig {
                size,
                ..NestedConfig::default()
            },
        ),
    }
}

/// `session` running until `command` exits, its displays printed.
#[cfg(target_os = "linux")]
fn run(
    mut session: tilo_session::Session,
    ours: &[String],
    command: &[String],
    launch: Option<&std::rc::Rc<tilo_session::mount::apps::Apps>>,
    session_bus: Option<&str>,
    export_displays: bool,
    host_desktop: bool,
    doors: &Doors<'_>,
) -> std::process::ExitCode {
    use std::process::ExitCode;

    let socket = match session.compositor().listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("WAYLAND_DISPLAY={socket}");
    // A session added to this machine's own login serves the installed folder
    // importer: whether identity setup is over, on the socket named for this
    // session's display, and the review the person left to be opened after it.
    if host_desktop && let Err(error) = session.serve_folder_import(std::ffi::OsStr::new(&socket)) {
        tracing::warn!(%error, display = %socket, "Add folders cannot ask whether setup is done");
    }
    // Where a program of this login asks to work here as an agent: what
    // `tilolang repl --door` connects to. The session bound it as it started.
    if let Some(door) = doors.agent.filter(|door| door.exists()) {
        println!("TILO_AGENT_DOOR={}", door.display());
    }
    // Where `tilo-control` drives it, in a development build started with one.
    if let Some(control) = doors.test_control.filter(|control| control.exists()) {
        println!("{}={}", tilo_session::test_control::ENV, control.display());
    }
    let x11_display = if ours.iter().any(|arg| arg == "--no-x11") {
        None
    } else {
        start_x11(session.compositor())
    };
    // The apps opened from now on connect where the person's windows are.
    // X11 needs the authority file Xwayland was started with as well, which
    // the compositor does not hand out yet.
    if let Some(launch) = launch {
        launch.set_displays(
            tilo_paths::xdg::runtime_dir_or_default().join(&socket),
            x11_display.clone().map(|display| (display, None)),
            session_bus.map(str::to_owned),
        );
    }
    let environment = SessionEnvironment {
        wayland_display: socket,
        x11_display,
        x11_authority: None,
    };
    if export_displays {
        environment.clone().export();
    }
    let mut child = command
        .split_first()
        .and_then(|(program, rest)| start_command(program, rest, &environment, session_bus));
    match session.run(|_| {
        tilo_session::termination::asked()
            || child
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A session that ends without a word leaves the person at the login screen
/// with no idea why: the one that owns the machine leaves a record of every
/// panic, which its next start tells them about.
#[cfg(target_os = "linux")]
fn record_panics(backend: Backend) {
    if backend == Backend::Drm {
        tilo_crash_records::install(tilo_crash_records::PANIC_TITLE);
    }
}

/// What this desktop knows of its own machine reaches the person through
/// their notifications. Only the session that owns the machine tells them: a
/// session in a window of another desktop, or a headless one, has that
/// machine's storage, updates and history to leave alone.
#[cfg(target_os = "linux")]
fn tell_the_person(
    backend: Backend,
    desktop: Option<&tilo_session::desktop_services::DesktopServices>,
) {
    if backend == Backend::Drm
        && let Some(desktop) = desktop
    {
        tilo_session::notices::start(desktop.notifications());
    }
}

/// The drives this desktop announces, when it owns the machine and the
/// desktop services hold a system bus to ask udisks over: a session in a
/// window of another desktop leaves that desktop's drives to it.
#[cfg(target_os = "linux")]
fn drives(
    backend: Backend,
    desktop: Option<&std::rc::Rc<tilo_session::desktop_services::DesktopServices>>,
) -> Option<std::rc::Rc<tilo_session::mount::drives::Drives>> {
    if backend != Backend::Drm {
        return None;
    }
    let desktop = desktop?;
    let connection = desktop.system_bus()?.clone();
    Some(std::rc::Rc::new(tilo_session::mount::drives::Drives::new(
        tilo_session::mount::drives::udisks(connection),
        desktop.notifications().clone(),
    )))
}

/// A main loop that stops for good keeps the display frozen on its last frame
/// and the keyboard unread; the watchdog records why and ends the process, so
/// the person comes back to the login screen. Only the TTY backend owns the
/// display; a window or a headless run leaves nothing to hand back.
#[cfg(target_os = "linux")]
fn watch_the_loop(backend: Backend, session: &mut tilo_session::Session) {
    if backend == Backend::Drm
        && let Err(error) = tilo_session::notices::guard_main_loop(session.compositor())
    {
        tracing::warn!(%error, "the main loop is not watched");
    }
}

/// The desktop services on `buses`, their resources added to `services`;
/// what keeps them running.
#[cfg(target_os = "linux")]
fn desktop_services(
    buses: &tilo_session::desktop_services::Buses,
    wake: &tilo_service::LoopWake,
    capture: std::sync::Arc<dyn tilo_portal::Capture>,
    side: &tilo_session::portal_link::PortalSide,
    services: &mut Vec<Box<dyn tilo_session::Service>>,
    inhibitors: Option<tilo_power_mgmt::InhibitRegistry>,
) -> Option<std::rc::Rc<tilo_session::desktop_services::DesktopServices>> {
    match tilo_session::desktop_services::DesktopServices::start(
        buses,
        &wake.handle(),
        capture,
        side,
        inhibitors,
    ) {
        Ok(desktop) => {
            services.extend(desktop.services());
            Some(std::rc::Rc::new(desktop))
        }
        Err(error) => {
            tracing::warn!(%error, "the desktop services did not start");
            None
        }
    }
}

/// What the session reads of this device: its configuration, search, quick
/// settings over its own backends and the players, its session actions and
/// who unlocks it. A session in a window of another desktop
/// ([`tilo_session::Machine::Window`]) reads no device of its own and
/// carries out no session action: the window's machine is the host's.
#[cfg(target_os = "linux")]
fn device_options(machine: tilo_session::Machine) -> tilo_session::Options {
    use tilo_session::{
        ActionPlane, DeviceHost, Options, QuickSettings, SessionConfig, Vpn,
        displays::{Backlights, Screens},
    };
    let owned = machine.is_owned();
    Options {
        config: SessionConfig::load(),
        search: search_options(),
        // The apps this session starts reach its services over the socket
        // the launcher hands them; a session inside another desktop's
        // window leaves that desktop's own socket alone.
        action_plane: if owned {
            ActionPlane::Launcher
        } else {
            ActionPlane::Off
        },
        // A program of this login asks the person at the seat's agent door
        // to work in a workspace as an agent; a session in another
        // desktop's window serves none.
        agent_door: owned.then(tilo_session::agent_door::path).flatten(),
        quick_settings: owned.then(|| QuickSettings::Device(Box::new(DeviceHost::os))),
        // A session in another desktop's window dims none of the window's
        // machine: the display it draws in is that desktop's.
        backlights: owned.then(|| Box::new(Screens) as Box<dyn Backlights>),
        // The controllers in the room drive the desktop of the session that
        // owns the machine; a session in another desktop's window leaves
        // them to that desktop.
        controllers: owned.then_some(tilo_session::controllers::Source::Machine),
        session_actions: if owned {
            os_actions::perform_session_action
        } else {
            tilo_session::no_session_actions
        },
        // The lock screen and the on-screen keyboard are the session's own
        // apps: the screen is started when the session locks, and the
        // keyboard whenever the person may want it, under the hands they
        // type with.
        surfaces: surfaces(),
        user: std::env::var("USER").unwrap_or_default(),
        verifier: tilo_session::unlock::installed(),
        state: tilo_paths::state_dir(),
        // A session in a window of another desktop leaves the machine's
        // traffic to the session that owns it.
        vpn: owned.then(Vpn::machine),
        ..Options::default()
    }
}

/// The surface apps the session starts itself: its lock screen and its
/// on-screen keyboard, each when what it shows is wanted. A machine without
/// either loses that surface, never the session.
#[cfg(target_os = "linux")]
fn surfaces() -> Vec<tilo_session::surfaces::Surface> {
    let mut surfaces = lock_screen();
    match tilo_session::keyboard::surface() {
        Ok(keyboard) => surfaces.push(keyboard),
        Err(error) => eprintln!("tilo-session: no on-screen keyboard: {error}"),
    }
    surfaces
}

/// The lock screen, when one is installed: the app whose window asks the
/// compositor for the session's lock, drawn on every output and given every
/// input while the session is locked.
#[cfg(target_os = "linux")]
fn lock_screen() -> Vec<tilo_session::surfaces::Surface> {
    let program = match tilo_session::surfaces::program_path("tilo-lock-screen") {
        Ok(program) => program,
        Err(error) => {
            eprintln!("tilo-session: no lock screen: {error}");
            return Vec::new();
        }
    };
    vec![
        tilo_session::surfaces::Surface::new("lock", program)
            .person(std::env::var("USER").unwrap_or_default())
            .lock(),
    ]
}

/// Xwayland for X11 clients, its display printed. They connect as soon as
/// its sockets are there, which they are once it is started; without it
/// only Wayland is named.
#[cfg(target_os = "linux")]
fn start_x11(compositor: &mut tilo_compositor::Compositor) -> Option<String> {
    match compositor.start_xwayland() {
        Ok(display) => {
            let display = format!(":{display}");
            println!("DISPLAY={display}");
            Some(display)
        }
        Err(error) => {
            tracing::warn!(%error, "Xwayland did not start: no X11 clients");
            None
        }
    }
}

/// `program` started with `rest` on `environment`'s displays and, when a
/// flag named the session bus, on that bus rather than the one it inherits.
#[cfg(target_os = "linux")]
fn start_command(
    program: &str,
    rest: &[String],
    environment: &SessionEnvironment,
    session_bus: Option<&str>,
) -> Option<std::process::Child> {
    let mut started = std::process::Command::new(program);
    started
        .args(rest)
        .env("WAYLAND_DISPLAY", &environment.wayland_display);
    match &environment.x11_display {
        Some(display) => started.env("DISPLAY", display),
        None => started.env_remove("DISPLAY"),
    };
    if let Some(address) = session_bus {
        started.env("DBUS_SESSION_BUS_ADDRESS", address);
    }
    started
        .spawn()
        .map_err(|error| eprintln!("tilo-session: {program}: {error}"))
        .ok()
}

/// What keeps the services' readings following the device while it lives.
#[cfg(target_os = "linux")]
type Following = Vec<Box<dyn std::any::Any>>;

/// The system services the session hosts, on this device's own daemons,
/// and what keeps their readings following the device. A session in a
/// window of another desktop hosts only the settings service: the machine's
/// connectivity, clock, time zone and audio routing are the host's, and
/// changing them from a window would change the host.
#[cfg(target_os = "linux")]
fn services(
    wake: &tilo_service::LoopWake,
    power: Option<tilo_session_power::PowerLink>,
    machine: tilo_session::Machine,
) -> (Vec<Box<dyn tilo_session::Service>>, Following) {
    use std::sync::Arc;
    use tilo_connectivity_service::{Connectivity, device::OsDevice};

    let mut services: Vec<Box<dyn tilo_session::Service>> = Vec::new();
    let mut following: Following = Vec::new();
    services.push(Box::new(tilo_settings_service::SettingsService::new()));
    // What this session ran into: the list as the resource's reading and
    // each record's text as its own, which Settings reads as an app does.
    services.push(Box::new(tilo_crash_records::CrashRecords::new()));
    if !machine.is_owned() {
        return (services, following);
    }
    if let Some(link) = power {
        match tilo_session_power::SessionPower::start(link) {
            Ok(power) => services.push(Box::new(power)),
            Err(error) => tracing::warn!(%error, "the power service did not start"),
        }
    }
    match Connectivity::new(Arc::new(OsDevice), Some(wake.handle())) {
        Ok(connectivity) => {
            match connectivity.follow() {
                Ok(follow) => following.push(Box::new(follow)),
                Err(error) => tracing::warn!(%error, "connectivity does not follow the device"),
            }
            services.push(Box::new(connectivity));
        }
        Err(error) => tracing::warn!(%error, "connectivity service not started"),
    }
    match tilo_system_service::SystemService::new(
        Arc::new(tilo_system_service::device::OsSystem),
        Some(wake.handle()),
    ) {
        Ok(mut system) => {
            if let Err(error) = system.follow_location() {
                tracing::warn!(%error, "the time zone does not follow the device");
            }
            services.push(Box::new(system));
        }
        Err(error) => tracing::warn!(%error, "system service not started"),
    }
    match audio_helper().map(|helper| {
        let mut command = std::process::Command::new(helper);
        command.arg("--serve");
        tilo_service::lines::Process::spawn(
            command,
            tilo_audio_service::publish,
            Some(wake.handle()),
        )
    }) {
        Some(Ok(audio)) => services.push(Box::new(audio)),
        Some(Err(error)) => tracing::warn!(%error, "the audio service did not start"),
        None => tracing::warn!("the audio service is not installed here"),
    }
    (services, following)
}

/// The audio service (`tilo-audio`): beside the session, else where the
/// image installs it.
#[cfg(target_os = "linux")]
fn audio_helper() -> Option<std::path::PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-audio")));
    beside
        .into_iter()
        .chain([std::path::PathBuf::from("/usr/libexec/tilo-audio")])
        .find(|path| path.exists())
}

/// What search reads of this device: the Tilo apps installed here and the
/// machine's own and the apps the person can get, each followed as it
/// changes, what apps hold, the person's files, the records the workspaces
/// hold, and the search sources they switched off.
#[cfg(target_os = "linux")]
fn search_options() -> tilo_session::SearchOptions {
    use std::sync::Arc;
    use tilo_desktop_search::providers::files::FileNames;

    let files = tilo_paths::home().map(|home| {
        let content = tilo_desktop_search::files_index::FilesIndex::open_or_create(&home)
            .ok()
            .map(Arc::new);
        (home.clone(), FileNames::scan(home), content)
    });
    // What a reference written under an app's earlier id names, for search
    // and for every decision this process makes.
    let content = tilo_desktop_search::content_index::session();
    match tilo_installed_apps::earlier_ids_from_env() {
        Ok(earlier) => {
            tilo_action_policy::earlier::register_all(&earlier);
            content.set_earlier_ids(earlier);
        }
        Err(error) => eprintln!("tilo-session: earlier app ids: {error}"),
    }
    tilo_session::SearchOptions {
        lists: tilo_session::Lists::of_device(),
        content: Some(content),
        files,
        preferences: tilo_desktop_search::SearchPreferences::from_values(
            &settings_store::load_section(tilo_desktop_search::preferences::SECTION),
        ),
        places: tilo_desktop_search::providers::places::RecentPlaces::default(),
        // The records the workspaces on this device hold, read now and
        // followed as the apps of those workspaces name more.
        records: Some(tilo_session::Records::of_device()),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
