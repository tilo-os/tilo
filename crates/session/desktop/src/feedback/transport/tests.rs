use super::*;
use ed25519_dalek::{Signer, SigningKey};
use std::{cell::RefCell, path::Path, process::Command};
use tilo_app::feedback::{Composer, Consent, DeliveryState, MemoryStore, Outbox};
use ui::application::{
    ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::permissions::{Actor, KindSet, Role, UserId, policy};
use ui::record::{RecordCheckout, RecordOwners};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const ID: &str = "0123456789abcdef0123456789abcdef";

fn enrollment() -> Enrollment {
    let owner = SigningKey::from_bytes(&[41; 32]);
    let reporter = SigningKey::from_bytes(&[42; 32]);
    Enrollment {
        destination: Destination {
            room: ROOM.into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "feedback".into(),
        },
        owner: Caller::authenticated([11; 32], owner.verifying_key().to_bytes()).in_room(ROOM),
        reporter: Caller::authenticated([12; 32], reporter.verifying_key().to_bytes())
            .in_room(ROOM),
        permissions: PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Owner)
            .at_unix_time(0),
    }
}

/// `person`, holding no role in the room.
fn no_member(person: &str) -> PermissionContext {
    PermissionContext::new(
        Actor::person(person).in_room(ROOM),
        vec![],
        ui::permissions::Standing::person(
            policy::RoomPolicy::unowned(ROOM, &policy::Subject::Role(Role::Owner)),
            person,
        ),
    )
}

/// A grant to the reporter of `kinds` on submitting feedback, answered `answer`.
fn submit_grant(kinds: KindSet, answer: Answer) -> policy::Grant {
    policy::Grant::new(
        "feedback-submit",
        policy::Subject::person(hex::encode([12; 32])),
        TreePath::room(ROOM).join(&chat_action(
            "channels",
            "feedback",
            Some(destination::SUBMIT_FIELD),
            "submit-feedback",
        )),
        kinds,
        answer,
        policy::Subject::person(hex::encode([11; 32])),
    )
}

/// The feedback contract names what Chat itself publishes: its package and
/// resource kind, the item ids its channels and reports have, and the
/// action that files a report.
#[test]
fn the_feedback_contract_names_what_chat_publishes() {
    use tilo_app::framework::ResourceApp;
    use tilo_app::tilo_resource::Resource;
    assert_eq!(destination::PACKAGE, chat::resource::ChatShell::ID);
    assert_eq!(
        destination::RESOURCE_KIND,
        <chat::resource::Chat as Resource>::KIND
    );
    assert_eq!(
        chat_item(destination::CHANNELS, "feedback"),
        chat::projection::item_id("channels", "feedback")
    );
    assert_eq!(
        chat_item(destination::REPORTS, "report-1"),
        chat::projection::item_id("feedback", "report-1")
    );
    assert!(
        <chat::resource::Chat as Resource>::declarations()
            .iter()
            .any(|declaration| declaration.id == destination::SUBMIT)
    );
}

fn submission() -> Submission {
    Submission {
        id: ID.into(),
        destination: enrollment().destination,
        feedback: tilo_app::feedback::ReviewedFeedback {
            text: "The book lost my position".into(),
            context: vec![],
            screenshot: None,
            ..Default::default()
        },
    }
}

fn session(
    current: Rc<RefCell<Enrollment>>,
    received: Rc<RefCell<Option<SignedReceipt>>>,
    sent: Rc<RefCell<Vec<Submission>>>,
) -> SessionTransport {
    SessionTransport::new(
        Box::new(move || Ok(current.borrow().clone())),
        Box::new(move |target, submission| {
            assert_eq!(target.provider, submission.destination.provider);
            assert_eq!(
                target.address,
                tilo_actions::ActionAddress::new(
                    chat::projection::item_id("channels", "feedback"),
                    "submit-feedback"
                )
            );
            sent.borrow_mut().push(submission.clone());
            Ok(())
        }),
        Box::new(move |_| Ok(received.borrow().clone())),
    )
}

