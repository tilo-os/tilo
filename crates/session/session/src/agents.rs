//! The agents each workspace keeps, as its own record names them.
//! An agent works in a workspace because Chat's record of the workspace's agents
//! ([`tilo_app::tilo_workspace_agent`]) names it and names the person who keeps it. That record is
//! the agent's admission: the session reads it every pass, as the person sitting here, and the
//! broker admits an agent's session only for the person the record names
//! ([`tilo_broker_core::services::AgentRoster`]). Nothing here keeps state of its own beyond the
//! record this device has open: who works here is the record's.
//! The MCP server adds a member to that record between two passes, and tells its client it may
//! join at once: the member it added is listed for the broker there
//! ([`crate::Shared::mcp_member`]) before anyone is told to join as it, and a pass that cannot
//! read the record keeps that listing.
//! The agents the person here keeps ([`Kept`]) are whom what they type is handed to. Their turns
//! are worked by the workspace's agent engine, which this device's workspace service runs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tilo_action_policy::{MemberAccess, WorkspacePolicy};
use tilo_app::framework::WorkspaceResource;
use tilo_app::tilo_workspace_agent::{KIND, OWNER, WorkspaceAgents as Record};
use ui::permissions::{LivePermissions, PermissionContext};
use ui::resource::workspace::WorkspaceResources;

/// How this session names a record it opened, where the record's history is told.
const SESSION_APP: (&str, &str) = ("tilo-session", "Workspace settings");

/// An agent the person sitting here keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    /// The workspace whose record names it.
    pub workspace: String,
    /// Its id, which its actor works as (`agent:<id>`).
    pub id: String,
    /// Its name, as people read it.
    pub name: String,
}

pub(crate) use crate::search::ListedAgent as Listed;

/// Who keeps each of the workspaces' agents, and what each may spend today,
/// as the session last read their records: every pass, and when its MCP server added a member.
#[derive(Default)]
pub(crate) struct Roster {
    listed: Mutex<BTreeMap<String, Vec<tilo_broker_core::services::ListedAgent>>>,
    /// What each agent may spend today and has, by workspace and agent: what a run of its is
    /// admitted with.
    spend: Mutex<BTreeMap<(String, String), runtime_collab::AgentSpend>>,
}

impl tilo_broker_core::services::AgentRoster for Roster {
    fn listed(&self, workspace: &str) -> Vec<tilo_broker_core::services::ListedAgent> {
        self.listed
            .lock()
            .ok()
            .and_then(|listed| listed.get(workspace).cloned())
            .unwrap_or_default()
    }

    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::AgentSpend> {
        self.spend
            .lock()
            .ok()?
            .get(&(workspace.to_owned(), agent.to_owned()))
            .cloned()
    }
}

