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
//! - an app with no entry becomes a [`Candidate`] for its window, and the
//!   person is asked once whether to add it ([`take_ask`], shown by
//!   [`crate::shell::add_app_sheet`]). Their answer ([`answer`]) adds it,
//!   moves an `AppImage` into their Applications folder first when they
//!   chose that, or remembers that this file is not to be asked about
//!   again. Nothing here adds an app by itself.
//!
//! The entries are ordinary desktop entries in the applications model
//! ([`desktop_entry::downloaded::Store`]), so search, the launcher and
//! `tilo app list` see them like any other host application, and they start
//! with the same confinement any host program has.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use desktop_entry::downloaded::{self, Icon, Identity, Program, ProgramKind, Store, WindowFacts};

use crate::shell::add_app_sheet::{Answer, Ask};
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
    /// Add the window's program; `true` to move an `AppImage` into the
    /// person's Applications folder first.
    Add(u64, bool, mpsc::Sender<Result<String, String>>),
    /// Don't add the window's program, and don't ask about its file again.
    Decline(u64),
    Locate(String, PathBuf),
    /// The entry's file is gone from where it was: find where its record
    /// moved to in the person's files, else start another copy of the app.
    Follow(String, Option<[u8; 16]>),
    /// [`Job::Follow`] every entry of the current scan whose file is gone.
    FollowAll,
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

/// The window was destroyed: its candidate goes with it, unless the person
/// is being asked about it, whose answer still counts.
pub fn forget_window(surface_id: u64) {
    let asking = asks()
        .lock_recover()
        .iter()
        .any(|ask| ask.surface_id == surface_id)
        || ON_SCREEN.lock_recover().contains(&surface_id);
    if !asking {
        candidates().lock_recover().remove(&surface_id);
    }
}

fn asks() -> &'static Mutex<VecDeque<Ask>> {
    static ASKS: OnceLock<Mutex<VecDeque<Ask>>> = OnceLock::new();
    ASKS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Windows whose question is on screen.
static ON_SCREEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());

static WAKE: Mutex<Option<crate::session_runtime::Wake>> = Mutex::new(None);

/// How a question that is ready wakes the compositor's loop to show it.
pub(crate) fn set_wake(wake: crate::session_runtime::Wake) {
    *WAKE.lock_recover() = Some(wake);
}

fn wake() {
    let wake = WAKE.lock_recover().clone();
    if let Some(wake) = wake {
        wake();
    }
}

/// The next question to ask, once: the caller shows it and passes the
/// answer to [`answer`].
pub fn take_ask() -> Option<Ask> {
    let ask = asks().lock_recover().pop_front()?;
    ON_SCREEN.lock_recover().push(ask.surface_id);
    Some(ask)
}

/// The person answered the question about window `surface_id`'s program.
pub fn answer(answer: &Answer) {
    let (Answer::Add { surface_id, .. } | Answer::Declined { surface_id }) = *answer;
    ON_SCREEN.lock_recover().retain(|id| *id != surface_id);
    match answer {
        Answer::Add {
            move_to_applications,
            ..
        } => {
            let (reply, _) = mpsc::channel();
            send(Job::Add(surface_id, *move_to_applications, reply));
        }
        Answer::Declined { .. } => send(Job::Decline(surface_id)),
    }
}

/// The downloaded program behind window `surface_id`, when it has no entry.
pub fn candidate(surface_id: u64) -> Option<Candidate> {
    candidates().lock_recover().get(&surface_id).cloned()
}

/// Every running downloaded program with no entry, by its window.
pub fn candidates_snapshot() -> Vec<(u64, Candidate)> {
    let mut all: Vec<(u64, Candidate)> = candidates()
        .lock_recover()
        .iter()
        .map(|(surface_id, candidate)| (*surface_id, candidate.clone()))
        .collect();
    all.sort_by_key(|(surface_id, _)| *surface_id);
    all
}

