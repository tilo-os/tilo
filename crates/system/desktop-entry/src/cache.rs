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
//!
//! An entry for a downloaded program ([`crate::downloaded`]) says whether
//! its file is still there, and that file lives outside the application
//! directories. So the folders on the way to each such file are watched
//! too, for that one name: deleting, moving or renaming the file (or a
//! folder above it) reads the scan again, and the entry shows the file is
//! gone without anyone having to try to start it. Each scan hands every
//! entry whose file is gone to the hook the session set with
//! [`on_missing_program`], which can find the file again (Tilo's filesystem
//! knows where a record moved) and point the entry at it.

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
    /// The downloaded programs' folders being watched, and their watches.
    programs: std::sync::Arc<Mutex<ProgramWatches>>,
}

#[derive(Default)]
struct ProgramWatches {
    programs: Vec<PathBuf>,
    _watches: Vec<tilo_files::native::ChangeWatch>,
}

/// Every folder between each program and the root, each with the one name
/// in it that leads to the program.
fn program_paths(scan: &Scan) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = scan
        .entries
        .iter()
        .filter_map(|entry| entry.downloaded.as_ref())
        .map(|downloaded| downloaded.program.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Watch the way to each downloaded program in `scan`, replacing the
/// watches of a previous scan when the programs changed. A change re-reads
/// the scan on the watch's thread.
fn watch_programs(scan: &Scan, slot: &Mutex<ProgramWatches>) {
    let programs = program_paths(scan);
    let mut held = slot.lock_recover();
    if held.programs == programs {
        return;
    }
    let mut watches = Vec::new();
    for program in &programs {
        let mut child = program.as_path();
        while let Some(parent) = child.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            let leads_to = child.to_path_buf();
            let leaf = child == program.as_path();
            // The program itself is read again when it is written; the directories on the
            // way to it only matter when an entry on that way appears, goes or is renamed.
            let watch = if leaf {
                tilo_files::native::watch_changes(parent, move |path| path == leads_to, refresh)
            } else {
                tilo_files::native::watch_presence(parent, move |path| path == leads_to, refresh)
            };
            if let Ok(watch) = watch {
                watches.push(watch);
            }
            if parent.parent().is_none() || Some(parent) == tilo_paths::home().as_deref() {
                break;
            }
            child = parent;
        }
    }
    *held = ProgramWatches {
        programs,
        _watches: watches,
    };
}

/// Read the current scan again, on this thread, now: for a writer in this
/// process that must see its own change (an entry it just wrote), and for a
/// downloaded program's file that changed. Reads the disk; never call it on
/// the compositor's loop.
pub fn refresh() {
    let scan = cell()
        .lock_recover()
        .as_ref()
        .map(|cached| cached.scan.clone());
    if let Some(scan) = scan {
        scan.refresh();
    }
}

/// Called with each downloaded program's entry whose file is gone, after
/// every scan.
type MissingHook = Box<dyn Fn(&crate::DesktopEntry) + Send + Sync>;

fn missing_hook() -> &'static OnceLock<MissingHook> {
    static HOOK: OnceLock<MissingHook> = OnceLock::new();
    &HOOK
}

/// Have `hook` see each downloaded program's entry whose file is gone,
/// after every scan, on the thread that scanned. It must not block: it
/// hands the entry to a worker. Set once per process; a later call is
/// ignored.
pub fn on_missing_program(hook: impl Fn(&crate::DesktopEntry) + Send + Sync + 'static) {
    let _ = missing_hook().set(Box::new(hook));
}

fn report_missing(scan: &Scan) {
    let Some(hook) = missing_hook().get() else {
        return;
    };
    for entry in &scan.entries {
        if entry
            .downloaded
            .as_ref()
            .is_some_and(|downloaded| !downloaded.present)
        {
            hook(entry);
        }
    }
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
    let programs = std::sync::Arc::new(Mutex::new(ProgramWatches::default()));
    let follow = std::sync::Arc::clone(&programs);
    watched.on_change(move || {
        REVISION.fetch_add(1, Ordering::AcqRel);
        // Looked up rather than captured: a captured handle would keep the
        // value (and its worker) alive after the cache let go of it.
        let current = cell()
            .lock_recover()
            .as_ref()
            .filter(|cached| std::sync::Arc::ptr_eq(&cached.programs, &follow))
            .map(|cached| cached.scan.get());
        if let Some(scan) = current {
            watch_programs(&scan, &follow);
            report_missing(&scan);
        }
    });
    let scan = (*watched.get()).clone();
    watch_programs(&scan, &programs);
    report_missing(&scan);
    *guard = Some(Cached {
        dirs,
        desktop,
        scan: watched,
        programs,
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
