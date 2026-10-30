//! The native session: `tilo-session [--backend drm|headless] [--size WxH]
//! [--host-desktop] [--firstboot|--firstboot-only] [--no-x11] [--session-bus ADDRESS|none] [--system-bus
//! ADDRESS|none] [-- command args...]`. Listens on a fresh `wayland-N`
//! socket, starts Xwayland for X11 clients (unless `--no-x11`), prints both
//! displays and starts `command` with `WAYLAND_DISPLAY` and `DISPLAY` set
//! to them; with a command, the session lasts as long as it does. The desktop
//! services serve on the buses the environment names unless a flag names
//! another (or none).

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
    use std::sync::Arc;
    use tilo_compositor::{DisplayConfig, OutputConfig};
    use tilo_session::desktop_services::{Bus, Buses};
    use tilo_session::{Options, Session};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (ours, command) = match args.iter().position(|arg| arg == "--") {
        Some(split) => (
            args.get(..split).unwrap_or_default(),
            args.get(split.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (args.as_slice(), &[][..]),
    };
    // The machine's first boot: setup runs and the process stops there,
    // before any GPU, font or session state is opened. `--firstboot` is the
    // host package's wrapper hook, which goes on to the session it was
    // starting.
    let host_desktop = ours.iter().any(|arg| arg == "--host-desktop");
    match tilo_session::mount::firstboot::one_shot(ours) {
        Ok(Some(tilo_session::mount::firstboot::OneShot::SetupOnly)) => {
            return match tilo_session::mount::firstboot::run_setup(host_desktop) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("tilo-session: first boot setup failed: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(Some(tilo_session::mount::firstboot::OneShot::SetupThenSession)) => {
            if let Err(error) = tilo_session::mount::firstboot::run_setup(host_desktop) {
                // A machine that cannot be prepared still boots: the person
                // sees what is missing rather than a login that never comes.
                eprintln!("tilo-session: first boot setup failed: {error}; starting anyway");
            }
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::from(2);
        }
    }
    let value = |flag: &str| {
        ours.iter()
            .position(|arg| arg == flag)
            .and_then(|index| ours.get(index.saturating_add(1)))
    };
    let backend = value("--backend").map_or("drm", String::as_str);
    let size = value("--size")
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));

    let bus = |flag: &str| match value(flag).map(String::as_str) {
        None => Bus::Default,
        Some("none") => Bus::None,
        Some(address) => Bus::Address(address.to_owned()),
    };
    let buses = Buses {
        session: bus("--session-bus"),
        system: bus("--system-bus"),
    };
    record_panics(backend);
    let wake = tilo_service::LoopWake::default();
    // The person's settings, read once here: the power policy they resolve is
    // part of them.
    let config = tilo_session::SessionConfig::load();
    // The machine's power: that policy, applied by the session's loop and
    // acted on over logind by the service started beside the others.
    let power = Some(tilo_session_power::PowerLink::new(config.power.clone()));
    let (mut services, _following) = services(&wake, power.clone());
    let (capture, captures) =
        tilo_session::capture::channel_to(tilo_session::capture::directory(), wake.handle());
    // The portal's side of the desktop, which the session answers.
    let (portal_side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = desktop_services(
        &buses,
        &wake,
        Arc::new(capture),
        &portal_side,
        &mut services,
        power.as_ref().map(|link| link.registry().clone()),
    );
    // Where input captures and the shared screens are is the portal's: its
    // capture broker tells them what the session's seat and outputs are.
    let portal = desktop
        .as_ref()
        .and_then(|desktop| desktop.portal())
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    tell_the_person(backend, desktop.as_deref());
    // The apps this session opens start from its own displays and the
    // person's variables, once the compositor has bound its socket.
    let launch = std::rc::Rc::new(tilo_session::mount::apps::Apps::new(
        tilo_app_launch::SessionEnvironment::from_process(),
        Some(wake.handle()),
    ));
    let options = Options {
        services,
        wake,
        setup: setup(host_desktop),
        captures: Some(captures),
        apps: Some(std::rc::Rc::clone(&launch) as std::rc::Rc<dyn tilo_session::AppActions>),
        launch: Some(std::rc::Rc::clone(&launch)),
        desktop,
        portal,
        power,
        ..device_options()
    };
    let started = match backend {
        "headless" => {
            Session::headless(options, &[OutputConfig::new("HEADLESS-1", size.0, size.1)])
        }
        "drm" => Session::drm(options, DisplayConfig::default()),
        other => {
            eprintln!("tilo-session: unknown backend {other} (drm, headless)");
            return ExitCode::FAILURE;
        }
    };
    let session = match started {
        Ok(session) => session,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    watch_the_loop(backend, &mut session);
    let session_bus = match &buses.session {
        Bus::Address(address) => Some(address.clone()),
        _ => None,
    };
    run(session, ours, command, Some(&launch), session_bus)
}

/// `session` running until `command` exits, its displays printed.
#[cfg(target_os = "linux")]
fn run(
    mut session: tilo_session::Session,
    ours: &[String],
    command: &[String],
    launch: Option<&std::rc::Rc<tilo_session::mount::apps::Apps>>,
    session_bus: Option<String>,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    use tilo_session::SessionEnvironment;

    let socket = match session.compositor().listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("WAYLAND_DISPLAY={socket}");
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
            session_bus,
        );
    }
    SessionEnvironment {
        wayland_display: socket.clone(),
        x11_display: x11_display.clone(),
        x11_authority: None,
    }
    .export();
    let mut child = command
        .split_first()
        .and_then(|(program, rest)| start_command(program, rest, &socket, x11_display.as_deref()));
    match session.run(|_| {
        child
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
fn record_panics(backend: &str) {
    if backend == "drm" {
        tilo_crash_records::install(tilo_crash_records::PANIC_TITLE);
    }
}

/// What this desktop knows of its own machine reaches the person through
/// their notifications. Only the session that owns the machine tells them: a
/// session in a window of another desktop, or a headless one, has that
/// machine's storage, updates and history to leave alone.
#[cfg(target_os = "linux")]
fn tell_the_person(backend: &str, desktop: Option<&tilo_session::desktop_services::DesktopServices>) {
    if backend == "drm"
        && let Some(desktop) = desktop
    {
        tilo_session::notices::start(desktop.notifications());
    }
}

/// A main loop that stops for good keeps the display frozen on its last frame
/// and the keyboard unread; the watchdog records why and ends the process, so
/// the person comes back to the login screen. Only the TTY backend owns the
/// display; a window or a headless run leaves nothing to hand back.
#[cfg(target_os = "linux")]
fn watch_the_loop(backend: &str, session: &mut tilo_session::Session) {
    if backend == "drm"
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
/// who unlocks it.
#[cfg(target_os = "linux")]
fn device_options() -> tilo_session::Options {
    use tilo_quick_settings::device::PlayerLink;
    use tilo_session::{DeviceHost, Options, QuickSettings, SessionConfig};
    Options {
        config: SessionConfig::load(),
        search: search_options(),
        quick_settings: Some(QuickSettings::Device(Box::new(|wake| {
            let device = DeviceHost::os(wake);
            match PlayerLink::helper() {
                Ok(players) => device.follow_players(players),
                Err(error) => eprintln!("tilo-session: no media helper: {error}"),
            }
            device
        }))),
        session_actions: os_actions::perform_session_action,
        lock_power: true,
        user: std::env::var("USER").unwrap_or_default(),
        verifier: tilo_session::unlock::installed(),
        state: tilo_paths::state_dir(),
        ..Options::default()
    }
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

/// `program` started with `rest`, on the session's displays.
#[cfg(target_os = "linux")]
fn start_command(
    program: &str,
    rest: &[String],
    wayland: &str,
    x11: Option<&str>,
) -> Option<std::process::Child> {
    let mut started = std::process::Command::new(program);
    started.args(rest).env("WAYLAND_DISPLAY", wayland);
    match x11 {
        Some(display) => started.env("DISPLAY", display),
        None => started.env_remove("DISPLAY"),
    };
    started
        .spawn()
        .map_err(|error| eprintln!("tilo-session: {program}: {error}"))
        .ok()
}

/// What keeps the services' readings following the device while it lives.
#[cfg(target_os = "linux")]
type Following = Vec<Box<dyn std::any::Any>>;

/// The system services the session hosts, on this device's own daemons,
/// and what keeps their readings following the device.
#[cfg(target_os = "linux")]
fn services(
    wake: &tilo_service::LoopWake,
    power: Option<tilo_session_power::PowerLink>,
) -> (Vec<Box<dyn tilo_session::Service>>, Following) {
    use std::sync::Arc;
    use tilo_connectivity_service::{Connectivity, device::OsDevice};

    let mut services: Vec<Box<dyn tilo_session::Service>> = Vec::new();
    let mut following: Following = Vec::new();
    services.push(Box::new(tilo_settings_service::SettingsService::new(
        &tilo_session::Options::default().person,
    )));
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
/// changes, what apps hold, the person's files, and the search sources they
/// switched off.
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
    }
}

/// The setup this device needs before its desktop is used: the first use
/// when it has no identity yet, setup it started and did not finish, the
/// recovery of its identity when startup found it unusable; `None` when
/// there is nothing left to set up.
#[cfg(target_os = "linux")]
fn setup(host_desktop: bool) -> Option<tilo_session::OnboardingView> {
    use tilo_onboarding_host::identity_provisioning::{
        DiskIdentityStore, IDENTITY_RECOVERY_ENV, needs_first_run_flow,
    };
    use tilo_session::OnboardingView;
    use tilo_session::mount::firstboot::setup_due;
    let store = match DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("tilo-session: no identity store: {error}");
            return None;
        }
    };
    let recovery = std::env::var_os(IDENTITY_RECOVERY_ENV).is_some();
    let identity_dir = store.path().parent()?.to_path_buf();
    if !setup_due(needs_first_run_flow(&store), recovery, &identity_dir) {
        return None;
    }
    Some(match (host_desktop, recovery) {
        (false, false) => OnboardingView::new(identity_dir),
        (false, true) => OnboardingView::new_for_recovery(identity_dir),
        (true, false) => OnboardingView::new_for_host_desktop(identity_dir),
        (true, true) => OnboardingView::new_for_host_desktop_recovery(identity_dir),
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {}
