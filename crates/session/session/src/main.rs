//! The native session: `tilo-session [--backend drm|headless] [--size WxH]
//! [-- command args...]`. Listens on a fresh `wayland-N` socket, prints its
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

    let options = Options::default();
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

#[cfg(not(target_os = "linux"))]
fn main() {}