/// Add the program behind window `surface_id` to the person's apps, and
/// wait up to `wait` for the entry to be written. Returns the entry's
/// desktop id. `move_to_applications` moves an `AppImage` into the
/// person's Applications folder first; any other program stays where it
/// is.
///
/// # Errors
///
/// The window has no candidate, or the entry could not be written in time.
pub fn add_candidate(
    surface_id: u64,
    move_to_applications: bool,
    wait: Duration,
) -> Result<String, String> {
    let (reply, answer) = mpsc::channel();
    send(Job::Add(surface_id, move_to_applications, reply));
    answer
        .recv_timeout(wait)
        .map_err(|_| "the downloaded-apps worker did not answer".to_owned())?
}

/// Point the entry `desktop_id` at `program`, on the worker.
pub fn locate(desktop_id: &str, program: &Path) {
    send(Job::Locate(desktop_id.to_owned(), program.to_path_buf()));
}

/// Have every scan hand the entries whose file is gone to the worker,
/// which follows a file that moved or was renamed inside Tilo's filesystem
/// to where it is now, or else points the entry at another copy of the
/// app still there ([`Store::fall_back`]). A file elsewhere is found again
/// by running it ([`Store::update_location`]) or by Locate.
pub fn follow_moves() {
    desktop_entry::cache::on_missing_program(|entry| {
        if let Some(downloaded) = &entry.downloaded
            && (downloaded.record().is_some() || !downloaded.locations.is_empty())
        {
            send(Job::Follow(entry.id.clone(), downloaded.record()));
        }
    });
    // Files that moved while the session was not running: the first scan
    // may have run before the hook was set.
    send(Job::FollowAll);
}

/// Wait until every job queued before this call has run. For tests and for
/// a caller that must see its own change.
///
/// Every window reaches this worker through the X11 tracer's queue
/// (`crate::compositor::x11_windows`), so the wait goes through that
/// queue first: a window that named itself before this call has been
/// looked at when it returns.
pub fn settle(wait: Duration) -> bool {
    let (reply, done) = mpsc::channel();
    #[cfg(target_os = "linux")]
    crate::compositor::x11_windows::settle(reply);
    #[cfg(not(target_os = "linux"))]
    settle_after(reply);
    done.recv_timeout(wait).is_ok()
}

