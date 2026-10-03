//! Real accepted backend connected to the production Files registration and host.
use super::*;
use collab_types::files::*;
use std::sync::{Arc, Mutex};
use ui::history_mode::{
    ManagedRoot,
    checked_files::{FileHistoryTarget, FileHistoryTransport},
};

/// Submits a transport returned. A checked restore is durable backend work:
/// the accepted backend commits a phased, fsynced transaction and the mounted
/// projection's setxattr joins a filesystem transaction, and on a contended
/// disk either can outlast a frame deadline. Tests wait for the backend here
/// first and then hold the UI to its deadline.
#[derive(Default)]
struct SubmitReturns(std::sync::atomic::AtomicUsize);
impl SubmitReturns {
    fn record(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Drive frames until `count` submits returned.
    fn wait(&self, driver: &mut ui::headless::FrameHarness, count: usize) {
        assert!(
            driver.wait_until(tilo_test_env::hang_bound(), |_| self.count() >= count),
            "the backend returned {} of {count} submits",
            self.count()
        );
    }
}

struct Backend {
    fs: Arc<fs::QuickFS>,
    ino: u64,
    hide_first_ack: std::sync::atomic::AtomicBool,
    submitted: Mutex<Vec<CheckedHistoryRequestV1>>,
    lookups: Mutex<Vec<uuid::Uuid>>,
    returned: SubmitReturns,
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
        self.submitted.lock().unwrap().push(request);
        let actual = self.fs.checked_history_restore(self.ino, request);
        self.returned.record();
        if self
            .hide_first_ack
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            assert!(matches!(actual, CheckedHistoryOutcomeV1::Applied { .. }));
            CheckedHistoryOutcomeV1::Unavailable
        } else {
            actual
        }
    }
    fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        self.lookups.lock().unwrap().push(intent);
        self.fs.checked_history_result(self.ino, intent)
    }
}
/// A production transport whose submits are counted, nothing else changed.
struct Counted {
    inner: Arc<dyn FileHistoryTransport>,
    returned: Arc<SubmitReturns>,
}
impl FileHistoryTransport for Counted {
    fn read(
        &self,
        query: FileHistoryReadQueryV1,
    ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
        self.inner.read(query)
    }
    fn preview(
        &self,
        scope: FileHistoryReadScopeV1,
        version: collab_types::FileVersionId,
    ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
        self.inner.preview(scope, version)
    }
    fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
        self.inner.review()
    }
    fn submit(&self, request: CheckedHistoryRequestV1) -> CheckedHistoryOutcomeV1 {
        let outcome = self.inner.submit(request);
        self.returned.record();
        outcome
    }
    fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        self.inner.result(intent)
    }
}
/// The session's report for a submit or lookup that found no receipt.
const UNAVAILABLE_ACK: &str = "Restore acknowledgement is unavailable";
fn wait(driver: &mut ui::headless::FrameHarness, text: &str) {
    assert!(
        driver.wait_until(tilo_test_env::hang_bound(), |driver| driver
            .inspection_json()
            .unwrap()
            .contains(text)),
        "missing {text}: {}",
        driver.inspection_json().unwrap()
    );
}
fn confirm(driver: &mut ui::headless::FrameHarness, phase: &str) {
    assert!(
        driver.wait_until(tilo_test_env::hang_bound(), |driver| driver
            .a11y_node_named("Restore version")
            .is_some()),
        "confirmation missing in {phase}: {}",
        driver.inspection_json().unwrap()
    );
    driver.key("Enter");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end restore/undo/redo/reopen scenario reads top to bottom"
)]
async fn framework_history_real_backend_uncertain_result_restore_undo_redo_and_reopen() {
    use automerge::transaction::Transactable;
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
    let heuristics = Arc::new(fs::Heuristics::new(hydrator.clone()));
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
    let transport = Arc::new(Backend {
        fs: backend.clone(),
        ino,
        hide_first_ack: std::sync::atomic::AtomicBool::new(true),
        submitted: Mutex::new(vec![]),
        lookups: Mutex::new(vec![]),
        returned: SubmitReturns::default(),
    });
    let root = FileExplorerWidget::new();
    root.framework_history.set_target(Some(FileHistoryTarget {
        binding: uuid::Uuid::new_v4(),
        record,
        label: "report.txt".into(),
        transport: transport.clone(),
    }));
    let registration = ui::history_mode::capability::history_registration(&root)
        .unwrap()
        .unwrap();
    let mut state = ui::State::new();
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
    confirm(&mut driver, "backend restore");
    // Confirming relabels the dialog at once; the submit runs in the
    // background. Wait for it, then for its (deliberately hidden)
    // acknowledgement to reach the session.
    wait(&mut driver, "Refresh review");
    transport.returned.wait(&mut driver, 1);
    wait(&mut driver, UNAVAILABLE_ACK);
    let submitted = transport.submitted.lock().unwrap().clone();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        history.snapshot(record).unwrap().versions.len(),
        3,
        "backend committed despite hidden acknowledgement"
    );
    driver.key("Escape");
    driver.click_named("history-files-result").unwrap();
    wait(&mut driver, "Restored revision");
    assert_eq!(
        *transport.lookups.lock().unwrap(),
        vec![submitted[0].intent_id],
        "host queries the actual accepted backend with exactly its unresolved intent"
    );
    assert_eq!(
        *transport.submitted.lock().unwrap(),
        submitted,
        "result observation must not submit a replacement operation"
    );
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 3);
    assert_eq!(
        driver.state_mut().doc.get_heads(),
        original_heads,
        "file restore must not write incidental State"
    );
    driver.key("Escape");
    driver.click_named("history-files-undo").unwrap();
    confirm(&mut driver, "backend undo");
    transport.returned.wait(&mut driver, 2);
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 4);
    driver.key("Escape");
    wait(&mut driver, "history-files-redo");
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 4);
    driver.click_named("history-files-redo").unwrap();
    confirm(&mut driver, "backend redo");
    transport.returned.wait(&mut driver, 3);
    assert_eq!(history.snapshot(record).unwrap().versions.len(), 5);
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
    let reopened_heuristics = Arc::new(fs::Heuristics::new(hydrator.clone()));
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

