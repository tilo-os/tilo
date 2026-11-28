//! The runtime executable's command line, shared by every host binary.

use crate::daemon;
use crate::profile;
use std::path::Path;
use std::{ffi::OsString, io};

#[allow(
    clippy::disallowed_methods,
    reason = "called from `run` before tracing or any other thread starts, so no other thread can read the environment concurrently; the profile variables must reach this process and every child it spawns"
)]
fn configure_profile(profile: &Path) -> io::Result<()> {
    for (name, path) in profile::prepare_environment(profile)? {
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

/// Prints this computer's device as JSON (its key, label and the people whose certificate names
/// it), as its running daemon answers it, and exits.
pub const DEVICE_INFO_ARG: &str = "--device-info";

/// Prints the tools a workspace's MCP server offers an agent, as JSON, and exits.
pub const TOOLS_JSON_ARG: &str = "--tools-json";

/// Prints what each workspace a state directory keeps holds where its types' readers are not,
/// and how many members it has: counts only, read without opening anything for writes or
/// repairing it. Takes the workspaces directory and the surface directory beside it.
pub const READERS_REPORT_ARG: &str = "--readers-report";

/// Runs this executable as the consent prompt shown before a confined app
/// first captures from a camera or microphone (macOS).
pub const CONSENT_HELPER_ARG: &str = "--device-consent";

/// The signed surface of an app this device installed whose address is `hash`, read from the
/// package store the device keeps. A package another process installed while this daemon ran is
/// one this daemon never named: every change of that app would wait for a surface no one brings.
fn installed_surface(hash: &str) -> Option<Vec<u8>> {
    let catalog = package_store::Catalog::from_env().ok()?;
    catalog
        .list()
        .ok()?
        .into_iter()
        .filter_map(|package| package_store::integrity::signed_surface(&package))
        .find(|surface| blake3::hash(surface).to_hex().as_str() == hash)
}

/// Keep `bytes`, an installed app's surface, as the content it is, so every later read of that
/// address in this process finds it where its changes are decided.
fn keep_content(content: &tilo_content::ContentStore, bytes: &[u8]) {
    if let Err(error) = content.put_public(bytes) {
        tracing::warn!(%error, "an installed app's surface could not be kept as content");
    }
}

/// Decide changes against the app surfaces in the content store this runtime's apps keep their
/// content in, the store of the apps this device installed, and those members sent: an app
/// another process of this device installed after the daemon started is one whose signed surface
/// this process was never handed, and its changes are decided by it. The program does this
/// before anything else; a test that runs the daemon as the program does too.
pub fn decide_against_installed_surfaces() {
    let content = tilo_content_device::for_app();
    tilo_app_host::decide_receipt_against_surfaces(move |_, hash| {
        content
            .read(hash)
            .or_else(|| installed_surface(hash).inspect(|bytes| keep_content(&content, bytes)))
            .or_else(|| tilo_workspace_service::surfaces::read(hash))
    });
}

fn run() -> io::Result<()> {
    decide_against_installed_surfaces();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The tools a hosted workspace offers an agent, without a workspace or a session:
    // `tools/docs/reference/mcp_tools.py` writes their reference from this answer, so the page
    // is the server's own list.
    if arguments
        .first()
        .is_some_and(|argument| argument == TOOLS_JSON_ARG)
    {
        println!("{}", tilo_mcp::tools_json());
        return Ok(());
    }
    // What each workspace a server's state directory keeps holds where its types' readers are
    // not: counts an operator reads with the daemon stopped, and nothing written.
    if arguments
        .first()
        .is_some_and(|argument| argument == READERS_REPORT_ARG)
    {
        let [_, workspaces, surfaces] = arguments.as_slice() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{READERS_REPORT_ARG} takes a workspaces directory and a surfaces directory"
                ),
            ));
        };
        let reports = tilo_workspace_service::readers_report::read_state(
            Path::new(workspaces),
            Path::new(surfaces),
        )
        .map_err(io::Error::other)?;
        for (name, report) in reports {
            for line in report.lines(&name) {
                println!("{line}");
            }
        }
        return Ok(());
    }
    // What this computer's device is, as its daemon answers it: a workspace VM's keeper reads it
    // to learn the device its daemon logged in as. Only the daemon reads the device's key.
    if arguments
        .first()
        .is_some_and(|argument| argument == DEVICE_INFO_ARG)
    {
        let info = tilo_device_identity::login::daemon::DeviceActions::here(
            tilo_api::calls::device_keys(),
        )
        .map_err(io::Error::other)?
        .ask("info", serde_json::json!({}))
        .map_err(|refusal| io::Error::other(format!("this computer's device: {refusal}")))?;
        println!("{info}");
        return Ok(());
    }
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
    let args = daemon::parse_args(arguments)?;
    if args.mode == daemon::Mode::Daemon {
        if args.standalone {
            // A profile named apart keeps the daemon's environment inside it.
            daemon::validate_profile(&args.profile)?;
            configure_profile(&args.profile)?;
        } else {
            // The person's own profile, made on the first start.
            tilo_files::native::private::directory(&args.profile)?;
        }
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    match args.mode {
        daemon::Mode::Daemon => daemon::serve(&args.profile, args.dev),
        daemon::Mode::Connect => daemon::connect(
            &args.profile,
            args.standalone,
            args.forward.as_ref(),
            args.workspace.as_deref(),
        ),
        daemon::Mode::RunApp {
            app_id,
            workspace,
            capture,
        } => daemon::run_app(
            &args.profile,
            &app_id,
            workspace.as_deref(),
            capture.as_deref(),
        ),
    }
}

/// Run the executable named `program`, serving the Automerge isolation workers that re-enter it
/// on their way. Call it from `main`, before starting any thread.
#[must_use]
pub fn host(program: &str) -> std::process::ExitCode {
    // Untrusted Automerge bytes are decoded in worker processes started from
    // this executable; each of them is served here and exits.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{program}: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    /// A change naming a surface a member sent is decided by it, as the workspace service keeps it.
    #[test]
    fn a_surface_a_member_sent_decides_the_changes_that_name_it() {
        let name = "a_surface_a_member_sent_decides_the_changes_that_name_it";
        if !tilo_test_env::in_own_process_here(module_path!(), name) {
            return;
        }
        let surface = br#"{"format":2,"resources":[{"fields":[{"path":"title","type":"string"}],"kind":"tilo.notes.note","label":"Note","verbs":[{"id":"type","kind":"undoable"}],"version":1}]}"#;
        let (kept, hash) = (tempfile::tempdir().unwrap(), blake3::hash(surface));
        std::fs::write(kept.path().join(hash.to_hex().as_str()), surface).unwrap();
        tilo_workspace_service::surfaces::keep_in(kept.path());
        super::decide_against_installed_surfaces();
        let (workspace, app, document) = ("w", "tilo.notes", "*/tilo.notes.note#0");
        let written = collab_types::WrittenDocument {
            workspace,
            app,
            document,
        };
        let surfaces = tilo_peer_state::default_surfaces();
        let found = surfaces
            .source()
            .and_then(|it| it.surface(&written, None, Some(hash.as_bytes())));
        assert!(found.is_some(), "decided by the surface a member sent");
    }
}
