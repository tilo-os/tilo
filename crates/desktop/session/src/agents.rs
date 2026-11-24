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

use tilo_action_policy::WorkspacePolicy;
use tilo_app::tilo_workspace_agent::{KIND, OWNER, WorkspaceAgents as Record};

use crate::followed_resource::FollowedResource;

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
    /// The device's daemon, where each workspace's list of its agents is read.
    daemon: tilo_session_client::daemon_documents::DaemonShell,
    /// Wakes the session's loop when a list moved.
    wake: tilo_service::Wake,
    /// Each workspace's list of its agents, followed here at the grants it was followed with.
    open: RefCell<BTreeMap<String, Opened>>,
}

struct Opened {
    /// The grants it was followed with: a workspace whose grants changed has it followed again.
    policy: WorkspacePolicy,
    followed: FollowedResource<Record>,
}

/// What a workspace's list of its agents is to a reader this pass.
enum Held {
    /// The workspace has no list of its agents: this reads one, never writes one.
    None,
    /// It is not read yet: what it says is not known this pass.
    Unknown,
    Open(FollowedResource<Record>),
}

impl Agents {
    /// The workspaces' agents, read at the device's `daemon`, waking the session's loop through
    /// `wake` when a list moved.
    pub(crate) fn new(
        daemon: tilo_session_client::daemon_documents::DaemonShell,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            roster: Arc::default(),
            daemon,
            wake,
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
            let state = record.get();
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
                    avatar: agent.avatar.clone(),
                    retired: !agent.is_active(),
                });
                if !agent.is_active() {
                    continue;
                }
                named.push(tilo_broker_core::services::ListedAgent {
                    id: agent.id.0.clone(),
                    name: agent.name.clone(),
                    avatar: agent.avatar.clone(),
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

    /// The workspace's list of its agents, followed here where it is not, at the grants the pass
    /// read: a workspace that keeps none is given none, so one nobody set an agent up in costs no
    /// list.
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
            followed: FollowedResource::follow(
                &self.daemon,
                (workspace, OWNER, &collab_types::workspace_document_id(KIND)),
                person,
                Some(policy.clone()),
                Arc::clone(&self.wake),
            ),
        });
        match opened.followed.held() {
            Some(true) => Held::Open(opened.followed.clone()),
            Some(false) => Held::None,
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
        // A turn the person asked on the canvas stands where they asked once Chat's record holds
        // the agent's answer to the message Chat posted for the ask (canvas.md §3).
        if self.layout.borrow().awaits_turns() {
            // What Chat answered the asks' sends, once they settled.
            self.search.poll_answers();
            let services = self.services.borrow();
            self.layout.borrow_mut().stand_asked(
                &|sent| {
                    let message = self.sends.posted(sent)?;
                    crate::asked_turns::asked_turn(&services, &message)
                },
                tilo_session_core::layout::Busy::default(),
            );
        }
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
                avatar: None,
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
    use tilo_broker_core::services::AgentRoster as _;
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

    use super::*;
    use crate::daemon_fixture::TestDaemon;

    /// The workspace the person works in, where nobody set an agent up yet.
    const STUDIO: &str = "8b0e8f5e-6c1f-4f4e-9d4a-1f2e3d4c5b6a";
    /// The MCP client whose agent the person approves.
    const CLIENT: &str = "claude-code";

    /// A headless session at the daemon `daemon`, its tree one workspace of the person's: the
    /// workspace's list of its agents is read and changed there, as a desktop's is.
    fn session(state: &std::path::Path, daemon: &TestDaemon) -> crate::Session {
        let person = hex::encode(daemon.certificate().user_public_key);
        let mut session = crate::Session::headless(
            crate::Options {
                person: person.clone(),
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(state.to_path_buf()),
                shell_link: Some(daemon.shell_link()),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session");
        let mut studio = Workspace::new(
            WorkspaceId::parse(STUDIO).expect("a workspace id"),
            "Studio",
        );
        studio.set_owner(person);
        session.handle().replace_tree(Layout {
            active: studio.id(),
            workspaces: vec![studio],
            ..Layout::default()
        });
        session
    }

    /// The id, name and MCP client of every agent the workspace's list holds at `daemon`.
    fn held(daemon: &TestDaemon, workspace: &str) -> Vec<(String, String, Option<String>)> {
        let record = collab_types::workspace_document_id(KIND);
        let Some(bytes) = tilo_rt::app()
            .block_on(
                daemon
                    .workspaces()
                    .resource_document(workspace, OWNER, &record),
            )
            .expect("the daemon reads the workspace")
        else {
            return Vec::new();
        };
        let doc = automerge::AutoCommit::load(&bytes).expect("a document");
        let reader = tilo_resource::Reader::<Record>::read(&doc).expect("the list of agents reads");
        reader
            .state()
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.id.0.clone(),
                    agent.name.clone(),
                    agent.mcp_client.clone(),
                )
            })
            .collect()
    }

    /// The MCP server tells a connection it may join as soon as the session answers it the
    /// person's agent member for its client, and the connection joins the broker on a thread of
    /// its own, at once. So the answer itself lists the member where the broker reads who works in
    /// the workspace: a member added to the workspace's list is the person's in the broker's
    /// roster when the call returns, with no pass of the session in between. On a machine no agent
    /// worked on, that is the first client the person approves.
    #[test]
    fn a_clients_agent_member_is_listed_for_the_broker_when_the_mcp_server_answers() {
        let _settled = ui::motion::testing::settled();
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let daemon = TestDaemon::start();
        let workspace = WorkspaceId::parse(STUDIO).expect("a workspace id");
        daemon.found(workspace, "Studio");
        let person = hex::encode(daemon.certificate().user_public_key);
        let session = session(state.path(), &daemon);
        let roster = session.shared.agents.roster();
        assert!(
            roster.listed(STUDIO).is_empty(),
            "nobody set an agent up in this workspace yet"
        );

        // Before any pass: what the MCP server answers is all the broker has when the connection
        // joins.
        let (member, name) = session
            .shared
            .mcp_member(STUDIO, CLIENT)
            .expect("the person's agent member for the client is added");
        let listed = roster.listed(STUDIO);
        assert_eq!(listed.len(), 1, "one member for the client: {listed:?}");
        assert_eq!(listed[0].id, member);
        assert_eq!(
            listed[0].acts_for, person,
            "the broker lists the member {} as the person's before its client is told it may \
             join",
            member
        );
        assert_eq!(
            name, CLIENT,
            "the workspace names the member for its client"
        );

        // The same client again is the same member, found and still listed: no second one.
        assert_eq!(
            session.shared.mcp_member(STUDIO, CLIENT),
            Ok((member.clone(), name.clone()))
        );
        assert_eq!(
            roster.listed(STUDIO).len(),
            1,
            "one member for the client, whichever connection it comes in on"
        );
        let recorded = held(&daemon, STUDIO);
        assert_eq!(
            recorded.len(),
            1,
            "the workspace's list holds one: {recorded:?}"
        );
        assert_eq!(recorded[0].0, member);
        assert_eq!(recorded[0].2.as_deref(), Some(CLIENT));

        // The passes that follow read the list again and keep the one listing: the member the MCP
        // server answered for stays the broker's, and no second row for it appears.
        tilo_test_env::wait_until("the workspace's list is read again", || {
            session.shared.feed_agents();
            roster.listed(STUDIO).len() == 1
        });
        let listed = roster.listed(STUDIO);
        assert_eq!(listed.len(), 1, "one member after the pass: {listed:?}");
        assert_eq!(listed[0].id, member);
        assert_eq!(listed[0].acts_for, person);
        assert_eq!(
            held(&daemon, STUDIO).len(),
            1,
            "and one agent in the workspace's list"
        );
    }

    /// An agent's persona, made and pictured through the record's own actions, reaches the
    /// names the broker hands a window: its name and its picture are what an author's face
    /// draws, wherever the workspace shows that agent.
    #[test]
    fn an_agents_persona_is_what_the_member_list_names_and_pictures_it_by() {
        /// A picture a persona may carry: a 1-by-1 PNG.
        const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";

        let _settled = ui::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let shell = Shell::new(&mut home, state.path());
        let session = &shell.session;
        let person = shell.person.clone();
        let studio = shell.workspace.clone();
        let workspace = session
            .handle()
            .read(Clone::clone)
            .workspaces
            .into_iter()
            .find(|workspace| workspace.id().to_string() == studio)
            .expect("the workspace in front");

        let roster = session.shared.agents.roster();
        let (agent, _) = {
            let feed = session.shared.workspace_settings_feed();
            feed.create(Some(&workspace), &person, ("Scout", None), "")
                .expect("the workspace makes the agent")
        };
        session.shared.feed_agents();
        let listed = roster.listed(&studio);
        assert_eq!(listed.len(), 1, "one agent: {listed:?}");
        assert_eq!(listed[0].name, "Scout");
        assert_eq!(listed[0].avatar, None, "she set no picture yet");

        {
            let feed = session.shared.workspace_settings_feed();
            feed.set_avatar(Some(&workspace), &agent, Some(PNG))
                .expect("the workspace pictures the agent");
        }
        session.shared.feed_agents();
        let listed = roster.listed(&studio);
        assert_eq!(listed[0].name, "Scout", "the persona keeps its name");
        assert_eq!(
            listed[0].avatar.as_deref(),
            Some(PNG),
            "and the picture it wears"
        );
    }

    /// An agent turn this device's daemon takes is listed in `session:agents`, read from the
    /// daemon over its dispatch, for as long as the turn runs, and leaves the list when its
    /// claim is let go of.
    #[test]
    fn session_agents_lists_a_turn_the_daemon_takes_until_it_ends() {
        use tilo_workspace_service::work::{Actor, Turn, takes_turn};

        let _settled = ui::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let mut shell = Shell::new(&mut home, state.path());
        let workspaces = shell
            .session
            .shared
            .workspace_service
            .service()
            .expect("this device's own workspace service is open")
            .workspaces()
            .clone();
        tilo_workspace_service::workspaces::fixtures::act_as_every_agent(&workspaces);
        let turn = Turn::once(
            "agents.agent:scout#answer",
            serde_json::json!({ "asked": "what changed" }),
        )
        .acting_as(Actor::Agent("scout".to_owned()));
        let lease = turn.lease();
        let runtime = tilo_rt::try_app().expect("the session's runtime");
        let claim = runtime
            .block_on(takes_turn(&workspaces, &shell.workspace, &turn))
            .expect("the session's runtime runs the claim")
            .expect("this device takes the agent's turn");
        let lists = move |session: &crate::Session, lease: &str| {
            session
                .shared
                .agent_sessions
                .readings()
                .iter()
                .any(|agent| agent.key == lease && agent.state == "running")
        };
        let taken = lease.clone();
        shell.until(
            "session:agents lists the turn the daemon took",
            move |session| lists(session, &taken),
        );

        runtime
            .block_on(claim.release())
            .expect("the session's runtime lets go of the claim");
        shell.until(
            "session:agents drops the turn once it ended",
            move |session| !lists(session, &lease),
        );
    }

    /// A turn this device takes shows on the person's agent card, read from the device's runs
    /// over its dispatch, and stopping it there ends the run and takes it off the card.
    #[test]
    fn the_agents_card_shows_a_taken_run_and_stopping_it_removes_it() {
        use tilo_workspace_service::work::{Actor, Turn, takes_turn};

        let _settled = ui::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let state = tempfile::tempdir().expect("a state directory");
        let mut shell = Shell::new(&mut home, state.path());
        let workspaces = shell
            .session
            .shared
            .workspace_service
            .service()
            .expect("this device's own workspace service is open")
            .workspaces()
            .clone();
        tilo_workspace_service::workspaces::fixtures::act_as_every_agent(&workspaces);
        let turn = Turn::once(
            "agents.agent:scout#answer",
            serde_json::json!({ "asked": "what changed" }),
        )
        .acting_as(Actor::Agent("scout".to_owned()));
        let lease = turn.lease();
        let runtime = tilo_rt::try_app().expect("the session's runtime");
        let claim = runtime
            .block_on(takes_turn(&workspaces, &shell.workspace, &turn))
            .expect("the session's runtime runs the claim")
            .expect("this device takes the agent's turn");
        // The card's rows, as the person's overview draws them from the runs the session read.
        let on_card = |session: &crate::Session, lease: &str| {
            let overview = tilo_desktop::overview_tilo::TiloOverview {
                runs: session.shared.agent_sessions.runs(),
                names: session.shared.agent_sessions.named_agents(),
                ..tilo_desktop::overview_tilo::TiloOverview::default()
            };
            tilo_desktop::overview_tilo::rows(&overview)
                .into_iter()
                .any(|row| {
                    matches!(
                        row,
                        tilo_desktop::overview_tilo::TiloRow::Item(item)
                            if item.key == format!("overview-agent:{lease}")
                    )
                })
        };
        let taken = lease.clone();
        shell.until(
            "the person's agent card shows the turn the daemon took",
            move |session| on_card(session, &taken),
        );
        assert!(
            workspaces
                .agent_runs()
                .follow()
                .borrow()
                .runs
                .iter()
                .any(|run| run.run == lease && run.agent == "scout"),
            "the run reads as the agent's"
        );

        // Stop, as the card's Stop does: over the device's dispatch, which releases the claim and
        // cancels the turn.
        shell.session.shared.agent_sessions.stop_run(&lease);
        let stopped = lease.clone();
        shell.until("stopping it takes it off the card", move |session| {
            !on_card(session, &stopped)
        });
        assert!(
            runtime
                .block_on(workspaces.lease_holder(&shell.workspace, &lease))
                .expect("the session's runtime runs the read")
                .expect("the workspace reads")
                .is_none(),
            "a stopped turn's claim is let go of"
        );
        drop(claim);
    }
}
