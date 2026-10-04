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

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shell::launcher::{self, LaunchEvent, LaunchEventKind};
use crate::shell::state::{CapOverride, RoomId};

/// Much longer than the installed-apps TTL: that re-reads two local
/// files, this talks to a network service.
const CATALOGUE_TTL: Duration = Duration::from_secs(120);

/// Without this an unreachable registry would start a fresh HTTP
/// attempt every time the palette opened.
const FAILED_REFRESH_BACKOFF: Duration = Duration::from_secs(30);
const LIST_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_OUTPUT_LIMIT: usize = 1024 * 1024;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn safe_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(512)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// **Wire contract.** These field names are what `app list
/// --json` prints; the crate that prints them pins the same names in
/// its own test, so changing one without the other fails loudly
/// instead of silently producing an empty catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CatalogueApp {
    pub package_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    pub version: String,
    /// Which configured registry offered it.
    pub registry: String,
    #[serde(default)]
    pub trust: String,
}

/// The envelope `app list --json` prints.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
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
fn lock_catalogue() -> std::sync::MutexGuard<'static, Option<(Vec<CatalogueApp>, Instant)>> {
    CATALOGUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run `f` on the session's pending "Opening…" notices, recovering from
/// poisoning as [`lock_catalogue`] does.
fn with_pending_notices<T>(f: impl FnOnce(&mut Vec<(String, u64)>) -> T) -> T {
    let session = launcher::LaunchSession::current();
    let mut pending = session
        .opening_notices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut pending)
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
    xdg_paths::cache_dir().map(|dir| dir.join("tilo-store/catalogue.json"))
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

pub fn note_pending_notice(package_id: &str, notification_id: u64) {
    with_pending_notices(|pending| {
        pending.retain(|(id, _)| id != package_id);
        pending.push((package_id.to_string(), notification_id));
    });
}

/// A peek, not a take — the frame thread walks this every frame.
/// Retirement goes through [`take_pending_notice`].
pub fn pending_notices() -> Vec<(String, u64)> {
    with_pending_notices(|pending| pending.clone())
}

/// Return the active opening notice for `package_id`, if any. Launching the
/// same app repeatedly while its first install/start is still in flight must
/// not create a stack of indistinguishable "Opening…" cards or overwrite the
/// only id that can retire the old cards.
pub fn pending_notice(package_id: &str) -> Option<u64> {
    with_pending_notices(|pending| {
        pending
            .iter()
            .find(|(id, _)| id == package_id)
            .map(|(_, notification_id)| *notification_id)
    })
}

/// So the "opening" toast is retired by the app appearing, not a timer.
pub fn take_pending_notice(package_id: &str) -> Option<u64> {
    with_pending_notices(|pending| {
        let idx = pending.iter().position(|(id, _)| id == package_id)?;
        Some(pending.remove(idx).1)
    })
}

/// `capabilities` is the activating workspace's grant list, captured
/// on the frame thread rather than read fresh — an install takes
/// seconds, and the app must land in the sandbox it was opened from,
/// not whichever workspace is active when the download finishes.
pub fn install_and_launch(
    package_id: String,
    display_name: String,
    room: RoomId,
    capabilities: Vec<CapOverride>,
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
        command.args(["install", &package_id]);
        let out =
            bounded_command::run_prepared(command, None, INSTALL_OUTPUT_LIMIT, INSTALL_TIMEOUT);
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
                let detail = safe_diagnostic(&o.stderr);
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

        launcher::set_workspace_capabilities(capabilities);
        if let Err(e) = launcher::launch(&package_id, room) {
            push_failure(&package_id, &display_name, e.user_message());
        }
    });
    if spawn_result.is_err() {
        push_failure(
            &identity.0,
            &identity.1,
            crate::shell::i18n::t("catalogue-error-spawn"),
        );
    }
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
        kind: LaunchEventKind::SpawnFailed { detail },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_listing_parses_strictly_and_seeds_the_palette() {
        /// Pinned against the exact JSON `app` prints;
        /// `app-cli`'s own test pins the other half.
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
                    "version":"1.0.0","registry":"team","trust":"verified"}]}"#,
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
    fn catalogue_open_notices_are_taken_once_and_replaced_by_newer_opens() {
        /// A pending notice belongs to exactly one open, and taking it
        /// consumes it — a second launch of the same app must not dismiss
        /// a notification that no longer exists.
        fn a_pending_notice_is_taken_once() {
            note_pending_notice("so.tilo.noticefixture", 42);
            assert_eq!(take_pending_notice("so.tilo.noticefixture"), Some(42));
            assert_eq!(take_pending_notice("so.tilo.noticefixture"), None);
        }

        fn a_newer_open_replaces_the_notice_for_the_same_package() {
            note_pending_notice("so.tilo.replacefixture", 1);
            assert_eq!(pending_notice("so.tilo.replacefixture"), Some(1));
            note_pending_notice("so.tilo.replacefixture", 2);
            assert_eq!(pending_notice("so.tilo.replacefixture"), Some(2));
            assert_eq!(
                take_pending_notice("so.tilo.replacefixture"),
                Some(2),
                "the live open's notification is the one to retire"
            );
            assert_eq!(take_pending_notice("so.tilo.replacefixture"), None);
        }

        a_pending_notice_is_taken_once();
        a_newer_open_replaces_the_notice_for_the_same_package();
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
