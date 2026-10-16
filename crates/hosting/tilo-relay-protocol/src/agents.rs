//! Agents as session participants: who an agent is, what it may touch, and
//! the vocabulary a browser uses to see and steer it.
//!
//! An agent is a device of the user. It holds its own device key and a
//! root-signed `DeviceCert` issued through ordinary device certification
//! (with no capabilities), and a root-signed
//! [`crate::delegation::Delegation`] naming it an agent, its label and cursor colour, and the rooms and apps it may act in.
//! The host refuses an agent device outside its rooms; browsers verify the
//! delegation against the user root before drawing the agent as "yours".
//!
//! Steering is `tilo_actions::remote` over [`Envelope::RemoteAction`](crate::Envelope::RemoteAction),
//! addressed to the host that supervises the agents:
//!
//! | Resource | Action | Argument | Answer |
//! |---|---|---|---|
//! | [`SUPERVISOR_RESOURCE`] | [`STATUS_ACTION`] | none | [`AgentsStatus`] JSON |
//! | [`SUPERVISOR_RESOURCE`] | [`REQUEST_GRANT_ACTION`] | [`GrantRequest`] JSON | `{grant, pairingCode}` |
//! | [`SUPERVISOR_RESOURCE`] | [`SPAWN_ACTION`] | `{grant, task, room?}` | `{agentId, taskId}` |
//! | `agent:<id>` | [`MESSAGE_ACTION`] | text | `{taskId}` |
//! | `agent:<id>` | [`CANCEL_TASK_ACTION`] | task id | `{}` |
//! | `agent:<id>` | [`PAUSE_ACTION`], [`RESUME_ACTION`], [`STOP_ACTION`] | none | `{}` |
//! | `agent:<id>` | [`REVOKE_ACTION`] | none | `{}` |
//!
//! Every request is an invocation with the caller's own id. The supervisor
//! retains the last 1024 invocations (including status polls) in its shared
//! in-memory ledger. While retained, a resent spawn or message returns the
//! first answer and conflicting input is refused. Eviction or host restart
//! ends that retry window; this is not a durable exactly-once guarantee.
//!
//! A grant is requested by a person in the page and approved on a
//! root-holding device in one step: the host queues a certification request
//! for a fresh agent key carrying the
//! [`crate::delegation::DelegateProposal`], and the approver
//! answers with the agent's certificate and the delegation together
//! ([`crate::certify::ApproverDecision::IssueDelegate`]).

use serde::{Deserialize, Serialize};

/// Provider token of the host's agent supervisor in remote-action targets.
pub const AGENTS_PROVIDER: &str = "agents";
/// The supervisor's own resource.
pub const SUPERVISOR_RESOURCE: &str = "agents";
/// Resource kind of the supervisor.
pub const SUPERVISOR_KIND: &str = "tilo.agents";
/// Resource kind of one agent.
pub const AGENT_KIND: &str = "tilo.agent";
/// Prefix of one agent's resource id: `agent:<agent id>`.
pub const AGENT_RESOURCE_PREFIX: &str = "agent:";
/// Snapshot every agent the caller may see.
pub const STATUS_ACTION: &str = "status";
/// Start an agent under an approved delegation with a first task.
pub const SPAWN_ACTION: &str = "spawn";
/// Give an agent a new task.
pub const MESSAGE_ACTION: &str = "message";
/// Hold the agent: it finishes nothing new until resumed.
pub const PAUSE_ACTION: &str = "pause";
/// Let a paused agent continue.
pub const RESUME_ACTION: &str = "resume";
/// End the agent's current task and every queued one.
pub const STOP_ACTION: &str = "stop";
/// Cancel one queued or running task; the agent goes on with the next.
pub const CANCEL_TASK_ACTION: &str = "cancel-task";
/// Ask the user's approver for a new agent grant.
pub const REQUEST_GRANT_ACTION: &str = "request-grant";
/// Withdraw a grant: its agent stops and the host refuses its device.
pub const REVOKE_ACTION: &str = "revoke";

/// The resource id of agent `agent_id`.
#[must_use]
pub fn agent_resource(agent_id: &str) -> String {
    format!("{AGENT_RESOURCE_PREFIX}{agent_id}")
}

/// The argument of [`REQUEST_GRANT_ACTION`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRequest {
    /// Display name, 1..=[`MAX_AGENT_TEXT_BYTES`] bytes.
    pub label: String,
    /// Cursor colour `#rrggbb`; the host picks one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Room uuids.
    pub rooms: Vec<String>,
    /// App ids; empty is every app.
    #[serde(default)]
    pub apps: Vec<String>,
    /// How long the grant lasts, in seconds.
    pub expires_in_secs: u64,
}

/// Parse `#rrggbb`.
#[must_use]
pub fn parse_color_hex(text: &str) -> Option<[u8; 3]> {
    let bytes = hex::decode(text.strip_prefix('#')?).ok()?;
    <[u8; 3]>::try_from(bytes).ok()
}

