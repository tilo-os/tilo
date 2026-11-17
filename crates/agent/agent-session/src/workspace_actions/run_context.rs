//! The group a caller's invocation is a step of, as the host stamps it on what
//! the invocation changes: the host's own group, by the title the agent gave it.

use tilo_actions::remote::{Caller, InvokeRequest};

/// The longest title the host keeps, as the API bounds the one it reads.
const MAX_INVOCATION_TITLE: usize = 96;
/// The longest id the host keeps, as the API bounds the one it reads.
const MAX_INVOCATION_ID: usize = 96;

/// What the agent called the group `request` says it is a step of (`_meta`
/// invocation.title), as History names the invocation's changes: words for people
/// to read, never the group itself, which is the host's to say ([`caller_context`]).
/// `None` when it called it nothing.
pub(super) fn said_invocation_title(request: &InvokeRequest) -> Option<String> {
    let title: String = request
        .context
        .as_ref()?
        .group
        .as_ref()
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)?
        .title
        .as_deref()?
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_INVOCATION_TITLE)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// The id the agent gave the group `request` says it is a step of (`_meta`
/// invocation.id), as History keeps it with each change: what tells the agent's
/// invocations apart, never the group itself, which is the host's to say
/// ([`caller_context`]). `None` when it named none, or one too long or with spaces
/// or control characters in it.
pub(super) fn said_invocation_id(request: &InvokeRequest) -> Option<String> {
    let id = request
        .context
        .as_ref()?
        .group
        .as_ref()
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)?
        .declared_id
        .as_deref()?;
    ((1..=MAX_INVOCATION_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic()))
        .then(|| id.to_owned())
}

/// The context the host stamps on what `caller`'s `request` changes: the
/// purpose and gesture the caller said, and the work it is part of. The group is
/// the host's to say: an agent's or a key's actions on one connection are the
/// group [`Caller::agent_session`] names, by the title the agent gave it
/// ([`said_invocation_title`]), else by the name the host knows for the agent, and
/// naming the id the agent gave it ([`said_invocation_id`]); one a client claims
/// for itself is never taken.
pub(super) fn caller_context(
    caller: &Caller,
    request: &InvokeRequest,
) -> Option<tilo_actions::ActionContext> {
    let said = request.context.as_ref();
    let group = match caller.agent_session() {
        Some(mut group) => {
            if let Some(title) = said_invocation_title(request) {
                group.label.clone_from(&title);
                group.title = Some(title);
            }
            group.declared_id = said_invocation_id(request);
            Some(group)
        }
        None => said
            .and_then(|context| context.group.clone())
            .filter(|group| group.kind != tilo_actions::context::AGENT_SESSION_GROUP),
    };
    if said.is_none() && group.is_none() {
        return None;
    }
    Some(tilo_actions::ActionContext {
        group,
        purpose: said.and_then(|context| context.purpose.clone()),
        gesture: said.and_then(|context| context.gesture),
        ..tilo_actions::ActionContext::default()
    })
}
