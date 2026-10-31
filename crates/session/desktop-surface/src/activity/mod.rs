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

/// A causal document paired with its admitted desktop projection. Implementors
/// retain native mutation/persistence authority; the default snapshot is read-only.
pub trait ActivityTree: Clone + Deref<Target = DesktopTree> + DerefMut {
    fn document(&self) -> &AutoCommit;
    fn document_mut(&mut self) -> &mut AutoCommit;
    /// Rebuild the desktop projection from the causal document.
    fn resync_activity_cache(&mut self) -> Result<(), String>;
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

/// What takes the desk a committing snapshot wrote back to where it is kept.
type Commit = std::rc::Rc<dyn Fn(&DesktopTree)>;

/// Read-only admitted history of the desk. Construct from the actual admitted
/// owner document, never from a re-encoded desktop cache. The supplied document
/// is owned so `HistorySource`'s `AutoCommit` cursor cannot touch the live owner.
#[derive(Clone)]
pub struct ActivitySnapshot {
    pub desktop: DesktopTree,
    pub doc: AutoCommit,
    /// The room whose document this is, or `None` when it is the device's own
    /// document of the whole desk.
    room: Option<tilo_desktop_model::RoomId>,
    /// What takes a change the model wrote back to the desk it came from: a
    /// host that keeps the workspace's own layout applies it here, and a
    /// host whose history is read only keeps none.
    commit: Option<Commit>,
}
impl ActivitySnapshot {
    /// Snapshot the active room of `desktop` from its admitted owner document.
    pub fn new(desktop: DesktopTree, doc: AutoCommit) -> Result<Self, String> {
        let room = desktop
            .active_workspace()
            .ok_or("active workspace unavailable")?
            .room();
        let mut snapshot = Self {
            desktop,
            doc,
            room: Some(room),
            commit: None,
        };
        tilo_desktop_model::projection::apply_admitted_room(
            &mut snapshot.desktop,
            room,
            &snapshot.doc,
        )?;
        Ok(snapshot)
    }

    /// Snapshot the whole desk of `desktop` from the device's own document of
    /// it: every workspace, in the one document this device keeps, whose
    /// changes are what the desk did and who did it.
    #[must_use]
    pub fn of_desktop(desktop: DesktopTree, doc: AutoCommit) -> Self {
        let mut snapshot = Self {
            desktop,
            doc,
            room: None,
            commit: None,
        };
        let _ = snapshot.resync_activity_cache();
        snapshot
    }

    /// Hand every change the model writes to `commit`, with the workspace as
    /// the change left it: a host that keeps the workspace's layout writes
    /// it back, so undoing or restoring a change reaches the desk itself.
    #[must_use]
    pub fn committing(mut self, commit: std::rc::Rc<dyn Fn(&DesktopTree)>) -> Self {
        self.commit = Some(commit);
        self
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
        // A room document records which windows the workspace holds and how
        // they are arranged, never how wide they are or which is in front:
        // those are the device's. The device's own document of the desk
        // records the view it was left in too, so there is nothing to note.
        self.room
            .map(|_| crate::i18n::t("activity-replay-layout-not-recorded"))
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        let Some(room) = self.room else {
            // The whole desk, out of the device's document: its topology,
            // and the view it was left in.
            tilo_desktop_model::desktop_doc::rebuild(&self.doc, &mut self.desktop);
            tilo_desktop_model::desktop_doc::rebuild_view(&self.doc, &mut self.desktop);
            return Ok(());
        };
        // A historical projection must not retain tabs that only exist in the
        // current cache. Room documents do not record local width or active-tab
        // selection, so reconstructed groups use neutral presentation defaults.
        self.desktop.workspaces.retain(|w| w.room() == room);
        for workspace in &mut self.desktop.workspaces {
            workspace.strips.clear();
        }
        tilo_desktop_model::projection::apply_admitted_room(&mut self.desktop, room, &self.doc)
            .map(|_| ())
    }
    fn commit_activity_change(&mut self) {
        let Some(commit) = self.commit.clone() else {
            return;
        };
        // The change was written into the document: what the desk becomes is
        // what that document projects, not the tree it projected before.
        let _ = self.resync_activity_cache();
        commit(&self.desktop);
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
    /// Whether the host can write `origin`'s history back: undo a change of
    /// it, restore it to an earlier state. A host that keeps the workspace's
    /// own layout writes that one; a record only where the host holds the
    /// writer it is kept by. What it cannot write, the pane does not offer.
    fn writable(&self, _origin: &index::HistoryOrigin) -> bool {
        self.mutations_available()
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
        let write = |doc: &mut AutoCommit, strips: &[Strip]| {
            let mut layout = workspace.clone();
            layout.strips = strips.to_vec();
            tilo_desktop_model::shared_record::device::write(
                doc,
                workspace.room(),
                &layout,
                &tilo_desktop_model::shared_record::device::Writer {
                    person: Some("ada"),
                    mine: &|id| id >> 48 == 5,
                    retained: &|_| false,
                    carried: tilo_desktop_model::shared_record::device::Writer::NOTHING_CARRIED,
                },
            )
            .unwrap();
            doc.commit();
        };
        write(&mut doc, &strips);
        strips[0].window_groups[0].tabs[0].title = "After".into();
        write(&mut doc, &strips);
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
