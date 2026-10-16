//! Memoising the scan.
//!
//! The palette queries the installed-application list on every
//! keystroke, and a full scan is ~200 files on a normal desktop. That
//! is cheap enough to do occasionally and far too expensive to do at
//! 60 Hz, so the result is memoised — with two independent ways to
//! notice it is stale:
//!
//! - a short TTL, so an app installed in another terminal shows up
//!   while the user is still typing its name; and
//! - a **fingerprint** of the search directories' modification times,
//!   checked when the TTL expires. Installing or removing an
//!   application changes the mtime of the directory its `.desktop` file
//!   lives in, so the common case — TTL expired, nothing installed
//!   since — costs a handful of `stat` calls instead of a re-parse.
//!
//! The fingerprint is a heuristic and is only ever allowed to *skip*
//! work, never to extend the answer's life: editing a file in place
//! without touching its directory is missed by the fingerprint and
//! caught by the next TTL expiry.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::scan::{Scan, scan_dirs};
use tilo_lock::MutexRecover as _;

/// How long a scan is reused before the directories are re-examined.
pub const SCAN_TTL: Duration = Duration::from_secs(5);

struct Cached {
    scan: Scan,
    at: Instant,
    fingerprint: Vec<(PathBuf, Option<SystemTime>)>,
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
pub fn cached_scan_snapshot() -> (u64, Scan) {
    let dirs = crate::scan::application_dirs();
    let desktop = crate::scan::current_desktops();
    let mut guard = cell().lock_recover();

    if let Some(cached) = guard.as_mut() {
        if cached.at.elapsed() < SCAN_TTL {
            return (REVISION.load(Ordering::Acquire), cached.scan.clone());
        }
        let fingerprint = fingerprint(&dirs);
        if fingerprint == cached.fingerprint {
            // Nothing was installed or removed. Push the deadline out
            // rather than re-reading two hundred files.
            cached.at = Instant::now();
            return (REVISION.load(Ordering::Acquire), cached.scan.clone());
        }
    }

    let fingerprint = fingerprint(&dirs);
    let scan = scan_dirs(&dirs, &desktop);
    *guard = Some(Cached {
        scan: scan.clone(),
        at: Instant::now(),
        fingerprint,
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

/// Drop the memoised scan. Called after something is known to have
/// changed — a Flatpak install finishing, say — and by tests, which
/// share this process-wide state.
pub fn invalidate() {
    let mut guard = cell().lock_recover();
    *guard = None;
    REVISION.fetch_add(1, Ordering::Release);
    drop(guard);
}

fn fingerprint(dirs: &[PathBuf]) -> Vec<(PathBuf, Option<SystemTime>)> {
    dirs.iter()
        .map(|dir| {
            let mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
            (dir.clone(), mtime)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_fingerprints_track_watched_directories_and_invalidation() {
        // Phase `the_cache_is_warm_on_the_second_call_and_cold_after_invalidate`.
        // Two calls in a row must not re-read the filesystem, and
        // `invalidate` must undo that.
        {
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
        // Phase `a_fingerprint_notices_a_new_entry_in_a_watched_directory`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let dirs = vec![tmp.path().to_path_buf()];
            let before = fingerprint(&dirs);
            // Some CI filesystems expose directory mtimes at coarse granularity.
            // Keep mutating the directory until its timestamp advances, but put a
            // firm bound on the retry: production deliberately remains one cheap
            // `stat` per application directory and the TTL is its fallback.
            let deadline = tilo_test_env::hang_deadline();
            let mut attempt = 0u32;
            loop {
                std::fs::write(tmp.path().join(format!("entry-{attempt}.desktop")), []).unwrap();
                let after = fingerprint(&dirs);
                if after != before {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "directory mtime did not advance before the retry deadline"
                );
                attempt += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Phase `a_missing_directory_fingerprints_as_absent_rather_than_failing`.
        {
            let fp = fingerprint(&[PathBuf::from("/nonexistent/tilo/applications")]);
            assert_eq!(fp.len(), 1);
            assert!(fp[0].1.is_none());
        }
    }
}
