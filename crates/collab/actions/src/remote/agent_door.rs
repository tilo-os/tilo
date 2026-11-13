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
//! The model is the API server's (`crates/hosting/tilo-api`): a
//! grant reaches every workspace of the person or exactly one, and each MCP
//! session through it is one run, named for the person ("Claude Code #2") as
//! History names it. Whoever runs the door serves [`PROVIDER`]; a host
//! without one leaves the reads unanswered, and a view that follows them
//! says so. Revoking a grant ([`REVOKE`]) ends every run it carries and lets
//! none in again; stopping a run ([`STOP`]) ends that one session and keeps
//! the grant, so the agent can come back.

use serde::{Deserialize, Serialize};

use super::{InvokeArgument, RemoteActionTarget};
use crate::door::ActionAddress;

/// The provider the agent door is served under.
pub const PROVIDER: &str = "tilo.agent-door";
/// The resource every action is an action of.
pub const RESOURCE: &str = "agent.door";

/// Live: the door and what it let in; answers [`Door`].
pub const DOOR: &str = "door";
/// Take back a grant ([`Grant`] by [`Grant::id`], as [`GrantRef`]): its
/// runs end now and it lets none in again.
pub const REVOKE: &str = "revoke";
/// End one run now, keeping its grant ([`RunRef`]).
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

/// One run of an agent through the door: one MCP session, open now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    /// The run's name, the client's and a number of the person's own runs
    /// ("Claude Code #2"): the name History gives the run, which is what
    /// ties a run in [`super::workspace_activity::RUNS`] to its grant.
    pub name: String,
    /// When it connected, Unix milliseconds.
    pub started_ms: i64,
}

/// One agent the door let in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    /// What [`GrantRef`] names it by.
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
    /// Its runs open now, oldest first.
    pub runs: Vec<Run>,
    /// The key operates web pages in Browser without asking the person each
    /// time, as its certificate carries it
    /// ([`tilo_user_identity::DeviceCapabilities::browser_pages`]): what it
    /// opens, fills, selects and clicks runs on the person's word, while
    /// submitting a page still asks them.
    pub browser_pages: bool,
    /// Where the agent runs, as its key's certificate names it: the
    /// machine it was connected from, or the service that holds the key
    /// for it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub runs_on: String,
    /// When the grant ends, Unix milliseconds; `None` for one that lasts
    /// until the person revokes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<i64>,
    /// The person made it as a key for their own script or backend rather
    /// than letting an agent's app in.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub script_key: bool,
}

impl Grant {
    /// Its open run History names `name`.
    #[must_use]
    pub fn run(&self, name: &str) -> Option<&Run> {
        self.runs.iter().find(|run| run.name == name)
    }
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
    /// Who made it, when the door knows: it knows the workspaces an agent
    /// made through it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<Maker>,
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

impl Door {
    /// The grant whose open run History names `name`, and that run.
    #[must_use]
    pub fn run(&self, name: &str) -> Option<(&Grant, &Run)> {
        self.grants
            .iter()
            .find_map(|grant| grant.run(name).map(|run| (grant, run)))
    }
}

/// Which grant [`REVOKE`] takes back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrantRef {
    pub grant: String,
}

/// Which run [`STOP`] ends: the grant it came in by, and its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunRef {
    pub grant: String,
    /// The run's [`Run::name`].
    pub run: String,
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
    fn a_run_in_history_finds_its_grant_by_the_name_history_gives_it() {
        let door = Door {
            grants: vec![Grant {
                id: "g1".into(),
                runs: vec![Run {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }],
                ..Grant::default()
            }],
            ..Door::default()
        };
        assert_eq!(
            door.run("Claude Code #2")
                .map(|(grant, _)| grant.id.as_str()),
            Some("g1")
        );
        assert!(
            door.run("Claude Code #1").is_none(),
            "a run that ended is no longer open"
        );
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
