//! A canonical report projection through the existing semantic-read door.
//! The SDK stamps the actual record and committed heads. The result alone is
//! not proof of disk durability: feedback hosts accept it only from the pinned
//! authenticated destination owner after that owner's durable commit.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionSpec, EncodedAction, OperationKind, Resource, ResultSpec,
};
use ui::semantic_document_read::{
    MAX_REQUEST_BYTES, ReadPage, RegisteredSemanticReader, SemanticReadError,
};

const OPERATION: &str = "chat.feedback.read";
pub(crate) const READER: RegisteredSemanticReader =
    RegisteredSemanticReader::new(OPERATION, schema, validate, project);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub(super) id: String,
}

pub(super) fn validate(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    if request.id.len() != 32
        || !request.id.bytes().all(|c| c.is_ascii_hexdigit())
        || resource.kind != "chat.feedback"
        || resource.id.as_str() != format!("chat.feedback:{}", request.id)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
}

pub(super) fn spec(id: &str) -> ActionSpec {
    let id = id.to_owned();
    ActionSpec::typed_operation(
        "read-feedback-receipt",
        "Read feedback receipt",
        "Read this report from its canonical destination",
        OperationKind::Effect,
        OPERATION,
        Vec::new(),
        move |_| {
            EncodedAction::encode_operation(OPERATION, &Request { id: id.clone() }).unwrap_or_else(
                |_| EncodedAction {
                    type_id: OPERATION.into(),
                    payload: Vec::new(),
                },
            )
        },
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: schema().clone(),
        },
        ..ActionContract::view()
    })
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
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    let document = document
        .clone()
        .fork_at(heads)
        .map_err(|_| SemanticReadError::Malformed)?;
    let report = super::report(&document, &request.id)
        .map_err(|_| SemanticReadError::Malformed)?
        .ok_or(SemanticReadError::InvalidRequest)?;
    let destination = &report.submission.destination;
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
        let mut schema = crate::semantic_read::action::result_schema().clone();
        let Some(properties) = schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return serde_json::json!(false);
        };
        properties.insert(
            "content".into(),
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
        );
        properties.insert("nextCursor".into(), serde_json::json!({"type":"null"}));
        schema
    })
}

#[cfg(test)]
mod tests;
