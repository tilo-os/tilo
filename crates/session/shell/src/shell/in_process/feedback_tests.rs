use super::*;
use serde_json::json;
use tilo_app::feedback::{
    Destination, Error, FeedbackHost, MaskedScreenshot, Receipt, ReviewText, Submission,
};
use ui::headless::FrameHarness;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const AUTHOR: &str = "feedback-fixture-person";

fn authenticated_startup() -> tilo_desktop::feedback::transport::Startup {
    use tilo_desktop::feedback::transport::{Enrollment, NoCapture, SessionTransport, Startup};
    let device = ed25519_dalek::SigningKey::from_bytes(&[42; 32])
        .verifying_key()
        .to_bytes();
    let admission = Enrollment {
        destination: Destination {
            room: ROOM.into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "feedback".into(),
        },
        owner: tilo_actions::remote::Caller::authenticated([11; 32], device).in_room(ROOM),
        reporter: tilo_actions::remote::Caller::authenticated([12; 32], device).in_room(ROOM),
        permissions: PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Owner)
            .at_unix_time(0),
    };
    let current = admission.clone();
    Startup::new(
        admission,
        Box::new(SessionTransport::new(
            Box::new(move || Ok(current.clone())),
            Box::new(|_, _| Ok(())),
            Box::new(|_| Ok(None)),
        )),
        Box::new(NoCapture),
    )
    .unwrap()
}

#[test]
fn authenticated_startup_binds_only_its_profile_person_and_mounts_private_review() {
    let root = tempfile::tempdir().unwrap();
    let mut wrong = InProcessApps::at(&root.path().join("wrong"), "another-person").unwrap();
    let reporter = authenticated_startup().reporter().clone();
    assert!(matches!(
        wrong.install_feedback_startup(&reporter, authenticated_startup()),
        Err(LaunchError::BindingMismatch)
    ));
    assert!(wrong.feedback.is_none());
    let mut apps =
        InProcessApps::at(&root.path().join("reporter"), &hex::encode([12; 32])).unwrap();
    let wrong_device = tilo_actions::remote::Caller::authenticated(
        [12; 32],
        ed25519_dalek::SigningKey::from_bytes(&[43; 32])
            .verifying_key()
            .to_bytes(),
    )
    .in_room(ROOM);
    assert!(matches!(
        apps.install_feedback_startup(&wrong_device, authenticated_startup()),
        Err(LaunchError::BindingMismatch)
    ));
    assert!(apps.feedback.is_none());
    apps.install_feedback_startup(&reporter, authenticated_startup())
        .unwrap();
    apps.store.flush(None).unwrap();
    assert!(
        apps.install_feedback_startup(&reporter, authenticated_startup())
            .is_err()
    );
    let inputs =
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(root.path(), apps)
            .unwrap();
    let mut frame = mount(&inputs);
    frame.relayout();
    assert!(frame.capture_protected());
    call(
        &mut frame,
        "field-edit",
        &json!({"edit": {"path": [{"field": "issue"}], "op": "set", "value": "The book lost my position"}}),
    )
    .unwrap();
    call(&mut frame, "review", &json!({})).unwrap();
    assert!(frame.capture_protected());
}

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
    fn offers_screenshots(&self) -> bool {
        false
    }
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        // Like the production adapter, nothing is read from the surface.
        Ok(Vec::new())
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
        inputs.in_process().unwrap().borrow_mut().pump_feedback();
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
    inputs.in_process().unwrap().borrow_mut().pump_feedback();
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
        inputs.in_process().unwrap().borrow_mut().pump_feedback();
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

