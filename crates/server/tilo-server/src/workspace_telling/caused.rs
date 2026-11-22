//! How a turn that a cause started ended, told to the person it was for.
//!
//! An agent a person keeps on this server works when they write to it, and when something else
//! asks it for them: an event of theirs that fired, a moment they asked to be told at, work
//! started for them that has news. Such an ask is made outright, keyed by its cause
//! ([`tilo_workspace_service::asks::ask`]). They read the answer to their own words where they
//! wrote them. Nobody is looking when a cause asks, so how its turn ended joins their log by
//! itself: what the agent answered, or the sentence its reply carries where the turn could not
//! work. Their own rule marks it there, and what is to reach them is told them wherever they are
//! ([`Workspaces::tell`]).
//!
//! Only the person the turn was for is told, and only the words that are in its reply already.
//! What kind of thing it is comes from its cause only where an event that person keeps on this
//! server asked: a reminder is one because they set it up. Whatever else names a cause makes
//! news of the answer, whatever kind it says it is, as what an agent tells is news.

use tilo_actions::remote::Caller;
use tilo_actions::remote::person_telling::{About, Cause, Kind, Tell, Teller};
use tilo_workspace_service::asks;

use crate::workspace_agents::{Ask, Outcome, Turn};
use crate::workspace_bots::events::WorkspaceEvents;
use crate::workspaces::{ToTell, Workspaces};

/// The cause `key` names, as the ask it made keeps it (`<kind>:<its own id>`), called `label`.
fn cause_of(key: &str, label: &str) -> Cause {
    Cause {
        kind: key.split_once(':').map_or("", |(kind, _)| kind).to_owned(),
        label: label.lines().next().unwrap_or_default().trim().to_owned(),
        key: key.to_owned(),
    }
}

/// The key the ask `ask` of `workspace` was made under, and the agent that made it, where an
/// agent did; `None` for an ask that names no cause, and for one that is not there.
async fn asked_by(
    workspaces: &Workspaces,
    workspace: &str,
    ask: &str,
) -> Result<Option<(String, Option<String>)>, String> {
    let packages = crate::workspace_agents::asked::packages()?;
    let ask = ask.to_owned();
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            crate::workspace_agents::asked::viewer(),
            move |opened| {
                Ok(asks::asks(opened)?
                    .into_iter()
                    .find(|asked| asked.at.id_string() == ask)
                    .filter(|asked| !asked.mention.is_empty())
                    .map(|asked| {
                        let by = match asked.who {
                            Some(asks::Actor::Member(member)) => {
                                member.agent_id().map(str::to_owned)
                            }
                            _ => None,
                        };
                        (asked.mention, by)
                    }))
            },
        )
        .await
}

/// Tell the person `turn` was for how it ended as `outcome`, where a cause started it. `person`
/// is that person as the turn acted for them in `workspace`, and `events` what this server
/// carries out for its people.
///
/// A turn its person wrote for themselves tells nothing, nor one they stopped, nor one that left
/// no words. A sub-agent's turn tells nothing either: it answers the agent that started it,
/// whose own turn tells.
pub(crate) async fn turn_ended(
    workspaces: &Workspaces,
    events: Option<&WorkspaceEvents>,
    person: &Caller,
    workspace: &str,
    turn: &Turn,
    outcome: &Outcome,
) {
    if turn.typed || turn.line.is_some() {
        return;
    }
    let Ask::Asked {
        app, ask, attempt, ..
    } = &turn.ask
    else {
        return;
    };
    let words = match outcome {
        Outcome::Answered(words) | Outcome::Stopped(words) | Outcome::Failed(words) => words,
        Outcome::Cancelled => return,
    };
    let Some(theirs) = person.user_public_key().copied() else {
        return;
    };
    if words.trim().is_empty() {
        return;
    }
    // The ask that started it names its cause by the key it was made under, and who made it.
    let (key, by) = match asked_by(workspaces, workspace, ask).await {
        Ok(Some(asked)) => asked,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%workspace, %ask, %error, "what asked for a turn could not be read, so its person is not told how it ended");
            return;
        }
    };
    let cause = cause_of(&key, &turn.asked);
    // What it is, is its cause's to say only for an event its person keeps.
    let kind = match (events, by) {
        (Some(events), Some(by)) if events.keeps(workspace, &by) => Kind::of_cause(Some(&cause)),
        _ => Kind::News,
    };
    let telling = ToTell {
        // One thing for each time the ask was worked on: handed over again, it is told once.
        id: Some(format!("turn:{workspace}:{ask}:{attempt}")),
        kind,
        tell: Tell {
            words: words.clone(),
            title: cause.label.clone(),
            cause: Some(cause),
            about: Some(About {
                workspace: workspace.to_owned(),
                app: app.clone(),
                record: String::new(),
                resource: ask.clone(),
            }),
            ..Tell::default()
        },
        from: Some(Teller {
            agent: turn.agent.id.0.clone(),
            name: turn.agent.name.clone(),
        }),
        workspace: Some(workspace.to_owned()),
        ..ToTell::default()
    };
    if let Err(error) = workspaces.tell(&theirs, telling).await {
        tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "a person was not told how a turn that a cause started ended");
    }
}
