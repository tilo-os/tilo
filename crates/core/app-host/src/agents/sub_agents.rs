//! Sub-agents on the server: how a sub-agent is told its job and its notes, and how it stands.
//!
//! A sub-agent is an agent like the others this server works ([`tilo_workspace_agent::sub`]).
//! What differs is here, and nowhere else in the runtime:
//!
//! - **What it is told.** Its job, and each note its starter gives it later, is a message of
//!   Chat that mentions it, sent by its starter beside the starter's reply ([`tell`]). The
//!   mention is what its trigger fires on, as any agent's: nothing else carries work to it.
//! - **Who pays.** Its model calls are counted on the day of the person's own agent its line
//!   of starters ends at, within that agent's limit, and are asked of the model its person
//!   chose for its kind of work ([`payer`], [`kind_of`]).
//! - **How it stands.** Written when a turn of it begins and when it ends, as the sub-agent
//!   itself. When work its starter gave it ends, its starter is told by a message that mentions
//!   it, beside the sub-agent's reply ([`ServerAgents::sub_ended`]).
//!
//! What a sub-agent may read and change is not decided here: it is one more actor in its
//! starter's chain ([`tilo_actions::remote::Caller::working_under`]), decided as every action is.

use serde_json::Map;
use tilo_actions::remote::Caller;
use tilo_automation::dispatch::DocumentEvent;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_workspace_agent::{Agent, Stands, WorkspaceAgents, sub};

use super::{CHAT, Outcome, ServerAgents, Turn, WorkspaceActionProvider, invoke_named, triggered};
use tilo_workspace_service::workspaces::model_answers::Interaction;

/// The most of a sub-agent's answer its starter is told, in characters: the rest is in the
/// sub-agent's reply.
const TOLD_CHARS: usize = 2_000;

/// Where a sub-agent's turn stands in its team.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The person's own agent its line of starters ends at: its model calls are counted on
    /// that agent's day.
    pub root: Agent,
    /// The agent that started it, while the list still holds that one.
    pub starter: Option<Agent>,
    /// Whether what the turn answers came from its line of starters (its job, a note) rather
    /// than from its person: when such work ends, its starter is told.
    pub from_starter: bool,
}

/// Where `agent`'s turn on `event` stands in its team, as `roster` holds it: none for an agent a
/// person made.
pub(super) fn line_of(
    roster: &WorkspaceAgents,
    agent: &Agent,
    event: &DocumentEvent,
) -> Option<Line> {
    let sub = agent.sub.as_ref()?;
    let starter = roster
        .agents
        .iter()
        .find(|its| its.id.0 == sub.started_by)
        .cloned();
    let from_starter = event
        .actor
        .agent_id()
        .is_some_and(|by| by == sub.started_by);
    Some(Line {
        root: roster.root_of(agent).clone(),
        starter,
        from_starter,
    })
}

/// A sub-agent's words with the mention that reaches it: `@name`, then `text`, as rich text
/// a published action takes.
fn mentioning(
    rich_text: &dyn tilo_workspace_service::tl_records::RichText,
    agent: &tilo_actions::MemberId,
    name: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    rich_text.mention(
        &format!("@{name}"),
        &tilo_reference::Embed::Member(agent.clone()),
        text,
    )
}

/// Tell the agent `agent` (called `name`) `text` as `caller`, in the conversation `place` holds,
/// answering its message: a message that mentions it, which its trigger fires on. Its job, a
/// note to it, and the news that its work ended are each sent so.
///
/// # Errors
/// Chat refused the message.
pub(super) async fn tell(
    (provider, caller): (&WorkspaceActionProvider, &Caller),
    rich_text: &dyn tilo_workspace_service::tl_records::RichText,
    workspace: &str,
    place: &tilo_workspace_service::work::Where,
    (agent, name): (&tilo_actions::MemberId, &str),
    text: &str,
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("body".into(), mentioning(rich_text, agent, name, text)?);
    arguments.insert("reply_to".into(), place.reference());
    invoke_named(
        provider,
        caller,
        &route,
        &triggered::published(CHAT, &place.record),
        "send",
        arguments,
    )
    .await
    .map(drop)
}

/// The agent whose day `turn`'s model calls are counted on.
pub(super) fn payer(turn: &Turn) -> &Agent {
    turn.line.as_ref().map_or(&turn.agent, |line| &line.root)
}

