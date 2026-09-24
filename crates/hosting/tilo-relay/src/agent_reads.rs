//! Data and validation for disposable certified agent reads. These DTOs confer no authority.
use qos_agent_control_protocol::{
    AgentBinding, AgentRequest, AgentResponse, AgentResponseKind, ExpectedResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bound for the complete JSON result inside the existing 256 KiB private frame.
pub const MAX_RESULT_BYTES: usize = 240 * 1024;

/// Exact route copied from the native provider's current catalogue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMutationRoute {
    /// Decimal provider peer ID.
    pub app_peer: String,
    /// Native application identifier.
    pub app_id: String,
    /// Exact record, including an empty workspace record.
    pub record_id: String,
    /// Native resource identifier.
    pub resource: String,
    /// Native action identifier.
    pub action: String,
    /// Current provider revision.
    pub revision: String,
}
/// Route and descriptor from the same native publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMutationBinding {
    /// Exact route, never a browser-derived substitute.
    pub route: AgentMutationRoute,
    /// Original catalogue descriptor.
    pub descriptor: serde_json::Value,
}
/// Native read publication. The browser adds its independently verified action context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentReadBinding {
    /// Registered native action host.
    pub action_host: String,
    /// Native provider/caller scope.
    pub agent: AgentBinding,
    /// Available spawn route.
    pub spawn: Option<AgentMutationBinding>,
    /// Available message route.
    pub message: Option<AgentMutationBinding>,
}

/// Return the canonical response kind only for the three disposable reads.
pub fn request_kind(request: &AgentRequest) -> Option<AgentResponseKind> {
    match request {
        AgentRequest::Capabilities { .. } => Some(AgentResponseKind::Capabilities),
        AgentRequest::List { .. } => Some(AgentResponseKind::List),
        AgentRequest::Conversation { .. } => Some(AgentResponseKind::Conversation),
        _ => None,
    }
}
/// Canonical validation does not itself exclude mutations; the read carrier does.
pub fn valid_request(request: &AgentRequest) -> bool {
    request_kind(request).is_some()
        && qos_agent_control_protocol::canonical_request(request).is_ok()
}

/// Check the ordinary publication data's shape and its native host binding.
pub fn valid_binding(binding: &AgentReadBinding, host: Uuid) -> bool {
    let identifier = |value: &str| !value.is_empty() && value.len() <= 512 && value.is_ascii();
    !host.is_nil()
        && binding.action_host == host.to_string()
        && binding.agent.host == binding.action_host
        && Uuid::parse_str(&binding.agent.room)
            .is_ok_and(|room| !room.is_nil() && room.to_string() == binding.agent.room)
        && binding.agent.machine.len() <= 128
        && identifier(&binding.agent.machine)
        && binding.agent.device.len() == 64
        && binding
            .agent
            .device
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && [&binding.spawn, &binding.message]
            .into_iter()
            .flatten()
            .all(|value| {
                let route = &value.route;
                route
                    .app_peer
                    .parse::<u64>()
                    .is_ok_and(|peer| peer.to_string() == route.app_peer)
                    && [
                        &route.app_id,
                        &route.resource,
                        &route.action,
                        &route.revision,
                    ]
                    .into_iter()
                    .all(|value| identifier(value))
                    && route.record_id.len() <= 512
                    && value.descriptor.is_object()
                    && value
                        .descriptor
                        .get("resource")
                        .and_then(|value| value.as_str())
                        == Some(route.resource.as_str())
                    && value
                        .descriptor
                        .get("action")
                        .and_then(|value| value.as_str())
                        == Some(route.action.as_str())
                    && value
                        .descriptor
                        .get("revision")
                        .and_then(|value| value.as_str())
                        == Some(route.revision.as_str())
            })
}

/// Validate a result's canonical body without trusting its claimed request kind.
pub fn valid_result(
    host: Uuid,
    invocation: Uuid,
    binding: &AgentReadBinding,
    response: &AgentResponse,
) -> bool {
    if invocation.is_nil() || !valid_binding(binding, host) {
        return false;
    }
    let kind = match response {
        AgentResponse::Capabilities { .. } => AgentResponseKind::Capabilities,
        AgentResponse::List { .. } => AgentResponseKind::List,
        AgentResponse::Conversation { .. } => AgentResponseKind::Conversation,
        AgentResponse::Error { .. } => AgentResponseKind::Error,
        AgentResponse::Operation { .. } => return false,
    };
    qos_agent_control_protocol::validate_response(
        response,
        &ExpectedResponse {
            invocation_id: invocation.to_string(),
            binding: binding.agent.clone(),
            kind: Some(kind),
        },
    )
    .is_ok()
}

/// Correlate the canonical result to the original read, including conversation ID.
pub fn matches_request(request: &AgentRequest, response: &AgentResponse) -> bool {
    valid_request(request)
        && match (request, response) {
            (_, AgentResponse::Error { .. }) => true,
            (AgentRequest::Capabilities { .. }, AgentResponse::Capabilities { .. }) => true,
            (AgentRequest::List { .. }, AgentResponse::List { .. }) => true,
            (
                AgentRequest::Conversation {
                    agent_id: expected, ..
                },
                AgentResponse::Conversation { agent_id, .. },
            ) => expected == agent_id,
            _ => false,
        }
}
