//! Attributed triage at the canonical committed basis, through the ordinary
//! View door. This projection carries no new task or document authority.
use std::sync::OnceLock;

use tilo_actions::{ActionContract, ActionSpec, EncodedAction, OperationKind, ResultSpec};
use ui::semantic_document_read::{ReadPage, RegisteredSemanticReader, SemanticReadError};

const OPERATION: &str = "chat.feedback.status";
pub(crate) const READER: RegisteredSemanticReader =
    RegisteredSemanticReader::new(OPERATION, schema, super::receipt_read::validate, project);

pub(crate) fn spec(id: &str) -> ActionSpec {
    let id = id.to_owned();
    ActionSpec::typed_operation(
        "read-feedback-status",
        "Read feedback status",
        "Read this report's recent attributed triage history",
        OperationKind::Effect,
        OPERATION,
        Vec::new(),
        move |_| {
            EncodedAction::encode_operation(
                OPERATION,
                &super::receipt_read::Request {
                    id: id.clone(),
                    expected_author: None,
                },
            )
            .unwrap_or_else(|_| EncodedAction {
                type_id: OPERATION.into(),
                payload: Vec::new(),
            })
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
    let (receipt, mut triage) = super::receipt_read::snapshot(document, heads, payload)?;
    // Concurrent full histories can merge past the per-writer 128-entry limit.
    // Keep the read bounded and explicitly account for any older entries.
    let earlier_entries = triage.len().saturating_sub(128);
    triage.drain(..earlier_entries);
    Ok(ReadPage {
        content: serde_json::json!({"receipt":receipt,"triage":triage,"earlierEntries":earlier_entries}),
        next_cursor: None,
    })
}

fn schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = super::receipt_read::schema().clone();
        let Some(properties) = schema.get_mut("properties").and_then(serde_json::Value::as_object_mut) else {
            return serde_json::json!(false);
        };
        let Some(receipt) = properties.get("content").cloned() else {
            return serde_json::json!(false);
        };
        properties.insert("content".into(), serde_json::json!({
            "type":"object","additionalProperties":false,"required":["receipt","triage","earlierEntries"],
            "properties":{
                "receipt":receipt,
                "earlierEntries":{"type":"integer","minimum":0},
                "triage":{"type":"array","maxItems":128,"items":{
                    "type":"object","additionalProperties":false,"required":["author","ts","update"],
                    "properties":{
                        "author":{"type":"string","minLength":1},"ts":{"type":"integer"},
                        "update": update_schema()
                    }
                }}
            }
        }));
        schema
    })
}

fn update_schema() -> serde_json::Value {
    serde_json::json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["status"],
         "properties":{"status":{"enum":["investigating","resolved","reopen"]}}},
        {"type":"object","additionalProperties":false,"required":["status","report"],
         "properties":{"status":{"const":"duplicate"},
                       "report":{"type":"string","pattern":"^[0-9a-fA-F]{32}$"}}},
        {"type":"object","additionalProperties":false,"required":["status","reference"],
         "properties":{"status":{"const":"task"},"reference":{
             "type":"object","additionalProperties":false,"required":["kind","id","label","span"],
             "properties":{
                 "kind":{"type":"string","minLength":1,"maxLength":128},
                 "id":{"type":"string","minLength":1,"maxLength":2048},
                 "label":{"type":"string","minLength":1,"maxLength":512},
                 "span":{"oneOf":[{"type":"null"},{
                     "type":"object","additionalProperties":false,"required":["start","end","unit"],
                     "properties":{"start":{"type":"integer","minimum":0},
                                   "end":{"type":"integer","minimum":0},
                                   "unit":{"enum":["Chars","Bytes"]}}
                 }]}
             }
         }}}
    ]})
}

#[cfg(test)]
mod tests;