fn startup(current: Enrollment) -> Startup {
    Startup::new(
        current.clone(),
        Box::new(session(
            Rc::new(RefCell::new(current)),
            Rc::new(RefCell::new(None)),
            Rc::new(RefCell::new(vec![])),
        )),
        Box::new(NoCapture),
    )
    .unwrap()
}

fn canonical(profile: &Path) -> (RecordStore, FrameHarness) {
    let admitted = enrollment();
    let mut owners = RecordOwners::new(profile.to_owned());
    let records = owners.open(chat::definition().app_id, ROOM).unwrap();
    let checkout = RecordCheckout::package_primary(chat::definition().record_kind.clone());
    let missing = match records.try_open_record(&checkout) {
        Ok(_) => false,
        Err(ui::record::RecordOpenError::MissingRecord) => true,
        Err(error) => panic!("canonical existing record could not open: {error}"),
    };
    if missing {
        records
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                let reporter = hex::encode([12; 32]);
                let failed = |error: ui::tilo_actions::DoorError| error.to_string();
                chat::demo::start(doc, &reporter, "Reporter", "R").map_err(failed)?;
                chat::demo::insert_category(doc, "category", "Feedback").map_err(failed)?;
                chat::demo::insert_channel(
                    doc,
                    "feedback",
                    "category",
                    "Feedback",
                    chat::model::ChannelKind::Text,
                )
                .map_err(failed)
            })
            .unwrap();
    }
    records.try_flush().unwrap();
    let views = Rc::new(InstanceViewStore::new(profile.to_owned(), "device", ROOM).unwrap());
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let context = HostContext::prepare(
        chat::definition(),
        HostContextInputs {
            records: records.clone(),
            views,
            device_id: "device".into(),
            instance_id: 7,
            permissions: Rc::new(RefCell::new(admitted.permissions)),
            author: UserId(hex::encode([12; 32])),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    let component = chat::definition()
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let frame = FrameHarness::from_component(
        component,
        (1000, 700),
        ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    (records, frame)
}

fn submit(frame: &mut FrameHarness, report: &Submission) {
    frame.relayout();
    frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 7,
                address: tilo_actions::ActionAddress::new(
                    chat::projection::item_id("channels", "feedback"),
                    "submit-feedback",
                ),
            },
            serde_json::json!({"submission": report})
                .as_object()
                .unwrap(),
        )
        .unwrap();
}

struct FixtureSigner {
    owner: Caller,
    key: SigningKey,
}

impl ReceiptSigner for FixtureSigner {
    fn owner(&self) -> &Caller {
        &self.owner
    }
    fn sign(&mut self, request: &ReceiptSigningRequest) -> Result<Vec<u8>, Error> {
        Ok(self.key.sign(request.digest()).to_bytes().to_vec())
    }
}

fn endpoint(records: RecordStore) -> CanonicalDestination {
    CanonicalDestination::new(
        enrollment().owner,
        enrollment().destination,
        CanonicalRecord {
            records,
            record: RecordId::package_primary(),
            project: chat::feedback::canonical_feedback_projection,
            content: None,
        },
        Box::new(FixtureSigner {
            owner: enrollment().owner,
            key: SigningKey::from_bytes(&[41; 32]),
        }),
    )
    .unwrap()
}

fn run_child(root: &Path, phase: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "feedback::transport::tests::canonical_owner_process",
            "--nocapture",
        ])
        .env("TILO_FEEDBACK_PROCESS_FIXTURE", root)
        .env("TILO_FEEDBACK_PROCESS_PHASE", phase)
        .status()
        .unwrap();
    assert!(status.success(), "canonical owner child failed");
}

