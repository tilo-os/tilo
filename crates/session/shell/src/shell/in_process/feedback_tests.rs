use super::*;
use serde_json::json;
use tilo_app::feedback::{
    Destination, Error, FeedbackHost, MaskedScreenshot, Receipt, ReviewText, Submission,
};
use ui::headless::FrameHarness;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const AUTHOR: &str = "feedback-fixture-person";

struct DestinationOwner {
    frame: FrameHarness,
    records: ui::record::RecordStore,
    destination: Destination,
    offline: bool,
    receipts: bool,
    sends: Vec<Submission>,
    pending: Vec<Submission>,
}

impl DestinationOwner {
    // Transport delivery runs on its own host turn, never re-entering a
    // different record from inside the private composer's effect dispatch.
    fn deliver(&mut self) {
        for submission in std::mem::take(&mut self.pending) {
            self.frame.relayout();
            self.frame
                .invoke_bound_action(
                    &ui::bound_dispatch::BoundActionAddress {
                        instance_id: 7,
                        address: tilo_actions::ActionAddress::new(
                            chat::projection::item_id("channels", "feedback"),
                            "submit-feedback",
                        ),
                    },
                    json!({"submission":submission}).as_object().unwrap(),
                )
                .unwrap();
            self.records.try_flush().unwrap();
            self.sends.push(submission);
        }
    }
}

struct AdmittedHost(Rc<RefCell<DestinationOwner>>);
impl FeedbackHost for AdmittedHost {
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Err(Error::CaptureProtected)
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        Err(Error::CaptureProtected)
    }
    fn persist_screenshot(&mut self, _: &MaskedScreenshot) -> Result<(), Error> {
        Err(Error::CaptureProtected)
    }
    fn send(&mut self, submission: &Submission) -> Result<(), Error> {
        let mut owner = self.0.borrow_mut();
        if owner.offline {
            return Err(Error::Invalid("transport unavailable"));
        }
        if submission.destination != owner.destination {
            return Err(Error::ReceiptMismatch);
        }
        owner.pending.push(submission.clone());
        Ok(())
    }
    fn receipt(&mut self, submission: &Submission) -> Result<Option<Receipt>, Error> {
        let owner = self.0.borrow();
        if !owner.receipts {
            return Ok(None);
        }
        if owner.records.read(&RecordId::package_primary(), |doc| {
            chat::feedback::report(doc, &submission.id)
                .unwrap()
                .is_some()
        }) != Some(true)
        {
            return Ok(None);
        }
        chat::feedback::receipt_from_record(
            &owner.records,
            &RecordId::package_primary(),
            &submission.id,
            &owner.destination,
        )
        .map(Some)
        .map_err(|_| Error::Invalid("canonical receipt refused"))
    }
}

fn destination_owner(apps: &mut InProcessApps) -> Rc<RefCell<DestinationOwner>> {
    let records = apps.owners.open(chat::definition().app_id, ROOM).unwrap();
    let checkout =
        ui::record::RecordCheckout::package_primary(chat::definition().record_kind.clone());
    records
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            let failed = |error: ui::tilo_actions::DoorError| error.to_string();
            chat::demo::start_in(doc, AUTHOR, (ROOM, "Private fixture"), "Fixture", "F")
                .map_err(failed)?;
            chat::demo::insert_category(doc, AUTHOR, "feedback-category", ROOM, "Feedback")
                .map_err(failed)?;
            chat::demo::insert_channel(
                doc,
                AUTHOR,
                "feedback",
                (ROOM, "feedback-category"),
                "Feedback",
                chat::model::ChannelKind::Text,
            )
            .map_err(failed)
        })
        .unwrap();
    records.try_flush().unwrap();
    let component = apps
        .mount(chat::definition(), ROOM, 7, checkout.id, |_| {})
        .unwrap();
    let frame = FrameHarness::from_component(
        component,
        (1000, 700),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    Rc::new(RefCell::new(DestinationOwner {
        frame,
        records,
        destination: Destination {
            room: ROOM.into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "feedback".into(),
        },
        offline: true,
        receipts: false,
        sends: Vec::new(),
        pending: Vec::new(),
    }))
}

fn call(
    frame: &mut FrameHarness,
    action: &str,
    arguments: &serde_json::Value,
) -> Result<(), tilo_actions::DoorError> {
    frame.relayout();
    let resource = frame
        .resources()
        .into_iter()
        .find(|r| r.kind == "tilo.feedback-draft")
        .unwrap();
    frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 42,
                address: tilo_actions::ActionAddress::new(resource.id.0, action),
            },
            arguments.as_object().unwrap(),
        )
        .map(|_| ())
}

fn mount(inputs: &crate::shell::hosted_inputs::HostedTileInputs) -> FrameHarness {
    let tab = crate::shell::state::Tab {
        id: 42,
        title: "Feedback".into(),
        tile: crate::shell::state::TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::Other("Feedback".into()),
            label: "Feedback".into(),
        },
    };
    let request = tilo_desktop::resolver::TileRequest {
        room: crate::shell::state::RoomId::new(),
        tab: &tab,
    };
    let Some(tilo_desktop::resolver::TileResolution::App(component, collaboration)) =
        crate::shell::mount_in_process(&request, inputs)
    else {
        panic!("native feedback entry must mount");
    };
    assert!(
        collaboration.is_none(),
        "the private draft must have no broker route"
    );
    FrameHarness::from_component(
        component,
        (1000, 700),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap()
}

