//! Apps the machine can open but has not installed yet — the other
//! list, complementing [`crate::shell::launcher`]'s enumeration of
//! what's *here*. Listing and installing both shell out to
//! `qos-catalogue`: network round-trips, signature verification and
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::shell::launcher::{self, LaunchEvent, LaunchEventKind};
use crate::shell::state::{CapOverride, RoomId};

/// Much longer than the installed-apps TTL: that re-reads two local
/// files, this talks to a network service.
const CATALOGUE_TTL: Duration = Duration::from_mins(2);

/// Without this an unreachable registry would start a fresh HTTP
/// attempt every time the palette opened.
const FAILED_REFRESH_BACKOFF: Duration = Duration::from_secs(30);

/// **Wire contract.** These field names are what `qos-catalogue list
/// --json` prints; the crate that prints them pins the same names in
/// its own test, so changing one without the other fails loudly
/// instead of silently producing an empty catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

/// The envelope `qos-catalogue list --json` prints.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
}

// ---------------------------------------------------------------------
// Process-wide state. One machine, one set of configured registries.
// ---------------------------------------------------------------------

static CATALOGUE: Mutex<Option<(Vec<CatalogueApp>, Instant)>> = Mutex::new(None);
/// So a burst of keystrokes schedules one refresh, not one per character.
static REFRESHING: AtomicBool = AtomicBool::new(false);
/// Per package, so the frame thread can retire it when the app starts.
static PENDING_NOTICES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// The same search the launcher does for `qos-launcher`.
fn catalogue_bin_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("QOS_CATALOGUE_BIN")
        && !explicit.is_empty()
    {
        out.push(PathBuf::from(explicit));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join("qos-catalogue"));
    }
    out.push(PathBuf::from("/usr/libexec/qos-catalogue"));
    out.push(PathBuf::from("/usr/bin/qos-catalogue"));
    out
}

/// `None` means no catalogue binary — not an error, the desktop then
/// offers exactly the installed apps.
pub fn resolve_catalogue_bin() -> Option<PathBuf> {
    catalogue_bin_candidates().into_iter().find(|p| p.exists())
}

/// Split out from the subprocess so the wire contract is assertable
/// without spawning anything.
pub fn parse_listing(stdout: &str) -> Result<CatalogueListing, String> {
    serde_json::from_str(stdout).map_err(|e| e.to_string())
}

/// Returns immediately: a cold or stale cache schedules a refresh and
/// answers with whatever it currently holds.
pub fn available_apps() -> Vec<CatalogueApp> {
    let (apps, needs_refresh) = {
        let cache = CATALOGUE.lock().expect("CATALOGUE poisoned");
        match cache.as_ref() {
            Some((apps, at)) => (apps.clone(), at.elapsed() >= CATALOGUE_TTL),
            None => (Vec::new(), true),
        }
    };
    if needs_refresh {
        schedule_refresh();
    }
    apps
}

/// Drop the cached catalogue and force the next read to refetch.
/// Called after an install so the app that just arrived stops being
/// offered as available.
pub fn invalidate() {
    *CATALOGUE.lock().expect("CATALOGUE poisoned") = None;
}

