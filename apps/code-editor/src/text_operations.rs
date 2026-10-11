//! Bounded reads of a collaborative file, never disk paths. Edits are
//! field edits of the file's text ([`crate::project::SourceEdit`]).
use automerge::{ReadDoc, transaction::Transactable};
use serde::Serialize;
use tilo_actions::{ActionArgs, ActionParam, ActionSpec, Doc, ParamType};

use crate::doc::EditorDoc;
use crate::project::text_digest;

pub(crate) const MAX_READ_CHARS: usize = 4096;
const MAX_READ_BYTES: usize = 8192;
const MAX_REPLY_BYTES: usize = 32768;
const CONTEXT_CHARS: usize = crate::project::CONTEXT_CHARS;

#[derive(Serialize)]
pub(crate) struct TextRead {
    file: String,
    position_units: &'static str,
    text_sha256: String,
    total_chars: usize,
    start_chars: usize,
    next_chars: usize,
    eof: bool,
    text: String,
    before_context: String,
    after_context: String,
}

pub(crate) fn read(
    doc: &Doc,
    file: &str,
    start: usize,
    maximum: usize,
) -> Result<TextRead, String> {
    if maximum == 0 || maximum > MAX_READ_CHARS {
        return Err("read length must be between 1 and 4096 Unicode scalar values".into());
    }
    let record = EditorDoc::file(doc, file).ok_or("file does not exist")?;
    if record.deleted || record.kind != "file" {
        return Err("text read target is not a live file".into());
    }
    let body = EditorDoc::read_buffer_key(doc, file)
        .ok_or("file body is unavailable; open this file first")?;
    let total = body.chars().count();
    if start > total {
        return Err("read position is outside the file".into());
    }
    let mut text = String::new();
    for character in body.chars().skip(start).take(maximum) {
        if text.len().saturating_add(character.len_utf8()) > MAX_READ_BYTES {
            break;
        }
        text.push(character);
    }
    let next = start.saturating_add(text.chars().count());
    let answer = TextRead {
        file: file.into(),
        position_units: "unicode_scalar_values",
        text_sha256: text_digest(&body),
        total_chars: total,
        start_chars: start,
        next_chars: next,
        eof: next == total,
        text,
        before_context: body
            .chars()
            .skip(start.saturating_sub(CONTEXT_CHARS))
            .take(start.min(CONTEXT_CHARS))
            .collect(),
        after_context: body.chars().skip(next).take(CONTEXT_CHARS).collect(),
    };
    if serde_json::to_vec(&answer)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_REPLY_BYTES
    {
        return Err("text read exceeds the aggregate response bound".into());
    }
    Ok(answer)
}

fn integer(args: &ActionArgs, name: &str) -> Result<usize, String> {
    args.integer(name)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value <= crate::disk::MAX_TEXT_BYTES as usize)
        .ok_or_else(|| format!("{name} must be a bounded nonnegative integer"))
}

pub(crate) fn committed_heads(
    doc: &Doc,
    heads: &[automerge::ChangeHash],
    expected: Option<&str>,
) -> Result<Vec<String>, String> {
    if doc.pending_ops() != 0 {
        return Err("source snapshot has uncommitted operations".into());
    }
    if heads.len() > 64 || doc.stats().num_ops > 1_000_000 {
        return Err("source snapshot exceeds its committed basis bound".into());
    }
    let mut actual = heads.iter().map(ToString::to_string).collect::<Vec<_>>();
    actual.sort_unstable();
    if actual
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left == right))
    {
        return Err("source snapshot has duplicate heads".into());
    }
    if let Some(expected) = expected {
        if expected.len() > 4224 {
            return Err("expected committed heads exceed their bound".into());
        }
        let requested: Vec<String> = serde_json::from_str(expected)
            .map_err(|_| "expected committed heads must be a JSON string array")?;
        if requested != actual {
            return Err("source snapshot committed revision changed".into());
        }
    }
    Ok(actual)
}

const SNAPSHOT_OPERATION: &str = "code_editor.committed-text-read";
pub(crate) static SNAPSHOT_READERS: [ui::semantic_document_read::RegisteredSemanticReader; 1] =
    [ui::semantic_document_read::RegisteredSemanticReader::new(
        SNAPSHOT_OPERATION,
        snapshot_schema,
        validate_snapshot,
        project_snapshot,
    )];

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRequest {
    file: String,
    start_chars: usize,
    max_chars: usize,
    expected_heads: Option<String>,
}

fn decode_snapshot(
    payload: &[u8],
) -> Result<SnapshotRequest, ui::semantic_document_read::SemanticReadError> {
    use ui::semantic_document_read::SemanticReadError as Error;
    let request: SnapshotRequest =
        serde_json::from_slice(payload).map_err(|_| Error::InvalidRequest)?;
    if request.file.is_empty()
        || request.file.len() > 256
        || request.max_chars == 0
        || request.max_chars > MAX_READ_CHARS
        || request.start_chars > crate::disk::MAX_TEXT_BYTES as usize
        || request
            .expected_heads
            .as_ref()
            .is_some_and(|heads| heads.len() > 4224)
    {
        return Err(Error::Limit);
    }
    Ok(request)
}

