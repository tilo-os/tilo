//! Private agent ledger. Recorded data never grants live execution authority.
use super::{
    AgentError, CommitBinding, CommitOperation, GrantAction, MAX_LIVE_PRIVATE_AGENTS,
    MAX_PRIVATE_AGENT_ADMISSIONS, MAX_PRIVATE_AGENT_LEDGER_BYTES, OwnerCommitRequest,
};
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};
mod validation;

/// Private owner data. Never serialize this ledger into a `StatePeer` room or publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PrivateAgentLedger {
    pub version: u32,
    pub facts: BTreeMap<String, AgentRuntimeFactReceipt>,
}
impl Default for PrivateAgentLedger {
    fn default() -> Self {
        Self {
            version: PRIVATE_AGENT_LEDGER_VERSION,
            facts: BTreeMap::new(),
        }
    }
}
/// Decodes only the exact canonical shape: unique keys, the current version and
/// no field a re-encode would drop or reorder.
impl<'de> Deserialize<'de> for PrivateAgentLedger {
    fn deserialize<D: de::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            version: u32,
            facts: BTreeMap<String, AgentRuntimeFactReceipt>,
        }
        let value = Unique::deserialize(decoder)?.0;
        let wire: Wire = serde_json::from_value(value.clone()).map_err(de::Error::custom)?;
        if wire.version != PRIVATE_AGENT_LEDGER_VERSION {
            return Err(de::Error::custom(
                "unsupported private agent ledger version",
            ));
        }
        let ledger = Self {
            version: wire.version,
            facts: wire.facts,
        };
        if serde_json::to_value(&ledger).map_err(de::Error::custom)? != value {
            return Err(de::Error::custom("noncanonical private agent ledger shape"));
        }
        Ok(ledger)
    }
}
impl PrivateAgentLedger {
    /// Check this ledger's internal consistency for `room` at `generation`.
    ///
    /// # Errors
    ///
    /// Returns an error if a record or its receipt history is inconsistent.
    pub fn validate(&self, room: &str, generation: u64) -> Result<(), String> {
        validation::validate(self, room, generation)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInvocationScope {
    pub machine: String,
    pub room: String,
    pub host: String,
    pub caller: tilo_user_identity::DeviceCert,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProviderRoute {
    pub app_id: String,
    pub app_peer: String,
    pub cluster_id: String,
    pub record_id: String,
    pub resource: String,
    pub resource_kind: String,
    pub action: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIntent {
    pub scope: AgentInvocationScope,
    pub route: AgentProviderRoute,
    pub descriptor_revision: String,
    pub request: String,
    pub grants: Vec<GrantAction>,
}

pub const PRIVATE_AGENT_LEDGER_VERSION: u32 = 1;
pub const MAX_AGENT_RUNTIME_FACTS: usize = 12;
pub const AGENT_CONTROL_CREDIT: u32 = 512 * 1024;
pub const AGENT_TASK_CREDIT: u32 = 192 * 1024;
pub const AGENT_PROCESS_CREDIT: u32 = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeFactReceipt {
    pub binding: CommitBinding,
    pub generation: u64,
    pub change: AgentRuntimeChange,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRuntimeChange {
    pub mutation: String,
    pub invocation: String,
    pub expected_generation: u64,
    pub expected_governance: String,
    pub payload_digest: [u8; 32],
    pub fact: AgentRuntimeFact,
}
impl AgentRuntimeChange {
    /// Canonical bytes of this change's business content.
    ///
    /// # Errors
    ///
    /// Returns an error if the change cannot be encoded.
    pub fn business_bytes(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(&(
            &self.mutation,
            &self.invocation,
            &self.expected_governance,
            &self.fact,
        ))
        .map_err(|e| e.to_string())
    }
    /// Structural validation of this change.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first malformed field.
    pub fn validate(&self) -> Result<(), String> {
        exact_uuid(&self.invocation)?;
        exact_hash(&self.expected_governance)?;
        if self.expected_generation == 0
            || self.mutation != self.fact.mutation_key(&self.invocation)
            || self.payload_digest != *blake3::hash(&self.business_bytes()?).as_bytes()
        {
            return Err("invalid private runtime fact identity/digest".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentExecutionIds {
    pub agent_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub message_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReservation {
    pub control_bytes: u32,
    pub task_bytes: u32,
    pub process_bytes: u32,
    pub fact_slots: u8,
}
impl AgentReservation {
    pub fn new(spawn: bool) -> Self {
        Self {
            control_bytes: AGENT_CONTROL_CREDIT,
            task_bytes: AGENT_TASK_CREDIT,
            process_bytes: if spawn { AGENT_PROCESS_CREDIT } else { 0 },
            fact_slots: 11,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedBindingFacts {
    pub room: String,
    pub app: String,
    pub agent: String,
    pub task: String,
    pub run: String,
    pub launch: String,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedRuntimeFacts {
    pub runtime_epoch: String,
    pub package_id: String,
    pub package_hash: String,
    pub executable_hash: String,
    pub runtime_hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedProcessIdentity {
    pub pid: u32,
    pub start: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedChannelProcessFacts {
    pub binding: ManagedBindingFacts,
    pub runtime: RetainedRuntimeFacts,
    pub process: RetainedProcessIdentity,
    pub supervisor: RetainedProcessIdentity,
    pub channel_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientAcceptance {
    pub version: u16,
    pub message_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub runtime_epoch: String,
    pub recipient_agent: String,
    pub channel_lease: String,
    pub acceptance_sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorExit {
    pub runtime: RetainedRuntimeFacts,
    pub binding: ManagedBindingFacts,
    pub supervisor: RetainedProcessIdentity,
    #[serde(deserialize_with = "required_nullable")]
    pub exit_code: Option<i32>,
    #[serde(deserialize_with = "required_nullable")]
    pub signal: Option<i32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "public wire enum: boxing a variant would change every constructor and match"
)]
pub enum AgentRuntimeFact {
    Admitted {
        ids: AgentExecutionIds,
        intent: AgentIntent,
        #[serde(deserialize_with = "required_nullable")]
        parent_invocation: Option<String>,
        reservation: AgentReservation,
        timestamp_ms: u64,
    },
    LaunchIntended {
        binding: ManagedBindingFacts,
        runtime: RetainedRuntimeFacts,
    },
    ProcessBound {
        launch_fact: String,
        process: RetainedChannelProcessFacts,
    },
    DispatchIntended {
        process_fact: String,
        envelope: String,
    },
    Completed {
        dispatch_fact: String,
        acceptance: RecipientAcceptance,
        timestamp_ms: u64,
    },
    Failed {
        error: AgentError,
        timestamp_ms: u64,
    },
    Unknown {
        error: AgentError,
        timestamp_ms: u64,
    },
    TaskUnknown {
        error: AgentError,
        timestamp_ms: u64,
    },
    TaskFinished {
        completed_fact: String,
        acceptance_sequence: u64,
        update_sequence: u64,
        outcome: TaskOutcome,
        text: String,
        timestamp_ms: u64,
    },
    ProcessExited {
        launch_fact: String,
        exit: SupervisorExit,
        timestamp_ms: u64,
    },
    /// Durable parent-step to child-operation mapping of one accepted task,
    /// committed before any attempt for that child can leave the host.
    /// `child` is the sender's 48-byte ticket and `intent` the digest of the
    /// child's immutable semantic request, both lowercase hex.
    TaskStepBound {
        dispatch_fact: String,
        step: u32,
        child: String,
        intent: String,
    },
    /// The exact committed child result and the parent's next-step
    /// continuation, committed before the child obligation is released.
    TaskStepCommitted {
        bound_fact: String,
        step: u32,
        result: String,
        result_json: String,
        continuation: String,
    },
}
/// Steps one accepted task may bind; each bound step may commit once.
pub const MAX_AGENT_TASK_STEPS: u32 = 64;
pub const MAX_AGENT_STEP_RESULT_BYTES: usize = 64 * 1024;
pub const MAX_AGENT_STEP_CONTINUATION_BYTES: usize = 16 * 1024;
impl AgentRuntimeFact {
    /// Ledger key. Singleton facts use `invocation:kind`; step facts append
    /// their step index so each step has its own immutable record.
    pub fn mutation_key(&self, invocation: &str) -> String {
        match self {
            Self::TaskStepBound { step, .. } | Self::TaskStepCommitted { step, .. } => {
                format!("{invocation}:{}:{step}", self.kind())
            }
            _ => format!("{invocation}:{}", self.kind()),
        }
    }
    pub fn is_task_step(&self) -> bool {
        matches!(
            self,
            Self::TaskStepBound { .. } | Self::TaskStepCommitted { .. }
        )
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Admitted { .. } => "admitted",
            Self::LaunchIntended { .. } => "launch_intended",
            Self::ProcessBound { .. } => "process_bound",
            Self::DispatchIntended { .. } => "dispatch_intended",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Unknown { .. } => "unknown",
            Self::TaskUnknown { .. } => "task_unknown",
            Self::TaskFinished { .. } => "task_finished",
            Self::ProcessExited { .. } => "process_exited",
            Self::TaskStepBound { .. } => "task_step_bound",
            Self::TaskStepCommitted { .. } => "task_step_committed",
        }
    }
    fn receipt_limit(&self) -> usize {
        match self {
            Self::TaskStepBound { .. } => 4 * 1024,
            Self::TaskStepCommitted { .. } | Self::TaskFinished { .. } => 128 * 1024,
            Self::Admitted { .. } => 256 * 1024,
            Self::LaunchIntended { .. }
            | Self::ProcessBound { .. }
            | Self::ProcessExited { .. } => 16 * 1024,
            Self::DispatchIntended { .. } => 320 * 1024,
            _ => 32 * 1024,
        }
    }
}
fn required_nullable<'de, D, T>(decoder: D) -> Result<Option<T>, D::Error>
where
    D: de::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(decoder)
}
fn exact_uuid(value: &str) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(value).map_err(|e| e.to_string())?;
    if id.is_nil() || id.to_string() != value {
        return Err("invalid exact runtime UUID".into());
    }
    Ok(())
}
fn exact_hash(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err("invalid exact runtime hash".into());
    }
    Ok(())
}

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        decoder.deserialize_any(UniqueVisitor).map(Unique)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<Unique>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate private agent JSON key"));
            }
            values.insert(key, map.next_value::<Unique>()?.0);
        }
        Ok(Value::Object(values))
    }
}
