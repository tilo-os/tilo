//! Private live action vocabulary. These are inert transport values: only a
//! certified native broker with durable admission may execute an invocation.
#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use tilo_actions::{InvocationId, InvocationUpdate};
use uuid::Uuid;

mod signed;
pub use signed::{
    ACTION_SIGNATURE_DOMAIN, ACTION_SIGNATURE_TOLERANCE_NS, ActionReplayWindow,
    ActionSignatureError, SignedDirectedAction,
};

pub const MAX_PRIVATE_BYTES: usize = 256 * 1024;
// Catalogues aggregate descriptors for every record. Their structural budget
// must be larger than a single effect's argument while remaining bounded by
// the same wire-size and nesting limits.
const MAX_CATALOGUE_NODES: usize = 32 * 1024;
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
    /// An admitted person's exact bounded redacted terminal audit. The
    /// governor rechecks the certified transport caller and commits durably.
    RecordEffectReceipt {
        invocation: InvocationId,
        redacted: String,
    },
    /// Governor commit completed; the caller still observes signed room state.
    EffectReceiptCommitted {
        invocation: InvocationId,
    },
    CatalogueRefresh {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ExactActionRoute>,
    },
    /// Best-effort immediate copy of a durably committed desktop publication.
    /// Receivers must pin the source authority and use ordinary state admission.
    CommittedDesktop {
        invocation: InvocationId,
        packets: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opened: Option<OpenedDesktopTab>,
    },
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

/// Optional exact discovery narrowing. The broker still performs ordinary
/// caller authorization and invocation binding for the selected route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactActionRoute {
    pub app_id: String,
    pub record_id: String,
    pub resource: String,
    pub action: String,
}

/// Exact native IDs, represented as decimal strings across the JS boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenedDesktopTab {
    pub strip: String,
    pub group: String,
    pub tab: String,
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

impl UnavailableReason {
    /// Every reason, for exhaustive mappings and their tests.
    pub const ALL: [Self; 6] = [
        Self::Offline,
        Self::UnsupportedPeer,
        Self::Backpressure,
        Self::NotAuthorised,
        Self::InvalidRequest,
        Self::AdmissionUnavailable,
    ];

    /// Stable token that leads the free-form `reason` of a v1
    /// `RemoteActionUndeliverable`, so a client can branch on it without
    /// the frozen v1 envelope changing shape. The relay writes
    /// `"<token>"` or `"<token>: <detail>"`.
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::UnsupportedPeer => "unsupported-peer",
            Self::Backpressure => "backpressure",
            Self::NotAuthorised => "not-authorised",
            Self::InvalidRequest => "invalid-request",
            Self::AdmissionUnavailable => "admission-unavailable",
        }
    }

    /// The reason whose [`wire_token`](Self::wire_token) is exactly `token`.
    pub fn from_wire_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.wire_token() == token)
    }

    /// The reason a v1 `RemoteActionUndeliverable.reason` leads with
    /// (`"<token>"` or `"<token>: <detail>"`); `None` for a reason written
    /// by a relay that predates the tokens.
    pub fn from_wire_reason(reason: &str) -> Option<Self> {
        let token = reason.split_once(':').map_or(reason, |(token, _)| token);
        Self::from_wire_token(token.trim())
    }
}

impl ActionMessage {
    /// Whether a target may only serve this message as a request its source
    /// device signed ([`SignedDirectedAction`]). Only effect-free reads are
    /// exempt; every other message, including any added later, requires it.
    pub const fn requires_signature(&self) -> bool {
        !matches!(self, Self::CatalogueRefresh { .. } | Self::Status { .. })
    }

    pub fn invocation(&self) -> Option<InvocationId> {
        match self {
            Self::RecordEffectReceipt { invocation, .. }
            | Self::EffectReceiptCommitted { invocation }
            | Self::CommittedDesktop { invocation, .. }
            | Self::InvokeBound { invocation, .. }
            | Self::Observation { invocation, .. }
            | Self::Status { invocation }
            | Self::Cancel { invocation }
            | Self::CancellationRequested { invocation } => Some(*invocation),
            Self::Update { update } => Some(update.invocation()),
            Self::CatalogueRefresh { .. }
            | Self::Catalogue { .. }
            | Self::BrokerCatalogue { .. }
            | Self::Rejected { .. } => None,
        }
    }