fn validate_snapshot(
    resource: &tilo_actions::Resource,
    payload: &[u8],
) -> Result<(), ui::semantic_document_read::SemanticReadError> {
    let request = decode_snapshot(payload)?;
    if resource.kind != crate::actions::FILE_KIND
        || resource.id.as_str() != crate::actions::file_resource_id(&request.file)
    {
        return Err(ui::semantic_document_read::SemanticReadError::InvalidRequest);
    }
    Ok(())
}

fn project_snapshot(
    document: &Doc,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ui::semantic_document_read::ReadPage, ui::semantic_document_read::SemanticReadError> {
    use ui::semantic_document_read::{ReadPage, SemanticReadError as Error};
    let request = decode_snapshot(payload)?;
    committed_heads(document, heads, None).map_err(|_| Error::Limit)?;
    if let Some(expected) = request.expected_heads.as_deref() {
        let requested: Vec<String> =
            serde_json::from_str(expected).map_err(|_| Error::InvalidRequest)?;
        let actual = committed_heads(document, heads, None).map_err(|_| Error::Limit)?;
        if requested != actual {
            return Err(Error::StaleCursor);
        }
    }
    let content = serde_json::to_value(
        read(
            document,
            &request.file,
            request.start_chars,
            request.max_chars,
        )
        .map_err(|_| Error::InvalidRequest)?,
    )
    .map_err(|_| Error::Limit)?;
    // Reserve room for the SDK's bounded record and committed-head stamp.
    if serde_json::to_vec(&content)
        .map_err(|_| Error::Limit)?
        .len()
        > 16 * 1024
    {
        return Err(Error::Limit);
    }
    Ok(ReadPage {
        content,
        next_cursor: None,
    })
}

pub(crate) fn snapshot_spec(file: String) -> ActionSpec {
    use tilo_actions::{ActionContract, EncodedAction, OperationKind, ResultSpec};
    ActionSpec::typed_operation(
        "read-text-snapshot",
        "Read committed file text",
        "A bounded page at one exact committed document revision",
        OperationKind::Effect,
        SNAPSHOT_OPERATION,
        vec![
            ActionParam::required(
                "start_chars",
                ParamType::Integer,
                "Zero-based Unicode scalar position",
            ),
            ActionParam::required(
                "max_chars",
                ParamType::Integer,
                "Maximum Unicode scalars, 1 through 4096",
            ),
            ActionParam::optional(
                "expected_heads",
                ParamType::Text,
                "Exact sorted JSON heads array from the first page",
            ),
        ],
        move |args| {
            let request = integer(args, "start_chars").and_then(|start_chars| {
                Ok(SnapshotRequest {
                    file: file.clone(),
                    start_chars,
                    max_chars: integer(args, "max_chars")?,
                    expected_heads: args.text("expected_heads").map(str::to_owned),
                })
            });
            match request {
                Ok(request) => EncodedAction::encode_operation(SNAPSHOT_OPERATION, &request)
                    .unwrap_or_else(|_| {
                        crate::actions::invalid_arguments("snapshot encoding failed")
                    }),
                Err(error) => crate::actions::invalid_arguments(&error),
            }
        },
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: snapshot_schema().clone(),
        },
        ..ActionContract::view()
    })
    .reads_document()
}

fn snapshot_schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::json!({
        "type":"object", "additionalProperties":false,
        "required":["version","record","basis","content","nextCursor"],
        "properties":{
            "version":{"type":"integer","enum":[1]},
            "record":{"type":"object","additionalProperties":false,"required":["roomId","appId","recordId"],"properties":{
                "roomId":{"type":["string","null"],"format":"uuid"},
                "appId":{"type":"string","minLength":1,"maxLength":256},
                "recordId":{"type":"string","maxLength":512}
            }},
            "basis":{"type":"object","additionalProperties":false,"required":["heads"],"properties":{
                "heads":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-f]{64}$"}}
            }},
            "content":{"type":"object","additionalProperties":false,
                "required":["file","position_units","text_sha256","total_chars","start_chars","next_chars","eof","text","before_context","after_context"],
                "properties":{
                    "file":{"type":"string","minLength":1,"maxLength":256},
                    "position_units":{"type":"string","enum":["unicode_scalar_values"]},
                    "text_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                    "total_chars":{"type":"integer","minimum":0},
                    "start_chars":{"type":"integer","minimum":0},
                    "next_chars":{"type":"integer","minimum":0},
                    "eof":{"type":"boolean"},"text":{"type":"string","maxLength":4096},
                    "before_context":{"type":"string","maxLength":32},
                    "after_context":{"type":"string","maxLength":32}
                }
            },
            "nextCursor":{"type":"null"}
        }
    }))
}
