//! Noticing that a downloaded program is running, and keeping its entry in
//! the person's apps.
//!
//! A downloaded `AppImage`, tarball program or loose binary has no desktop
//! entry, so its windows arrive as foreign toplevels nothing correlates.
//! When such a window names itself (its first app id), the compositor hands
//! the client's process here ([`observe_window`]); everything after that runs
//! on this module's own worker, never on the compositor's loop: reading
//! `/proc`, deciding whether the file is a download
//! ([`desktop_entry::downloaded::classify`]), reading an `AppImage`'s image or
//! the program's folder for its name and icon, and writing the entry.
//!
//! What it does with what it found:
//! - an app that already has an entry here, now run from another file (a new
//!   version, a second download), gets its entry pointed at that file, so it
//!   never has two entries;
//! - an app with no entry becomes a [`Candidate`] for its window. Adding it
//!   is the person's choice ([`add_candidate`]); nothing here adds an app by
//!   itself.
//!
//! The entries are ordinary desktop entries in the applications model
//! ([`desktop_entry::downloaded::Store`]), so search, the launcher and
//! `tilo app list` see them like any other host application, and they start
//! with the same confinement any host program has.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use desktop_entry::downloaded::{self, Icon, Identity, Program, Store, WindowFacts};
use tilo_lock::MutexRecover as _;

/// A window of an unconfined host process that has just named itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedWindow {
    pub surface_id: u64,
    pub pid: u32,
    /// The process's kernel start time when it connected, so a reused pid is
    /// never mistaken for it.
    pub pid_start_time: Option<u64>,
    pub app_id: String,
}

/// A downloaded program running with no entry: what adding it would add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub program: Program,
    pub identity: Identity,
    pub app_id: String,
}

enum Job {
    Observe(ObservedWindow),
    Add(u64, mpsc::Sender<Result<String, String>>),
    Locate(String, PathBuf),
    Settle(mpsc::Sender<()>),
}

fn worker() -> &'static Mutex<mpsc::Sender<Job>> {
    static WORKER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (send, jobs) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("tilo-downloaded-apps".into())
            .spawn(move || {
                let mut seen: HashSet<(u32, Option<u64>)> = HashSet::new();
                while let Ok(job) = jobs.recv() {
                    run(job, &mut seen);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "downloaded programs will not be noticed: no worker");
        }
        Mutex::new(send)
    })
}

fn send(job: Job) {
    let _ = worker().lock_recover().send(job);
}

fn candidates() -> &'static Mutex<HashMap<u64, Candidate>> {
    static CANDIDATES: OnceLock<Mutex<HashMap<u64, Candidate>>> = OnceLock::new();
    CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A host window named itself. Called on the compositor's loop: it only
/// queues the window for the worker.
pub fn observe_window(window: ObservedWindow) {
    if window.app_id.trim().is_empty() || window.app_id.starts_with("anon-") {
        return;
    }
    send(Job::Observe(window));
}

/// The window was destroyed: its candidate goes with it.
pub fn forget_window(surface_id: u64) {
    candidates().lock_recover().remove(&surface_id);
}

/// The downloaded program behind window `surface_id`, when it has no entry.
pub fn candidate(surface_id: u64) -> Option<Candidate> {
    candidates().lock_recover().get(&surface_id).cloned()
}

/// Add the program behind window `surface_id` to the app list, and wait
/// up to `wait` for the entry to be written. Returns the entry's desktop
/// id.
///
/// # Errors
///
/// The window has no candidate, or the entry could not be written in time.
pub fn add_candidate(surface_id: u64, wait: Duration) -> Result<String, String> {
    let (reply, answer) = mpsc::channel();
    send(Job::Add(surface_id, reply));
    answer
        .recv_timeout(wait)
        .map_err(|_| "the downloaded-apps worker did not answer".to_owned())?
}

/// Point the entry `desktop_id` at `program`, on the worker.
pub fn locate(desktop_id: &str, program: &Path) {
    send(Job::Locate(desktop_id.to_owned(), program.to_path_buf()));
}

/// Wait until every job queued before this call has run. For tests and for
/// a caller that must see its own change.
pub fn settle(wait: Duration) -> bool {
    let (reply, done) = mpsc::channel();
    send(Job::Settle(reply));
    done.recv_timeout(wait).is_ok()
}

