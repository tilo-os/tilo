//! Sub-agents on the server: what the workspace's list of its agents holds for one is asked of
//! it through the agent package, its answers are its replies, and how it stands is written back.
//!
//! A sub-agent is an agent like the others this server works ([`tilo_workspace_agent::sub`]).
//! What differs is here, and nowhere else in the runtime:
//!
//! - **What it is asked.** Its job, then each note its starter gives it: each an ask of the agent
//!   package in the name of the agent that gave it, keyed by its cause ([`key_of`]), so that a
//!   cause asks once however often a look finds it ([`ServerAgents::ask_team`]). The job is made
//!   from the reply of the turn that started it ([`super::team`]), a note from the job's ask. Its
//!   person also asks it directly, as they ask any agent.
//! - **What it is told.** What it was asked, and nothing that sits around it: its job, a note,
//!   its person's words. It reads none of its starter's conversation ([`team_turn`]).
//! - **Who pays.** Its model calls are counted on the day of the person's own agent its line
//!   of starters ends at, within that agent's limit, and are asked of the model its person
//!   chose for its kind of work ([`payer`], [`kind_of`]).
//! - **How it stands.** Written when a turn of it begins and when it ends, as the sub-agent
//!   itself. When work its line gave it ends, its starter is asked with the news, made from the
//!   sub-agent's reply, which the starter's turn reads ([`ServerAgents::sub_ended`]).
//! - **Stopping.** A stop on its item ends the turn that is going, and takes back every ask its
//!   line made of it before the stop.
//!
//! What a sub-agent may read and change is not decided here: it is one more actor in its
//! starter's chain ([`tilo_actions::remote::Caller::working_under`]), decided as every action is.

use std::collections::BTreeMap;

use crate::asks::{self, Actor, AskState, Asked, ResourceRef};
use crate::tl_records::{Opened, runtime::Value};
use tilo_resource::Id;
use tilo_workspace_agent::{Agent, Stands, WorkspaceAgents, sub};

use super::{Ask, Outcome, ServerAgents, Turn, TurnScope, asked};
use crate::workspaces::Workspaces;
use crate::workspaces::person_model::Interaction;

/// The cause of the ask a sub-agent's job is.
const JOB: &str = "job";
/// The cause of the ask a note of its starter's is, with the note's id.
const NOTE: &str = "note";
/// The cause of the ask that tells a starter its sub-agent's work ended, with the reply.
const ENDED: &str = "ended";
/// The most of a sub-agent's reply its starter's turn is told, in characters: the rest stays
/// on the reply.
const TOLD_CHARS: usize = 4_000;
/// The longest key a cause carries, in bytes.
const KEY_BYTES: usize = 200;

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
    /// Whether what the turn answers is its job.
    pub job: bool,
}

/// The key of the cause `tail` of the sub-agent `agent` is asked with: one key is one ask
/// however often it is asked, so a server that restarts asks nothing twice.
fn key_of(agent: &str, tail: &str) -> String {
    let plain = format!("sub-agent:{agent}:{tail}");
    if plain.len() <= KEY_BYTES {
        return plain;
    }
    let hash = blake3::hash(plain.as_bytes()).to_hex();
    format!("sub-agent:{}", hash.get(..40).unwrap_or_default())
}

