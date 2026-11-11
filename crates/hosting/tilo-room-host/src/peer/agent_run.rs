//! A device says the session it opened is one run of an agent
//! ([`Envelope::AgentRun`]): from then on every request the session makes is
//! the agent's, for the device's person, within what bounds an agent, and the
//! person's agent door lists the run while the session lasts.

use super::SessionCtx;
use crate::auth::AuthenticatedPeer;
use crate::transport::Envelope;

/// Make this session a run of the agent `label` names, asked as `request_id`.
/// The run is present from now on in the workspace the session entered last
/// (`subscribed`), the one a device enters for its agent just before it says
/// so: a person sees the agent there as soon as it connected, not only from
/// its first request.
pub(super) fn handle_agent_run(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    request_id: u64,
    label: &str,
) -> Envelope {
    // The name is the device's word: one line, bounded.
    let label: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(tilo_relay_protocol::transport::MAX_AGENT_LABEL_CHARS)
        .collect();
    let label = label.as_str();
    let refusal = if note_agent(session, label) {
        // The person's agent door lists the run while it lasts.
        if let (Some(admin), Some(stop)) = (session.admin, session.stop) {
            admin.note_agent(&peer.cert, label, stop);
        }
        if let (Some(plane), Some(&workspace_id)) = (session.app_serving, subscribed.last())
            && let Ok(caller) = super::actions::session_caller(peer, session)
        {
            plane.run_arrived(workspace_id, &caller);
        }
        None
    } else {
        Some("this session is already the run of another agent".to_owned())
    };
    Envelope::AgentRunAnswer {
        request_id,
        refusal,
    }
}

/// Record that `session` is a run of the agent `label`, unless it already is
/// one of another: whether it is now `label`'s. An agent with no name is
/// still an agent, named for the device that runs it.
fn note_agent(session: &SessionCtx<'_>, label: &str) -> bool {
    let name = label.trim();
    let name = if name.is_empty() { "agent" } else { name };
    session.agent.get_or_init(|| name.to_owned()) == name
}
