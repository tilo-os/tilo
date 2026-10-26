use super::pane::{RoomActivityPane, RoomScope};
use super::{ActivityTree, HistoryRoom};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tilo_sort::CompactSort as _;
use ui::time::SystemTime;
use ui::widget::{HistoryEntry, HistoryEntryId};
#[derive(Clone, Debug)]
pub(super) struct ReplayGroup {
    pub(super) tabs: Vec<String>,
    pub(super) active: usize,
    pub(super) width_permille: u32,
}

#[derive(Clone, Debug)]
pub(super) struct ReplayWorkspace {
    pub(super) name: String,
    pub(super) groups: Vec<ReplayGroup>,
}

#[derive(Clone, Debug)]
pub(super) struct WorkspaceReplayFrame {
    pub(super) entry_id: HistoryEntryId,
    pub(super) author: ui::widget::HistoryAuthor,
    pub(super) summary: String,
    pub(super) timestamp: SystemTime,
    pub(super) workspaces: Vec<ReplayWorkspace>,
    pub(super) pointers: Vec<ReplayPointer>,
}

#[derive(Clone, Debug)]
pub(super) struct ReplayPointer {
    pub(super) name: String,
    pub(super) color: (u8, u8, u8),
    pub(super) desktop_space: bool,
}

/// What the desktop looked like at one history entry: every workspace's
/// strips, `None` when the entry's document state cannot be read back. A
/// change's state is fixed by its hash, so a checkpoint is computed once.
pub(super) type ReplayCheckpoint = Option<Vec<(u64, ReplayWorkspace)>>;

