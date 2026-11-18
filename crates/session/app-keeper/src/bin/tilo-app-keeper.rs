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
    use std::io::Write as _;
    use std::process::ExitCode;

    // The keeper's standard error is a pipe the launcher relays, which may close or fill with
    // the session that read it; a log line that cannot be written is dropped, never a panic
    // (the subscriber's own report of a failed write would itself fail).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .log_internal_errors(false)
        .init();
    let options = match tilo_app_keeper::Options::from_args(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("tilo-app-keeper: {error}");
            return ExitCode::from(2);
        }
    };
    match tilo_app_keeper::Keeper::new(options).and_then(tilo_app_keeper::Keeper::run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "tilo-app-keeper: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
