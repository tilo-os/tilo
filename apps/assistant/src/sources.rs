//! What the window shows, read from the resources the system keeps: the workspace's agents
//! (`tilo.workspace.agent`), and, as their keepers publish them, the assistant's sub-agents,
//! its tasks, its scheduled work and events, and what it learned. The window keeps none of it.
//!
//! A read the workspace does not answer is [`Read::NotHere`], which the window says in one
//! sentence; it never stands for "none".

use std::rc::Rc;

use ui::prelude::*;
use ui::tilo_workspace_agent::{self as workspace_agent, Agent, WorkspaceAgents};

use crate::Window;

/// What a read of one of the system's resources answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Read<T> {
    /// What the resource holds now.
    Here(T),
    /// Nothing in this workspace answers the read.
    NotHere,
}

/// The person's assistant, as the workspace's document of its agents has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assistant {
    /// Its id in the workspace's agents; its actor is `agent:<id>`.
    pub(crate) id: String,
    pub(crate) name: String,
    /// The model the workspace chose for it, as `provider/model`; `None` follows the
    /// workspace's choice for each kind of work.
    pub(crate) model: Option<String>,
    /// What it spent today, in millionths of the currency.
    pub(crate) spent_micros: u64,
    /// The most it may spend in a day; `None` is unmetered.
    pub(crate) limit_micros: Option<u64>,
}

impl Assistant {
    fn of(agent: &Agent, day: &str) -> Self {
        Self {
            id: agent.id.0.clone(),
            name: agent.name.trim().to_owned(),
            model: agent.model.clone(),
            spent_micros: agent.spent_on(day),
            limit_micros: agent.spend_limit_micros,
        }
    }
}

/// The keeper's assistant, using the same identity as the host that answers its DM.
fn assistant(agents: &WorkspaceAgents, keeper: &str, day: &str) -> Option<Assistant> {
    agents
        .personal_assistant(keeper)
        .map(|agent| Assistant::of(agent, day))
}

/// How a sub-agent stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    Working,
    /// It waits for the person.
    NeedsYou,
    /// It finished.
    Done,
    Failed,
    Stopped,
}

/// A sub-agent the main agent started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubAgent {
    pub(crate) id: String,
    /// Its job, in a few words.
    pub(crate) job: String,
    /// What it is: the role the main agent gave it.
    pub(crate) role: String,
    pub(crate) progress: Progress,
}

/// Where a task stands.
#[cfg_attr(
    not(feature = "samples"),
    allow(
        dead_code,
        reason = "nothing publishes these in a workspace yet: only a capture makes one"
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    ReadyForReview,
    NeedsInput,
    InProgress,
    UpNext,
    Resolved,
}

/// A task of the assistant's: an issue in Tasks assigned to it.
#[cfg_attr(
    not(feature = "samples"),
    allow(
        dead_code,
        reason = "nothing publishes these in a workspace yet: only a capture makes one"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Task {
    pub(crate) id: String,
    pub(crate) title: String,
    /// What it waits for, when it waits for the person.
    pub(crate) note: Option<String>,
    pub(crate) stage: Stage,
}

/// Work the assistant does at a time: a Shortcut the host runs on a clock.
#[cfg_attr(
    not(feature = "samples"),
    allow(
        dead_code,
        reason = "nothing publishes these in a workspace yet: only a capture makes one"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scheduled {
    pub(crate) id: String,
    pub(crate) name: String,
    /// When it runs, in words ("Every day at 7:30").
    pub(crate) when: String,
    pub(crate) on: bool,
}

/// Work the assistant does when something happens: a Shortcut the host runs on a change.
#[cfg_attr(
    not(feature = "samples"),
    allow(
        dead_code,
        reason = "nothing publishes these in a workspace yet: only a capture makes one"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) id: String,
    pub(crate) name: String,
    /// What it does then, in the person's words.
    pub(crate) does: String,
    pub(crate) on: bool,
}

/// What a capture stands in for the resources a window someone uses reads: its scenario's
/// assistant and everything about it ([`ResourceApp::fixture`]). Never read in a live window.
#[cfg_attr(
    not(feature = "samples"),
    allow(
        dead_code,
        reason = "nothing publishes these in a workspace yet: only a capture makes one"
    )
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Staged {
    pub(crate) assistant: Option<Assistant>,
    pub(crate) sub_agents: Vec<SubAgent>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) scheduled: Vec<Scheduled>,
    pub(crate) events: Vec<Event>,
    pub(crate) learned: Vec<String>,
}

/// The resources the window reads, held for its views.
#[derive(Clone)]
pub(crate) struct Reads {
    staged: Option<Rc<Staged>>,
    keeper: Option<String>,
    /// Opens the workspace's document of its agents. It is asked each time the agents are
    /// read: a document still on its way when the window opened is read once it is here, by
    /// every part of the view and every effect.
    agents: ui::framework::ResourceOpener<WorkspaceAgents>,
}

impl Reads {
    pub(crate) fn of(cx: &ViewCx<Window>) -> Self {
        Self {
            staged: cx.fixture::<Staged>(),
            keeper: cx.me().and_then(|me| me.person_id()).map(str::to_owned),
            agents: cx.resource_opener(workspace_agent::OWNER),
        }
    }