impl Roster {
    /// List `agent` in `workspace` for the broker, as the workspace's record names it: the member
    /// the MCP server just answered for, which the broker reads before the pass that reads the
    /// record again.
    fn list(&self, workspace: &str, agent: tilo_broker_core::services::ListedAgent) {
        if let Ok(mut held) = self.listed.lock() {
            let listed = held.entry(workspace.to_owned()).or_default();
            match listed.iter_mut().find(|kept| kept.id == agent.id) {
                Some(kept) => *kept = agent,
                None => listed.push(agent),
            }
        }
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
    records: WorkspaceResources,
    /// `None` until it opens: the workspace's agents record, opened once and kept.
    record: Option<WorkspaceResource<Record>>,
}

/// What a workspace's record of its agents is to a reader this pass.
enum Held {
    /// The workspace holds no agents record: this reads one, never writes one.
    None,
    /// The record could not be opened or read now: what it says is not known this pass.
    Unknown,
    Open(WorkspaceResource<Record>),
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
    /// and answer the agents `person` keeps beside every agent each workspace lists, whoever
    /// keeps it.
    pub(crate) fn feed(
        &self,
        person: &str,
        workspaces: &[(String, WorkspacePolicy)],
    ) -> (Vec<Kept>, Vec<Listed>) {
        let day = ui::clock::today().to_string();
        // Who keeps each agent, as the broker reads them, started from what it read last: a
        // workspace whose record cannot be read this pass keeps its listing, so a read that
        // failed never says an agent it named before is gone.
        let mut listed: BTreeMap<String, Vec<tilo_broker_core::services::ListedAgent>> = self
            .roster
            .listed
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default();
        listed.retain(|workspace, _| workspaces.iter().any(|(held, _)| held == workspace));
        // What each agent may spend, kept the same way: a workspace read this pass replaces it.
        let mut spend: BTreeMap<(String, String), runtime_collab::AgentSpend> = self
            .roster
            .spend
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default();
        spend.retain(|(workspace, _), _| workspaces.iter().any(|(held, _)| held == workspace));
        let mut kept = Vec::new();
        let mut every = Vec::new();
        for (workspace, policy) in workspaces {
            let record = match self.record(workspace, policy, person) {
                Held::None => {
                    listed.remove(workspace);
                    spend.retain(|(held, _), _| held != workspace);
                    continue;
                }
                // Not readable this pass: what it named stays listed.
                Held::Unknown => continue,
                Held::Open(record) => record,
            };
            let Some(state) = record.try_get() else {
                continue;
            };
            spend.retain(|(held, _), _| held != workspace);
            let mut named = Vec::new();
            // Every agent the workspace's list names, retired ones too: a retired agent takes
            // no work, but what it said before it retired is still read by its name.
            for agent in state
                .agents
                .iter()
                .filter(|agent| !agent.name.trim().is_empty())
            {
                every.push(Listed {
                    workspace: workspace.clone(),
                    id: agent.id.0.clone(),
                    name: agent.name.clone(),
                    retired: !agent.is_active(),
                });
                if !agent.is_active() {
                    continue;
                }
                named.push(tilo_broker_core::services::ListedAgent {
                    id: agent.id.0.clone(),
                    name: agent.name.clone(),
                    acts_for: agent.acts_for.clone(),
                });
                spend.insert(
                    (workspace.clone(), agent.id.0.clone()),
                    runtime_collab::AgentSpend {
                        day: day.clone(),
                        limit_micros: agent.spend_limit_micros,
                        spent_micros: agent.spent_on(&day),
                    },
                );
                // An MCP agent connects on its own: what the person types goes to the others.
                if agent.mcp_client.is_none() && agent.acts_for == person {
                    kept.push(Kept {
                        workspace: workspace.clone(),
                        id: agent.id.0.clone(),
                        name: agent.name.clone(),
                    });
                }
            }
            listed.insert(workspace.clone(), named);
        }
        if let Ok(mut held) = self.roster.listed.lock() {
            *held = listed;
        }
        if let Ok(mut held) = self.roster.spend.lock() {
            *held = spend;
        }
        (kept, every)
    }

    /// The workspace's own agent in `workspace`, as `(id, name)`, read as `person`: the one no MCP
    /// client connects as that the record lists first, by its own order — the agent the canvas
    /// asks (shell.md §8.1). `None` where the workspace has none.
    pub(crate) fn assistant(
        &self,
        workspace: &str,
        policy: &WorkspacePolicy,
        person: &str,
    ) -> Option<(String, String)> {
        let Held::Open(record) = self.record(workspace, policy, person) else {
            return None;
        };
        let state = record.get();
        state
            .made_by_people()
            .find(|agent| agent.mcp_client.is_none())
            .map(|agent| (agent.id.0.clone(), agent.name.clone()))
    }

    /// The workspace's agents record, opened here where it is not, at the grants the pass read: a
    /// workspace that keeps none is given none, so one nobody set an agent up in costs no record.
    fn record(&self, workspace: &str, policy: &WorkspacePolicy, person: &str) -> Held {
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
            let id = collab_types::workspace_document_id(KIND);
            match opened.records.holds_resource(OWNER, &id, KIND) {
                Ok(false) => return Held::None,
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!(%error, %workspace, "the workspace's agents are not read");
                    return Held::Unknown;
                }
            }
            let permissions = PermissionContext::from_access(MemberAccess::person(
                policy.clone(),
                person.to_owned(),
            ))
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
            opened.record = WorkspaceResource::<Record>::open(
                &opened.records,
                OWNER,
                SESSION_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
        }
        match &opened.record {
            Some(record) => Held::Open(record.clone()),
            None => Held::Unknown,
        }
    }
}

