//! Comparison-only Writer invocation identities; never authority.
use tilo_actions::{
    ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationKey, ContinuationQuery,
    EncodedAction,
};

type Parts = Vec<Vec<u8>>;
fn key(kind: &str, parts: Parts) -> ContinuationKey {
    ContinuationKey::new(format!("writer.continuation.{kind}.v1"), parts)
}
fn evidence(handler: &'static str, parts: Parts, exact: bool) -> ContinuationEvidence {
    let handler = key("handler", vec![handler.as_bytes().to_vec()]);
    if exact {
        ContinuationEvidence::Exact {
            target: key("target", parts),
            handler,
        }
    } else {
        ContinuationEvidence::Draft {
            resolution_domain: key("domain", parts),
            handler,
        }
    }
}
/// Only the audited pure Writer argument encoders use this helper. Both real
/// invocation and evidence reuse the original normalization closure.
fn pure_encoding(spec: &ActionSpec, args: &ContinuationArguments<'_>) -> Option<EncodedAction> {
    let encoded = match *args {
        ContinuationArguments::Json(args) => spec.build_json(args).ok()?,
        ContinuationArguments::Immediate => match spec.fixed_invocation_encoding() {
            Some(encoded) => encoded.clone(),
            None => spec.build_json(&serde_json::Map::new()).ok()?,
        },
        ContinuationArguments::Text(text) => spec.build_text(text)?,
    };
    (encoded.type_id != tilo_actions::resource::INVALID_ARGUMENTS_TYPE).then_some(encoded)
}
pub(crate) fn pure(spec: ActionSpec, handler: &'static str, domain: Parts) -> ActionSpec {
    let builder = spec.clone();
    spec.with_continuation_identity(move |query| match query {
        ContinuationQuery::Draft => Some(evidence(handler, domain.clone(), false)),
        ContinuationQuery::Exact(args) => {
            let encoded = pure_encoding(&builder, &args)?;
            let mut target = domain.clone();
            target.extend([encoded.type_id.into_bytes(), encoded.payload]);
            Some(evidence(handler, target, true))
        }
    })
}

#[cfg(test)]
mod tests {
    use tilo_actions::{ContinuationArguments, ContinuationQuery, Resource};

    #[test]
    fn writer_continuation_read_text_stays_dynamic_and_encodes() {
        let read = crate::semantic_read::action::spec();
        assert!(
            read.continuation_identity(ContinuationQuery::Draft)
                .is_some()
        );
        assert!(
            read.fixed_invocation_encoding().is_none(),
            "typed actions stay dynamic"
        );
        let request = serde_json::json!({"pageSize":1});
        assert!(
            read.continuation_identity(ContinuationQuery::Exact(ContinuationArguments::Json(
                request.as_object().unwrap()
            )))
            .is_some()
        );
        let resource = Resource::new(
            "word-processor.document",
            "word-processor.document",
            "Document",
        )
        .with_action(read);
        let address = resource
            .action_address("read-text")
            .expect("the document publishes its read");
        let encoded =
            tilo_actions::door::encode_json(&[resource], &address, request.as_object().unwrap())
                .unwrap();
        assert_eq!(
            encoded.type_id,
            crate::semantic_read::action::OPERATION_TYPE
        );
    }
}
