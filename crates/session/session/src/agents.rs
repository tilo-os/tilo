//! The agents each workspace keeps, as its own record names them.
//! An agent works in a workspace because Chat's record of the workspace's agents
//! ([`tilo_app::tilo_workspace_agent`]) names it and names the person who keeps it. That record is
//! the agent's admission: the session reads it every pass, as the person sitting here, and the
//! broker admits an agent's session only for the keeper the record names
//! ([`tilo_broker_core::services::AgentRoster`]). Nothing here keeps state of its own beyond the
//! record this device has open: who works here is the record's.
//! The person who keeps an agent runs it: the pass hands the agents kept here to the session's
//! launches ([`Kept`]), which start each one's process — the record is who works, and whose device
//! runs them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tilo_action_policy::{Standing, WorkspacePolicy, same_person};
use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_agent::{
    KIND, OWNER, WorkspaceAgents as Record, workspace_agents as actions,
};
use tilo_resource::Id;
use ui::permissions::{LivePermissions, PermissionContext};
use ui::record::workspace::WorkspaceRecords;

/// How this session names a record it opened, where the record's history is told.
const SESSION_APP: (&str, &str) = ("tilo-session", "Workspace settings");

/// An agent this device runs because the person sitting here keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    /// The workspace whose record names it.
    pub workspace: String,
    /// Its id, which its actor works as (`agent:<id>`).
    pub id: String,
    /// Its name, as people read it.
    pub name: String,
    /// The model its keeper chose for it, as `provider/model` ; `None` follows the keeper's default
    /// here.
    pub model: Option<String>,
}

/// Who keeps each of the workspaces' agents, and what each may spend today,
/// as the pass read their records.
#[derive(Default)]
pub(crate) struct Roster {
    keepers: Mutex<BTreeMap<(String, String), String>>,
    /// What each agent may spend today and has, by workspace and agent: what a run of its is
    /// admitted with.
    spend: Mutex<BTreeMap<(String, String), runtime_collab::TaskRunSpend>>,
}

impl tilo_broker_core::services::AgentRoster for Roster {
    fn keeper(&self, workspace: &str, agent: &str) -> Option<String> {
        self.keepers
            .lock()
            .ok()?
            .get(&(workspace.to_owned(), agent.to_owned()))
            .cloned()
    }

    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::TaskRunSpend> {
        self.spend
            .lock()
            .ok()?
            .get(&(workspace.to_owned(), agent.to_owned()))
            .cloned()
    }
}

/// The workspaces' agents, read from each workspace's record of them.
pub(crate) struct Agents {
    roster: Arc<Roster>,
    /// The workspaces' records, as the workspace holds them.
    records: crate::engine_records::EngineRecords,
    /// Wakes the session's loop when a change to a record lands.
    wake: ui::WakeScope,
    /// Each workspace's records and its agents record, opened here and kept at the grants they were
    /// opened with.
    open: RefCell<BTreeMap<String, Opened>>,
}

struct Opened {
    /// The grants the record was opened with: a workspace whose grants changed has it opened again,
    /// so its people's reads follow.
    policy: WorkspacePolicy,
    records: WorkspaceRecords,
    /// `None` where the workspace holds no agents record: this reads one,
    /// never writes one.
    record: Option<WorkspaceRecord<Record>>,
}