/// `text` cut to `most` bytes at a character.
fn cut(text: &str, most: usize) -> &str {
    let mut end = text.len().min(most);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// The kind of work `agent`'s model is chosen by: a sub-agent's own, and for an agent a
/// person made the work an agent does for them.
pub(super) fn kind_of(agent: &Agent) -> Interaction {
    agent.sub.as_ref().map_or(Interaction::Task, |sub| {
        Interaction::from_id(&sub.kind).unwrap_or(Interaction::Subtask)
    })
}

/// What `turn`'s agent is told about sub-agents, after what every agent is told: a sub-agent
/// what it is, any other agent how it starts and steers them.
pub(super) fn told(turn: &Turn) -> String {
    // A person's own agent is told how it starts and steers a team where its turn is offered
    // one ([`super::team`]): the news it is asked with when work of a sub-agent of its ends
    // says what to do then. Its person reaches one as any agent, by a message in Chat that
    // mentions it, which starts that agent's own turn.
    let Some(sub) = &turn.agent.sub else {
        return super::team::told(turn);
    };
    let starter = turn
        .line
        .as_ref()
        .and_then(|line| line.starter.as_ref())
        .map_or(sub.started_by.as_str(), |starter| starter.name.as_str());
    let role = if sub.role.trim().is_empty() {
        String::new()
    } else {
        format!(" Your role: {}.", sub.role.trim())
    };
    format!(
        "\n\nYou are a sub-agent. {starter} started you for one job, for the person you both \
         work for.{role} Your job:\n{job}\n\nYou work in a conversation of your own. What \
         reaches you there marked (job) is that job, what is marked (note) is a note \
         {starter} gives you while you work, and anything else is your person writing to you. \
         Work from what you are told and from the workspace's other apps. When \
         the job is done, say what came of it in a few plain sentences; {starter} is told it. \
         When only your person can decide something, ask them in your answer.{team}",
        job = sub.job,
        team = super::team::told(turn),
    )
}

impl ServerAgents {
    /// Say on its item how the sub-agent `agent` stands, as the sub-agent itself in the whole
    /// chain its turn `scope` works in. The server that works it writes the item, as it writes
    /// what an agent spent: the list lets only a sub-agent's own runner say it, and no tool of
    /// a turn is asked for it. A sub-agent holds no grant of its own, so the write is decided
    /// as its turn's are: as one more actor in its starter's chain, which bounds it.
    async fn stands(&self, scope: &super::TurnScope, agent: &Agent, stands: Stands, line: &str) {
        let workspace = scope.workspace.as_str();
        let chain = scope.agent.acting().clone();
        let item = tilo_resource::Id::from_raw(agent.id.0.clone());
        let line = cut(line.trim(), sub::MAX_LINE.saturating_mul(2)).to_owned();
        let written = self
            .workspaces
            .author_agents(workspace, |doc| {
                let mut saying =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                        .as_acting(chain.clone())
                        .in_app(tilo_workspace_agent::OWNER, "Workspace");
                // A replica stamps what it writes with a fixed day of its own unless told the time.
                saying.set_now_ms(tilo_clock::unix_now_ms_i64());
                let applied = saying
                    .apply(WorkspaceAgents::SUB_AGENT_STANDS.with(
                        tilo_workspace_agent::workspace_agents::SubAgentStands {
                            agent: item,
                            stands,
                            line: (!line.is_empty()).then_some(line),
                        },
                    ))
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, saying.doc_mut());
                applied
            })
            .await;
        if let Err(error) = written {
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "how a sub-agent stands was not written");
        }
    }

    /// A turn of a sub-agent begins: its item says it works.
    pub(super) async fn sub_began(&self, scope: &super::TurnScope, turn: &Turn) {
        let Some(sub) = &turn.agent.sub else {
            return;
        };
        if sub.stands != Stands::Working || !sub.line.is_empty() {
            self.stands(scope, &turn.agent, Stands::Working, "").await;
        }
    }

    /// A turn of a sub-agent ended as `outcome`: its item says how it stands now, and when
    /// the work came from its line of starters, its starter is told.
    pub(super) async fn sub_ended(
        &self,
        acting: &super::TurnScope,
        turn: &Turn,
        outcome: &Outcome,
    ) {
        if turn.agent.sub.is_none() {
            return;
        }
        let (stands, said) = match outcome {
            Outcome::Answered(words) => (Stands::Done, words.as_str()),
            // It stopped at a limit only its person can raise, or needs to be told how to go on.
            Outcome::Stopped(words) => (Stands::NeedsPerson, words.as_str()),
            Outcome::Failed(words) => (Stands::Failed, words.as_str()),
            Outcome::Cancelled => (Stands::Stopped, ""),
        };
        self.stands(acting, &turn.agent, stands, said).await;
        let from_starter = turn.line.as_ref().is_some_and(|line| line.from_starter);
        if !from_starter || stands == Stands::Stopped {
            return;
        }
        if let Err(error) = self.tell_starter(acting, turn, (stands, said)).await {
            tracing::warn!(workspace = %acting.workspace, agent = %turn.agent.id.0, %error, "a sub-agent's starter was not told how its work ended");
        }
    }

    /// Tell the agent that started `turn`'s sub-agent how its work ended: a message of the
    /// sub-agent's that mentions its starter, answering the message that gave it the work. The
    /// starter's trigger fires on it, and it answers there like any ask.
    async fn tell_starter(
        &self,
        acting: &super::TurnScope,
        turn: &Turn,
        (stands, said): (Stands, &str),
    ) -> Result<(), String> {
        let (Some(sub), Some(line)) = (&turn.agent.sub, &turn.line) else {
            return Ok(());
        };
        let Some(starter) = &line.starter else {
            return Ok(());
        };
        let Some(place) = turn.ask.message() else {
            return Ok(());
        };
        let how = match stands {
            Stands::Done => "done",
            Stands::Failed => "failed",
            Stands::NeedsPerson => "needs its person",
            Stands::Working | Stands::Stopped => return Ok(()),
        };
        let said: String = said.trim().chars().take(TOLD_CHARS).collect();
        let said = if said.is_empty() {
            "(it said nothing)".to_owned()
        } else {
            said
        };
        let then = if starter.is_sub() {
            "Take it into your own job."
        } else {
            "Tell your person in one line what came of it."
        };
        let text = format!(
            "your sub-agent {name:?} (`{id}`) ended the work you gave it and now stands: {how}.\n\
             Its job: {job}\nWhat it said:\n{said}\n\n{then}",
            name = turn.agent.name,
            id = turn.agent.id.0,
            job = sub.job.lines().next().unwrap_or_default(),
        );
        let rich_text = acting
            .rich_text
            .as_deref()
            .ok_or("this daemon runs no tilolang runtime: it writes no rich text")?;
        let reply = acting.reply(turn);
        tell(
            (&reply.provider, &reply.caller),
            rich_text,
            &acting.workspace,
            place,
            (&starter.member(), &starter.name),
            &text,
        )
        .await
    }
}
