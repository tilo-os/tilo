//! Native services for the shared workspace activity pane.
use crate::shell::state::{
    Workspace, WorkspaceExt as _, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute,
    WorkspaceTaskStatus, WorkspaceTree,
};
use qos_desktop::activity::{ActivityHost, ActivityTree, WorkspaceSeenFrontier};
pub use qos_desktop::activity::{ActivityView, Period, RoomActivityOutcome, RoomScope};
use std::{collections::HashMap, rc::Rc};

#[derive(Default)]
struct NativeActivityHost {
    broker: Option<crate::shell::collab_broker::AppActionBroker>,
}
impl ActivityHost for NativeActivityHost {
    fn local_user(&self) -> String {
        crate::shell::state::local_user_id()
    }
    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("history author identity");
        if authors.register_local_identity(actor) {
            None
        } else {
            let user = self.local_user();
            (!user.is_empty()).then_some(user)
        }
    }
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        settings_store::load_section("history")
            .get("seen_frontiers")
            .and_then(|v| v.as_object())
            .into_iter()
            .flat_map(|values| values.iter())
            .filter_map(|(id, value)| {
                Some((
                    id.parse().ok()?,
                    serde_json::from_value(value.clone()).ok()?,
                ))
            })
            .collect()
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        let values = frontiers
            .iter()
            .filter_map(|(id, frontier)| {
                serde_json::to_value(frontier)
                    .ok()
                    .map(|value| (id.to_string(), value))
            })
            .collect();
        let mut section = serde_json::Map::new();
        section.insert("seen_frontiers".into(), serde_json::Value::Object(values));
        if let Err(error) = settings_store::update_section("history", section) {
            tracing::warn!(%error, "could not persist history seen frontier");
        }
    }
    fn mutations_available(&self) -> bool {
        true
    }
    fn all_workspaces_available(&self) -> bool {
        true
    }
    fn app_display_name(&self, app: &str) -> Option<String> {
        crate::shell::launcher::display_name_for_app_id(app)
    }
    fn agent_members(&self, workspace: &Workspace) -> Vec<(String, collab_types::role::Role)> {
        workspace.agent_members()
    }
    fn agent_is_live(&self, room: &str, agent: &str) -> bool {
        self.broker
            .as_ref()
            .is_some_and(|broker| broker.agent_is_live_task_capable(room, agent))
    }
    fn dispatch_task(&self, room: &str, task: &str) -> Result<(), String> {
        self.broker
            .as_ref()
            .ok_or_else(|| "agent task dispatch is unavailable".to_string())
            .and_then(|broker| broker.dispatch_task(room, task).map(|_| ()))
    }
    fn launch_agent(&self, agent: &str, workspace: &Workspace) -> Result<(), String> {
        crate::shell::launcher::launch_workspace_agent(agent, workspace)
            .map(|_| ())
            .map_err(|error| error.user_message())
    }
}
impl ActivityTree for WorkspaceTree {
    fn document(&self) -> &automerge::AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut automerge::AutoCommit {
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
        WorkspaceTree::create_workspace_task(self, workspace, title, actor, agent, route, resource)
    }
    fn set_workspace_task_status(
        &mut self,
        workspace: WorkspaceId,
        id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
    ) -> bool {
        WorkspaceTree::set_workspace_task_status(self, workspace, id, status, actor)
    }
}

pub struct RoomActivityPane(qos_desktop::activity::RoomActivityPane);
impl std::ops::Deref for RoomActivityPane {
    type Target = qos_desktop::activity::RoomActivityPane;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for RoomActivityPane {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}
impl RoomActivityPane {
    pub fn new() -> Self {
        let mut pane = qos_desktop::activity::RoomActivityPane::new();
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(pane)
    }
    pub fn new_persistent() -> Self {
        let mut pane = Self::new();
        pane.load_frontiers();
        pane
    }
    pub(crate) fn with_local_user(user: String) -> Self {
        let mut pane = qos_desktop::activity::RoomActivityPane::with_local_user(user);
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(pane)
    }
    pub fn attach_task_broker(&mut self, broker: crate::shell::collab_broker::AppActionBroker) {
        self.set_host(Rc::new(NativeActivityHost {
            broker: Some(broker),
        }));
    }
    pub fn apply_deep_link(&mut self, focus: crate::shell::chrome_state::ActivityFocus) {
        self.0
            .apply_deep_link(qos_desktop::activity::ActivityDeepLink {
                author: focus.author,
                group_id: focus.group_id,
                entry_id: focus.entry_id,
                resource_id: focus.resource_id,
                resource_version: focus.resource_version,
            });
    }
    pub fn observe_agent_launch_event(
        &mut self,
        event: &crate::shell::launcher::LaunchEvent,
    ) -> bool {
        self.0
            .observe_agent_launch_event(&activity_launch_event(event))
    }
}
fn activity_launch_event(
    event: &crate::shell::launcher::LaunchEvent,
) -> qos_desktop::activity::AgentLaunchEvent {
    use crate::shell::launcher::LaunchEventKind as Native;
    use qos_desktop::activity::AgentLaunchEventKind as Shared;
    qos_desktop::activity::AgentLaunchEvent {
        managed_agent_id: event.managed_agent_id.clone(),
        managed_workspace_id: event.managed_workspace_id.clone(),
        kind: match &event.kind {
            Native::Started { pid } => Shared::Started { pid: *pid },
            Native::SpawnFailed { detail } => Shared::SpawnFailed {
                detail: detail.clone(),
            },
            Native::Failed { code, stderr_tail } => Shared::Failed {
                code: *code,
                stderr_tail: stderr_tail.clone(),
            },
            Native::Exited => Shared::Exited,
        },
    }
}

// Keep the native fixture suite beside its authority adapter. Compiling the
// same implementation source here lets its established private-controller
// assertions survive without exporting playback internals to hosts.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "native fixtures compile shared pane source to exercise its private controllers"
)]
mod native_semantics {
    use qos_desktop::activity::{
        ActivityDeepLink, ActivityHost, ActivityTree, AgentLaunchEvent, AgentLaunchEventKind,
        UnavailableActivityHost,
    };
    mod index {
        pub use qos_desktop::activity::index::*;
    }
    mod pane {
        include!("../../../crates/session/qos-desktop/src/activity/pane.rs");
        include!("room_activity_tests.rs");
    }
    mod replay {
        include!("../../../crates/session/qos-desktop/src/activity/replay.rs");
    }
}
