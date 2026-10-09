use super::ActivityTree;
use super::pane::{RoomActivityPane, RoomScope};
use std::collections::{HashMap, HashSet};
use ui::time::SystemTime;
use ui::widget::{HistoryEntry, HistoryEntryId};
#[derive(Clone, Debug)]
pub(super) struct ReplayGroup {
    pub(super) tabs: Vec<String>,
    pub(super) active: usize,
    pub(super) width_permille: u16,
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
    pub(super) point: (f32, f32),
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
    /// whenever the desktop document moves (a workspace switch writes it).
    /// Checkpoints are therefore kept per entry and only entries new since
    /// the last rebuild are forked; switching workspaces or scope reuses
    /// them.
    pub(super) fn rebuild_replay_frames(
        &mut self,
        tree: &impl ActivityTree,
        rows: &[HistoryEntry],
        active_workspace: u64,
    ) {
        self.replay_unavailable = 0;
        let mut ordered = rows.to_vec();
        ordered.sort_by_key(|entry| entry.timestamp);
        let skip = ordered.len().saturating_sub(REPLAY_FRAMES);
        let mut baseline: Option<_> = None;
        let retained_pointers: HashMap<HistoryEntryId, Vec<ReplayPointer>> = self
            .replay_frames
            .iter()
            .map(|frame| (frame.entry_id, frame.pointers.clone()))
            .collect();
        let mut checkpoints = std::mem::take(&mut self.replay_checkpoints);
        let mut frames = Vec::with_capacity(ordered.len().min(REPLAY_FRAMES));
        for entry in ordered.into_iter().skip(skip) {
            // The entry's heads come from the history log, which read them
            // with the change: looking them up in the document lists every
            // change of the room again.
            let checkpoint = checkpoints.entry(entry.id).or_insert_with(|| {
                replay_checkpoint(
                    baseline.get_or_insert_with(|| tree.clone()),
                    self.history_log.heads_for_entry(&entry.id),
                )
            });
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
}

/// The desktop at a history entry of `baseline`'s document: a fork
/// constrained to the entry's `heads`, read back through the activity cache.
/// `None` heads (an entry that is not a change of the document) has none.
fn replay_checkpoint<T: ActivityTree>(
    baseline: &mut T,
    heads: Option<Vec<automerge::ChangeHash>>,
) -> ReplayCheckpoint {
    #[cfg(test)]
    REPLAY_CHECKPOINT_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
    let heads = heads?;
    // Automerge 0.8's `fork_at` collector can traverse a valid merged
    // graph in non-topological order and panic on a dependency that
    // appears later. Clone the complete graph, then constrain reads to
    // the historical clock; the replay snapshot is discarded after
    // this frame is materialised.
    let mut doc = baseline.document_mut().fork();
    tilo_automerge::ApplyIncoming::isolate_at(&mut doc, &heads);
    let mut snapshot = baseline.clone();
    *snapshot.document_mut() = doc;
    snapshot.resync_activity_cache().ok()?;
    let mut workspaces = Vec::new();
    for workspace in &snapshot.workspaces {
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
    Some(workspaces)
}