    /// The workspace's document of its agents, once this window holds it.
    pub(crate) fn agents(&self) -> Option<WorkspaceResource<WorkspaceAgents>> {
        self.agents
            .open(&ui::resource::workspace::document_id(workspace_agent::KIND))
            .filter(|record| record.may_read())
    }

    /// The keeper's assistant, or `Here(None)` when they have none.
    pub(crate) fn assistant(&self) -> Read<Option<Assistant>> {
        if let Some(staged) = &self.staged {
            return Read::Here(staged.assistant.clone());
        }
        let (Some(agents), Some(keeper)) = (self.agents(), self.keeper.as_deref()) else {
            return Read::NotHere;
        };
        let day = ui::clock::today().to_string();
        Read::Here(assistant(&agents.get(), keeper, &day))
    }

    /// The sub-agents the main agent started, read from the workspace's agents document.
    pub(crate) fn sub_agents(&self) -> Read<Vec<SubAgent>> {
        if let Some(staged) = &self.staged {
            return Read::Here(staged.sub_agents.clone());
        }
        let (Some(agents), Some(keeper)) = (self.agents(), self.keeper.as_deref()) else {
            return Read::NotHere;
        };
        let state = agents.get();
        let Some(main) = state.personal_assistant(keeper) else {
            return Read::Here(Vec::new());
        };
        let listed = state
            .team_of(main)
            .into_iter()
            .filter_map(|agent| {
                let sub = agent.sub.as_ref()?;
                let progress = match sub.stands {
                    workspace_agent::Stands::Working => Progress::Working,
                    workspace_agent::Stands::NeedsPerson => Progress::NeedsYou,
                    workspace_agent::Stands::Done => Progress::Done,
                    workspace_agent::Stands::Failed => Progress::Failed,
                    workspace_agent::Stands::Stopped => Progress::Stopped,
                };
                Some(SubAgent {
                    id: agent.id.0.clone(),
                    job: sub.job.clone(),
                    role: sub.role.clone(),
                    progress,
                })
            })
            .collect();
        Read::Here(listed)
    }

    /// Tasks owns the workspace's issue list. Captures supply the example rows they show.
    pub(crate) fn tasks(&self) -> Read<Vec<Task>> {
        self.staged(|staged| staged.tasks.clone())
    }

    /// A capture's scheduled work. In a live window the assistant manages the person's
    /// events through the workspace host.
    pub(crate) fn scheduled(&self) -> Read<Vec<Scheduled>> {
        self.staged(|staged| staged.scheduled.clone())
    }

    /// What the assistant does when something happens. See [`Self::scheduled`].
    pub(crate) fn events(&self) -> Read<Vec<Event>> {
        self.staged(|staged| staged.events.clone())
    }

    /// A capture's learned facts. The person's direct conversation can recall, correct
    /// and forget the real facts under the person's own grants.
    pub(crate) fn learned(&self) -> Read<Vec<String>> {
        self.staged(|staged| staged.learned.clone())
    }

    /// What a capture staged, or [`Read::NotHere`] in a window someone uses: the reads that
    /// have no resource to ask yet.
    fn staged<T>(&self, read: impl FnOnce(&Staged) -> T) -> Read<T> {
        self.staged
            .as_deref()
            .map_or(Read::NotHere, |staged| Read::Here(read(staged)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::{Atom, Keyed};
    use ui::tilo_workspace_agent::AgentState;

    const ME: &str = "aa";

    fn agent(id: &str, keeper: &str, created_at: i64) -> Agent {
        Agent {
            id: Atom(id.to_owned()),
            name: id.to_owned(),
            keeper: keeper.to_owned(),
            created_at,
            ..Agent::default()
        }
    }

    fn agents(list: Vec<Agent>) -> WorkspaceAgents {
        let mut agents = Keyed::default();
        for agent in list {
            agents.push(agent);
        }
        WorkspaceAgents { agents }
    }

    #[test]
    fn the_assistant_is_the_persons_first_active_agent_no_client_connects_as() {
        let mine = ME.repeat(32);
        let other = "bb".repeat(32);
        let mut client = agent("claude-code", &mine, 1);
        client.mcp_client = Some("Claude Code".to_owned());
        let mut retired = agent("old", &mine, 2);
        retired.state = AgentState::Retired;
        let mut paid = agent("assistant", &mine, 5);
        paid.spend_limit_micros = Some(10_000_000);
        paid.spent = Some(workspace_agent::Spent {
            day: "2026-11-17".to_owned(),
            cost_micros: 3_120_000,
        });
        let record = agents(vec![
            client,
            retired,
            agent("theirs", &other, 1),
            agent("later", &mine, 9),
            paid,
        ]);
        let found = assistant(&record, &mine, "2026-11-17").expect("the workspace has one");
        assert_eq!(found.id, "assistant");
        assert_eq!(found.spent_micros, 3_120_000);
        assert_eq!(found.limit_micros, Some(10_000_000));
        // Another day starts at nothing spent.
        let next = assistant(&record, &mine, "2026-11-18").expect("the workspace has one");
        assert_eq!(next.spent_micros, 0);
        // Another person sees their own agent, or no agent.
        assert_eq!(
            assistant(&record, &other, "2026-11-17")
                .expect("their assistant")
                .id,
            "theirs"
        );
        assert_eq!(assistant(&record, &"cc".repeat(32), "2026-11-17"), None);
    }
}
