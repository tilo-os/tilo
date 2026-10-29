//! The compositor on its own: `tilo-compositor [--backend drm|headless]
//! [--size WxH] [-- command args...]`. Listens on a fresh `wayland-N`
//! socket, prints its name and starts `command` with `WAYLAND_DISPLAY` set to
//! it. Windows float on a plain background; no shell surfaces.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use tilo_compositor::{Compositor, DisplayConfig, OutputConfig, StackPolicy};

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

    let started = match backend {
        "headless" => Compositor::headless(Box::new(StackPolicy)).and_then(|mut compositor| {
            compositor.add_output(&OutputConfig::new("HEADLESS-1", size.0, size.1))?;
            Ok(compositor)
        }),
        "drm" => Compositor::drm(Box::new(StackPolicy), DisplayConfig::default()),
        other => {
            eprintln!("tilo-compositor: unknown backend {other} (drm, headless)");
            return ExitCode::FAILURE;
        }
    };
    let mut compositor = match started {
        Ok(compositor) => compositor,
        Err(error) => {
            eprintln!("tilo-compositor: {error}");
            return ExitCode::FAILURE;
        }
    };
    let socket = match compositor.listen() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("tilo-compositor: {error}");
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
            Err(error) => eprintln!("tilo-compositor: {program}: {error}"),
        }
    }
    // With a command, the session lasts as long as it does.
    let result = compositor.run(|_| {
        child
            .as_mut()
            .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-compositor: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
