//! Workspace history presentation shared by native and admitted portable hosts.
//! The host owns persistence and effects; reviewing cloned causal documents is
//! local presentation. Missing services stay unavailable.
use automerge::AutoCommit;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};
use tilo_desktop_model::DesktopTree;

pub mod index;
pub mod morning;
pub mod view_actions;
pub use view_actions::WorkspaceViewRequest;
mod pane;
mod replay;
#[cfg(test)]
mod testing;
pub use index::*;
pub use pane::{
    ACTIVITY_ACKNOWLEDGE, ActivityView, HISTORY_COMPARE, HISTORY_RESTORE, HISTORY_UNDO, Period,
    RoomActivityOutcome, RoomActivityPane, RoomScope, ServedTree, TASKS_APP, is_document_write,
    is_personal, is_read, refused_to_agent,
};

/// A room whose admitted document Activity reads: `Some(room)` when the host
/// keeps one document per room, `None` when its one document holds every
/// workspace's history (a host whose projection is a cache of a single log).
pub type HistoryRoom = Option<tilo_desktop_model::RoomId>;

/// A causal document paired with its admitted desktop projection. Implementors
/// retain native mutation/persistence authority; the default snapshot is read-only.
pub trait ActivityTree: Clone + Deref<Target = DesktopTree> + DerefMut {
    fn document(&self) -> &AutoCommit;
    fn document_mut(&mut self) -> &mut AutoCommit;
    /// Rebuild the desktop projection from the causal document.
    fn resync_activity_cache(&mut self) -> Result<(), String>;
    /// The admitted documents Activity derives history from, the active
    /// workspace's room first. A host whose one document holds every
    /// workspace answers `[None]`; a host that keeps each room's admitted
    /// document answers each room. History is never re-derived from a
    /// projection cache, so a room whose document the host does not hold is
    /// simply absent here.
    fn history_rooms(&self) -> Vec<HistoryRoom> {
        vec![None]
    }
    /// The admitted document `room` names, or `None` when this host holds
    /// none for it: an unreadable or not-yet-installed room is unavailable
    /// rather than replaced by a projection of it.
    fn history_document(&self, room: HistoryRoom) -> Option<&AutoCommit> {
        room.is_none().then(|| self.document())
    }
    fn history_document_mut(&mut self, room: HistoryRoom) -> Option<&mut AutoCommit> {
        if room.is_none() {
            Some(self.document_mut())
        } else {
            None
        }
    }
    /// Whether a history write (a restore, a name, an undo) may be committed
    /// into `room`'s document here. A room whose document this host only
    /// holds as an admitted copy, or does not hold, is not writable: the
    /// write is refused rather than committed where it would be lost.
    fn history_room_writable(&self, room: HistoryRoom) -> bool {
        self.history_document(room).is_some()
    }
    fn replay_presentation_note(&self) -> Option<String> {
        None
    }
    /// The delegated agents on the rooms' rosters whose person's root signed
    /// the delegation for exactly that device, as the host verified them:
    /// the changes their own keys signed read as theirs, on behalf of their
    /// person. A host that verifies none leaves those changes unknown.
    fn roster_agents(&self) -> Vec<ui::history_source::RosterAgent> {
        Vec::new()
    }
    fn commit_activity_change(&mut self) {}
}

/// Read-only admitted single-room history. Construct from the actual admitted
/// owner document, never from a re-encoded desktop cache. The supplied document
/// is owned so `HistorySource`'s `AutoCommit` cursor cannot touch the live owner.
#[derive(Clone)]
pub struct ActivitySnapshot {
    pub desktop: DesktopTree,
    pub doc: AutoCommit,
    room: tilo_desktop_model::RoomId,
}
impl ActivitySnapshot {
    /// Snapshot the active room of `desktop` from its admitted owner document.
    pub fn new(desktop: DesktopTree, doc: AutoCommit) -> Result<Self, String> {
        let room = desktop
            .active_workspace()
            .ok_or("active workspace unavailable")?
            .room();
        let mut snapshot = Self { desktop, doc, room };
        tilo_desktop_model::projection::apply_admitted_room(
            &mut snapshot.desktop,
            room,
            &snapshot.doc,
        )?;
        Ok(snapshot)
    }
}
impl Deref for ActivitySnapshot {
    type Target = DesktopTree;
    fn deref(&self) -> &DesktopTree {
        &self.desktop
    }
}
impl DerefMut for ActivitySnapshot {
    fn deref_mut(&mut self) -> &mut DesktopTree {
        &mut self.desktop
    }
}
impl ActivityTree for ActivitySnapshot {
    fn document(&self) -> &AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }
    fn history_rooms(&self) -> Vec<HistoryRoom> {
        vec![Some(self.room)]
    }
    fn history_document(&self, room: HistoryRoom) -> Option<&AutoCommit> {
        (room == Some(self.room)).then_some(&self.doc)
    }
    fn history_document_mut(&mut self, room: HistoryRoom) -> Option<&mut AutoCommit> {
        (room == Some(self.room)).then_some(&mut self.doc)
    }
    fn replay_presentation_note(&self) -> Option<String> {
        Some(crate::i18n::t("activity-replay-layout-not-recorded"))
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        // A historical projection must not retain tabs that only exist in the
        // current cache. Room documents do not record local width or active-tab
        // selection, so reconstructed groups use neutral presentation defaults.
        self.desktop.workspaces.retain(|w| w.room() == self.room);
        for workspace in &mut self.desktop.workspaces {
            workspace.strips.clear();
        }
        tilo_desktop_model::projection::apply_admitted_room(&mut self.desktop, self.room, &self.doc)
            .map(|_| ())
    }
}

