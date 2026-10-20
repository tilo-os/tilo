//! Apps the machine can open but has not installed yet — the other
//! list, complementing [`crate::shell::launcher`]'s enumeration of
//! what's *here*. Listing and installing both shell out to
//! `app`: network round-trips, signature verification and
//! archive extraction don't belong in the process that owns every
//! window on the machine.
//!
//! [`available_apps`] answers from an in-memory cache and returns
//! immediately even when stale or empty — the palette calls it on every
//! keystroke. [`install_and_launch`] fetches, then starts through the
//! *same* [`crate::shell::launcher::launch`] every installed app uses,
//! so it's sandboxed, roomed and placed identically.
//!
//! The rest of the catalogue the Store and search share, Flathub and the
//! block list ([`shared_catalogue`]), is read from the files
//! [`app_catalogue`] names, on a worker, when they change.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub use registry_types::catalogue::{CatalogueApp, CatalogueListing};

use crate::shell::launcher::{self, LaunchEvent, LaunchEventKind};
use crate::shell::state::RoomId;
use tilo_lock::MutexRecover as _;

/// Much longer than the installed-apps TTL: that re-reads two local
/// files, this talks to a network service.
const CATALOGUE_TTL: Duration = Duration::from_secs(120);

/// Without this an unreachable registry would start a fresh HTTP
/// attempt every time the palette opened.
const FAILED_REFRESH_BACKOFF: Duration = Duration::from_secs(30);
const LIST_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_OUTPUT_LIMIT: usize = 1024 * 1024;
pub(crate) const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn safe_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(512)
        .collect::<String>()
        .trim()
        .to_owned()
}

// ---------------------------------------------------------------------
// Process-wide state. One machine, one set of configured registries.
// ---------------------------------------------------------------------

static CATALOGUE: Mutex<Option<(Vec<CatalogueApp>, Instant)>> = Mutex::new(None);
static CATALOGUE_REVISION: AtomicU64 = AtomicU64::new(0);
/// So a burst of keystrokes schedules one refresh, not one per character.
static REFRESHING: AtomicBool = AtomicBool::new(false);
/// The small search-metadata cache is read once per shell process. App
/// payloads remain uncached until the user opens one.
static DISK_CACHE_LOADED: AtomicBool = AtomicBool::new(false);

/// Lock [`CATALOGUE`], recovering from poisoning instead of panicking:
/// a panic while holding this lock doesn't corrupt the cached listing
/// it protects, so a poisoned lock is treated the same as a healthy
/// one rather than wedging every future caller in this process.
/// What an install under way has done so far, as `app install
/// --progress-lines` reports it, for the card standing in the place its
/// window will open in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallProgress {
    Downloading { received: u64, total: Option<u64> },
    Installing,
}

static INSTALL_PROGRESS: Mutex<Vec<(String, InstallProgress)>> = Mutex::new(Vec::new());

/// How often the install worker looks for new progress lines.
const INSTALL_PROGRESS_POLL: Duration = Duration::from_millis(100);

fn with_install_progress<T>(f: impl FnOnce(&mut Vec<(String, InstallProgress)>) -> T) -> T {
    f(&mut INSTALL_PROGRESS.lock_recover())
}

/// The latest progress `package_id`'s install reported, if it is under way
/// and has reported any.
pub(crate) fn install_progress(package_id: &str) -> Option<InstallProgress> {
    with_install_progress(|all| {
        all.iter()
            .find(|(package, _)| package == package_id)
            .map(|(_, progress)| *progress)
    })
}

pub(crate) fn set_install_progress(package_id: &str, progress: Option<InstallProgress>) {
    with_install_progress(|all| {
        all.retain(|(package, _)| package != package_id);
        if let Some(progress) = progress {
            all.push((package_id.to_owned(), progress));
        }
    });
}

/// One `--progress-lines` line; anything else is not progress.
fn parse_progress_line(line: &str) -> Option<InstallProgress> {
    let mut words = line.trim().strip_prefix("progress ")?.split(' ');
    match (words.next()?, words.next(), words.next(), words.next()) {
        ("installing", None, None, None) => Some(InstallProgress::Installing),
        ("downloading", Some(received), Some(total), None) => Some(InstallProgress::Downloading {
            received: received.parse().ok()?,
            total: match total {
                "-" => None,
                total => Some(total.parse().ok()?),
            },
        }),
        _ => None,
    }
}