/// Production registration rejects foreign witnesses, the embedded root shares an entry, and round-1 targets recover sessions after A/B/A.
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end A/B/A session recovery scenario reads top to bottom"
)]
#[test]
fn framework_history_production_registration_and_session_recovery() {
    // --- phase: framework_history_production_registration_rejects_foreign_or_replaced_source_witness ---
    {
        let root = FileExplorerWidget::new();
        let registration = ui::history_mode::capability::history_registration(&root)
            .unwrap()
            .unwrap();
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
        let registration = ui::history_mode::capability::history_registration(&root)
            .unwrap()
            .unwrap();
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

    // --- phase: framework_history_embedded_files_root_has_shared_entry_and_preserves_incidental_document ---
    {
        let directory = tempfile::tempdir().unwrap();
        let mut owners = ui::record::RecordOwners::new(directory.path().to_path_buf());
        let store = owners
            .open("so.tilo.file-explorer", "embedded-history-test")
            .unwrap();
        let checkout = ui::RecordCheckout::new(
            ui::RecordId::for_tab(501),
            ui::RecordKind::new("files-view-state"),
        );
        let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(
            FileExplorerWidget::new(),
        )));
        tile.bind_or_create_record(store, checkout);
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(tile), (1000, 700));
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

    // --- phase: round1_production_targets_recover_pending_and_applied_session_after_a_b_a ---
    // Ordinary xattrs exercise production target construction/session routing,
    // not backend authority. The separate `QuickFS` fixture proves real operations.
    {
        fn install(
            path: &std::path::Path,
            record: u128,
        ) -> (collab_types::FileRecordId, collab_types::FileVersionId) {
            let record_id = collab_types::FileRecordId(uuid::Uuid::from_u128(record));
            let id = collab_types::FileVersionId(uuid::Uuid::from_u128(record + 100));
            std::fs::write(path, b"live file").unwrap();
            let scope = FileHistoryReadScopeV1 {
                workspace_id: uuid::Uuid::from_u128(1),
                mount_lifetime: uuid::Uuid::from_u128(2),
                record_id,
                frontier: [record as u8; 32],
            };
            let version = collab_types::FileVersionV1 {
                id,
                record_id,
                parents: vec![],
                actor: collab_types::FileActor {
                    person: collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                    device: collab_types::Ed25519PublicKey::from_bytes([2; 32]),
                },
                timestamp_ms: 1,
                content_hash: Some(collab_types::ContentHash([5; 32])),
                metadata_hash: collab_types::ContentHash([6; 32]),
                provenance: collab_types::FileProvenance::Create,
            };
            let page = FileHistoryReadValueV1::Page(FileHistoryPageV1 {
                scope,
                versions: vec![version.clone()],
                total_versions: 1,
                heads: vec![id],
                current_revision: None,
                next_after: None,
            });
            xattr::set(
                path,
                HISTORY_READ_XATTR,
                &serde_json::to_vec(&FileHistoryReadResponseV1 {
                    schema_version: 1,
                    result: Ok(page),
                })
                .unwrap(),
            )
            .unwrap();
            xattr::set(
                path,
                FileHistoryReadQueryV1::Version {
                    frontier: scope.frontier,
                    version_id: id,
                }
                .xattr_name(),
                &serde_json::to_vec(&FileHistoryReadResponseV1 {
                    schema_version: 1,
                    result: Ok(FileHistoryReadValueV1::Version { scope, version }),
                })
                .unwrap(),
            )
            .unwrap();
            xattr::set(
                path,
                FileHistoryReadQueryV1::Preview {
                    frontier: scope.frontier,
                    version_id: id,
                }
                .xattr_name(),
                b"historical bytes",
            )
            .unwrap();
            xattr::set(
                path,
                CHECKED_HISTORY_REVIEW_XATTR,
                &serde_json::to_vec(&CheckedHistoryReviewResponseV1 {
                    schema_version: 1,
                    result: Ok(FileRestoreReviewV1 {
                        schema_version: 1,
                        workspace_id: scope.workspace_id,
                        mount_lifetime: scope.mount_lifetime,
                        record_id,
                        frontier: scope.frontier,
                    }),
                })
                .unwrap(),
            )
            .unwrap();
            (record_id, id)
        }
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("A");
        let b = directory.path().join("B");
        let (record_a, version_a) = install(&a, 3);
        let (record_b, version_b) = install(&b, 4);
        let returned = Arc::new(SubmitReturns::default());
        let target = |path: &std::path::Path, record, label: &str| {
            let target = crate::framework_history::target(path, record, label.into()).unwrap();
            FileHistoryTarget {
                transport: Arc::new(Counted {
                    inner: target.transport.clone(),
                    returned: returned.clone(),
                }),
                ..target
            }
        };
        let root = FileExplorerWidget::new();
        let source = root.framework_history.clone();
        source.set_target(Some(target(&a, record_a, "A")));
        let registration = ui::history_mode::capability::history_registration(&root)
            .unwrap()
            .unwrap();
        let mut driver = ui::headless::FrameHarness::from_managed_root(
            ui::State::new(),
            ManagedRoot::new(Box::new(root), registration),
            (1100, 750),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let select = format!("timeline-entry:{}", version_a.0.simple());
        wait(&mut driver, &select);
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&select),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        wait(&mut driver, "historical bytes");
        source.open(Some(version_a));
        driver.relayout();
        wait(&mut driver, "historical bytes");
        driver.click_named("history-restore").unwrap();
        confirm(
            &mut driver,
            "production initial restore after repeated open",
        );
        // Confirming relabels the dialog at once; the submit that writes the
        // request runs in the background and settles with the missing receipt.
        wait(&mut driver, "Refresh review");
        returned.wait(&mut driver, 1);
        wait(&mut driver, UNAVAILABLE_ACK);
        driver.key("Escape");
        let original: CheckedHistoryRequestV1 = serde_json::from_slice(
            &xattr::get(&a, CHECKED_HISTORY_RESTORE_XATTR)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        // Model the projection consuming the request. A retry must republish the
        // controller's retained exact intent even when the transport no longer
        // exposes the earlier write.
        xattr::remove(&a, CHECKED_HISTORY_RESTORE_XATTR).unwrap();
        source.set_target(Some(target(&b, record_b, "B")));
        wait(
            &mut driver,
            &format!("timeline-entry:{}", version_b.0.simple()),
        );
        source.set_target(Some(target(&a, record_a, "A again")));
        // The result button survives target changes. Wait for A's immutable
        // timeline entry instead so the A -> B -> A reconciliation has completed
        // before the retry command is produced; commands from the superseded B
        // presentation are deliberately discarded by the host.
        wait(&mut driver, &select);
        assert!(
            !driver.inspection_json().unwrap().contains(UNAVAILABLE_ACK),
            "the reloaded timeline replaced the earlier report, so the next one is the retry's"
        );
        // This case exercises exact session routing across descriptor swaps, not
        // button hit-testing. Activate the semantic control directly so the
        // controller's background completions cannot rebuild the toolbar
        // between `click_named` resolving its rect and dispatching the pointer.
        driver
            .perform_semantic_named("history-files-retry", None, &ui::a11y::Action::Activate)
            .unwrap();
        // The retry writes the request and then looks the receipt up. Let its
        // report reach the session so it cannot land on the steps below.
        returned.wait(&mut driver, 2);
        wait(&mut driver, UNAVAILABLE_ACK);
        let retry: CheckedHistoryRequestV1 = serde_json::from_slice(
            &xattr::get(&a, CHECKED_HISTORY_RESTORE_XATTR)
                .unwrap()
                .expect("the retry rewrote the checked restore request"),
        )
        .unwrap();
        assert_eq!(retry, original);
        wait(&mut driver, &select);
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&select),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        wait(&mut driver, "historical bytes");
        driver.click_named("history-restore").unwrap();
        confirm(&mut driver, "production replacement-intent refusal");
        wait(&mut driver, "An earlier restore is still pending");
        driver.key("Escape");
        assert_eq!(
            serde_json::from_slice::<CheckedHistoryRequestV1>(
                &xattr::get(&a, CHECKED_HISTORY_RESTORE_XATTR)
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            original
        );
        let revision = |id| collab_types::ContentRevisionV1 {
            schema_version: 1,
            record_id: record_a,
            version_id: collab_types::FileVersionId(uuid::Uuid::from_u128(id)),
            content_hash: Some(collab_types::ContentHash([5; 32])),
            byte_len: 16,
            metadata_hash: collab_types::ContentHash([6; 32]),
        };
        let receipt = FileRestoreReceiptV1 {
            schema_version: 1,
            intent_id: original.intent_id,
            workspace_id: original.review.workspace_id,
            mount_lifetime: original.review.mount_lifetime,
            record_id: record_a,
            selected_version: version_a,
            prior_revision: revision(200),
            prior_reference: FileRestorePriorReferenceV1::ObservedRevision,
            restored_revision: revision(201),
            compensates: None,
        };
        xattr::set(
            &a,
            format!(
                "{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{}",
                original.intent_id
            ),
            &serde_json::to_vec(&CheckedHistoryResponseV1 {
                schema_version: 1,
                outcome: CheckedHistoryOutcomeV1::Applied { receipt },
            })
            .unwrap(),
        )
        .unwrap();
        driver.click_named("history-files-result").unwrap();
        wait(&mut driver, "Restored revision");
        source.set_target(Some(target(&b, record_b, "B again")));
        wait(
            &mut driver,
            &format!("timeline-entry:{}", version_b.0.simple()),
        );
        source.set_target(Some(target(&a, record_a, "A third")));
        wait(&mut driver, "history-files-undo");
        wait(&mut driver, &select);
        driver.click_named("history-files-undo").unwrap();
        confirm(&mut driver, "production undo after receipt recovery");
        returned.wait(&mut driver, 3);
        wait(&mut driver, UNAVAILABLE_ACK);
        let undo: CheckedHistoryRequestV1 = serde_json::from_slice(
            &xattr::get(&a, CHECKED_HISTORY_RESTORE_XATTR)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            undo.operation,
            CheckedHistoryOperationV1::Undo {
                restore_intent: original.intent_id
            }
        );
        assert_ne!(undo.intent_id, original.intent_id);
    }
}
