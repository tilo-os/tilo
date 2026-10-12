//! How many extra CPU-driver devices may compile a renderer's pipelines right now.
//!
//! Lavapipe compiles a pipeline on its first dispatch, one after another, so a cold first frame
//! spends seconds compiling. Extra devices can take a share each, but every one brings its own
//! driver thread pool and memory. Several windows or apps starting at once, each opening the
//! most devices the machine has cores for, would oversubscribe it many times over.
//!
//! The extra devices are therefore one budget for the user's whole session, shared across its
//! processes: one slot file per core but the renderer's own, each held under an exclusive lock
//! while a device compiles. The locks are released when the warm-up ends or its process exits.

use std::path::{Path, PathBuf};

use advisory_lock::{FileLock, LockMode};

/// Most extra devices one renderer opens. Each opens a device and builds every pipeline before
/// compiling its share, so beyond a few that setup outweighs the split.
const RENDERER_DEVICES: usize = 8;

/// Memory one extra lavapipe device and its compiles occupy, kept free per device.
const DEVICE_BYTES: u64 = 256 << 20;

/// Extra devices this renderer may compile on, released when dropped.
pub struct WarmupSlots {
    locks: Vec<FileLock>,
}

impl WarmupSlots {
    /// How many extra devices the warm-up may open.
    pub fn len(&self) -> usize {
        self.locks.len()
    }

    /// Whether the warm-up compiles on the renderer's own device only.
    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

/// Take free slots of the session's budget, as many as this renderer and the memory allow.
pub fn acquire() -> WarmupSlots {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let want = wanted(system.available_memory());
    let locks = match slot_dir() {
        Some(dir) if want > 0 => take(&dir, budget(cores), want),
        _ => Vec::new(),
    };
    WarmupSlots { locks }
}

/// The session's slots: one per core but the one the renderer compiles on itself.
fn budget(cores: usize) -> usize {
    cores.saturating_sub(1)
}

/// Extra devices one renderer asks for, given the memory available for them.
fn wanted(available_memory: u64) -> usize {
    usize::try_from(available_memory / DEVICE_BYTES)
        .unwrap_or(usize::MAX)
        .min(RENDERER_DEVICES)
}

/// Lock up to `want` free slots among the first `budget` of `dir`.
fn take(dir: &Path, budget: usize, want: usize) -> Vec<FileLock> {
    (0..budget)
        .filter_map(|slot| {
            FileLock::try_open(&dir.join(format!("slot-{slot}")), LockMode::Exclusive).ok()
        })
        .take(want)
        .collect()
}

/// The session's slot directory: the user's runtime directory, shared by every process of
/// theirs whatever environment each was started with, else a per-user temporary directory.
fn slot_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let base = {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let run = PathBuf::from(format!("/run/user/{uid}"));
        if run.is_dir() {
            run
        } else {
            std::env::temp_dir().join(format!("tilo-{uid}"))
        }
    };
    #[cfg(not(unix))]
    let base = std::env::temp_dir().join("tilo");
    let dir = base.join("renderer-warmup");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn the_budget_keeps_one_core_and_memory_bounds_each_renderer() {
        assert_eq!(budget(4), 3, "a four-core board keeps one core for itself");
        assert_eq!(budget(1), 0);
        assert_eq!(wanted(64 * GIB), RENDERER_DEVICES);
        assert_eq!(wanted(600 << 20), 2, "each device needs its memory");
    }

    #[test]
    fn concurrent_renderers_share_one_budget() {
        let dir = tempfile::tempdir().unwrap();
        let budget = budget(26);
        let barrier = std::sync::Barrier::new(26);
        let held: Vec<Vec<FileLock>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..26)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        take(dir.path(), budget, RENDERER_DEVICES)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let total: usize = held.iter().map(Vec::len).sum();
        assert_eq!(
            total, budget,
            "26 renderers on 26 cores open 25 extra devices in all"
        );
        assert!(held.iter().all(|slots| slots.len() <= RENDERER_DEVICES));
    }

    #[test]
    fn released_slots_are_free_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = take(dir.path(), 3, RENDERER_DEVICES);
        assert_eq!(
            first.len(),
            3,
            "a four-core board's first window gets all three"
        );
        assert!(
            take(dir.path(), 3, RENDERER_DEVICES).is_empty(),
            "held slots are not shared"
        );
        drop(first);
        assert_eq!(take(dir.path(), 3, RENDERER_DEVICES).len(), 3);
    }
}
