//! Inspect an existing text-channel destination without creating or joining it.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, Resource};
use ui::semantic_document_read::{
    ReadPage, RegisteredSemanticReader, SemanticReadError, decode_request,
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
    let request: Request = decode_request(payload)?;
    let named =
        ui::tilo_resource::items::item_of_resource::<crate::resource::Chat>(resource.id.as_str());
    if request.channel.is_empty()
        || request.channel.len() > 256
        || resource.kind != "chat.workspace.channels"
        || !named.is_some_and(|(field, channel)| {
            field == "channels" && channel.as_str() == request.channel
        })
    {
        return Err(SemanticReadError::InvalidRequest);
    }
    Ok(())
}

pub(crate) fn spec(channel: &str) -> ActionSpec {
    let channel = channel.to_owned();
    super::receipt_read::read_spec(
        (
            ui::feedback::destination::READ_DESTINATION,
            "Read feedback destination",
            "Inspect this existing text channel",
        ),
        OPERATION,
        Vec::new(),
        schema,
        move |_| Request {
            channel: channel.clone(),
        },
    )
}

fn project(
    document: &automerge::AutoCommit,
    heads: &[automerge::ChangeHash],
    payload: &[u8],
) -> Result<ReadPage, SemanticReadError> {
    let request: Request = decode_request(payload)?;
    let document = document
        .clone()
        .fork_at(heads)
        .map_err(|_| SemanticReadError::Malformed)?;
    if !crate::projection::read(&document)
        .channel_rows()
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
        ui::semantic_document_read::result_schema(serde_json::json!({"type":"object","additionalProperties":false,"required":["channel","kind"],"properties":{"channel":{"type":"string","minLength":1,"maxLength":256},"kind":{"const":"text"}}}), false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    #[test]
    fn destination_read_requires_an_existing_text_channel() {
        let mut document = crate::testing::demo_doc();
        document.commit();
        let channels = crate::testing::channels(&document);
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