/// Also exercises the real owner when run alone; no unobservable skip path.
#[test]
fn canonical_owner_process() {
    let temporary = tempfile::tempdir().unwrap();
    let configured = std::env::var_os("TILO_FEEDBACK_PROCESS_FIXTURE");
    let root = configured
        .as_deref()
        .map_or_else(|| temporary.path(), Path::new);
    let phase = std::env::var("TILO_FEEDBACK_PROCESS_PHASE").unwrap_or_else(|_| "write".into());
    let report = if configured.is_some() {
        serde_json::from_slice(&std::fs::read(root.join("approved.json")).unwrap()).unwrap()
    } else {
        submission()
    };
    let (records, mut frame) = canonical(&root.join("destination"));
    if phase == "write" {
        submit(&mut frame, &report);
        records.try_flush().unwrap();
    } else {
        assert_eq!(phase, "read");
    }
    let stored = records
        .read(&RecordId::package_primary(), |doc| {
            let reports = chat::feedback::reports(doc).unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].submission, report);
            assert_eq!(reports[0].author, hex::encode([12; 32]));
            let message = chat::feedback::message_id(ID);
            assert_eq!(
                chat::projection::read(doc)
                    .message_row(&chat::messages::Messages::default(), &message)
                    .map(|shown| shown.channel),
                Some("feedback".to_owned())
            );
            true
        })
        .unwrap();
    assert!(stored);
    let signed = endpoint(records).receipt(&report, &enrollment()).unwrap();
    std::fs::write(root.join("ack.json"), serde_json::to_vec(&signed).unwrap()).unwrap();
}

#[test]
fn reviewed_retry_and_canonical_receipt_survive_both_process_restarts() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("reporter");
    let current = Rc::new(RefCell::new(enrollment()));
    let received = Rc::new(RefCell::new(None));
    let sent = Rc::new(RefCell::new(vec![]));
    let make = || {
        let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(&profile));
        let setup = Startup::new(
            enrollment(),
            Box::new(session(current.clone(), received.clone(), sent.clone())),
            Box::new(NoCapture),
        )
        .unwrap();
        let host = Host::open(store.clone(), setup).unwrap();
        store.flush(None).unwrap();
        let composer = Composer::new(
            enrollment().destination,
            Outbox::open(
                store.clone(),
                StorageKey::package("so.tilo.feedback-test-outbox"),
            )
            .unwrap(),
            Box::new(host),
        )
        .unwrap();
        (store, composer)
    };
    {
        let (store, mut composer) = make();
        composer
            .review(&submission().feedback.text, Consent::default())
            .unwrap();
        composer
            .submit(ID.into(), &submission().feedback.text, Consent::default())
            .unwrap();
        store.flush(None).unwrap();
        composer.pump().unwrap();
        assert_eq!(
            composer.state().unwrap(),
            Some(DeliveryState::AwaitingReceipt)
        );
        std::fs::write(
            root.path().join("approved.json"),
            serde_json::to_vec(&sent.borrow()[0]).unwrap(),
        )
        .unwrap();
        run_child(root.path(), "write");
        // Lost reply: the owner's durable report does not itself change the outbox.
        assert_eq!(
            composer.state().unwrap(),
            Some(DeliveryState::AwaitingReceipt)
        );
    }
    {
        let (store, mut composer) = make();
        composer.pump().unwrap();
        assert_eq!(sent.borrow().len(), 2);
        assert_eq!(sent.borrow()[0], sent.borrow()[1]);
        run_child(root.path(), "write"); // actual retry after a fresh owner process
        run_child(root.path(), "read"); // another fresh owner reads durable storage
        *received.borrow_mut() = Some(
            SignedReceipt::decode(&std::fs::read(root.path().join("ack.json")).unwrap()).unwrap(),
        );
        composer.pump().unwrap();
        store.flush(None).unwrap();
        assert!(matches!(
            composer.state().unwrap(),
            Some(DeliveryState::Sent { .. })
        ));
    }
    let (_, composer) = make();
    // Confirmed outbox remains on disk, and reopening never resends it.
    assert!(composer.pending_ids().any(|id| id == ID));
    assert_eq!(sent.borrow().len(), 2);
}

