//! What the people of a workspace ask its agents on the tracker: the issues they gave an agent.
//!
//! An agent works inside a turn a person caused ([`tilo_action_policy::turn`]). On the tracker the
//! cause is a person giving an issue to the agent (its `assign` action), or a person writing in the
//! thread of an issue the agent has; the issue's thread is the conversation, and the agent's answer
//! is its own message there after the ask. Nothing is kept beside the record: whoever runs the
//! agent reads the same asks from it.

use tilo_resource::{Id, MemberId};

use super::{Issue, MessageKind};

/// A person's ask of the agent an issue is assigned to that the agent has not answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueAsk {
    /// The issue.
    pub issue: Id,
    /// The agent asked, by its bare id.
    pub agent: String,
    /// The person who asked: the agent works for them.
    pub person: MemberId,
    /// What asked: the id of the person's message in the thread, or `assigned-<ms>` for the
    /// assignment itself.
    pub cause: String,
    /// The person's words in the thread, when a message asked; empty for the assignment.
    pub words: String,
}

/// Every open issue of `issues` whose agent has a person's ask it has not answered: the newest
/// ask on each.
#[must_use]
pub fn agent_asks<'a>(issues: impl IntoIterator<Item = (&'a Id, &'a Issue)>) -> Vec<IssueAsk> {
    issues
        .into_iter()
        .filter_map(|(id, issue)| ask_on(id, issue))
        .collect()
}

fn is_person(member: Option<&MemberId>) -> bool {
    member.is_some_and(MemberId::is_person)
}

/// The ask on `issue` its agent has not answered, if any.
fn ask_on(id: &Id, issue: &Issue) -> Option<IssueAsk> {
    if issue.status.closed() {
        return None;
    }
    let agent = issue.assigned_agent()?.strip_prefix("agent:")?.to_owned();
    let by_agent = MemberId::agent(agent.clone());
    // The newest ask: the assignment, or a person's later message in the thread.
    let mut ask: Option<(i64, MemberId, String, String)> = issue
        .assigned_by
        .clone()
        .filter(|by| is_person(Some(by)) && issue.assigned_ms > 0)
        .map(|by| {
            (
                issue.assigned_ms,
                by,
                format!("assigned-{}", issue.assigned_ms),
                String::new(),
            )
        });
    for (message_id, message) in issue.comments.iter() {
        let Some(author) = message
            .author
            .clone()
            .filter(|author| is_person(Some(author)))
        else {
            continue;
        };
        if message.kind != MessageKind::Comment {
            continue;
        }
        // Only what was said once the agent had the issue asks it.
        let Some((since, ..)) = &ask else {
            continue;
        };
        if message.at_ms >= *since {
            ask = Some((
                message.at_ms,
                author,
                message_id.to_string(),
                message.text.clone(),
            ));
        }
    }
    let (since, person, cause, words) = ask?;
    let answered = issue
        .comments
        .iter()
        .any(|(_, message)| message.author.as_ref() == Some(&by_agent) && message.at_ms >= since);
    (!answered).then(|| IssueAsk {
        issue: id.clone(),
        agent,
        person,
        cause,
        words,
    })
}
