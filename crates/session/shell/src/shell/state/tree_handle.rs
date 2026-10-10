//! The shared handle to the one [`WorkspaceTree`], with a write generation.
//!
//! Protocol callbacks, the portal, peer sync and the chrome all mutate the
//! same tree. The compositor loop derives state from it (input placements,
//! the app behind each surface, the rooms presence follows) and must know
//! when to derive again without walking the tree on every wakeup to find out.
//! Every write goes through [`WorkspaceTreeHandle::write`], whose guard bumps
//! the generation as it is released, so a writer cannot forget to announce a
//! change: a reader that saw generation `n` and later sees `n` again knows
//! nothing was written in between.

use super::WorkspaceTree;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

/// Shared handle to a single [`WorkspaceTree`], cloned by the compositor,
/// portal and peer-sync so every mutation lands on the same tree.
/// `parking_lot::RwLock` rather than a `Mutex`: readers vastly outnumber
/// writers at ~60 Hz repaint, and an uncontended read lock is ~10 ns.
#[derive(Clone, Default)]
pub struct WorkspaceTreeHandle(Arc<TreeCell>);

/// A handle that does not keep the tree alive.
#[derive(Clone, Default)]
pub struct WeakWorkspaceTreeHandle(Weak<TreeCell>);

#[derive(Default)]
struct TreeCell {
    lock: parking_lot::RwLock<WorkspaceTree>,
    /// Write guards released so far. Bumped while the write lock is still
    /// held, so a reader that loads `n` and then takes the read lock sees
    /// every write up to `n`.
    generation: AtomicU64,
}

/// Exclusive access to the tree; releasing it advances the generation.
#[must_use = "the tree is unlocked as soon as the guard is dropped"]
#[clippy::has_significant_drop]
pub struct WorkspaceTreeWriteGuard<'a> {
    guard: parking_lot::RwLockWriteGuard<'a, WorkspaceTree>,
    generation: &'a AtomicU64,
}

impl WorkspaceTreeHandle {
    /// Wrap `tree` in a new handle at generation 0.
    pub fn new(tree: WorkspaceTree) -> Self {
        Self(Arc::new(TreeCell {
            lock: parking_lot::RwLock::new(tree),
            generation: AtomicU64::new(0),
        }))
    }

    /// Shared read access.
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, WorkspaceTree> {
        self.0.lock.read()
    }

    /// Shared read access, if no writer holds the tree.
    pub fn try_read(&self) -> Option<parking_lot::RwLockReadGuard<'_, WorkspaceTree>> {
        self.0.lock.try_read()
    }

    /// Exclusive access. The generation advances when the guard is dropped,
    /// whether or not anything was changed: a spurious bump costs a reader
    /// one re-derivation, a missed one would leave it stale.
    pub fn write(&self) -> WorkspaceTreeWriteGuard<'_> {
        WorkspaceTreeWriteGuard {
            guard: self.0.lock.write(),
            generation: &self.0.generation,
        }
    }

    /// Exclusive access, if nobody holds the tree.
    pub fn try_write(&self) -> Option<WorkspaceTreeWriteGuard<'_>> {
        Some(WorkspaceTreeWriteGuard {
            guard: self.0.lock.try_write()?,
            generation: &self.0.generation,
        })
    }

    /// How many write guards have been released. Equal values read at two
    /// moments mean the tree was not written in between.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// A weak handle to the same tree.
    pub fn downgrade(&self) -> WeakWorkspaceTreeHandle {
        WeakWorkspaceTreeHandle(Arc::downgrade(&self.0))
    }

    /// Whether both handles share one tree.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for WorkspaceTreeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceTreeHandle")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl WeakWorkspaceTreeHandle {
    /// A weak handle that never upgrades.
    pub const fn new() -> Self {
        Self(Weak::new())
    }

    /// The tree, while some strong handle still holds it.
    pub fn upgrade(&self) -> Option<WorkspaceTreeHandle> {
        self.0.upgrade().map(WorkspaceTreeHandle)
    }

    /// Whether both weak handles point at one tree.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }

    /// Strong handles still holding the tree.
    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
}

impl std::fmt::Debug for WeakWorkspaceTreeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakWorkspaceTreeHandle")
            .field("strong", &self.strong_count())
            .finish()
    }
}

impl std::ops::Deref for WorkspaceTreeWriteGuard<'_> {
    type Target = WorkspaceTree;
    fn deref(&self) -> &WorkspaceTree {
        &self.guard
    }
}

impl std::ops::DerefMut for WorkspaceTreeWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut WorkspaceTree {
        &mut self.guard
    }
}

impl Drop for WorkspaceTreeWriteGuard<'_> {
    fn drop(&mut self) {
        // Runs before `guard` releases the lock.
        self.generation.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_released_write_advances_the_generation() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let start = handle.generation();
        for _ in 0..100 {
            drop(handle.read());
        }
        assert_eq!(handle.generation(), start, "reads never advance it");

        let clone = handle.clone();
        clone.write().desktop.external_revision = 7;
        assert_eq!(
            handle.generation(),
            start.wrapping_add(1),
            "a write through any clone advances the shared generation"
        );

        drop(handle.try_write().expect("nobody holds the tree"));
        assert_eq!(handle.generation(), start.wrapping_add(2));
    }

    #[test]
    fn a_weak_handle_reaches_the_same_tree_and_its_generation() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let weak = handle.downgrade();
        assert_eq!(weak.strong_count(), 1);
        let upgraded = weak.upgrade().expect("the tree is alive");
        assert!(upgraded.ptr_eq(&handle));
        drop(upgraded.write());
        assert_eq!(handle.generation(), 1);
        assert!(weak.ptr_eq(&handle.downgrade()));
        drop(upgraded);
        drop(handle);
        assert!(weak.upgrade().is_none());
        assert!(WeakWorkspaceTreeHandle::new().upgrade().is_none());
    }
}