/// What a failed install says went wrong: its `app:` error lines, without
/// the progress and the announcements it printed on the way.
fn install_failure_detail(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let lines = || {
        stderr
            .lines()
            .filter(|line| !line.trim().is_empty() && parse_progress_line(line).is_none())
    };
    let errors: Vec<&str> = lines().filter(|line| line.starts_with("app:")).collect();
    let detail = if errors.is_empty() {
        lines().next_back().unwrap_or_default().to_owned()
    } else {
        errors.join(" ")
    };
    safe_diagnostic(detail.as_bytes())
}

/// Run `command` (an `app install --progress-lines`), recording its progress
/// for `package_id` as the lines arrive.
fn run_install_reporting(
    command: std::process::Command,
    package_id: &str,
) -> Result<bounded_command::Output, bounded_command::Error> {
    let mut child = bounded_command::spawn_prepared_observing_stderr(
        command,
        None,
        INSTALL_OUTPUT_LIMIT,
        INSTALL_TIMEOUT,
    )?;
    let mut partial = Vec::new();
    let result = loop {
        while let Some(observation) = child.try_stderr() {
            let bounded_command::StderrObservation::Bytes(bytes) = observation else {
                continue;
            };
            partial.extend(bytes);
            while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = partial.drain(..=end).collect();
                if let Some(progress) = parse_progress_line(&String::from_utf8_lossy(&line)) {
                    set_install_progress(package_id, Some(progress));
                }
            }
        }
        if let Some(result) = child.try_wait() {
            break result;
        }
        std::thread::sleep(INSTALL_PROGRESS_POLL);
    };
    set_install_progress(package_id, None);
    result
}

fn lock_catalogue() -> std::sync::MutexGuard<'static, Option<(Vec<CatalogueApp>, Instant)>> {
    CATALOGUE.lock_recover()
}

/// Run `f` on the session's launches still opening, recovering from
/// poisoning as [`lock_catalogue`] does.
fn with_pending_launches<T>(f: impl FnOnce(&mut Vec<String>) -> T) -> T {
    launcher::LaunchSession::with(|session| {
        let mut pending = session.opening_launches.lock_recover();
        f(&mut pending)
    })
}

/// The same search the launcher does for `tilo-launcher`, including its
/// rule that only a developer build lets the environment name the binary
/// (`TILO_APP_BIN`): the catalogue binary runs unconfined and decides what
/// gets offered and installed.
fn catalogue_bin_candidates() -> Vec<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    catalogue_bin_candidates_from(
        crate::compositor::dev_env::DEV_ENV_OVERRIDES,
        std::env::var_os("TILO_APP_BIN"),
        current_exe.as_deref(),
    )
}

/// [`catalogue_bin_candidates`] as a pure function of its inputs, so the
/// shipped ordering is assertable from a debug test run.
fn catalogue_bin_candidates_from(
    dev_overrides: bool,
    explicit: Option<std::ffi::OsString>,
    current_exe: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dev_overrides
        && let Some(explicit) = explicit
        && !explicit.is_empty()
    {
        out.push(PathBuf::from(explicit));
    }
    if let Some(dir) = current_exe.and_then(std::path::Path::parent) {
        out.push(dir.join("app"));
    }
    out.push(PathBuf::from("/usr/libexec/app"));
    out.push(PathBuf::from("/usr/bin/app"));
    out
}

/// `None` means no catalogue binary — not an error, the desktop then
/// offers exactly the installed apps.
pub fn resolve_catalogue_bin() -> Option<PathBuf> {
    catalogue_bin_candidates().into_iter().find(|p| p.exists())
}

/// Install an owned service through the same normal registry verifier used
/// by app search, without opening a window or changing workspace capabilities.
/// Call only on a worker after the original host's enrollment approval.
#[cfg(target_os = "linux")]
pub(crate) fn install_service_package(package: &str) -> std::io::Result<()> {
    let bin = resolve_catalogue_bin().ok_or(std::io::ErrorKind::NotFound)?;
    let mut command = std::process::Command::new(bin);
    command.args(["install", package]);
    let output =
        bounded_command::run_prepared(command, None, INSTALL_OUTPUT_LIMIT, INSTALL_TIMEOUT)
            .map_err(std::io::Error::other)?;
    if !output.status.success() {
        return Err(std::io::ErrorKind::Other.into());
    }
    invalidate();
    launcher::invalidate_app_cache();
    Ok(())
}

/// Split out from the subprocess so the wire contract is assertable
/// without spawning anything.
///
/// # Errors
///
/// Returns the underlying parse error (as a message) if `stdout` is
/// not valid JSON or doesn't match [`CatalogueListing`]'s shape.
pub fn parse_listing(stdout: &str) -> Result<CatalogueListing, String> {
    serde_json::from_str(stdout).map_err(|e| e.to_string())
}

