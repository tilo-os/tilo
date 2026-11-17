//! What a watch of a workspace follows besides its own [session state](super::ActionHost::session_state)
//! and its runs: the records of the workspace, whose [feed](super::ChangeFeed) reports every change
//! and whose `history` verbs list them, and who is in it, which its
//! [roster](super::ActionHost::here) names.
//!
//! A host tells a waiting watch that one of them may have moved
//! ([`super::ActionHost::changed`]), and a caller asks that of another device with
//! [`super::RemoteActionMessage::ChangedRequest`].

use super::{ActionHost, ActionHostError, Caller, Pending, RemoteActionMessage};

/// One member of a workspace present now: a person, or an agent acting in it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Here {
    /// The member's id: a person's user root key, or an agent's id.
    pub who: String,
    /// The name people read for them; the id when the roster names none.
    pub name: String,
    /// `person` or `agent`.
    pub kind: String,
}

/// What a watch of a workspace follows besides its own
/// [session state](ActionHost::session_state) and its runs: the two sources a host reports
/// changes of ([`ActionHost::changed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WatchScope {
    /// The records of the workspace: every change a [feed](super::ChangeFeed) reports and every
    /// step a resource's own `history` lists.
    Records,
    /// Who is in the workspace: its [roster](ActionHost::here).
    Here,
}

/// What a host that cannot tell when what a watch follows changed answers
/// ([`ActionHost::changed`]).
pub const CHANGED_UNAVAILABLE: &str =
    "this host cannot tell when the workspace's records or its people changed";

/// The next time what `scope` follows may have changed, as `host` tells it
/// ([`ActionHost::changed`]), answered for `correlation`: a host that cannot tell says so, and a
/// waiter then waits until its own deadline rather than on a signal that never comes.
pub(super) fn noticed<H: ActionHost>(
    host: &H,
    caller: &Caller,
    correlation: u64,
    scope: WatchScope,
) -> Pending<RemoteActionMessage> {
    let respond = move |result| RemoteActionMessage::ChangedResponse {
        correlation,
        result,
    };
    match host.changed(caller, scope) {
        Some(changed) => changed.map(move |()| respond(Ok(()))),
        None => Pending::ready(respond(Err(ActionHostError::Unavailable {
            reason: CHANGED_UNAVAILABLE.into(),
        }))),
    }
}