    pub fn validate(&self) -> bool {
        let Ok(json) = serde_json::to_value(self) else {
            return false;
        };
        let mut remaining = if matches!(self, Self::Catalogue { .. } | Self::BrokerCatalogue { .. })
        {
            MAX_CATALOGUE_NODES
        } else {
            4096
        };
        if !bounded_json(&json, 0, &mut remaining) {
            return false;
        }
        let identifier = |value: &str| !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES;
        match self {
            Self::RecordEffectReceipt {
                invocation,
                redacted,
            } => !invocation.0.is_nil() && !redacted.is_empty() && redacted.len() <= 16 * 1024,
            Self::CommittedDesktop {
                invocation,
                packets,
                opened,
            } => {
                !invocation.0.is_nil()
                    && !packets.is_empty()
                    && packets.len() <= 64
                    && packets.iter().all(|p| {
                        let p = p.strip_prefix("z:").unwrap_or(p);
                        !p.is_empty()
                            && p.len() % 2 == 0
                            && p.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    && opened.as_ref().is_none_or(|route| {
                        [&route.strip, &route.group, &route.tab]
                            .iter()
                            .all(|id| id.parse::<u64>().is_ok_and(|id| id != 0))
                    })
            }
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
            Self::Catalogue { catalogue } | Self::BrokerCatalogue { catalogue, .. } => {
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
            Self::CatalogueRefresh { scope } => scope.as_ref().is_none_or(|scope| {
                identifier(&scope.app_id)
                    && scope.record_id.len() <= MAX_IDENTIFIER_BYTES
                    && identifier(&scope.resource)
                    && identifier(&scope.action)
            }),
            _ => self.invocation().is_none_or(|id| !id.0.is_nil()),
        }
    }
}

fn bounded_json(value: &serde_json::Value, depth: usize, remaining: &mut usize) -> bool {
    if depth > 32 || *remaining == 0 {
        return false;
    }
    *remaining = (*remaining).saturating_sub(1);
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| bounded_json(value, depth.saturating_add(1), remaining)),
        serde_json::Value::Object(values) => values
            .values()
            .all(|value| bounded_json(value, depth.saturating_add(1), remaining)),
        _ => true,
    }
}

// Private leaves are accepted only when their JSON re-encodes byte for byte,
// and catalogues carry `serde_json::Value` maps: without `preserve_order` a
// target sorts their keys and refuses every catalogue a native host sends.
// `shift_remove` exists only with that feature, so its absence fails the build.
const _: fn(&mut serde_json::Map<String, serde_json::Value>, &str) -> Option<serde_json::Value> =
    |map, key| map.shift_remove(key);

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

#[cfg(test)]
mod catalogue_bounds_tests {
    use super::*;

    #[test]
    fn scoped_catalogue_refresh_remains_compatible_with_frozen_unit_reader() {
        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum FrozenActionMessage {
            CatalogueRefresh,
        }
        let scoped = ActionMessage::CatalogueRefresh {
            scope: Some(ExactActionRoute {
                app_id: "so.tilo.desktop".into(),
                record_id: String::new(),
                resource: "desktop:session".into(),
                action: "command".into(),
            }),
        };
        let encoded = serde_json::to_vec(&scoped).unwrap();
        let frozen: FrozenActionMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(frozen, FrozenActionMessage::CatalogueRefresh);
        assert_eq!(
            serde_json::to_vec(&frozen).unwrap(),
            br#"{"type":"catalogue_refresh"}"#
        );
        assert_eq!(
            serde_json::to_vec(&ActionMessage::CatalogueRefresh { scope: None }).unwrap(),
            br#"{"type":"catalogue_refresh"}"#
        );
        assert_eq!(
            serde_json::from_slice::<ActionMessage>(&encoded).unwrap(),
            scoped
        );
    }

