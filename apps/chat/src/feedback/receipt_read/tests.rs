use super::*;
use automerge::{ObjType, ROOT, transaction::Transactable};
use tilo_actions::EncodedAction;
use ui::feedback::{Destination, Receipt, ReviewedFeedback, Submission};

fn submitted() -> (automerge::AutoCommit, Submission, Resource) {
    submitted_as("visitor")
}

fn submitted_as(author: &str) -> (automerge::AutoCommit, Submission, Resource) {
    let mut doc = crate::testing::demo_chat();
    let channel = crate::testing::channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == crate::model::ChannelKind::Text)
        .unwrap();
    let submission = Submission {
        id: "0123456789abcdef0123456789abcdef".into(),
        destination: Destination {
            room: "room".into(),
            provider: "fixture-chat".into(),
            channel: channel.id,
        },
        feedback: ReviewedFeedback {
            text: "The page loses my position".into(),
            context: Vec::new(),
            screenshot: None,
            ..Default::default()
        },
    };
    crate::testing::apply_as(
        &mut doc,
        author,
        crate::testing::submit_feedback(&submission),
    )
    .unwrap();
    doc.commit();
    let report = crate::feedback::report(&doc, &submission.id)
        .unwrap()
        .unwrap();
    let resource =
        crate::testing::report_resource(&submission.id, &report.submission.feedback.text);
    (doc, submission, resource)
}

fn encoded(resource: &Resource) -> EncodedAction {
    tilo_actions::door::encode_json(
        std::slice::from_ref(resource),
        &resource
            .action_address(ui::feedback::destination::READ_RECEIPT)
            .expect("a report publishes its receipt read"),
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
    other.id = tilo_actions::ResourceId::new(crate::testing::item(
        "feedback",
        "ffffffffffffffffffffffffffffffff",
    ));
    assert_eq!(
        validate(&other, &operation.payload),
        Err(SemanticReadError::InvalidRequest)
    );
    let ambiguous = serde_json::json!({"id":submission.id,"heads":[]});
    assert_eq!(
        validate(&resource, &serde_json::to_vec(&ambiguous).unwrap()),
        Err(SemanticReadError::InvalidRequest)
    );
}

#[test]
fn projection_uses_the_committed_basis_and_its_declared_result_contract() {
    let (mut doc, submission, resource) = submitted();
    let operation = encoded(&resource);
    let heads = doc.base_heads();
    // A later replacement cannot change what the earlier committed basis says.
    let feedback = ui::tilo_resource::Slot::Key(&ROOT, "feedback");
    feedback.remove(&mut doc).unwrap();
    feedback.object(&mut doc, ObjType::Map).unwrap();
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
            ..Default::default()
        },
    }
}

struct MountedFixture {
    _temp: tempfile::TempDir,
    submission: Submission,
    frame: ui::headless::FrameHarness,
    permissions: std::rc::Rc<std::cell::RefCell<ui::permissions::PermissionContext>>,
    records: ui::record::RecordStore,
}

