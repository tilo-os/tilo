//! Memoising the scan. The palette asks for the installed-application list on
//! every keystroke and a full scan reads ~200 files, so the loop must never
//! wait on the disk: the scan is a [`Watched`] value, read once and again on a
//! worker whenever an application directory changes. A caller only ever takes
//! the latest scan.
//!
//! A downloaded program's file lives outside those directories, so the folders
//! on the way to it are watched too: deleting, moving or renaming it reads the
//! scan again, and each scan hands every entry whose file is gone to the hook
//! the session set with [`on_missing_program`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::scan::{Scan, scan_dirs};
use tilo_files::native::Watched;
use tilo_lock::MutexRecover as _;

struct Cached {
    dirs: Vec<PathBuf>,
    desktop: Vec<String>,
    locales: Vec<String>,
    scan: Watched<Scan>,
    programs: std::sync::Arc<Mutex<ProgramWatches>>,
}

#[derive(Default)]
struct ProgramWatches {
    programs: Vec<PathBuf>,
    _watches: Vec<tilo_files::native::ChangeWatch>,
}

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

/// Watch the way to each downloaded program, replacing the previous watches
/// when the programs changed.
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

/// Read the current scan again, on this thread, now: for a writer that must
/// see its own change, and for a downloaded program's file that changed.
/// Reads the disk; never call it on the compositor's loop.
pub fn refresh() {
    let scan = cell()
        .lock_recover()
        .as_ref()
        .map(|cached| cached.scan.clone());
    if let Some(scan) = scan {
        scan.refresh();
    }
}

type MissingHook = Box<dyn Fn(&crate::DesktopEntry) + Send + Sync>;

fn missing_hook() -> &'static OnceLock<MissingHook> {
    static HOOK: OnceLock<MissingHook> = OnceLock::new();
    &HOOK
}

/// Have `hook` see each downloaded program's entry whose file is gone, after
/// every scan, on the thread that scanned. It must not block: it hands the
/// entry to a worker. Set once per process; a later call is ignored.
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

/// The host's application entries, memoised, named in the interface language
/// `locales` names, which is part of what is cached: a tool that asked in
/// another language is read again rather than served the wrong one. Cloning
/// the scan on every call is deliberate: a caller doing UI work must not hold
/// the lock.
pub fn cached_scan(locales: &[String]) -> Scan {
    cached_scan_snapshot(locales).1
}

/// The generation and records observed under the same cache lock, so filling a
/// cold cache is a valid snapshot, not an invalidation racing its read.
pub fn cached_scan_snapshot(locales: &[String]) -> (u64, Scan) {
    let dirs = crate::scan::application_dirs();
    let desktop = crate::scan::current_desktops();
    let mut guard = cell().lock_recover();
    if let Some(cached) = guard.as_ref()
        && cached.dirs == dirs
        && cached.desktop == desktop
        && cached.locales == locales
    {
        return (
            REVISION.load(Ordering::Acquire),
            (*cached.scan.get()).clone(),
        );
    }
    let read_dirs = dirs.clone();
    let read_desktop = desktop.clone();
    let read_locales = locales.to_vec();
    let read = move || scan_dirs(&read_dirs, &read_desktop, &read_locales);
    let Ok(watched) = Watched::start("desktop-entries", dirs.clone(), read) else {
        // Unwatchable directories are scanned per call, never served stale.
        *guard = None;
        drop(guard);
        return unwatched_snapshot(dirs, desktop, locales.to_vec(), scan_dirs);
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
        locales: locales.to_vec(),
        scan: watched,
        programs,
    });
    let generation = REVISION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    drop(guard);
    (generation, scan)
}

/// The last scan of directories that could not be watched, with the
/// generation it was served under.
struct Unwatched {
    dirs: Vec<PathBuf>,
    desktop: Vec<String>,
    locales: Vec<String>,
    scan: Scan,
    generation: u64,
}

fn unwatched() -> &'static Mutex<Option<Unwatched>> {
    static UNWATCHED: OnceLock<Mutex<Option<Unwatched>>> = OnceLock::new();
    UNWATCHED.get_or_init(|| Mutex::new(None))
}

/// Scan `dirs`, which cannot be watched, on this call. The generation moves
/// only when the scan differs from the last one served, so an index built from
/// a scan stays current until the entries change.
fn unwatched_snapshot(
    dirs: Vec<PathBuf>,
    desktop: Vec<String>,
    locales: Vec<String>,
    scan: impl FnOnce(&[PathBuf], &[String], &[String]) -> Scan,
) -> (u64, Scan) {
    let scan = scan(&dirs, &desktop, &locales);
    let mut last = unwatched().lock_recover();
    if let Some(last) = last.as_ref()
        && last.dirs == dirs
        && last.desktop == desktop
        && last.locales == locales
        && last.scan == scan
    {
        return (last.generation, scan);
    }
    let generation = REVISION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    *last = Some(Unwatched {
        dirs,
        desktop,
        locales,
        scan: scan.clone(),
        generation,
    });
    drop(last);
    (generation, scan)
}

#[must_use]
pub fn cached_scan_generation() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// Drop the memoised scan, so the next call scans again on its own thread.
/// For tests, which share this process-wide state.
pub fn invalidate() {
    let mut guard = cell().lock_recover();
    *guard = None;
    REVISION.fetch_add(1, Ordering::Release);
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Directories that cannot be watched are scanned on every call, and the
    /// generation moves only when what it found changed.
    #[test]
    fn an_unwatched_scan_keeps_its_generation_until_its_entries_change() {
        let dirs = vec![PathBuf::from("/nonexistent/tilo-unwatched/applications")];
        let desktop = vec!["Tilo".to_owned()];
        let locales = vec!["de_DE".to_owned()];
        let empty = |_: &[PathBuf], _: &[String], _: &[String]| Scan::default();
        let (first, _) = unwatched_snapshot(dirs.clone(), desktop.clone(), locales.clone(), empty);
        let (again, _) = unwatched_snapshot(dirs.clone(), desktop.clone(), locales.clone(), empty);
        assert_eq!(again, first, "an unchanged scan is the same generation");
        let (other_language, _) =
            unwatched_snapshot(dirs.clone(), desktop.clone(), Vec::new(), empty);
        assert_ne!(
            other_language, first,
            "another language's names are another scan"
        );
        let one_hidden = |_: &[PathBuf], _: &[String], _: &[String]| Scan {
            hidden: vec!["org.example.Gone.desktop".to_owned()],
            ..Scan::default()
        };
        let (changed, _) = unwatched_snapshot(dirs, desktop, locales, one_hidden);
        assert_ne!(changed, first, "a changed scan is a new generation");
    }

    #[test]
    fn the_scan_is_warm_after_the_first_call_and_cold_after_invalidate() {
        invalidate();
        let (cold_generation, first) = cached_scan_snapshot(&[]);
        assert_eq!(
            cold_generation,
            cached_scan_generation(),
            "filling a cold cache produces a current snapshot"
        );
        let (warm_generation, second) = cached_scan_snapshot(&[]);
        assert_eq!(warm_generation, cold_generation);
        assert_eq!(
            first.entries.len(),
            second.entries.len(),
            "a warm read returns the same list"
        );
        invalidate();
        let third = cached_scan(&[]);
        assert_eq!(first.entries.len(), third.entries.len());
    }
}