/// `text` cut to `most` bytes at a character.
fn cut(text: &str, most: usize) -> &str {
    let mut end = text.len().min(most);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// When `asked` was asked, in milliseconds since 1970; 0 where its record says no time.
fn asked_ms(asked: &Asked) -> i64 {
    asked
        .asked_at
        .parse::<jiff::Timestamp>()
        .map_or(0, jiff::Timestamp::as_millisecond)
}

/// The agent `actor` is, by its id.
fn agent_of(actor: Option<&Actor>) -> Option<&str> {
    match actor {
        Some(Actor::Member(member)) => member.agent_id(),
        _ => None,
    }
}

/// Whether `asked` is the team's to read as a turn of `agent`: anything asked of a sub-agent,
/// and anything an agent asked, which is the news of a sub-agent of its own.
pub(super) fn is_team(agent: &Agent, asked: &Asked) -> bool {
    agent.sub.is_some() || agent_of(asked.who.as_ref()).is_some()
}

/// The turn `asked`, a team's ask ([`is_team`]), gives `agent`, kept in `app`'s record beside
/// `all`, every ask there, as `roster` holds the workspace's agents now; with whether a stop on
/// the sub-agent's item ends it. None for an ask nobody of the team made: an agent that is no
/// starter of it, or a member who is not its person.
pub(super) fn team_turn(
    roster: &WorkspaceAgents,
    app: &str,
    agent: &Agent,
    asked: &Asked,
    all: &[Asked],
) -> Option<(Turn, bool)> {
    let said = asked.said.clone().unwrap_or_default();
    let named = |id: &str| {
        roster
            .get(&Id::from_raw(id.to_owned()))
            .map_or_else(|| id.to_owned(), |by| by.name.clone())
    };
    // What it is about, whose place it takes: the sub-agent asked, or for news the one below.
    let (request, from_starter, job, typed, sub_agent) = match asked.who.as_ref() {
        Some(Actor::Member(member)) if member.agent_id().is_some() => {
            let by = member.agent_id().unwrap_or_default();
            if roster.started_under(agent, by) {
                let job = asked.mention == key_of(&agent.id.0, JOB);
                let request = if job {
                    said.clone()
                } else {
                    format!("A note from {}: {said}", named(by))
                };
                (request, true, job, false, agent.id.0.clone())
            } else if roster
                .get(&Id::from_raw(by.to_owned()))
                .is_some_and(|below| roster.started_under(below, &agent.id.0))
            {
                // Its sub-agent's news, made from that sub-agent's reply: read there.
                let answered = asked
                    .from
                    .as_ref()
                    .and_then(|from| {
                        all.iter()
                            .flat_map(|ask| ask.replies.iter())
                            .find(|reply| reply.at.same(from))
                    })
                    .map(|reply| {
                        reply
                            .body
                            .trim()
                            .chars()
                            .take(TOLD_CHARS)
                            .collect::<String>()
                    })
                    .filter(|body| !body.is_empty())
                    .unwrap_or_else(|| "(it said nothing)".to_owned());
                let request = format!("{said}\n\nWhat it said:\n{answered}");
                (request, false, false, false, by.to_owned())
            } else {
                return None;
            }
        }
        Some(Actor::Member(member))
            if agent.sub.is_some() && member.person_id() == Some(agent.acts_for.as_str()) =>
        {
            (said.clone(), false, false, true, agent.id.0.clone())
        }
        _ => return None,
    };
    let line = agent.sub.as_ref().map(|sub| Line {
        root: roster.root_of(agent).clone(),
        starter: roster.get(&Id::from_raw(sub.started_by.clone())).cloned(),
        from_starter,
        job,
    });
    // A stop on its item ends what its line asked before it: what was asked since is new work.
    let stopped = from_starter
        && agent
            .sub
            .as_ref()
            .is_some_and(|sub| sub.stands == Stands::Stopped && asked_ms(asked) <= sub.since);
    let attempt = asked.attempt();
    Some((
        Turn {
            agent: agent.clone(),
            person: agent.acts_for.clone(),
            conversation: format!("sub-agents/{sub_agent}"),
            cause: asked.at.id_string(),
            request,
            asked: said,
            typed,
            line,
            summary: None,
            summarize: None,
            ask: Ask::Asked {
                app: app.to_owned(),
                ask: asked.at.id_string(),
                ty: asked.at.ty.clone(),
                key: asked.mention.clone(),
                attempt,
                // Nothing around a team's ask is read for its turn.
                private: false,
            },
        },
        stopped,
    ))
}

/// Ask `agent` `said` as `asker`, under `key`, made from `from`, unless an ask under that key
/// stands in `all` already, whatever it was made from.
fn ask_once(
    opened: &mut Opened,
    all: &[Asked],
    (asker, agent): (&str, &str),
    (said, key): (&str, &str),
    from: &ResourceRef,
) -> Result<(), String> {
    let agent = tilo_actions::MemberId::agent(agent.to_owned());
    if all
        .iter()
        .any(|asked| asked.agent == agent && asked.mention == key)
    {
        return Ok(());
    }
    let asker = Actor::Member(tilo_actions::MemberId::agent(asker.to_owned()));
    asks::ask(opened, &asker, &agent, said, key, from).map(drop)
}

/// What the record holds for `sub_agent`, as `roster` holds it, that it was not asked yet: its
/// job, made from `from` where the turn that started it says, else from its starter's newest
/// reply; and each note, made from the job's ask. What its line asked before a stop on its
/// item and nobody took yet is taken back.
fn ask_of(
    opened: &mut Opened,
    roster: &WorkspaceAgents,
    sub_agent: &Agent,
    from: Option<&ResourceRef>,
) -> Result<(), String> {
    let Some(sub) = &sub_agent.sub else {
        return Ok(());
    };
    let id = sub_agent.id.0.as_str();
    let all = asks::asks(opened)?;
    let job_key = key_of(id, JOB);
    if sub.stands == Stands::Working {
        let newest = all
            .iter()
            .filter(|asked| asked.agent.agent_id() == Some(sub.started_by.as_str()))
            .filter_map(|asked| {
                asked
                    .latest()
                    .map(|reply| (asked_ms(asked), reply.at.clone()))
            })
            .max_by_key(|(at, _)| *at)
            .map(|(_, reply)| reply);
        let root = opened
            .host()
            .root_of(asks::AGENT_APP)
            .map_err(|error| error.to_string())?;
        let from = from.cloned().or(newest).unwrap_or(root);
        ask_once(
            opened,
            &all,
            (&sub.started_by, id),
            (&sub.job, &job_key),
            &from,
        )?;
    }
    let all = asks::asks(opened)?;
    let job = all
        .iter()
        .find(|asked| asked.mention == job_key && asked.agent.agent_id() == Some(id))
        .map(|asked| asked.at.clone());
    if let Some(job) = &job {
        for note in &sub.notes {
            if sub.stands == Stands::Stopped && note.at <= sub.since {
                continue;
            }
            let key = key_of(id, &format!("{NOTE}:{}", note.id));
            ask_once(opened, &all, (&note.by, id), (&note.text, &key), job)?;
        }
    }
    if sub.stands == Stands::Stopped {
        for asked in &all {
            let by = agent_of(asked.who.as_ref());
            let of_its_line = by.is_some_and(|by| roster.started_under(sub_agent, by));
            if asked.agent.agent_id() == Some(id)
                && asked.state == AskState::Waiting
                && of_its_line
                && asked_ms(asked) <= sub.since
                && let Some(by) = by
            {
                opened.invoke(
                    &Actor::Member(tilo_actions::MemberId::agent(by.to_owned())),
                    (asks::AGENT_APP, "withdraw"),
                    vec![Value::resource(&asked.at)],
                    None,
                )?;
            }
        }
    }
    Ok(())
}

/// The agent whose day `turn`'s model calls are counted on.
pub(super) fn payer(turn: &Turn) -> &Agent {
    turn.line.as_ref().map_or(&turn.agent, |line| &line.root)
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
    // says what to do then.
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
         work for.{role} Your job:\n{job}\n\nYou are asked that job first; a note {starter} \
         gives you while you work is asked of you after, and your person may ask you \
         themselves. You cannot read {starter}'s conversation or any other, and nothing Chat \
         keeps is yours to read: work from what you are asked and from the workspace's other \
         apps. When the job is done, say what came of it in a few plain sentences; {starter} \
         reads it. When only your person can decide something, ask them in your answer.{team}",
        job = sub.job,
        team = super::team::told(turn),
    )
}

