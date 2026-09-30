//! The `read-text` action: a closed request, its result contract, and the
//! body-text projection the SDK runs over the committed canonical document.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tilo_actions::{
    ActionContract, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    ResultSpec,
};
use ui::semantic_document_read::{
    MAX_REQUEST_BYTES, ReadPage, RegisteredSemanticReader, SemanticReadError,
};

pub const OPERATION_TYPE: &str = "word-processor.read";
pub static READERS: [RegisteredSemanticReader; 1] = [RegisteredSemanticReader::new(
    OPERATION_TYPE,
    result_schema,
    validate_request,
    project,
)];

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    page_size: u8,
    cursor: Option<String>,
    #[serde(default)]
    format: Format,
}

/// What a page reads as: Markdown by default, or the raw segments.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    #[default]
    Markdown,
    Segments,
}

/// A page's size when the caller names none: the most one page holds.
const DEFAULT_PAGE_SIZE: u8 = 64;

/// Shape validation only; no supplied heads or cursor confer authority.
fn validate_request(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    if resource.kind != "tilo.rich-text"
        || resource.id.as_str() != "word-processor.document"
        || !(1..=64).contains(&request.page_size)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    if let Some(cursor) = request.cursor {
        if cursor.len() > 1024 {
            return Err(SemanticReadError::Limit);
        }
        let _: super::Cursor =
            serde_json::from_str(&cursor).map_err(|_| SemanticReadError::InvalidRequest)?;
    }
    Ok(())
}

pub fn spec() -> ActionSpec {
    let spec = ActionSpec::typed_operation(
        "read-text",
        "Read document text",
        "optional pageSize, cursor and format",
        OperationKind::Effect,
        OPERATION_TYPE,
        vec![
            ActionParam::optional(
                "pageSize",
                ParamType::Integer,
                "Maximum blocks in this page (default 64)",
            )
            .with_schema(serde_json::json!({"type":"integer","minimum":1,"maximum":64})),
            ActionParam::optional(
                "cursor",
                ParamType::Text,
                "Opaque nextCursor from the preceding page",
            )
            .with_schema(serde_json::json!({"type":"string","maxLength":1024})),
            ActionParam::optional(
                "format",
                ParamType::Text,
                "markdown (default) or segments: each block's raw text, kind and id, cells too",
            )
            .with_schema(serde_json::json!({"type":"string","enum":["markdown","segments"]})),
        ],
        move |args| {
            let request = Request {
                page_size: args.integer("pageSize").map_or(DEFAULT_PAGE_SIZE, |n| {
                    u8::try_from(n).unwrap_or(0)
                }),
                cursor: args.text("cursor").map(str::to_owned),
                format: match args.text("format") {
                    Some("segments") => Format::Segments,
                    _ => Format::Markdown,
                },
            };
            EncodedAction::encode_operation(OPERATION_TYPE, &request).unwrap_or_else(|_| {
                EncodedAction {
                    type_id: OPERATION_TYPE.into(),
                    payload: Vec::new(),
                }
            })
        },
    )
    .with_description(
        "The document as Markdown, one block per line ending in its {#block id}; **bold**, *italic*, ~~struck~~, `code` and [links](url) show inline, their delimiters not counted in character offsets; tables are Markdown tables tagged {#table id}, each cell ending in its {#block id}. Pages of up to 64 blocks: pass nextCursor for the next",
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: result_schema().clone(),
        },
        ..ActionContract::view()
    });
    crate::actions::continuation::pure(
        spec,
        "qualified-read-parameter-protocol.v1",
        vec![b"word_processor.document".to_vec()],
    )
}

/// One page of body text at `heads`, for a request [`validate_request`] accepted.
fn project(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
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
    let page = super::read_text(document, heads, cursor.as_ref(), limits)
        .map_err(|error| super::read_error(&error))?;
    let next_cursor = page
        .next
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| SemanticReadError::Limit)?;
    let content = match request.format {
        Format::Markdown => {
            serde_json::json!({ "markdown": super::markdown::render(&page.segments) })
        }
        Format::Segments => serde_json::json!({ "segments": page.segments }),
    };
    Ok(ReadPage {
        content,
        next_cursor,
    })
}

/// The SDK stamps `record` and `basis`; the projection supplies `content` and
/// `nextCursor`.
pub fn result_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let segments = serde_json::json!({"type":"object","required":["segments"],"additionalProperties":false,"properties":{
            "segments":{"type":"array","maxItems":64,"items":{"type":"object","required":["block","kind","text","complete"],"additionalProperties":false,"properties":{
                "block":{"type":"string","maxLength":256},"kind":{"type":"string","maxLength":64},"text":{"type":"string"},"complete":{"type":"boolean"},
                "level":{"type":"integer"},"ordered":{"type":"boolean"},"nesting":{"type":"integer"},
                "table":{"type":"string","maxLength":128},"row":{"type":"integer"},"col":{"type":"integer"}
            }}}
        }});
        let markdown = serde_json::json!({"type":"object","required":["markdown"],"additionalProperties":false,"properties":{
            "markdown":{"type":"string"}
        }});
        let content = serde_json::json!({"oneOf":[markdown, segments]});
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
