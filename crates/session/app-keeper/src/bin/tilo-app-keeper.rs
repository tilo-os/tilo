//! `tilo-app-keeper --dir <private dir> --launch <opaque> [--app-id-prefix <p>] -- <argv...>`
//!
//! Keeps one app's Wayland windows open while the desktop restarts (crate docs). It binds
//! `<dir>/wayland-0` and `<dir>/control`, starts the app with `WAYLAND_DISPLAY` set to the
//! socket's absolute path once a desktop is attached, and exits when the app ended and a
//! session heard it (or none came within a minute; `<dir>/ended` says how it ended).

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::ffi::OsString;
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
    let mut args = std::env::args_os().skip(1);
    let mut dir = None;
    let mut launch = None;
    let mut prefix = None;
    let mut command: Vec<OsString> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--dir") => dir = args.next(),
            Some("--launch") => launch = args.next().and_then(|value| value.into_string().ok()),
            Some("--app-id-prefix") => {
                prefix = args.next().and_then(|value| value.into_string().ok());
            }
            Some("--") => {
                command.extend(args.by_ref());
            }
            _ => {
                eprintln!(
                    "tilo-app-keeper: unexpected argument {}",
                    arg.to_string_lossy()
                );
                return ExitCode::from(2);
            }
        }
    }
    let (Some(dir), Some(launch)) = (dir, launch) else {
        eprintln!(
            "usage: tilo-app-keeper --dir <private dir> --launch <name> [--app-id-prefix <prefix>] -- <command...>"
        );
        return ExitCode::from(2);
    };
    if command.is_empty() {
        eprintln!("tilo-app-keeper: no command after --");
        return ExitCode::from(2);
    }
    let mut options = tilo_app_keeper::Options::new(dir, launch);
    options.app_id_prefix = prefix;
    options.command = command;
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