#[test]
fn canonical_owner_refuses_pending_edits_wrong_owner_and_wrong_reporter() {
    use ui::automerge::{ROOT, transaction::Transactable};
    let root = tempfile::tempdir().unwrap();
    let (records, mut frame) = canonical(root.path());
    let report = submission();
    submit(&mut frame, &report);
    records.try_flush().unwrap();
    let mut service = endpoint(records.clone());
    service.receipt(&report, &enrollment()).unwrap();
    let mut wrong_owner = enrollment();
    wrong_owner.owner = Caller::authenticated(
        [11; 32],
        SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes(),
    )
    .in_room(ROOM);
    assert!(matches!(
        service.receipt(&report, &wrong_owner),
        Err(Error::ReceiptMismatch)
    ));
    let mut wrong_reporter = enrollment();
    wrong_reporter.reporter = Caller::authenticated(
        [15; 32],
        SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes(),
    )
    .in_room(ROOM);
    wrong_reporter.permissions =
        PermissionContext::member_of(ROOM, hex::encode([15; 32]), Role::Owner).at_unix_time(0);
    assert!(matches!(
        service.receipt(&report, &wrong_reporter),
        Err(Error::ReceiptMismatch)
    ));
    records
        .with(&RecordId::package_primary(), |doc| {
            doc.put(ROOT, "newer_unsaved_edit", true).unwrap();
        })
        .unwrap();
    assert!(matches!(
        service.receipt(&report, &enrollment()),
        Err(Error::NotDurable)
    ));
}

