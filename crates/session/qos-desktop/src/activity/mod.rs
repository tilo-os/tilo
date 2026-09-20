//! Workspace history presentation shared by native and admitted portable hosts.
//! The host owns persistence and effects; reviewing cloned causal documents is
//! local presentation. Missing services stay unavailable.
use crate::{
    Workspace, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
    WorkspaceTree,
};
use automerge::AutoCommit;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

pub mod index;
pub mod view_actions;
pub use view_actions::WorkspaceViewRequest;
mod pane;
mod replay;
pub use index::*;
pub use pane::{ActivityView, Period, RoomActivityOutcome, RoomActivityPane, RoomScope};

/// A causal document paired with its admitted desktop projection. Implementors
/// retain native mutation/persistence authority; the default snapshot is read-only.
pub trait ActivityTree: Clone + Deref<Target = WorkspaceTree> + DerefMut {
    fn document(&self) -> &AutoCommit;
    fn document_mut(&mut self) -> &mut AutoCommit;
    fn resync_activity_cache(&mut self) -> Result<(), String>;
    fn replay_presentation_note(&self) -> Option<&'static str> {
        None
    }
    fn commit_activity_change(&mut self) {}
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
/// is owned so HistorySource's AutoCommit cursor cannot touch the live owner.
#[derive(Clone)]
pub struct ActivitySnapshot {
    pub desktop: WorkspaceTree,
    pub doc: AutoCommit,
    room: crate::RoomId,
}
impl ActivitySnapshot {
    pub fn new(desktop: WorkspaceTree, doc: AutoCommit) -> Result<Self, String> {
        let room = desktop
            .active_workspace()
            .ok_or("active workspace unavailable")?
            .room;
        let mut snapshot = Self { desktop, doc, room };
        crate::projection::RoomProjection::apply_admitted_room(
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
            workspace.tasks = crate::projection::workspace_tasks_of(&snapshot.doc);
        }
        Ok(snapshot)
    }
}
impl Deref for ActivitySnapshot {
    type Target = WorkspaceTree;
    fn deref(&self) -> &WorkspaceTree {
        &self.desktop
    }
}
impl DerefMut for ActivitySnapshot {
    fn deref_mut(&mut self) -> &mut WorkspaceTree {
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
    fn replay_presentation_note(&self) -> Option<&'static str> {
        Some("Window sizes and selected tabs were not recorded")
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        // A historical projection must not retain tabs that only exist in the
        // current cache. Room documents do not record local width or active-tab
        // selection, so reconstructed groups use neutral presentation defaults.
        self.desktop.workspaces.retain(|w| w.room == self.room);
        for workspace in &mut self.desktop.workspaces {
            workspace.strips.clear();
        }
        crate::projection::RoomProjection::apply_admitted_room(
            &mut self.desktop,
            self.room,
            &self.doc,
        )
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
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        HashMap::new()
    }
    fn save_frontiers(&self, _frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {}
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
    fn dispatch_task(&self, _room: &str, _task: &str) -> Result<(), String> {
        Err("agent task dispatch is unavailable".into())
    }
    fn launch_agent(&self, _agent: &str, _workspace: &Workspace) -> Result<(), String> {
        Err("agent launching is unavailable".into())
    }
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
        use crate::{AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
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
                        kind: AppKind::Other("dev.qos.notes".into()),
                        label: "Notes".into(),
                    },
                }],
            }],
        }];
        crate::projection::RoomProjection::project_owned_contents(&mut doc, &strips, 5, None);
        doc.commit();
        strips[0].window_groups[0].tabs[0].title = "After".into();
        crate::projection::RoomProjection::project_owned_contents(&mut doc, &strips, 5, None);
        doc.commit();
        let mut snapshot = ActivitySnapshot::new(
            WorkspaceTree {
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
        assert_eq!(pane.entries().len(), 3);
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