fn listing_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TILO_APP_CACHE_FILE") {
        return Some(PathBuf::from(path));
    }
    tilo_paths::cache_dir().map(|dir| dir.join("tilo-store/catalogue.json"))
}

fn read_cached_listing(path: &std::path::Path) -> Result<CatalogueListing, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_listing(&text)
}

/// Seed palette search from the last complete, verified catalogue response.
/// This is deliberately synchronous: it is one small local read, while the
/// network refresh it enables us to hide remains on the worker thread.
fn load_disk_cache_once() -> bool {
    if DISK_CACHE_LOADED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let Some(path) = listing_cache_path() else {
        return false;
    };
    let Ok(listing) = read_cached_listing(&path) else {
        return false;
    };
    *lock_catalogue() = Some((listing.apps, Instant::now()));
    CATALOGUE_REVISION.fetch_add(1, Ordering::Relaxed);
    true
}

/// Returns immediately: a cold or stale cache schedules a refresh and
/// answers with whatever it currently holds.
pub fn available_apps() -> Vec<CatalogueApp> {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("catalogue available apps");
    let loaded_from_disk = load_disk_cache_once();
    let (apps, needs_refresh) = {
        let cache = lock_catalogue();
        match cache.as_ref() {
            Some((apps, at)) => (
                apps.clone(),
                loaded_from_disk || at.elapsed() >= CATALOGUE_TTL,
            ),
            None => (Vec::new(), true),
        }
    };
    if needs_refresh {
        schedule_refresh();
    }
    apps
}

/// Revisioned immutable catalogue snapshot for retained search projections.
pub fn available_apps_snapshot() -> (u64, Vec<CatalogueApp>) {
    let before = CATALOGUE_REVISION.load(Ordering::Acquire);
    let apps = available_apps();
    let after = CATALOGUE_REVISION.load(Ordering::Acquire);
    (if before == after { before } else { 0 }, apps)
}

/// Current catalogue source generation, without cloning the catalogue.
#[must_use]
pub fn available_apps_generation() -> u64 {
    CATALOGUE_REVISION.load(Ordering::Relaxed)
}

/// Make the cached catalogue `apps`, as a fresh listing would.
#[cfg(test)]
pub(crate) fn replace_for_test(apps: Vec<CatalogueApp>) {
    DISK_CACHE_LOADED.store(true, Ordering::Relaxed);
    *lock_catalogue() = Some((apps, Instant::now()));
    CATALOGUE_REVISION.fetch_add(1, Ordering::Relaxed);
}