/// Optional host services. Browser hosts install only real admitted adapters.
/// Resource opening and narration use the pane's existing outcome/request APIs.
pub trait ActivityHost {
    fn local_user(&self) -> String {
        String::new()
    }
    fn local_author(&self, _authors: &mut ui::AuthorDirectory, _actor: &str) -> Option<String> {
        None
    }
    fn morning_sources_available(&self) -> bool {
        false
    }
    /// Presently admitted provider data only; no implicit refresh or account connection.
    fn morning_payload(
        &self,
        _room: &str,
        _source: &morning::MorningSource,
    ) -> Option<morning::MorningPayload> {
        None
    }
    /// Submit a chosen source command through the existing admitted action session.
    fn submit_morning_command(
        &self,
        _room: &str,
        _source: &morning::MorningSource,
        _command: morning::MorningCommand,
    ) -> Result<(), String> {
        Err(crate::i18n::t("activity-morning-command-unavailable"))
    }
    fn morning_command_status(
        &self,
        _room: &str,
        _source: &morning::MorningSource,
    ) -> Option<String> {
        None
    }
    fn load_morning_sources(&self) -> HashMap<u64, morning::MorningPreferences> {
        HashMap::new()
    }
    fn save_morning_sources(&self, _sources: &HashMap<u64, morning::MorningPreferences>) {}
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        HashMap::new()
    }
    fn save_frontiers(&self, _frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {}
    /// When this device last left each workspace, for the away recap.
    /// Private to the device, like the seen frontiers.
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        HashMap::new()
    }
    fn save_departures(&self, _departures: &HashMap<u64, WorkspaceDeparture>) {}
    fn mutations_available(&self) -> bool {
        false
    }
    fn all_workspaces_available(&self) -> bool {
        false
    }
    fn app_display_name(&self, _app: &str) -> Option<String> {
        None
    }
    /// Undo an agent's run of task `task`: each document in `steps` (its
    /// app and record) undoes the changes grouped under the task, through
    /// its own history, as the person at the screen.
    ///
    /// # Errors
    /// The host cannot undo here, or refused to start.
    fn undo_task_run(&self, _undo: TaskRunUndo) -> Result<(), String> {
        Err(crate::i18n::t("activity-undo-task-unavailable"))
    }
    /// How the undo `invocation` ended: the records it was carried out in,
    /// or why nothing was undone. `None` while it runs.
    fn poll_task_run_undo(
        &self,
        _invocation: uuid::Uuid,
    ) -> Option<Result<Vec<TaskRunDocument>, String>> {
        None
    }
}

/// One document an undo of an agent's task was carried out in: the step's
/// app and record, so the task view reports what came back and what did not.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRunDocument {
    pub app: String,
    pub record: String,
}

/// An agent's run of a task to undo as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRunUndo {
    pub room: String,
    pub invocation: uuid::Uuid,
    pub task_id: String,
    /// The documents the run changed: each step's app and record.
    pub steps: Vec<(String, String)>,
}
pub struct UnavailableActivityHost;
impl ActivityHost for UnavailableActivityHost {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityDeepLink {
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    pub resource_id: Option<String>,
    pub resource_version: Option<[u8; 16]>,
}
#[cfg(test)]
mod admitted_history_tests {
    use super::*;
    use automerge::{ROOT, transaction::Transactable};
    use tilo_desktop_model::{Workspace, WorkspaceId};

    #[test]
    fn workspace_activity_real_room_topology_enters_pane_feed_and_replay() {
        use tilo_desktop_model::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        };
        let workspace = Workspace::new(WorkspaceId(9), "Actual room");
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "room", workspace.room().to_string()).unwrap();
        doc.put(ROOT, "name", "Actual room").unwrap();
        doc.commit();
        let mut strips = vec![Strip {
            id: StripId((5 << 48) | 1),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: (5 << 48) | 2,
                active_tab: (5 << 48) | 3,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
                tabs: vec![Tab {
                    id: (5 << 48) | 3,
                    title: "Before".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("so.tilo.notes".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                }],
            }],
        }];
        tilo_desktop_model::projection::project_owned_contents(
            &mut doc,
            &strips,
            5,
            None,
            tilo_desktop_model::projection::EmptyStrips::Skip,
        )
        .unwrap();
        doc.commit();
        strips[0].window_groups[0].tabs[0].title = "After".into();
        tilo_desktop_model::projection::project_owned_contents(
            &mut doc,
            &strips,
            5,
            None,
            tilo_desktop_model::projection::EmptyStrips::Skip,
        )
        .unwrap();
        doc.commit();
        let mut snapshot = ActivitySnapshot::new(
            DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..Default::default()
            },
            doc,
        )
        .unwrap();
        let heads = snapshot.doc.get_heads();
        let mut pane = RoomActivityPane::new();
        pane.begin_view(9);
        pane.refresh(&mut snapshot);
        // The window opened and the title change are listed; setup and
        // layout commits are not.
        let summaries = pane
            .entries()
            .iter()
            .map(|entry| entry.summary.as_str())
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 2, "{summaries:?}");
        assert!(summaries.contains(&"Opened Before"), "{summaries:?}");
        let labels = pane
            .replay_frames
            .iter()
            .flat_map(|frame| &frame.workspaces)
            .flat_map(|workspace| &workspace.groups)
            .flat_map(|group| &group.tabs)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|s| s == "Before"),
            "actual causal topology was lost: {labels:?}"
        );
        assert!(labels.iter().any(|s| s == "After"));
        assert_eq!(snapshot.doc.get_heads(), heads);
    }
}