/// The workspace's list of its agents, as `workspaces` holds it in `workspace` now.
async fn roster_of(
    workspaces: &Workspaces,
    workspace: &str,
) -> Result<Option<WorkspaceAgents>, String> {
    let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let Some(bytes) = workspaces
        .resource_document(workspace, tilo_workspace_agent::OWNER, &document)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    crate::agents::opened::<WorkspaceAgents>(&mut doc).map(Some)
}

/// Ask the sub-agent `sub_agent` of `workspace` its job, made from `from`, the reply of the
/// turn that started it: at once, where a look would only find the starter's newest reply.
///
/// # Errors
/// The list or the record could not be read, or the ask was refused.
pub(super) async fn ask_started(
    workspaces: &Workspaces,
    workspace: &str,
    sub_agent: &str,
    from: ResourceRef,
) -> Result<(), String> {
    let Some(roster) = roster_of(workspaces, workspace).await? else {
        return Ok(());
    };
    let Some(agent) = roster.get(&Id::from_raw(sub_agent.to_owned())).cloned() else {
        return Ok(());
    };
    workspaces
        .with_tl_record(
            workspace,
            &asked::packages()?,
            asked::viewer(),
            move |opened| ask_of(opened, &roster, &agent, Some(&from)),
        )
        .await
}

