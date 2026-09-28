//! Real mounted Writer jobs retain their original caller across the chooser.
use super::ScopedAppFixture;
use std::sync::{Arc, Mutex, mpsc};
use tilo_actions::InvocationUpdate;
use ui::automerge::transaction::Transactable;

const SENTINEL: &[u8] = b"existing file must survive an expired caller";

/// A failed assertion must also release the actual chooser worker.
struct ReleasePicker(Option<mpsc::Sender<()>>);
impl ReleasePicker {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for ReleasePicker {
    fn drop(&mut self) {
        self.release();
    }
}

async fn accepted_chooser(fixture: &mut ScopedAppFixture, job: u64, entered: &mpsc::Receiver<()>) {
    let deadline = tilo_test_env::hang_deadline();
    let mut entered_worker = false;
    loop {
        entered_worker |= entered.try_recv().is_ok();
        let status = fixture.invocation_status(job);
        if entered_worker && matches!(status, Some(InvocationUpdate::Accepted { .. })) {
            return;
        }
        assert!(
            status.as_ref().is_none_or(|update| !update.is_terminal()),
            "Writer did not accept its chooser job: {status:?}"
        );
        fixture.pump().await.unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "actual accepted chooser never entered"
        );
    }
}

async fn writer_save_as(expire: bool) {
    let mut fixture = ScopedAppFixture::new("so.tilo.word-processor");
    fixture.wait_action("save-as").await;
    let output = fixture.root.path().join("original-caller.txt");
    std::fs::write(&output, SENTINEL).unwrap();
    let picked = output.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ReleasePicker(Some(release_tx));
    let release_rx = Mutex::new(release_rx);
    let _picker = ui::portal::scoped_picker_fixture(Arc::new(move |_| {
        let _ = entered_tx.send(());
        match release_rx
            .lock()
            .unwrap()
            .recv_timeout(tilo_test_env::hang_bound())
        {
            Ok(()) => ui::portal::PortalOutcome::Opened(picked.clone()),
            Err(error) => {
                ui::portal::PortalOutcome::Failed(format!("chooser test release: {error}"))
            }
        }
    }))
    .unwrap();
    let job = fixture
        .invoke_confirmed("save-as", serde_json::Map::new())
        .await;
    accepted_chooser(&mut fixture, job, &entered_rx).await;
    let record = fixture.record_id();
    let heads = fixture
        .records()
        .read(&record, |doc| doc.base_heads())
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), SENTINEL);
    if expire {
        fixture.expire_original_caller();
    }
    release.release();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        fixture.pump().await.unwrap();
        let status = fixture.invocation_status(job);
        if expire {
            assert_eq!(
                std::fs::read(&output).unwrap(),
                SENTINEL,
                "expired original caller overwrote the chosen file"
            );
            if matches!(status, Some(InvocationUpdate::Failed { .. })) {
                break;
            }
            assert!(
                !matches!(status, Some(InvocationUpdate::Succeeded { .. })),
                "expired original caller reported success"
            );
        } else if std::fs::read(&output).unwrap() != SENTINEL {
            // Actual file export is the control. Native durable publication is
            // independently closed pending its scoped document witness.
            break;
        } else {
            assert!(
                status.as_ref().is_none_or(|update| !update.is_terminal()),
                "live original caller failed before writing: {status:?}"
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Writer did not finish the actual scoped continuation: {status:?}"
        );
    }
    for _ in 0..5 {
        fixture.pump().await.unwrap();
    }
    if expire {
        assert_eq!(std::fs::read(&output).unwrap(), SENTINEL);
        assert!(matches!(
            fixture.invocation_status(job),
            Some(InvocationUpdate::Failed { .. })
        ));
    }
    // SaveAs is an immutable export: this checks isolation, not attempted
    // canonical-mutation denial or a native durable commit receipt.
    assert_eq!(
        fixture
            .records()
            .read(&record, |doc| doc.base_heads())
            .unwrap(),
        heads
    );
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_writer_save_as_valid_original_caller_writes_selected_file() {
    writer_save_as(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_writer_save_as_expired_accepted_caller_cannot_write_selected_file() {
    writer_save_as(true).await;
}

fn read_text(
    resources: &[tilo_actions::Resource],
) -> Result<tilo_actions::EncodedAction, tilo_actions::DoorError> {
    let descriptors = tilo_actions::door::survey(resources);
    let reads: Vec<_> = descriptors
        .iter()
        .filter(|descriptor| {
            descriptor.resource_kind == "word_processor.document"
                && descriptor.address.action == "read-text"
        })
        .collect();
    assert_eq!(
        reads.len(),
        1,
        "actual Writer surface must contain one exact read descriptor"
    );
    tilo_actions::door::encode_json(
        resources,
        &reads[0].address,
        serde_json::json!({"pageSize": 2}).as_object().unwrap(),
    )
}

fn assert_read_encodes(resources: &[tilo_actions::Resource]) {
    let encoded =
        read_text(resources).expect("the registered read encodes through the ordinary action door");
    assert_eq!(
        encoded.type_id,
        word_processor::semantic_read::action::OPERATION_TYPE
    );
}

/// A host worker publishes Writer's declaration-only catalogue schema: it
/// declares the read, and the door refuses to encode it from that tree.
fn assert_read_declared_only(resources: &[tilo_actions::Resource]) {
    assert!(
        matches!(
            read_text(resources),
            Err(tilo_actions::DoorError::InvalidArguments(_))
        ),
        "the worker's catalogue schema must not encode the read"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_writer_catalogue_matches_actual_admitted_document() {
    let mut fixture = ScopedAppFixture::new("so.tilo.word-processor");
    let document = fixture
        .owner
        .peer()
        .document(&fixture.app, &fixture.record)
        .unwrap();
    let document = ui::automerge::AutoCommit::load(&document).unwrap();
    let widget = word_processor::WordProcessorWidget::from_document(&document).unwrap();
    let resources = tilo_actions::ResourceProvider::resources(&widget, &document);
    assert_read_encodes(&resources);
    let mut registry = tilo_actions::ActionRegistry::new();
    word_processor::actions::register(&mut registry);
    let handlers = ui::widget::Widget::resource_operation_handlers(&widget);
    let violations = tilo_actions::validate_surface(&resources, &registry, &handlers);
    eprintln!("actual Writer handlers={handlers:?}, violations={violations:?}");
    assert!(
        violations.is_empty(),
        "actual Writer surface fails its registered handler contract: {violations:?}"
    );
    let declared = tilo_actions::door::survey(&resources);
    eprintln!("admitted Writer resource actions: {:?}", declared);
    assert!(
        resources
            .iter()
            .any(|resource| resource.actions.iter().any(|action| action.id == "save-as")),
        "actual admitted document has no SaveAs resource"
    );
    for _ in 0..100 {
        fixture.pump().await.unwrap();
    }
    for worker in &mut fixture.host.workers {
        if worker.app_id == fixture.app && worker.record_id == fixture.record {
            let app = worker.runner.app_mut();
            assert_read_declared_only(&app.collect_resources());
            eprintln!(
                "mounted Writer resources: {:?}",
                tilo_actions::door::survey(&app.collect_resources())
            );
            eprintln!(
                "mounted Writer control coverage: {:?}",
                app.control_action_coverage()
            );
        }
    }
    fixture.wait_action("save-as").await;
}
