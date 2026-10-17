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
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// A wait for the tree longer than this is reported. The compositor thread
/// reads the tree several times a tick, so one wait this long is a stalled
/// frame, and it is the waiter that has to say who held the lock.
pub const SLOW_TREE_WAIT: Duration = Duration::from_millis(20);

/// Who holds the tree for writing, and since when.
#[derive(Clone, Copy)]
struct Holder {
    at: &'static Location<'static>,
    since: Instant,
    thread: std::thread::ThreadId,
}

/// The `SLOW-TREE-LOCK` line for a wait of `elapsed` by `waiter`, when it is
/// long enough to report. `holder` is the writer that held the lock when the
/// wait began, with how long it had held it; without one the waiter was
/// queued behind readers.
fn slow_wait_report(
    kind: &str,
    waiter: &Location<'_>,
    elapsed: Duration,
    holder: Option<(&Location<'_>, Duration)>,
) -> Option<String> {
    if elapsed < SLOW_TREE_WAIT {
        return None;
    }
    let held_by = holder.map_or_else(
        || "readers".to_owned(),
        |(at, held)| format!("writer {at} (held {held:?} at the start of the wait)"),
    );
    Some(format!(
        "SLOW-TREE-LOCK {kind} at {waiter} waited={elapsed:?} held_by={held_by}"
    ))
}

/// The newest policy revision handed out, across every tree of the process.
static POLICY_REVISION: AtomicU64 = AtomicU64::new(0);

/// A policy revision later than every one handed out before.
pub fn next_policy_revision() -> u64 {
    POLICY_REVISION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
}

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
    /// The writer holding `lock` now, for a waiter that has to name it.
    holder: parking_lot::Mutex<Option<Holder>>,
    /// The policy revision the current writer promised a launch it started,
    /// which its release then publishes; 0 when it promised none.
    reserved_revision: AtomicU64,
}

/// The parts of the tree an authorization decision reads: each
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
    holder: &'a parking_lot::Mutex<Option<Holder>>,
    reserved_revision: &'a AtomicU64,
    /// The workspace showing when the guard was taken, and where it was taken:
    /// a write that moves the desktop to another workspace is logged with
    /// its writer, since the active workspace has several.
    active_on_entry: tilo_desktop_model::WorkspaceId,
    taken_at: &'static Location<'static>,
}

impl WorkspaceTreeHandle {
    /// Wrap `tree` in a new handle at generation 0.
    pub fn new(tree: WorkspaceTree) -> Self {
        let policy = parking_lot::Mutex::new(PolicySnapshot::of(&tree));
        Self(Arc::new(TreeCell {
            lock: parking_lot::RwLock::new(tree),
            generation: AtomicU64::new(0),
            policy,
            holder: parking_lot::Mutex::new(None),
            reserved_revision: AtomicU64::new(0),
        }))
    }

    /// Shared read access. A wait of [`SLOW_TREE_WAIT`] or more is reported
    /// with the writer that held the tree.
    #[track_caller]
    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, WorkspaceTree> {
        if let Some(guard) = self.0.lock.try_read() {
            return guard;
        }
        let waiter = Location::caller();
        let holder = self.holder_now();
        let started = Instant::now();
        let guard = self.0.lock.read();
        Self::report_wait("read", waiter, started.elapsed(), holder);
        guard
    }