impl ServerAgents {
    /// The workspace's list of its agents, when it holds one.
    ///
    /// # Errors
    /// The list could not be read.
    pub(super) async fn team_roster(
        &self,
        workspace: &str,
    ) -> Result<Option<WorkspaceAgents>, String> {
        roster_of(&self.workspaces, workspace).await
    }

    /// Ask the sub-agents of `workspace` among `worked`, the agents this server runs, what
    /// `roster` holds for them and they were not asked yet, and take back what a stop on an
    /// item ended before anyone took it.
    ///
    /// # Errors
    /// The record could not be read or written.
    pub(super) async fn ask_team(
        &self,
        workspace: &str,
        roster: &WorkspaceAgents,
        worked: &BTreeMap<String, Agent>,
    ) -> Result<(), String> {
        let subs: Vec<Agent> = roster
            .agents
            .iter()
            .filter(|agent| agent.is_active() && agent.sub.is_some())
            .filter(|agent| worked.contains_key(&agent.id.0))
            .cloned()
            .collect();
        if subs.is_empty() {
            return Ok(());
        }
        let roster = roster.clone();
        self.workspaces
            .with_tl_record(
                workspace,
                &asked::packages()?,
                asked::viewer(),
                move |opened| {
                    for agent in &subs {
                        ask_of(opened, &roster, agent, None)?;
                    }
                    Ok(())
                },
            )
            .await
    }

    /// Say on its item how the sub-agent `agent` stands, as the sub-agent itself. The server
    /// that works it writes the item, as it writes what an agent spent: the list lets only a
    /// sub-agent's own runner say it, and no tool of a turn is asked for it.
    async fn stands(&self, workspace: &str, agent: &Agent, stands: Stands, line: &str) {
        let document = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let (id, acts_for) = (agent.id.0.clone(), agent.acts_for.clone());
        let item = tilo_resource::Id::from_raw(id.clone());
        let line = cut(line.trim(), sub::MAX_LINE.saturating_mul(2)).to_owned();
        let written = self
            .workspaces
            .author_application(
                workspace,
                self.workspaces.governor(),
                &tilo_agent_session::AppCatalogue::native(),
                tilo_workspace_agent::OWNER,
                &document,
                |doc| {
                    let mut saying =
                        tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                            .as_agent_for(&id, &acts_for)
                            .in_app(tilo_workspace_agent::OWNER, "Assistant");
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
                },
            )
            .await;
        if let Err(error) = written {
            tracing::warn!(%workspace, agent = %agent.id.0, %error, "how a sub-agent stands was not written");
        }
    }