fn mounted_fixture(author: &str) -> MountedFixture {
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
            crate::testing::apply_as(
                doc,
                "visitor",
                crate::resource::Chat::BEGIN_CHAT.with(crate::record::chat::BeginChat {
                    name: "Visitor".into(),
                    initials: "V".into(),
                    color: 0,
                }),
            )
            .map_err(|error| error.to_string())?;
            crate::testing::insert_category(doc, "feedback-category", "Feedback");
            crate::testing::insert_channel(
                doc,
                "feedback",
                "feedback-category",
                "Feedback",
                crate::model::ChannelKind::Text,
            );
            crate::testing::apply_as(doc, "visitor", crate::testing::submit_feedback(&submission))
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
    let permissions = Rc::new(RefCell::new(ui::permissions::PermissionContext::member(
        author,
        ui::permissions::Role::Owner,
    )));
    let context = HostContext::prepare(
        crate::definition(),
        HostContextInputs {
            records: records.clone(),
            views,
            device_id: "device".into(),
            instance_id: 42,
            permissions: permissions.clone(),
            author: ui::permissions::UserId(author.into()),
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
    MountedFixture {
        _temp: temp,
        submission,
        frame,
        permissions,
        records,
    }
}

#[test]
fn mounted_chat_reader_returns_the_actual_primary_route_stamp() {
    let mut fixture = mounted_fixture("visitor");
    let submission = &fixture.submission;
    let started = fixture
        .frame
        .call_on(
            &crate::testing::item("feedback", &submission.id),
            ui::feedback::destination::READ_RECEIPT,
            serde_json::json!({}),
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

fn maintainer_role(fixture: &MountedFixture, role: ui::permissions::Role) {
    *fixture.permissions.borrow_mut() =
        ui::permissions::PermissionContext::member("maintainer", role);
}

#[test]
fn mounted_triage_observes_live_write_permission_and_keeps_attributed_task_reference() {
    let mut fixture = mounted_fixture("maintainer");
    let investigating = serde_json::json!({"status": "investigating"});
    maintainer_role(&fixture, ui::permissions::Role::Commenter);
    assert!(triage(&mut fixture.frame, &fixture.submission.id, &investigating).is_err());
    let record = fixture.frame.component_record().unwrap().id.clone();
    assert!(
        fixture
            .records
            .read(&record, |doc| {
                crate::feedback::report(doc, &fixture.submission.id)
                    .unwrap()
                    .unwrap()
                    .triage
                    .is_empty()
            })
            .unwrap()
    );
    maintainer_role(&fixture, ui::permissions::Role::Editor);
    assert!(
        triage(&mut fixture.frame, &fixture.submission.id, &investigating)
            .unwrap()
            .is_some()
    );
    let reference =
        tilo_reference::ResourceRef::whole("tasks.task", "fixture-task", "Fixture task");
    let task = serde_json::json!({"status": "task", "reference": reference});
    assert!(
        triage(&mut fixture.frame, &fixture.submission.id, &task)
            .unwrap()
            .is_some()
    );
    fixture.frame.flush_component_records().unwrap();
    let report = fixture
        .records
        .read(&record, |doc| {
            crate::feedback::report(doc, &fixture.submission.id)
                .unwrap()
                .unwrap()
        })
        .unwrap();
    assert_eq!(report.submission, fixture.submission);
    assert_eq!(report.author, "visitor");
    assert_eq!(report.triage.len(), 2);
    assert!(
        report
            .triage
            .iter()
            .all(|entry| entry.author == "maintainer")
    );
    assert_eq!(
        report.triage[1].update,
        crate::feedback::TriageUpdate::Task { reference }
    );
    maintainer_role(&fixture, ui::permissions::Role::Commenter);
    assert!(triage(&mut fixture.frame, &fixture.submission.id, &task).is_err());
    assert_eq!(
        fixture
            .records
            .read(&record, |doc| {
                crate::feedback::report(doc, &fixture.submission.id)
                    .unwrap()
                    .unwrap()
            })
            .unwrap(),
        report
    );
}

#[test]
fn mounted_feedback_status_reads_attributed_triage_with_view_permission() {
    let mut fixture = mounted_fixture("maintainer");
    for status in ["investigating", "resolved"] {
        triage(
            &mut fixture.frame,
            &fixture.submission.id,
            &serde_json::json!({"status": status}),
        )
        .unwrap()
        .unwrap();
    }
    let reference = tilo_reference::ResourceRef::ranged(
        "tasks.task",
        "fixture-task",
        "Fixture task",
        tilo_reference::Span::chars(1, 4),
    );
    triage(
        &mut fixture.frame,
        &fixture.submission.id,
        &serde_json::json!({"status": "task", "reference": reference}),
    )
    .unwrap()
    .unwrap();
    fixture.frame.flush_component_records().unwrap();
    maintainer_role(&fixture, ui::permissions::Role::Viewer);
    let output = fixture
        .frame
        .call_on(
            &crate::testing::item("feedback", &fixture.submission.id),
            "read-feedback-status",
            serde_json::json!({}),
        )
        .expect("a returning viewer must be able to read triage through the public door")
        .completed()
        .unwrap();
    let value = output.value.unwrap();
    assert_eq!(value["content"]["receipt"]["id"], fixture.submission.id);
    assert_eq!(
        value["content"]["receipt"]["digest"],
        fixture.submission.digest().unwrap()
    );
    assert_eq!(value["content"]["triage"].as_array().unwrap().len(), 3);
    assert_eq!(value["content"]["earlierEntries"], 0);
    assert_eq!(
        value["content"]["triage"][0]["update"]["status"],
        "investigating"
    );
    assert_eq!(
        value["content"]["triage"][1]["update"]["status"],
        "resolved"
    );
    assert_eq!(value["content"]["triage"][1]["author"], "maintainer");
    assert_eq!(
        value["content"]["triage"][2]["update"]["reference"],
        serde_json::to_value(reference).unwrap()
    );
}

#[test]
fn canonical_read_requires_the_exact_attributed_reporter_when_requested() {
    let author = "a".repeat(64);
    let (doc, submission, resource) = submitted_as(&author);
    let request = |author: &str| {
        serde_json::to_vec(&serde_json::json!({"id": submission.id, "expectedAuthor": author}))
            .unwrap()
    };
    let payload = request(&author);
    validate(&resource, &payload).unwrap();
    assert!(project(&doc, &doc.base_heads(), &payload).is_ok());
    assert_eq!(
        project(&doc, &doc.base_heads(), &request(&"b".repeat(64))).err(),
        Some(SemanticReadError::InvalidOwner)
    );
    assert_eq!(
        validate(&resource, &request("visitor")),
        Err(SemanticReadError::InvalidRequest)
    );
}

/// Triage the report of `submission` through the public door, as a
/// maintainer's agent does: what it answered.
#[expect(
    clippy::redundant_closure_for_method_calls,
    reason = "the toolkit does not export ResourceOperationStart, so its method cannot be named here"
)]
fn triage(
    frame: &mut ui::headless::FrameHarness,
    submission: &str,
    update: &serde_json::Value,
) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::DoorError> {
    frame
        .call_on(
            &crate::testing::item("feedback", submission),
            crate::resource::Chat::TRIAGE.id,
            serde_json::json!({ "update": update }),
        )
        .map(|started| started.completed())
}
