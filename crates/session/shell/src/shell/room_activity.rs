//! Native services for the shared workspace activity pane.
//!
//! The pane itself is [`tilo_desktop::activity::RoomActivityPane`]; this
//! module gives it the device's identity, its private seen frontiers and
//! departures (kept in the user's state directory), live agent dispatch through the session's
//! action broker, managed agent launches, and the native workspace tree as
//! its causal history source.
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    Workspace, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
    WorkspaceTree,
};
use std::{collections::HashMap, rc::Rc};
use tilo_desktop::activity::{
    ActivityHost, ActivityTree, WorkspaceDeparture, WorkspaceSeenFrontier,
};
pub use tilo_desktop::activity::{ActivityView, Period, RoomActivityOutcome, RoomScope};

/// Refreshes a finished shortcut run waits for its changes to reach an open
/// document before it is given up.
const SHORTCUT_RUN_RECORD_ATTEMPTS: u32 = 240;

#[derive(Default)]
pub(crate) struct NativeActivityHost {
    broker: Option<crate::shell::collab_broker::AppActionBroker>,
    /// Shortcut runs the broker finished whose history is not recorded yet,
    /// with how many refreshes have looked for their changes.
    pub(crate) pending_shortcut_runs:
        std::cell::RefCell<Vec<(crate::shell::automation_host::FinishedShortcutRun, u32)>>,
}

/// `$XDG_STATE_HOME/tilo/activity-history.json`: what this device has seen of
/// each workspace and when it last left one. Private viewer state rewritten on
/// every workspace switch, so it stays out of `settings.json`, whose writes
/// every settings reader treats as a preference change to re-apply.
fn history_path() -> Option<std::path::PathBuf> {
    xdg_paths::state_dir().map(|root| root.join("tilo/activity-history.json"))
}