/// Drop the cached catalogue and force the next read to refetch.
/// Called after an install so the app that just arrived stops being
/// offered as available.
pub fn invalidate() {
    *lock_catalogue() = None;
    CATALOGUE_REVISION.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------
// The rest of the shared catalogue (`app_catalogue`): Flathub, read from
// Flatpak's cached AppStream metadata, and the block list. Other schedules
// keep those files current (Flatpak's AppStream update in the hourly
// `tilo-app-update` run, the Store for the block list); this re-reads a
// file on a worker when it changed, never on a keystroke.
// ---------------------------------------------------------------------

/// How often the files behind the shared catalogue are looked at again.
/// Only `stat`s run at this pace; a file is read again when it changed.
const SHARED_RECHECK: Duration = Duration::from_secs(30);

/// Flathub's apps and the block list, as last read.
#[derive(Clone, Default)]
pub struct SharedCatalogue {
    pub flathub: std::sync::Arc<Vec<app_catalogue::CatalogueEntry>>,
    pub blocks: std::sync::Arc<app_catalogue::BlockList>,
}

struct SharedState {
    catalogue: SharedCatalogue,
    /// The cache stamp and desktop language Flathub was read for.
    flathub_read: Option<(u64, String)>,
    blocks_stamp: Option<u64>,
    checked_at: Option<Instant>,
}

static SHARED: Mutex<Option<SharedState>> = Mutex::new(None);
static SHARED_REVISION: AtomicU64 = AtomicU64::new(0);
static SHARED_CHECKING: AtomicBool = AtomicBool::new(false);

/// Flathub's apps and the block list, as last read. Returns immediately
/// and reads no file; [`refresh_shared`] keeps it current.
pub fn shared_catalogue() -> SharedCatalogue {
    SHARED
        .lock_recover()
        .as_ref()
        .map(|state| state.catalogue.clone())
        .unwrap_or_default()
}

/// [`shared_catalogue`] with the revision it was read at; `0` when it
/// changed during the read.
pub fn shared_snapshot() -> (u64, SharedCatalogue) {
    let before = SHARED_REVISION.load(Ordering::Acquire);
    let catalogue = shared_catalogue();
    let after = SHARED_REVISION.load(Ordering::Acquire);
    (if before == after { before } else { 0 }, catalogue)
}

/// Changes whenever Flathub's apps or the block list do.
#[must_use]
pub fn shared_generation() -> u64 {
    SHARED_REVISION.load(Ordering::Relaxed)
}

/// Look again at the files behind the shared catalogue when that is due,
/// on a worker, and read whichever changed.
pub fn refresh_shared() {
    let due = SHARED
        .lock_recover()
        .as_ref()
        .and_then(|state| state.checked_at)
        .is_none_or(|at| at.elapsed() >= SHARED_RECHECK);
    if !due || SHARED_CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(test)]
    crate::shell::test_support::ambient_start("shared catalogue refresh");
    let locale = crate::shell::i18n::current_locale();
    let wake = crate::shell::search::invocation_wake::waker();
    let spawned = std::thread::Builder::new()
        .name("catalogue-shared".to_string())
        .spawn(move || {
            let (flathub_read, blocks_stamp) = SHARED
                .lock_recover()
                .as_ref()
                .map_or((None, None), |state| {
                    (state.flathub_read.clone(), state.blocks_stamp)
                });
            let sources = app_catalogue::appstream::cached_sources();
            let read = (app_catalogue::appstream::cache_stamp(&sources), locale);
            let flathub = (flathub_read.as_ref() != Some(&read)).then(|| {
                std::sync::Arc::new(app_catalogue::appstream::load_all(&sources, &read.1))
            });
            let blocks_now = app_catalogue::block::current_stamp();
            let blocks = (blocks_stamp != Some(blocks_now))
                .then(|| std::sync::Arc::new(app_catalogue::BlockList::load()));
            let changed = flathub.is_some() || blocks.is_some();
            {
                let mut guard = SHARED.lock_recover();
                let state = guard.get_or_insert_with(|| SharedState {
                    catalogue: SharedCatalogue::default(),
                    flathub_read: None,
                    blocks_stamp: None,
                    checked_at: None,
                });
                if let Some(flathub) = flathub {
                    state.catalogue.flathub = flathub;
                    state.flathub_read = Some(read);
                }
                if let Some(blocks) = blocks {
                    state.catalogue.blocks = blocks;
                    state.blocks_stamp = Some(blocks_now);
                }
                state.checked_at = Some(Instant::now());
                drop(guard);
            }
            if changed {
                SHARED_REVISION.fetch_add(1, Ordering::Release);
                wake.wake();
            }
            SHARED_CHECKING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        SHARED_CHECKING.store(false, Ordering::SeqCst);
    }
}

/// Stage the catalogue a screenshot scene searches: the Tilo apps the
/// registries offer and Flathub's cached apps, read from nowhere and kept
/// from being read again. Only the `shots` harness calls this.
#[doc(hidden)]
pub fn stage_for_shots(apps: Vec<CatalogueApp>, flathub: Vec<app_catalogue::CatalogueEntry>) {
    DISK_CACHE_LOADED.store(true, Ordering::Relaxed);
    *lock_catalogue() = Some((apps, Instant::now()));
    CATALOGUE_REVISION.fetch_add(1, Ordering::Relaxed);
    *SHARED.lock_recover() = Some(SharedState {
        catalogue: SharedCatalogue {
            flathub: std::sync::Arc::new(flathub),
            blocks: std::sync::Arc::default(),
        },
        flathub_read: Some((u64::MAX, String::new())),
        blocks_stamp: Some(u64::MAX),
        checked_at: Some(Instant::now()),
    });
    SHARED_REVISION.fetch_add(1, Ordering::Release);
}

/// A failure caches an *empty* list rather than leaving the cache cold
/// (which would mean a new HTTP attempt per character typed), and
/// expires under [`FAILED_REFRESH_BACKOFF`] so a registry that comes
/// back is picked up shortly after.
fn schedule_refresh() {
    if REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(bin) = resolve_catalogue_bin() else {
        *lock_catalogue() = Some((Vec::new(), Instant::now()));
        REFRESHING.store(false, Ordering::SeqCst);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("app-refresh".to_string())
        .spawn(move || {
            let mut command = std::process::Command::new(&bin);
            command.args(["list", "--json"]);
            let out = bounded_command::run_prepared(command, None, LIST_OUTPUT_LIMIT, LIST_TIMEOUT);
            let (apps, expiry) = match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    match parse_listing(&text) {
                        Ok(listing) => {
                            for bad in &listing.unreachable {
                                eprintln!(
                                    "[catalogue] registry {} unreachable: {}",
                                    bad.name, bad.detail
                                );
                            }
                            let fresh = listing.unreachable.is_empty();
                            if fresh
                                && let Some(path) = listing_cache_path()
                                && let Ok(encoded) = serde_json::to_vec(&listing)
                                && let Err(e) = tilo_files::native::atomic::write_creating_parent(
                                    &path, &encoded,
                                )
                            {
                                eprintln!("[catalogue] could not save search cache: {e}");
                            }
                            (listing.apps, fresh)
                        }
                        Err(e) => {
                            eprintln!("[catalogue] could not parse the listing: {e}");
                            (Vec::new(), false)
                        }
                    }
                }
                Ok(o) => {
                    eprintln!(
                        "[catalogue] list failed ({}): {}",
                        o.status,
                        safe_diagnostic(&o.stderr)
                    );
                    (Vec::new(), false)
                }
                Err(e) => {
                    eprintln!("[catalogue] could not run {}: {e}", bin.display());
                    (Vec::new(), false)
                }
            };
            // A result we know is incomplete is backdated so it
            // expires sooner than a good one.
            let stamped = if expiry {
                Instant::now()
            } else {
                Instant::now()
                    .checked_sub(CATALOGUE_TTL.saturating_sub(FAILED_REFRESH_BACKOFF))
                    .unwrap_or_else(Instant::now)
            };
            *lock_catalogue() = Some((apps, stamped));
            CATALOGUE_REVISION.fetch_add(1, Ordering::Relaxed);
            REFRESHING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------
// Opening something that isn't here yet.
// ---------------------------------------------------------------------

/// Note that `package_id` is opening. The window's own place (its
/// placeholder card, then its first frame) is what the person sees; this
/// only keeps a repeated activation from starting a second launch.
pub fn note_pending_launch(package_id: &str) {
    with_pending_launches(|pending| {
        if !pending.iter().any(|id| id == package_id) {
            pending.push(package_id.to_string());
        }
    });
}

/// A peek, not a take — the frame thread walks this every frame.
/// Retirement goes through [`take_pending_launch`].
pub fn pending_launches() -> Vec<String> {
    with_pending_launches(|pending| pending.clone())
}

/// Whether `package_id` is still opening. Launching the same app repeatedly
/// while its first install/start is in flight must not start another one.
pub fn pending_launch(package_id: &str) -> bool {
    with_pending_launches(|pending| pending.iter().any(|id| id == package_id))
}

/// Settle the launch of `package_id`: its window appeared, or it failed.
/// Returns whether it was still opening.
pub fn take_pending_launch(package_id: &str) -> bool {
    with_pending_launches(|pending| {
        let Some(idx) = pending.iter().position(|id| id == package_id) else {
            return false;
        };
        pending.remove(idx);
        true
    })
}

/// `capabilities` is the activating workspace's launch grants, captured
/// on the frame thread rather than read fresh — an install takes
/// seconds, and the app must land in the sandbox it was opened from,
/// not whichever workspace is active when the download finishes.
pub fn install_and_launch(
    package_id: String,
    display_name: String,
    room: RoomId,
    capabilities: collab_types::caps::LaunchGrants,
) {
    install_then(package_id, display_name, move |package_id, display_name| {
        // The install is done; the window is now due as for any start.
        launcher::hold_presentation_for(package_id, Duration::ZERO);
        launcher::set_workspace_capabilities(capabilities);
        if let Err(e) = launcher::launch(package_id, room) {
            push_failure(package_id, display_name, e.user_message());
        }
    });
}

/// Fetch `package_id` on a worker, then run `then` there with its id and
/// name once it is installed. A failure is reported as a launch failure of
/// `display_name`, and `then` does not run.
pub fn install_then(
    package_id: String,
    display_name: String,
    then: impl FnOnce(&str, &str) + Send + 'static,
) {
    let Some(bin) = resolve_catalogue_bin() else {
        push_failure(
            &package_id,
            &display_name,
            crate::shell::i18n::t("catalogue-error-missing"),
        );
        return;
    };
    let builder = std::thread::Builder::new().name("app-install".to_string());
    let identity = (package_id.clone(), display_name.clone());
    let spawn_result = launcher::spawn_reporting(builder, move || {
        let mut command = std::process::Command::new(&bin);
        command.args(["install", "--progress-lines", &package_id]);
        let out = run_install_reporting(command, &package_id);
        match out {
            Err(e) => {
                push_failure(
                    &package_id,
                    &display_name,
                    crate::shell::i18n::t_args(
                        "catalogue-error-run",
                        &[("error", e.to_string().into())],
                    ),
                );
                return;
            }
            Ok(o) if !o.status.success() => {
                let detail = install_failure_detail(&o.stderr);
                let detail = if detail.is_empty() {
                    crate::shell::i18n::t_args(
                        "catalogue-error-exited",
                        &[("status", o.status.to_string().into())],
                    )
                } else {
                    detail
                };
                push_failure(&package_id, &display_name, detail);
                return;
            }
            Ok(_) => {}
        }

        invalidate();
        launcher::invalidate_app_cache();
        then(&package_id, &display_name);
    });
    if spawn_result.is_err() {
        push_failure(
            &identity.0,
            &identity.1,
            crate::shell::i18n::t("catalogue-error-spawn"),
        );
    }
}

/// Update `package_id` to the newest build this system can run, reporting
/// its download like an install's ([`install_progress`]). Blocks; call on a
/// worker.
pub(crate) fn update_package(package_id: &str) -> crate::shell::app_versions::UpdateOutcome {
    use crate::shell::app_versions::UpdateOutcome;
    let Some(bin) = resolve_catalogue_bin() else {
        return UpdateOutcome::Failed(crate::shell::i18n::t("catalogue-error-missing"));
    };
    let mut command = std::process::Command::new(&bin);
    command.args(["update", "--progress-lines", package_id]);
    match run_install_reporting(command, package_id) {
        Err(error) => UpdateOutcome::Failed(crate::shell::i18n::t_args(
            "catalogue-error-run",
            &[("error", error.to_string().into())],
        )),
        Ok(output) if output.status.success() => {
            invalidate();
            UpdateOutcome::Updated
        }
        Ok(output) => {
            let detail = install_failure_detail(&output.stderr);
            UpdateOutcome::Failed(if detail.is_empty() {
                crate::shell::i18n::t_args(
                    "catalogue-error-exited",
                    &[("status", output.status.to_string().into())],
                )
            } else {
                detail
            })
        }
    }
}

/// Report that getting `package_id` failed, as a failed launch of
/// `display_name` saying `detail`: the place held for it says so and offers
/// to try again.
pub(crate) fn report_install_failure(package_id: &str, display_name: &str, detail: String) {
    push_failure(package_id, display_name, detail);
}

// ---------------------------------------------------------------------
// Installing from Flathub: the app host's rule (`app_catalogue::flathub`),
// into the person's own installation, then opened like any host app.
// ---------------------------------------------------------------------

/// Flathub installs under way, by app id, so a repeated press is one install.
static FLATHUB_INSTALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Reading Flatpak's configured sources is local and quick.
const FLATPAK_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether `app_id` is being installed from Flathub.
pub(crate) fn installing_from_flathub(app_id: &str) -> bool {
    FLATHUB_INSTALLS
        .lock_recover()
        .iter()
        .any(|id| id == app_id)
}

/// Install `app_id` from Flathub on a worker, then open it at `placement`
/// once its desktop entry is there. A failure is reported as a failed
/// launch of `display_name`.
pub(crate) fn install_flathub_and_open(
    app_id: String,
    display_name: String,
    placement: crate::shell::tab_binding::Placement,
    portal_workspace_id: Option<String>,
) {
    {
        let mut installs = FLATHUB_INSTALLS.lock_recover();
        if installs.contains(&app_id) {
            return;
        }
        installs.push(app_id.clone());
    }
    let builder = std::thread::Builder::new().name("flathub-install".to_string());
    let identity = (app_id.clone(), display_name.clone());
    let spawned = launcher::spawn_reporting(builder, move || {
        let outcome = install_from_flathub(&app_id);
        FLATHUB_INSTALLS.lock_recover().retain(|id| *id != app_id);
        if let Err(detail) = outcome {
            push_failure(&app_id, &display_name, detail);
            return;
        }
        crate::shell::host_apps::invalidate_cache();
        let Some(app) = crate::shell::host_apps::find(&app_id) else {
            push_failure(
                &app_id,
                &display_name,
                crate::shell::i18n::t("catalogue-error-flathub-not-found"),
            );
            return;
        };
        let queued = crate::shell::host_apps::queue_launch(crate::shell::host_apps::QueuedLaunch {
            app,
            placement,
            portal_workspace_id,
            requested_at: Instant::now(),
            reopen_from_tray: true,
        });
        if let Err(error) = queued {
            push_failure(&app_id, &display_name, error.user_message());
        }
    });
    if spawned.is_err() {
        FLATHUB_INSTALLS
            .lock_recover()
            .retain(|id| *id != identity.0);
        push_failure(
            &identity.0,
            &identity.1,
            crate::shell::i18n::t("catalogue-error-spawn"),
        );
    }
}

/// Run the Flatpak commands that install `app_id` from Flathub. Blocks.
fn install_from_flathub(app_id: &str) -> Result<(), String> {
    let run = |args: &[String], timeout: Duration| -> Result<Vec<u8>, String> {
        let mut command = std::process::Command::new("flatpak");
        command.args(args);
        let output = bounded_command::run_prepared(command, None, INSTALL_OUTPUT_LIMIT, timeout)
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "catalogue-error-flathub-run",
                    &[("error", error.to_string().into())],
                )
            })?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(safe_diagnostic(&output.stderr))
        }
    };
    let remotes = run(
        &[
            "remotes".to_owned(),
            "--columns=name,url,title,options".to_owned(),
        ],
        FLATPAK_QUERY_TIMEOUT,
    )?;
    let commands =
        app_catalogue::flathub::install_commands(app_id, &String::from_utf8_lossy(&remotes))
            .map_err(|refusal| {
                crate::shell::i18n::t(match refusal {
                    app_catalogue::flathub::InstallRefusal::InvalidId => {
                        "catalogue-error-flathub-invalid"
                    }
                    app_catalogue::flathub::InstallRefusal::UntrustedUserSource => {
                        "catalogue-error-flathub-untrusted"
                    }
                    app_catalogue::flathub::InstallRefusal::NoFlathub => {
                        "catalogue-error-flathub-missing"
                    }
                })
            })?;
    for args in commands {
        run(&args, INSTALL_TIMEOUT)?;
    }
    Ok(())
}

