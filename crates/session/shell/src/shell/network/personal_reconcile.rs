//! Keeping the person's workspace list in step with this device's tree,
//! off the compositor's loop.
//!
//! What the reconcile reads are the local workspace set (created, deleted,
//! renamed or moved workspaces), the list the person's devices share, and
//! a device's shown or hidden choice for a workspace. Every one of them
//! reaches the reconcile through a write to the tree: a local edit, a merged
//! list adopted into it, a choice queued on it from the workspace's menu. So
//! the loop asks for a reconcile only when the tree's write generation moved
//! since the last one read it, which costs it one atomic load, and an idle
//! desktop runs none. The reconcile itself reads the tree and, when
//! something moved, writes it together with the list; it runs on the
//! peer-sync worker, so the loop never waits on another thread's hold of the
//! tree for it. The worker writes its result into the tree and wakes the
//! loop when it did, as the worker's other jobs hand theirs back. Lock
//! order stays owner, then tree, then the list.

use super::{PERSONAL_WORKSPACES_RESTATE_INTERVAL, ShellNetwork, refresh_device_choices};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How soon a reconcile that could not write everything into the list
/// tries again while nothing else moves.
const RETRY_AFTER: Duration = Duration::from_secs(1);

/// What the last reconcile took in, and whether another is queued.
#[derive(Default)]
pub(super) struct State {
    /// A reconcile is queued on the peer-sync worker and has not started:
    /// it covers every change made before it starts.
    pub(super) queued: AtomicBool,
    seen: Mutex<Seen>,
    /// How many reconciles ran.
    #[cfg(test)]
    pub(super) runs: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct Seen {
    /// The tree's write generation the last reconcile read, before any
    /// write of its own: while the tree is still at it, the reconcile has
    /// nothing new to read.
    generation: Option<u64>,
    /// When a reconcile that left part of the list unwritten runs again.
    retry_at: Option<Instant>,
}

impl ShellNetwork {
    /// The frame tick's part: queue a reconcile when the tree was written
    /// since the last one read it, when one left part of the list unwritten,
    /// or when the list is due to be restated. Reads no tree. `in_place`
    /// runs it here instead, as a network the session started no worker
    /// for does too.
    pub(super) fn reconcile_personal_workspaces_if_changed(&self, in_place: bool) {
        if self.personal_workspaces.is_none()
            || self.personal_continuity_user.is_none()
            || self.personal_reconcile.queued.load(Ordering::Acquire)
        {
            return;
        }
        let generation = self.tree.generation();
        let now = Instant::now();
        let moved = self.personal_reconcile.seen.lock().is_ok_and(|seen| {
            seen.generation != Some(generation) || seen.retry_at.is_some_and(|at| now >= at)
        });
        let restate = self.host_identity.is_some()
            && self
                .last_personal_workspaces_snapshot
                .lock()
                .is_ok_and(|last| {
                    last.is_none_or(|at| {
                        now.saturating_duration_since(at) >= PERSONAL_WORKSPACES_RESTATE_INTERVAL
                    })
                });
        if !moved && !restate {
            return;
        }
        if in_place || !self.reconcile_personal_workspaces_off_loop() {
            let _ = self.reconcile_personal_workspaces();
        }
    }

    /// List this device's own workspaces for the person's other devices,
    /// bring in the choices made from a workspace's menu, and send the list
    /// when it changed (or, rarely, when it did not, to repair a lost
    /// snapshot). Waits for the tree: runs on the peer-sync worker, asked by
    /// [`Self::reconcile_personal_workspaces_if_changed`]. Returns whether
    /// it wrote the tree, which the loop then draws.
    pub(super) fn reconcile_personal_workspaces(&self) -> bool {
        #[cfg(test)]
        self.personal_reconcile.runs.fetch_add(1, Ordering::Relaxed);
        let tree = self.tree.clone();
        // Loaded before the read: the read sees every write up to it.
        let generation = tree.generation();
        let (Some(workspaces), Some(here)) = (
            self.personal_workspaces.as_ref(),
            self.host_identity
                .as_ref()
                .map(|identity| identity.device_cert.device_public_key),
        ) else {
            self.note_personal_reconciled(generation, true);
            return false;
        };
        // Read from the owner before the tree is held: the owner's lock
        // comes first.
        let held = self.held_personal_rooms();
        let (mut changed, settle, complete) = {
            let tree = tree.read();
            let (changed, complete) = workspaces.lock().map_or((false, false), |mut workspaces| {
                let changed = workspaces.publish_local(&tree, &held);
                (changed, workspaces.publish_settled())
            });
            let settle = !tree.personal_device_requests.is_empty()
                || (tree.personal_device_choices.is_empty() && !tree.desktop.workspaces.is_empty());
            drop(tree);
            (changed, settle, complete)
        };
        self.note_personal_reconciled(generation, complete);
        if changed || settle {
            let mut adoption = None;
            {
                let mut tree = tree.write();
                if let Ok(mut workspaces) = workspaces.lock() {
                    for (room, device, shown) in std::mem::take(&mut tree.personal_device_requests)
                    {
                        changed |= workspaces.set_shown(room, device, shown);
                    }
                    // Keeping a workspace off this device takes it off at once.
                    adoption = Some(workspaces.adopt_into(&mut tree, &here));
                    refresh_device_choices(&mut tree, &workspaces);
                }
                drop(tree);
            }
            if let Some(adoption) = adoption {
                self.follow_list_off_loop(adoption);
            }
            if changed {
                self.enter_configured_host_rooms_off_loop(None);
            }
        }
        let due = self
            .last_personal_workspaces_snapshot
            .lock()
            .is_ok_and(|last| {
                last.is_none_or(|at| at.elapsed() >= PERSONAL_WORKSPACES_RESTATE_INTERVAL)
            });
        if changed || due {
            self.publish_personal_workspaces();
        }
        changed || settle
    }

    fn note_personal_reconciled(&self, generation: u64, complete: bool) {
        if let Ok(mut seen) = self.personal_reconcile.seen.lock() {
            seen.generation = Some(generation);
            seen.retry_at = (!complete).then(|| {
                let now = Instant::now();
                now.checked_add(RETRY_AFTER).unwrap_or(now)
            });
        }
    }
}
