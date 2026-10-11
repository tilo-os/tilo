//! Agents as session participants: who an agent is, what it may touch, and
//! the vocabulary a browser uses to see and steer it.
//!
//! An agent is a device of the user. It holds its own device key and a
//! root-signed `DeviceCert` issued through ordinary device certification
//! (with no capabilities), and a root-signed [`AgentDelegation`] naming it an
//! agent, its label and cursor colour, and the rooms and apps it may act in.
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
//! for a fresh agent key carrying the [`AgentProposal`], and the approver
//! answers with the agent's certificate and the delegation together
//! ([`crate::certify::ApproverDecision::IssueAgent`]).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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

/// Largest agent id and label.
pub const MAX_AGENT_TEXT_BYTES: usize = 64;
/// Rooms one delegation may name.
pub const MAX_DELEGATED_ROOMS: usize = 16;
/// Apps one delegation may name.
pub const MAX_DELEGATED_APPS: usize = 64;

const DELEGATION_DOMAIN: &[u8] = b"tilo-agent-delegation-v1\0";

/// The resource id of agent `agent_id`.
#[must_use]
pub fn agent_resource(agent_id: &str) -> String {
    format!("{AGENT_RESOURCE_PREFIX}{agent_id}")
}

/// Where an agent may act.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentScope {
    /// Room uuids, 1..=[`MAX_DELEGATED_ROOMS`].
    pub rooms: Vec<String>,
    /// App ids; empty means every app in those rooms.
    pub apps: Vec<String>,
    /// Whether the agent may satisfy an action's confirmation step itself.
    /// Without it, an action that asks a person to confirm is refused.
    pub may_confirm: bool,
}

/// A root-signed statement that `agent_device_public_key` is an agent of
/// `user_public_key`, how it is shown, and where it may act.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDelegation {
    /// The delegating user's root.
    pub user_public_key: [u8; 32],
    /// The agent's certified device key.
    pub agent_device_public_key: [u8; 32],
    /// Stable id, 1..=[`MAX_AGENT_TEXT_BYTES`] bytes.
    pub agent_id: String,
    /// Display name, 1..=[`MAX_AGENT_TEXT_BYTES`] bytes.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// What it may touch.
    pub scope: AgentScope,
    /// Issue time, ns since the epoch.
    pub issued_at_ns: u64,
    /// Expiry, ns since the epoch. Delegations always expire.
    pub expires_at_ns: u64,
    /// Root signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Why a delegation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    /// Signed by another root than the one expected.
    #[error("the delegation names another user")]
    WrongUser,
    /// The signature does not verify.
    #[error("the delegation signature does not verify")]
    Signature,
    /// Outside its validity window.
    #[error("the delegation has expired")]
    Expired,
    /// A field is out of bounds.
    #[error("invalid delegation: {0}")]
    Invalid(&'static str),
}

