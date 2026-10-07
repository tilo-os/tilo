use super::*;
use automerge::{ObjType, ROOT, transaction::Transactable};
use ui::feedback::{Destination, Receipt, ReviewedFeedback, Submission};

fn submitted() -> (automerge::AutoCommit, Submission, Resource) {
    let mut doc = automerge::AutoCommit::new();
    crate::doc::ChatDoc::seed_if_empty(&mut doc);
    let channel = crate::doc::ChatDoc::list_channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == crate::model::ChannelKind::Text)
        .unwrap();
    let submission = Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: Destination {
            room: channel.workspace,
            provider: "fixture-chat".into(),
            channel: channel.id,
        },
        feedback: ReviewedFeedback {
            text: "The page loses my position".into(),
            context: Vec::new(),
            screenshot: None,
        },
    };
    crate::actions::apply_as(
        &mut doc,
        "visitor",
        &crate::actions::ChatAction::Feedback(crate::feedback::FeedbackAction::Submit {
            submission: submission.clone(),
            ts: 123,
        }),
    )
    .unwrap();
    doc.commit();
    let report = crate::feedback::report(&doc, &submission.id)
        .unwrap()
        .unwrap();
    (doc, submission, crate::feedback::publish(&report))
}

fn encoded(resource: &Resource) -> EncodedAction {
    tilo_actions::door::encode_json(
        std::slice::from_ref(resource),
        &tilo_actions::ActionAddress::new(resource.id.as_str(), "read-feedback-receipt"),
        &serde_json::Map::new(),
    )
    .unwrap()
}

#[test]
fn receipt_request_is_bound_to_the_published_report_and_has_no_caller_heads() {
    let (_, submission, resource) = submitted();
    let operation = encoded(&resource);
    validate(&resource, &operation.payload).unwrap();
    let mut other = resource.clone();
    other.id = tilo_actions::ResourceId::new("chat.feedback:ffffffffffffffffffffffffffffffff");
    assert_eq!(
        validate(&other, &operation.payload),
        Err(SemanticReadError::InvalidRequest)
    );
    let ambiguous = serde_json::json!({"id":submission.id,"heads":[]});
    assert_eq!(
        validate(&resource, &serde_json::to_vec(&ambiguous).unwrap()),
        Err(SemanticReadError::InvalidRequest)
    );
    assert_eq!(
        validate(&resource, &vec![b' '; MAX_REQUEST_BYTES + 1]),
        Err(SemanticReadError::Limit)
    );
}

#[test]
fn projection_uses_the_committed_basis_and_its_declared_result_contract() {
    let (mut doc, submission, resource) = submitted();
    let operation = encoded(&resource);
    let heads = doc.base_heads();
    // A later replacement cannot change what the earlier committed basis says.
    doc.put_object(ROOT, crate::feedback::REPORTS, ObjType::Map)
        .unwrap();
    doc.commit();
    let page = project(&doc, &heads, &operation.payload).unwrap();
    let receipt: Receipt = serde_json::from_value(page.content.clone()).unwrap();
    assert_eq!(receipt.id, submission.id);
    assert_eq!(receipt.destination, submission.destination);
    assert_eq!(receipt.digest, submission.digest().unwrap());
    assert!(page.next_cursor.is_none());
    assert!(project(&doc, &doc.base_heads(), &operation.payload).is_err());
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == "read-feedback-receipt")
        .unwrap();
    assert!(spec.contract.validate_declaration().is_empty());
    spec.contract
        .result
        .validate(&tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "version":1,
                "record":{"roomId":null,"appId":"so.tilo.chat","recordId":""},
                "basis":{"heads":heads.iter().map(ToString::to_string).collect::<Vec<_>>()},
                "content":page.content,
                "nextCursor":null
            })),
            ..Default::default()
        })
        .unwrap();
}