#[cfg(test)]
thread_local! {
    /// How many replay checkpoints this thread forked the document for.
    pub(super) static REPLAY_CHECKPOINT_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// The most recent entries replay shows; a room with years of edits still
/// opens immediately.
const REPLAY_FRAMES: usize = 120;

impl RoomActivityPane {
    /// Build each replay checkpoint from the real causal document state.
    /// Recent frames are bounded so a room with years of edits still opens
    /// immediately; every retained frame remains an exact Automerge fork.
    ///
    /// Forking the document is O(history) and runs on the compositor thread
    /// whenever the rows move. Checkpoints are therefore kept per entry and
    /// only entries new since the last rebuild are forked; switching
    /// workspaces or scope reuses them, and a switch's own change is no
    /// entry (the history source passes over it).
    pub(super) fn rebuild_replay_frames(
        &mut self,
        tree: &impl ActivityTree,
        rows: &[HistoryEntry],
        active_workspace: u64,
    ) {
        self.replay_unavailable = 0;
        // A frame shows every room's layout as it stood, so a change admitted
        // into any room moves the frames from its own time on; earlier frames
        // are the same history they were.
        let heads = super::pane::history_heads(tree);
        let invalid_from = if heads != self.replay_heads {
            let invalid = self.replay_invalid_from(tree, &heads);
            self.replay_heads = heads;
            invalid
        } else {
            None
        };
        let mut ordered = rows.to_vec();
        ordered.compact_sort_by_key(|entry| entry.timestamp);
        let skip = ordered.len().saturating_sub(REPLAY_FRAMES);
        let retained_pointers: HashMap<HistoryEntryId, Vec<ReplayPointer>> = self
            .replay_frames
            .iter()
            .map(|frame| (frame.entry_id, frame.pointers.clone()))
            .collect();
        let mut checkpoints = std::mem::take(&mut self.replay_checkpoints);
        let mut frames = Vec::with_capacity(ordered.len().min(REPLAY_FRAMES));
        for entry in ordered.into_iter().skip(skip) {
            if let Some(from) = invalid_from
                && entry.timestamp >= from
            {
                checkpoints.remove(&entry.id);
            }
            // The entry's heads come from the log that read them with the
            // change: looking them up in the document lists every change of
            // the room again.
            let checkpoint = checkpoints
                .entry(entry.id)
                .or_insert_with(|| self.replay_checkpoint_for(tree, &entry));
            let Some(snapshot) = checkpoint.as_ref() else {
                self.replay_unavailable = self.replay_unavailable.saturating_add(1);
                continue;
            };
            let workspaces = snapshot
                .iter()
                .filter(|(id, _)| self.scope != RoomScope::ThisRoom || *id == active_workspace)
                .map(|(_, workspace)| workspace.clone())
                .collect();
            frames.push(WorkspaceReplayFrame {
                entry_id: entry.id,
                author: entry.author,
                summary: entry.summary,
                timestamp: entry.timestamp,
                workspaces,
                pointers: retained_pointers
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        // Another workspace's entries stay: switching back replays them
        // without forking again. Only entries gone from the history go.
        let history: HashSet<HistoryEntryId> = self.derived_entry_ids().collect();
        checkpoints.retain(|id, _| history.contains(id));
        self.replay_checkpoints = checkpoints;
        self.replay_frames = frames;
        self.replay_index = self
            .replay_index
            .min(self.replay_frames.len().saturating_sub(1));
        if self.replay_frames.is_empty() {
            self.replay_playing = false;
        }
    }

    /// The time from which retained checkpoints no longer describe history:
    /// the oldest change admitted since they were built, or `None` when every
    /// change since then moved no window. A room that went invalidates every
    /// frame.
    fn replay_invalid_from(
        &self,
        tree: &impl ActivityTree,
        heads: &BTreeMap<HistoryRoom, Vec<automerge::ChangeHash>>,
    ) -> Option<SystemTime> {
        if self
            .replay_heads
            .keys()
            .any(|room| !heads.contains_key(room))
        {
            return Some(ui::time::UNIX_EPOCH);
        }
        let mut invalid = None;
        for (room, now) in heads {
            let known = self.replay_heads.get(room);
            if known == Some(now) {
                continue;
            }
            let Some(document) = tree.history_document(*room) else {
                return Some(ui::time::UNIX_EPOCH);
            };
            let mut document = (*document).clone();
            document.rollback();
            let changes = document.get_changes(known.map_or(&[][..], Vec::as_slice));
            // A change the feed passes over moved no window: the frames built
            // before it stand.
            let Some(oldest) = changes
                .iter()
                .filter(|change| !self.source.passes_over_change(change))
                .map(|change| change.timestamp().max(0))
                .min()
            else {
                continue;
            };
            let at = ui::time::UNIX_EPOCH
                .checked_add(ui::time::Duration::from_secs(
                    u64::try_from(oldest).unwrap_or_default(),
                ))
                .unwrap_or(ui::time::UNIX_EPOCH);
            invalid = Some(invalid.map_or(at, |current: SystemTime| current.min(at)));
        }
        invalid
    }

    /// The desktop at `entry`: the document the entry changed, forked to its
    /// own heads, and every other room at the state its own changes had
    /// reached by the entry's time.
    fn replay_checkpoint_for(
        &self,
        tree: &impl ActivityTree,
        entry: &HistoryEntry,
    ) -> ReplayCheckpoint {
        let room = self.history_room_of(&entry.id);
        let heads: Vec<automerge::ChangeHash> = self
            .history_logs
            .get(&room)
            .and_then(|log| log.heads_for_entry(&entry.id))?;
        replay_checkpoint(tree, room, &heads, entry.timestamp)
    }
}

/// The desktop at a history entry: the document the entry changed at exactly
/// its heads, and each other room at the state its own changes had reached by
/// `at`. A host whose one document holds every workspace forks it once, as
/// that document's own causal history.
fn replay_checkpoint<T: ActivityTree>(
    baseline: &T,
    room: HistoryRoom,
    heads: &[automerge::ChangeHash],
    at: SystemTime,
) -> ReplayCheckpoint {
    #[cfg(test)]
    REPLAY_CHECKPOINT_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    let rooms = baseline.history_rooms();
    if room.is_none() && rooms == [None] {
        // Automerge 0.8's `fork_at` collector can traverse a valid merged
        // graph in non-topological order and panic on a dependency that
        // appears later. Clone the complete graph, then constrain reads to
        // the historical clock; the replay snapshot is discarded after
        // this frame is materialised.
        let mut doc = baseline.document().clone().fork();
        tilo_automerge::ApplyIncoming::isolate_at(&mut doc, heads);
        let mut snapshot = baseline.clone();
        *snapshot.document_mut() = doc;
        snapshot.resync_activity_cache().ok()?;
        return Some(replay_workspaces(&snapshot));
    }
    let mut desktop = (**baseline).clone();
    desktop.workspaces.clear();
    let mut shown: BTreeSet<tilo_desktop_model::RoomId> = BTreeSet::new();
    for known in rooms {
        let Some(known) = known else {
            continue;
        };
        let Some(document) = baseline.history_document(Some(known)) else {
            continue;
        };
        let heads = if Some(known) == room {
            Some(heads.to_vec())
        } else {
            heads_as_of(document, at)
        };
        let Some(heads) = heads.filter(|heads| !heads.is_empty()) else {
            continue;
        };
        let mut forked = (*document).clone();
        forked.rollback();
        tilo_automerge::ApplyIncoming::isolate_at(&mut forked, &heads);
        // The workspace keeps the identity this desktop shows it as.
        if let Some(workspace) = baseline
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == known)
        {
            desktop.workspaces.push(workspace.clone());
        }
        match tilo_desktop_model::projection::apply_admitted_room(&mut desktop, known, &forked) {
            Ok(_) => {
                shown.insert(known);
            }
            // The change's own room cannot be read back: the entry has no
            // honest frame rather than one showing something else.
            Err(_) if Some(known) == room => return None,
            Err(_) => {}
        }
    }
    desktop
        .workspaces
        .retain(|workspace| shown.contains(&workspace.room()));
    Some(replay_workspaces(&desktop))
}

/// The heads a room document's own changes had reached by `at`: every change
/// stamped no later than that, in the document's causal order. Changes are
/// the only cross-document order two rooms' documents share.
fn heads_as_of(doc: &automerge::AutoCommit, at: SystemTime) -> Option<Vec<automerge::ChangeHash>> {
    let elapsed = at.duration_since(ui::time::UNIX_EPOCH).ok()?;
    let cutoff = i64::try_from(elapsed.as_millis()).ok()?;
    let mut forked = (*doc).clone();
    forked.rollback();
    let mut heads: Vec<automerge::ChangeHash> = Vec::new();
    for change in forked.get_changes(&[]) {
        let Some(timestamp) = change.timestamp().checked_mul(1000) else {
            continue;
        };
        if timestamp > cutoff {
            break;
        }
        heads.retain(|head| !change.deps().contains(head));
        heads.push(change.hash());
    }
    Some(heads)
}

/// Every workspace of `desktop` as a frame shows it: its name and the
/// windows each strip holds, with the geometry the projection kept.
fn replay_workspaces(desktop: &tilo_desktop_model::DesktopTree) -> Vec<(u64, ReplayWorkspace)> {
    let mut workspaces = Vec::new();
    for workspace in &desktop.workspaces {
        let multiple_strips = workspace.strips.len() > 1;
        for strip in &workspace.strips {
            let name = if multiple_strips {
                format!("{} · {}", workspace.name, strip.name)
            } else {
                workspace.name.clone()
            };
            let groups = strip
                .window_groups
                .iter()
                .map(|group| ReplayGroup {
                    tabs: group.tabs.iter().map(|tab| tab.title.clone()).collect(),
                    active: group
                        .tabs
                        .iter()
                        .position(|tab| tab.id == group.active_tab)
                        .unwrap_or(0),
                    width_permille: group.width.permille(),
                })
                .collect();
            workspaces.push((workspace.id.raw(), ReplayWorkspace { name, groups }));
        }
    }
    workspaces
}