#[derive(Serialize)]
struct Unsigned<'a> {
    user_public_key: &'a [u8; 32],
    agent_device_public_key: &'a [u8; 32],
    agent_id: &'a str,
    label: &'a str,
    color: &'a [u8; 3],
    scope: &'a AgentScope,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl AgentDelegation {
    /// Sign a delegation of `agent_device_public_key` with the user's `root`.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        root: &SigningKey,
        agent_device_public_key: [u8; 32],
        agent_id: impl Into<String>,
        label: impl Into<String>,
        color: [u8; 3],
        scope: AgentScope,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Result<Self, DelegationError> {
        let mut delegation = Self {
            user_public_key: root.verifying_key().to_bytes(),
            agent_device_public_key,
            agent_id: agent_id.into(),
            label: label.into(),
            color,
            scope,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        delegation.validate()?;
        delegation.signature = root.sign(&delegation.signing_bytes()).to_bytes();
        Ok(delegation)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let unsigned = Unsigned {
            user_public_key: &self.user_public_key,
            agent_device_public_key: &self.agent_device_public_key,
            agent_id: &self.agent_id,
            label: &self.label,
            color: &self.color,
            scope: &self.scope,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        };
        let mut bytes = DELEGATION_DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&unsigned).unwrap_or_default());
        bytes
    }

    fn validate(&self) -> Result<(), DelegationError> {
        let text = |value: &str| !value.is_empty() && value.len() <= MAX_AGENT_TEXT_BYTES;
        if !text(&self.agent_id) || !text(&self.label) {
            return Err(DelegationError::Invalid("agent id or label"));
        }
        if self.scope.rooms.is_empty()
            || self.scope.rooms.len() > MAX_DELEGATED_ROOMS
            || self
                .scope
                .rooms
                .iter()
                .any(|room| crate::room_workspace_id(room).is_err())
        {
            return Err(DelegationError::Invalid("rooms"));
        }
        if self.scope.apps.len() > MAX_DELEGATED_APPS
            || self
                .scope
                .apps
                .iter()
                .any(|app| app.is_empty() || app.len() > 128)
        {
            return Err(DelegationError::Invalid("apps"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(DelegationError::Invalid("validity window"));
        }
        Ok(())
    }

    /// Check the delegation was signed by `user_public_key` and is valid at
    /// `now_ns`.
    ///
    /// # Errors
    ///
    /// Returns the first reason it is not usable.
    pub fn verify(&self, user_public_key: &[u8; 32], now_ns: u64) -> Result<(), DelegationError> {
        if &self.user_public_key != user_public_key {
            return Err(DelegationError::WrongUser);
        }
        self.validate()?;
        let key =
            VerifyingKey::from_bytes(user_public_key).map_err(|_| DelegationError::Signature)?;
        key.verify(
            &self.signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| DelegationError::Signature)?;
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    /// Whether the agent may be in relay workspace `workspace_id`.
    #[must_use]
    pub fn permits_workspace(&self, workspace_id: u64) -> bool {
        self.scope
            .rooms
            .iter()
            .any(|room| crate::room_workspace_id(room).is_ok_and(|id| id == workspace_id))
    }

    /// Whether the agent may act in `app_id`.
    #[must_use]
    pub fn permits_app(&self, app_id: &str) -> bool {
        self.scope.apps.is_empty() || self.scope.apps.iter().any(|app| app == app_id)
    }

    /// Canonical encoding, for carrying the delegation to a browser.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    /// Decode [`Self::encode`] output. The caller still verifies it.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Invalid`] for undecodable bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DelegationError> {
        postcard::from_bytes(bytes).map_err(|_| DelegationError::Invalid("encoding"))
    }
}

/// What a person asks an agent to be allowed, before the approver signs it:
/// everything an [`AgentDelegation`] states except the device and times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProposal {
    /// Stable id the delegation will carry.
    pub agent_id: String,
    /// Display name.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// Rooms, apps and whether it may confirm.
    pub scope: AgentScope,
    /// Latest expiry the person asked for, ns since the epoch.
    pub expires_at_ns: u64,
}

