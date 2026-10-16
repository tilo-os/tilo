//! A causal room and a host for the activity pane's own tests.
//!
//! [`TestTree`] pairs a desktop projection with its room document and writes
//! through the same `desktop_doc` mutations a native host uses, so the pane
//! reads real Automerge history. [`TestHost`] answers for a device with a
//! fixed local user and keeps its private read state in memory.
use super::{ActivityHost, ActivityTree, WorkspaceDeparture, WorkspaceSeenFrontier};
use automerge::{AutoCommit, ChangeHash};
use collab_types::caps::CapOverride;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::{
    AppKind, Collaborator, DesktopTree, RoomId, Strip, StripId, Tab, TileContent, UserId,
    WindowGroup, WindowWidth, Workspace, WorkspaceEffectReceipt, WorkspaceId, WorkspaceTask,
    WorkspaceTaskAuditEvent, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
};

/// The local user every [`TestHost`] answers for.
pub(crate) const LOCAL_USER: &str = "local";

/// A verb named after `label` (`Rewrite opening` → `rewrite-opening`), as a
/// test's entry records it.
pub(crate) fn verb(label: &str) -> tilo_actions::Described {
    tilo_actions::Described::new(tilo_actions::Did::verb(
        tilo_actions::ActionKey::new(
            "org.tilo.Writer",
            "document",
            tilo_actions::entry::kebab(label),
        ),
        [],
    ))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id for a workspace, strip, group or tab in a fixture.
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn empty_workspace(id: WorkspaceId, name: &str, strips: Vec<Strip>) -> Workspace {
    let mut workspace = Workspace::new(id, name);
    workspace.set_room(RoomId::new());
    workspace.strips = strips;
    workspace
}

#[derive(Clone, Default)]
pub(crate) struct TestTree {
    pub desktop: DesktopTree,
    pub doc: AutoCommit,
}

impl std::ops::Deref for TestTree {
    type Target = DesktopTree;
    fn deref(&self) -> &DesktopTree {
        &self.desktop
    }
}

impl std::ops::DerefMut for TestTree {
    fn deref_mut(&mut self) -> &mut DesktopTree {
        &mut self.desktop
    }
}

impl TestTree {
    /// One empty Home workspace.
    pub fn bootstrap_default() -> Self {
        let ws_id = WorkspaceId(next_id());
        let strip_id = StripId(next_id());
        let home = empty_workspace(ws_id, "Home", vec![Strip::new(strip_id, "Main")]);
        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: ws_id,
                active_strip: HashMap::from([(ws_id, strip_id)]),
                ..DesktopTree::default()
            },
            ..Self::default()
        };
        tree.seed_doc_from_cache();
        tree
    }

    /// Home with four apps open, plus empty Research and Design rooms.
    pub fn bootstrap_demo() -> Self {
        let ws_id = WorkspaceId(next_id());
        let strip_id = StripId(next_id());
        let group_id = next_id();
        let tab = |title: &str, kind: AppKind, label: &str| Tab {
            id: next_id(),
            title: title.into(),
            tile: TileContent::AppPlaceholder {
                kind,
                label: label.into(),
            },
        };
        let tabs = vec![
            tab("Notes", AppKind::Other("Notes".into()), "Notes"),
            tab("Files", AppKind::FileExplorer, "Files"),
            tab("Terminal", AppKind::Terminal, "Terminal"),
            tab("Document", AppKind::Other("Word".into()), "Document"),
        ];
        let active_tab = tabs[0].id;
        let mut strip = Strip::new(strip_id, "Main");
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs,
            active_tab,
            width: WindowWidth::Wide,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut home = empty_workspace(ws_id, "Home", vec![strip]);
        home.collaborators = vec![
            Collaborator::new("alice", "Alice")
                .with_cursor_color("#ec4899")
                .with_role("editor"),
        ];
        let mut tree = Self {
            desktop: DesktopTree {
                workspaces: vec![home],
                active: ws_id,
                active_strip: HashMap::from([(ws_id, strip_id)]),
                active_group: HashMap::from([(strip_id, group_id)]),
                ..DesktopTree::default()
            },
            ..Self::default()
        };
        let research = tree.append_empty_workspace("Research");
        if let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == research)
        {
            workspace.collaborators.push(
                Collaborator::new("alice", "Alice")
                    .with_cursor_color("#ec4899")
                    .with_role("editor"),
            );
            workspace.collaborators.push(
                Collaborator::new("bob", "Bob")
                    .with_cursor_color("#f97316")
                    .with_role("viewer"),
            );
        }
        tree.append_empty_workspace("Design");
        tree.seed_doc_from_cache();
        tree
    }

    pub fn commit_room_change(&mut self) {
        let _ = tilo_resource::history::commit_stamped(&mut self.doc);
    }

    pub fn seed_doc_from_cache(&mut self) {
        tilo_desktop_model::desktop_doc::seed(&mut self.doc, &self.desktop);
        self.commit_room_change();
    }

    pub fn resync_cache_from_doc(&mut self) {
        tilo_desktop_model::desktop_doc::rebuild(&self.doc, &mut self.desktop);
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.desktop.workspaces.iter().any(|w| w.id == id) {
            self.desktop.active = id;
            tilo_desktop_model::desktop_doc::set_active(&mut self.doc, id);
        }
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        let ws_id = WorkspaceId(next_id());
        let strip_id = StripId(next_id());
        let workspace = empty_workspace(ws_id, name, vec![Strip::new(strip_id, "Main")]);
        let room = workspace.room;
        self.desktop.active_strip.insert(ws_id, strip_id);
        self.desktop.workspaces.push(workspace);
        tilo_desktop_model::desktop_doc::append_workspace(&mut self.doc, ws_id, name, room);
        ws_id
    }

    pub fn doc_append_tab_to_first_group_of_active_workspace(
        &mut self,
        tab_id: u64,
        title: String,
        tile_blob: Vec<u8>,
    ) -> bool {
        tilo_desktop_model::desktop_doc::append_tab(
            &mut self.doc,
            self.desktop.active,
            None,
            tab_id,
            title,
            tile_blob,
        )
    }

    fn doc_insert_sandbox(&mut self, workspace_id: WorkspaceId, cap: &CapOverride) {
        if tilo_desktop_model::desktop_doc::insert_sandbox(&mut self.doc, workspace_id, cap) {
            self.commit_room_change();
        }
    }

    /// Let an app reach a sandboxed resource, as `grantor`, who must be
    /// allowed to grant in the room.
    pub fn grant_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
    ) -> Option<String> {
        let workspace = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)?;
        if !workspace.may_govern_at(
            grantor,
            tilo_action_policy::roles::governance::GRANT,
            tilo_clock::unix_now(),
        ) {
            return None;
        }
        let id = workspace
            .grant_sandbox(cap.granted_by(collab_types::caps::Principal::Person(grantor.clone())));
        let stored = workspace.sandbox.iter().find(|c| c.id == id).cloned()?;
        self.doc_insert_sandbox(workspace_id, &stored);
        Some(id)
    }

    pub fn revoke_in_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        revoker: &UserId,
        cap_id: &str,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
        else {
            return false;
        };
        if !workspace.may_govern_at(
            revoker,
            tilo_action_policy::roles::governance::REVOKE,
            tilo_clock::unix_now(),
        ) || !workspace.revoke_sandbox(cap_id)
        {
            return false;
        }
        if tilo_desktop_model::desktop_doc::revoke_sandbox(&mut self.doc, workspace_id, cap_id) {
            self.commit_room_change();
        }
        true
    }

    pub fn adopt_ownerless_workspaces(&mut self, user: &UserId) -> Vec<WorkspaceId> {
        let mut adopted = Vec::new();
        for workspace in &mut self.desktop.workspaces {
            if workspace.owner().is_none() {
                workspace.set_owner(user.clone());
                adopted.push(workspace.id);
            }
        }
        for id in &adopted {
            if tilo_desktop_model::desktop_doc::set_owner(&mut self.doc, *id, user.as_str()) {
                self.commit_room_change();
            }
            let caps = self
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == *id)
                .map(|w| w.sandbox.clone())
                .unwrap_or_default();
            for cap in caps {
                self.doc_insert_sandbox(*id, &cap);
            }
        }
        adopted
    }

    pub fn append_workspace_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> bool {
        let Some(workspace) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return false;
        };
        if workspace
            .effect_receipts
            .iter()
            .any(|receipt| receipt.invocation == invocation)
        {
            return false;
        }
        context
            .facets
            .push(tilo_actions::ActionFacet::new("tilo.receipt", "effect"));
        context.facets.push(tilo_actions::ActionFacet::new(
            "tilo.invocation",
            invocation.0.to_string(),
        ));
        let receipt = WorkspaceEffectReceipt {
            invocation,
            actor: actor.to_string(),
            context,
            status,
            completed_at: tilo_clock::unix_now_i64(),
        };
        workspace.effect_receipts.push(receipt.clone());
        if tilo_desktop_model::desktop_doc::upsert_effect_receipt(
            &mut self.doc,
            workspace_id,
            &receipt,
        ) {
            self.commit_room_change();
        }
        true
    }

    pub fn create_workspace_task(
        &mut self,
        workspace_id: WorkspaceId,
        title: impl Into<String>,
        actor: impl Into<String>,
        assigned_agent: Option<String>,
        route: WorkspaceTaskRoute,
        resource: Option<WorkspaceTaskResource>,
    ) -> Option<String> {
        let title = title.into().trim().to_string();
        if title.is_empty() {
            return None;
        }
        let actor = actor.into();
        let now = tilo_clock::unix_now_i64();
        let id = format!("task:{}", uuid::Uuid::new_v4());
        let task = WorkspaceTask {
            id: id.clone(),
            title: title.clone(),
            created_by: actor.clone(),
            assigned_agent,
            route,
            status: WorkspaceTaskStatus::Queued,
            resource,
            subject: None,
            created_at: now,
            updated_at: now,
            audit: vec![WorkspaceTaskAuditEvent::new(
                now,
                actor,
                "create-task",
                [("title", tilo_actions::Scalar::text(&title))],
            )],
        };
        self.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)?
            .tasks
            .push(task.clone());
        if tilo_desktop_model::desktop_doc::upsert_task(&mut self.doc, workspace_id, &task) {
            self.commit_room_change();
        }
        Some(id)
    }

    pub fn set_workspace_task_status(
        &mut self,
        workspace_id: WorkspaceId,
        task_id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
    ) -> bool {
        let now = tilo_clock::unix_now_i64();
        let Some(task) = self
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| workspace.tasks.iter_mut().find(|task| task.id == task_id))
        else {
            return false;
        };
        if task.status == status {
            return false;
        }
        task.status = status;
        task.updated_at = now;
        task.audit.push(WorkspaceTaskAuditEvent::new(
            now,
            actor,
            "set-status",
            [("status", tilo_actions::Scalar::from(status.key()))],
        ));
        let task = task.clone();
        if tilo_desktop_model::desktop_doc::upsert_task(&mut self.doc, workspace_id, &task) {
            self.commit_room_change();
        }
        true
    }

    pub fn export_changes_since(&mut self, since: &[ChangeHash]) -> Vec<Vec<u8>> {
        self.doc
            .get_changes(since)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    pub fn apply_changes(&mut self, changes: &[Vec<u8>]) -> Result<(), automerge::AutomergeError> {
        let parsed: Result<Vec<automerge::Change>, _> = changes
            .iter()
            .map(|bytes| automerge::Change::try_from(bytes.as_slice()))
            .collect();
        self.doc.apply_incoming(parsed?)?;
        self.resync_cache_from_doc();
        Ok(())
    }
}

