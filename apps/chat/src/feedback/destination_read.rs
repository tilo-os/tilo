//! Inspect an existing text-channel destination without creating or joining it.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionSpec, EncodedAction, OperationKind, Resource, ResultSpec,
};
use ui::semantic_document_read::{
    MAX_REQUEST_BYTES, ReadPage, RegisteredSemanticReader, SemanticReadError,
};

const OPERATION: &str = "chat.feedback.destination";
pub(crate) const READER: RegisteredSemanticReader =
    RegisteredSemanticReader::new(OPERATION, schema, validate, project);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    channel: String,
}

fn validate(resource: &Resource, payload: &[u8]) -> Result<(), SemanticReadError> {
    if payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    if request.channel.is_empty()
        || request.channel.len() > 256
        || resource.kind != "chat.channel"
        || resource.id.as_str() != format!("chat.channel:{}", request.channel)
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
}

pub(crate) fn spec(channel: &str) -> ActionSpec {
    let channel = channel.to_owned();
    ActionSpec::typed_operation(
        "read-feedback-destination",
        "Read feedback destination",
        "Inspect this existing text channel",
        OperationKind::Effect,
        OPERATION,
        Vec::new(),
        move |_| {
            EncodedAction::encode_operation(
                OPERATION,
                &Request {
                    channel: channel.clone(),
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
    let request: Request =
        serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)?;
    let document = document
        .clone()
        .fork_at(heads)
        .map_err(|_| SemanticReadError::Malformed)?;
    if !crate::doc::ChatDoc::list_channels(&document)
        .iter()
        .any(|channel| {
            channel.id == request.channel && channel.kind != crate::model::ChannelKind::Voice
        })
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(ReadPage {
        content: serde_json::json!({"channel":request.channel,"kind":"text"}),
        next_cursor: None,
    })
}

fn schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = crate::semantic_read::action::result_schema().clone();
        let Some(properties) = schema.get_mut("properties").and_then(serde_json::Value::as_object_mut) else { return serde_json::json!(false); };
        properties.insert("content".into(), serde_json::json!({"type":"object","additionalProperties":false,"required":["channel","kind"],"properties":{"channel":{"type":"string","minLength":1,"maxLength":256},"kind":{"const":"text"}}}));
        properties.insert("nextCursor".into(),serde_json::json!({"type":"null"}));
        schema
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    #[test]
    fn destination_read_requires_an_existing_text_channel() {
        let mut document = automerge::AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut document);
        document.commit();
        let channels = crate::doc::ChatDoc::list_channels(&document);
        let text = channels
            .iter()
            .find(|channel| channel.kind == crate::model::ChannelKind::Text)
            .unwrap();
        let voice = channels
            .iter()
            .find(|channel| channel.kind == crate::model::ChannelKind::Voice)
            .unwrap();
        let request = |channel: &str| {
            serde_json::to_vec(&Request {
                channel: channel.into(),
            })
            .unwrap()
        };
        let page = project(&document, &document.base_heads(), &request(&text.id)).unwrap();
        assert_eq!(
            page.content,
            serde_json::json!({"channel":text.id,"kind":"text"})
        );
        assert!(page.next_cursor.is_none());
        for channel in [voice.id.as_str(), "missing-channel"] {
            assert_eq!(
                project(&document, &document.base_heads(), &request(channel)).err(),
                Some(SemanticReadError::InvalidRequest)
            );
        }
    }
}