/// Answer `reply` once every job queued here before this call has run.
pub(crate) fn settle_after(reply: mpsc::Sender<()>) {
    send(Job::Settle(reply));
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
        Job::Add(surface_id, move_to_applications, reply) => {
            let added = add(surface_id, move_to_applications);
            if let Err(error) = &added {
                tracing::warn!(%error, "could not add a downloaded app");
            }
            let _ = reply.send(added);
        }
        Job::Decline(surface_id) => {
            let Some(candidate) = candidates().lock_recover().remove(&surface_id) else {
                return;
            };
            if let Some(store) = Store::for_session()
                && let Err(error) = store.decline(&candidate.program.path)
            {
                tracing::warn!(%error, "could not remember a declined downloaded app");
            }
        }
        Job::Locate(desktop_id, program) => {
            if let Some(store) = Store::for_session()
                && let Err(error) = store.locate(&desktop_id, &program)
            {
                tracing::warn!(%desktop_id, %error, "could not point a downloaded app's entry at its file");
            }
            refresh_scan();
        }
        Job::Follow(desktop_id, record) => {
            let Some(store) = Store::for_session() else {
                return;
            };
            let moved = record
                .and_then(|record| {
                    file_explorer::model::resolve_workspace_record(
                        &file_explorer::model::workspace_record_authority_roots(),
                        uuid::Uuid::from_bytes(record),
                    )
                })
                .map(|found| found.path)
                .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()));
            let changed = match moved {
                Some(path) => store.locate(&desktop_id, &path).map(|()| true),
                None => store.fall_back(&desktop_id).map(|copy| copy.is_some()),
            };
            match changed {
                Ok(true) => refresh_scan(),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%desktop_id, %error, "could not follow a downloaded app's file");
                }
            }
        }
        Job::FollowAll => {
            for entry in desktop_entry::cache::cached_scan().entries {
                if let Some(downloaded) = entry
                    .downloaded
                    .as_ref()
                    .filter(|downloaded| !downloaded.present)
                {
                    let record = downloaded.record();
                    run(Job::Follow(entry.id, record), seen);
                }
            }
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
            && crate::shell::host_runtime::proc_stat(window.pid).map(|(_, start)| start)
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
        let Some(store) = Store::for_session() else {
            return;
        };
        // A copy only its window names joins the entry another copy of the
        // same program has.
        let identity = store.identify(
            &program,
            &WindowFacts {
                app_id: window.app_id.clone(),
                icon: window_icon(window.surface_id),
            },
        );
        match store.update_location(&identity, &program) {
            Ok(Some(desktop_id)) => {
                tracing::info!(%desktop_id, program = %program.path.display(), "a downloaded app now runs from another file");
                refresh_scan();
            }
            Ok(None) if store.lookup(&identity.key).is_some() => {}
            Ok(None) => {
                // Asked once per file: one the person said no to stays a
                // candidate search can add, and is not asked about again.
                let declined = store.declined(&program.path);
                let ask = Ask {
                    surface_id: window.surface_id,
                    name: identity.name.clone(),
                    file: program.path.clone(),
                    appimage: program.kind == ProgramKind::AppImage,
                    icon: ask_icon(identity.icon.as_ref()),
                };
                candidates().lock_recover().insert(
                    window.surface_id,
                    Candidate {
                        program,
                        identity,
                        app_id: window.app_id.clone(),
                    },
                );
                if !declined {
                    asks().lock_recover().push_back(ask);
                    wake();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not update a downloaded app's entry");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = window;
}

fn add(surface_id: u64, move_to_applications: bool) -> Result<String, String> {
    let mut candidate = candidate(surface_id).ok_or_else(|| "no downloaded program".to_owned())?;
    // The window may have set its icon after it named itself.
    if candidate.identity.icon.is_none() {
        candidate.identity.icon = window_icon(surface_id);
    }
    let store = Store::for_session().ok_or_else(|| "no place for the person's apps".to_owned())?;
    // Only an AppImage is one self-contained file; anything else may need
    // the files beside it and stays where it is.
    if move_to_applications && candidate.program.kind == ProgramKind::AppImage {
        let folder = applications_folder().ok_or_else(|| "no home folder".to_owned())?;
        candidate.program.path = downloaded::move_into(&candidate.program.path, &folder)
            .map_err(|error| error.to_string())?;
    }
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

/// The edge, in device pixels, an asked-about app's icon is drawn at.
const ASK_ICON_PX: u32 = 96;

/// The person's Applications folder, where an `AppImage` they chose to move
/// goes: `~/Applications`, a visible folder in their files.
fn applications_folder() -> Option<PathBuf> {
    tilo_paths::home().map(|home| home.join("Applications"))
}

/// The icon the question shows, the one the entry will have: the app's
/// own artwork, else the system's generic application icon
/// ([`desktop_entry::downloaded::GENERIC_ICON`]), as the list and search
/// show an entry without artwork.
pub fn ask_icon(icon: Option<&Icon>) -> Option<crate::app_icons::Raster> {
    match icon {
        Some(icon) => crate::app_icons::rasterize_bytes(&icon.bytes, icon.format, ASK_ICON_PX),
        None => desktop_entry::icon_theme::lookup(
            desktop_entry::downloaded::GENERIC_ICON,
            ASK_ICON_PX,
            1,
        )
        .and_then(|file| crate::app_icons::rasterize(&file, ASK_ICON_PX)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_draws_the_apps_own_artwork_at_its_size() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" rx="2" fill="#2a3fd1"/></svg>"##;
        let icon = Icon::sniff(svg.to_vec()).unwrap();
        let raster = ask_icon(Some(&icon)).expect("the app's own icon is drawn");
        assert_eq!(raster.image_data().width, ASK_ICON_PX);
    }
}