impl ActivityTree for TestTree {
    fn document(&self) -> &AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut AutoCommit {
        &mut self.doc
    }
    fn commit_activity_change(&mut self) {
        self.commit_room_change();
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        self.resync_cache_from_doc();
        Ok(())
    }
    fn create_workspace_task(
        &mut self,
        workspace: WorkspaceId,
        title: String,
        actor: String,
        agent: Option<String>,
        route: WorkspaceTaskRoute,
        resource: Option<WorkspaceTaskResource>,
    ) -> Option<String> {
        Self::create_workspace_task(self, workspace, title, actor, agent, route, resource)
    }
    fn set_workspace_task_status(
        &mut self,
        workspace: WorkspaceId,
        id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
    ) -> bool {
        Self::set_workspace_task_status(self, workspace, id, status, actor)
    }
}

/// A device that may mutate the room, answers for [`LOCAL_USER`] and keeps
/// its private read state in memory.
#[derive(Default)]
pub(crate) struct TestHost {
    frontiers: RefCell<HashMap<u64, WorkspaceSeenFrontier>>,
    departures: RefCell<HashMap<u64, WorkspaceDeparture>>,
}

impl ActivityHost for TestHost {
    fn morning_sources_available(&self) -> bool {
        true
    }
    fn local_user(&self) -> String {
        LOCAL_USER.to_string()
    }
    fn local_author(&self, _authors: &mut ui::AuthorDirectory, _actor: &str) -> Option<String> {
        Some(self.local_user())
    }
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.frontiers.borrow().clone()
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        self.frontiers.borrow_mut().clone_from(frontiers);
    }
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.departures.borrow().clone()
    }
    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.departures.borrow_mut().clone_from(departures);
    }
    fn mutations_available(&self) -> bool {
        true
    }
    fn all_workspaces_available(&self) -> bool {
        true
    }
    fn agent_members(&self, workspace: &Workspace) -> Vec<(String, tilo_action_policy::KindSet)> {
        workspace.agent_members_at(tilo_clock::unix_now())
    }
    fn launch_agent(&self, _agent: &str, _workspace: &Workspace) -> Result<(), String> {
        Ok(())
    }
}
