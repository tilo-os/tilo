//! Closed inert request/contract for the ordinary app action door.
//! Registration and a valid request do not qualify the legacy app document.
//! The action stays unavailable until the SDK owns a genuine immutable source,
//! complete committed heads, original-caller custody, and final disclosure fence.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    ResultSpec,
};
use ui::semantic_document_read::{MAX_REQUEST_BYTES, RegisteredSemanticReader, SemanticReadError};

pub const OPERATION_TYPE: &str = "chat.read";
pub static READERS: [RegisteredSemanticReader; 1] = [RegisteredSemanticReader::new(
    OPERATION_TYPE,
    result_schema,
    validate_request,
)];

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    channel: String,
    page_size: u8,
    cursor: Option<String>,
}

/// Shape validation only. The future owner adapter must retain its input/decode
/// credit before calling this allocating legacy serde component. No budget token
/// or supplied heads turn this request data into owner authority.
fn validate_request(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    if resource.kind != "chat.channel"
        || !(resource
            .id
            .as_str()
            .strip_prefix("chat.channel:")
            .is_some_and(|channel| {
                !channel.is_empty() && channel.len() <= 128 && channel == request.channel
            }))
        || !(1..=32).contains(&request.page_size)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    if let Some(cursor) = request.cursor {
        if cursor.len() > 1024 {
            return Err(SemanticReadError::Limit);
        }
        let cursor: super::Cursor =
            serde_json::from_str(&cursor).map_err(|_| SemanticReadError::InvalidRequest)?;
        if cursor.channel != request.channel {
            return Err(SemanticReadError::InvalidRequest);
        }
    }
    Ok(())
}

pub fn spec(channel: &str) -> ActionSpec {
    let channel = channel.to_owned();
    ActionSpec::typed_operation(
        "read-messages",
        "Read conversation messages",
        "pageSize and optional cursor",
        OperationKind::Effect,
        OPERATION_TYPE,
        vec![
            ActionParam::required("pageSize", ParamType::Integer, "Maximum items in this page")
                .with_schema(serde_json::json!({"type":"integer","minimum":1,"maximum":32})),
            ActionParam::optional(
                "cursor",
                ParamType::Text,
                "Opaque nextCursor from the preceding page",
            )
            .with_schema(serde_json::json!({"type":"string","maxLength":1024})),
        ],
        move |args| {
            let request = Request {
                channel: channel.clone(),
                page_size: args
                    .integer("pageSize")
                    .and_then(|n| u8::try_from(n).ok())
                    .unwrap_or(0),
                cursor: args.text("cursor").map(str::to_owned),
            };
            EncodedAction::encode_operation(OPERATION_TYPE, &request).unwrap_or_else(|_| {
                EncodedAction {
                    type_id: OPERATION_TYPE.into(),
                    payload: Vec::new(),
                }
            })
        },
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: result_schema().clone(),
        },
        ..ActionContract::view()
    })
    .unavailable(
        "qualified_read_unavailable",
        "A current committed document read is not available",
    )
}

/// Existing Json wire variant; only the future private owner stamp supplies
/// record and basis. General Completed JSON cannot select that result path.
pub fn result_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let content = serde_json::json!({"type":"object","required":["channel","messages"],"additionalProperties":false,"properties":{
            "channel":{"type":"string","minLength":1,"maxLength":128},
            "messages":{"type":"array","maxItems":32,"items":{"type":"object","required":["id","author","ts","parent","edited","text","complete"],"additionalProperties":false,"properties":{
                "id":{"type":"string","maxLength":128},"author":{"type":"string","maxLength":256},"ts":{"type":"string","pattern":"^-?[0-9]+$"},"parent":{"type":"string","maxLength":128},"edited":{"type":"boolean"},"text":{"type":"string"},"complete":{"type":"boolean"}
            }}}
        }});
        serde_json::json!({"type":"object","required":["version","record","basis","content","nextCursor"],"additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "record":{"type":"object","required":["roomId","appId","recordId"],"additionalProperties":false,"properties":{
                "roomId":{"type":"string","format":"uuid"},"appId":{"type":"string","minLength":1,"maxLength":256},"recordId":{"type":"string","maxLength":512}
            }},
            "basis":{"type":"object","required":["heads"],"additionalProperties":false,"properties":{
                "heads":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-f]{64}$"}}
            }},
            "content":content,
            "nextCursor":{"type":["string","null"],"maxLength":1024}
        }})
    })
}

#[cfg(test)]
mod tests;