impl crate::Shared {
    /// Feed the agents every pass what their work is decided against: every workspace with the
    /// grants its people gave. Who keeps each workspace's agents
    /// is read from the workspace's own record of them, as the broker admits an agent's session on,
    /// and its actions are admitted as the grants of the person it acts for say.
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
        let (kept, listed) = self.agents.feed(&self.person, &workspaces);
        self.search.set_kept_agents(&kept);
        self.search.set_listed_agents(&listed);
        // Asks are the agent package's resources now: the session feeds no turns from
        // conversations, and its agents answer none of their own here.
    }

    /// The person's agent member for the MCP client `client` in `workspace`, by id and by the name
    /// the workspace calls it, as the MCP server
    /// admits a connection ([`crate::mcp_server::Members`]): the one the workspace's record of its
    /// agents lists, else a new one ([`crate::workspace_settings::Feeding::mcp_agent`]), listed
    /// for the broker as the record names it. The server tells the connection it may join on this
    /// answer, and the connection joins the broker on a thread of its own, at once — before any
    /// pass reads the record again — so the member is listed here, from what the record said.
    ///
    /// # Errors
    /// The member could not be found or added: the client is refused here, told so, and not at its
    /// join.
    pub(crate) fn mcp_member(
        &self,
        workspace: &str,
        client: &str,
    ) -> Result<(String, String), String> {
        let (agent, name) = self
            .workspace_settings_feed()
            .mcp_agent(workspace, client)?;
        self.agents.roster().list(
            workspace,
            tilo_broker_core::services::ListedAgent {
                id: agent.clone(),
                name: name.clone(),
                acts_for: self.person.clone(),
            },
        );
        Ok((agent, name))
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

    /// The MCP server tells a connection it may join as soon as the session answers it the
    /// person's agent member for its client, and the connection joins the broker on a thread of
    /// its own, at once. So the answer itself lists the member where the broker reads who works
    /// in the workspace: a member this call added to the workspace's record is the person's in
    /// the broker's roster when the call returns, with no pass of the session in between. On a
    /// machine no agent worked on, that is the first client the person approves.
    #[test]
    fn a_clients_agent_member_is_listed_for_the_broker_when_the_mcp_server_answers() {
        let _settled = ui::motion::testing::settled();
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let session = session(state.path());
        let roster = session.shared.agents.roster();
        assert!(
            roster.listed(STUDIO).is_empty(),
            "nobody set an agent up in this workspace yet"
        );

        // Before any pass: what the MCP server answers is all the broker has when the
        // connection joins.
        let (member, name) = session
            .shared
            .mcp_member(STUDIO, "claude-code")
            .expect("the person's agent member for the client is added");
        let acts_for = roster.acts_for(STUDIO, &member);
        assert!(
            acts_for
                .as_deref()
                .is_some_and(|acts_for| acts_for == PERSON),
            "the broker lists the member {member} as the person's before its client is told it \
             may join: {acts_for:?}"
        );

        // The same client again is the same member, found and still listed: no second one.
        assert_eq!(
            session.shared.mcp_member(STUDIO, "claude-code"),
            Ok((member.clone(), name))
        );
        assert_eq!(
            roster.listed(STUDIO).len(),
            1,
            "one member for the client, whichever connection it comes in on"
        );

        // The passes that follow read the record again and keep the one listing: the member the
        // MCP server answered for stays the broker's, and no second row for it appears.
        session.shared.feed_agents();
        let listed = roster.listed(STUDIO);
        assert_eq!(listed.len(), 1, "one member after the pass: {listed:?}");
        assert_eq!(listed[0].id, member);
        assert_eq!(listed[0].acts_for, PERSON);
    }
}
