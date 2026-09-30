//! Bounded inert private delivery values. Decoding never authenticates a caller.
#![allow(missing_docs)]
use super::{
    capabilities,
    framing::{self, MessageType},
};
use crate::peer_actions::{ActionMessage, DirectedAction};
use uuid::Uuid;

pub const PRIVATE_EVENTS_TARGET: &str = "/peer/v3/private/events?mode=poll";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateOperation {
    Catalogue,
    Invoke,
    Status,
    Cancel,
}
impl PrivateOperation {
    pub fn path(self) -> &'static str {
        match self {
            Self::Catalogue => "/peer/v3/commands/catalogue",
            Self::Invoke => "/peer/v3/commands/invoke",
            Self::Status => "/peer/v3/commands/status",
            Self::Cancel => "/peer/v3/commands/cancel",
        }
    }
    /// # Errors
    ///
    /// Returns `WireError` if the inputs violate this operation's bounds or invariants.
    pub fn from_target(method: &str, target: &str) -> Result<Self, WireError> {
        [Self::Catalogue, Self::Invoke, Self::Status, Self::Cancel]
            .into_iter()
            .find(|v| method == "POST" && v.path() == target)
            .ok_or(WireError::Direction)
    }
    pub fn request_kind(self) -> MessageType {
        match self {
            Self::Catalogue => MessageType::CatalogueRequest,
            Self::Invoke => MessageType::Invoke,
            Self::Status => MessageType::Status,
            Self::Cancel => MessageType::Cancel,
        }
    }
    pub fn response_kind(self) -> MessageType {
        match self {
            Self::Catalogue => MessageType::CatalogueResponse,
            _ => MessageType::PrivateUpdate,
        }
    }
    pub fn capability(self) -> u64 {
        capabilities::PRIVATE_COMMANDS_V1
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PrivateScope {
    pub room: Uuid,
    pub source: [u8; 32],
    pub target: [u8; 32],
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    #[error("invalid private encoding")]
    Codec,
    #[error("private payload exceeds limit")]
    Oversized,
    #[error("private direction mismatch")]
    Direction,
    #[error("private scope mismatch")]
    Scope,
}
// Postcard envelope preflight borrows the JSON leaf before any action allocation.
#[derive(serde::Deserialize)]
struct BorrowedAction<'a> {
    room: Uuid,
    request_id: Uuid,
    source: [u8; 32],
    target: [u8; 32],
    #[serde(borrow, with = "serde_bytes")]
    message: &'a [u8],
}
fn request_direction(op: PrivateOperation, message: &ActionMessage) -> bool {
    matches!(
        (op, message),
        (
            PrivateOperation::Catalogue,
            ActionMessage::CatalogueRefresh { .. }
        ) | (PrivateOperation::Invoke, ActionMessage::InvokeBound { .. })
            | (PrivateOperation::Status, ActionMessage::Status { .. })
            | (PrivateOperation::Cancel, ActionMessage::Cancel { .. })
    )
}
fn response_direction(op: PrivateOperation, message: &ActionMessage) -> bool {
    matches!(message, ActionMessage::Rejected { .. })
        || (op == PrivateOperation::Cancel
            && matches!(message, ActionMessage::CancellationRequested { .. }))
        || match op {
            PrivateOperation::Catalogue => matches!(message, ActionMessage::BrokerCatalogue { .. }),
            _ => matches!(
                message,
                ActionMessage::Observation { .. } | ActionMessage::CommittedDesktop { .. }
            ),
        }
}
fn check(scope: PrivateScope, action: &DirectedAction) -> Result<(), WireError> {
    if action.room != scope.room
        || action.source != scope.source
        || action.target != scope.target
        || scope.source == [0; 32]
        || scope.target == [0; 32]
        || !action.validate()
    {
        return Err(WireError::Scope);
    }
    Ok(())
}
fn encode(kind: MessageType, action: &DirectedAction) -> Result<Vec<u8>, WireError> {
    let size = postcard::experimental::serialized_size(action).map_err(|_| WireError::Codec)?;
    if size > framing::MAX_PAYLOAD_BYTES {
        return Err(WireError::Oversized);
    }
    let payload = postcard::to_stdvec(action).map_err(|_| WireError::Codec)?;
    let mut output = Vec::new();
    framing::append_frame(
        &mut output,
        kind,
        &payload,
        framing::HEADER_BYTES + framing::MAX_PAYLOAD_BYTES,
    )
    .map_err(|_| WireError::Codec)?;
    Ok(output)
}
fn decode(kind: MessageType, bytes: &[u8]) -> Result<DirectedAction, WireError> {
    if bytes.len() > framing::HEADER_BYTES + framing::MAX_PAYLOAD_BYTES {
        return Err(WireError::Oversized);
    }
    let (actual, payload, tail) =
        framing::decode_frame_prefix(bytes).map_err(|_| WireError::Codec)?;
    if actual != kind {
        return Err(WireError::Direction);
    }
    if !tail.is_empty() {
        return Err(WireError::Codec);
    }
    let (wire, tail): (BorrowedAction<'_>, _) =
        postcard::take_from_bytes(payload).map_err(|_| WireError::Codec)?;
    if !tail.is_empty() || wire.message.len() > framing::MAX_PAYLOAD_BYTES {
        return Err(WireError::Codec);
    }
    // JSON contains no declared allocation counts. This seed additionally checks
    // actual nested element/depth counts before the owned enum/Value is decoded.
    let mut remaining = 64 * 1024;
    let mut decoder = serde_json::Deserializer::from_slice(wire.message);
    serde::de::DeserializeSeed::deserialize(
        JsonBudget {
            depth: 0,
            remaining: &mut remaining,
        },
        &mut decoder,
    )
    .map_err(|_| WireError::Codec)?;
    decoder.end().map_err(|_| WireError::Codec)?;
    let message: ActionMessage =
        serde_json::from_slice(wire.message).map_err(|_| WireError::Codec)?;
    let action = DirectedAction {
        room: wire.room,
        request_id: wire.request_id,
        source: wire.source,
        target: wire.target,
        message,
    };
    if !action.validate() {
        return Err(WireError::Codec);
    }
    // Preserve the existing leaf bytes exactly, rejecting alternative Postcard
    // integers, JSON key order/spacing/duplicate keys and unsupported variants.
    if postcard::to_stdvec(&action).map_err(|_| WireError::Codec)? != payload {
        return Err(WireError::Codec);
    }
    Ok(action)
}
struct JsonBudget<'a> {
    depth: usize,
    remaining: &'a mut usize,
}
impl<'de> serde::de::DeserializeSeed<'de> for JsonBudget<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if self.depth > 32 || *self.remaining == 0 {
            return Err(serde::de::Error::custom("private JSON budget"));
        }
        *self.remaining = (*self.remaining).saturating_sub(1);
        d.deserialize_any(self)
    }
}
impl<'de> serde::de::Visitor<'de> for JsonBudget<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded private JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a
            .next_element_seed(JsonBudget {
                depth: self.depth.saturating_add(1),
                remaining: self.remaining,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a
            .next_key_seed(JsonBudget {
                depth: self.depth.saturating_add(1),
                remaining: self.remaining,
            })?
            .is_some()
        {
            a.next_value_seed(JsonBudget {
                depth: self.depth.saturating_add(1),
                remaining: self.remaining,
            })?;
        }
        Ok(())
    }
}
/// # Errors
///
/// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
pub fn encode_request(
    op: PrivateOperation,
    scope: PrivateScope,
    action: &DirectedAction,
) -> Result<Vec<u8>, WireError> {
    check(scope, action)?;
    if !request_direction(op, &action.message) {
        return Err(WireError::Direction);
    }
    encode(op.request_kind(), action)
}
/// # Errors
///
/// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
pub fn decode_request(
    op: PrivateOperation,
    scope: PrivateScope,
    bytes: &[u8],
) -> Result<DirectedAction, WireError> {
    let action = decode(op.request_kind(), bytes)?;
    check(scope, &action)?;
    if !request_direction(op, &action.message) {
        return Err(WireError::Direction);
    }
    Ok(action)
}
fn response_scope(request: &DirectedAction) -> PrivateScope {
    PrivateScope {
        room: request.room,
        source: request.target,
        target: request.source,
    }
}
fn response_check(
    op: PrivateOperation,
    request: &DirectedAction,
    action: &DirectedAction,
) -> Result<(), WireError> {
    check(response_scope(request), action)?;
    if !request_direction(op, &request.message) || !response_direction(op, &action.message) {
        return Err(WireError::Direction);
    }
    if action.request_id != request.request_id
        || (request.message.invocation().is_some()
            && !matches!(action.message, ActionMessage::Rejected { .. })
            && action.message.invocation() != request.message.invocation())
    {
        return Err(WireError::Scope);
    }
    if let ActionMessage::Observation {
        host,
        invocation,
        update,
        ..
    } = &action.message
        && (update
            .as_ref()
            .is_some_and(|value| value.invocation() != *invocation)
            || matches!(&request.message, ActionMessage::InvokeBound { host: expected, .. } if expected != host))
    {
        return Err(WireError::Scope);
    }
    Ok(())
}
/// # Errors
///
/// Returns `WireError` if the value violates its wire bounds or cannot be serialized.
pub fn encode_response(
    op: PrivateOperation,
    request: &DirectedAction,
    action: &DirectedAction,
) -> Result<Vec<u8>, WireError> {
    response_check(op, request, action)?;
    encode(op.response_kind(), action)
}
/// # Errors
///
/// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
pub fn decode_response(
    op: PrivateOperation,
    request: &DirectedAction,
    bytes: &[u8],
) -> Result<DirectedAction, WireError> {
    let action = decode(op.response_kind(), bytes)?;
    response_check(op, request, &action)?;
    Ok(action)
}
/// Decode one inert private event. The consumer must still match the retained
/// request ID, invocation and operation; decoding never authenticates a sender.
///
/// # Errors
///
/// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
pub fn decode_event(
    scope: PrivateScope,
    host: Uuid,
    bytes: &[u8],
) -> Result<DirectedAction, WireError> {
    if host.is_nil() {
        return Err(WireError::Scope);
    }
    let action = decode(MessageType::PrivateUpdate, bytes)?;
    check(scope, &action)?;
    match &action.message {
        ActionMessage::Observation {
            host: actual,
            invocation,
            update,
            ..
        } => {
            if *actual != host
                || update
                    .as_ref()
                    .is_some_and(|value| value.invocation() != *invocation)
            {
                return Err(WireError::Scope);
            }
        }
        ActionMessage::Rejected { .. } => {}
        _ => return Err(WireError::Direction),
    }
    Ok(action)
}

#[cfg(test)]
mod tests;
