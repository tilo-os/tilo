//! A canonical report projection through the existing semantic-read door.
//! The SDK stamps the actual record and committed heads. The result alone is
//! not proof of disk durability: feedback hosts accept it only from the pinned
//! authenticated destination owner after that owner's durable commit.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, Resource};
use ui::semantic_document_read::{
    ReadPage, RegisteredSemanticReader, SemanticReadError, decode_request,
};
#[allow(unused_imports, reason = "the `JsonSchema` derive names the crate")]
use ui::tilo_resource;

const OPERATION: &str = "chat.feedback.read";
pub(crate) const READER: RegisteredSemanticReader =
    RegisteredSemanticReader::new(OPERATION, schema, validate, project);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub(super) id: String,
    #[serde(
        default,
        rename = "expectedAuthor",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) expected_author: Option<String>,
}

pub(super) fn validate(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    let request: Request = decode_request(payload)?;
    if request.id.len() != 32
        || !request.id.bytes().all(|c| c.is_ascii_hexdigit())
        || resource.kind != "chat.workspace.feedback"
        || resource.id.as_str()
            != ui::tilo_resource::items::item_resource_id::<crate::resource::Chat>(
                "feedback",
                &ui::tilo_resource::Id::from_raw(request.id),
            )
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    if request.expected_author.as_ref().is_some_and(|author| {
        author.len() != 64 || !author.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
}

/// What a receipt read is invoked with.
#[derive(Deserialize, ui::tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Params {
    /// Require the attributed reporter root
    expected_author: Option<String>,
}

pub(crate) fn spec(id: &str) -> ActionSpec {
    let id = id.to_owned();
    ui::semantic_document_read::read_action(
        ui::semantic_document_read::ReadName {
            id: ui::feedback::destination::READ_RECEIPT,
            label: "Read feedback receipt",
            description: "Read this report from its canonical destination",
        },
        OPERATION,
        schema,
        move |Params { expected_author }: Params| Request {
            id: id.clone(),
            expected_author,
        },
    )
}

fn project(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    let (receipt, _) = snapshot(document, heads, payload)?;
    Ok(ReadPage {
        content: serde_json::to_value(receipt).map_err(|_| SemanticReadError::Malformed)?,
        next_cursor: None,
    })
}

pub(super) fn snapshot(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<(ui::feedback::Receipt, Vec<super::TriageEntry>), SemanticReadError> {
    let request: Request = decode_request(payload)?;
    let document = document
        .clone()
        .fork_at(heads)
        .map_err(|_| SemanticReadError::Malformed)?;
    let report = super::report(&document, &request.id)
        .map_err(|_| SemanticReadError::Malformed)?
        .ok_or(SemanticReadError::InvalidRequest)?;
    let destination = &report.submission.destination;
    if request
        .expected_author
        .as_ref()
        .is_some_and(|author| *author != report.author)
    {
        return Err(SemanticReadError::InvalidOwner);
    }
    if ui::record::room_scope(&document).is_some_and(|room| room != destination.room) {
        return Err(SemanticReadError::InvalidOwner);
    }
    let receipt = super::receipt_content(&document, &request.id, destination)
        .map_err(|_| SemanticReadError::Malformed)?;
    Ok((receipt, report.triage))
}

pub(super) fn schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        ui::semantic_document_read::result_schema(
            serde_json::json!({
                "type":"object", "additionalProperties":false,
                "required":["id","digest","destination","report"], "properties":{
                    "id":{"type":"string","pattern":"^[0-9a-fA-F]{32}$"},
                    "digest":{"type":"string","pattern":"^[0-9a-f]{64}$"},
                    "report":{"type":"string","minLength":1,"maxLength":512},
                    "destination":{"type":"object","additionalProperties":false,
                        "required":["room","provider","channel"],"properties":{
                            "room":{"type":"string","minLength":1,"maxLength":256},
                            "provider":{"type":"string","minLength":1,"maxLength":256},
                            "channel":{"type":"string","minLength":1,"maxLength":256}
                        }}
                }
            }),
            false,
        )
    })
}

#[cfg(test)]
mod tests;
