//! `tilo-app-keeper --dir <private dir> --launch <opaque> [--app-id-prefix <p>] [--x11] -- <argv...>`
//!
//! Keeps one app's Wayland windows open while the desktop restarts (crate docs). It binds
//! `<dir>/wayland-0` and `<dir>/control`, starts the app with `WAYLAND_DISPLAY` set to the
//! socket's absolute path once a desktop is attached, and exits when the app ended and a
//! session heard it (or none came within a minute; `<dir>/ended` says how it ended). With
//! `--x11` the app also gets an X11 display of its own as `DISPLAY`, whose server is a client of
//! the keeper.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let options = match tilo_app_keeper::Options::from_args(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            // The keeper's directory is not known yet, so this one line goes where the launcher
            // found the keeper: the launch it names never started.
            eprintln!("tilo-app-keeper: {error}");
            return ExitCode::from(2);
        }
    };
    // What the keeper says of itself goes to its own bounded log in its directory, never to its
    // streams: those carry the app's words alone, which the launcher passes on and the session
    // reads as the app's output.
    let log = tilo_app_keeper::prepare_dir(&options.dir).and_then(|()| {
        tilo_app_keeper::output::Log::create(&tilo_app_keeper::output::log_path(&options.dir))
    });
    let log = match log {
        Ok(log) => log,
        Err(error) => {
            eprintln!("tilo-app-keeper: no directory for its log: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || log.clone())
        .with_target(false)
        .log_internal_errors(false)
        .init();
    if let Err(error) = tilo_app_keeper::end_when_told() {
        tracing::error!(%error, "the keeper cannot be told to end");
        return ExitCode::FAILURE;
    }
    match tilo_app_keeper::Keeper::new(options).and_then(tilo_app_keeper::Keeper::run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "the keeper stopped");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
