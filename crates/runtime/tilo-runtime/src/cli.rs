//! The runtime executable's command line, shared by every host binary.

use crate::{RuntimeServices, daemon, mcp_process};
use std::{ffi::OsString, io, path::Path};

#[allow(
    clippy::disallowed_methods,
    reason = "called from `run` before tracing or any other thread starts, so no other thread can read the environment concurrently; the profile variables must reach this process and every child it spawns"
)]
fn configure_profile(profile: &Path) -> io::Result<()> {
    for (name, path) in mcp_process::prepare_profile_environment(profile)? {
        // SAFETY: this executable has not started any threads. Reusable host
        // code never mutates the environment; applications receive Command env.
        unsafe { std::env::set_var(name, path) };
    }
    Ok(())
}

fn run<S: RuntimeServices>() -> io::Result<()> {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Legacy manifest invocation remains an independently owned stdio session.
    if arguments
        .iter()
        .take_while(|argument| *argument != "--" && *argument != "--run-app")
        .any(|argument| argument == "--apps")
    {
        let args = mcp_process::parse_args(arguments)?;
        configure_profile(&args.profile)?;
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        return mcp_process::run::<S>(&args.profile, &args.apps);
    }
    let args = daemon::parse_args(arguments)?;
    if args.mode == daemon::Mode::Daemon {
        daemon::validate_profile(&args.profile)?;
        configure_profile(&args.profile)?;
    }
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
    match args.mode {
        daemon::Mode::Daemon => daemon::serve::<S>(&args.profile, args.dev),
        daemon::Mode::Connect => daemon::connect(&args.profile),
        daemon::Mode::Join { invitation } => daemon::join(&args.profile, &invitation),
        daemon::Mode::RunApp {
            app_id,
            room,
            executable,
            args: app_args,
        } => daemon::run_app(
            &args.profile,
            &app_id,
            room.as_deref(),
            &executable,
            &app_args,
        ),
    }
}

/// Run the executable named `program` over the services `S` hosts. Call it
/// from `main` before starting any thread.
#[must_use]
pub fn main<S: RuntimeServices>(program: &str) -> std::process::ExitCode {
    match run::<S>() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{program}: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
