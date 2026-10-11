//! Workspace history presentation shared by native and admitted portable hosts.
//! The host owns persistence and effects; reviewing cloned causal documents is
//! local presentation. Missing services stay unavailable.
use automerge::AutoCommit;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};
use tilo_desktop_model::{
    DesktopTree, Workspace, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute,
    WorkspaceTaskStatus,
};

pub mod index;
pub mod morning;
pub mod view_actions;
pub use view_actions::WorkspaceViewRequest;
mod pane;
mod replay;
mod task_review;
pub use task_review::{TaskReviewCommand, TaskReviewRequest, TaskReviewSnapshot};
#[cfg(test)]
mod testing;
pub use index::*;
pub use pane::{
    ACTIVITY_ACKNOWLEDGE, ACTIVITY_TASKS, ActivityView, HISTORY_COMPARE, HISTORY_RESTORE,
    HISTORY_UNDO, NativeManagedTaskRequest, NativeManualTaskRequest, Period, RoomActivityOutcome,
    RoomActivityPane, RoomScope,
};

/// A causal document paired with its admitted desktop projection. Implementors
/// retain native mutation/persistence authority; the default snapshot is read-only.
pub trait ActivityTree: Clone + Deref<Target = DesktopTree> + DerefMut {
    fn document(&self) -> &AutoCommit;
    fn document_mut(&mut self) -> &mut AutoCommit;
    /// Rebuild the desktop projection from the causal document.
    ///
    /// # Errors
    ///
    /// Returns a description when the document no longer projects to a desktop.
    fn resync_activity_cache(&mut self) -> Result<(), String>;
    fn replay_presentation_note(&self) -> Option<String> {
        None
    }
    fn commit_activity_change(&mut self) {}
    fn native_manual_tasks(&self, _workspace: WorkspaceId) -> bool {
        false
    }
    fn create_workspace_task(
        &mut self,
        _workspace: WorkspaceId,
        _title: String,
        _actor: String,
        _agent: Option<String>,
        _route: WorkspaceTaskRoute,
        _resource: Option<WorkspaceTaskResource>,
    ) -> Option<String> {
        None
    }
    fn set_workspace_task_status(
        &mut self,
        _workspace: WorkspaceId,
        _id: &str,
        _status: WorkspaceTaskStatus,
        _actor: &str,
    ) -> bool {
        false
    }
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
    ///
    /// # Errors
    ///
    /// Returns a description when there is no active workspace or the room does not project.
    pub fn new(desktop: DesktopTree, doc: AutoCommit) -> Result<Self, String> {
        let room = desktop
            .active_workspace()
            .ok_or("active workspace unavailable")?
            .room;
        let mut snapshot = Self { desktop, doc, room };
        tilo_desktop_model::projection::apply_admitted_room(
            &mut snapshot.desktop,
            room,
            &snapshot.doc,
        )?;
        if let Some(workspace) = snapshot
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.room == room)
        {
            workspace.tasks = tilo_desktop_model::projection::workspace_tasks_of(&snapshot.doc);
        }
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
    fn replay_presentation_note(&self) -> Option<String> {
        Some(crate::i18n::t("activity-replay-layout-not-recorded"))
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        // A historical projection must not retain tabs that only exist in the
        // current cache. Room documents do not record local width or active-tab
        // selection, so reconstructed groups use neutral presentation defaults.
        self.desktop.workspaces.retain(|w| w.room == self.room);
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
    fn agent_members(&self, _workspace: &Workspace) -> Vec<(String, collab_types::role::Role)> {
        Vec::new()
    }
    fn agent_is_live(&self, _room: &str, _agent: &str) -> bool {
        false
    }
    /// Cached, asynchronously refreshed evidence from the admitted room owner.
    /// `None` means loading; an error means unavailable, never an empty success.
    fn task_review_required(&self, _room: &str) -> bool {
        false
    }

    fn task_review_snapshot(
        &self,
        _room: &str,
    ) -> Option<Result<std::sync::Arc<TaskReviewSnapshot>, String>> {
        Some(Err(crate::i18n::t("activity-task-review-unavailable")))
    }
    /// Queue an authenticated owner command without blocking the paint/input thread.
    ///
    /// # Errors
    /// The owner adapter is unavailable or its bounded queue is full.
    fn submit_task_review(&self, _request: TaskReviewRequest) -> Result<(), String> {
        Err(crate::i18n::t("activity-task-review-unavailable"))
    }
    /// A command is successful only after its durable owner receipt is verified.
    fn poll_task_review(&self, _invocation: uuid::Uuid) -> Option<Result<(), String>> {
        None
    }
    /// Dispatch a workspace task to its agent.
    ///
    /// # Errors
    ///
    /// Returns a description when dispatch is unavailable or refused.
    fn dispatch_task(&self, _room: &str, _task: &str) -> Result<(), String> {
        Err(crate::i18n::t("activity-agent-dispatch-unavailable"))
    }
    /// Launch an agent into `workspace`.
    ///
    /// # Errors
    ///
    /// Returns a description when launching is unavailable or refused.
    fn launch_agent(&self, _agent: &str, _workspace: &Workspace) -> Result<(), String> {
        Err(crate::i18n::t("activity-agent-launch-unavailable"))
    }
}
pub struct UnavailableActivityHost;
impl ActivityHost for UnavailableActivityHost {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityDeepLink {
    pub task_id: Option<String>,
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    pub resource_id: Option<String>,
    pub resource_version: Option<[u8; 16]>,
}
#[derive(Clone, Debug)]
pub struct AgentLaunchEvent {
    pub managed_agent_id: Option<String>,
    pub managed_workspace_id: Option<String>,
    pub kind: AgentLaunchEventKind,
}
#[derive(Clone, Debug)]
pub enum AgentLaunchEventKind {
    Started {
        pid: u32,
    },
    SpawnFailed {
        detail: String,
    },
    Failed {
        code: Option<i32>,
        stderr_tail: String,
    },
    Exited,
}

#[cfg(test)]
mod admitted_history_tests {
    use super::*;
    use automerge::{ROOT, transaction::Transactable};

    #[test]
    fn workspace_activity_real_room_topology_enters_pane_feed_and_replay() {
        use tilo_desktop_model::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        };
        let workspace = Workspace::new(WorkspaceId(9), "Actual room");
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "room", workspace.room.to_string()).unwrap();
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
                tabs: vec![Tab {
                    id: (5 << 48) | 3,
                    title: "Before".into(),
                    tile: TileContent::AppPlaceholder {
                        kind: AppKind::Other("so.tilo.notes".into()),
                        label: "Notes".into(),
                    },
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
        // Setup/layout commits are omitted from the explorer feed; the
        // title change is the one Change row people should see.
        assert_eq!(pane.entries().len(), 1);
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