/// Where a grant stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantState {
    /// Waiting for the approver.
    Pending,
    /// Approved: an agent may be spawned under it.
    #[default]
    Approved,
    /// The approver declined, or nobody answered in time.
    Denied,
    /// Past its expiry.
    Expired,
    /// Withdrawn by a person.
    Revoked,
}

/// Where an agent stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Joining the room.
    Starting,
    /// Working on a task.
    Running,
    /// Idle, waiting for a task.
    Waiting,
    /// Held by a person.
    Paused,
    /// Stopped by a person; it takes no further tasks.
    Stopped,
    /// Its session failed.
    Failed,
}

/// Where one task stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// Waiting for the agent.
    Queued,
    /// Being worked on.
    Running,
    /// Done.
    Completed,
    /// Could not be done; see its error.
    Failed,
    /// Stopped by a person.
    Cancelled,
}

/// One app action a task performed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    /// App the action belongs to.
    pub app: String,
    /// Resource id.
    pub resource: String,
    /// Action id.
    pub action: String,
    /// `completed` or `failed`.
    pub state: TaskState,
    /// Refusal or failure text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTask {
    /// Task id.
    pub id: String,
    /// What was asked.
    pub text: String,
    /// Where it stands.
    pub state: TaskState,
    /// Actions performed so far.
    pub steps: Vec<AgentStep>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Who said something in an agent's conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// A person.
    User,
    /// The agent.
    Agent,
}

/// One conversation entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessage {
    /// Speaker.
    pub role: MessageRole,
    /// Text.
    pub text: String,
}

/// One agent as a browser shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// Agent id, as in its delegation.
    pub id: String,
    /// Display name.
    pub label: String,
    /// Cursor colour, `#rrggbb`.
    pub color: String,
    /// Its device key (hex): the live publisher its cursor arrives from.
    pub device: String,
    /// Where it stands.
    pub status: AgentStatus,
    /// Short activity line, as in its presence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// The room it works in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Newest first is not implied; tasks in the order they were given.
    pub tasks: Vec<AgentTask>,
    /// Conversation in order.
    pub conversation: Vec<AgentMessage>,
    /// [`AgentDelegation::encode`], hex: the browser verifies it against the
    /// user root before calling the agent "yours".
    pub delegation: String,
}

/// An approved delegation an agent can be spawned under (Cloud's "grant
/// offer").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGrant {
    /// The delegation's agent id; spawn names it.
    pub id: String,
    /// Display name.
    pub label: String,
    /// Rooms it covers.
    pub rooms: Vec<String>,
    /// Apps it covers; empty is every app.
    pub apps: Vec<String>,
    /// Whether an agent under it is already running.
    pub in_use: bool,
    /// Where the grant stands.
    #[serde(default)]
    pub state: GrantState,
    /// Cursor colour, `#rrggbb`.
    #[serde(default)]
    pub color: String,
    /// Expiry, ms since the epoch.
    #[serde(default)]
    pub expires_at_ms: u64,
    /// The code the approving device shows while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
    /// Why it was denied, expired or revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The answer to [`STATUS_ACTION`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsStatus {
    /// Always 1.
    pub version: u8,
    /// Whether the supervisor implements individual task cancellation.
    #[serde(default)]
    pub cancel_task: bool,
    /// Every agent the caller may see.
    pub agents: Vec<AgentInfo>,
    /// Delegations an agent may be spawned under.
    pub grants: Vec<AgentGrant>,
}

/// `#rrggbb` for an sRGB colour.
#[must_use]
pub fn color_hex(color: [u8; 3]) -> String {
    format!("#{}", hex::encode(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_uses_the_browser_spelling() {
        let status = AgentsStatus {
            version: 1,
            cancel_task: true,
            agents: vec![AgentInfo {
                id: "writer".into(),
                label: "Writer helper".into(),
                color: color_hex([0x7c, 0x3a, 0xed]),
                device: "00".into(),
                status: AgentStatus::Running,
                activity: None,
                tasks: vec![AgentTask {
                    id: "t1".into(),
                    text: "tidy".into(),
                    state: TaskState::Running,
                    steps: Vec::new(),
                    error: None,
                }],
                room: None,
                conversation: Vec::new(),
                delegation: String::new(),
            }],
            grants: Vec::new(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["cancelTask"], true);
        let mut legacy = json.clone();
        legacy.as_object_mut().unwrap().remove("cancelTask");
        assert!(
            !serde_json::from_value::<AgentsStatus>(legacy)
                .unwrap()
                .cancel_task
        );
        assert_eq!(json["agents"][0]["status"], "running");
        assert_eq!(json["agents"][0]["color"], "#7c3aed");
        assert_eq!(json["agents"][0]["tasks"][0]["state"], "running");
    }

    #[test]
    fn colors_use_the_css_hex_spelling() {
        assert_eq!(parse_color_hex("#7c3aed"), Some([0x7c, 0x3a, 0xed]));
        assert_eq!(parse_color_hex("7c3aed"), None);
    }
}
