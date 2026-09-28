//! Private live action vocabulary. These are inert transport values: only a
//! certified native broker with durable admission may execute an invocation.
#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use tilo_actions::{InvocationId, InvocationUpdate};
use uuid::Uuid;

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
#[expect(
    clippy::large_enum_variant,
    reason = "public serde wire enum; boxing would change every constructor and match in dependent crates"
)]
pub enum ActionMessage {
    /// Disposable canonical agent read; never a durable business invocation.
    AgentReadV1 {
        host: Uuid,
        invocation: Uuid,
        request: tilo_agent_control_protocol::AgentRequest,
    },
    /// Certified provider result, bound to the original disposable read.
    AgentReadResultV1 {
        host: Uuid,
        invocation: Uuid,
        binding: crate::agent_reads::AgentReadBinding,
        response: tilo_agent_control_protocol::AgentResponse,
    },
    CatalogueRefresh {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<ExactActionRoute>,
        /// Where the previous page of this catalogue ended; absent for the
        /// first page. See [`serve_catalogue_page`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<CatalogueCursor>,
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
        /// Present while more of the catalogue remains: ask again with it
        /// as `after` until a page arrives without one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<CatalogueCursor>,
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

/// Where a paged catalogue continues: which catalogue it is a page of (the
/// digest of every surface in it) and the first surface of the next page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueCursor {
    pub revision: String,
    pub surface: u32,
}

/// The refusal a caller gets when the catalogue it was paging through
/// changed under it: it starts again from the first page.
pub const CATALOGUE_MOVED: &str = "catalogue_moved";

/// What a page may occupy of a private message, leaving room for the
/// message around it (its type, host and cursor).
const CATALOGUE_PAGE_BYTES: usize = MAX_PRIVATE_BYTES - 1024;
const CATALOGUE_PAGE_NODES: usize = MAX_CATALOGUE_NODES - 64;
const CATALOGUE_PAGE_SURFACES: usize = 256;

/// Serve the page of a whole catalogue `reply` that `request` asks for.
///
/// A room's catalogue can outgrow one private message, so every answer to a
/// catalogue refresh is a page: whole surfaces, in one stable order, filling
/// the message, with a cursor to the rest pinned to the catalogue they come
/// from. Every surface appears whole on exactly one page; a caller asking
/// after a catalogue that has since changed is told [`CATALOGUE_MOVED`] and
/// starts again. Anything but a whole catalogue answering a refresh passes
/// through unchanged.
#[must_use]
pub fn serve_catalogue_page(request: &ActionMessage, reply: ActionMessage) -> ActionMessage {
    let ActionMessage::CatalogueRefresh { after, .. } = request else {
        return reply;
    };
    let ActionMessage::BrokerCatalogue {
        host,
        catalogue,
        next: None,
    } = reply
    else {
        return reply;
    };
    match catalogue_page(catalogue, after.as_ref()) {
        Ok((catalogue, next)) => ActionMessage::BrokerCatalogue {
            host,
            catalogue,
            next,
        },
        Err(code) => ActionMessage::Rejected { code: code.into() },
    }
}

fn catalogue_page(
    catalogue: serde_json::Value,
    after: Option<&CatalogueCursor>,
) -> Result<(serde_json::Value, Option<CatalogueCursor>), &'static str> {
    let serde_json::Value::Object(mut fields) = catalogue else {
        return Err("invalid_catalogue");
    };
    let Some(serde_json::Value::Array(mut surfaces)) = fields.shift_remove("surfaces") else {
        return Err("invalid_catalogue");
    };
    // One order for every page, whatever order the surfaces were gathered in.
    surfaces.sort_by_cached_key(|surface| serde_json::to_vec(surface).unwrap_or_default());
    let mut digest = blake3::Hasher::new();
    for surface in &surfaces {
        digest.update(&serde_json::to_vec(surface).map_err(|_| "invalid_catalogue")?);
        digest.update(b"\n");
    }
    let revision = digest.finalize().to_hex().to_string();
    let start = match after {
        None => 0,
        Some(cursor) if cursor.revision == revision => {
            usize::try_from(cursor.surface).map_err(|_| CATALOGUE_MOVED)?
        }
        Some(_) => return Err(CATALOGUE_MOVED),
    };
    if start > surfaces.len() || (start == surfaces.len() && start != 0) {
        return Err(CATALOGUE_MOVED);
    }
    fields.insert("surfaces".into(), serde_json::Value::Array(Vec::new()));
    let base = serde_json::Value::Object(fields.clone());
    let mut bytes = serde_json::to_vec(&base)
        .map_err(|_| "invalid_catalogue")?
        .len();
    let mut nodes = json_nodes(&base);
    let mut page = Vec::new();
    for surface in surfaces.iter().skip(start) {
        let size = serde_json::to_vec(surface)
            .map_err(|_| "invalid_catalogue")?
            .len()
            .saturating_add(1);
        let count = json_nodes(surface);
        let fits = bytes.saturating_add(size) <= CATALOGUE_PAGE_BYTES
            && nodes.saturating_add(count) <= CATALOGUE_PAGE_NODES
            && page.len() < CATALOGUE_PAGE_SURFACES;
        // A page always carries at least one surface; one too large for any
        // page is then refused by the message bounds, never split.
        if !fits && !page.is_empty() {
            break;
        }
        bytes = bytes.saturating_add(size);
        nodes = nodes.saturating_add(count);
        page.push(surface.clone());
    }
    let end = start.saturating_add(page.len());
    let next = (end < surfaces.len())
        .then(|| {
            u32::try_from(end).map(|surface| CatalogueCursor {
                revision: revision.clone(),
                surface,
            })
        })
        .transpose()
        .map_err(|_| "invalid_catalogue")?;
    fields.insert("surfaces".into(), serde_json::Value::Array(page));
    Ok((serde_json::Value::Object(fields), next))
}