#[test]
fn signing_callback_cannot_acknowledge_after_policy_clock_becomes_unavailable() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct RetiringClockSigner {
        signer: FixtureSigner,
        available: Arc<AtomicBool>,
    }
    impl ReceiptSigner for RetiringClockSigner {
        fn owner(&self) -> &Caller {
            self.signer.owner()
        }
        fn sign(&mut self, request: &ReceiptSigningRequest) -> Result<Vec<u8>, Error> {
            let signature = self.signer.sign(request)?;
            self.available.store(false, Ordering::SeqCst);
            Ok(signature)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (records, mut frame) = canonical(root.path());
    let report = submission();
    submit(&mut frame, &report);
    records.try_flush().unwrap();
    let available = Arc::new(AtomicBool::new(true));
    let sampled = available.clone();
    let mut admission = enrollment();
    admission.permissions = admission
        .permissions
        .with_policy_time_sampler(Arc::new(move || {
            if sampled.load(Ordering::SeqCst) {
                Ok(0)
            } else {
                Err("retained owner policy clock unavailable".into())
            }
        }));
    let mut service = CanonicalDestination::new(
        admission.owner.clone(),
        admission.destination.clone(),
        CanonicalRecord {
            records,
            record: RecordId::package_primary(),
            project: chat::feedback::canonical_feedback_projection,
            content: None,
        },
        Box::new(RetiringClockSigner {
            signer: FixtureSigner {
                owner: admission.owner.clone(),
                key: SigningKey::from_bytes(&[41; 32]),
            },
            available: available.clone(),
        }),
    )
    .unwrap();
    assert!(matches!(
        service.receipt(&report, &admission),
        Err(Error::Invalid("feedback policy time unavailable"))
    ));
    assert!(!available.load(Ordering::SeqCst));
}

#[test]
fn canonical_owner_acknowledges_a_screenshot_only_once_it_keeps_the_bytes() {
    let root = tempfile::tempdir().unwrap();
    let (records, mut frame) = canonical(root.path());
    let picture = b"masked report picture".to_vec();
    let mut report = submission();
    report.feedback.screenshot = Some(tilo_app::feedback::ScreenshotRef {
        hash: blake3::hash(&picture).to_hex().to_string(),
        width: 1,
        height: 1,
    });
    submit(&mut frame, &report);
    records.try_flush().unwrap();
    // No content store: the report's metadata alone proves nothing.
    assert!(matches!(
        endpoint(records.clone()).receipt(&report, &enrollment()),
        Err(Error::NotDurable)
    ));
    let content = ui::content::ContentStore::memory();
    let with_content = |content: &ui::content::ContentStore| {
        CanonicalDestination::new(
            enrollment().owner,
            enrollment().destination,
            CanonicalRecord {
                records: records.clone(),
                record: RecordId::package_primary(),
                project: chat::feedback::canonical_feedback_projection,
                content: Some(content.clone()),
            },
            Box::new(FixtureSigner {
                owner: enrollment().owner,
                key: SigningKey::from_bytes(&[41; 32]),
            }),
        )
        .unwrap()
    };
    // Other bytes kept are not this picture.
    content.put(b"some other picture").unwrap();
    assert!(matches!(
        with_content(&content).receipt(&report, &enrollment()),
        Err(Error::NotDurable)
    ));
    content.put(&picture).unwrap();
    let signed = with_content(&content)
        .receipt(&report, &enrollment())
        .unwrap();
    assert_eq!(
        signed.verify(&report, &enrollment()).unwrap().digest,
        report.digest().unwrap()
    );
}

/// A report with a picture reaches its owner before the picture does: the
/// owner takes the picture in from the room only once the report landed.
/// Asked for the receipt before then, the owner answers "not yet" rather
/// than refusing, so the reporter keeps waiting with no error, and the
/// report ends Sent once the owner keeps the picture.
#[test]
fn a_picture_report_ends_sent_once_the_owner_keeps_its_picture() {
    let root = tempfile::tempdir().unwrap();
    let (records, frame) = canonical(root.path());
    let frame = Rc::new(RefCell::new(frame));
    let content = ui::content::ContentStore::memory();
    let picture = b"masked report picture".to_vec();
    let mut report = submission();
    report.feedback.screenshot = Some(tilo_app::feedback::ScreenshotRef {
        hash: blake3::hash(&picture).to_hex().to_string(),
        width: 1,
        height: 1,
    });
    let owner = Rc::new(RefCell::new(
        CanonicalDestination::new(
            enrollment().owner,
            enrollment().destination,
            CanonicalRecord {
                records: records.clone(),
                record: RecordId::package_primary(),
                project: chat::feedback::canonical_feedback_projection,
                content: Some(content.clone()),
            },
            Box::new(FixtureSigner {
                owner: enrollment().owner,
                key: SigningKey::from_bytes(&[41; 32]),
            }),
        )
        .unwrap(),
    ));
    let filed = Rc::new(RefCell::new(0));
    let transport = SessionTransport::new(
        Box::new(|| Ok(enrollment())),
        Box::new({
            let filed = filed.clone();
            move |_, submission| {
                submit(&mut frame.borrow_mut(), submission);
                records.try_flush().unwrap();
                *filed.borrow_mut() += 1;
                Ok(())
            }
        }),
        Box::new({
            let owner = owner.clone();
            move |submission| {
                owner
                    .borrow_mut()
                    .receipt_when_kept(submission, &enrollment())
            }
        }),
    );
    let store: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
    let setup = Startup::new(enrollment(), Box::new(transport), Box::new(NoCapture)).unwrap();
    let host = Host::open(store.clone(), setup).unwrap();
    let mut outbox = Outbox::open(
        store.clone(),
        StorageKey::package("so.tilo.feedback-test-outbox"),
    )
    .unwrap();
    outbox.queue(report.clone()).unwrap();
    let mut composer = Composer::new(enrollment().destination, outbox, Box::new(host)).unwrap();
    composer.resume(ID).unwrap();
    // Asked before the report is filed, and again once it is but before
    // its picture arrived: not yet, and no error either time.
    composer.pump().unwrap();
    assert_eq!(*filed.borrow(), 1);
    composer.pump().unwrap();
    assert_eq!(
        composer.state().unwrap(),
        Some(DeliveryState::AwaitingReceipt)
    );
    content.put(&picture).unwrap();
    // A report changed since review is still refused outright.
    let mut changed = report;
    changed.feedback.text.push_str(" changed");
    assert!(matches!(
        owner
            .borrow_mut()
            .receipt_when_kept(&changed, &enrollment()),
        Err(Error::ReceiptMismatch)
    ));
    composer.pump().unwrap();
    assert!(matches!(
        composer.state().unwrap(),
        Some(DeliveryState::Sent { .. })
    ));
}

#[test]
fn unsigned_wrong_owner_and_wrong_destination_acknowledgments_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let (records, mut frame) = canonical(root.path());
    let report = submission();
    submit(&mut frame, &report);
    records.try_flush().unwrap();
    let valid = endpoint(records).receipt(&report, &enrollment()).unwrap();
    let received = Rc::new(RefCell::new(Some(valid.clone())));
    let current = Rc::new(RefCell::new(enrollment()));
    let setup = Startup::new(
        enrollment(),
        Box::new(session(
            current.clone(),
            received.clone(),
            Rc::new(RefCell::new(vec![])),
        )),
        Box::new(NoCapture),
    )
    .unwrap();
    let mut host = Host::open(Rc::new(MemoryStore::new()), setup).unwrap();
    host.receipt(&report).unwrap().unwrap();
    current.borrow_mut().permissions =
        PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Viewer).at_unix_time(0);
    // Read-only admission can confirm a previous durable report, never resend it.
    host.receipt(&report).unwrap().unwrap();
    assert!(host.send(&report).is_err());
    let mut unsigned = valid.clone();
    unsigned.signature.clear();
    *received.borrow_mut() = Some(unsigned);
    assert!(matches!(host.receipt(&report), Err(Error::ReceiptMismatch)));
    let mut wrong_owner = valid.clone();
    wrong_owner.signature = SigningKey::from_bytes(&[99; 32])
        .sign(
            digest(&enrollment().authorize().unwrap(), &wrong_owner.receipt)
                .unwrap()
                .as_bytes(),
        )
        .to_bytes()
        .to_vec();
    *received.borrow_mut() = Some(wrong_owner);
    assert!(matches!(host.receipt(&report), Err(Error::ReceiptMismatch)));
    let mut other_destination = valid;
    other_destination.receipt.destination.channel = "other".into();
    *received.borrow_mut() = Some(other_destination);
    assert!(matches!(host.receipt(&report), Err(Error::ReceiptMismatch)));
}

