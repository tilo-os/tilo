//! Announcing this device's room changes to the room's peers as they land.

use super::{NeighbourMsg, RoomId, ShellNetwork};

/// Upper bound between full projections of every room while nothing the
/// push trigger watches has moved. Most local edits move the desktop
/// document's heads and are pushed the same tick; this catches the few that
/// only reach a room document through projection (a record index, say).
pub(super) const ROOM_PUSH_FALLBACK_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// What [`ShellNetwork::push_room_changes`] last saw and last announced.
#[derive(Default)]
pub(super) struct RoomPush {
    /// Desktop-document heads, peer-merge epoch and external revision at
    /// the last full check: when none moved, no room can have changed
    /// locally, so a tick costs one heads read.
    trigger: Option<(Vec<automerge::ChangeHash>, u64, u64)>,
    /// When every room was last projected.
    last_full: Option<std::time::Instant>,
    /// Each room's document heads as last announced (or first seen, which
    /// announces nothing: a peer that lacks older history pulls it).
    announced: std::collections::HashMap<RoomId, Vec<automerge::ChangeHash>>,
    /// The tree's write generation right after the last full projection.
    /// Projection reads nothing but the tree, and every tree write bumps the
    /// generation, so while it holds there is nothing to project.
    projected_generation: Option<u64>,
    /// The generation right after a check that found no trigger moved:
    /// until it changes, only the fallback projection has anything to do.
    checked_generation: Option<u64>,
    /// When a tick found the tree held by another thread: the push runs
    /// again at [`ROOM_PUSH_RETRY`] rather than waiting for the lock.
    pub(super) retry_at: Option<std::time::Instant>,
}

/// How soon a room push that found the tree held by another thread tries
/// again: about a frame, so a change still goes out promptly, while the
/// loop never waits on that thread's write.
const ROOM_PUSH_RETRY: std::time::Duration = std::time::Duration::from_millis(8);

impl ShellNetwork {
    /// Announce every room document change since the last announcement as a
    /// `SyncResp`, so peers see an edit, an opened, moved or closed tab, a
    /// rename within one network round trip instead of at their next pull.
    /// At most one message per room per tick. Cheap when idle: rooms are
    /// projected only when the desktop document, the peer-merge epoch or the
    /// external revision moved, or [`ROOM_PUSH_FALLBACK_INTERVAL`] passed,
    /// and never while the tree is unwritten since the last projection.
    /// Runs on the compositor's loop, so it never waits for the tree: held
    /// by another thread, the push retries at [`Self::next_deadline`].
    /// A batch that landed from a peer is re-announced once, which reaches
    /// a member that missed the original; the echo changes nothing for
    /// those that already hold it, so it does not repeat.
    pub(super) fn push_room_changes(&self) {
        let Some(handle) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return;
        };
        let Ok(mut push) = self.room_push.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let generation = handle.generation();
        let due = push
            .last_full
            .is_none_or(|last| now.duration_since(last) >= ROOM_PUSH_FALLBACK_INTERVAL);
        if push.projected_generation == Some(generation)
            || (!due && push.checked_generation == Some(generation))
        {
            push.retry_at = None;
            return;
        }
        // Projecting writes the room documents. Another thread holding the
        // tree (a worker, the portal, a protocol callback) would hold the
        // loop for as long as it keeps it; the push runs again shortly
        // instead ([`Self::next_deadline`]).
        let Some(mut tree) = handle.try_write() else {
            push.retry_at = Some(now.checked_add(ROOM_PUSH_RETRY).unwrap_or(now));
            return;
        };
        push.retry_at = None;
        // This guard's release is the next bump; a later one is a write
        // made after this projection.
        let projected = handle.generation().wrapping_add(1);
        let mut outgoing = Vec::new();
        let trigger = (
            tree.current_heads(),
            tree.desktop.room_epoch,
            tree.desktop.external_revision,
        );
        let moved = push.trigger.as_ref() != Some(&trigger);
        if !moved && !due {
            push.checked_generation = Some(projected);
            return;
        }
        if moved {
            // Once this activity settles, ask the rooms for what this shell
            // or a peer missed of it.
            self.schedule_repair_pull();
        }
        push.trigger = Some(trigger);
        push.last_full = Some(now);
        push.projected_generation = Some(projected);
        let rooms = tree.rooms();
        push.announced.retain(|room, _| rooms.contains(room));
        for room in rooms {
            if tree.requires_native_owner(room) {
                continue;
            }
            tree.sync_room_doc(room);
            let heads = tree.room_heads(room);
            if heads.is_empty() {
                continue;
            }
            let Some(announced) = push.announced.get(&room) else {
                push.announced.insert(room, heads);
                continue;
            };
            if *announced == heads {
                continue;
            }
            let changes = tree
                .room_docs
                .get_mut(&room)
                .map(|doc| {
                    doc.get_changes(announced)
                        .into_iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            push.announced.insert(room, heads);
            if !changes.is_empty() {
                outgoing.push(NeighbourMsg::SyncResp {
                    changes,
                    room: room.to_string(),
                });
            }
        }
        drop(tree);
        drop(push);
        for msg in outgoing {
            let _ = self.publish_message(msg);
        }
    }
}