fn json_nodes(value: &serde_json::Value) -> usize {
    1usize.saturating_add(match value {
        serde_json::Value::Array(values) => values.iter().map(json_nodes).sum(),
        serde_json::Value::Object(values) => values.values().map(json_nodes).sum(),
        _ => 0,
    })
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

impl ActionMessage {
    pub fn invocation(&self) -> Option<InvocationId> {
        match self {
            Self::CommittedDesktop { invocation, .. }
            | Self::InvokeBound { invocation, .. }
            | Self::Observation { invocation, .. }
            | Self::Status { invocation }
            | Self::Cancel { invocation }
            | Self::CancellationRequested { invocation } => Some(*invocation),
            Self::Update { update } => Some(update.invocation()),
            Self::CatalogueRefresh { .. }
            | Self::AgentReadV1 { .. }
            | Self::AgentReadResultV1 { .. }
            | Self::Catalogue { .. }
            | Self::BrokerCatalogue { .. }
            | Self::Rejected { .. } => None,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    pub fn validate(&self) -> bool {
        let Ok(json) = serde_json::to_value(self) else {
            return false;
        };
        let mut remaining = if matches!(
            self,
            Self::Catalogue { .. } | Self::BrokerCatalogue { .. } | Self::AgentReadResultV1 { .. }
        ) {
            MAX_CATALOGUE_NODES
        } else {
            4096
        };
        if !bounded_json(&json, 0, &mut remaining) {
            return false;
        }
        let identifier = |value: &str| !value.is_empty() && value.len() <= MAX_IDENTIFIER_BYTES;
        match self {
            Self::AgentReadV1 {
                host,
                invocation,
                request,
            } => {
                !host.is_nil() && !invocation.is_nil() && crate::agent_reads::valid_request(request)
            }
            Self::AgentReadResultV1 {
                host,
                invocation,
                binding,
                response,
            } => {
                crate::agent_reads::valid_result(*host, *invocation, binding, response)
                    && serde_json::to_vec(self)
                        .is_ok_and(|bytes| bytes.len() <= crate::agent_reads::MAX_RESULT_BYTES)
            }
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
            Self::BrokerCatalogue {
                next: Some(next), ..
            } if !next.is_well_formed() => false,
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
            Self::CatalogueRefresh { scope, after } => {
                scope.as_ref().is_none_or(|scope| {
                    identifier(&scope.app_id)
                        && scope.record_id.len() <= MAX_IDENTIFIER_BYTES
                        && identifier(&scope.resource)
                        && identifier(&scope.action)
                }) && after.as_ref().is_none_or(CatalogueCursor::is_well_formed)
            }
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

impl CatalogueCursor {
    fn is_well_formed(&self) -> bool {
        self.revision.len() == 64 && self.revision.bytes().all(|b| b.is_ascii_hexdigit())
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

#[cfg(test)]
mod catalogue_bounds_tests {
    use super::*;

    fn surface(index: usize) -> serde_json::Value {
        serde_json::json!({
            "peer_id": index,
            "app_id": "so.tilo.word-processor",
            "record_id": format!("tab:{index:016x}"),
            "live": true,
            "actions": vec![serde_json::json!({
                "resource": format!("document:{index}"),
                "action": "edit",
                "description": "x".repeat(900),
                "params": [],
            }); 20],
        })
    }

    fn refresh(after: Option<CatalogueCursor>) -> ActionMessage {
        ActionMessage::CatalogueRefresh { scope: None, after }
    }

    fn whole(surfaces: &[serde_json::Value]) -> ActionMessage {
        ActionMessage::BrokerCatalogue {
            host: Uuid::from_u128(7),
            catalogue: serde_json::json!({"version": 1, "surfaces": surfaces, "agentReadsV1": {"fresh": true}}),
            next: None,
        }
    }

    fn directed(message: ActionMessage) -> DirectedAction {
        DirectedAction {
            room: Uuid::from_u128(1),
            request_id: Uuid::from_u128(2),
            source: [1; 32],
            target: [2; 32],
            message,
        }
    }

    /// A catalogue far larger than one private message arrives whole, each
    /// surface exactly once, across pages that each fit a message.
    #[test]
    fn a_catalogue_above_one_message_arrives_whole_across_pages() {
        let surfaces = (0..40).map(surface).collect::<Vec<_>>();
        let size = serde_json::to_vec(&whole(&surfaces)).unwrap().len();
        assert!(
            size > 2 * MAX_PRIVATE_BYTES,
            "{size} bytes spans several messages"
        );
        let mut seen = Vec::new();
        let mut after = None;
        let mut pages = 0;
        loop {
            let page = serve_catalogue_page(&refresh(after.take()), whole(&surfaces));
            assert!(
                directed(page.clone()).validate(),
                "every page fits one message"
            );
            let ActionMessage::BrokerCatalogue {
                catalogue, next, ..
            } = page
            else {
                panic!("a page of the catalogue: {page:?}");
            };
            assert_eq!(catalogue["agentReadsV1"]["fresh"], true);
            seen.extend(catalogue["surfaces"].as_array().unwrap().iter().cloned());
            pages += 1;
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert!(pages > 2);
        let mut expected = surfaces;
        expected.sort_by_cached_key(|s| serde_json::to_vec(s).unwrap());
        assert_eq!(seen, expected, "every surface once, whole, in one order");
    }

    /// Paging through a catalogue that changes meanwhile starts again
    /// rather than mixing two catalogues; one that fits a message is one
    /// page, whatever order its surfaces were gathered in.
    #[test]
    fn a_catalogue_that_moves_while_paged_is_started_again() {
        let surfaces = (0..40).map(surface).collect::<Vec<_>>();
        let ActionMessage::BrokerCatalogue {
            next: Some(next), ..
        } = serve_catalogue_page(&refresh(None), whole(&surfaces))
        else {
            panic!("more than one page");
        };
        let changed = (1..41).map(surface).collect::<Vec<_>>();
        assert_eq!(
            serve_catalogue_page(&refresh(Some(next.clone())), whole(&changed)),
            ActionMessage::Rejected {
                code: CATALOGUE_MOVED.into()
            }
        );
        let mut reordered = surfaces;
        reordered.reverse();
        assert!(
            matches!(
                serve_catalogue_page(&refresh(Some(next)), whole(&reordered)),
                ActionMessage::BrokerCatalogue { .. }
            ),
            "the same catalogue gathered in another order is the same catalogue"
        );
        let small = (0..3).map(surface).collect::<Vec<_>>();
        assert!(matches!(
            serve_catalogue_page(&refresh(None), whole(&small)),
            ActionMessage::BrokerCatalogue { next: None, .. }
        ));
    }

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
            after: None,
        };
        let encoded = serde_json::to_vec(&scoped).unwrap();
        let frozen: FrozenActionMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(frozen, FrozenActionMessage::CatalogueRefresh);
        assert_eq!(
            serde_json::to_vec(&frozen).unwrap(),
            br#"{"type":"catalogue_refresh"}"#
        );
        assert_eq!(
            serde_json::to_vec(&ActionMessage::CatalogueRefresh {
                scope: None,
                after: None
            })
            .unwrap(),
            br#"{"type":"catalogue_refresh"}"#
        );
        assert_eq!(
            serde_json::from_slice::<ActionMessage>(&encoded).unwrap(),
            scoped
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
                next: None,
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
                    next: None,
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
                next: None,
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
mod agent_read_codec_tests {
    use super::*;
    use serde_json::{Value, json};

    const HOST: &str = "11111111-1111-4111-8111-111111111111";
    const INVOCATION: &str = "22222222-2222-4222-8222-222222222222";

    #[expect(
        clippy::needless_pass_by_value,
        reason = "test helper keeps the call shape of the fixtures it builds"
    )]
    fn request(body: Value) -> Value {
        json!({"type":"agent_read_v1","host":HOST,"invocation":INVOCATION,"request":body})
    }
    fn valid(value: Value) -> bool {
        serde_json::from_value::<ActionMessage>(value).is_ok_and(|message| message.validate())
    }
    fn result() -> Value {
        let binding = json!({"machine":"machine","room":HOST,"device":"ab".repeat(32),"host":HOST});
        json!({"type":"agent_read_result_v1","host":HOST,"invocation":INVOCATION,
            "binding":{"actionHost":HOST,"agent":binding,"spawn":null,"message":null},
            "response":{"kind":"agent_list","version":1,"invocationId":INVOCATION,
                "binding":binding,"agents":[],"nextCursor":null,"revision":"1"}})
    }
    #[test]
    fn agent_read_only_requests_round_trip_without_business_invocation() {
        for body in [
            json!({"kind":"agent_capabilities","version":1}),
            json!({"kind":"agent_list","version":1,"cursor":null}),
            json!({"kind":"agent_conversation","version":1,"agent_id":"agent","cursor":null}),
        ] {
            let value = request(body);
            let parsed = serde_json::from_value::<ActionMessage>(value.clone());
            assert!(parsed.is_ok(), "read request must decode: {parsed:?}");
            let message = parsed.unwrap();
            assert!(message.validate());
            assert_eq!(message.invocation(), None);
            assert_eq!(serde_json::to_value(message).unwrap(), value);
        }
    }
    #[test]
    fn agent_read_rejects_mutations_status_unknown_fields_and_versions() {
        for body in [
            json!({"kind":"agent_spawn","version":1,"name":"n","task":"t","grant_set_id":"g"}),
            json!({"kind":"agent_message","version":1,"agent_id":"a","text":"t"}),
            json!({"kind":"agent_status","version":1,"invocation_id":"x"}),
            json!({"kind":"agent_list","version":2,"cursor":null}),
            json!({"kind":"agent_list","version":1,"cursor":null,"extra":true}),
            json!({"kind":"agent_list","version":1}),
        ] {
            assert!(!valid(request(body)));
        }
    }
    #[test]
    fn agent_read_result_checks_binding_and_correlation() {
        let baseline = result();
        assert!(valid(baseline.clone()));
        for path in [
            vec!["host"],
            vec!["invocation"],
            vec!["binding", "actionHost"],
            vec!["binding", "agent", "host"],
            vec!["response", "invocationId"],
            vec!["response", "binding", "device"],
        ] {
            let mut invalid = baseline.clone();
            let mut slot = &mut invalid;
            for key in path {
                slot = &mut slot[key];
            }
            *slot = json!("33333333-3333-4333-8333-333333333333");
            assert!(!valid(invalid));
        }
        let mut oversized = baseline;
        oversized["binding"]["spawn"] = json!({"route":{"app_peer":"1","app_id":"app","record_id":"","resource":"r","action":"spawn","revision":"1"},"descriptor":{"resource":"r","action":"spawn","revision":"1","blob":"x".repeat(240*1024)}});
        assert!(!valid(oversized));
    }
}
