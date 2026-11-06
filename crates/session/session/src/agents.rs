//! The agents each workspace keeps, as its own record names them.
//!
//! An agent works in a workspace because Chat's record of the workspace's
//! agents ([`tilo_app::tilo_workspace_agent`]) names it and names the person
//! who keeps it. That record is the agent's admission: the session reads it
//! every pass, as the person sitting here, and the broker admits an agent's
//! session only for the keeper the record names
//! ([`tilo_action_broker::services::AgentRoster`]). Nothing here keeps state
//! of its own beyond the record this device has open: who works here is the
//! record's.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_action_policy::{Standing, WorkspacePolicy};
use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_agent::{KIND, OWNER, WorkspaceAgents as Record};
use ui::permissions::{LivePermissions, PermissionContext};
use ui::record::workspace::WorkspaceRecords;
use ui::record::{RecordId, RecordOwners};

/// How this session names a record it opened, where the record's history is
/// told.
const SESSION_APP: (&str, &str) = ("tilo-session", "Workspace settings");

/// Who keeps each of the workspaces' agents, as the pass read their records.
#[derive(Default)]
pub(crate) struct Roster {
    keepers: Mutex<BTreeMap<(String, String), String>>,
}

impl tilo_action_broker::services::AgentRoster for Roster {
    fn keeper(&self, workspace: &str, agent: &str) -> Option<String> {
        self.keepers
            .lock()
            .ok()?
            .get(&(workspace.to_owned(), agent.to_owned()))
            .cloned()
    }
}

/// The workspaces' agents, read from each workspace's record of them.
pub(crate) struct Agents {
    roster: Arc<Roster>,
    /// Where this device keeps its apps' records.
    owners: RecordOwners,
    /// Wakes the session's loop when a change to a record lands.
    wake: ui::WakeScope,
    /// Each workspace's records and its agents record, opened here and kept
    /// at the grants they were opened with.
    open: RefCell<BTreeMap<String, Opened>>,
}

struct Opened {
    /// The grants the record was opened with: a workspace whose grants
    /// changed has it opened again, so its people's reads follow.
    policy: WorkspacePolicy,
    records: WorkspaceRecords,
    /// `None` where the workspace holds no agents record: this reads one,
    /// never writes one.
    record: Option<WorkspaceRecord<Record>>,
}

impl Agents {
    /// The workspaces' agents over the store at `root`, waking the session's
    /// loop through `wake` when a change lands.
    pub(crate) fn new(root: PathBuf, wake: tilo_service::Wake) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the workspaces' agents are not woken");
        }
        Self {
            roster: Arc::default(),
            owners: RecordOwners::new(root),
            wake: scope,
            open: RefCell::default(),
        }
    }

    /// Who keeps each workspace's agents, as the broker reads them.
    pub(crate) fn roster(&self) -> Arc<Roster> {
        Arc::clone(&self.roster)
    }

    /// Read each workspace's agents record as `person`, and hand who keeps
    /// each agent to the broker.
    pub(crate) fn feed(&self, person: &str, workspaces: &[(String, WorkspacePolicy)]) {
        let mut keepers = BTreeMap::new();
        for (workspace, policy) in workspaces {
            let Some(record) = self.record(workspace, policy, person) else {
                continue;
            };
            let state = record.get();
            for agent in state.active() {
                keepers.insert(
                    (workspace.clone(), agent.id.0.clone()),
                    agent.keeper.clone(),
                );
            }
        }
        if let Ok(mut held) = self.roster.keepers.lock() {
            *held = keepers;
        }
    }

    /// The workspace's agents record, opened here where it is not, at the
    /// grants the pass read.
    fn record(
        &self,
        workspace: &str,
        policy: &WorkspacePolicy,
        person: &str,
    ) -> Option<WorkspaceRecord<Record>> {
        let mut open = self.open.borrow_mut();
        if open
            .get(workspace)
            .is_some_and(|opened| opened.policy != *policy)
        {
            open.remove(workspace);
        }
        let opened = open.entry(workspace.to_owned()).or_insert_with(|| Opened {
            policy: policy.clone(),
            records: WorkspaceRecords::in_workspace(
                self.owners.clone(),
                workspace.to_owned(),
                ui::permissions::UserId(person.to_owned()),
            ),
            record: None,
        });
        if opened.record.is_none() {
            let id = RecordId::package_primary();
            let held = opened
                .records
                .holds_record(OWNER, id.as_str(), KIND)
                .unwrap_or(false);
            if !held {
                return None;
            }
            let permissions = PermissionContext::from_standing(
                workspace,
                Standing::person(policy.clone(), person.to_owned()),
            )
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
            opened.record = WorkspaceRecord::<Record>::open_at(
                &opened.records,
                OWNER,
                id.as_str(),
                SESSION_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
        }
        opened.record.clone()
    }
}
