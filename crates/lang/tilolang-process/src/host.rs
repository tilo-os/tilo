//! `tilolang-process`: the process the session starts as a tilolang host, for one workspace and author.
//!
//! It holds its api (its process link, `TILO_SVC_PROCESS_FD`) and its storage
//! (`TILO_TILOLANG_PROCESS_STORAGE`) and nothing else. It waits on the API, its instances' windows,
//! its own wake and a stop (`stop`), and runs a frame of every instance when one of them has
//! something for it. It ends when the session closes its api and its last window is gone, or
//! when it is asked to stop: both drop the instances.
//!
//! Every wait ends on something its owner then takes out: the API's message is received, the
//! wake is drained ([`Wake::consume`]), the task's lines are served, a running window's input is
//! read by its instance's frame, and an opening window is waited on only for closing. An idle
//! host's wait does not end.
//!
//! Whatever ends it, the host says why on its api last (`FromProcess::Exiting`), and on its log.

use std::sync::{Arc, OnceLock};

use runtime_portal::process_link::{Descriptor, FromProcess, Packet as _, STORAGE_ENV, ToProcess};
use tilolang_process::TilolangProcess;
use tilolang_process::stop::Stop;
use tilolang_process::wait::{Wake, wait};

/// The API, for the panic hook to say why the host ends.
static API: OnceLock<Arc<Descriptor>> = OnceLock::new();

pub(super) fn main() -> std::process::ExitCode {
    // The engines this host answers its apps with, registered before any of them asks.
    tilolang_process::engines::register_all();
    tracing_subscriber::fmt()
        // RUST_LOG, with the launch timing lines kept, as the session's own log keeps them.
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref()),
        ))
        .with_writer(std::io::stderr)
        // The session reads the host's stderr into its log line by line: colour codes there are
        // noise a person reads around.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let Some(api) = runtime_portal::process_link::take() else {
        // No api to say it on: the session's log of the host's stderr is all there is.
        eprintln!("tilolang-process: exiting: no api: the session starts the tilolang host");
        return std::process::ExitCode::from(2);
    };
    let api = Arc::new(api);
    let _ = API.set(Arc::clone(&api));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if let Some(api) = API.get() {
            let _ = FromProcess::exiting(&format!("the host panicked: {info}")).send(api);
        }
    }));
    let (exit, api) = run(api);
    if exit.failed {
        tracing::error!(reason = exit.reason, "tilolang host exiting");
    } else {
        tracing::info!(reason = exit.reason, "tilolang host exiting");
    }
    if let Some(api) = api
        && let Err(error) = FromProcess::exiting(&exit.reason).send(&api)
    {
        tracing::warn!(%error, "the session was not told why the host exits");
    }
    if exit.failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// Why the host ends.
struct Exit {
    reason: String,
    failed: bool,
}

impl Exit {
    fn failed(reason: String) -> Self {
        Self {
            reason,
            failed: true,
        }
    }

    /// The end a stop asks for: the instances are dropped, and what they keep is closed with
    /// them, as when the API closes.
    fn stopped() -> Self {
        Self {
            reason: "it was asked to stop, and closed what it keeps".to_owned(),
            failed: false,
        }
    }
}

/// Keep this host's own compiled modules in the storage the session granted it: what a later start
/// of this host starts compiled.
fn keep_compiled() {
    let storage = std::env::var_os(STORAGE_ENV)
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(storage) = &storage
        && let Err(error) =
            tilolang_tilo::runtime::engine::keep_compiled_in(storage.join(".compiled"))
    {
        tracing::warn!(%error, "compiled tilolang modules are not kept");
    }
}

/// Run the host's frames until nothing changes: what a frame changed may ask for another (a view
/// that read what it changed), so it settles.
fn run_frames(host: &mut TilolangProcess) {
    let began = std::time::Instant::now();
    let mut frames = 1_u32;
    while host.turn() {
        frames = frames.saturating_add(1);
    }
    tracing::debug!(
        frames,
        frames_us = began.elapsed().as_micros(),
        "the host's frames settled"
    );
}

/// The stop the host's loop ends on; nothing when its signals cannot be taken, which leaves every
/// index it keeps to be built anew at its next start.
fn stop_watching() -> Option<Stop> {
    match Stop::install() {
        Ok(stop) => Some(stop),
        Err(error) => {
            tracing::warn!(
                %error,
                "the host does not stop on a signal: every index it keeps is built anew at its next start"
            );
            None
        }
    }
}