/// `key`'s per-workspace map from the history file at `path`. A missing or
/// malformed file reads as empty.
fn load_history_map_at<T: serde::de::DeserializeOwned>(
    path: Option<&std::path::Path>,
    key: &str,
) -> HashMap<u64, T> {
    let doc: serde_json::Map<String, serde_json::Value> = path
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    doc.get(key)
        .and_then(|value| value.as_object())
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

/// Replace `key`'s map in the history file at `path`, keeping its other maps:
/// directory `0700`, file `0600`, written atomically.
fn save_history_map_at<T: serde::Serialize>(
    path: &std::path::Path,
    key: &str,
    values: &HashMap<u64, T>,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut doc: serde_json::Map<String, serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let values = values
        .iter()
        .filter_map(|(id, value)| {
            serde_json::to_value(value)
                .ok()
                .map(|value| (id.to_string(), value))
        })
        .collect();
    doc.insert(key.into(), serde_json::Value::Object(values));
    let bytes = serde_json::to_vec(&doc).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
}

fn load_history_map<T: serde::de::DeserializeOwned>(key: &str) -> HashMap<u64, T> {
    load_history_map_at(history_path().as_deref(), key)
}

fn save_history_map<T: serde::Serialize>(key: &str, values: &HashMap<u64, T>) {
    let Some(path) = history_path() else {
        return;
    };
    if let Err(error) = save_history_map_at(&path, key, values) {
        tracing::warn!(%error, key, "could not persist history state");
    }
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
        load_history_map("seen_frontiers")
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        save_history_map("seen_frontiers", frontiers);
    }
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        load_history_map("departures")
    }
    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        save_history_map("departures", departures);
    }
    /// Record each shortcut run the broker finished as one undoable run in
    /// the workspace document, once its changes have reached documents open
    /// here. A run whose changes never arrive is given up after a while: it
    /// changed nothing this desktop can undo.
    fn record_finished_runs(
        &self,
        doc: &mut automerge::AutoCommit,
        records: &ui::record::RecordStore,
    ) -> bool {
        let mut pending = self.pending_shortcut_runs.borrow_mut();
        if let Some(broker) = &self.broker {
            pending.extend(
                broker
                    .take_finished_shortcut_runs()
                    .into_iter()
                    .map(|finished| (finished, 0)),
            );
        }
        if pending.is_empty() {
            return false;
        }
        let open = records.open_records();
        let mut recorded = false;
        pending.retain_mut(|(finished, attempts)| {
            let shares = open
                .iter()
                .filter_map(|checkout| {
                    records
                        .with(&checkout.id, |doc| {
                            crate::shell::automation_host::document_share(
                                finished.report.run,
                                checkout.id.as_str(),
                                doc,
                            )
                        })
                        .flatten()
                })
                .collect();
            let steps = crate::shell::automation_host::ordered_steps(shares);
            if crate::shell::automation_host::record_run_steps(doc, finished, &steps).is_some() {
                recorded = true;
                return false;
            }
            *attempts = attempts.saturating_add(1);
            *attempts < SHORTCUT_RUN_RECORD_ATTEMPTS && !finished.report.invocations().is_empty()
        });
        recorded
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

/// The shared pane with the native services installed.
pub struct RoomActivityPane(
    tilo_desktop::activity::RoomActivityPane,
    Option<crate::shell::collab_broker::AppActionBroker>,
);

impl std::ops::Deref for RoomActivityPane {
    type Target = tilo_desktop::activity::RoomActivityPane;
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
        let mut pane = tilo_desktop::activity::RoomActivityPane::new();
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(pane, None)
    }
    /// The production pane: the device's private seen frontiers and
    /// departures are read from and written to the user's state directory.
    pub fn new_persistent() -> Self {
        let mut pane = Self::new();
        pane.load_frontiers();
        pane
    }
    pub(crate) fn with_local_user(user: String) -> Self {
        let mut pane = tilo_desktop::activity::RoomActivityPane::with_local_user(user);
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(pane, None)
    }
    /// Attach the live broker after the session binds. Without it, agent
    /// tasks remain visible but cannot be launched into an offline queue.
    pub fn attach_task_broker(&mut self, broker: crate::shell::collab_broker::AppActionBroker) {
        self.1 = Some(broker.clone());
        self.set_host(Rc::new(NativeActivityHost {
            broker: Some(broker),
            pending_shortcut_runs: std::cell::RefCell::default(),
        }));
    }
    /// Whether `agent` is admitted to `room` and holds a live task
    /// subscription right now. Without a broker no agent is ever present.
    pub fn agent_is_present_in(&self, room: &str, agent: &str) -> bool {
        self.1
            .as_ref()
            .is_some_and(|broker| broker.agent_is_live_task_capable(room, agent))
    }
    /// Dispatch a queued task in `room` to its assigned live agent, for
    /// surfaces outside Task Center that hand work over directly.
    ///
    /// # Errors
    ///
    /// Returns an error when no broker is attached or the assigned agent
    /// cannot accept the task.
    pub fn dispatch_task_in(&self, room: &str, task_id: &str) -> Result<u64, String> {
        self.1
            .as_ref()
            .ok_or_else(|| "agent task dispatch is unavailable".to_string())?
            .dispatch_task(room, task_id)
    }
    pub fn apply_deep_link(&mut self, focus: crate::shell::chrome_state::ActivityFocus) {
        self.0
            .apply_deep_link(tilo_desktop::activity::ActivityDeepLink {
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
) -> tilo_desktop::activity::AgentLaunchEvent {
    use crate::shell::launcher::LaunchEventKind as Native;
    use tilo_desktop::activity::AgentLaunchEventKind as Shared;
    tilo_desktop::activity::AgentLaunchEvent {
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable};
    use ui::record::{RecordId, RecordStore};

    /// Leaving a workspace rewrites the history file and nothing else: a
    /// write to `settings.json` would make every settings reader re-apply
    /// its preferences on each workspace switch.
    #[test]
    fn workspace_history_keeps_both_maps_in_its_own_private_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state/tilo/activity-history.json");
        let departures = HashMap::from([(
            7_u64,
            WorkspaceDeparture {
                departed_at_secs: 1_700_000_000,
                frontier: WorkspaceSeenFrontier::default(),
            },
        )]);
        let frontiers = HashMap::from([(9_u64, WorkspaceSeenFrontier::default())]);
        save_history_map_at(&path, "departures", &departures).expect("departures saved");
        save_history_map_at(&path, "seen_frontiers", &frontiers).expect("frontiers saved");

        let loaded: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&path), "departures");
        assert_eq!(
            loaded.get(&7).map(|departure| departure.departed_at_secs),
            Some(1_700_000_000),
            "saving the frontiers keeps the departures beside them"
        );
        let loaded: HashMap<u64, WorkspaceSeenFrontier> =
            load_history_map_at(Some(&path), "seen_frontiers");
        assert!(loaded.contains_key(&9));
        let mode = std::fs::metadata(&path)
            .expect("saved")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            history_path().is_none_or(|history| Some(history) != settings_store::settings_path()),
            "history must not live in settings.json"
        );
        let missing: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&dir.path().join("absent.json")), "departures");
        assert!(missing.is_empty());
    }

    /// A shortcut run the broker finished lands in the workspace document
    /// as one undoable run as soon as a document it changed is open here.
    #[test]
    fn a_finished_shortcut_run_is_recorded_once_from_the_documents_it_changed() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("drafts.qdoc");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document"));
        assert!(records.open_record(&checkout));
        let run = tilo_automation::RunId::new();
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &room.to_string());
            doc.put(automerge::ROOT, "draft", "Launch notes").unwrap();
            let context = ui::tilo_actions::ActionContext::for_resource(
                "Write draft",
                "fixture.drafts:inbox",
                "fixture.drafts",
                "Inbox",
            )
            .with_app("so.tilo.fixture-drafts", "Drafts")
            .with_group(run.0.to_string(), "Publish a draft", "shortcut");
            doc.commit_with(
                CommitOptions::default().with_message(context.change_message().unwrap()),
            );
        });
        let shortcut: tilo_automation::Shortcut = serde_json::from_value(
            serde_json::json!({"id": "publish", "name": "Publish a draft", "steps": []}),
        )
        .unwrap();
        let finished = (
            crate::shell::automation_host::FinishedShortcutRun {
                room: room.to_string(),
                shortcut,
                invoker: "ada".into(),
                report: tilo_automation::RunReport {
                    run,
                    shortcut: "publish".into(),
                    trigger: "manual".into(),
                    mode: tilo_automation::RunMode::Unattended,
                    steps: Vec::new(),
                    output: None,
                    outcome: tilo_automation::RunOutcome::Completed,
                },
            },
            0,
        );
        let host = Rc::new(NativeActivityHost {
            broker: None,
            pending_shortcut_runs: std::cell::RefCell::new(vec![finished]),
        });
        let mut pane = tilo_desktop::activity::RoomActivityPane::new();
        pane.set_host(host.clone());

        pane.refresh_records(&mut tree, &records);
        pane.refresh_records(&mut tree, &records);

        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1, "recorded exactly once");
        assert_eq!(runs[0].label, "Publish a draft");
        assert_eq!(runs[0].issued_by.as_deref(), Some("ada"));
        assert_eq!(runs[0].steps[0].record, record.to_string());
        assert_eq!(runs[0].steps[0].app, "so.tilo.fixture-drafts");
        let pending_left = host.pending_shortcut_runs.borrow().len();
        drop(host);
        assert_eq!(pending_left, 0, "the recorded run is no longer pending");
    }
}
