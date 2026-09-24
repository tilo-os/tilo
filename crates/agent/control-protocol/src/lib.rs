//! Version 1 hosted-agent request and response DTOs.
//!
//! This crate defines data and validation only. Authentication, transport,
//! persistence, execution, and retry policy belong to their owning layers.

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};
use std::{cmp::Ordering, collections::HashSet};
use thiserror::Error;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_NAME_SCALARS: usize = 80;
pub const MAX_TEXT_SCALARS: usize = 8_000;
pub const MAX_OPAQUE_ID_BYTES: usize = 128;
/// Complete component-qualified SDK resource addresses retain their exact bytes.
/// This field-specific reader expansion leaves every other identity bound intact.
pub const MAX_GRANT_RESOURCE_BYTES: usize = 4096;
pub const MAX_REQUEST_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_FRAME_BYTES: usize = 512 * 1024;
pub const MAX_AGENT_PAGE_ITEMS: usize = 64;
pub const MAX_CONVERSATION_PAGE_ITEMS: usize = 100;
pub const MAX_GRANT_OFFERS: usize = 64;
pub const MAX_ACTIONS_PER_GRANT: usize = 64;
const MAX_LABEL_SCALARS: usize = 256;
const MAX_ERROR_MESSAGE_SCALARS: usize = 4_096;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("agent protocol frame exceeds its byte limit")]
    FrameTooLarge,
    #[error("agent protocol JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("unsupported agent protocol version")]
    UnsupportedVersion,
    #[error("invalid agent protocol field: {0}")]
    InvalidField(&'static str),
    #[error("agent response correlation mismatch")]
    CorrelationMismatch,
    #[error("agent response binding mismatch")]
    BindingMismatch,
    #[error("agent response kind mismatch")]
    KindMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpawnInput {
    pub name: String,
    pub task: String,
    pub grant_set_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum AgentRequest {
    #[serde(rename = "agent_capabilities")]
    Capabilities { version: u8 },
    #[serde(rename = "agent_list")]
    List {
        version: u8,
        #[serde(deserialize_with = "required_nullable")]
        cursor: Option<String>,
    },
    #[serde(rename = "agent_conversation")]
    Conversation {
        version: u8,
        agent_id: String,
        #[serde(deserialize_with = "required_nullable")]
        cursor: Option<String>,
    },
    #[serde(rename = "agent_spawn")]
    Spawn {
        version: u8,
        name: String,
        task: String,
        grant_set_id: String,
    },
    #[serde(rename = "agent_message")]
    Message {
        version: u8,
        agent_id: String,
        text: String,
    },
    #[serde(rename = "agent_status")]
    Status { version: u8, invocation_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentBinding {
    pub machine: String,
    pub room: String,
    pub device: String,
    pub host: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantAction {
    pub app_id: String,
    pub record_id: String,
    pub resource: String,
    pub action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantOffer {
    pub id: String,
    pub revision: String,
    pub expires_at: String,
    pub label: String,
    pub actions: Vec<GrantAction>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AgentUnavailableReason {
    UnsupportedVersion,
    NotConfigured,
    PackageMissing,
    ModelUnavailable,
    PermissionDenied,
    NoDelegableActions,
    CapacityReached,
    Disconnected,
    StorageFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum AgentErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    NotConfigured,
    PackageMissing,
    ModelUnavailable,
    PermissionDenied,
    NoDelegableActions,
    CapacityReached,
    Disconnected,
    StorageFailed,
    InvalidCursor,
    GrantExpired,
    GrantRevisionChanged,
    InvocationConflict,
    UnknownInvocation,
    AgentNotFound,
    OutcomeUncertain,
    InternalError,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentError {
    pub code: AgentErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentOperationState {
    Pending,
    Accepted,
    Failed,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentOperation {
    pub invocation_id: String,
    pub state: AgentOperationState,
    #[serde(deserialize_with = "required_nullable")]
    pub agent_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub task_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<AgentError>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Starting,
    Running,
    Waiting,
    Paused,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Sent,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    pub status: MessageStatus,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSnapshot {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: AgentStatus,
    pub conversation: Vec<ChatMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCapabilities {
    pub list: bool,
    pub spawn: bool,
    pub message: bool,
    pub grant_offers: Vec<GrantOffer>,
    pub unavailable_reasons: Vec<AgentUnavailableReason>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentResponseKind {
    #[serde(rename = "agent_capabilities")]
    Capabilities,
    #[serde(rename = "agent_list")]
    List,
    #[serde(rename = "agent_conversation")]
    Conversation,
    #[serde(rename = "agent_operation")]
    Operation,
    #[serde(rename = "agent_error")]
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum AgentResponse {
    #[serde(rename = "agent_capabilities")]
    Capabilities {
        version: u8,
        #[serde(rename = "invocationId")]
        invocation_id: String,
        binding: AgentBinding,
        capabilities: AgentCapabilities,
    },
    #[serde(rename = "agent_list")]
    List {
        version: u8,
        #[serde(rename = "invocationId")]
        invocation_id: String,
        binding: AgentBinding,
        agents: Vec<AgentSnapshot>,
        #[serde(rename = "nextCursor")]
        #[serde(deserialize_with = "required_nullable")]
        next_cursor: Option<String>,
        revision: String,
    },
    #[serde(rename = "agent_conversation")]
    Conversation {
        version: u8,
        #[serde(rename = "invocationId")]
        invocation_id: String,
        binding: AgentBinding,
        #[serde(rename = "agentId")]
        agent_id: String,
        messages: Vec<ChatMessage>,
        #[serde(rename = "nextCursor")]
        #[serde(deserialize_with = "required_nullable")]
        next_cursor: Option<String>,
        revision: String,
    },
    #[serde(rename = "agent_operation")]
    Operation {
        version: u8,
        #[serde(rename = "invocationId")]
        invocation_id: String,
        binding: AgentBinding,
        operation: AgentOperation,
    },
    #[serde(rename = "agent_error")]
    Error {
        version: u8,
        #[serde(rename = "invocationId")]
        invocation_id: String,
        binding: AgentBinding,
        error: AgentError,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedResponse {
    pub invocation_id: String,
    pub binding: AgentBinding,
    pub kind: Option<AgentResponseKind>,
}

/// Validate a user-facing spawn input. Normalization itself is performed by
/// the caller because this signature intentionally does not mutate the DTO.
///
/// # Errors
///
/// Returns an error when text or the grant identifier exceeds the v1 bounds.
pub fn validate_spawn(input: &SpawnInput) -> Result<(), ValidationError> {
    validate_trimmed_scalar_text(&input.name, MAX_NAME_SCALARS, "name")?;
    validate_trimmed_scalar_text(&input.task, MAX_TEXT_SCALARS, "task")?;
    validate_opaque_id(&input.grant_set_id, "grant_set_id")
}

/// Decode, validate, and normalize one bounded v1 request frame.
///
/// # Errors
///
/// Returns an error for an oversized frame, malformed or unknown JSON fields,
/// unsupported versions, or values outside the v1 bounds.
pub fn decode_request(bytes: &[u8]) -> Result<AgentRequest, ValidationError> {
    if bytes.len() > MAX_REQUEST_FRAME_BYTES {
        return Err(ValidationError::FrameTooLarge);
    }
    let request = serde_json::from_slice::<AgentRequest>(bytes)
        .map_err(|error| ValidationError::InvalidJson(error.to_string()))?;
    normalize_request(request)
}

/// Serialize a validated request in the fixed v1 DTO field order.
///
/// # Errors
///
/// Returns an error if the request is invalid or its canonical bytes exceed
/// the request-frame bound.
pub fn canonical_request(request: &AgentRequest) -> Result<Vec<u8>, ValidationError> {
    let request = normalize_request(request.clone())?;
    let bytes = match &request {
        AgentRequest::Capabilities { version } => serialize_canonical(&CanonicalCapabilities {
            version: *version,
            kind: "agent_capabilities",
        })?,
        AgentRequest::List { version, cursor } => serialize_canonical(&CanonicalList {
            version: *version,
            kind: "agent_list",
            cursor: cursor.as_deref(),
        })?,
        AgentRequest::Conversation {
            version,
            agent_id,
            cursor,
        } => serialize_canonical(&CanonicalConversation {
            version: *version,
            kind: "agent_conversation",
            agent_id,
            cursor: cursor.as_deref(),
        })?,
        AgentRequest::Spawn {
            version,
            name,
            task,
            grant_set_id,
        } => serialize_canonical(&CanonicalSpawn {
            version: *version,
            kind: "agent_spawn",
            name,
            task,
            grant_set_id,
        })?,
        AgentRequest::Message {
            version,
            agent_id,
            text,
        } => serialize_canonical(&CanonicalMessage {
            version: *version,
            kind: "agent_message",
            agent_id,
            text,
        })?,
        AgentRequest::Status {
            version,
            invocation_id,
        } => serialize_canonical(&CanonicalStatus {
            version: *version,
            kind: "agent_status",
            invocation_id,
        })?,
    };
    if bytes.len() > MAX_REQUEST_FRAME_BYTES {
        return Err(ValidationError::FrameTooLarge);
    }
    Ok(bytes)
}

/// Compare two canonical page-revision generations numerically.
///
/// # Errors
///
/// Returns an error unless each value is the canonical unsigned decimal
/// representation of a `u64` (`0` or a nonzero digit followed by digits).
pub fn compare_revisions(left: &str, right: &str) -> Result<Ordering, ValidationError> {
    Ok(parse_revision(left)?.cmp(&parse_revision(right)?))
}

#[derive(Serialize)]
struct CanonicalCapabilities {
    version: u8,
    kind: &'static str,
}

#[derive(Serialize)]
struct CanonicalList<'a> {
    version: u8,
    kind: &'static str,
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct CanonicalConversation<'a> {
    version: u8,
    kind: &'static str,
    agent_id: &'a str,
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct CanonicalSpawn<'a> {
    version: u8,
    kind: &'static str,
    name: &'a str,
    task: &'a str,
    grant_set_id: &'a str,
}

#[derive(Serialize)]
struct CanonicalMessage<'a> {
    version: u8,
    kind: &'static str,
    agent_id: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct CanonicalStatus<'a> {
    version: u8,
    kind: &'static str,
    invocation_id: &'a str,
}

fn serialize_canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, ValidationError> {
    serde_json::to_vec(value).map_err(|error| ValidationError::InvalidJson(error.to_string()))
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Decode and validate one bounded v1 response frame against its caller scope.
///
/// # Errors
///
/// Returns an error for malformed data, bounds violations, unsupported
/// versions, or correlation, response-kind, and binding mismatches.
pub fn decode_response(
    bytes: &[u8],
    expected: &ExpectedResponse,
) -> Result<AgentResponse, ValidationError> {
    if bytes.len() > MAX_RESPONSE_FRAME_BYTES {
        return Err(ValidationError::FrameTooLarge);
    }
    let response = serde_json::from_slice::<AgentResponse>(bytes)
        .map_err(|error| ValidationError::InvalidJson(error.to_string()))?;
    validate_response(&response, expected)?;
    Ok(response)
}

/// Validate a decoded response against its invocation, binding, and kind.
///
/// # Errors
///
/// Returns an error for invalid response data or an expected-scope mismatch.
pub fn validate_response(
    response: &AgentResponse,
    expected: &ExpectedResponse,
) -> Result<(), ValidationError> {
    let (version, invocation_id, binding, kind) = response.header();
    validate_version(version)?;
    validate_opaque_id(invocation_id, "invocationId")?;
    validate_binding(binding)?;
    if invocation_id != expected.invocation_id {
        return Err(ValidationError::CorrelationMismatch);
    }
    if binding != &expected.binding {
        return Err(ValidationError::BindingMismatch);
    }
    if expected
        .kind
        .is_some_and(|expected_kind| expected_kind != kind)
    {
        return Err(ValidationError::KindMismatch);
    }
    match response {
        AgentResponse::Capabilities { capabilities, .. } => validate_capabilities(capabilities),
        AgentResponse::List {
            agents,
            next_cursor,
            revision,
            ..
        } => {
            if agents.len() > MAX_AGENT_PAGE_ITEMS {
                return Err(ValidationError::InvalidField("agents"));
            }
            validate_unique(agents.iter().map(|agent| agent.id.as_str()), "agents")?;
            for agent in agents {
                validate_agent(agent)?;
            }
            validate_optional_id(next_cursor.as_deref(), "nextCursor")?;
            validate_revision(revision)
        }
        AgentResponse::Conversation {
            agent_id,
            messages,
            next_cursor,
            revision,
            ..
        } => {
            validate_agent_reference(agent_id, "agentId")?;
            if messages.len() > MAX_CONVERSATION_PAGE_ITEMS {
                return Err(ValidationError::InvalidField("messages"));
            }
            validate_unique(
                messages.iter().map(|message| message.id.as_str()),
                "messages",
            )?;
            for message in messages {
                validate_message(message)?;
            }
            validate_optional_id(next_cursor.as_deref(), "nextCursor")?;
            validate_revision(revision)
        }
        AgentResponse::Operation {
            invocation_id,
            operation,
            ..
        } => validate_operation(operation, invocation_id),
        AgentResponse::Error { error, .. } => validate_error(error),
    }
}

impl AgentResponse {
    fn header(&self) -> (u8, &str, &AgentBinding, AgentResponseKind) {
        match self {
            Self::Capabilities {
                version,
                invocation_id,
                binding,
                ..
            } => (
                *version,
                invocation_id,
                binding,
                AgentResponseKind::Capabilities,
            ),
            Self::List {
                version,
                invocation_id,
                binding,
                ..
            } => (*version, invocation_id, binding, AgentResponseKind::List),
            Self::Conversation {
                version,
                invocation_id,
                binding,
                ..
            } => (
                *version,
                invocation_id,
                binding,
                AgentResponseKind::Conversation,
            ),
            Self::Operation {
                version,
                invocation_id,
                binding,
                ..
            } => (
                *version,
                invocation_id,
                binding,
                AgentResponseKind::Operation,
            ),
            Self::Error {
                version,
                invocation_id,
                binding,
                ..
            } => (*version, invocation_id, binding, AgentResponseKind::Error),
        }
    }
}

fn normalize_request(mut request: AgentRequest) -> Result<AgentRequest, ValidationError> {
    match &mut request {
        AgentRequest::Capabilities { version } => validate_version(*version)?,
        AgentRequest::List { version, cursor } => {
            validate_version(*version)?;
            validate_optional_id(cursor.as_deref(), "cursor")?;
        }
        AgentRequest::Conversation {
            version,
            agent_id,
            cursor,
        } => {
            validate_version(*version)?;
            validate_agent_reference(agent_id, "agent_id")?;
            validate_optional_id(cursor.as_deref(), "cursor")?;
        }
        AgentRequest::Spawn {
            version,
            name,
            task,
            grant_set_id,
        } => {
            validate_version(*version)?;
            let input = SpawnInput {
                name: name.clone(),
                task: task.clone(),
                grant_set_id: grant_set_id.clone(),
            };
            validate_spawn(&input)?;
            *name = trim_ecmascript(name).to_owned();
            *task = trim_ecmascript(task).to_owned();
        }
        AgentRequest::Message {
            version,
            agent_id,
            text,
        } => {
            validate_version(*version)?;
            validate_agent_reference(agent_id, "agent_id")?;
            validate_trimmed_scalar_text(text, MAX_TEXT_SCALARS, "text")?;
            *text = trim_ecmascript(text).to_owned();
        }
        AgentRequest::Status {
            version,
            invocation_id,
        } => {
            validate_version(*version)?;
            validate_opaque_id(invocation_id, "invocation_id")?;
        }
    }
    Ok(request)
}

fn validate_version(version: u8) -> Result<(), ValidationError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ValidationError::UnsupportedVersion)
    }
}

fn validate_binding(binding: &AgentBinding) -> Result<(), ValidationError> {
    validate_opaque_id(&binding.machine, "binding.machine")?;
    validate_opaque_id(&binding.room, "binding.room")?;
    validate_opaque_id(&binding.device, "binding.device")?;
    validate_opaque_id(&binding.host, "binding.host")
}

fn validate_capabilities(capabilities: &AgentCapabilities) -> Result<(), ValidationError> {
    if capabilities.grant_offers.len() > MAX_GRANT_OFFERS {
        return Err(ValidationError::InvalidField("grantOffers"));
    }
    validate_unique(
        capabilities
            .grant_offers
            .iter()
            .map(|offer| offer.id.as_str()),
        "grantOffers",
    )?;
    for offer in &capabilities.grant_offers {
        validate_grant(offer)?;
    }
    validate_unique(
        capabilities
            .unavailable_reasons
            .iter()
            .map(|reason| format!("{reason:?}")),
        "unavailableReasons",
    )
}

/// Validate one exact published resource address without rewriting its identity.
/// Grant count, frame limits and the other opaque-identity bounds remain separate.
pub fn validate_grant_resource(value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > MAX_GRANT_RESOURCE_BYTES
        || !value.is_ascii()
        || value.trim() != value
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ValidationError::InvalidField("grantAction.resource"));
    }
    Ok(())
}

fn validate_grant(offer: &GrantOffer) -> Result<(), ValidationError> {
    validate_opaque_id(&offer.id, "grantOffer.id")?;
    validate_opaque_id(&offer.revision, "grantOffer.revision")?;
    validate_timestamp(&offer.expires_at)?;
    validate_scalar_text(&offer.label, 1, MAX_LABEL_SCALARS, "grantOffer.label")?;
    if offer.actions.is_empty() || offer.actions.len() > MAX_ACTIONS_PER_GRANT {
        return Err(ValidationError::InvalidField("grantOffer.actions"));
    }
    for action in &offer.actions {
        validate_opaque_id(&action.app_id, "grantAction.appId")?;
        validate_opaque_id(&action.record_id, "grantAction.recordId")?;
        validate_grant_resource(&action.resource)?;
        validate_opaque_id(&action.action, "grantAction.action")?;
    }
    Ok(())
}

fn validate_agent(agent: &AgentSnapshot) -> Result<(), ValidationError> {
    validate_agent_reference(&agent.id, "agent.id")?;
    validate_scalar_text(&agent.name, 1, MAX_NAME_SCALARS, "agent.name")?;
    validate_scalar_text(&agent.description, 0, MAX_TEXT_SCALARS, "agent.description")?;
    if agent.conversation.len() > MAX_CONVERSATION_PAGE_ITEMS {
        return Err(ValidationError::InvalidField("agent.conversation"));
    }
    validate_unique(
        agent.conversation.iter().map(|message| message.id.as_str()),
        "agent.conversation",
    )?;
    for message in &agent.conversation {
        validate_message(message)?;
    }
    Ok(())
}

fn validate_message(message: &ChatMessage) -> Result<(), ValidationError> {
    validate_opaque_id(&message.id, "message.id")?;
    validate_scalar_text(&message.text, 1, MAX_TEXT_SCALARS, "message.text")?;
    validate_timestamp(&message.timestamp)
}

fn validate_operation(
    operation: &AgentOperation,
    outer_invocation_id: &str,
) -> Result<(), ValidationError> {
    validate_opaque_id(&operation.invocation_id, "operation.invocationId")?;
    if operation.invocation_id != outer_invocation_id {
        return Err(ValidationError::CorrelationMismatch);
    }
    if let Some(agent) = operation.agent_id.as_deref() {
        validate_agent_reference(agent, "operation.agentId")?;
    }
    validate_optional_id(operation.task_id.as_deref(), "operation.taskId")?;
    match operation.state {
        AgentOperationState::Pending | AgentOperationState::Accepted
            if operation.error.is_some() =>
        {
            return Err(ValidationError::InvalidField("operation.error"));
        }
        AgentOperationState::Failed | AgentOperationState::Uncertain
            if operation.error.is_none() =>
        {
            return Err(ValidationError::InvalidField("operation.error"));
        }
        _ => {}
    }
    if operation.state == AgentOperationState::Accepted
        && (operation.agent_id.is_none() || operation.task_id.is_none())
    {
        return Err(ValidationError::InvalidField("operation.acceptedIds"));
    }
    if let Some(error) = &operation.error {
        validate_operation_diagnostic(error)?;
    }
    Ok(())
}

/// Validate a diagnostic retained for an existing operation using the frozen V1 contract.
///
/// # Errors
/// Rejects an absent-invocation code or a message outside the existing scalar bounds.
pub fn validate_operation_diagnostic(error: &AgentError) -> Result<(), ValidationError> {
    if error.code == AgentErrorCode::UnknownInvocation {
        return Err(ValidationError::InvalidField("operation.error"));
    }
    validate_error(error)
}

fn validate_error(error: &AgentError) -> Result<(), ValidationError> {
    validate_scalar_text(
        &error.message,
        1,
        MAX_ERROR_MESSAGE_SCALARS,
        "error.message",
    )
}

fn validate_timestamp(value: &str) -> Result<(), ValidationError> {
    if value.len() > 64 || !value.is_ascii() {
        return Err(ValidationError::InvalidField("timestamp"));
    }
    let year = decimal_component(value, 0..4).ok_or(ValidationError::InvalidField("timestamp"))?;
    let month = decimal_component(value, 5..7).ok_or(ValidationError::InvalidField("timestamp"))?;
    let day = decimal_component(value, 8..10).ok_or(ValidationError::InvalidField("timestamp"))?;
    let hour =
        decimal_component(value, 11..13).ok_or(ValidationError::InvalidField("timestamp"))?;
    let minute =
        decimal_component(value, 14..16).ok_or(ValidationError::InvalidField("timestamp"))?;
    let second =
        decimal_component(value, 17..19).ok_or(ValidationError::InvalidField("timestamp"))?;
    if value.get(4..5) != Some("-")
        || value.get(7..8) != Some("-")
        || value.get(10..11) != Some("T")
        || value.get(13..14) != Some(":")
        || value.get(16..17) != Some(":")
        || year == 0
        || NaiveDate::from_ymd_opt(
            i32::try_from(year).map_err(|_| ValidationError::InvalidField("timestamp"))?,
            month,
            day,
        )
        .is_none()
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(ValidationError::InvalidField("timestamp"));
    }

    let zone_start = if value.ends_with('Z') {
        value.len().checked_sub(1)
    } else {
        value.len().checked_sub(6)
    }
    .ok_or(ValidationError::InvalidField("timestamp"))?;
    let fraction = value
        .get(19..zone_start)
        .ok_or(ValidationError::InvalidField("timestamp"))?;
    if !fraction.is_empty()
        && (fraction.len() < 2
            || fraction.len() > 10
            || !fraction.starts_with('.')
            || !fraction
                .get(1..)
                .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit())))
    {
        return Err(ValidationError::InvalidField("timestamp"));
    }
    let zone = value
        .get(zone_start..)
        .ok_or(ValidationError::InvalidField("timestamp"))?;
    if zone != "Z" {
        let sign = zone.get(0..1);
        let offset_hour = decimal_component(zone, 1..3);
        let offset_minute = decimal_component(zone, 4..6);
        if !matches!(sign, Some("+" | "-"))
            || zone.get(3..4) != Some(":")
            || offset_hour.is_none_or(|value| value > 23)
            || offset_minute.is_none_or(|value| value > 59)
        {
            return Err(ValidationError::InvalidField("timestamp"));
        }
    }
    Ok(())
}

fn decimal_component(value: &str, range: std::ops::Range<usize>) -> Option<u32> {
    let component = value.get(range)?;
    if !component.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    component.parse().ok()
}

fn validate_revision(value: &str) -> Result<(), ValidationError> {
    parse_revision(value).map(|_| ())
}

fn parse_revision(value: &str) -> Result<u64, ValidationError> {
    if value != "0"
        && (value.starts_with('0')
            || value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(ValidationError::InvalidField("revision"));
    }
    value
        .parse::<u64>()
        .map_err(|_| ValidationError::InvalidField("revision"))
}

fn validate_trimmed_scalar_text(
    value: &str,
    maximum: usize,
    field: &'static str,
) -> Result<(), ValidationError> {
    validate_scalar_text(trim_ecmascript(value), 1, maximum, field)
}

fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(is_ecmascript_whitespace)
}

fn is_ecmascript_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

fn validate_scalar_text(
    value: &str,
    minimum: usize,
    maximum: usize,
    field: &'static str,
) -> Result<(), ValidationError> {
    let count = value.chars().count();
    if count < minimum || count > maximum {
        return Err(ValidationError::InvalidField(field));
    }
    Ok(())
}

fn validate_optional_id(value: Option<&str>, field: &'static str) -> Result<(), ValidationError> {
    value.map_or(Ok(()), |value| validate_opaque_id(value, field))
}

/// Agent references alone may carry an exact historical native UTF-8 principal.
/// This is shape validation, not admission: hosts must resolve existing native
/// identity/grants. New agent IDs and every other opaque ID remain ASCII.
/// ASCII retains the existing v1 contract; non-ASCII uses the old native byte
/// bound and Rust whitespace rule, without normalization or rewriting.
///
/// # Errors
/// Returns `InvalidField` when the reference violates its encoding, byte bound,
/// or historical native whitespace constraints.
pub fn validate_agent_reference(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_ascii() {
        return validate_opaque_id(value, field);
    }
    if value.is_empty() || value.len() > MAX_OPAQUE_ID_BYTES || value.trim() != value {
        return Err(ValidationError::InvalidField(field));
    }
    Ok(())
}

fn validate_opaque_id(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > MAX_OPAQUE_ID_BYTES || !value.is_ascii() {
        return Err(ValidationError::InvalidField(field));
    }
    Ok(())
}

fn validate_unique<I, S>(values: I, field: &'static str) -> Result<(), ValidationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value.as_ref().to_owned()) {
            return Err(ValidationError::InvalidField(field));
        }
    }
    Ok(())
}
