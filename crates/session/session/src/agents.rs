//! The agents each workspace keeps, as its own record names them.
//! An agent works in a workspace because Chat's record of the workspace's agents
//! ([`tilo_app::tilo_workspace_agent`]) names it and names the person who keeps it. That record is
//! the agent's admission: the session reads it every pass, as the person sitting here, and the
//! broker admits an agent's session only for the keeper the record names
//! ([`tilo_broker_core::services::AgentRoster`]). Nothing here keeps state of its own beyond the
//! record this device has open: who works here is the record's.
//! The agent door adds a member to that record between two passes, and tells its client it may
//! join at once: the record is read again there ([`crate::Shared::mcp_member`]), so the broker
//! lists a member before anyone is told to join as it.
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

pub(crate) use crate::search::ListedAgent as Listed;

/// Who keeps each of the workspaces' agents, and what each may spend today,
/// as the session last read their records: every pass, and when its agent door added a member.
#[derive(Default)]
pub(crate) struct Roster {
    listed: Mutex<BTreeMap<String, Vec<tilo_broker_core::services::ListedAgent>>>,
    /// What each agent may spend today and has, by workspace and agent: what a run of its is
    /// admitted with.
    spend: Mutex<BTreeMap<(String, String), runtime_collab::TaskRunSpend>>,
}

