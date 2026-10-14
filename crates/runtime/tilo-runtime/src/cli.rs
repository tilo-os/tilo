//! The runtime executable's command line, shared by every host binary.

use crate::{RuntimeServices, daemon, mcp_process};
use std::{ffi::OsString, io, path::Path};

#[allow(
    clippy::disallowed_methods,
    reason = "called from `run` before tracing or any other thread starts, so no other thread can read the environment concurrently; the profile variables must reach this process and every child it spawns"
)]
fn configure_profile(profile: &Path, admission: bool) -> io::Result<()> {
    let paths = if admission {
        mcp_process::prepare_admission_profile_environment(profile)?
    } else {
        mcp_process::prepare_profile_environment(profile)?
    };
    for (name, path) in paths {
        // SAFETY: this executable has not started any threads. Reusable host
        // code never mutates the environment; applications receive Command env.
        unsafe { std::env::set_var(name, path) };
    }
    Ok(())
}

/// Runs this executable as the file chooser a confined app's request is
/// shown in (macOS): a process of its own, since panels need a main-thread
/// run loop the daemon does not have.
pub const CHOOSER_HELPER_ARG: &str = "--file-chooser";

/// Runs this executable as the consent prompt shown before a confined app
/// first captures from a camera or microphone (macOS).
pub const CONSENT_HELPER_ARG: &str = "--device-consent";

fn run<S: RuntimeServices>() -> io::Result<()> {
    // The app surfaces changes are decided against live in the content store
    // the apps this runtime hosts keep their content in.
    let content = tilo_content::ContentStore::for_app();
    tilo_action_broker::decide_receipt_against_surfaces(move |_, hash| content.read(hash));
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    #[cfg(target_os = "macos")]
    if arguments
        .first()
        .is_some_and(|argument| argument == CHOOSER_HELPER_ARG)
    {
        return tilo_dialogs::confined::run_chooser_helper();
    }
    #[cfg(target_os = "macos")]
    if arguments
        .first()
        .is_some_and(|argument| argument == CONSENT_HELPER_ARG)
    {
        return tilo_dialogs::confined::run_consent_helper();
    }
    // Legacy manifest invocation remains an independently owned stdio session.
    if arguments
        .iter()
        .take_while(|argument| *argument != "--" && *argument != "--run-app")
        .any(|argument| argument == "--apps")
    {
        let args = mcp_process::parse_args(arguments)?;
        configure_profile(&args.profile, false)?;
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .try_init();
        return mcp_process::run::<S>(&args.profile, &args.apps);
    }
    let args = daemon::parse_args(arguments)?;
    if matches!(
        args.mode,
        daemon::Mode::Daemon | daemon::Mode::AdmissionDaemon { .. }
    ) {
        daemon::validate_profile(&args.profile)?;
        configure_profile(
            &args.profile,
            matches!(args.mode, daemon::Mode::AdmissionDaemon { .. }),
        )?;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    match args.mode {
        daemon::Mode::Daemon => daemon::serve::<S>(&args.profile, args.dev),
        daemon::Mode::AdmissionDaemon { owner_uid } => {
            daemon::serve_admission::<S>(&args.profile, owner_uid)
        }
        daemon::Mode::Connect => daemon::connect(&args.profile, args.standalone),
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