/// Run the host until it ends: why, and its api while it is still open.
fn run(api: Arc<Descriptor>) -> (Exit, Option<Arc<Descriptor>>) {
    let wake = match Wake::new() {
        Ok(wake) => wake,
        Err(error) => return (Exit::failed(format!("no wake: {error}")), Some(api)),
    };
    keep_compiled();
    // Jobs this host runs or starts are the device's: their moves go to it on the api.
    tilolang_process::engines::attach(Arc::clone(&api), wake.waker());
    let stop = stop_watching();
    // The platform's published resources (the device, Settings, the workspace's activity) are
    // read from the session's services, which this process's connection reaches.
    tilolang_tilo::platform_apps::install(Box::new(tilolang_tilo::platform_apps::Session));
    let mut host = TilolangProcess::new(wake.waker());
    let mut api = Some(api);
    // Why the API closed, when the session did not close it.
    let mut api_failed: Option<String> = None;
    loop {
        run_frames(&mut host);
        // What each instance plays, as the system's media controls show it: said when it moved.
        for (instance, view) in host.take_media() {
            if let Some(open) = api.as_ref()
                && let Err(error) = (FromProcess::Media { instance, view }).send(open)
            {
                tracing::warn!(%error, instance, "the session was not told what plays");
            }
        }
        // The session stops the host once it ran no instance for a while.
        for instance in host.take_ended() {
            if let Some(open) = api.as_ref()
                && let Err(error) = FromProcess::Ended(instance).send(open)
            {
                tracing::warn!(%error, instance, "the session was not told an instance ended");
            }
        }
        // A job the device holds this process for finishes first: its record must say how.
        if api.is_none() && host.is_empty() && !tilolang_process::engines::busy() {
            let exit = match api_failed {
                Some(reason) => Exit::failed(reason),
                None => Exit {
                    reason: "the session closed its api and no instance runs".to_owned(),
                    failed: false,
                },
            };
            return (exit, None);
        }
        // Until something arrives, or until the time a view read moves on (its midnight).
        let timeout_ms = host.wakes_in().map_or(-1, |wait| {
            i32::try_from(wait.as_millis()).unwrap_or(i32::MAX)
        });
        let ready = match wait(api.as_deref(), &wake, stop.as_ref(), &host, timeout_ms) {
            Ok(ready) => ready,
            Err(error) => return (Exit::failed(format!("its wait failed: {error}")), api),
        };
        tracing::debug!(?ready, "the host's wait ended");
        if ready.wake
            && let Err(error) = wake.consume()
        {
            return (Exit::failed(format!("its wake failed: {error}")), api);
        }
        if ready.stop || stop.as_ref().is_some_and(Stop::asked) {
            return (Exit::stopped(), api);
        }
        if ready.task {
            host.serve_task();
        }
        if ready.api
            && let Some(open) = api.as_ref()
        {
            match ToProcess::receive(open) {
                Ok(Some(ToProcess::Finish) | None) => api = None,
                Ok(Some(message)) => take(&mut host, open, message),
                // The packet was taken: the API carries the next one.
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::error!(%error, "the API carried something unreadable");
                }
                // The API itself failed: it carries nothing more, and stays ready, so it is
                // closed here; the host ends once its instances have.
                Err(error) => {
                    tracing::error!(%error, "the API failed");
                    api_failed = Some(format!("its api failed: {error}"));
                    api = None;
                }
            }
        }
    }
}

/// Take one message of the session's: an instance or task to run, or what changed for those
/// running.
fn take(host: &mut TilolangProcess, open: &Descriptor, message: ToProcess) {
    match message {
        ToProcess::Instance { request, fds } => {
            let (app, instance) = (request.app.clone(), request.instance);
            if let Err(error) = host.admit(*request, fds) {
                tracing::error!(app, %error, "an instance did not start");
                if let Err(error) = FromProcess::Ended(instance).send(open) {
                    tracing::warn!(%error, instance, "the session was not told an instance ended");
                }
            }
        }
        ToProcess::Locale(locale) => {
            // The person set another clock, date style or language: every instance
            // writes its times and its words again, and its window shows them.
            if host.set_locale(&locale) {
                tracing::info!("the person's formats or language changed");
            }
        }
        ToProcess::Access(access) => {
            // A role was given or taken back: the app's windows offer what the person
            // may do now.
            if host.set_access(&access.app, access.access.as_ref()) {
                tracing::info!(app = access.app, "what the person may do changed");
            }
        }
        ToProcess::Build { build, module } => {
            // A development session took a later build of the app: its windows show it.
            if let Err(error) = host.take_build(&build, &module) {
                tracing::error!(app = build.app, %error, "a later build was not taken");
            }
        }
        ToProcess::MediaCommand { instance, command } => {
            host.media_command(instance, command);
        }
        ToProcess::Task { request, fds } => {
            if let Err(error) = host.begin_task(&request, fds) {
                tracing::error!(task = request.task, %error, "a task did not start");
            }
        }
        // The call that started the job was cancelled, or its app stopped it: its engine stops
        // it, and the job reads cancelled over the link.
        ToProcess::CancelJob { job } => tilolang_process::engines::stop(&job),
        ToProcess::JobsUnder(under) => tilolang_process::engines::jobs_under(under),
        ToProcess::Engine(job) => tilolang_process::engines::run(&job),
        ToProcess::Finish => {}
        other @ (ToProcess::Window { .. } | ToProcess::Patch { .. }) => {
            tracing::error!(
                ?other,
                "the API asked for a window, which a tilolang process has none of"
            );
        }
    }
}