impl Agents {
    /// The workspaces' agents in the workspaces' copy of them (`records`), waking the session's loop through `wake`
    /// when a change lands.
    pub(crate) fn new(
        records: crate::engine_records::EngineRecords,
        wake: tilo_service::Wake,
    ) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the workspaces' agents are not woken");
        }
        Self {
            roster: Arc::default(),
            records,
            wake: scope,
            open: RefCell::default(),
        }
    }

    /// Who keeps each workspace's agents, as the broker reads them.
    pub(crate) fn roster(&self) -> Arc<Roster> {
        Arc::clone(&self.roster)
    }

    /// Read each workspace's agents record as `person` , hand who keeps each agent to the broker,
    /// and answer the agents kept here: this device runs those.
    pub(crate) fn feed(&self, person: &str, workspaces: &[(String, WorkspacePolicy)]) -> Vec<Kept> {
        let day = ui::clock::today().to_string();
        let mut keepers = BTreeMap::new();
        let mut spend = BTreeMap::new();
        let mut kept = Vec::new();
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
                spend.insert(
                    (workspace.clone(), agent.id.0.clone()),
                    runtime_collab::TaskRunSpend {
                        day: day.clone(),
                        limit_micros: agent.spend_limit_micros,
                        spent_micros: agent.spent_on(&day),
                    },
                );
                if same_person(&agent.keeper, person) {
                    kept.push(Kept {
                        workspace: workspace.clone(),
                        id: agent.id.0.clone(),
                        name: agent.name.clone(),
                        model: agent.model.clone(),
                    });
                }
            }
        }
        if let Ok(mut held) = self.roster.keepers.lock() {
            *held = keepers;
        }
        if let Ok(mut held) = self.roster.spend.lock() {
            *held = spend;
        }
        kept
    }

    /// Charge what the runs' agents spent to their day, on each workspace's record of its agents,
    /// as the person sitting here: a keeper's device counts what the agents it runs spend.
    pub(crate) fn charge(
        &self,
        workspaces: &[(String, WorkspacePolicy)],
        person: &str,
        spent: &[(String, String, u64)],
    ) {
        if spent.is_empty() {
            return;
        }
        let day = ui::clock::today().to_string();
        for (workspace, agent, cost_micros) in spent {
            let Some((_, policy)) = workspaces.iter().find(|(held, _)| held == workspace) else {
                continue;
            };
            let Some(record) = self.record(workspace, policy, person) else {
                continue;
            };
            let counted =
                record.invoke(&Record::RECORD_AGENT_SPEND.with(actions::RecordAgentSpend {
                    agent: Id::from_raw(agent.clone()),
                    day: day.clone(),
                    cost_micros: *cost_micros,
                }));
            if let Err(error) = counted {
                tracing::warn!(%error, %workspace, %agent, "an agent's spend is not counted");
            }
        }
    }

    /// The agent `person` keeps in `workspace`, as `(id, name)`: whoever the record lists first, by
    /// its own order — the agent the canvas asks (shell.md §8.1). `None` where they keep none.
    pub(crate) fn kept(
        &self,
        workspace: &str,
        policy: &WorkspacePolicy,
        person: &str,
    ) -> Option<(String, String)> {
        let record = self.record(workspace, policy, person)?;
        let state = record.get();
        state
            .active()
            .find(|agent| same_person(&agent.keeper, person))
            .map(|agent| (agent.id.0.clone(), agent.name.clone()))
    }

    /// The workspace's agents record, opened here where it is not, at the grants the pass read: a
    /// workspace that keeps none is given none, so one nobody set an agent up in costs no record.
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
            records: self
                .records
                .in_workspace(workspace, ui::permissions::UserId(person.to_owned())),
            record: None,
        });
        if opened.record.is_none() {
            let id = collab_types::workspace_record_id(KIND);
            let held = opened
                .records
                .holds_record(OWNER, &id, KIND)
                .unwrap_or(false);
            if !held {
                return None;
            }
            let permissions = PermissionContext::from_standing(
                workspace,
                Standing::person(policy.clone(), person.to_owned()),
            )
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
            opened.record = WorkspaceRecord::<Record>::open(
                &opened.records,
                OWNER,
                SESSION_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
        }
        opened.record.clone()
    }
}

impl crate::Shared {
    /// Feed the agents every pass what their work is decided against: every workspace with the
    /// grants its people gave, and the turns the conversations of each give its agents. Who keeps
    /// each workspace's agents is read from the workspace's own record of them, as the broker
    /// admits an agent's session on: an agent works in the turns of the conversations it takes part
    /// in, and its actions are admitted as its keeper's grants say. The agents kept here are this
    /// device's to run: it starts each one's process, which answers the turns the broker delivers
    /// it.
    pub(crate) fn feed_agents(&self) {
        // What moved in the workspaces' records, and what this session wrote into them, before
        // anything here reads them.
        self.records.replicate();
        let workspaces: Vec<(String, tilo_action_policy::WorkspacePolicy)> =
            self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .map(|held| (held.id().to_string(), held.policy.clone()))
                    .collect()
            });
        self.agents
            .charge(&workspaces, &self.person, &self.conversations.take_spent());
        let kept = self.agents.feed(&self.person, &workspaces);
        self.search.set_kept_agents(&kept);
        if let Some(apps) = &self.launch {
            for agent in &kept {
                apps.start_agent(agent);
            }
        }
        self.conversations.feed(
            &self.person,
            workspaces
                .into_iter()
                .map(|(workspace, policy)| crate::conversations::WorkspaceFacts {
                    workspace,
                    policy,
                })
                .collect(),
        );
    }
}

/// Who a request typed in a workspace goes to: the agent the person keeps there, and a new one —
/// named as the workspace names its agent — where they keep none. Text typed on the canvas that
/// asks and search's row that hands an agent a request both send to it (shell.md §8.1).
pub(crate) fn agent_recipient(kept: Option<(String, String)>) -> ui::floating_surface::Recipient {
    use ui::floating_surface::{Recipient, RecipientKind};
    match kept {
        Some((id, name)) => Recipient::found(RecipientKind::Agent, id, name),
        None => Recipient::new_agent(crate::i18n::t("session-agent-name")),
    }
}
