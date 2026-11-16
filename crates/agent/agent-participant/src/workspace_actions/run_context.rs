//! The run a caller's invocation is a step of, as the host stamps it on what
//! the invocation changes: the host's own run, by the title the agent gave it.

use tilo_actions::remote::{Caller, InvokeRequest};

/// The longest run title the host keeps, as the door bounds the one it reads.
const MAX_RUN_TITLE: usize = 96;
/// The longest run id the host keeps, as the door bounds the one it reads.
const MAX_RUN_ID: usize = 96;

/// What the agent called the run `request` says it is a step of (`_meta`
/// run.title), as History names the run's changes: words for people to read,
/// never the run itself, which is the host's to say ([`caller_context`]).
/// `None` when it called it nothing.
pub(super) fn said_run_title(request: &InvokeRequest) -> Option<String> {
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
        .take(MAX_RUN_TITLE)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// The id the agent gave the run `request` says it is a step of (`_meta`
/// run.id), as History keeps it with each change: what tells the agent's runs
/// apart, never the run itself, which is the host's to say
/// ([`caller_context`]). `None` when it named none, or one too long or with
/// spaces or control characters in it.
pub(super) fn said_run_id(request: &InvokeRequest) -> Option<String> {
    let id = request
        .context
        .as_ref()?
        .group
        .as_ref()
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)?
        .declared_id
        .as_deref()?;
    ((1..=MAX_RUN_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic()))
        .then(|| id.to_owned())
}

/// The context the host stamps on what `caller`'s `request` changes: the
/// purpose and gesture the caller said, and the work it is part of. A run is
/// the host's to say: an agent's or a key's actions on one connection are the
/// run [`Caller::agent_session`] names, by the title the agent gave it
/// ([`said_run_title`]), else by the name the host knows for the agent, and
/// naming the id the agent gave it ([`said_run_id`]); a run a client claims
/// for itself is never taken.
pub(super) fn caller_context(
    caller: &Caller,
    request: &InvokeRequest,
) -> Option<tilo_actions::ActionContext> {
    let said = request.context.as_ref();
    let group = match caller.agent_session() {
        Some(mut run) => {
            if let Some(title) = said_run_title(request) {
                run.label.clone_from(&title);
                run.title = Some(title);
            }
            run.declared_id = said_run_id(request);
            Some(run)
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