    #[test]
    fn member_terminal_receipt_wire_is_bounded_and_keeps_original_invocation() {
        let invocation = InvocationId::new();
        let message = ActionMessage::RecordEffectReceipt {
            invocation,
            redacted: "{}".into(),
        };
        assert!(message.validate());
        assert_eq!(message.invocation(), Some(invocation));
        assert_eq!(
            serde_json::from_slice::<ActionMessage>(&serde_json::to_vec(&message).unwrap())
                .unwrap(),
            message
        );
        for redacted in [String::new(), "x".repeat(16 * 1024 + 1)] {
            assert!(
                !ActionMessage::RecordEffectReceipt {
                    invocation,
                    redacted
                }
                .validate()
            );
        }
        assert!(
            !ActionMessage::EffectReceiptCommitted {
                invocation: InvocationId(Uuid::nil())
            }
            .validate()
        );
    }

    fn catalogue(nodes: usize) -> serde_json::Value {
        serde_json::json!({"version": 1, "surfaces": [{"actions": vec![serde_json::json!({"resource": "document", "action": "edit", "params": {}}); nodes]}]})
    }

    #[test]
    fn catalogue_has_a_separate_node_budget_from_effect_arguments() {
        let catalogue = catalogue(1500);
        for message in [
            ActionMessage::Catalogue {
                catalogue: catalogue.clone(),
            },
            ActionMessage::BrokerCatalogue {
                host: Uuid::new_v4(),
                catalogue: catalogue.clone(),
            },
        ] {
            let directed = DirectedAction {
                room: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                source: [1; 32],
                target: [2; 32],
                message,
            };
            assert!(
                directed.validate(),
                "a bounded catalogue with over 4096 nodes must remain discoverable"
            );
            assert!(
                crate::transport_v2::encode(&crate::transport_v2::EnvelopeV2::Directed(directed))
                    .is_ok()
            );
        }
        let invoke = ActionMessage::InvokeBound {
            host: Uuid::new_v4(),
            invocation: tilo_actions::InvocationId::new(),
            app_peer: "1".into(),
            app_id: "app".into(),
            record_id: String::new(),
            resource: "document".into(),
            action: "edit".into(),
            argument: Some(catalogue),
            confirmation: None,
            revision: None,
        };
        assert!(
            !invoke.validate(),
            "effect arguments retain the original node bound"
        );
    }

    #[test]
    fn catalogue_still_rejects_excessive_nodes_depth_surfaces_and_bytes() {
        for catalogue in [
            catalogue(9000),
            serde_json::json!({"version":1,"surfaces": vec![serde_json::json!({});257]}),
            {
                let mut deep = serde_json::Value::Null;
                for _ in 0..33 {
                    deep = serde_json::json!([deep]);
                }
                serde_json::json!({"version":1,"surfaces":[],"nested":deep})
            },
        ] {
            for message in [
                ActionMessage::Catalogue {
                    catalogue: catalogue.clone(),
                },
                ActionMessage::BrokerCatalogue {
                    host: Uuid::new_v4(),
                    catalogue,
                },
            ] {
                assert!(!message.validate());
            }
        }
        let action = DirectedAction {
            room: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::BrokerCatalogue {
                host: Uuid::new_v4(),
                catalogue: serde_json::json!({"version":1,"surfaces":[],"text":"x".repeat(MAX_PRIVATE_BYTES)}),
            },
        };
        assert!(!action.validate());
        assert!(
            crate::transport_v2::encode(&crate::transport_v2::EnvelopeV2::Directed(action))
                .is_err()
        );
    }
}

#[cfg(test)]
mod wire_token_tests {
    use super::UnavailableReason;

    #[test]
    fn wire_token_round_trips_every_reason() {
        let mut seen = std::collections::HashSet::new();
        for reason in UnavailableReason::ALL {
            let token = reason.wire_token();
            assert!(seen.insert(token), "{token} names one reason only");
            assert_eq!(UnavailableReason::from_wire_token(token), Some(reason));
            assert_eq!(UnavailableReason::from_wire_reason(token), Some(reason));
            assert_eq!(
                UnavailableReason::from_wire_reason(&format!("{token}: some detail")),
                Some(reason)
            );
        }
        assert_eq!(
            UnavailableReason::from_wire_reason("this host does not serve apps"),
            None,
            "a pre-token reason is not guessed"
        );
        assert_eq!(
            UnavailableReason::from_wire_token("not-authorised: x"),
            None
        );
    }
}
