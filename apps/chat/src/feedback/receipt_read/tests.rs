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

fn fixture_submission(room: &str) -> Submission {
    Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: Destination {
            room: room.into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "feedback".into(),
        },
        feedback: ReviewedFeedback {
            text: "The reader loses my position".into(),
            context: Vec::new(),
            screenshot: None,
        },
    }
}

fn mounted_fixture() -> (tempfile::TempDir, Submission, ui::headless::FrameHarness) {
    use std::{cell::RefCell, rc::Rc};
    use ui::application::{
        ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    use ui::record::{RecordCheckout, RecordKind, RecordOwners};

    let room = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    let temp = tempfile::tempdir().unwrap();
    let mut owners = RecordOwners::new(temp.path().join("profile"));
    let records = owners.open(crate::definition().app_id, room).unwrap();
    let checkout = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
    let submission = fixture_submission(room);
    records
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            crate::doc::ChatDoc::seed_workspace(
                doc, room, "Fixture", "visitor", "Visitor", "V", 0,
            )?;
            crate::doc::ChatDoc::insert_category(doc, "feedback-category", room, "Feedback");
            crate::doc::ChatDoc::insert_channel(
                doc,
                "feedback",
                room,
                "feedback-category",
                "Feedback",
                crate::model::ChannelKind::Text,
            );
            crate::actions::apply_as(
                doc,
                "visitor",
                &crate::actions::ChatAction::Feedback(crate::feedback::FeedbackAction::Submit {
                    submission: submission.clone(),
                    ts: 123,
                }),
            )
            .map_err(|error| error.to_string())
        })
        .unwrap();
    records.try_flush().unwrap();
    let views = Rc::new(
        ui::application_view::InstanceViewStore::new(temp.path().join("profile"), "device", room)
            .unwrap(),
    );
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let context = HostContext::prepare(
        crate::definition(),
        HostContextInputs {
            records,
            views,
            device_id: "device".into(),
            instance_id: 42,
            permissions: Rc::new(RefCell::new(
                ui::permissions::PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "visitor"),
                    vec![],
                ),
            )),
            author: ui::permissions::UserId("visitor".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    let component = crate::definition()
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let frame = ui::headless::FrameHarness::from_component(
        component,
        (1280, 720),
        ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    (temp, submission, frame)
}

#[test]
fn mounted_chat_reader_returns_the_actual_primary_route_stamp() {
    let (_temp, submission, mut frame) = mounted_fixture();
    let started = frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 42,
                address: tilo_actions::ActionAddress::new(
                    format!("chat.feedback:{}", submission.id),
                    "read-feedback-receipt",
                ),
            },
            &serde_json::Map::new(),
        )
        .unwrap();
    let output = started
        .completed()
        .expect("the registered read must complete immediately");
    let value = output.value.unwrap();
    assert_eq!(value["record"]["roomId"], submission.destination.room);
    assert_eq!(value["record"]["appId"], crate::definition().app_id);
    assert_eq!(value["record"]["recordId"], "");
    let receipt: Receipt = serde_json::from_value(value["content"].clone()).unwrap();
    assert_eq!(receipt.digest, submission.digest().unwrap());
    assert_eq!(receipt.destination, submission.destination);
}
