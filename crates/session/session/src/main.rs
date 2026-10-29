//! The native session: `tilo-session [--backend drm|headless] [--size WxH]
//! [--host-desktop] [-- command args...]`. Listens on a fresh `wayland-N` socket, prints its
//! name and starts `command` with `WAYLAND_DISPLAY` set to it; with a
//! command, the session lasts as long as it does.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use tilo_compositor::{DisplayConfig, OutputConfig};
    use tilo_session::{Options, Session};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (ours, command) = match args.iter().position(|arg| arg == "--") {
        Some(split) => (
            args.get(..split).unwrap_or_default(),
            args.get(split.saturating_add(1)..).unwrap_or_default(),
        ),
        None => (args.as_slice(), &[][..]),
    };
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

    let options = Options {
        setup: setup(ours.iter().any(|arg| arg == "--host-desktop")),
        session_actions: os_actions::perform_session_action,
        ..Options::default()
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
    let mut session = match started {
        Ok(session) => session,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    let socket = match session.compositor().listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("WAYLAND_DISPLAY={socket}");
    let mut child = None;
    if let Some((program, rest)) = command.split_first() {
        match std::process::Command::new(program)
            .args(rest)
            .env("WAYLAND_DISPLAY", &socket)
            .spawn()
        {
            Ok(spawned) => child = Some(spawned),
            Err(error) => eprintln!("tilo-session: {program}: {error}"),
        }
    }
    let result = session.run(|_| {
        child
            .as_mut()
            .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-session: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The setup this device needs before its desktop is used: the first use
/// when it has no identity yet, the recovery of its identity when startup
/// found it unusable; `None` when the identity is ready.
#[cfg(target_os = "linux")]
fn setup(host_desktop: bool) -> Option<tilo_session::OnboardingView> {
    use tilo_onboarding_host::identity_provisioning::{
        DiskIdentityStore, IDENTITY_RECOVERY_ENV, needs_first_run_flow,
    };
    use tilo_session::OnboardingView;
    let store = match DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("tilo-session: no identity store: {error}");
            return None;
        }
    };
    let recovery = std::env::var_os(IDENTITY_RECOVERY_ENV).is_some();
    if !recovery && !needs_first_run_flow(&store) {
        return None;
    }
    let identity_dir = store.path().parent()?.to_path_buf();
    Some(match (host_desktop, recovery) {
        (false, false) => OnboardingView::new(identity_dir),
        (false, true) => OnboardingView::new_for_recovery(identity_dir),
        (true, false) => OnboardingView::new_for_host_desktop(identity_dir),
        (true, true) => OnboardingView::new_for_host_desktop_recovery(identity_dir),
    })
}

#[cfg(not(target_os = "linux"))]
fn main() {}
