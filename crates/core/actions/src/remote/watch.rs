//! What a watch of a workspace follows besides its own [session state](super::ActionProvider::session_state)
//! and its runs: the records of the workspace, whose [feed](super::ChangeFeed) reports every change
//! and whose `history` verbs list them, and who is in it, which its
//! [roster](super::ActionProvider::here) names.
//!
//! A provider tells a waiting watch that one of them may have moved
//! ([`super::ActionProvider::changed`]), and a caller asks that of another device with
//! [`super::RemoteActionMessage::ChangedRequest`].

use super::{ActionCallError, ActionProvider, Caller, Pending, RemoteActionMessage};

/// One member of a workspace present now: a person, or an agent acting in it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Here {
    /// The member's id: a person's user root key, or an agent's id.
    pub who: String,
    /// The name people read for them; the id when the roster names none.
    pub name: String,
    /// `person` or `agent`.
    pub kind: String,
    /// The window they work in, as their presence says: `None` while they work in none, or
    /// where the roster does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub works_in: Option<HereWindow>,
    /// The keys of the devices they are present on now (lowercase hex), as the presence service
    /// hears them: the device's own links and the devices it reaches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<String>,
}

/// What a provider that keeps no roster answers a read of who is here.
pub const HERE_UNAVAILABLE: &str = "this provider keeps no roster of who is in the workspace";

/// The window a member works in, as their presence names it to everyone in the workspace.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HereWindow {
    /// The app the window shows, as its stable id.
    pub app: String,
    /// The resource it shows, as the 16-hex short digest of its live resource: what a reader
    /// matches against the resources it holds.
    pub document: String,
}

/// What a watch of a workspace follows besides its own
/// [session state](ActionProvider::session_state) and its runs: the two sources a provider reports
/// changes of ([`ActionProvider::changed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WatchScope {
    /// The records of the workspace: every change a [feed](super::ChangeFeed) reports and every
    /// step a resource's own `history` lists.
    Records,
    /// Who is in the workspace: its [roster](ActionProvider::here).
    Here,
}

/// What a provider that cannot tell when what a watch follows changed answers
/// ([`ActionProvider::changed`]).
pub const CHANGED_UNAVAILABLE: &str =
    "this provider cannot tell when the workspace's records or its people changed";

/// The next time what `scope` follows may have changed, as `provider` tells it
/// ([`ActionProvider::changed`]), answered for `correlation`: a provider that cannot tell says so,
/// and the device that asked tells its waiter why rather than leaving it to a signal that never
/// comes.
pub(super) fn noticed<H: ActionProvider>(
    provider: &H,
    caller: &Caller,
    correlation: u64,
    scope: WatchScope,
) -> Pending<RemoteActionMessage> {
    let respond = move |result| RemoteActionMessage::ChangedResponse {
        correlation,
        result,
    };
    match provider.changed(caller, scope) {
        Some(changed) => changed.map(respond),
        None => Pending::ready(respond(Err(ActionCallError::Unavailable {
            reason: CHANGED_UNAVAILABLE.into(),
        }))),
    }
}