    /// Shared read access, if no writer holds the tree.
    pub fn try_read(&self) -> Option<parking_lot::RwLockReadGuard<'_, WorkspaceTree>> {
        self.0.lock.try_read()
    }

    /// Exclusive access. The generation advances when the guard is dropped,
    /// whether or not anything was changed: a spurious bump costs a reader
    /// one re-derivation, a missed one would leave it stale.
    #[track_caller]
    pub fn write(&self) -> WorkspaceTreeWriteGuard<'_> {
        let waiter = Location::caller();
        let guard = if let Some(guard) = self.0.lock.try_write() {
            guard
        } else {
            let holder = self.holder_now();
            let started = Instant::now();
            let guard = self.0.lock.write();
            Self::report_wait("write", waiter, started.elapsed(), holder);
            guard
        };
        self.held(guard, waiter)
    }

    /// Exclusive access, if nobody holds the tree.
    #[track_caller]
    pub fn try_write(&self) -> Option<WorkspaceTreeWriteGuard<'_>> {
        let guard = self.0.lock.try_write()?;
        Some(self.held(guard, Location::caller()))
    }

    fn held<'a>(
        &'a self,
        guard: parking_lot::RwLockWriteGuard<'a, WorkspaceTree>,
        at: &'static Location<'static>,
    ) -> WorkspaceTreeWriteGuard<'a> {
        *self.0.holder.lock() = Some(Holder {
            at,
            since: Instant::now(),
            thread: std::thread::current().id(),
        });
        WorkspaceTreeWriteGuard {
            active_on_entry: guard.desktop.active,
            taken_at: at,
            guard,
            generation: &self.0.generation,
            policy: &self.0.policy,
            holder: &self.0.holder,
            reserved_revision: &self.0.reserved_revision,
        }
    }

    fn holder_now(&self) -> Option<Holder> {
        *self.0.holder.lock()
    }

    fn report_wait(kind: &str, waiter: &Location<'_>, elapsed: Duration, holder: Option<Holder>) {
        let holder = holder.map(|holder| (holder.at, holder.since.elapsed()));
        if let Some(line) = slow_wait_report(kind, waiter, elapsed, holder) {
            eprintln!("{line}");
        }
    }

    /// Where the writer holding the tree took it, if one does.
    pub fn writer_location(&self) -> Option<&'static Location<'static>> {
        self.holder_now().map(|holder| holder.at)
    }

    /// How many write guards have been released. Equal values read at two
    /// moments mean the tree was not written in between.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// The revision of the authorization state the tree holds now.
    ///
    /// Asked by the thread that holds the tree for writing (a launch started
    /// in the middle of a write, such as a restored window opening while the
    /// frame updates the tree), this cannot wait for the read lock: that
    /// thread would wait on itself. The state it means is the one its write
    /// is building, which has no revision until the write is released. So
    /// the write is promised a fresh revision, which its release assigns and
    /// publishes even when it changed no policy, and a launch waiting for
    /// that publication is not left waiting for a save that never comes.
    pub fn policy_revision(&self) -> u64 {
        if self.held_by_this_thread() {
            return self.reserve_policy_revision();
        }
        self.read().policy_revision
    }

    fn held_by_this_thread(&self) -> bool {
        self.holder_now()
            .is_some_and(|holder| holder.thread == std::thread::current().id())
    }

    /// Only the thread holding the write lock calls this, so the check and
    /// the store cannot race another writer.
    fn reserve_policy_revision(&self) -> u64 {
        let reserved = self.0.reserved_revision.load(Ordering::Acquire);
        if reserved != 0 {
            return reserved;
        }
        let fresh = next_policy_revision();
        self.0.reserved_revision.store(fresh, Ordering::Release);
        fresh
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
        if self.guard.desktop.active != self.active_on_entry {
            tracing::info!(
                target: "tilo::workspace",
                before = ?self.active_on_entry,
                after = ?self.guard.desktop.active,
                writer = %self.taken_at,
                "the active workspace changed"
            );
        }
        self.generation.fetch_add(1, Ordering::Release);
        *self.holder.lock() = None;
        // Rosters, roles, grants and the devices a room admitted live here:
        // the collaboration broker re-checks what it admitted on this
        // announcement rather than on a clock. Only a write that
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
        let reserved = self.reserved_revision.swap(0, Ordering::AcqRel);
        if changed || reserved != 0 {
            // A later state than any published or captured before it. Taken
            // while the write lock is held, so a reader sees this value with
            // the state it names. A launch made during this write already
            // captured the reserved one, so that is the revision to publish.
            self.guard.policy_revision = if reserved == 0 {
                next_policy_revision()
            } else {
                reserved
            };
            // And the save that publishes it, submitted now rather than left
            // to whichever caller mutated the tree remembering to save.
            self.guard.queue_save();
        }
        if changed {
            tilo_identity_host::policy_change::announce();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_wait_for_the_tree_names_the_writer_that_held_it() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        assert!(handle.writer_location().is_none());
        let held_at = {
            let guard = handle.write();
            let at = handle.writer_location().expect("a writer holds the tree");
            assert_eq!(at.file(), file!(), "the holder is where write() was called");
            assert!(handle.try_read().is_none());
            drop(guard);
            at
        };
        assert!(
            handle.writer_location().is_none(),
            "releasing the tree clears its holder"
        );
        let waiter = Location::caller();
        let held = Duration::from_millis(30);
        assert!(
            slow_wait_report(
                "read",
                waiter,
                SLOW_TREE_WAIT.saturating_sub(Duration::from_millis(1)),
                None
            )
            .is_none(),
            "a short wait is not a stall"
        );
        let line = slow_wait_report("read", waiter, SLOW_TREE_WAIT, Some((held_at, held)))
            .expect("a wait of the threshold is reported");
        assert!(line.starts_with("SLOW-TREE-LOCK read at "), "{line}");
        assert!(line.contains(&format!("writer {held_at}")), "{line}");
        let queued = slow_wait_report("write", waiter, SLOW_TREE_WAIT, None).unwrap();
        assert!(queued.ends_with("held_by=readers"), "{queued}");
    }

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
    fn only_a_write_that_changes_authorization_advances_the_policy_revision() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let start = handle.policy_revision();
        handle.write().desktop.external_revision = 11;
        assert_eq!(
            handle.policy_revision(),
            start,
            "a layout write is no new policy"
        );
        let _ = handle.write().append_empty_workspace("Granted");
        let first = handle.policy_revision();
        assert!(first > start, "a new workspace is a new policy");
        handle.write().desktop.workspaces[0]
            .sandbox
            .push(collab_types::caps::CapOverride::grant(
                collab_types::Subject::Principal(collab_types::Principal::App(
                    "so.tilo.revision".into(),
                )),
                collab_types::Scope::FsPath("/disposable/revision".into()),
                collab_types::Access::Read,
            ));
        assert!(handle.policy_revision() > first, "a grant is a new policy");
    }

    /// #4022: the collaboration broker re-checks rosters, roles and
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
        // another test of this process announced meanwhile. An announcement
        // counts the epoch up before it wakes, so whether one came is read
        // after the wait: read before it, one landing during the wait woke
        // the watch and failed the test in a parallel run.
        let before = watch.arm();
        handle.write().desktop.external_revision = 9;
        drop(handle.try_write().expect("nobody holds the tree"));
        let woke = wakes.recv_timeout(Duration::from_millis(200)).is_ok();
        assert!(
            !woke || tilo_identity_host::policy_change::epoch() != before,
            "a write that changed no policy woke the watch"
        );

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

    /// A launch started while its own thread holds the tree for writing (a
    /// restored window opening during the frame's update) asks for the
    /// policy revision. It used to wait for the read lock its own thread
    /// held, and the compositor stopped. It now gets the revision the write
    /// will publish, without waiting.
    #[test]
    fn the_writer_asking_for_the_policy_revision_gets_the_one_its_release_publishes() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let before = handle.policy_revision();
        let writer = handle.clone();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let guard = writer.write();
            let promised = writer.policy_revision();
            let again = writer.policy_revision();
            drop(guard);
            let _ = done.send((promised, again));
        });
        let (promised, again) = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("asking for the policy revision while writing must not wait on the write");
        assert_eq!(promised, again, "one write promises one revision");
        assert!(
            promised > before,
            "the promise is newer than any published state"
        );
        assert_eq!(
            handle.policy_revision(),
            promised,
            "releasing the write assigns the revision it promised, though no policy changed"
        );
        handle.write().desktop.external_revision = 3;
        assert_eq!(
            handle.policy_revision(),
            promised,
            "a later write that promised nothing and changed no policy keeps it"
        );
    }

    /// Another thread asking while the tree is written waits for the write
    /// and sees the state it leaves; only the writer itself is answered from
    /// the promise.
    #[test]
    fn another_thread_asking_during_a_write_waits_for_its_release() {
        let handle = WorkspaceTreeHandle::new(WorkspaceTree::default());
        let mut guard = handle.write();
        let reader = handle.clone();
        let asked = std::thread::spawn(move || reader.policy_revision());
        let _ = guard.append_empty_workspace("Granted");
        std::thread::sleep(Duration::from_millis(50));
        drop(guard);
        let seen = asked.join().unwrap();
        assert_eq!(
            seen,
            handle.policy_revision(),
            "the reader saw the released state"
        );
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
