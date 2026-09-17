//! Real accepted backend connected to the production Files registration and host.
use super::*;
use collab_types::files::*;
use std::sync::{Arc, Mutex};
use ui::history_mode::{
    ManagedRoot,
    checked_files::{FileHistoryTarget, FileHistoryTransport},
};

struct Backend {
    fs: Arc<fs::QuickFS>,
    ino: u64,
}
impl FileHistoryTransport for Backend {
    fn read(
        &self,
        query: FileHistoryReadQueryV1,
    ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
        self.fs.read_file_history(self.ino, query)
    }
    fn preview(
        &self,
        scope: FileHistoryReadScopeV1,
        version: collab_types::FileVersionId,
    ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
        self.fs.read_file_history_preview(self.ino, scope, version)
    }
    fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
        self.fs.checked_history_review(self.ino)
    }
    fn submit(&self, request: CheckedHistoryRequestV1) -> CheckedHistoryOutcomeV1 {
        self.fs.checked_history_restore(self.ino, request)
    }
    fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        self.fs.checked_history_result(self.ino, intent)
    }
}
fn wait(driver: &mut ui::headless::FrameHarness, text: &str) {
    assert!(
        driver.wait_until(std::time::Duration::from_secs(5), |driver| driver
            .inspection_json()
            .unwrap()
            .contains(text)),
        "missing {text}: {}",
        driver.inspection_json().unwrap()
    );
}
fn confirm(driver: &mut ui::headless::FrameHarness) {
    assert!(
        driver.wait_until(std::time::Duration::from_secs(5), |driver| driver
            .a11y_node_named("Restore version")
            .is_some())
    );
    driver.key("Enter");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn framework_history_real_backend_review_restore_undo_redo_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
        .await
        .unwrap();
    let content = Arc::new(fs::ContentStore::memory());
    let old_hash = content
        .write_content(b"old exact file content")
        .await
        .unwrap()
        .unwrap();
    let live_hash = content
        .write_content(b"current file content")
        .await
        .unwrap()
        .unwrap();
    let mut namespace = fs::Namespace::new("files-shared-history-test").unwrap();
    let ino = namespace
        .create_file(fs::ROOT_INO, "report.txt", Some(&live_hash), 20, 1)
        .unwrap();
    let record = namespace.record_id_for(ino).unwrap();
    let old = collab_types::FileVersionId(uuid::Uuid::from_u128(101));
    let current = collab_types::FileVersionId(uuid::Uuid::from_u128(102));
    let actor = collab_types::FileActor {
        person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
        device: collab_types::Ed25519PublicKey::from_bytes([2; 32]),
    };
    let history =
        Arc::new(fs::FileHistoryStore::open(directory.path().join("history.json")).unwrap());
    for (id, parents, hash, time, provenance) in [
        (
            old,
            vec![],
            old_hash,
            1,
            collab_types::FileProvenance::Create,
        ),
        (
            current,
            vec![old],
            live_hash,
            2,
            collab_types::FileProvenance::Edit,
        ),
    ] {
        history
            .append(collab_types::FileVersionV1 {
                id,
                record_id: record,
                parents,
                actor,
                timestamp_ms: time,
                content_hash: Some(collab_types::ContentHash(*hash.as_bytes())),
                metadata_hash: collab_types::ContentHash([7; 32]),
                provenance,
            })
            .unwrap();
    }
    let namespace = Arc::new(Mutex::new(namespace));
    let hydrator = Arc::new(fs::Hydrator::new(endpoint.clone(), content));
    let heuristics = Arc::new(fs::Heuristics::new(hydrator.clone(), namespace.clone()));
    let workspace = uuid::Uuid::new_v4();
    let journal = Arc::new(Mutex::new(
        fs::record_transaction::RecordTransactionJournal::open(
            directory.path().join("transactions"),
        )
        .unwrap(),
    ));
    let backend = Arc::new(
        fs::QuickFS::new(namespace, hydrator.clone(), heuristics)
            .with_history(history.clone(), actor)
            .unwrap()
            .with_checked_history_workspace(workspace)
            .with_replacement_transactions(journal, directory.path().join("namespace.am"))
            .unwrap(),
    );
    let root = FileExplorerWidget::new();
    root.framework_history.set_target(Some(FileHistoryTarget {
        binding: uuid::Uuid::new_v4(),
        record,
        label: "report.txt".into(),
        transport: Arc::new(Backend {
            fs: backend.clone(),
            ino,
        }),
    }));
    let registration = root.history_registration().unwrap().unwrap();
    let mut state = ui::State::new();
    use automerge::transaction::Transactable;
    state
        .doc
        .put(automerge::ROOT, "incidental-view-state", "untouched")
        .unwrap();
    state.doc.commit();
    let original_heads = state.doc.get_heads();
    let mut driver = ui::headless::FrameHarness::from_managed_root(
        state,
        ManagedRoot::new(Box::new(root), registration),
        (1100, 750),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    let target = format!("timeline-entry:{}", old.0.simple());
    wait(&mut driver, &target);
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    wait(&mut driver, "old exact file content");
    assert_eq!(driver.state_mut().doc.get_heads(), original_heads);
    driver.click_named("history-restore").unwrap();
    confirm(&mut driver);
    wait(&mut driver, "Restored revision");
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 3);
    assert_eq!(
        driver.state_mut().doc.get_heads(),
        original_heads,
        "file restore must not write incidental State"
    );
    driver.key("Escape");
    driver.click_named("history-files-undo").unwrap();
    confirm(&mut driver);
    assert!(driver.wait_until(std::time::Duration::from_secs(5), |_| {
        history.snapshot(record).unwrap().versions.len() == 4
    }));
    driver.key("Escape");
    wait(&mut driver, "history-files-redo");
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 4);
    driver.click_named("history-files-redo").unwrap();
    confirm(&mut driver);
    assert!(driver.wait_until(std::time::Duration::from_secs(5), |_| {
        history.snapshot(record).unwrap().versions.len() == 5
    }));
    driver.key("Escape");
    wait(&mut driver, "history-files-undo");
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 5);
    driver.click_named("history-back-live").unwrap();
    assert_eq!(driver.state_mut().doc.get_heads(), original_heads);
    let reopened_namespace = Arc::new(Mutex::new(
        fs::Namespace::from_bytes(
            "reopened",
            &std::fs::read(directory.path().join("namespace.am")).unwrap(),
        )
        .unwrap(),
    ));
    let reopened_history =
        Arc::new(fs::FileHistoryStore::open(directory.path().join("history.json")).unwrap());
    let reopened_heuristics = Arc::new(fs::Heuristics::new(
        hydrator.clone(),
        reopened_namespace.clone(),
    ));
    let reopened = fs::QuickFS::new(reopened_namespace, hydrator, reopened_heuristics)
        .without_legacy_record_migration()
        .with_history(reopened_history.clone(), actor)
        .unwrap()
        .with_checked_history_workspace(workspace)
        .with_replacement_transactions(
            Arc::new(Mutex::new(
                fs::record_transaction::RecordTransactionJournal::open(
                    directory.path().join("transactions"),
                )
                .unwrap(),
            )),
            directory.path().join("namespace.am"),
        )
        .unwrap();
    let reopened_ino = reopened
        .namespace
        .lock()
        .unwrap()
        .lookup(fs::ROOT_INO, "report.txt")
        .unwrap()
        .ino
        .0;
    assert_eq!(
        reopened
            .namespace
            .lock()
            .unwrap()
            .record_id_for(reopened_ino),
        Some(record)
    );
    let FileHistoryReadValueV1::Page(page) = reopened
        .read_file_history(reopened_ino, FileHistoryReadQueryV1::Page { cursor: None })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.total_versions, 5);
    assert_eq!(
        reopened
            .read_file_history_preview(reopened_ino, page.scope, old)
            .unwrap(),
        b"old exact file content"
    );
    assert!(reopened.checked_history_review(reopened_ino).is_ok());
    endpoint.close().await;
}