#[test]
fn startup_admits_a_commenter_and_refuses_a_viewer_a_stranger_and_replacement() {
    let mut missing = enrollment();
    missing.permissions = no_member(&hex::encode([12; 32]));
    assert!(missing.authorize().is_err());
    // Filing feedback is commenting: a commenter files, a viewer cannot.
    let mut commenter = enrollment();
    commenter.permissions =
        PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Commenter).at_unix_time(0);
    assert!(commenter.authorize().is_ok());
    let mut viewer = enrollment();
    viewer.permissions =
        PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Viewer).at_unix_time(0);
    assert!(viewer.authorize().is_err());
    let mut other_room = enrollment();
    other_room.destination.room = "other".into();
    assert!(other_room.authorize().is_err());
    // A room's owner holds everything; a member's grants can be refused.
    let mut blocked = enrollment();
    blocked.permissions = PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Editor)
        .with_grant(submit_grant(KindSet::ALL, Answer::Deny))
        .at_unix_time(0);
    let mut editor = blocked.clone();
    editor.permissions =
        PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Editor).at_unix_time(0);
    assert!(editor.authorize().is_ok());
    assert!(blocked.authorize().is_err());
    let store: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
    let mut slot = None;
    startup(enrollment())
        .install(store.clone(), &mut slot)
        .unwrap();
    let component = slot
        .as_ref()
        .unwrap()
        .prepare("device", &hex::encode([12; 32]), 42)
        .unwrap();
    let frame = FrameHarness::from_component(
        component,
        (1000, 700),
        ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    assert!(frame.capture_protected());
    assert!(matches!(
        startup(enrollment()).install(store, &mut slot),
        Err(Error::Conflict)
    ));
}

#[test]
fn changed_reporter_before_dispatch_cannot_enqueue_under_another_identity() {
    let current = Rc::new(RefCell::new(enrollment()));
    let admission = current.clone();
    let sends = Rc::new(RefCell::new(0));
    let observed = sends.clone();
    let mut transport = SessionTransport::new(
        Box::new(move || Ok(admission.borrow().clone())),
        Box::new(move |_, _| {
            *observed.borrow_mut() += 1;
            Ok(())
        }),
        Box::new(|_| Ok(None)),
    );
    transport.enrollment().unwrap();
    current.borrow_mut().reporter = Caller::authenticated(
        [15; 32],
        SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes(),
    )
    .in_room(ROOM);
    current.borrow_mut().permissions =
        PermissionContext::member_of(ROOM, hex::encode([15; 32]), Role::Owner);
    assert!(matches!(
        transport.enqueue(&submission()),
        Err(Error::ReceiptMismatch)
    ));
    assert_eq!(*sends.borrow(), 0);
}

