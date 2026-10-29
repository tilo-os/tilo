//! The compositor on its own: `tilo-compositor [--backend
//! nested|drm|headless] [--size WxH] [--scale S] [-- command args...]`.
//! Listens on a fresh `wayland-N` socket, starts Xwayland, prints both and
//! starts `command` with `WAYLAND_DISPLAY` and `DISPLAY` set to them. Windows float on a plain
//! background; no shell surfaces. Inside a desktop (`DISPLAY` set) it runs in
//! a window by default, else on the TTY.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use tilo_compositor::{Compositor, DisplayConfig, NestedConfig, OutputConfig, StackPolicy};

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
    let inside_desktop = std::env::var_os("DISPLAY").is_some();
    let backend = value("--backend").map_or(
        if inside_desktop { "nested" } else { "drm" },
        String::as_str,
    );
    let size: (u16, u16) = value("--size")
        .and_then(|size| size.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((1280, 720));
    let scale = value("--scale")
        .and_then(|scale| scale.parse().ok())
        .unwrap_or(1.0);

    let started = match backend {
        "headless" => Compositor::headless(Box::new(StackPolicy)).and_then(|mut compositor| {
            compositor.add_output(
                &OutputConfig::new("HEADLESS-1", i32::from(size.0), i32::from(size.1))
                    .with_scale(scale),
            )?;
            Ok(compositor)
        }),
        "nested" => Compositor::nested(
            Box::new(StackPolicy),
            &NestedConfig {
                size,
                scale,
                ..NestedConfig::default()
            },
        ),
        "drm" => Compositor::drm(Box::new(StackPolicy), DisplayConfig::default()),
        other => {
            eprintln!("tilo-compositor: unknown backend {other} (nested, drm, headless)");
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
    // X11 clients through Xwayland, unless it is missing.
    let x_display = match compositor.start_xwayland() {
        Ok(number) => {
            println!("DISPLAY=:{number}");
            Some(format!(":{number}"))
        }
        Err(error) => {
            eprintln!("tilo-compositor: no Xwayland: {error}");
            None
        }
    };
    let mut child = None;
    if let Some((program, rest)) = command.split_first() {
        let mut command = std::process::Command::new(program);
        command.args(rest).env("WAYLAND_DISPLAY", &socket);
        match &x_display {
            Some(display) => command.env("DISPLAY", display),
            None => command.env_remove("DISPLAY"),
        };
        match command.spawn() {
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
