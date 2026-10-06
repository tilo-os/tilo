//! Shared authored Agent Message vocabulary. These values are replay data,
//! never delivery authority: only the owning broker/native owner can admit
//! explicitly addressed input from a certified invocation and committed row.
use serde::{Deserialize, Serialize};

/// Explicit authored intent, never inferred from the message's prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentIntent {
    Message,
    Queue,
    Steer,
}

impl std::str::FromStr for AgentIntent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "message" => Ok(Self::Message),
            "queue" => Ok(Self::Queue),
            "steer" => Ok(Self::Steer),
            _ => Err(()),
        }
    }
}

impl AgentIntent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }
}

/// The authored body of one direct request. Chat instantiates its actual
/// rich types; an owner can retain inert serialized extras for correlation
/// without obtaining any authority to resolve an attachment or reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    deserialize = "Attachment: Deserialize<'de>, Mention: Deserialize<'de>, Reference: Deserialize<'de>, Body: Deserialize<'de>"
))]
pub struct AgentMessagePayload<Attachment, Mention, Reference, Body> {
    /// A local id is descriptive. Broker dispatch pins the invocation-derived
    /// request row, independently of any id supplied in this payload.
    #[serde(default)]
    pub id: String,
    pub ts: i64,
    pub text: String,
    pub intent: AgentIntent,
    #[serde(default)]
    pub attachment: Option<Attachment>,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(default)]
    pub reference: Option<Reference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Body>,
}

/// Exact canonical `PostAgentMessage` wire shape, shared with the Chat action
/// variant. Deserializing this value never mints an input receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentMessageMutation<Payload> {
    PostAgentMessage {
        workspace: String,
        agent: String,
        message: Payload,
    },
}

/// Native owners inspect extras as inert JSON, not as resource capabilities.
pub type SerializedAgentMessagePayload =
    AgentMessagePayload<serde_json::Value, serde_json::Value, serde_json::Value, serde_json::Value>;

/// Existing durable direct-conversation identity. The FNV-1a byte sequence is
/// frozen for compatibility, not used as an authentication or secret boundary.
pub fn agent_conversation_channel_id(workspace: &str, agent: &str) -> String {
    let part = |suffix: u8| {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in b"tilo.chat.agent-conversation.v1\0"
            .iter()
            .copied()
            .chain(workspace.bytes())
            .chain([0])
            .chain(agent.bytes())
            .chain([suffix])
        {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    };
    let identity = u128::from(part(0)) << 64 | u128::from(part(1));
    format!("c_agent_{}", uuid::Uuid::from_u128(identity).simple())
}
