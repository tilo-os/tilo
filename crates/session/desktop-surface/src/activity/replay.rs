use super::ActivityTree;
use super::pane::{RoomActivityPane, RoomScope};
use std::collections::HashMap;
use ui::clock::SystemTime;
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

impl RoomActivityPane {
    /// Build each replay checkpoint from the real causal document state.
    /// Recent frames are bounded so a room with years of edits still opens
    /// immediately; every retained frame remains an exact Automerge fork.
    pub(super) fn rebuild_replay_frames(
        &mut self,
        tree: &impl ActivityTree,
        rows: &[HistoryEntry],
        active_workspace: u64,
    ) {
        self.replay_unavailable = 0;
        let mut ordered = rows.to_vec();
        ordered.sort_by_key(|entry| entry.timestamp);
        let skip = ordered.len().saturating_sub(120);
        let mut baseline = tree.clone();
        let retained_pointers: HashMap<HistoryEntryId, Vec<ReplayPointer>> = self
            .replay_frames
            .iter()
            .map(|frame| (frame.entry_id, frame.pointers.clone()))
            .collect();
        let mut frames = Vec::with_capacity(ordered.len().min(120));
        for entry in ordered.into_iter().skip(skip) {
            let Some(heads) = ui::heads_for_entry(baseline.document_mut(), &entry.id) else {
                self.replay_unavailable = self.replay_unavailable.saturating_add(1);
                continue;
            };
            // Automerge 0.8's `fork_at` collector can traverse a valid merged
            // graph in non-topological order and panic on a dependency that
            // appears later. Clone the complete graph, then constrain reads to
            // the historical clock; the replay snapshot is discarded after
            // this frame is materialised.
            let mut doc = baseline.document_mut().fork();
            tilo_automerge::ApplyIncoming::isolate_at(&mut doc, &heads);
            let mut snapshot = baseline.clone();
            *snapshot.document_mut() = doc;
            if snapshot.resync_activity_cache().is_err() {
                self.replay_unavailable = self.replay_unavailable.saturating_add(1);
                continue;
            }

            let mut workspaces = Vec::new();
            for workspace in &snapshot.workspaces {
                if self.scope == RoomScope::ThisRoom && workspace.id.raw() != active_workspace {
                    continue;
                }
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
                    workspaces.push(ReplayWorkspace { name, groups });
                }
            }
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
        self.replay_frames = frames;
        self.replay_index = self
            .replay_index
            .min(self.replay_frames.len().saturating_sub(1));
        if self.replay_frames.is_empty() {
            self.replay_playing = false;
        }
    }
}
