//! The group a caller's invocation is a step of, as the host stamps it on what
//! the invocation changes: the host's own group, by the title the agent gave it.

use tilo_actions::remote::{Caller, InvokeRequest};

/// The longest id the host keeps, as the API bounds the one it reads.
const MAX_INVOCATION_ID: usize = 96;

/// The agent-session group `request` says it is a step of, as the client wrote it: what the
/// agent said about its work, never the group the server stamps ([`caller_context`]).
fn said_group(request: &InvokeRequest) -> Option<&tilo_actions::WorkGroup> {
    request
        .context
        .as_ref()?
        .group
        .as_ref()
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
}

/// What the agent called the group `request` says it is a step of (`_meta`
/// invocation.title), as History names the invocation's changes: words for people
/// to read, never the group itself, which is the host's to say ([`caller_context`]).
/// `None` when it called it nothing, or only by its session's name, which is no step's title.
pub(super) fn said_invocation_title(request: &InvokeRequest) -> Option<String> {
    let title = tilo_actions::context::one_line(
        said_group(request)?.label.as_deref()?,
        tilo_actions::context::MAX_LABEL_CHARS,
    )?;
    (request.said_session_name().as_deref() != Some(title.as_str())).then_some(title)
}

/// The id the agent gave the group `request` says it is a step of (`_meta`
/// invocation.id), as History keeps it with each change: what tells the agent's
/// invocations apart, never the group itself, which is the host's to say
/// ([`caller_context`]). `None` when it named none, or one too long or with spaces
/// or control characters in it.
pub(super) fn said_invocation_id(request: &InvokeRequest) -> Option<String> {
    let id = said_group(request)?.declared_id.as_deref()?;
    ((1..=MAX_INVOCATION_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic()))
        .then(|| id.to_owned())
}

/// The context the host stamps on what `caller`'s `request` changes: the
/// invocation id the host admitted it under, the purpose and gesture the caller
/// said, and the work it is part of. The group is
/// the host's to say: an agent's or a key's actions on one connection are the
/// group [`Caller::agent_session`] names, by the title the agent gave it
/// ([`said_invocation_title`]), else by the session's own name ([`InvokeRequest::said_session_name`]), else by
/// what it was asked, and naming the id the agent gave it ([`said_invocation_id`]); one a client
/// claims for itself is never taken, only the words it says of it.
pub(super) fn caller_context(
    caller: &Caller,
    request: &InvokeRequest,
) -> tilo_actions::ActionContext {
    let said = request.context.as_ref();
    let group = match caller.agent_session() {
        Some(mut group) => {
            // The session's name is display only, like the title: what people see the agent by
            // (its chip, its pointer's tag, its History rows), never which group it is.
            group.session = request.said_session_name();
            if let Some(label) = said_invocation_title(request).or_else(|| group.session.clone()) {
                group.label = Some(label);
            }
            group.declared_id = said_invocation_id(request);
            Some(group)
        }
        None => said
            .and_then(|context| context.group.clone())
            .filter(|group| group.kind != tilo_actions::context::AGENT_SESSION_GROUP),
    };
    tilo_actions::ActionContext {
        group,
        purpose: said.and_then(|context| context.purpose.clone()),
        gesture: said.and_then(|context| context.gesture),
        source_invocation: Some(request.invocation),
        // What the caller signed, kept with every change it makes: the host signs those as
        // the one who ran them, and this says who asked. Never what the caller's own context
        // claims of it.
        signed: tilo_actions::remote::SignedCall::of(request).map(Box::new),
        ..tilo_actions::ActionContext::default()
    }
}
