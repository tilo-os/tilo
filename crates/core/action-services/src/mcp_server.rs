//! The MCP server agents come in by from outside, as the person who owns it
//! sees it: where an agent connects, which agents it let in and into which
//! workspaces, and the person's say over each of them.
//!
//! An agent on another machine (any MCP client) reaches
//! the person's workspaces over MCP at [`Server::endpoint`]. The person grants
//! it once, and the grant names the workspaces it may work in. Every change
//! it makes there is attributed to it in the workspace's history
//! (`tilo_session_core::desktop::activity::vocabulary`), where its work is
//! read and undone.
//!
//! The model is the API server's (`crates/core/tilo-api`): a
//! grant reaches every workspace of the person or exactly one, and each MCP
//! session through it is one agent session, named for the person ("Claude
//! Code #2") as History names it. Whoever serves the server serves
//! [`PROVIDER`]; a provider without one leaves the reads unanswered, and a view
//! that follows them says so. A grant's key is one of the person's keys, taken
//! back as every one is ([`tilo_actions::remote::device_tickets::REVOKE`]). Stopping an agent
//! ([`STOP`]) refuses its every call, cancels what it is running, and ends its
//! run as the connection ending ends it: its session leaves the listing, until
//! the person lets it go on ([`GO_ON`]).

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

/// The provider the MCP server is served under.
pub const PROVIDER: &str = "tilo.mcp-server";
/// The resource every action is an action of.
pub const RESOURCE: &str = "mcp.server";

/// Live: the server and what it let in; answers [`Server`].
pub const SERVER: &str = "server";
/// Stop an agent now ([`AgentSessionRef`]): its running calls are cancelled, every next one is
/// refused in every workspace it works in until [`GO_ON`], and its run's session ends as the
/// connection ending ends it.
pub const STOP: &str = "stop";
/// Let an agent its person stopped go on ([`AgentSessionRef`]): its calls are answered again,
/// and its run is listed again.
pub const GO_ON: &str = "go-on";

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
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(named)) => InvokeArgument::Json(named),
        _ => InvokeArgument::None,
    }
}

/// The MCP configuration an agent adds to reach the server at `endpoint`, as the server `tilo`
/// over HTTP: one line, for any client that speaks MCP.
#[must_use]
pub fn client_config(endpoint: &str) -> String {
    format!(r#"{{"mcpServers": {{"tilo": {{"type": "http", "url": "{endpoint}"}}}}}}"#)
}

/// Where a grant reaches, as the person chose when they let the agent in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Reach {
    /// Every workspace the person has, and making new ones.
    #[default]
    Account,
    /// One workspace, and nothing else: the agent cannot see or name others.
    Workspace {
        /// The workspace's id.
        id: String,
        name: String,
    },
}

/// One agent session through the server: one MCP connection, open now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSession {
    /// The session's name, the client's and a number of the person's own
    /// sessions ("Claude Code #2"): the name History gives it, which is what
    /// ties it to its grant.
    pub name: String,
    /// When it connected, Unix milliseconds.
    pub started_ms: i64,
}

/// One agent the server let in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    /// Its key, in hex: what [`AgentSessionRef`] and the person's keys name it by.
    pub id: String,
    /// The agent as its client named itself when the person agreed
    /// ("Claude Code").
    pub agent: String,
    pub reach: Reach,
    /// When the person granted it, Unix milliseconds.
    pub granted_ms: i64,
    /// When it last acted; `None` before it ever did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<i64>,
    /// Its agent sessions open now, oldest first.
    pub sessions: Vec<AgentSession>,
    /// The key operates web pages in Browser without asking the person each
    /// time, as its certificate carries it
    /// ([`tilo_user_identity::DeviceCapabilities::browser_pages`]): what it
    /// opens, fills, selects and clicks runs on the person's word, while
    /// submitting a page still asks them.
    pub browser_pages: bool,
    /// The person stopped the agent ([`STOP`]): its every call is refused until
    /// they let it go on ([`GO_ON`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
}

impl Grant {
    /// Its open session History names `name`.
    #[must_use]
    pub fn session(&self, name: &str) -> Option<&AgentSession> {
        self.sessions.iter().find(|session| session.name == name)
    }
}

/// Who made a workspace the server reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Maker {
    /// The person themselves.
    Person,
    /// An agent, through its grant ([`Grant::id`]).
    Agent { grant: String, agent: String },
}

/// One of the person's workspaces the server reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's id, as a session's history names it.
    pub id: String,
    pub name: String,
    /// Who made it, when the server knows: it knows the workspaces an agent
    /// made through it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<Maker>,
    /// When it was made, Unix milliseconds.
    pub made_ms: i64,
    /// Where the full desktop opens on it: a `tilo://open#…` address.
    pub open: String,
}

/// The server, as the person sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    /// The MCP endpoint agents connect to (`https://mcp.tilo.sh`).
    pub endpoint: String,
    /// Every grant, newest first.
    pub grants: Vec<Grant>,
    /// The person's workspaces it reaches, newest first.
    pub workspaces: Vec<Workspace>,
}

impl Server {
    /// The grant whose open session History names `name`, and that session.
    #[must_use]
    pub fn session(&self, name: &str) -> Option<(&Grant, &AgentSession)> {
        self.grants
            .iter()
            .find_map(|grant| grant.session(name).map(|session| (grant, session)))
    }
}

/// Which agent [`STOP`] stops and [`GO_ON`] lets go on: the grant it came in by, and the name
/// its sessions run under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSessionRef {
    pub grant: String,
    /// The session's [`AgentSession::name`].
    pub session: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn an_mcp_server_names_its_endpoint_and_grants;

        phase a_client_connects_with_one_line_naming_the_endpoint;
        {
            let endpoint = "https://mcp.example";
            let line = client_config(endpoint);
            assert!(!line.contains('\n'), "{line}");
            let config: serde_json::Value = serde_json::from_str(&line).expect("the config is JSON");
            assert_eq!(config["mcpServers"]["tilo"]["url"], endpoint);
        }

        phase a_session_in_history_finds_its_grant_by_the_name_history_gives_it;
        {
            let server = Server {
                grants: vec![Grant {
                    id: "g1".into(),
                    sessions: vec![AgentSession {
                        name: "Claude Code #2".into(),
                        started_ms: 1,
                    }],
                    ..Grant::default()
                }],
                ..Server::default()
            };
            assert_eq!(
                server.session("Claude Code #2")
                    .map(|(grant, _)| grant.id.as_str()),
                Some("g1")
            );
            assert!(
                server.session("Claude Code #1").is_none(),
                "a session that ended is no longer open"
            );
        }
    }
}
