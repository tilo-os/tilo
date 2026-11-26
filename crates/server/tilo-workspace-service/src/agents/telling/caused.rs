//! How a turn that a cause started ended, told to the person it was for.
//!
//! An agent a person keeps on this server works when they write to it, and
//! when something else asks for them through Chat's `ask-agent`: an event
//! of theirs that fired, a moment they asked to be told at, work started
//! for them that has news. They read the answer to their own words where
//! they wrote them. Nobody is looking when a cause asks, so how its turn
//! ended joins their log by itself: what the agent answered, or the
//! sentence its reply carries where the turn could not work. Their own
//! rule marks it there, and what is to reach them is told them wherever
//! they are ([`Workspaces::tell`]).
//!
//! Only the person the turn was for is told, and only the words that are
//! in their conversation already. What kind of thing it is comes from its
//! cause only where an event that person keeps on this server asked: a
//! reminder is one because they set it up. Whatever else names a cause
//! makes news of the answer, whatever kind it says it is, as what an agent
//! tells is news.

use tilo_actions::remote::person_telling::{Cause, Kind, Tell, Teller};
use tilo_resource::Id;

use super::questions::at_message;
use crate::agents::events::WorkspaceEvents;
use crate::agents::{Ask, Outcome, Turn};
use crate::workspaces::{ToTell, Workspaces};

/// The app that keeps the conversations.
const CHAT: &str = "so.tilo.chat";

/// What asked with `message`, and who wrote it.
type Asked = (Cause, Option<String>);

/// What asked with `message` in the part of Chat's messages `bytes` hold,
/// where it was a cause asking for `person`: the cause as a thing told
/// keeps it, and the agent or the event that wrote the message. None for
/// their own words, and for a message that is not there.
///
/// # Errors
/// The part does not read.
fn asked_by(bytes: &[u8], message: &Id, person: &str) -> Result<Option<Asked>, String> {
    let mut doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    let part = crate::agents::opened::<chat::parts::ChatMessages>(&mut doc)?;
    Ok(part.messages.get(message).and_then(|held| {
        let cause = held
            .cause
            .as_ref()
            .filter(|cause| !cause.by_person() && cause.person.person_id() == Some(person))?;
        Some((
            Cause {
                kind: cause.kind.0.clone(),
                label: cause.label.clone(),
                key: cause.key.0.clone(),
            },
            held.author.agent_id().map(str::to_owned),
        ))
    }))
}

/// Tell the person `turn` was for how it ended as `outcome`, where a cause
/// started it. `person` is that person, in `workspace`, and `events` what
/// this server carries out for its people.
///
/// A turn its person wrote for themselves tells nothing, nor one they
/// stopped, nor one that left no words. A sub-agent's turn tells nothing
/// either: it answers the agent that started it, whose own turn tells.
pub(crate) async fn turn_ended(
    workspaces: &Workspaces,
    events: Option<&WorkspaceEvents>,
    person: &[u8; 32],
    workspace: &str,
    turn: &Turn,
    outcome: &Outcome,
) {
    if turn.typed || turn.line.is_some() {
        return;
    }
    let Ask::Message {
        part,
        message,
        attempt,
    } = &turn.ask
    else {
        return;
    };
    let words = match outcome {
        Outcome::Answered(words) | Outcome::Stopped(words) | Outcome::Failed(words) => words,
        Outcome::Cancelled => return,
    };
    let theirs = *person;
    if words.trim().is_empty() {
        return;
    }
    let message = Id::from_raw(message.as_str());
    let read = match workspaces.resource_document(workspace, CHAT, part).await {
        Ok(Some(bytes)) => asked_by(&bytes, &message, &turn.person),
        Ok(None) => Ok(None),
        Err(error) => Err(error.to_string()),
    };
    let (cause, author) = match read {
        Ok(Some(asked)) => asked,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%workspace, %message, %error, "what asked for a turn could not be read, so its person is not told how it ended");
            return;
        }
    };
    // What it is, is its cause's to say only for an event its person keeps.
    let kind = if events
        .zip(author.as_deref())
        .is_some_and(|(events, author)| events.set_up_by(workspace, author, person))
    {
        Kind::of_cause(Some(&cause))
    } else {
        Kind::News
    };
    let telling = ToTell {
        // One thing for each time the ask was worked on: handed over again,
        // it is told once.
        id: Some(format!("turn:{workspace}:{message}:{attempt}")),
        kind,
        tell: Tell {
            words: words.clone(),
            title: cause.label.clone(),
            cause: Some(cause),
            about: Some(at_message(workspace, &message)),
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
