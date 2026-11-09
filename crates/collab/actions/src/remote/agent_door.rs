//! The door agents come in by from outside, as the person who owns it sees
//! it: where an agent connects, which agents it let in and into which
//! workspaces, and the person's say over each of them.
//!
//! An agent on another machine (Claude Code, Codex, any MCP client) reaches
//! the person's workspaces over MCP at [`Door::endpoint`]. The person grants
//! it once, and the grant names the workspaces it may work in. Every change
//! it makes there is attributed to it in the workspace's history
//! ([`super::workspace_activity`]), where its runs are read and undone.
//!
//! Whoever runs the door serves [`PROVIDER`]; a host without one leaves the
//! reads unanswered, and a view that follows them says so. Revoking a grant
//! ([`REVOKE`]) ends every connection it carries and lets none in again;
//! stopping an agent ([`STOP`]) ends its connections now and keeps the grant,
//! so it can come back.

use serde::{Deserialize, Serialize};

use super::{InvokeArgument, RemoteActionTarget};
use crate::door::ActionAddress;

/// The provider the agent door is served under.
pub const PROVIDER: &str = "tilo.agent-door";
/// The resource every action is an action of.
pub const RESOURCE: &str = "agent.door";

/// Live: the door and what it let in; answers [`Door`].
pub const DOOR: &str = "door";
/// Take back a grant ([`Grant`] by [`Grant::id`], as [`GrantRef`]).
pub const REVOKE: &str = "revoke";
/// End an agent's connections now, keeping its grant ([`GrantRef`]).
pub const STOP: &str = "stop";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The argument an invocation carrying `value` sends.
#[must_use]
pub fn argument(value: &impl Serialize) -> InvokeArgument {
    super::workspace_activity::argument(value)
}

/// An MCP client and the command that connects it to [`Door::endpoint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    ClaudeCode,
    Codex,
    /// Any other client that speaks MCP over HTTP.
    Other,
}

impl Client {
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Other];

    /// The one line that connects this client to the door at `endpoint`,
    /// or for [`Client::Other`] the configuration it reads.
    #[must_use]
    pub fn command(self, endpoint: &str) -> String {
        match self {
            Self::ClaudeCode => format!("claude mcp add --transport http tilo {endpoint}"),
            Self::Codex => format!("codex mcp add tilo --url {endpoint}"),
            Self::Other => {
                format!(r#"{{"mcpServers": {{"tilo": {{"type": "http", "url": "{endpoint}"}}}}}}"#)
            }
        }
    }
}

/// Where the door's grant reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reach {
    /// The workspaces it may work in, by name; empty for every workspace
    /// of the person, including the ones it makes.
    pub workspaces: Vec<String>,
    /// Whether it may make workspaces of its own.
    pub create: bool,
}

/// One agent the door let in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    /// What [`GrantRef`] names it by.
    pub id: String,
    /// The agent as its client named itself when it asked ("Claude Code").
    pub agent: String,
    /// The identity its changes are attributed to in the workspace's
    /// history, as [`super::workspace_activity::Author::identity`] spells it:
    /// what ties its runs to this grant.
    pub identity: String,
    /// The machine or account it connected from, as the person would
    /// recognise it ("quinten@studio").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Its colour, as presence and history paint it, `[r, g, b]`.
    pub rgb: [u8; 3],
    pub reach: Reach,
    /// When the person granted it, Unix milliseconds.
    pub granted_ms: i64,
    /// When it last acted; `None` before it ever did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<i64>,
    /// How many connections it holds open now.
    pub connected: u32,
}

/// Who made a workspace the door reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Maker {
    /// The person themselves.
    Person,
    /// An agent, through its grant ([`Grant::id`]).
    Agent { grant: String, agent: String },
}

/// One of the person's workspaces the door reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's id, as a run's history names it.
    pub id: String,
    pub name: String,
    pub made_by: Maker,
    /// When it was made, Unix milliseconds.
    pub made_ms: i64,
    /// Where the full desktop opens on it: a `tilo://open#…` address.
    pub open: String,
}

/// The door, as the person sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Door {
    /// The MCP endpoint agents connect to (`https://mcp.tilo.sh`).
    pub endpoint: String,
    /// Every grant, newest first.
    pub grants: Vec<Grant>,
    /// The person's workspaces it reaches, newest first.
    pub workspaces: Vec<Workspace>,
}

/// Which grant [`REVOKE`] or [`STOP`] acts on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrantRef {
    pub grant: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_client_connects_with_one_line_naming_the_endpoint() {
        let endpoint = "https://mcp.example";
        assert_eq!(
            Client::ClaudeCode.command(endpoint),
            "claude mcp add --transport http tilo https://mcp.example"
        );
        for client in Client::ALL {
            let line = client.command(endpoint);
            assert!(line.contains(endpoint), "{line}");
            assert!(!line.contains('\n'), "{line}");
        }
        let other: serde_json::Value =
            serde_json::from_str(&Client::Other.command(endpoint)).expect("the config is JSON");
        assert_eq!(other["mcpServers"]["tilo"]["url"], endpoint);
    }

    #[test]
    fn a_grant_reference_reads_back_and_refuses_what_it_does_not_name() {
        let InvokeArgument::Json(map) = argument(&GrantRef { grant: "g1".into() }) else {
            panic!("a reference is an object");
        };
        let read: GrantRef = serde_json::from_value(serde_json::Value::Object(map)).unwrap();
        assert_eq!(read.grant, "g1");
        assert!(serde_json::from_str::<GrantRef>(r#"{"grant":"g1","all":true}"#).is_err());
    }
}