#[test]
fn framework_history_production_registration_rejects_foreign_or_replaced_source_witness() {
    let root = FileExplorerWidget::new();
    let registration = root.history_registration().unwrap().unwrap();
    let foreign = FileExplorerWidget::new();
    assert!(matches!(
        ui::headless::FrameHarness::from_managed_root(
            ui::State::new(),
            ManagedRoot::new(Box::new(foreign), registration),
            (900, 650)
        ),
        Err(ui::history_mode::HistoryRegistrationError::UnsupportedOwner)
    ));
    let mut root = FileExplorerWidget::new();
    let registration = root.history_registration().unwrap().unwrap();
    root.framework_history = ui::history_mode::checked_files::FileHistorySource::default();
    assert!(matches!(
        ui::headless::FrameHarness::from_managed_root(
            ui::State::new(),
            ManagedRoot::new(Box::new(root), registration),
            (900, 650)
        ),
        Err(ui::history_mode::HistoryRegistrationError::UnsupportedOwner)
    ));
}

#[test]
fn framework_history_embedded_files_root_has_shared_entry_and_preserves_incidental_document() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = ui::record::RecordOwners::new(directory.path().to_path_buf());
    let store = owners
        .open("dev.qos.file-explorer", "embedded-history-test")
        .unwrap();
    let checkout = ui::RecordCheckout::new(
        ui::RecordId::for_tab(501),
        ui::RecordKind::new("files-view-state"),
    );
    let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(
        FileExplorerWidget::new(),
    )));
    tile.bind_or_create_record(store, checkout);
    let mut driver = ui::headless::FrameHarness::new(ui::State::new(), Box::new(tile), (1000, 700));
    assert!(driver.targets().contains_key("history-open"));
    let heads = driver.state_mut().doc.get_heads();
    driver.click_named("history-open").unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Select a file with versioned history")
    );
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
}