/// For tests, which must not depend on a registry being reachable.
pub fn seed_for_test(apps: Vec<CatalogueApp>) {
    *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((apps, Instant::now()));
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
        *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((Vec::new(), Instant::now()));
        REFRESHING.store(false, Ordering::SeqCst);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("qos-catalogue-refresh".to_string())
        .spawn(move || {
            let out = std::process::Command::new(&bin)
                .args(["list", "--json"])
                .output();
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
                        String::from_utf8_lossy(&o.stderr).trim()
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
            *CATALOGUE.lock().expect("CATALOGUE poisoned") = Some((apps, stamped));
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
    let mut pending = PENDING_NOTICES.lock().expect("PENDING_NOTICES poisoned");
    pending.retain(|(id, _)| id != package_id);
    pending.push((package_id.to_string(), notification_id));
}

/// A peek, not a take — the frame thread walks this every frame.
/// Retirement goes through [`take_pending_notice`].
pub fn pending_notices() -> Vec<(String, u64)> {
    PENDING_NOTICES
        .lock()
        .expect("PENDING_NOTICES poisoned")
        .clone()
}

/// So the "opening" toast is retired by the app appearing, not a timer.
pub fn take_pending_notice(package_id: &str) -> Option<u64> {
    let mut pending = PENDING_NOTICES.lock().expect("PENDING_NOTICES poisoned");
    let idx = pending.iter().position(|(id, _)| id == package_id)?;
    Some(pending.remove(idx).1)
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
            "this system has no app catalogue (qos-catalogue is missing)".to_string(),
        );
        return;
    };
    let builder = std::thread::Builder::new().name("qos-catalogue-install".to_string());
    let identity = (package_id.clone(), display_name.clone());
    let spawn_result = builder.spawn(move || {
        let out = std::process::Command::new(&bin)
            .args(["install", &package_id])
            .output();
        match out {
            Err(e) => {
                push_failure(
                    &package_id,
                    &display_name,
                    format!("could not run the app catalogue: {e}"),
                );
                return;
            }
            Ok(o) if !o.status.success() => {
                let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let detail = if detail.is_empty() {
                    format!("the app catalogue exited {}", o.status)
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
            "could not start loading it".to_string(),
        );
    }
}

fn push_failure(package_id: &str, display_name: &str, detail: String) {
    launcher::push_event(LaunchEvent {
        package_id: package_id.to_string(),
        display_name: display_name.to_string(),
        kind: LaunchEventKind::SpawnFailed { detail },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against the exact JSON `qos-catalogue` prints;
    /// `app-catalogue`'s own test pins the other half.
    #[test]
    fn the_listing_envelope_parses_the_binarys_field_names() {
        let json = r#"{"apps":[{"package_id":"dev.qos.sheets","display_name":"Sheets",
            "description":"a spreadsheet","category":"Office","version":"2.0.0",
            "registry":"main","download_url":"https://x/y.qpkg","trust":"verified"}],
            "unreachable":[{"name":"mirror","detail":"connection refused"}]}"#;
        let listing = parse_listing(json).expect("the envelope must parse");
        assert_eq!(listing.apps.len(), 1);
        let app = &listing.apps[0];
        assert_eq!(app.package_id, "dev.qos.sheets");
        assert_eq!(app.display_name, "Sheets");
        assert_eq!(app.category.as_deref(), Some("Office"));
        assert_eq!(app.version, "2.0.0");
        assert_eq!(app.registry, "main");
        assert_eq!(app.trust, "verified");
        assert_eq!(listing.unreachable.len(), 1);
        assert_eq!(listing.unreachable[0].name, "mirror");
    }

    /// Nothing available is an empty listing, not a parse error.
    #[test]
    fn an_empty_catalogue_is_a_valid_answer() {
        let listing = parse_listing(r#"{"apps":[],"unreachable":[]}"#).expect("parses");
        assert!(listing.apps.is_empty());
        let bare = parse_listing("{}").expect("a bare object parses too");
        assert!(bare.apps.is_empty());
    }

    #[test]
    fn garbage_on_stdout_is_an_error_not_an_empty_list() {
        assert!(
            parse_listing("not json at all").is_err(),
            "an unparseable listing must be reported, not silently read as \
             'nothing is available'"
        );
    }

    /// A pending notice belongs to exactly one open, and taking it
    /// consumes it — a second launch of the same app must not dismiss
    /// a notification that no longer exists.
    #[test]
    fn a_pending_notice_is_taken_once() {
        note_pending_notice("dev.qos.noticefixture", 42);
        assert_eq!(take_pending_notice("dev.qos.noticefixture"), Some(42));
        assert_eq!(take_pending_notice("dev.qos.noticefixture"), None);
    }

    #[test]
    fn a_newer_open_replaces_the_notice_for_the_same_package() {
        note_pending_notice("dev.qos.replacefixture", 1);
        note_pending_notice("dev.qos.replacefixture", 2);
        assert_eq!(
            take_pending_notice("dev.qos.replacefixture"),
            Some(2),
            "the live open's notification is the one to retire"
        );
        assert_eq!(take_pending_notice("dev.qos.replacefixture"), None);
    }

    /// How a test, or the headless harness, points at a specific build.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_explicit_binary_override_is_searched_first() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("qos-catalogue");
        std::fs::write(&fake, b"#!/bin/true\n").unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_CATALOGUE_BIN", &fake) };
        let resolved = resolve_catalogue_bin();
        unsafe { std::env::remove_var("QOS_CATALOGUE_BIN") };
        assert_eq!(resolved.as_deref(), Some(fake.as_path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_missing_override_falls_through_rather_than_resolving_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("not-here");
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_CATALOGUE_BIN", &absent) };
        let resolved = resolve_catalogue_bin();
        unsafe { std::env::remove_var("QOS_CATALOGUE_BIN") };
        assert_ne!(
            resolved.as_deref(),
            Some(absent.as_path()),
            "a path that does not exist must not be handed to Command::new"
        );
    }
}
