//! Private live action vocabulary. These are inert transport values: only a
//! certified native broker with durable admission may execute an invocation.
#![allow(missing_docs)]

use qos_actions::{InvocationId, InvocationUpdate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PRIVATE_BYTES: usize = 256 * 1024;
pub const MAX_IDENTIFIER_BYTES: usize = 512;
pub const MAX_CONFIRMATION_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirectedAction {
    pub room: Uuid,
    pub request_id: Uuid,
    pub source: [u8; 32],
    pub target: [u8; 32],
    // Existing updates and catalogue contain serde_json::Value and tagged
    // enums. Encode this leaf as bounded JSON bytes, not postcard's
    // unsupported deserialize_any; the enclosing routing ABI stays binary.
    #[serde(with = "message_json")]
    pub message: ActionMessage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionMessage {
    CatalogueRefresh,
    BrokerCatalogue {
        host: Uuid,
        catalogue: serde_json::Value,
    },
    InvokeBound {
        host: Uuid,
        invocation: InvocationId,
        app_peer: String,
        app_id: String,
        record_id: String,
        resource: String,
        action: String,
        argument: Option<serde_json::Value>,
        confirmation: Option<Confirmation>,
        revision: Option<String>,
    },
    Observation {
        host: Uuid,
        invocation: InvocationId,
        update: Option<InvocationUpdate>,
        uncertain: bool,
        cancellation_requested: bool,
    },
    Rejected {
        code: String,
    },
    /// Existing broker JSON {version:1,surfaces:[...]}, without translation.
    Catalogue {
        catalogue: serde_json::Value,
    },
    Invoke {
        invocation: InvocationId,
        app_peer: String,
        resource: String,
        action: String,
        argument: Option<serde_json::Value>,
        confirmation: Option<Confirmation>,
        revision: Option<String>,
    },
    Status {
        invocation: InvocationId,
    },
    Update {
        update: InvocationUpdate,
    },
    Cancel {
        invocation: InvocationId,
    },
    /// The cancellation request was recorded, not proof of cancellation.
    CancellationRequested {
        invocation: InvocationId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "phrase", rename_all = "snake_case")]
pub enum Confirmation {
    Confirm,
    Phrase(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableReason {
    Offline,
    UnsupportedPeer,
    Backpressure,
    NotAuthorised,
    InvalidRequest,
    /// Native durable admission has not been installed. Never a terminal result.
    AdmissionUnavailable,
}

impl ActionMessage {
    pub fn invocation(&self) -> Option<InvocationId> {
        match self {
            Self::InvokeBound { invocation, .. }
            | Self::Observation { invocation, .. }
            | Self::Invoke { invocation, .. }
            | Self::Status { invocation }
            | Self::Cancel { invocation }
            | Self::CancellationRequested { invocation } => Some(*invocation),
            Self::Update { update } => Some(update.invocation()),
            Self::CatalogueRefresh
            | Self::Catalogue { .. }
            | Self::BrokerCatalogue { .. }
            | Self::Rejected { .. } => None,
        }
    }

    pub fn validate(&self) -> bool {
        let Ok(json) = serde_json::to_value(self) else {
            return false;
        };
        if !bounded_json(&json, 0, &mut 4096) {
            return false;
        }
        let identifier = |value: &str| !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES;
        match self {
            Self::InvokeBound {
                host,
                invocation,
                app_peer,
                app_id,
                record_id,
                resource,
                action,
                confirmation,
                revision,
                ..
            } => {
                !host.is_nil()
                    && !invocation.0.is_nil()
                    && [app_peer, app_id, resource, action]
                        .iter()
                        .all(|s| identifier(s))
                    && record_id.len() <= MAX_IDENTIFIER_BYTES
                    && revision.as_ref().is_none_or(|v| identifier(v))
                    && !matches!(confirmation, Some(Confirmation::Phrase(v)) if v.len() > MAX_CONFIRMATION_BYTES)
            }
            Self::Invoke {
                invocation,
                app_peer,
                resource,
                action,
                confirmation,
                revision,
                ..
            } => {
                !invocation.0.is_nil()
                    && identifier(app_peer)
                    && identifier(resource)
                    && identifier(action)
                    && revision.as_ref().is_none_or(|v| identifier(v))
                    && !matches!(confirmation, Some(Confirmation::Phrase(v)) if v.len() > MAX_CONFIRMATION_BYTES)
            }
            Self::Catalogue { catalogue } => {
                catalogue.get("version").and_then(serde_json::Value::as_u64) == Some(1)
                    && catalogue
                        .get("surfaces")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|v| v.len() <= 256)
            }
            Self::Update { update } => {
                !update.invocation().0.is_nil()
                    && match update {
                        InvocationUpdate::Progress { phase, message, .. } => {
                            identifier(phase)
                                && message
                                    .as_ref()
                                    .is_none_or(|v| v.len() <= MAX_CONFIRMATION_BYTES)
                        }
                        InvocationUpdate::Succeeded { output, .. } => {
                            output.resources.len() <= 256
                                && output.resources.iter().all(|v| identifier(v))
                        }
                        InvocationUpdate::Failed { error, .. } => {
                            identifier(&error.code) && error.message.len() <= MAX_CONFIRMATION_BYTES
                        }
                        InvocationUpdate::Cancelled { message, .. } => message
                            .as_ref()
                            .is_none_or(|v| v.len() <= MAX_CONFIRMATION_BYTES),
                        _ => true,
                    }
            }
            _ => self.invocation().is_none_or(|id| !id.0.is_nil()),
        }
    }
}

fn bounded_json(value: &serde_json::Value, depth: usize, remaining: &mut usize) -> bool {
    if depth > 32 || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| bounded_json(value, depth + 1, remaining)),
        serde_json::Value::Object(values) => values
            .values()
            .all(|value| bounded_json(value, depth + 1, remaining)),
        _ => true,
    }
}

impl DirectedAction {
    pub fn validate(&self) -> bool {
        !self.room.is_nil()
            && !self.request_id.is_nil()
            && self.message.validate()
            && serde_json::to_vec(&self.message).is_ok_and(|bytes| bytes.len() <= MAX_PRIVATE_BYTES)
    }
}

mod message_json {
    use super::ActionMessage;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        message: &ActionMessage,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let bytes = serde_json::to_vec(message).map_err(serde::ser::Error::custom)?;
        serializer.serialize_bytes(&bytes)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ActionMessage, D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(deserializer)?;
        if bytes.len() > super::MAX_PRIVATE_BYTES {
            return Err(serde::de::Error::custom("private payload too large"));
        }
        serde_json::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}
