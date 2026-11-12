//! `tilolang-host`: the process the session starts as a tilolang host, for one workspace and author.
//!
//! It holds its door (`TILO_SVC_TILOLANG_HOST_FD`) and its storage
//! (`TILO_TILOLANG_HOST_STORAGE`) and nothing else. It waits on the door, its instances' windows,
//! its own wake and a stop (`stop`), and runs a frame of every instance when one of them has
//! something for it. It ends when the session closes its door and its last window is gone, or
//! when it is asked to stop: both drop the instances, and every index they keep closes with them
//! rather than being left for the next start to build anew.
//!
//! Every wait ends on something its owner then takes out: the door's message is received, the
//! wake is drained ([`Wake::consume`]), the task's lines are served, a running window's input is
//! read by its instance's frame, and an opening window is waited on only for closing. An idle
//! host's wait does not end.
//!
//! Whatever ends it, the host says why on its door last ([`send_exiting`]), and on its log.

use std::os::fd::AsFd as _;
use std::sync::{Arc, OnceLock};

use runtime_portal::tilolang_host::{
    Descriptor, Handed, STORAGE_ENV, receive, send_ended, send_exiting, take_door,
};
use tilolang_host::TilolangHost;
use tilolang_host::stop::Stop;
use tilolang_host::wait::{Wake, wait};

/// The door, for the panic hook to say why the host ends.
static DOOR: OnceLock<Arc<Descriptor>> = OnceLock::new();

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        // RUST_LOG, with the launch timing lines kept, as the session's own log keeps them.
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref()),
        ))
        .with_writer(std::io::stderr)
        .init();
    let Some(door) = take_door() else {
        // No door to say it on: the session's log of the host's stderr is all there is.
        eprintln!("tilolang-host: exiting: no door: the session starts the tilolang host");
        return std::process::ExitCode::from(2);
    };
    let door = Arc::new(door);
    let _ = DOOR.set(Arc::clone(&door));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if let Some(door) = DOOR.get() {
            let _ = send_exiting(door, &format!("the host panicked: {info}"));
        }
    }));
    let (exit, door) = run(door);
    if exit.failed {
        tracing::error!(reason = exit.reason, "tilolang host exiting");
    } else {
        tracing::info!(reason = exit.reason, "tilolang host exiting");
    }
    if let Some(door) = door
        && let Err(error) = send_exiting(&door, &exit.reason)
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
    /// them, as when the door closes.
    fn stopped() -> Self {
        Self {
            reason: "it was asked to stop, and closed what it keeps".to_owned(),
            failed: false,
        }
    }
}

/// The storage the session granted this host, its own compiled modules kept in it: what a later
/// start of this host starts compiled. A package's directory is not one: those never start with a
/// dot.
fn granted_storage() -> Option<std::path::PathBuf> {
    let storage = std::env::var_os(STORAGE_ENV)
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(storage) = &storage
        && let Err(error) =
            tilolang_tilo::runtime::engine::keep_compiled_in(storage.join(".compiled"))
    {
        tracing::warn!(%error, "compiled tilolang modules are not kept");
    }
    storage
}

/// Run the host's frames until nothing changes: what a frame changed may ask for another (a view
/// that read what it changed), so it settles.
fn run_frames(host: &mut TilolangHost) {
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

/// Run the host until it ends: why, and its door while it is still open.
fn run(door: Arc<Descriptor>) -> (Exit, Option<Arc<Descriptor>>) {
    let wake = match Wake::new() {
        Ok(wake) => wake,
        Err(error) => return (Exit::failed(format!("no wake: {error}")), Some(door)),
    };
    let storage = granted_storage();
    let stop = stop_watching();
    let mut host = TilolangHost::new(storage, wake.waker());
    let mut door = Some(door);
    // Why the door closed, when the session did not close it.
    let mut door_failed: Option<String> = None;
    loop {
        run_frames(&mut host);
        // The session stops the host once its last instance ended.
        for instance in host.take_ended() {
            if let Some(open) = door.as_ref()
                && let Err(error) = send_ended(open, instance)
            {
                tracing::warn!(%error, instance, "the session was not told an instance ended");
            }
        }
        if door.is_none() && host.is_empty() {
            let exit = match door_failed {
                Some(reason) => Exit::failed(reason),
                None => Exit {
                    reason: "the session closed its door and no instance runs".to_owned(),
                    failed: false,
                },
            };
            return (exit, None);
        }
        // Until something arrives, or until the time a view read moves on (its midnight).
        let timeout_ms = host.wakes_in().map_or(-1, |wait| {
            i32::try_from(wait.as_millis()).unwrap_or(i32::MAX)
        });
        let ready = match wait(
            door.as_ref().map(|door| door.as_fd()),
            wake.fd(),
            stop.as_ref().map(Stop::fd),
            &host,
            timeout_ms,
        ) {
            Ok(ready) => ready,
            Err(error) => return (Exit::failed(format!("its wait failed: {error}")), door),
        };
        tracing::debug!(?ready, "the host's wait ended");
        if ready.wake
            && let Err(error) = wake.consume()
        {
            return (Exit::failed(format!("its wake failed: {error}")), door);
        }
        if ready.stop || stop.as_ref().is_some_and(Stop::asked) {
            return (Exit::stopped(), door);
        }
        if ready.task {
            host.serve_task();
        }
        if ready.door
            && let Some(open) = door.as_ref()
        {
            match receive(open) {
                Ok(Some(Handed::Instance(request, fds))) => {
                    let (app, instance) = (request.app.clone(), request.instance);
                    if let Err(error) = host.admit(request, fds) {
                        tracing::error!(app, %error, "an instance did not start");
                        if let Err(error) = send_ended(open, instance) {
                            tracing::warn!(%error, instance, "the session was not told an instance ended");
                        }
                    }
                }
                Ok(Some(Handed::Locale(locale))) => {
                    // The person set another clock, date style or language: every instance
                    // writes its times and its words again, and its window shows them.
                    if host.set_locale(&locale) {
                        tracing::info!("the person's formats or language changed");
                    }
                }
                Ok(Some(Handed::Task(request, fds))) => {
                    if let Err(error) = host.begin_task(&request, fds) {
                        tracing::error!(task = request.task, %error, "a task did not start");
                    }
                }
                Ok(None) => door = None,
                // The packet was taken: the door carries the next one.
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::error!(%error, "the door carried something unreadable");
                }
                // The door itself failed: it carries nothing more, and stays ready, so it is
                // closed here; the host ends once its instances have.
                Err(error) => {
                    tracing::error!(%error, "the door failed");
                    door_failed = Some(format!("its door failed: {error}"));
                    door = None;
                }
            }
        }
    }
}
