//! What a connection's invocations are attributed to: one agent session, named by its caller or
//! given by its provider.

use crate::Caller;

/// What a connection's invocations are attributed to.
pub(crate) enum Session {
    /// A session this connection named, labelled as the caller the provider
    /// admitted it as says ([`Caller::session_group`]).
    Named(String),
    /// A group the provider gave the connection, kept as it gave it.
    Given(tilo_actions::WorkGroup),
}

impl Session {
    pub(crate) fn group(&self, caller: &Caller) -> tilo_actions::WorkGroup {
        match self {
            Self::Named(id) => caller.session_group(id.clone()),
            Self::Given(group) => group.clone(),
        }
    }
}

/// The session the client says it is: `{"so.tilo": {"session-name": "Office Hours"}}`, as
/// `tilo mcp` writes what its agent's harness names the session. Display only: what the workspace
/// shows the agent by, never what the connection may do.
pub(crate) const SURFACE_SESSION_NAME: &str = "/capabilities/experimental/so.tilo/session-name";
/// The most characters of a session name a client may say.
const MAX_SESSION_NAME: usize = 64;

/// Name `held`, the connection's session, as the client's `initialize` (`params`) says it is
/// ("Office Hours"): what the workspace shows the agent by, in place of its product's name and
/// wherever it opens a window or points. A name, never authority.
pub(crate) fn name_from_initialize(
    caller: &Caller,
    held: &mut Option<Session>,
    params: &serde_json::Value,
) {
    let named: Option<String> = params
        .pointer(SURFACE_SESSION_NAME)
        .and_then(serde_json::Value::as_str)
        .map(|name| {
            name.trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(MAX_SESSION_NAME)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty());
    let Some(named) = named else {
        return;
    };
    let group = held.as_ref().map(|session| session.group(caller));
    if let Some(mut group) = group {
        group.label = Some(named.clone());
        group.session = Some(named);
        *held = Some(Session::Given(group));
    }
}
