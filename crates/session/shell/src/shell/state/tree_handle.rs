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
    /// What authorization last read from the tree; a released write that
    /// changes it announces a policy change.
    policy: parking_lot::Mutex<PolicySnapshot>,
}

/// The parts of the tree an authorization decision reads (TIL-707): each
/// workspace's room, owner, roster, grants, permission profiles, tasks and
/// pending ownership transfer, and the session's room authorities, admitted
/// devices, authenticated persons and stewarded rooms. Strips, records,
/// presence and effect receipts are written on every frame or by the broker
/// itself and decide nothing.
#[derive(Default, PartialEq)]
struct PolicySnapshot {
    workspaces: Vec<WorkspacePolicy>,
    room_authority: std::collections::HashMap<tilo_desktop_model::RoomId, super::RoomAuthority>,
    room_peer_identities: std::collections::HashMap<
        tilo_desktop_model::RoomId,
        std::collections::BTreeMap<String, super::GovernedPeerIdentity>,
    >,
    local_person: Option<[u8; 32]>,
    authenticated_persons: std::collections::HashMap<super::PeerOrigin, [u8; 32]>,
    stewarded_rooms: std::collections::BTreeSet<tilo_desktop_model::RoomId>,
}

#[derive(PartialEq)]
struct WorkspacePolicy {
    id: tilo_desktop_model::WorkspaceId,
    room: tilo_desktop_model::RoomId,
    owner: Option<tilo_desktop_model::UserId>,
    collaborators: Vec<tilo_desktop_model::Collaborator>,
    policy: tilo_action_policy::RoomPolicy,
    sandbox: Vec<collab_types::caps::CapOverride>,
    tasks: Vec<tilo_desktop_model::WorkspaceTask>,
    pending_ownership_transfer: Option<collab_types::OwnershipTransferProposalV1>,
}

impl PolicySnapshot {
    fn of(tree: &WorkspaceTree) -> Self {
        Self {
            workspaces: tree
                .desktop
                .workspaces
                .iter()
                .map(|workspace| WorkspacePolicy {
                    id: workspace.id,
                    room: workspace.room,
                    owner: workspace.owner().cloned(),
                    collaborators: workspace.collaborators.clone(),
                    policy: workspace.policy.clone(),
                    sandbox: workspace.sandbox.clone(),
                    tasks: workspace.tasks.clone(),
                    pending_ownership_transfer: workspace.pending_ownership_transfer.clone(),
                })
                .collect(),
            room_authority: tree.room_authority.clone(),
            room_peer_identities: tree.room_peer_identities.clone(),
            local_person: tree.local_person,
            authenticated_persons: tree.authenticated_persons.clone(),
            stewarded_rooms: tree.stewarded_rooms.clone(),
        }
    }

    /// Whether `tree` still reads as this snapshot. Allocates nothing: the
    /// common write changes none of it.
    fn matches(&self, tree: &WorkspaceTree) -> bool {
        self.workspaces.len() == tree.desktop.workspaces.len()
            && self
                .workspaces
                .iter()
                .zip(&tree.desktop.workspaces)
                .all(|(seen, workspace)| {
                    seen.id == workspace.id
                        && seen.room == workspace.room
                        && seen.owner.as_ref() == workspace.owner()
                        && seen.collaborators == workspace.collaborators
                        && seen.policy == workspace.policy
                        && seen.sandbox == workspace.sandbox
                        && seen.tasks == workspace.tasks
                        && seen.pending_ownership_transfer == workspace.pending_ownership_transfer
                })
            && self.room_authority == tree.room_authority
            && self.room_peer_identities == tree.room_peer_identities
            && self.local_person == tree.local_person
            && self.authenticated_persons == tree.authenticated_persons
            && self.stewarded_rooms == tree.stewarded_rooms
    }
}

/// Exclusive access to the tree; releasing it advances the generation.
#[must_use = "the tree is unlocked as soon as the guard is dropped"]
#[clippy::has_significant_drop]
pub struct WorkspaceTreeWriteGuard<'a> {
    guard: parking_lot::RwLockWriteGuard<'a, WorkspaceTree>,
    generation: &'a AtomicU64,
    policy: &'a parking_lot::Mutex<PolicySnapshot>,
}

impl WorkspaceTreeHandle {
    /// Wrap `tree` in a new handle at generation 0.
    pub fn new(tree: WorkspaceTree) -> Self {
        let policy = parking_lot::Mutex::new(PolicySnapshot::of(&tree));
        Self(Arc::new(TreeCell {
            lock: parking_lot::RwLock::new(tree),
            generation: AtomicU64::new(0),
            policy,
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
            policy: &self.0.policy,
        }
    }

    /// Exclusive access, if nobody holds the tree.
    pub fn try_write(&self) -> Option<WorkspaceTreeWriteGuard<'_>> {
        Some(WorkspaceTreeWriteGuard {
            guard: self.0.lock.try_write()?,
            generation: &self.0.generation,
            policy: &self.0.policy,
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
        // Rosters, roles, grants and the devices a room admitted live here:
        // the collaboration broker re-checks what it admitted on this
        // announcement rather than on a clock (TIL-707). Only a write that
        // changed them announces; most writes are a frame's strips and
        // records, and announcing each kept the broker on a one-second
        // cadence. A reader it wakes takes the read lock once this guard has
        // released it, and the snapshot is replaced before it can look.
        let changed = {
            let mut policy = self.policy.lock();
            let changed = !policy.matches(&self.guard);
            if changed {
                *policy = PolicySnapshot::of(&self.guard);
            }
            changed
        };
        if changed {
            tilo_identity_host::policy_change::announce();
        }
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

    /// TIL-707: the collaboration broker re-checks rosters, roles and
    /// grants on the policy announcement a released write makes when it
    /// changed them, so no writer can change who may do what without it
    /// hearing; and the frame's writes to strips and records, which decide
    /// nothing, do not wake it.
    #[test]
    fn a_write_announces_a_policy_change_only_when_it_changed_policy() {
        use std::time::Duration;
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let workspace = handle.write().append_empty_workspace("Shared");
        let (woke, wakes) = std::sync::mpsc::channel();
        let woke = std::sync::Mutex::new(woke);
        let watch = tilo_identity_host::policy_change::Watch::new(move || {
            let _ = woke.lock().unwrap().send(());
        });

        // A layout-only write: the watch stays armed and unwoken, unless
        // another test of this process announced meanwhile.
        let before = watch.arm();
        handle.write().desktop.external_revision = 9;
        drop(handle.try_write().expect("nobody holds the tree"));
        if tilo_identity_host::policy_change::epoch() == before {
            assert!(
                wakes.recv_timeout(Duration::from_millis(200)).is_err(),
                "a write that changed no policy woke the watch"
            );
        }

        // A new owner: announced.
        let _ = watch.arm();
        let before = tilo_identity_host::policy_change::epoch();
        handle
            .write()
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .unwrap()
            .set_owner("someone");
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("the new owner woke the armed watch");
        assert_ne!(tilo_identity_host::policy_change::epoch(), before);

        // A room this device now stewards: announced.
        let _ = watch.arm();
        let room = handle.read().room_of(workspace).unwrap();
        handle.write().stewarded_rooms.insert(room);
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("stewarding a room woke the armed watch");
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