#[test]
fn ordinary_projection_json_and_oversized_acknowledgments_cannot_be_promoted() {
    let receipt = Receipt {
        id: ID.into(),
        digest: submission().digest().unwrap(),
        destination: enrollment().destination,
        report: format!("chat.message:m_feedback_{ID}"),
    };
    assert!(SignedReceipt::decode(&serde_json::to_vec(&receipt).unwrap()).is_err());
    assert!(matches!(
        SignedReceipt::decode(&vec![0; 4097]),
        Err(Error::Full)
    ));
}

#[test]
fn expired_snapshot_grants_and_failed_host_clocks_cannot_authorize_send() {
    let now = tilo_clock::try_unix_now().unwrap();
    assert!(now > 0, "execution clock must be observable");
    let mut expired = enrollment();
    expired.permissions = PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Viewer)
        .with_grant(
            submit_grant(KindSet::of(ActionKind::Undoable), Answer::Allow)
                .with_lifetime(policy::Lifetime::Until(now.saturating_mul(1000))),
        )
        .at_unix_time(now.saturating_sub(1));
    assert!(expired.authorize().is_err());
    let mut unavailable = enrollment();
    unavailable.permissions =
        unavailable
            .permissions
            .with_policy_time_sampler(std::sync::Arc::new(|| {
                Err("retained host clock unavailable".into())
            }));
    assert!(unavailable.authorize().is_err());
    assert!(unavailable.authorize_receipt(&submission()).is_err());
}

#[test]
fn persisted_owner_pin_and_current_role_survive_reconnect_without_reauthorization() {
    let store: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
    let current = Rc::new(RefCell::new(enrollment()));
    let setup = Startup::new(
        enrollment(),
        Box::new(session(
            current.clone(),
            Rc::new(RefCell::new(None)),
            Rc::new(RefCell::new(vec![])),
        )),
        Box::new(NoCapture),
    )
    .unwrap();
    let mut host = Host::open(store.clone(), setup).unwrap();
    current.borrow_mut().permissions =
        PermissionContext::member_of(ROOM, hex::encode([12; 32]), Role::Viewer);
    assert!(host.send(&submission()).is_err());
    drop(host);
    let mut changed = enrollment();
    changed.owner = Caller::authenticated(
        [11; 32],
        SigningKey::from_bytes(&[99; 32]).verifying_key().to_bytes(),
    )
    .in_room(ROOM);
    assert!(matches!(
        Host::open(store, startup(changed)),
        Err(Error::ReceiptMismatch)
    ));
}

#[test]
fn retained_receipt_source_cannot_substitute_report_identity_or_destination() {
    struct CorruptSource {
        canonical: CanonicalRecord,
        change_destination: bool,
    }
    impl CanonicalReceiptSource for CorruptSource {
        fn namespace(&self) -> (String, String) {
            self.canonical.namespace()
        }
        fn current_report(&self, submission: &Submission) -> Result<(Receipt, String), Error> {
            let (mut receipt, author) = self.canonical.current_report(submission)?;
            if self.change_destination {
                receipt.destination.channel = "foreign-channel".into();
            } else {
                receipt.id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
            }
            Ok((receipt, author))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (records, mut frame) = canonical(root.path());
    let report = submission();
    submit(&mut frame, &report);
    records.try_flush().unwrap();
    for change_destination in [false, true] {
        let source = CorruptSource {
            canonical: CanonicalRecord {
                records: records.clone(),
                record: RecordId::package_primary(),
                project: chat::feedback::canonical_feedback_projection,
                content: None,
            },
            change_destination,
        };
        let mut service = CanonicalDestination::from_source(
            enrollment().owner,
            enrollment().destination,
            Box::new(source),
            Box::new(FixtureSigner {
                owner: enrollment().owner,
                key: SigningKey::from_bytes(&[41; 32]),
            }),
        )
        .unwrap();
        assert!(matches!(
            service.receipt(&report, &enrollment()),
            Err(Error::ReceiptMismatch)
        ));
    }
}
