//! How a turn that an event of a person's started ended, told to that person.
//!
//! An agent a person keeps on this server works when they write to it, and
//! when an event of theirs asks it for them: an event that fired, a moment
//! they asked to be told at. Its instruction is an ask made from the event
//! ([`crate::workspace_agents::ask_for_event`]). They read the answer to
//! their own words where they wrote them. Nobody is looking when an event
//! asks, so how its turn ended joins their log by itself: what the agent
//! answered, or the sentence its reply carries where the turn could not
//! work. Their own rule marks it there, and what is to reach them is told
//! them wherever they are ([`Workspaces::tell`]).
//!
//! Only the person the turn was for is told, and only the words the ask's
//! reply holds. What kind of thing it is comes from the event the ask is
//! made from, where that event is one this person keeps on this server: a
//! reminder is one because they set it up. An ask made from any other event
//! makes news of the answer, as what an agent tells is news.

use tilo_actions::remote::Caller;
use tilo_actions::remote::person_telling::{
    About, CAUSE_BRIEF, CAUSE_EVENT, CAUSE_REMINDER, Cause, Kind, Tell, Teller,
};

use crate::workspace_agents::{Ask, Outcome, Turn};
use crate::workspace_bots::events::{WorkspaceEvents, event_named};
use crate::workspaces::{ToTell, Workspaces};

/// Tell the person `turn` was for how it ended as `outcome`, where an event
/// asked for it. `person` is that person as the turn acted for them in
/// `workspace`, and `events` what this server carries out for its people.
///
/// A turn its person wrote for themselves tells nothing, nor one they
/// stopped, nor one that left no words. A sub-agent's turn tells nothing
/// either: it answers the agent that started it, whose own turn tells.
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
        app,
        ask,
        ty,
        key,
        attempt,
        ..
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
    let from = match crate::workspace_agents::made_from(workspaces, workspace, (ask, ty)).await {
        Ok(Some(from)) => from,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%workspace, %ask, %error, "what asked for a turn could not be read, so its person is not told how it ended");
            return;
        }
    };
    let Some(event) = event_named(&from) else {
        return;
    };
    // What it is, is the event's to say only where it is one its person keeps.
    let (kind, label) = events
        .and_then(|events| events.told_as(workspace, event, &turn.person))
        .map_or((Kind::News, String::new()), |(name, kind)| (kind, name));
    let cause = Cause {
        kind: match kind {
            Kind::Reminder => CAUSE_REMINDER,
            Kind::Brief => CAUSE_BRIEF,
            Kind::Question | Kind::News | Kind::Mention => CAUSE_EVENT,
        }
        .to_owned(),
        label: label.clone(),
        key: key.clone(),
    };
    let telling = ToTell {
        // One thing for each time the ask was worked on: handed over again,
        // it is told once.
        id: Some(format!("turn:{workspace}:{ask}:{attempt}")),
        kind,
        tell: Tell {
            words: words.clone(),
            title: label,
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
        tracing::warn!(%workspace, agent = %turn.agent.id.0, %error, "a person was not told how a turn that an event started ended");
    }
}
