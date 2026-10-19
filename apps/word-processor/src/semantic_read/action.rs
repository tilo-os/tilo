//! The `read-text` action: a closed request, its result contract, and the
//! body-text projection the SDK runs over the committed canonical document.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tilo_actions::{ActionSpec, Resource};
use ui::semantic_document_read::{
    ReadPage, RegisteredSemanticReader, SemanticReadError, decode_cursor, decode_request,
    encode_cursor,
};
#[allow(unused_imports, reason = "the `JsonSchema` derive names the crate")]
use ui::tilo_resource;

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
#[derive(
    Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::JsonSchema,
)]
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
    let request: Request = decode_request(payload)?;
    let kind = <crate::model::Document as ui::tilo_resource::Resource>::KIND;
    if resource.kind != kind
        || resource.id.as_str() != kind
        || !(1..=64).contains(&request.page_size)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    decode_cursor::<super::Cursor>(request.cursor.as_deref())?;
    Ok(())
}

/// What `read-text` is invoked with.
#[derive(Deserialize, ui::tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Params {
    /// Maximum blocks in this page (default 64)
    #[schema(range(min = 1, max = 64))]
    page_size: Option<u8>,
    /// Opaque nextCursor from the preceding page
    #[schema(range(max = 1024))]
    cursor: Option<String>,
    /// markdown (default) or segments: each block's raw text, kind and id, cells too
    format: Option<Format>,
}

pub fn spec() -> ActionSpec {
    let spec = ui::semantic_document_read::read_action(
        ui::semantic_document_read::ReadName {
            id: "read-text",
            label: "Read document text",
            description: "optional pageSize, cursor and format",
        },
        OPERATION_TYPE,
        result_schema,
        |params: Params| Request {
            page_size: params.page_size.unwrap_or(DEFAULT_PAGE_SIZE),
            cursor: params.cursor,
            format: params.format.unwrap_or_default(),
        },
    )
    .with_description(
        "The document as Markdown, one block per line ending in its {#block id}; **bold**, *italic*, ~~struck~~, `code` and [links](url) show inline, their delimiters not counted in character offsets; tables are Markdown tables tagged {#table id}, each cell ending in its {#block id}. Pages of up to 64 blocks: pass nextCursor for the next",
    )
    .reads_document();
    crate::actions::continuation::pure(
        spec,
        "qualified-read-parameter-protocol.v1",
        vec![b"word-processor.document".to_vec()],
    )
}

/// One page of body text at `heads`, for a request [`validate_request`] accepted.
fn project(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    let request: Request = decode_request(payload)?;
    let cursor: Option<super::Cursor> = decode_cursor(request.cursor.as_deref())?;
    let limits = tilo_doc::semantic_read::Limits {
        items: usize::from(request.page_size),
        ..tilo_doc::semantic_read::Limits::default()
    };
    let page = super::read_text(document, heads, cursor.as_ref(), limits)?;
    let next_cursor = encode_cursor(page.next.as_ref())?;
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
        ui::semantic_document_read::result_schema(content, true)
    })
}

#[cfg(test)]
mod tests;