#[test]
fn feedback_opened_from_a_window_is_about_its_app_and_attaches_it_only_with_consent() {
    use crate::shell::window_actions::{Destination as Place, open_hosted};
    use tilo_desktop::feedback::{Focused, composer_tab, focused};
    let root = tempfile::tempdir().unwrap();
    let mut apps = InProcessApps::at(&root.path().join("profile"), AUTHOR).unwrap();
    let owner = destination_owner(&mut apps);
    owner.borrow_mut().offline = false;
    apps.install_feedback(
        owner.borrow().destination.clone(),
        Box::new(AdmittedHost(owner.clone())),
    )
    .unwrap();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_default(),
    );
    chrome.borrow_mut().hosted_inputs =
        crate::shell::hosted_inputs::HostedTileInputs::for_host_directory(root.path(), apps)
            .unwrap();
    open_hosted(
        &mut chrome.borrow_mut(),
        crate::shell::state::AppKind::Other("Notes".into()),
        "Shopping list",
        Place::Beside,
    );
    let Focused::App(notes) = focused(&chrome.borrow().tree().desktop, |_| None) else {
        panic!("the opened window is in front");
    };
    assert_eq!(
        notes.name, "Notes",
        "the app's own name, not the window title"
    );
    crate::shell::feedback_entry::open(&mut chrome.borrow_mut()).unwrap();
    assert_eq!(
        focused(&chrome.borrow().tree().desktop, |_| None),
        Focused::Composer
    );
    let opened = composer_tab(&chrome.borrow().tree().desktop).expect("the composer opened");
    // Asking again from the composer brings back the same one, still about Notes.
    crate::shell::feedback_entry::open(&mut chrome.borrow_mut()).unwrap();
    assert_eq!(composers(&chrome.borrow().tree().desktop), 1);
    assert_eq!(composer_tab(&chrome.borrow().tree().desktop), Some(opened));

    let mut frame = mount(&chrome.borrow().hosted_inputs);
    frame.relayout();
    assert!(frame.inspection_json().unwrap().contains("About Notes"));
    call(
        &mut frame,
        "field-edit",
        &json!({"edit": {"path": [{"field": "issue"}], "op": "replace", "text": "Saving lost my edit; write to jana@example.org"}}),
    )
    .unwrap();
    call(&mut frame, "review", &json!({})).unwrap();
    frame.relayout();
    let without = frame.inspection_json().unwrap();
    assert!(
        !without.contains("App: Notes"),
        "nothing is attached without consent"
    );
    let preview = preview(&frame.inspection().root).expect("the review is shown");
    assert!(
        preview.contains("write to ") && !preview.contains("jana@example.org"),
        "{preview}"
    );
    call(
        &mut frame,
        "field-edit",
        &json!({"edit": {"path": [{"field": "include_context"}], "op": "set", "value": true}}),
    )
    .unwrap();
    call(&mut frame, "review", &json!({})).unwrap();
    frame.relayout();
    assert!(frame.inspection_json().unwrap().contains("App: Notes ("));
    let _approval = call(&mut frame, "submit", &json!({}));
    assert!(frame.wait_until(ui::time::Duration::from_secs(5), |_| {
        chrome
            .borrow()
            .hosted_inputs
            .in_process()
            .unwrap()
            .borrow_mut()
            .pump_feedback();
        !owner.borrow().pending.is_empty()
    }));
    owner.borrow_mut().deliver();
    assert_sent_about_notes(&owner.borrow().sends);
}

/// The reviewed text exactly as the composer shows it for approval.
fn preview(node: &ui::headless::InspectionNode) -> Option<String> {
    if node.test_id.as_deref() == Some("feedback-preview") {
        return node.semantics.as_ref()?.name.clone();
    }
    node.children.iter().find_map(preview)
}

fn composers(desktop: &crate::shell::state::DesktopTree) -> usize {
    desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| tilo_desktop::feedback::is_composer(&tab.tile))
        .count()
}

/// The one report sent: the description masked, the app named, no title.
fn assert_sent_about_notes(sends: &[Submission]) {
    let sent = sends.first().unwrap();
    assert!(!sent.feedback.text.contains("jana@example.org"));
    let context: Vec<_> = sent
        .feedback
        .context
        .iter()
        .map(|field| field.value.as_str())
        .collect();
    assert_eq!(context.len(), 1);
    assert!(context[0].starts_with("App: Notes ("), "{context:?}");
    assert!(
        !serde_json::to_string(sent)
            .unwrap()
            .contains("Shopping list"),
        "the window's title never leaves the device"
    );
}
