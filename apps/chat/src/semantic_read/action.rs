//! The `read-messages` action: a closed request, its result contract, and the
//! channel projection the SDK runs over the committed canonical document.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    ResultSpec,
};
use ui::semantic_document_read::{
    MAX_REQUEST_BYTES, ReadPage, RegisteredSemanticReader, SemanticReadError,
};

pub const OPERATION_TYPE: &str = "chat.read";
pub static READERS: [RegisteredSemanticReader; 3] = [
    RegisteredSemanticReader::new(OPERATION_TYPE, result_schema, validate_request, project),
    crate::feedback::receipt_read::READER,
    crate::feedback::status_read::READER,
];

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    channel: String,
    page_size: u8,
    cursor: Option<String>,
}

/// Shape validation only; no supplied heads or cursor confer authority.
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
}

/// One page of the channel's messages at `heads`, for a request
/// [`validate_request`] accepted.
fn project(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    use tilo_doc::semantic_read::ReadError;
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    let cursor = request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<super::Cursor>)
        .transpose()
        .map_err(|_| SemanticReadError::InvalidRequest)?;
    let limits = tilo_doc::semantic_read::Limits {
        items: usize::from(request.page_size),
        ..tilo_doc::semantic_read::Limits::default()
    };
    let page = super::read_messages(document, heads, &request.channel, cursor.as_ref(), limits)
        .map_err(|error| match error {
            ReadError::Pending => SemanticReadError::Pending,
            ReadError::StaleCursor => SemanticReadError::StaleCursor,
            ReadError::InvalidCursor => SemanticReadError::InvalidRequest,
            ReadError::Limit => SemanticReadError::Limit,
            ReadError::Malformed => SemanticReadError::Malformed,
        })?;
    let messages: Vec<serde_json::Value> = page
        .messages
        .iter()
        .map(|message| {
            let mut value = serde_json::json!({
                "id": message.id,
                "author": message.author,
                "ts": message.ts.to_string(),
                "parent": message.parent,
                "edited": message.edited,
                "text": message.text,
                "complete": message.complete,
            });
            if let (Some(intent), Some(object)) = (message.agent_intent, value.as_object_mut()) {
                object.insert(
                    "agentIntent".into(),
                    serde_json::Value::String(intent.as_str().into()),
                );
            }
            value
        })
        .collect();
    let next_cursor = page
        .next
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| SemanticReadError::Limit)?;
    Ok(ReadPage {
        content: serde_json::json!({ "channel": page.channel, "messages": messages }),
        next_cursor,
    })
}

/// The SDK stamps `record` and `basis`; the projection supplies `content` and
/// `nextCursor`.
pub fn result_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let content = serde_json::json!({"type":"object","required":["channel","messages"],"additionalProperties":false,"properties":{
            "channel":{"type":"string","minLength":1,"maxLength":128},
            "messages":{"type":"array","maxItems":32,"items":{"type":"object","required":["id","author","ts","parent","edited","text","complete"],"additionalProperties":false,"properties":{
                "id":{"type":"string","maxLength":128},"author":{"type":"string","maxLength":256},"ts":{"type":"string","pattern":"^-?[0-9]+$"},"parent":{"type":"string","maxLength":128},"edited":{"type":"boolean"},"text":{"type":"string"},"complete":{"type":"boolean"},"agentIntent":{"type":"string","enum":["message","queue","steer"]}
            }}}
        }});
        serde_json::json!({"type":"object","required":["version","record","basis","content","nextCursor"],"additionalProperties":false,"properties":{
            "version":{"type":"integer","enum":[1]},
            "record":{"type":"object","required":["roomId","appId","recordId"],"additionalProperties":false,"properties":{
                "roomId":{"type":["string","null"],"format":"uuid"},"appId":{"type":"string","minLength":1,"maxLength":256},"recordId":{"type":"string","maxLength":512}
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