fn run(job: Job, seen: &mut HashSet<(u32, Option<u64>)>) {
    match job {
        Job::Observe(window) => {
            // One look per process: a program opening ten windows is one
            // program.
            if seen.insert((window.pid, window.pid_start_time)) {
                observe(&window);
            }
        }
        Job::Add(surface_id, reply) => {
            let _ = reply.send(add(surface_id));
        }
        Job::Locate(desktop_id, program) => {
            if let Some(store) = Store::for_session()
                && let Err(error) = store.locate(&desktop_id, &program)
            {
                tracing::warn!(%desktop_id, %error, "could not point a downloaded app's entry at its file");
            }
            refresh_scan();
        }
        Job::Settle(reply) => {
            let _ = reply.send(());
        }
    }
}

/// Re-read the application directories now, on this worker, so the entry
/// just written is in the next query rather than after the watch fires.
fn refresh_scan() {
    desktop_entry::cache::refresh();
}

fn observe(window: &ObservedWindow) {
    #[cfg(target_os = "linux")]
    {
        if let Some(expected) = window.pid_start_time
            && crate::shell::launcher::proc_stat(window.pid).map(|(_, start)| start)
                != Some(expected)
        {
            return;
        }
        let Some(facts) = downloaded::process_facts(window.pid) else {
            return;
        };
        let Some(home) = tilo_paths::home() else {
            return;
        };
        let Some(program) = downloaded::classify(&facts, &home) else {
            return;
        };
        // An app that has an entry of its own (a package, a hand-written
        // entry naming this window) is already in the list.
        let installed = crate::shell::host_apps::host_apps().into_iter().any(|app| {
            app.downloaded.is_none()
                && app
                    .window_identities
                    .iter()
                    .any(|identity| identity.eq_ignore_ascii_case(&window.app_id))
        });
        if installed {
            return;
        }
        let identity = downloaded::identify(
            &program,
            &WindowFacts {
                app_id: window.app_id.clone(),
                icon: window_icon(window.surface_id),
            },
        );
        let Some(store) = Store::for_session() else {
            return;
        };
        match store.update_location(&identity, &program) {
            Ok(Some(desktop_id)) => {
                tracing::info!(%desktop_id, program = %program.path.display(), "a downloaded app now runs from another file");
                refresh_scan();
            }
            Ok(None) if store.lookup(&identity.key).is_some() => {}
            Ok(None) => {
                candidates().lock_recover().insert(
                    window.surface_id,
                    Candidate {
                        program,
                        identity,
                        app_id: window.app_id.clone(),
                    },
                );
            }
            Err(error) => {
                tracing::warn!(%error, "could not update a downloaded app's entry");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = window;
}

fn add(surface_id: u64) -> Result<String, String> {
    let mut candidate = candidate(surface_id).ok_or_else(|| "no downloaded program".to_owned())?;
    // The window may have set its icon after it named itself.
    if candidate.identity.icon.is_none() {
        candidate.identity.icon = window_icon(surface_id);
    }
    let store = Store::for_session().ok_or_else(|| "no place for the person's apps".to_owned())?;
    let desktop_id = store
        .add(&candidate.identity, &candidate.program)
        .map_err(|error| error.to_string())?;
    candidates().lock_recover().remove(&surface_id);
    refresh_scan();
    Ok(desktop_id)
}

/// The icon window `surface_id` set for itself, as a file's bytes: its
/// largest image as a PNG, or the theme icon it named.
fn window_icon(surface_id: u64) -> Option<Icon> {
    #[cfg(target_os = "linux")]
    {
        use crate::compositor::toplevel_icon::{ToplevelIcon, current};
        match current(surface_id)? {
            ToplevelIcon::Named(name) => {
                let file = desktop_entry::icon_theme::lookup(&name, 256, 1)?;
                Icon::sniff(std::fs::read(file.path).ok()?)
            }
            ToplevelIcon::Pixels(images) => {
                let largest = images.iter().max_by_key(|image| image.width)?;
                png(largest)
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = surface_id;
        None
    }
}

/// Premultiplied RGBA pixels as a straight-alpha PNG.
fn png(image: &vello::peniko::ImageData) -> Option<Icon> {
    let mut rgba = image.data.data().to_vec();
    if image.alpha_type == vello::peniko::ImageAlphaType::AlphaPremultiplied {
        for pixel in rgba.chunks_exact_mut(4) {
            let alpha = u16::from(*pixel.get(3)?);
            if alpha == 0 {
                continue;
            }
            for channel in pixel.iter_mut().take(3) {
                let straight = u16::from(*channel)
                    .saturating_mul(255)
                    .checked_div(alpha)
                    .unwrap_or(0);
                *channel = u8::try_from(straight.min(255)).unwrap_or(u8::MAX);
            }
        }
    }
    let buffer = image::RgbaImage::from_raw(image.width, image.height, rgba)?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    buffer.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Icon::sniff(bytes.into_inner())
}