fn push_failure(package_id: &str, display_name: &str, detail: String) {
    // Installation can fail after the search action has already reserved a
    // placement for the eventual window. Drop that reservation with the
    // failure event; otherwise a later, unrelated app can inherit the dead
    // launch's destination.
    launcher::clear_pending_placement(package_id);
    launcher::push_event(LaunchEvent {
        package_id: package_id.to_string(),
        display_name: display_name.to_string(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: LaunchEventKind::SpawnFailed {
            reason: launcher::LaunchFailure::Other,
            detail,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_reports_its_bytes_while_it_runs_and_fails_with_its_error_alone() {
        assert_eq!(
            parse_progress_line("progress downloading 4200000 18000000\n"),
            Some(InstallProgress::Downloading {
                received: 4_200_000,
                total: Some(18_000_000),
            })
        );
        assert_eq!(
            parse_progress_line("progress downloading 12 -"),
            Some(InstallProgress::Downloading {
                received: 12,
                total: None,
            })
        );
        assert_eq!(
            parse_progress_line("progress installing"),
            Some(InstallProgress::Installing)
        );
        assert_eq!(
            parse_progress_line("Finding the app in the Tilo Store…"),
            None
        );
        assert_eq!(parse_progress_line("progress downloading many -"), None);

        let package = "so.tilo.test.progress-lines";
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "echo 'Finding the app' >&2; echo 'progress downloading 5 10' >&2; \
             sleep 2; echo 'app: the store is unreachable' >&2; exit 1",
        ]);
        let worker = std::thread::spawn(move || run_install_reporting(command, package));
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut seen = None;
        while seen.is_none() && Instant::now() < deadline {
            seen = install_progress(package);
            std::thread::yield_now();
        }
        assert_eq!(
            seen,
            Some(InstallProgress::Downloading {
                received: 5,
                total: Some(10),
            }),
            "the bytes are known while the install still runs"
        );
        let output = worker.join().unwrap().expect("the install ran");
        assert_eq!(
            install_progress(package),
            None,
            "a finished install has none"
        );
        assert_eq!(
            install_failure_detail(&output.stderr),
            "app: the store is unreachable"
        );
    }

    #[test]
    fn catalogue_listing_parses_strictly_and_seeds_the_palette() {
        /// The exact JSON `app` prints.
        fn the_listing_envelope_parses_the_binarys_field_names() {
            let json = r#"{"apps":[{"package_id":"so.tilo.sheets","display_name":"Sheets",
                "description":"a spreadsheet","category":"Office","version":"2.0.0",
                "registry":"main","download_url":"https://x/y.tapp","trust":"verified"}],
                "unreachable":[{"name":"mirror","detail":"connection refused"}]}"#;
            let listing = parse_listing(json).expect("the envelope must parse");
            assert_eq!(listing.apps.len(), 1);
            let app = &listing.apps[0];
            assert_eq!(app.package_id, "so.tilo.sheets");
            assert_eq!(app.display_name, "Sheets");
            assert_eq!(app.category.as_deref(), Some("Office"));
            assert_eq!(app.version, "2.0.0");
            assert_eq!(app.registry, "main");
            assert_eq!(app.trust, "verified");
            assert_eq!(listing.unreachable.len(), 1);
            assert_eq!(listing.unreachable[0].name, "mirror");
        }

        /// Nothing available is an empty listing, not a parse error.
        fn an_empty_catalogue_is_a_valid_answer() {
            let listing = parse_listing(r#"{"apps":[],"unreachable":[]}"#).expect("parses");
            assert!(listing.apps.is_empty());
            let bare = parse_listing("{}").expect("a bare object parses too");
            assert!(bare.apps.is_empty());
        }

        fn garbage_on_stdout_is_an_error_not_an_empty_list() {
            assert!(
                parse_listing("not json at all").is_err(),
                "an unparseable listing must be reported, not silently read as \
                 'nothing is available'"
            );
        }

        fn persisted_metadata_can_seed_a_cold_palette() {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("catalogue.json");
            std::fs::write(
                &path,
                r#"{"apps":[{"package_id":"so.tilo.remote","display_name":"Remote",
                    "version":"1.0.0","registry":"team","download_url":"https://x/y.tapp",
                    "trust":"verified"}]}"#,
            )
            .unwrap();
            let listing = read_cached_listing(&path).expect("cached listing should parse");
            assert_eq!(listing.apps[0].package_id, "so.tilo.remote");
        }

        the_listing_envelope_parses_the_binarys_field_names();
        an_empty_catalogue_is_a_valid_answer();
        garbage_on_stdout_is_an_error_not_an_empty_list();
        persisted_metadata_can_seed_a_cold_palette();
    }

    #[test]
    fn catalogue_opening_launches_are_taken_once() {
        /// A pending launch belongs to exactly one open, and taking it
        /// consumes it.
        fn a_pending_launch_is_taken_once() {
            note_pending_launch("so.tilo.noticefixture");
            assert!(take_pending_launch("so.tilo.noticefixture"));
            assert!(!take_pending_launch("so.tilo.noticefixture"));
        }

        fn a_second_open_of_the_same_package_is_one_launch() {
            note_pending_launch("so.tilo.replacefixture");
            note_pending_launch("so.tilo.replacefixture");
            assert!(pending_launch("so.tilo.replacefixture"));
            assert!(take_pending_launch("so.tilo.replacefixture"));
            assert!(!pending_launch("so.tilo.replacefixture"));
        }

        a_pending_launch_is_taken_once();
        a_second_open_of_the_same_package_is_one_launch();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn catalogue_binary_override_is_dev_only_and_falls_through_when_missing() {
        /// How a test, or the headless harness, points at a specific build.
        fn an_explicit_binary_override_is_searched_first() {
            let tmp = tempfile::tempdir().unwrap();
            let fake = tmp.path().join("app");
            std::fs::write(&fake, b"#!/bin/true\n").unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APP_BIN", &fake);
            let resolved = resolve_catalogue_bin();
            env.remove("TILO_APP_BIN");
            assert_eq!(resolved.as_deref(), Some(fake.as_path()));
        }

        fn a_release_build_ignores_the_catalogue_binary_override() {
            let exe = std::path::Path::new("/usr/bin/shell");
            let shipped =
                catalogue_bin_candidates_from(false, Some("/tmp/evil-app".into()), Some(exe));
            assert!(
                !shipped.contains(&PathBuf::from("/tmp/evil-app")),
                "a release compositor must not run a catalogue binary named by the environment: {shipped:?}",
            );
            assert_eq!(shipped.first(), Some(&PathBuf::from("/usr/bin/app")));
            let developer = catalogue_bin_candidates_from(true, Some("/dev/app".into()), Some(exe));
            assert_eq!(developer.first(), Some(&PathBuf::from("/dev/app")));
        }

        fn a_missing_override_falls_through_rather_than_resolving_to_it() {
            let tmp = tempfile::tempdir().unwrap();
            let absent = tmp.path().join("not-here");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_APP_BIN", &absent);
            let resolved = resolve_catalogue_bin();
            env.remove("TILO_APP_BIN");
            assert_ne!(
                resolved.as_deref(),
                Some(absent.as_path()),
                "a path that does not exist must not be handed to Command::new"
            );
        }

        an_explicit_binary_override_is_searched_first();
        a_release_build_ignores_the_catalogue_binary_override();
        a_missing_override_falls_through_rather_than_resolving_to_it();
    }
}