impl tilo_broker_core::services::AgentRoster for Roster {
    fn listed(&self, workspace: &str) -> Vec<tilo_broker_core::services::ListedAgent> {
        self.listed
            .lock()
            .ok()
            .and_then(|listed| listed.get(workspace).cloned())
            .unwrap_or_default()
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
    /// and answer the agents kept here — this device runs those — beside every agent each
    /// workspace lists, whoever keeps it.
    pub(crate) fn feed(
        &self,
        person: &str,
        workspaces: &[(String, WorkspacePolicy)],
    ) -> (Vec<Kept>, Vec<Listed>) {
        let day = ui::clock::today().to_string();
        let mut listed: BTreeMap<String, Vec<_>> = BTreeMap::new();
        let mut spend = BTreeMap::new();
        let mut kept = Vec::new();
        let mut every = Vec::new();
        for (workspace, policy) in workspaces {
            let Some(record) = self.record(workspace, policy, person) else {
                continue;
            };
            let state = record.get();
            for agent in state.active() {
                every.push(Listed {
                    workspace: workspace.clone(),
                    id: agent.id.0.clone(),
                    name: agent.name.clone(),
                });
                listed.entry(workspace.clone()).or_default().push(
                    tilo_broker_core::services::ListedAgent {
                        id: agent.id.0.clone(),
                        name: agent.name.clone(),
                        keeper: agent.keeper.clone(),
                    },
                );
                spend.insert(
                    (workspace.clone(), agent.id.0.clone()),
                    runtime_collab::TaskRunSpend {
                        day: day.clone(),
                        limit_micros: agent.spend_limit_micros,
                        spent_micros: agent.spent_on(&day),
                    },
                );
                // An MCP agent connects on its own; the keeper's device runs every other one.
                if agent.mcp_client.is_none() && same_person(&agent.keeper, person) {
                    kept.push(Kept {
                        workspace: workspace.clone(),
                        id: agent.id.0.clone(),
                        name: agent.name.clone(),
                        model: agent.model.clone(),
                    });
                }
            }
        }
        if let Ok(mut held) = self.roster.listed.lock() {
            *held = listed;
        }
        if let Ok(mut held) = self.roster.spend.lock() {
            *held = spend;
        }
        (kept, every)
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
            .find(|agent| agent.mcp_client.is_none() && same_person(&agent.keeper, person))
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
            records: self.records.in_workspace(
                workspace,
                ui::permissions::MemberId::person(person.to_owned()),
            ),
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
            let permissions = PermissionContext::from_standing(Standing::person(
                policy.clone(),
                person.to_owned(),
            ))
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
        let (kept, listed) = self.agents.feed(&self.person, &workspaces);
        self.search.set_kept_agents(&kept);
        self.search.set_listed_agents(&listed);
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
        // A turn the person asked on the canvas stands where they asked once Chat said which
        // message it posted for the ask and its record holds it (canvas.md §3).
        let mut arrangement = self.arrangement.borrow_mut();
        if arrangement.awaits_turns() {
            // What Chat answered the asks' sends, once they settled.
            self.search.poll_answers();
            arrangement.stand_asked(
                &|sent| {
                    let message = self.sends.posted(sent)?;
                    self.conversations.asked_turn(&message)
                },
                tilo_session_core::arrangement::Busy::default(),
            );
        }
    }

    /// The person's agent member for the MCP client `client` in `workspace`, as the agent door
    /// admits a connection ([`crate::mcp_server::Members`]): the one the workspace's record of its
    /// agents lists, else a new one ([`crate::workspace_settings::Feeding::mcp_agent`]), answered
    /// once the roster the broker reads lists it. The door tells the connection it may join on
    /// this answer, and the connection joins the broker on a thread of its own, at once: a member
    /// the record names since this call is one no pass has read, so the records are read again
    /// here ([`Self::feed_agents`]) rather than on the pass that follows.
    ///
    /// # Errors
    /// The member could not be found or added, or the workspace's agents as this session reads
    /// them still do not list it: the client is refused here, told so, and not at its join.
    pub(crate) fn mcp_member(&self, workspace: &str, client: &str) -> Result<String, String> {
        use tilo_broker_core::services::AgentRoster as _;
        let agent = self
            .workspace_settings_feed()
            .mcp_agent(workspace, client)?;
        let roster = self.agents.roster();
        if roster.keeper(workspace, &agent).is_none() {
            self.feed_agents();
        }
        if roster.keeper(workspace, &agent).is_none() {
            tracing::warn!(
                %workspace,
                %agent,
                %client,
                "the workspace's agents as read here do not list a client's agent member"
            );
            return Err(tilo_sheets::word("mcp-server-unlisted"));
        }
        Ok(agent)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tilo_broker_core::services::AgentRoster as _;
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

    use super::same_person;

    /// The person at the shell, who owns its workspace.
    const PERSON: &str = "ada";
    /// The workspace in front of them, where nobody set an agent up yet.
    const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";

    /// A headless session at a shell whose one workspace is the person's, its passes run until the
    /// shell stands.
    fn session(state: &std::path::Path) -> crate::Session {
        let mut session = crate::Session::headless(
            crate::Options {
                person: PERSON.to_owned(),
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(state.to_path_buf()),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session");
        let mut studio = Workspace::new(
            WorkspaceId::parse(STUDIO).expect("a workspace id"),
            "Studio",
        );
        studio.set_owner(PERSON);
        session.handle().replace_tree(Layout {
            active: studio.id(),
            workspaces: vec![studio],
            ..Layout::default()
        });
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        session
    }

    /// The agent door tells a connection it may join as soon as the session answers it the
    /// person's agent member for its client, and the connection joins the broker on a thread of
    /// its own, at once. So the answer itself lists the member where the broker reads who works
    /// in the workspace: a member this call added to the workspace's record is the person's in
    /// the broker's roster when the call returns, with no pass of the session in between. On a
    /// machine no agent worked on, that is the first client the person approves.
    #[test]
    fn a_clients_agent_member_is_listed_for_the_broker_when_the_door_is_answered() {
        let _settled = ui::motion::testing::settled();
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let session = session(state.path());
        let roster = session.shared.agents.roster();
        assert!(
            roster.listed(STUDIO).is_empty(),
            "nobody set an agent up in this workspace yet"
        );

        // No pass of the session from here on: what the door is answered is all the broker has
        // when the connection joins.
        let member = session
            .shared
            .mcp_member(STUDIO, "claude-code")
            .expect("the person's agent member for the client is added");
        let keeper = roster.keeper(STUDIO, &member);
        assert!(
            keeper
                .as_deref()
                .is_some_and(|keeper| same_person(keeper, PERSON)),
            "the broker lists the member {member} as the person's before its client is told it \
             may join: {keeper:?}"
        );

        // The same client again is the same member, found and still listed: no second one.
        assert_eq!(session.shared.mcp_member(STUDIO, "claude-code"), Ok(member));
        assert_eq!(
            roster.listed(STUDIO).len(),
            1,
            "one member for the client, whichever connection it comes in on"
        );
    }
}
