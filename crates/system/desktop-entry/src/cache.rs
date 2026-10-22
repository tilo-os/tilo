//! Memoising the scan.
//!
//! The palette queries the installed-application list on every keystroke,
//! and the compositor's loop asks for it while it saves the desktop; a full
//! scan is ~200 files on a normal desktop. The loop must never wait on the
//! disk, not for the scan and not for a `stat` asking whether the scan is
//! stale. So the scan is a [`Watched`] value: read once, then again on a
//! worker whenever an application directory changes (an app installed or
//! removed, an entry edited in place). A caller only ever takes the latest
//! scan.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::scan::{Scan, scan_dirs};
use tilo_files::native::Watched;
use tilo_lock::MutexRecover as _;

/// The scan of one environment's application directories, kept current.
struct Cached {
    dirs: Vec<PathBuf>,
    desktop: Vec<String>,
    scan: Watched<Scan>,
}

fn cell() -> &'static Mutex<Option<Cached>> {
    static CELL: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}
static REVISION: AtomicU64 = AtomicU64::new(0);

/// The host's application entries, memoised.
///
/// Cloning the scan on every call is deliberate: the alternative hands
/// a lock guard to a caller that then does UI work under it, and the
/// list is a few hundred small structs.
pub fn cached_scan() -> Scan {
    cached_scan_snapshot().1
}

/// Generation and records observed under the same cache lock. Filling a cold
/// cache is a valid snapshot, not an invalidation racing its read.
///
/// Only the first call for an environment scans on the calling thread (the
/// session asks at startup); every later one returns the worker's latest
/// scan without touching the disk.
pub fn cached_scan_snapshot() -> (u64, Scan) {
    let dirs = crate::scan::application_dirs();
    let desktop = crate::scan::current_desktops();
    let mut guard = cell().lock_recover();
    if let Some(cached) = guard.as_ref()
        && cached.dirs == dirs
        && cached.desktop == desktop
    {
        return (
            REVISION.load(Ordering::Acquire),
            (*cached.scan.get()).clone(),
        );
    }
    let read_dirs = dirs.clone();
    let read_desktop = desktop.clone();
    let read = move || scan_dirs(&read_dirs, &read_desktop);
    let Ok(watched) = Watched::start("desktop-entries", dirs.clone(), read) else {
        // Unwatchable directories are scanned per call, never served stale.
        *guard = None;
        let generation = REVISION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        return (generation, scan_dirs(&dirs, &desktop));
    };
    watched.on_change(|| {
        REVISION.fetch_add(1, Ordering::AcqRel);
    });
    let scan = (*watched.get()).clone();
    *guard = Some(Cached {
        dirs,
        desktop,
        scan: watched,
    });
    let generation = REVISION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    drop(guard);
    (generation, scan)
}

/// Current desktop-entry source generation, without cloning the scan.
#[must_use]
pub fn cached_scan_generation() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// Drop the memoised scan, so the next call scans again on its own thread.
/// For tests, which share this process-wide state and change the
/// environment under it; a live session follows its directories by itself.
pub fn invalidate() {
    let mut guard = cell().lock_recover();
    *guard = None;
    REVISION.fetch_add(1, Ordering::Release);
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_is_warm_after_the_first_call_and_cold_after_invalidate() {
        invalidate();
        let (cold_generation, first) = cached_scan_snapshot();
        assert_eq!(
            cold_generation,
            cached_scan_generation(),
            "filling a cold cache produces a current snapshot"
        );
        let (warm_generation, second) = cached_scan_snapshot();
        assert_eq!(warm_generation, cold_generation);
        assert_eq!(
            first.entries.len(),
            second.entries.len(),
            "a warm read returns the same list"
        );
        invalidate();
        let third = cached_scan();
        assert_eq!(first.entries.len(), third.entries.len());
    }
}