impl AgentProposal {
    /// The delegation of this proposal for `agent_device_public_key`,
    /// signed with the user's `root`: what an approver sends back.
    ///
    /// # Errors
    ///
    /// Returns [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        &self,
        root: &SigningKey,
        agent_device_public_key: [u8; 32],
        now_ns: u64,
    ) -> Result<AgentDelegation, DelegationError> {
        AgentDelegation::sign(
            root,
            agent_device_public_key,
            self.agent_id.clone(),
            self.label.clone(),
            self.color,
            self.scope.clone(),
            now_ns,
            self.expires_at_ns,
        )
    }

    /// Whether `delegation` grants no more than this proposal: same agent,
    /// label and colour, rooms and apps within it, confirmation only if
    /// asked, and an expiry no later.
    ///
    /// # Errors
    ///
    /// Returns what is wider than proposed.
    pub fn admits(&self, delegation: &AgentDelegation) -> Result<(), &'static str> {
        if delegation.agent_id != self.agent_id
            || delegation.label != self.label
            || delegation.color != self.color
        {
            return Err("the delegation names another agent than proposed");
        }
        let scope = &delegation.scope;
        if !scope
            .rooms
            .iter()
            .all(|room| self.scope.rooms.contains(room))
        {
            return Err("the delegation names a room that was not proposed");
        }
        if !self.scope.apps.is_empty()
            && (scope.apps.is_empty()
                || !scope.apps.iter().all(|app| self.scope.apps.contains(app)))
        {
            return Err("the delegation names an app that was not proposed");
        }
        if scope.may_confirm && !self.scope.may_confirm {
            return Err("the delegation may confirm, which was not proposed");
        }
        if delegation.expires_at_ns > self.expires_at_ns {
            return Err("the delegation outlasts the proposal");
        }
        Ok(())
    }
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
    /// Whether the agent may satisfy confirmation steps.
    #[serde(default)]
    pub may_confirm: bool,
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
    /// Whether its agent may satisfy confirmation steps.
    #[serde(default)]
    pub may_confirm: bool,
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

    const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    fn delegation(root: &SigningKey) -> AgentDelegation {
        AgentDelegation::sign(
            root,
            [7; 32],
            "writer",
            "Writer helper",
            [0x7c, 0x3a, 0xed],
            AgentScope {
                rooms: vec![ROOM.into()],
                apps: vec!["so.tilo.scratchpad".into()],
                may_confirm: false,
            },
            10,
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn a_delegation_verifies_against_its_root_only() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let other = SigningKey::from_bytes(&[2; 32]);
        let signed = delegation(&root);
        let user = root.verifying_key().to_bytes();
        assert_eq!(signed.verify(&user, 500), Ok(()));
        assert_eq!(
            signed.verify(&other.verifying_key().to_bytes(), 500),
            Err(DelegationError::WrongUser)
        );
        assert_eq!(signed.verify(&user, 1_000), Err(DelegationError::Expired));
        let mut widened = signed.clone();
        widened.scope.apps.clear();
        assert_eq!(
            widened.verify(&user, 500),
            Err(DelegationError::Signature),
            "a scope cannot be widened after signing"
        );
        assert_eq!(AgentDelegation::decode(&signed.encode()), Ok(signed));
    }

    #[test]
    fn scope_names_rooms_and_apps() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let signed = delegation(&root);
        assert!(signed.permits_workspace(crate::room_workspace_id(ROOM).unwrap()));
        assert!(!signed.permits_workspace(1));
        assert!(signed.permits_app("so.tilo.scratchpad"));
        assert!(!signed.permits_app("so.tilo.notes"));
        assert!(matches!(
            AgentDelegation::sign(
                &root,
                [7; 32],
                "a",
                "A",
                [0; 3],
                AgentScope::default(),
                1,
                2
            ),
            Err(DelegationError::Invalid("rooms"))
        ));
    }

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
    fn an_approver_signs_no_more_than_was_proposed() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let proposal = AgentProposal {
            agent_id: "writer".into(),
            label: "Writer helper".into(),
            color: [0x7c, 0x3a, 0xed],
            scope: AgentScope {
                rooms: vec![ROOM.into()],
                apps: vec!["so.tilo.notes".into(), "so.tilo.sheets".into()],
                may_confirm: false,
            },
            expires_at_ns: 1_000,
        };
        let signed = proposal.sign(&root, [7; 32], 10).unwrap();
        assert_eq!(proposal.admits(&signed), Ok(()));
        let mut narrower = proposal.clone();
        narrower.scope.apps.pop();
        assert_eq!(
            proposal.admits(&narrower.sign(&root, [7; 32], 10).unwrap()),
            Ok(())
        );
        let mut wider = proposal.clone();
        wider.scope.apps.clear();
        assert!(
            proposal
                .admits(&wider.sign(&root, [7; 32], 10).unwrap())
                .is_err()
        );
        let mut longer = proposal.clone();
        longer.expires_at_ns = 2_000;
        assert!(
            proposal
                .admits(&longer.sign(&root, [7; 32], 10).unwrap())
                .is_err()
        );
        let mut confirming = proposal.clone();
        confirming.scope.may_confirm = true;
        assert!(
            proposal
                .admits(&confirming.sign(&root, [7; 32], 10).unwrap())
                .is_err()
        );
        assert_eq!(parse_color_hex("#7c3aed"), Some([0x7c, 0x3a, 0xed]));
        assert_eq!(parse_color_hex("7c3aed"), None);
    }
}