    /// A turn of a sub-agent begins: its item says it works.
    pub(super) async fn sub_began(&self, workspace: &str, turn: &Turn) {
        let Some(sub) = &turn.agent.sub else {
            return;
        };
        if sub.stands != Stands::Working || !sub.line.is_empty() {
            self.stands(workspace, &turn.agent, Stands::Working, "")
                .await;
        }
    }

    /// A turn of a sub-agent ended as `outcome`: its item says how it stands now, and when
    /// the work came from its line of starters (`from_starter`), its starter is told.
    pub(super) async fn sub_ended(
        &self,
        acting: &TurnScope,
        turn: &Turn,
        outcome: &Outcome,
        from_starter: bool,
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
        self.stands(&acting.workspace, &turn.agent, stands, said)
            .await;
        let from_starter = from_starter || turn.line.as_ref().is_some_and(|line| line.from_starter);
        if !from_starter || stands == Stands::Stopped {
            return;
        }
        if let Err(error) = self.tell_starter(acting, turn, stands).await {
            tracing::warn!(workspace = %acting.workspace, agent = %turn.agent.id.0, %error, "a sub-agent's starter was not told how its work ended");
        }
    }

    /// Ask the agent that started `turn`'s sub-agent with the news that its work ended as
    /// `stands`: an ask in the sub-agent's name, made from its reply, which the starter's turn
    /// reads ([`team_turn`]). Keyed by that reply, so its news is asked once.
    async fn tell_starter(
        &self,
        acting: &TurnScope,
        turn: &Turn,
        stands: Stands,
    ) -> Result<(), String> {
        let (Some(sub), Some(line)) = (&turn.agent.sub, &turn.line) else {
            return Ok(());
        };
        let (Some(starter), Some(replying)) = (&line.starter, &acting.replying) else {
            return Ok(());
        };
        let how = match stands {
            Stands::Done => "done",
            Stands::Failed => "failed",
            Stands::NeedsPerson => "needs its person",
            Stands::Working | Stands::Stopped => return Ok(()),
        };
        let reply = replying.reply().await?;
        let then = if starter.is_sub() {
            "Take it into your own job."
        } else {
            "Tell your person in one line what came of it."
        };
        let said = format!(
            "Your sub-agent {name:?} (`{id}`) ended the work you gave it and now stands: {how}.\n\
             Its job: {job}\n\n{then} The whole of what it said is its reply.",
            name = turn.agent.name,
            id = turn.agent.id.0,
            job = sub.job.lines().next().unwrap_or_default(),
        );
        let key = key_of(&turn.agent.id.0, &format!("{ENDED}:{}", reply.id_string()));
        let (by, to) = (turn.agent.id.0.clone(), starter.id.0.clone());
        self.workspaces
            .with_tl_record(
                &acting.workspace,
                &asked::packages()?,
                asked::viewer(),
                move |opened| {
                    let all = asks::asks(opened)?;
                    ask_once(opened, &all, (&by, &to), (&said, &key), &reply)
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cause_key_names_its_sub_agent_and_stays_within_what_a_cause_carries() {
        assert_eq!(key_of("researcher", JOB), "sub-agent:researcher:job");
        assert_eq!(
            key_of("researcher", "note:n1"),
            "sub-agent:researcher:note:n1"
        );
        let long = "a".repeat(400);
        let key = key_of(&long, JOB);
        assert!(key.len() <= KEY_BYTES, "{key}");
        assert_eq!(key, key_of(&long, JOB), "the same every time it is made");
        assert_ne!(key, key_of(&long, "note:n1"));
        assert_eq!(cut("äöü", 3), "ä", "cut at a character");
    }
}