/// The composer's field edit that includes the screenshot or leaves it out.
fn screenshot(include: bool) -> serde_json::Value {
    json!({"edit": {"path": [{"field": "include_screenshot"}], "op": "set", "value": include}})
}

#[test]
fn native_private_feedback_reopens_approved_outbox_and_reads_actual_canonical_receipt() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    let mut apps = InProcessApps::at(&profile, AUTHOR).unwrap();
    let owner = destination_owner(&mut apps);
    apps.install_feedback(
        owner.borrow().destination.clone(),
        Box::new(AdmittedHost(owner.clone())),
    )
    .unwrap();
    let inputs =
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(root.path(), apps)
            .unwrap();
    assert!(
        inputs
            .native_apps()
            .iter()
            .any(|app| app.package_id == tilo_desktop::feedback::PACKAGE)
    );
    let mut frame = mount(&inputs);
    frame.relayout();
    assert!(frame.capture_protected());
    call(
        &mut frame,
        "field-edit",
        &json!({"edit": {"path": [{"field": "issue"}], "op": "replace", "text": "The book failed; token=secret12345"}}),
    )
    .unwrap();
    assert!(call(&mut frame, "submit", &json!({})).is_err());
    call(&mut frame, "field-edit", &screenshot(true)).unwrap();
    assert!(call(&mut frame, "review", &json!({})).is_err());
    assert!(owner.borrow().sends.is_empty());
    call(&mut frame, "field-edit", &screenshot(false)).unwrap();
    call(&mut frame, "review", &json!({})).unwrap();
    // Approval may still be Saving when the action returns. Observe the
    // production controller's later transport refusal after local durability.
    let _approval = call(&mut frame, "submit", &json!({}));
    assert!(frame.wait_until(ui::time::Duration::from_secs(5), |frame| {
        inputs.in_process().unwrap().borrow().pump_feedback();
        frame
            .inspection_json()
            .unwrap()
            .contains("Could not finish")
    }));
    assert!(
        !frame
            .inspection_json()
            .unwrap()
            .contains("Feedback received")
    );
    assert!(
        owner.borrow().sends.is_empty(),
        "offline approval must remain in the durable private outbox"
    );
    frame.close_components();
    drop(frame);
    drop(inputs);

    let mut restored = InProcessApps::at(&profile, AUTHOR).unwrap();
    restored
        .install_feedback(
            owner.borrow().destination.clone(),
            Box::new(AdmittedHost(owner.clone())),
        )
        .unwrap();
    let inputs =
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(root.path(), restored)
            .unwrap();
    let mut frame = mount(&inputs);
    frame.relayout();
    assert!(!frame.inspection_json().unwrap().contains("secret12345"));
    owner.borrow_mut().offline = false;
    inputs.in_process().unwrap().borrow().pump_feedback();
    call(&mut frame, "retry", &json!({})).unwrap();
    owner.borrow_mut().deliver();
    let submission = owner.borrow().sends.first().unwrap().clone();
    assert!(!submission.feedback.text.contains("secret12345"));
    assert!(submission.feedback.context.is_empty());
    assert!(submission.feedback.screenshot.is_none());
    assert!(owner.borrow().sends.iter().all(|sent| sent == &submission));
    frame.relayout();
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("Waiting for destination confirmation")
    );
    owner.borrow_mut().receipts = true;
    assert!(frame.wait_until(ui::time::Duration::from_secs(5), |frame| {
        inputs.in_process().unwrap().borrow().pump_feedback();
        frame
            .inspection_json()
            .unwrap()
            .contains("Feedback received")
    }));
    assert_canonical_report(&owner.borrow(), &submission);
}

fn assert_canonical_report(owner: &DestinationOwner, submission: &Submission) {
    let report = owner
        .records
        .read(&RecordId::package_primary(), |doc| {
            assert_eq!(chat::feedback::reports(doc).unwrap().len(), 1);
            assert_eq!(
                chat::projection::read(doc).message_rows("feedback").len(),
                1
            );
            chat::feedback::report(doc, &submission.id)
                .unwrap()
                .unwrap()
        })
        .unwrap();
    assert_eq!(report.author, AUTHOR);
    assert_eq!(&report.submission, submission);
}

#[test]
fn unconfigured_native_feedback_cannot_review_send_or_create_shared_drafts() {
    let root = tempfile::tempdir().unwrap();
    let apps = InProcessApps::at(root.path(), AUTHOR).unwrap();
    let inputs =
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(root.path(), apps)
            .unwrap();
    let mut frame = mount(&inputs);
    call(
        &mut frame,
        "field-edit",
        &json!({"edit": {"path": [{"field": "issue"}], "op": "replace", "text": "Unconfigured private draft"}}),
    )
    .unwrap();
    assert!(call(&mut frame, "review", &json!({})).is_err());
    assert!(call(&mut frame, "submit", &json!({})).is_err());
    frame.relayout();
    assert!(frame.capture_protected());
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("Feedback is not configured")
    );
    assert!(inputs.in_process().unwrap().borrow().views.is_empty());
}
