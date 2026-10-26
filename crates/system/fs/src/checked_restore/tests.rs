use super::*;
use collab_types::Ed25519PublicKey;
use fs_protocol::files::CheckedHistoryOperationV1;
use fs_protocol::{FileActor, FileProvenance};
use iroh::{Endpoint, endpoint::presets};

struct Fixture {
    directory: tempfile::TempDir,
    endpoint: Endpoint,
    fs: QuickFS,
    ino: u64,
    original: fs_protocol::FileVersionId,
    history: Arc<crate::history::FileHistoryStore>,
    journal: Arc<Mutex<crate::record_transaction::RecordTransactionJournal>>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let hash = store.write_content(b"first").await.unwrap().unwrap();
        let mut namespace = Namespace::new("checked-restore-test").unwrap();
        let ino = namespace
            .create_file(crate::namespace::ROOT_INO, "test.bin", Some(&hash), 5, 1)
            .unwrap();
        let record = namespace.record_id_for(ino).unwrap();
        let namespace = Arc::new(Mutex::new(namespace));
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let signer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let person = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes(person.verifying_key().to_bytes()),
            device: Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes()),
        };
        let history = Arc::new(
            crate::history::FileHistoryStore::open(directory.path().join("history.json")).unwrap(),
        );
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(directory.path().join("records"))
                .unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                uuid::Uuid::new_v4(),
                actor,
                signer,
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                directory.path().join("transactions"),
            )
            .unwrap(),
        ));
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap()
            .with_checked_history_workspace(publisher.workspace_id())
            .with_record_publisher(publisher)
            .unwrap()
            .with_replacement_transactions(journal.clone(), directory.path().join("namespace.am"))
            .unwrap();
        let original = history.snapshot(record).unwrap().versions[0].id;
        fs.commit_content(ino, b"second").unwrap();
        Self {
            directory,
            endpoint,
            fs,
            ino,
            original,
            history,
            journal,
        }
    }

    fn reopen(&self) -> QuickFS {
        let path = self.directory.path();
        let namespace = Arc::new(Mutex::new(
            Namespace::from_bytes(
                "reopened-mount",
                &std::fs::read(path.join("namespace.am")).unwrap(),
            )
            .unwrap(),
        ));
        let hydrator = self.fs.hydrator.clone();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let actor = self.fs.history.as_ref().unwrap().1;
        let workspace = self.fs.restore_workspace().unwrap();
        let history =
            Arc::new(crate::history::FileHistoryStore::open(path.join("history.json")).unwrap());
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .without_legacy_record_migration()
            .with_checked_history_workspace(workspace)
            .with_history(history, actor)
            .unwrap();
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(path.join("records")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                workspace,
                actor,
                ed25519_dalek::SigningKey::from_bytes(&[31; 32]),
            )
            .unwrap(),
        );
        let fs = fs.with_record_publisher(publisher).unwrap();
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(path.join("transactions"))
                .unwrap(),
        ));
        fs.with_replacement_transactions(journal, path.join("namespace.am"))
            .unwrap()
    }

    fn durable_bytes(&self) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        fn collect(path: &std::path::Path, output: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
            for item in std::fs::read_dir(path).unwrap() {
                let path = item.unwrap().path();
                if path.is_dir() {
                    collect(&path, output);
                } else {
                    output.push((path.clone(), std::fs::read(path).unwrap()));
                }
            }
        }
        let mut output = Vec::new();
        collect(self.directory.path(), &mut output);
        output.sort_by(|a, b| a.0.cmp(&b.0));
        output
    }

    fn request(&self) -> Request {
        Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: self.fs.checked_history_review(self.ino).unwrap(),
            operation: CheckedHistoryOperationV1::Restore {
                version_id: self.original,
            },
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checked_restore_appends_exact_results_once_and_noops_preserve_bytes() {
    // Phase `checked_restore_appends_exact_result_and_retry_does_not_append`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let before = fixture.history.snapshot(request.review.record_id).unwrap();
        let result = fixture.fs.checked_history_restore(fixture.ino, request);
        let Outcome::Applied { receipt } = &result else {
            panic!("{result:?}");
        };
        assert_eq!(receipt.intent_id, request.intent_id);
        assert_eq!(receipt.selected_version, fixture.original);
        let after = fixture.history.snapshot(request.review.record_id).unwrap();
        let mut collision = request;
        collision.operation = Operation::Restore {
            version_id: receipt.restored_revision.version_id,
        };
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, collision),
            Outcome::InvalidRequest
        );
        assert_eq!(after.versions.len(), before.versions.len() + 1);
        let restored = after
            .versions
            .iter()
            .find(|v| v.id == receipt.restored_revision.version_id)
            .unwrap();
        assert_eq!(restored.parents, before.heads);
        assert_eq!(
            restored.provenance,
            FileProvenance::Restore {
                from: fixture.original
            }
        );
        assert_eq!(
            fixture.fs.ns().content_revision_for(fixture.ino),
            Some(receipt.restored_revision)
        );
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            result
        );
        assert_eq!(
            fixture.history.snapshot(request.review.record_id).unwrap(),
            after
        );
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_fresh_noop_preserves_all_durable_bytes_and_namespace`.
    {
        let fixture = Fixture::new().await;
        let mut request = fixture.request();
        let state = fixture.history.snapshot(request.review.record_id).unwrap();
        request.operation = Operation::Restore {
            version_id: state.heads[0],
        };
        let before = fixture.durable_bytes();
        let namespace_before = fixture.fs.ns().save();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Unchanged {
                intent_id: request.intent_id,
                review: request.review
            }
        );
        assert_eq!(fixture.durable_bytes(), before);
        assert_eq!(fixture.fs.ns().save(), namespace_before);
        assert_eq!(fixture.journal.lock().unwrap().retained_count(), 0);
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checked_restore_refuses_changed_reviews_and_same_byte_ambiguity() {
    // Phase `checked_restore_same_byte_edit_invalidates_review_before_any_restore_write`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        fixture
            .fs
            .record_version(fixture.ino, FileProvenance::Edit, None);
        let revision = fixture
            .fs
            .record_publisher
            .as_ref()
            .unwrap()
            .snapshot(request.review.record_id)
            .unwrap()
            .unwrap()
            .content_revision
            .unwrap();
        fixture
            .fs
            .ns()
            .set_content_revision(fixture.ino, &revision, now_ms());
        let before = fixture.history.snapshot(request.review.record_id).unwrap();
        assert_eq!(
            before.versions.len(),
            3,
            "a same-byte authored version must actually exist"
        );
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::ReviewRequired
        );
        assert_eq!(
            fixture.history.snapshot(request.review.record_id).unwrap(),
            before
        );
        assert!(fixture.journal.lock().unwrap().entries().next().is_none());
        fixture.endpoint.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top to bottom"
)]
async fn checked_restore_prepared_recovery_preserves_later_edits_and_fences_writers() {
    // Phase `checked_restore_prepared_recovery_preserves_later_edit_and_exact_receipt`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        RESTORE_FAULTS
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap()
            .insert(request.intent_id, TestFault::Prepared);
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::RecoveryPending {
                intent_id: request.intent_id
            }
        );
        let runtime = fixture.fs.replacement_transactions.as_ref().unwrap();
        let admitted = runtime
            .journal
            .lock()
            .unwrap()
            .get(request.intent_id)
            .unwrap()
            .clone();
        let expected = receipt(&admitted).unwrap();
        // Complete repository participation, then simulate a crash before its
        // history/namespace projection. A later ordinary edit is independently
        // accepted before retry; recovery must not demand the old frontier.
        let publisher = fixture.fs.record_publisher.as_ref().unwrap();
        publisher
            .repository()
            .apply_transaction(
                &mut runtime.journal.lock().unwrap(),
                &admitted,
                &publisher.local_authorizer(request.review.record_id),
            )
            .unwrap();
        fixture
            .history
            .append(
                admitted.transaction.manifest.participants[0]
                    .proposed_version
                    .clone(),
            )
            .unwrap();
        fixture
            .fs
            .ns()
            .set_content_revision(fixture.ino, &expected.restored_revision, now_ms());
        fixture
            .fs
            .commit_content(fixture.ino, b"later edit")
            .unwrap();
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.bin",
                crate::namespace::ROOT_INO,
                "renamed.bin",
            )
            .unwrap();
        let later = fixture.fs.ns().content_revision_for(fixture.ino).unwrap();
        let unrelated = fixture
            .fs
            .ns()
            .create_file(crate::namespace::ROOT_INO, "unrelated.bin", None, 0, 1)
            .unwrap();
        publisher
            .recover_replacement_transactions(
                &mut runtime.journal.lock().unwrap(),
                &fixture.history,
                &fixture.fs.namespace,
                &runtime.namespace_path,
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            fixture.fs.ns().content_revision_for(fixture.ino),
            Some(later)
        );
        assert_eq!(
            fixture.fs.ns().path_for(fixture.ino).as_deref(),
            Some("/renamed.bin")
        );
        assert!(fixture.fs.ns().getattr(unrelated).is_some());
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Applied { receipt: expected }
        );
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_prepared_fences_same_repository_second_publisher_and_raw_writes`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        RESTORE_FAULTS
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap()
            .insert(request.intent_id, TestFault::Prepared);
        assert!(matches!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::RecoveryPending { .. }
        ));
        let original = fixture.fs.record_publisher.as_ref().unwrap();
        let second = crate::record_publisher::RecordPublisher::new(
            original.repository().clone(),
            original.workspace_id(),
            fixture.fs.history.as_ref().unwrap().1,
            ed25519_dalek::SigningKey::from_bytes(&[31; 32]),
        )
        .unwrap();
        let before = fixture.durable_bytes();
        let ns = fixture.fs.ns().save();
        let second_intent = fixture.request();
        assert_eq!(
            fixture
                .fs
                .checked_history_restore(fixture.ino, second_intent),
            Outcome::Unavailable
        );
        assert_eq!(fixture.journal.lock().unwrap().retained_count(), 1);
        assert!(
            fixture
                .fs
                .commit_content(fixture.ino, b"must retry after recovery")
                .is_err()
        );
        let state = fixture.history.snapshot(request.review.record_id).unwrap();
        let mut version = state.versions.last().unwrap().clone();
        version.id = fs_protocol::FileVersionId::new();
        version.parents = state.heads;
        assert!(
            matches!(second.publish_structural_version(&version),Err(crate::record_publisher::RecordPublisherError::Repository(crate::record_repository::RecordRepositoryError::RestoreRecoveryRequired(id))) if id == request.intent_id)
        );
        let entry = fixture
            .journal
            .lock()
            .unwrap()
            .get(request.intent_id)
            .unwrap()
            .clone();
        let key = RecordRepositoryKey {
            workspace_id: request.review.workspace_id,
            record_id: request.review.record_id,
        };
        assert!(matches!(
            original.repository().apply_batch(
                key,
                &entry.participant_batches[0].envelopes,
                &original.local_authorizer(key.record_id)
            ),
            Err(crate::record_repository::RecordRepositoryError::RestoreRecoveryRequired(_))
        ));
        assert_eq!(fixture.durable_bytes(), before);
        assert_eq!(fixture.fs.ns().save(), ns);
        original
            .repository()
            .apply_transaction(
                &mut fixture.journal.lock().unwrap(),
                &entry,
                &original.local_authorizer(key.record_id),
            )
            .unwrap();
        fixture
            .history
            .append(
                entry.transaction.manifest.participants[0]
                    .proposed_version
                    .clone(),
            )
            .unwrap();
        fixture.fs.ns().set_content_revision(
            fixture.ino,
            &fixture
                .journal
                .lock()
                .unwrap()
                .checked_receipt(request.intent_id)
                .unwrap()
                .restored_revision,
            now_ms(),
        );
        fixture
            .fs
            .commit_content(fixture.ino, b"later accepted ordinary edit")
            .unwrap();
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.bin",
                crate::namespace::ROOT_INO,
                "later.bin",
            )
            .unwrap();
        let later = fixture.fs.ns().content_revision_for(fixture.ino).unwrap();
        std::fs::write(
            fixture.directory.path().join("namespace.am"),
            fixture.fs.ns().save(),
        )
        .unwrap();
        let reopened = fixture.reopen();
        let ino = reopened
            .ns()
            .ensure_ino_for_record_id(key.record_id)
            .unwrap();
        assert_eq!(reopened.ns().content_revision_for(ino), Some(later));
        assert_eq!(reopened.ns().path_for(ino).as_deref(), Some("/later.bin"));
        assert!(matches!(
            reopened.checked_history_restore(ino, request),
            Outcome::Applied { .. }
        ));
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn checked_restore_receipts_and_undo_survive_remount_and_reopen() {
    // Phase `checked_restore_remount_receipt_retry_and_current_visibility`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let applied = fixture.fs.checked_history_restore(fixture.ino, request);
        assert!(matches!(applied, Outcome::Applied { .. }));
        let mut reopened = fixture.reopen();
        let ino = reopened
            .ns()
            .ensure_ino_for_record_id(request.review.record_id)
            .unwrap();
        assert_ne!(reopened.mount_lifetime, request.review.mount_lifetime);
        let before = fixture.durable_bytes();
        assert_eq!(reopened.checked_history_restore(ino, request), applied);
        assert_eq!(fixture.durable_bytes(), before);
        let mut new_intent = request;
        new_intent.intent_id = uuid::Uuid::new_v4();
        assert_eq!(
            reopened.checked_history_restore(ino, new_intent),
            Outcome::ReviewRequired
        );
        let other = reopened
            .ns()
            .create_file(crate::namespace::ROOT_INO, "other.bin", None, 0, 1)
            .unwrap();
        assert_eq!(
            reopened.checked_history_result(other, request.intent_id),
            Outcome::ReviewRequired
        );
        assert_eq!(
            reopened.checked_history_restore(other, request),
            Outcome::ReviewRequired
        );
        let acl = AclHandle::new(crate::acl::WorkspaceAcl::new());
        reopened.enforcement = Some((acl.clone(), [55; 32]));
        assert_eq!(
            reopened.checked_history_result(ino, request.intent_id),
            Outcome::NotVisible
        );
        let mut readonly = crate::acl::WorkspaceAcl::new();
        readonly.grant_read([55; 32], "/");
        acl.replace(readonly);
        assert_eq!(
            reopened.checked_history_result(ino, request.intent_id),
            applied
        );
        assert_eq!(
            reopened.checked_history_restore(ino, request),
            Outcome::ReadOnly
        );
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_backend_undo_redo_survive_reopen_and_refuse_intervening_work`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let Outcome::Applied { receipt: restored } =
            fixture.fs.checked_history_restore(fixture.ino, request)
        else {
            panic!("restore");
        };
        let reopened = fixture.reopen();
        let ino = reopened
            .ns()
            .ensure_ino_for_record_id(request.review.record_id)
            .unwrap();
        let undo = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: reopened.checked_history_review(ino).unwrap(),
            operation: Operation::Undo {
                restore_intent: request.intent_id,
            },
        };
        let Outcome::Applied { receipt: undone } = reopened.checked_history_restore(ino, undo)
        else {
            panic!("undo");
        };
        assert_eq!(
            undone.restored_revision.content_hash,
            restored.prior_revision.content_hash
        );
        let redo = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: reopened.checked_history_review(ino).unwrap(),
            operation: Operation::Redo {
                undo_intent: undo.intent_id,
            },
        };
        let Outcome::Applied { receipt: redone } = reopened.checked_history_restore(ino, redo)
        else {
            panic!("redo");
        };
        assert_eq!(
            redone.restored_revision.content_hash,
            restored.restored_revision.content_hash
        );
        reopened.commit_content(ino, b"intervening work").unwrap();
        let undo_later = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: reopened.checked_history_review(ino).unwrap(),
            operation: Operation::Undo {
                restore_intent: redo.intent_id,
            },
        };
        assert_eq!(
            reopened.checked_history_restore(ino, undo_later),
            Outcome::ReviewRequired
        );
        fixture.endpoint.close().await;
    }
}

fn pause(
    id: uuid::Uuid,
    point: TestPause,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered, wait) = std::sync::mpsc::channel();
    let (resume, receive) = std::sync::mpsc::channel();
    RESTORE_PAUSES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(
            id,
            PauseGate {
                point,
                entered,
                resume: receive,
            },
        );
    (wait, resume)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checked_restore_late_acl_replacement_buffer_aba_and_rebinding_are_zero_write_rejections() {
    for change in 0..5 {
        let mut fixture = Fixture::new().await;
        let mut grants = crate::acl::WorkspaceAcl::new();
        grants.grant_read_write([55; 32], "/");
        let acl = AclHandle::new(grants);
        fixture.fs.enforcement = Some((acl.clone(), [55; 32]));
        let request = fixture.request();
        let (entered, resume) = pause(request.intent_id, TestPause::Hydrated);
        let handle = tokio::runtime::Handle::current();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let _runtime = handle.enter();
                fixture.fs.checked_history_restore(fixture.ino, request)
            });
            entered.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            let expected = match change {
                0 => {
                    acl.replace(crate::acl::WorkspaceAcl::new());
                    Outcome::NotVisible
                }
                1 => {
                    fixture.fs.install_stage(fixture.ino, b"unflushed".to_vec());
                    Outcome::PendingWrites
                }
                2 => {
                    fixture
                        .fs
                        .commit_content(fixture.ino, b"ABA alternate")
                        .unwrap();
                    fixture.fs.commit_content(fixture.ino, b"second").unwrap();
                    Outcome::ReviewRequired
                }
                3 => {
                    fixture
                        .fs
                        .rename_named(
                            crate::namespace::ROOT_INO,
                            "test.bin",
                            crate::namespace::ROOT_INO,
                            "renamed.bin",
                        )
                        .unwrap();
                    Outcome::ReviewRequired
                }
                _ => {
                    let state = fixture.history.snapshot(request.review.record_id).unwrap();
                    let mut version = state
                        .versions
                        .iter()
                        .find(|v| v.id == fixture.original)
                        .unwrap()
                        .clone();
                    version.id = fs_protocol::FileVersionId::new();
                    version.parents = vec![fixture.original];
                    version.timestamp_ms = 0;
                    version.provenance = FileProvenance::Edit;
                    if let Some(original) = fixture.fs.record_publisher.as_ref() {
                        let second = crate::record_publisher::RecordPublisher::new(
                            original.repository().clone(),
                            original.workspace_id(),
                            fixture.fs.history.as_ref().unwrap().1,
                            ed25519_dalek::SigningKey::from_bytes(&[31; 32]),
                        )
                        .unwrap();
                        second.publish_structural_version(&version).unwrap();
                    }
                    fixture.history.append(version).unwrap();
                    Outcome::ReviewRequired
                }
            };
            let before = fixture.durable_bytes();
            let namespace = fixture.fs.ns().save();
            resume.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), expected);
            assert_eq!(fixture.durable_bytes(), before);
            assert_eq!(fixture.fs.ns().save(), namespace);
            assert_eq!(fixture.journal.lock().unwrap().retained_count(), 0);
        });
        fixture.endpoint.close().await;
    }
}

/// This thread's id, as `/proc/self/task` names it.
fn thread_id() -> String {
    tilo_test_env::idle::current_thread_id()
        .unwrap()
        .to_string()
}

/// Waits until thread `tid` sleeps (on a lock, a condvar or a channel) or
/// `done` says it got through without sleeping.
fn blocked_or_done(tid: &str, what: &str, mut done: impl FnMut() -> bool) {
    tilo_test_env::wait_until(format_args!("{what} to sleep or finish"), || {
        done()
            || std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('S'))
            })
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn checked_restore_expiry_and_storage_interleavings_complete_safely() {
    // Phase `checked_restore_real_expiry_is_rechecked_at_final_admission_and_acl_writer_waits`.
    {
        use collab_types::{Access, CapOverride, Effect, Expiry, Scope, Subject};
        let mut fixture = Fixture::new().await;
        // The wall clock stands still until the test moves it past the
        // expiry, so admission sees the grant live however slowly it runs.
        let start = tilo_clock::wall();
        tilo_clock::freeze_wall(start);
        let expiry = tilo_clock::try_unix_now().unwrap() + 2;
        let cap = CapOverride::new(
            Subject::User(Ed25519PublicKey([55; 32])),
            Scope::FsPath("/".into()),
            Effect::Grant,
            Access::Write,
        )
        .expiring(Expiry::AtUnix(expiry));
        let acl = AclHandle::new(crate::acl::WorkspaceAcl::from_capabilities(
            [55; 32],
            &[cap],
            &[],
            None,
        ));
        fixture.fs.enforcement = Some((acl.clone(), [55; 32]));
        let request = fixture.request();
        let (entered, resume) = pause(request.intent_id, TestPause::Admission);
        let before = fixture.durable_bytes();
        let handle = tokio::runtime::Handle::current();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let _runtime = handle.enter();
                fixture.fs.checked_history_restore(fixture.ino, request)
            });
            entered.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            let (finished, wait) = std::sync::mpsc::channel();
            let (writer_tid, writer) = std::sync::mpsc::channel();
            scope.spawn(move || {
                writer_tid.send(thread_id()).unwrap();
                acl.replace(crate::acl::WorkspaceAcl::new());
                finished.send(()).unwrap();
            });
            let writer = tilo_test_env::recv("the ACL writer's thread", &writer);
            let mut through = false;
            blocked_or_done(&writer, "the ACL writer", || {
                through = through || wait.try_recv().is_ok();
                through
            });
            assert!(!through, "ACL write must wait behind admission read lease");
            tilo_clock::freeze_wall(start + std::time::Duration::from_secs(3));
            resume.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), Outcome::NotVisible);
            wait.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        });
        assert_eq!(fixture.durable_bytes(), before);
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_storage_buffer_namespace_and_acl_interleaving_completes_without_deadlock`.
    {
        let mut fixture = Fixture::new().await;
        let mut grants = crate::acl::WorkspaceAcl::new();
        grants.grant_read_write([55; 32], "/");
        let acl = AclHandle::new(grants);
        fixture.fs.enforcement = Some((acl.clone(), [55; 32]));
        let request = fixture.request();
        let before = fixture.durable_bytes();
        let namespace_before = fixture.fs.ns().save();
        let (entered, resume) = pause(request.intent_id, TestPause::BeforeBuffers);
        let (finished, result) = std::sync::mpsc::channel();
        let (acl_done, acl_result) = std::sync::mpsc::channel();
        let handle = tokio::runtime::Handle::current();
        // The actual owners used by storage::execute are held in its audited B→N
        // order. Admission reaches J/P before waiting for B and must not retain N/A.
        let storage_buffers = fixture.fs.write_buffers.lock().unwrap();
        let (restorer_tid, restorer) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _runtime = handle.enter();
                restorer_tid.send(thread_id()).unwrap();
                finished
                    .send(fixture.fs.checked_history_restore(fixture.ino, request))
                    .unwrap();
            });
            let restorer = tilo_test_env::recv("the restore's thread", &restorer);
            entered.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            let storage_namespace = fixture
                .fs
                .namespace
                .try_lock()
                .expect("restore must not hold N while waiting for storage's B");
            scope.spawn(|| {
                let mut readonly = crate::acl::WorkspaceAcl::new();
                readonly.grant_read([55; 32], "/");
                acl.replace(readonly);
                acl_done.send(()).unwrap();
            });
            acl_result
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("ACL replacement must not wait on admission before B/N");
            resume.send(()).unwrap();
            // Resumed, the restore goes on until it needs the buffers storage
            // holds, and sleeps there.
            let mut early = None;
            blocked_or_done(&restorer, "the resumed restore", || {
                early = early.take().or_else(|| result.try_recv().ok());
                early.is_some()
            });
            assert!(early.is_none(), "storage still owns the admission buffers");
            drop(storage_namespace);
            drop(storage_buffers);
            assert_eq!(
                result.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                Outcome::ReadOnly
            );
        });
        assert_eq!(fixture.durable_bytes(), before);
        assert_eq!(fixture.fs.ns().save(), namespace_before);
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top to bottom"
)]
async fn checked_restore_durable_phases_reopen_exactly_and_frontiers_parent_all_heads() {
    // Phase `checked_restore_every_durable_phase_reopens_exactly_and_next_edit_is_valid`.
    {
        for point in [
            TestFault::Prepared,
            TestFault::Participant,
            TestFault::Records,
            TestFault::History,
            TestFault::Delta,
            TestFault::Namespace,
            TestFault::Broadcast,
        ] {
            let fixture = Fixture::new().await;
            let request = fixture.request();
            std::fs::write(
                fixture.directory.path().join("namespace.am"),
                fixture.fs.ns().save(),
            )
            .unwrap();
            RESTORE_FAULTS
                .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap()
                .insert(request.intent_id, point);
            assert_eq!(
                fixture.fs.checked_history_restore(fixture.ino, request),
                Outcome::RecoveryPending {
                    intent_id: request.intent_id
                },
                "{point:?}"
            );
            let receipt = fixture
                .journal
                .lock()
                .unwrap()
                .checked_receipt(request.intent_id)
                .unwrap();
            let reopened = fixture.reopen();
            let ino = reopened
                .ns()
                .ensure_ino_for_record_id(request.review.record_id)
                .unwrap();
            assert_eq!(
                reopened.checked_history_restore(ino, request),
                Outcome::Applied {
                    receipt: receipt.clone()
                }
            );
            let count = reopened
                .history
                .as_ref()
                .unwrap()
                .0
                .snapshot(request.review.record_id)
                .unwrap()
                .versions
                .len();
            let before = fixture.durable_bytes();
            assert_eq!(
                reopened.checked_history_restore(ino, request),
                Outcome::Applied {
                    receipt: receipt.clone()
                }
            );
            assert_eq!(fixture.durable_bytes(), before);
            reopened.commit_content(ino, b"next ordinary edit").unwrap();
            assert_ne!(
                reopened
                    .ns()
                    .hash_for(ino)
                    .map(|v| ContentHash(*v.as_bytes())),
                receipt.restored_revision.content_hash
            );
            assert_eq!(
                reopened
                    .history
                    .as_ref()
                    .unwrap()
                    .0
                    .snapshot(request.review.record_id)
                    .unwrap()
                    .versions
                    .len(),
                count + 1
            );
            fixture.endpoint.close().await;
        }
    }
    // Phase `checked_restore_nonwinning_head_invalidates_frontier_and_fresh_restore_parents_all_heads`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let before = fixture.fs.ns().hash_for(fixture.ino);
        let state = fixture.history.snapshot(request.review.record_id).unwrap();
        let mut losing = state
            .versions
            .iter()
            .find(|v| v.id == fixture.original)
            .unwrap()
            .clone();
        losing.id = fs_protocol::FileVersionId::new();
        losing.parents = vec![fixture.original];
        losing.timestamp_ms = 0;
        losing.provenance = FileProvenance::Edit;
        if let Some(publisher) = fixture.fs.record_publisher.as_ref() {
            publisher.publish_structural_version(&losing).unwrap();
        }
        fixture.history.append(losing).unwrap();
        assert_eq!(fixture.fs.ns().hash_for(fixture.ino), before);
        let durable = fixture.durable_bytes();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::ReviewRequired
        );
        assert_eq!(fixture.durable_bytes(), durable);
        let heads = fixture
            .history
            .snapshot(request.review.record_id)
            .unwrap()
            .heads;
        assert_eq!(heads.len(), 2);
        let request = fixture.request();
        let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
        else {
            panic!("fresh multihead restore");
        };
        let result = fixture
            .history
            .snapshot(request.review.record_id)
            .unwrap()
            .versions
            .into_iter()
            .find(|v| v.id == receipt.restored_revision.version_id)
            .unwrap();
        assert_eq!(result.parents, heads);
        let undo = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: fixture.fs.checked_history_review(fixture.ino).unwrap(),
            operation: Operation::Undo {
                restore_intent: request.intent_id,
            },
        };
        let Outcome::Applied { receipt: undone } =
            fixture.fs.checked_history_restore(fixture.ino, undo)
        else {
            panic!("multihead undo");
        };
        assert_eq!(
            undone.restored_revision.content_hash,
            receipt.prior_revision.content_hash
        );
        let redo = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: fixture.fs.checked_history_review(fixture.ino).unwrap(),
            operation: Operation::Redo {
                undo_intent: undo.intent_id,
            },
        };
        let Outcome::Applied { receipt: redone } =
            fixture.fs.checked_history_restore(fixture.ino, redo)
        else {
            panic!("multihead redo");
        };
        assert_eq!(
            redone.restored_revision.content_hash,
            receipt.restored_revision.content_hash
        );
        let final_state = fixture.history.snapshot(request.review.record_id).unwrap();
        assert_eq!(final_state.versions.len(), 6);
        assert_eq!(
            final_state
                .versions
                .iter()
                .find(|v| v.id == undone.restored_revision.version_id)
                .unwrap()
                .parents,
            vec![receipt.restored_revision.version_id]
        );
        assert_eq!(
            final_state
                .versions
                .iter()
                .find(|v| v.id == redone.restored_revision.version_id)
                .unwrap()
                .parents,
            vec![undone.restored_revision.version_id]
        );
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checked_restore_scope_codecs_transport_and_parent_bounds_fail_closed() {
    // Phase `checked_restore_scope_codecs_and_transport_fail_closed`.
    {
        let mut fixture = Fixture::new().await;
        let request = fixture.request();
        let actual = fixture.fs.checked_history_workspace;
        fixture.fs.checked_history_workspace = Some(uuid::Uuid::nil());
        assert!(fixture.fs.checked_history_review(fixture.ino).is_err());
        fixture.fs.checked_history_workspace = Some(uuid::Uuid::new_v4());
        assert!(fixture.fs.checked_history_review(fixture.ino).is_err());
        fixture.fs.checked_history_workspace = actual;
        assert_eq!(
            fixture
                .fs
                .checked_restore_xattr(fixture.ino, &vec![b' '; 1025]),
            Err(EINVAL)
        );
        assert_eq!(
            fixture.fs.checked_restore_xattr(
                fixture.ino,
                br#"{"schema_version":1,"operation":"restore"}"#
            ),
            Err(EINVAL)
        );
        let mut wrong = request;
        wrong.review.mount_lifetime = uuid::Uuid::new_v4();
        assert_eq!(
            fixture
                .fs
                .checked_restore_xattr(fixture.ino, &serde_json::to_vec(&wrong).unwrap()),
            Err(EBUSY)
        );
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.bin",
                crate::namespace::ROOT_INO,
                "test.tilo-doc",
            )
            .unwrap();
        let before = fixture.durable_bytes();
        assert!(matches!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::UnsupportedOwner { .. }
        ));
        assert_eq!(fixture.durable_bytes(), before);
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.tilo-doc",
                crate::namespace::ROOT_INO,
                "test.bin",
            )
            .unwrap();
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_service_parent_bound_rejects_before_any_authored_write`.
    {
        let fixture = Fixture::new().await;
        let initial = fixture.request();
        let state = fixture.history.snapshot(initial.review.record_id).unwrap();
        let source = state
            .versions
            .iter()
            .find(|v| v.id == fixture.original)
            .unwrap();
        for _ in 0..fs_protocol::files::MAX_VERSION_PARENTS {
            let mut version = source.clone();
            version.id = fs_protocol::FileVersionId::new();
            version.parents = vec![fixture.original];
            version.timestamp_ms = 0;
            version.provenance = FileProvenance::Edit;
            if let Some(publisher) = fixture.fs.record_publisher.as_ref() {
                publisher.publish_structural_version(&version).unwrap();
            }
            fixture.history.append(version).unwrap();
        }
        assert_eq!(
            fixture
                .history
                .snapshot(initial.review.record_id)
                .unwrap()
                .heads
                .len(),
            fs_protocol::files::MAX_VERSION_PARENTS + 1
        );
        let request = fixture.request();
        let before = fixture.durable_bytes();
        let namespace = fixture.fs.ns().save();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Unavailable
        );
        assert_eq!(fixture.durable_bytes(), before);
        assert_eq!(fixture.fs.ns().save(), namespace);
        fixture.endpoint.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checked_restore_service_retained_journal_capacity_survives_reopen_and_rejects_before_admission()
 {
    let fixture = Fixture::new().await;
    let request = fixture.request();
    let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
    else {
        panic!("initial receipt");
    };
    let template = fixture
        .journal
        .lock()
        .unwrap()
        .get(request.intent_id)
        .unwrap()
        .clone();
    let publisher = fixture.fs.record_publisher.as_ref().unwrap();
    // Reopen a real on-disk bounded fixture. Each entry is validated by
    // the existing journal decoder, including completed receipt scope.
    for _ in 1..crate::record_transaction::MAX_RECORD_TRANSACTION_ENTRIES {
        let intent_id = uuid::Uuid::new_v4();
        let mut manifest = template.transaction.manifest.clone();
        manifest.transaction_id = intent_id;
        let fs_protocol::FileMutationOperation::RestoreContent { request, .. } =
            &mut manifest.operation
        else {
            panic!("restore manifest");
        };
        request.intent_id = intent_id;
        let mut entry = publisher.sign_checked_restore(manifest, 0).unwrap();
        entry.phase = Phase::BroadcastCommitted;
        entry.namespace_delta.clone_from(&template.namespace_delta);
        entry.validate().unwrap();
        std::fs::write(
            fixture
                .directory
                .path()
                .join("transactions")
                .join(format!("{intent_id}.json")),
            serde_json::to_vec(&entry).unwrap(),
        )
        .unwrap();
    }
    *fixture.journal.lock().unwrap() = crate::record_transaction::RecordTransactionJournal::open(
        fixture.directory.path().join("transactions"),
    )
    .unwrap();
    assert_eq!(
        fixture.journal.lock().unwrap().retained_count(),
        crate::record_transaction::MAX_RECORD_TRANSACTION_ENTRIES
    );
    assert_eq!(
        fixture.fs.checked_history_restore(fixture.ino, request),
        Outcome::Applied {
            receipt: receipt.clone()
        }
    );
    let undo = Request {
        schema_version: 1,
        intent_id: uuid::Uuid::new_v4(),
        review: fixture.fs.checked_history_review(fixture.ino).unwrap(),
        operation: Operation::Undo {
            restore_intent: request.intent_id,
        },
    };
    let before = fixture.durable_bytes();
    let namespace = fixture.fs.ns().save();
    assert_eq!(
        fixture.fs.checked_history_restore(fixture.ino, undo),
        Outcome::Unavailable
    );
    assert_eq!(fixture.durable_bytes(), before);
    assert_eq!(fixture.fs.ns().save(), namespace);
    assert_eq!(
        fixture
            .fs
            .checked_history_result(fixture.ino, request.intent_id),
        Outcome::Applied { receipt }
    );
    fixture.endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top to bottom"
)]
async fn checked_restore_remote_transactions_and_fault_startup_clear_exact_fences() {
    // Phase `checked_restore_remote_signed_transaction_materializes_then_preserves_later_ordinary_content`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let remote = tempfile::tempdir().unwrap();
        for (path, bytes) in fixture.durable_bytes() {
            if let Ok(relative) = path.strip_prefix(fixture.directory.path().join("records")) {
                let target = remote.path().join("records").join(relative);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, bytes).unwrap();
            }
        }
        // History is derived from the copied signed repository. Legacy files
        // are copied read-only for the import path, never as authority.
        for name in ["history.json", "history.json.log"] {
            let source = fixture.directory.path().join(name);
            if source.exists() {
                std::fs::copy(source, remote.path().join(name)).unwrap();
            }
        }
        let namespace = Arc::new(Mutex::new(fixture.fs.ns().clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(remote.path().join("history.json")).unwrap(),
        );
        let repository =
            crate::record_repository::RecordRepository::open(remote.path().join("records"))
                .unwrap();
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                remote.path().join("transactions"),
            )
            .unwrap(),
        ));
        let materializer = crate::record_materializer::RecordMaterializer::new(
            request.review.workspace_id,
            namespace.clone(),
            Some(history.clone()),
        )
        .with_transaction_recovery(journal.clone(), remote.path().join("namespace.am"));
        let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
        else {
            panic!("local restore");
        };
        let entry = fixture
            .journal
            .lock()
            .unwrap()
            .get(request.intent_id)
            .unwrap()
            .clone();
        let publisher = fixture.fs.record_publisher.as_ref().unwrap();
        let key = RecordRepositoryKey {
            workspace_id: request.review.workspace_id,
            record_id: request.review.record_id,
        };
        repository
            .apply_batch(
                key,
                &entry.participant_batches[0].envelopes,
                &publisher.local_authorizer(key.record_id),
            )
            .unwrap();
        materializer.reconcile_repository(&repository).unwrap();
        assert_eq!(
            namespace.lock().unwrap().content_revision_for(fixture.ino),
            Some(receipt.restored_revision)
        );
        assert_eq!(
            journal.lock().unwrap().checked_receipt(request.intent_id),
            Some(receipt.clone())
        );
        fixture
            .fs
            .commit_content(fixture.ino, b"ordinary after remote transaction")
            .unwrap();
        let later = fixture.fs.ns().content_revision_for(fixture.ino).unwrap();
        let actor = fixture.fs.history.as_ref().unwrap().1;
        let sequence = repository.highest_sequence(key, &actor).unwrap();
        let batch = publisher.repository().reconciliation_batch(key).unwrap();
        let new = batch
            .envelopes
            .into_iter()
            .filter(|envelope| envelope.actor == actor && envelope.sequence > sequence)
            .collect::<Vec<_>>();
        repository
            .apply_batch(key, &new, &publisher.local_authorizer(key.record_id))
            .unwrap();
        materializer.reconcile_repository(&repository).unwrap();
        materializer.reconcile_repository(&repository).unwrap();
        assert_eq!(
            namespace.lock().unwrap().content_revision_for(fixture.ino),
            Some(later)
        );
        assert_eq!(
            journal.lock().unwrap().checked_receipt(request.intent_id),
            Some(receipt)
        );
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_production_startup_clears_exact_prepared_fence_after_participant_fault`.
    {
        for remote_authored in [false, true] {
            for projections_already_complete in [false, true] {
                let fixture = Fixture::new().await;
                let request = fixture.request();
                let namespace_path = fixture.directory.path().join("namespace.am");
                std::fs::write(&namespace_path, fixture.fs.ns().save()).unwrap();
                RESTORE_FAULTS
                    .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                    .lock()
                    .unwrap()
                    .insert(request.intent_id, TestFault::Participant);
                assert_eq!(
                    fixture.fs.checked_history_restore(fixture.ino, request),
                    Outcome::RecoveryPending {
                        intent_id: request.intent_id
                    }
                );
                let admitted = fixture
                    .journal
                    .lock()
                    .unwrap()
                    .get(request.intent_id)
                    .unwrap()
                    .clone();
                let exact_receipt = fixture
                    .journal
                    .lock()
                    .unwrap()
                    .checked_receipt(request.intent_id)
                    .unwrap();
                assert_eq!(admitted.phase, Phase::Prepared);
                if projections_already_complete {
                    // Model projections already delivered/recovered independently
                    // before this journal's participant phase was acknowledged.
                    let version = admitted.transaction.manifest.participants[0]
                        .proposed_version
                        .clone();
                    fixture.history.append(version.clone()).unwrap();
                    assert!(fixture.fs.ns().set_content_revision(
                        fixture.ino,
                        &exact_receipt.restored_revision,
                        version.timestamp_ms
                    ));
                    std::fs::write(&namespace_path, fixture.fs.ns().save()).unwrap();
                }
                let hydrator = fixture.fs.hydrator.clone();
                let Fixture {
                    directory,
                    endpoint,
                    fs,
                    history,
                    journal,
                    ..
                } = fixture;
                drop(fs);
                drop(history);
                drop(journal);

                let namespace = Arc::new(Mutex::new(
                    Namespace::from_bytes(
                        "production-reopen",
                        &std::fs::read(&namespace_path).unwrap(),
                    )
                    .unwrap(),
                ));
                let history = Arc::new(
                    crate::history::FileHistoryStore::open(directory.path().join("history.json"))
                        .unwrap(),
                );
                let repository = Arc::new(
                    crate::record_repository::RecordRepository::open(
                        directory.path().join("records"),
                    )
                    .unwrap(),
                );
                let journal = Arc::new(Mutex::new(
                    crate::record_transaction::RecordTransactionJournal::open(
                        directory.path().join("transactions"),
                    )
                    .unwrap(),
                ));
                assert_eq!(
                    journal
                        .lock()
                        .unwrap()
                        .get(request.intent_id)
                        .unwrap()
                        .phase,
                    Phase::Prepared
                );
                assert!(
                    repository
                        .transaction_status(request.review.workspace_id, request.intent_id)
                        .unwrap()
                        .unwrap()
                        .complete
                );
                assert_eq!(
                    repository
                        .completed_transaction_journal_entry(
                            request.review.workspace_id,
                            &admitted.transaction
                        )
                        .unwrap()
                        .participant_batches,
                    admitted.participant_batches
                );

                // Production order: main seeds R, then materializes before the
                // mount publisher and QuickFS transaction runtime are attached.
                repository
                    .restore_admission_fences(&journal.lock().unwrap())
                    .unwrap();
                let materializer = crate::record_materializer::RecordMaterializer::new(
                    request.review.workspace_id,
                    namespace.clone(),
                    Some(history.clone()),
                )
                .with_transaction_recovery(journal.clone(), namespace_path.clone());
                materializer.reconcile_repository(&repository).unwrap();
                assert_eq!(
                    journal
                        .lock()
                        .unwrap()
                        .get(request.intent_id)
                        .unwrap()
                        .phase,
                    Phase::BroadcastCommitted
                );

                let key = ed25519_dalek::SigningKey::from_bytes(
                    &[if remote_authored { 41 } else { 31 }; 32],
                );
                let actor = FileActor {
                    person: admitted.transaction.manifest.actor.person,
                    device: Ed25519PublicKey(key.verifying_key().to_bytes()),
                };
                let publisher = Arc::new(
                    crate::record_publisher::RecordPublisher::new(
                        repository.clone(),
                        request.review.workspace_id,
                        actor,
                        key,
                    )
                    .unwrap(),
                );
                let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
                let reopened = QuickFS::new(namespace.clone(), hydrator, heuristics)
                    .without_legacy_record_migration()
                    .with_checked_history_workspace(request.review.workspace_id)
                    .with_history(history.clone(), actor)
                    .unwrap()
                    .with_record_publisher(publisher)
                    .unwrap()
                    .with_replacement_transactions(journal.clone(), namespace_path)
                    .unwrap();
                let ino = reopened
                    .ns()
                    .ensure_ino_for_record_id(request.review.record_id)
                    .unwrap();
                assert_eq!(
                    reopened.checked_history_restore(ino, request),
                    Outcome::Applied {
                        receipt: exact_receipt.clone()
                    }
                );
                assert_eq!(
                    history
                        .snapshot(request.review.record_id)
                        .unwrap()
                        .versions
                        .iter()
                        .filter(|version| version.id == exact_receipt.restored_revision.version_id)
                        .count(),
                    1
                );
                assert!(Arc::ptr_eq(
                    reopened.record_publisher.as_ref().unwrap().repository(),
                    &repository
                ));
                reopened
                    .commit_content(ino, b"ordinary edit after materializer-first recovery")
                    .expect("completed restore must release the same repository's fence");
                let later = reopened.ns().content_revision_for(ino).unwrap();
                assert_ne!(later.version_id, exact_receipt.restored_revision.version_id);
                materializer.reconcile_repository(&repository).unwrap();
                assert_eq!(reopened.ns().content_revision_for(ino), Some(later));
                assert_eq!(
                    reopened.checked_history_restore(ino, request),
                    Outcome::Applied {
                        receipt: exact_receipt
                    }
                );
                endpoint.close().await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn checked_restore_admission_errors_retry_exactly_and_availability_tracks_projection() {
    // Phase `checked_restore_admission_io_error_retains_intent_and_signed_fence_until_exact_retry`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let root = fixture.directory.path().join("transactions");
        std::fs::create_dir_all(&root).unwrap();
        let backup = fixture.directory.path().join("transactions-before-fault");
        std::fs::rename(&root, &backup).unwrap();
        std::fs::write(&root, b"unavailable journal directory").unwrap();
        let namespace = fixture.fs.ns().save();
        let history = fixture.history.snapshot(request.review.record_id).unwrap();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::RecoveryPending {
                intent_id: request.intent_id
            }
        );
        assert_eq!(
            fixture
                .journal
                .lock()
                .unwrap()
                .checked_request(request.intent_id),
            Some(request)
        );
        assert_eq!(fixture.fs.ns().save(), namespace);
        assert_eq!(
            fixture.history.snapshot(request.review.record_id).unwrap(),
            history
        );
        assert!(
            fixture
                .fs
                .commit_content(fixture.ino, b"cannot bypass uncertain admission")
                .is_err()
        );
        std::fs::remove_file(&root).unwrap();
        std::fs::rename(&backup, &root).unwrap();
        let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
        else {
            panic!("exact IO retry");
        };
        let before = fixture.durable_bytes();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Applied { receipt }
        );
        assert_eq!(fixture.durable_bytes(), before);
        fixture.endpoint.close().await;
    }
    // Phase `checked_restore_availability_tracks_current_projection_without_changing_exact_receipt`.
    {
        let mut fixture = Fixture::new().await;
        let request = fixture.request();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(fixture.directory.path().join("availability.json")).unwrap(),
        ));
        let root = fixture
            .fs
            .ns()
            .record_id_for(crate::namespace::ROOT_INO)
            .unwrap();
        registry
            .lock()
            .unwrap()
            .put(RecordAvailability::new(root, RecordKind::Directory))
            .unwrap();
        let mut row = RecordAvailability::new(request.review.record_id, RecordKind::File);
        row.parent = Some(root);
        row.current_version = Some(fixture.original);
        row.freshness.local_verified_at_ms = Some(99);
        registry.lock().unwrap().put(row).unwrap();
        fixture.fs.availability = Some(registry.clone());
        let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
        else {
            panic!("restore");
        };
        let row = registry
            .lock()
            .unwrap()
            .get(request.review.record_id)
            .unwrap()
            .clone();
        assert_eq!(
            row.current_version,
            Some(receipt.restored_revision.version_id)
        );
        assert_eq!(row.freshness.local_verified_at_ms, None);
        fixture
            .fs
            .commit_content(fixture.ino, b"later current availability")
            .unwrap();
        let later = fixture.fs.ns().content_revision_for(fixture.ino).unwrap();
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Applied { receipt }
        );
        assert_eq!(
            registry
                .lock()
                .unwrap()
                .get(request.review.record_id)
                .unwrap()
                .current_version,
            Some(later.version_id)
        );
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top to bottom"
)]
async fn checked_restore_startup_completion_rejects_bad_batches_and_retries_io_failures() {
    // Phase `checked_restore_startup_completion_rejects_incomplete_or_mismatching_admitted_batches`.
    {
        for mismatching_batch in [false, true] {
            let fixture = Fixture::new().await;
            let request = fixture.request();
            RESTORE_FAULTS
                .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap()
                .insert(
                    request.intent_id,
                    if mismatching_batch {
                        TestFault::Participant
                    } else {
                        TestFault::Prepared
                    },
                );
            assert_eq!(
                fixture.fs.checked_history_restore(fixture.ino, request),
                Outcome::RecoveryPending {
                    intent_id: request.intent_id
                }
            );
            let mut proposed = fixture
                .journal
                .lock()
                .unwrap()
                .get(request.intent_id)
                .unwrap()
                .clone();
            let repository = crate::record_repository::RecordRepository::open(
                fixture.directory.path().join("records"),
            )
            .unwrap();
            repository.clear_admission_fences();
            if mismatching_batch {
                assert!(
                    repository
                        .transaction_status(request.review.workspace_id, request.intent_id)
                        .unwrap()
                        .unwrap()
                        .complete
                );
                let key = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
                for envelope in &mut proposed.participant_batches[0].envelopes {
                    *envelope = crate::record_replication::RecordEnvelopeV1::signed(
                        envelope.record_id,
                        envelope.actor,
                        envelope.sequence + 100,
                        envelope.operation.clone(),
                        &key,
                    )
                    .unwrap();
                }
                proposed.validate().unwrap(); // Valid signatures do not make it the admitted batch.
            } else {
                let publisher = fixture.fs.record_publisher.as_ref().unwrap();
                let key = RecordRepositoryKey {
                    workspace_id: request.review.workspace_id,
                    record_id: request.review.record_id,
                };
                repository
                    .apply(
                        key,
                        &proposed.participant_batches[0].envelopes[0],
                        &publisher.local_authorizer(key.record_id),
                    )
                    .unwrap();
                assert!(
                    !repository
                        .transaction_status(request.review.workspace_id, request.intent_id)
                        .unwrap()
                        .unwrap()
                        .complete
                );
            }
            repository
                .restore_admission_fences(&fixture.journal.lock().unwrap())
                .unwrap();
            let before = fixture.durable_bytes();
            let namespace_before = fixture.fs.ns().save();
            let outcome = crate::transaction_coordinator::complete_checked_restore_after_repository(
                &repository,
                &mut fixture.journal.lock().unwrap(),
                &fixture.history,
                &fixture.fs.namespace,
                &fixture.directory.path().join("namespace.am"),
                &proposed,
                |_| Ok(()),
            );
            assert!(
                outcome.is_err(),
                "only the exact complete admitted batch can finish repository participation"
            );
            assert_eq!(
                fixture
                    .journal
                    .lock()
                    .unwrap()
                    .get(request.intent_id)
                    .unwrap()
                    .phase,
                Phase::Prepared
            );
            assert_eq!(fixture.durable_bytes(), before);
            assert_eq!(fixture.fs.ns().save(), namespace_before);
            let publisher = fixture.fs.record_publisher.as_ref().unwrap();
            let key = RecordRepositoryKey {
                workspace_id: request.review.workspace_id,
                record_id: request.review.record_id,
            };
            assert!(
                matches!(repository.apply_batch(key,&proposed.participant_batches[0].envelopes,&publisher.local_authorizer(key.record_id)),Err(crate::record_repository::RecordRepositoryError::RestoreRecoveryRequired(id)) if id==request.intent_id)
            );
            fixture.endpoint.close().await;
        }
    }
    // Phase `checked_restore_startup_completion_io_failure_remains_retryable_and_fenced`.
    {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        RESTORE_FAULTS
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap()
            .insert(request.intent_id, TestFault::Participant);
        assert_eq!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::RecoveryPending {
                intent_id: request.intent_id
            }
        );
        let records_path = fixture.directory.path().join("records");
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&records_path).unwrap());
        repository
            .restore_admission_fences(&fixture.journal.lock().unwrap())
            .unwrap();
        let materializer = crate::record_materializer::RecordMaterializer::new(
            request.review.workspace_id,
            fixture.fs.namespace.clone(),
            Some(fixture.history.clone()),
        )
        .with_transaction_recovery(
            fixture.journal.clone(),
            fixture.directory.path().join("namespace.am"),
        );
        repository.fail_writes(true);
        assert!(matches!(
            materializer.reconcile_repository(&repository),
            Err(
                crate::record_materializer::RecordMaterializerError::TransactionRecoveryUnavailable
            )
        ));
        assert_eq!(
            fixture
                .journal
                .lock()
                .unwrap()
                .get(request.intent_id)
                .unwrap()
                .phase,
            Phase::Prepared
        );
        let admitted = fixture
            .journal
            .lock()
            .unwrap()
            .get(request.intent_id)
            .unwrap()
            .clone();
        let publisher = fixture.fs.record_publisher.as_ref().unwrap();
        let key = RecordRepositoryKey {
            workspace_id: request.review.workspace_id,
            record_id: request.review.record_id,
        };
        assert!(
            matches!(repository.apply_batch(key,&admitted.participant_batches[0].envelopes,&publisher.local_authorizer(key.record_id)),Err(crate::record_repository::RecordRepositoryError::RestoreRecoveryRequired(id)) if id==request.intent_id)
        );
        repository.fail_writes(false);
        materializer.reconcile_repository(&repository).unwrap();
        assert_eq!(
            fixture
                .journal
                .lock()
                .unwrap()
                .get(request.intent_id)
                .unwrap()
                .phase,
            Phase::BroadcastCommitted
        );
        let publisher = crate::record_publisher::RecordPublisher::new(
            repository.clone(),
            request.review.workspace_id,
            fixture.fs.history.as_ref().unwrap().1,
            ed25519_dalek::SigningKey::from_bytes(&[31; 32]),
        )
        .unwrap();
        let mut later = admitted.transaction.manifest.participants[0]
            .proposed_version
            .clone();
        later.parents = vec![later.id];
        later.id = fs_protocol::FileVersionId::new();
        later.provenance = FileProvenance::Edit;
        publisher.publish_structural_version(&later).unwrap();
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn history_read_complete_paging_and_exact_parent_metadata() {
    use fs_protocol::files::{FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V};
    let fixture = Fixture::new().await;
    for i in 0..25 {
        fixture
            .fs
            .commit_content(fixture.ino, format!("edit-{i}").as_bytes())
            .unwrap();
    }
    let V::Page(first) = fixture
        .fs
        .read_file_history(fixture.ino, Q::Page { cursor: None })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(first.total_versions, 27);
    assert_eq!(first.versions.len(), 12);
    let scope = first.scope;
    let mut versions = first.versions;
    let mut next = first.next_after;
    while let Some(id) = next {
        let V::Page(page) = fixture
            .fs
            .read_file_history(
                fixture.ino,
                Q::Page {
                    cursor: Some((scope.frontier, id)),
                },
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(page.scope, scope);
        versions.extend(page.versions);
        next = page.next_after;
    }
    assert_eq!(versions.len(), 27);
    assert_eq!(
        versions
            .iter()
            .map(|v| v.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        27
    );
    assert!(
        versions
            .windows(2)
            .all(|v| (v[0].timestamp_ms, v[0].id) < (v[1].timestamp_ms, v[1].id))
    );
    let old = versions.iter().find(|v| v.id == fixture.original).unwrap();
    let V::Version {
        scope: actual,
        version,
        ..
    } = fixture
        .fs
        .read_file_history(
            fixture.ino,
            Q::Version {
                frontier: scope.frontier,
                version_id: old.id,
            },
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(actual, scope);
    assert_eq!(&version, old);
    assert!(version.parents.is_empty());
    fixture.endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top to bottom"
)]
async fn history_reads_reject_stale_cursors_and_bind_them_to_live_scope() {
    // Phase `history_read_rejects_stale_wrong_record_missing_cursor_and_revocation`.
    {
        use fs_protocol::files::{
            FileHistoryReadErrorV1 as E, FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V,
        };
        let mut fixture = Fixture::new().await;
        let V::Page(page) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        let mut wrong = page.scope;
        wrong.record_id = fs_protocol::FileRecordId::new();
        wrong.frontier[0] ^= 1;
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Version {
                    frontier: wrong.frontier,
                    version_id: fixture.original
                }
            ),
            Err(E::Stale)
        );
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Page {
                    cursor: Some((page.scope.frontier, fs_protocol::FileVersionId::new()))
                }
            ),
            Err(E::InvalidRequest)
        );
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Version {
                    frontier: page.scope.frontier,
                    version_id: fs_protocol::FileVersionId::new()
                }
            ),
            Err(E::MissingVersion)
        );
        fixture.fs.commit_content(fixture.ino, b"later").unwrap();
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Page {
                    cursor: Some((page.scope.frontier, fixture.original))
                }
            ),
            Err(E::Stale)
        );
        fixture.fs.enforcement = Some((AclHandle::new(crate::acl::WorkspaceAcl::new()), [55; 32]));
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Version {
                    frontier: wrong.frontier,
                    version_id: fixture.original
                }
            ),
            Err(E::NotVisible)
        );
        fixture.endpoint.close().await;
    }
    // Phase `history_read_cursors_bind_workspace_mount_record_and_allow_codec_browsing`.
    {
        use fs_protocol::files::{
            FileHistoryReadErrorV1 as E, FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V,
        };
        let fixture = Fixture::new().await;
        let V::Page(page) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        let query = Q::Version {
            frontier: page.scope.frontier,
            version_id: fixture.original,
        };
        let other = fixture
            .fs
            .namespace
            .lock()
            .unwrap()
            .create_file(crate::namespace::ROOT_INO, "other.bin", None, 0, 1)
            .unwrap();
        fixture.fs.ensure_history_baseline().unwrap();
        assert_eq!(
            fixture.fs.read_file_history(other, query),
            Err(E::Stale),
            "another record must reject the cursor"
        );
        let request = fixture.request();
        assert!(matches!(
            fixture.fs.checked_history_restore(fixture.ino, request),
            Outcome::Applied { .. }
        ));
        let V::Page(fresh) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        let fresh_query = Q::Version {
            frontier: fresh.scope.frontier,
            version_id: fixture.original,
        };
        assert!(
            fixture
                .fs
                .read_file_history(fixture.ino, fresh_query)
                .is_ok()
        );
        let reopened = fixture.reopen();
        let reopened_ino = reopened
            .namespace
            .lock()
            .unwrap()
            .lookup(crate::namespace::ROOT_INO, "test.bin")
            .unwrap()
            .ino
            .0;
        assert_eq!(
            reopened
                .namespace
                .lock()
                .unwrap()
                .record_id_for(reopened_ino),
            Some(fresh.scope.record_id)
        );
        assert_eq!(
            reopened.read_file_history(reopened_ino, fresh_query),
            Err(E::Stale),
            "only remounted lifetime changed after the last successful read"
        );
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.bin",
                crate::namespace::ROOT_INO,
                "test.tilo-doc",
            )
            .unwrap();
        assert!(
            fixture.fs.checked_history_review(fixture.ino).is_ok(),
            "a serialized document is reviewed like any record"
        );
        let V::Page(page) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            fixture
                .fs
                .read_file_history_preview(fixture.ino, page.scope, fixture.original)
                .unwrap(),
            b"first",
            "the record's content kind does not suppress authorized immutable reads"
        );
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn history_reads_keep_exact_bytes_separate_from_current_visibility() {
    // Phase `history_read_immutable_bytes_and_post_hydration_visibility`.
    {
        use fs_protocol::files::{
            FileHistoryReadErrorV1 as E, FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V,
        };
        let mut fixture = Fixture::new().await;
        let mut grants = crate::acl::WorkspaceAcl::new();
        grants.grant_read([55; 32], "/");
        let acl = AclHandle::new(grants);
        fixture.fs.enforcement = Some((acl.clone(), [55; 32]));
        let V::Page(page) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            fixture
                .fs
                .read_file_history_preview(fixture.ino, page.scope, fixture.original)
                .unwrap(),
            b"first"
        );
        assert_eq!(
            fixture.fs.read_file_history_preview_using(
                fixture.ino,
                page.scope,
                fixture.original,
                |_| Ok(b"second".to_vec())
            ),
            Err(E::Unavailable)
        );
        assert_eq!(
            fixture.fs.read_file_history_preview_using(
                fixture.ino,
                page.scope,
                fixture.original,
                |_| {
                    acl.replace(crate::acl::WorkspaceAcl::new());
                    Ok(b"first".to_vec())
                }
            ),
            Err(E::NotVisible)
        );
        let mut grants = crate::acl::WorkspaceAcl::new();
        grants.grant_read([55; 32], "/");
        acl.replace(grants);
        assert_eq!(
            fixture.fs.read_file_history_preview_using(
                fixture.ino,
                page.scope,
                fixture.original,
                |_| {
                    let mut replacement =
                        crate::namespace::Namespace::new("retargeted-during-hydration").unwrap();
                    let other = replacement
                        .create_file(crate::namespace::ROOT_INO, "other.bin", None, 0, 1)
                        .unwrap();
                    assert_eq!(
                        other, fixture.ino,
                        "simulate inode reuse during an owner lifetime change"
                    );
                    let record_id = replacement.record_id_for(other).unwrap();
                    let mut version = page.versions[0].clone();
                    version.id = fs_protocol::FileVersionId::new();
                    version.record_id = record_id;
                    version.parents.clear();
                    fixture.history.append(version).unwrap();
                    *fixture.fs.namespace.lock().unwrap() = replacement;
                    Ok(b"first".to_vec())
                }
            ),
            Err(E::Stale),
            "hydrated bytes cannot cross a changed stable record binding"
        );
        fixture.endpoint.close().await;
    }
    // Phase `history_read_backfill_changes_frontier_without_changing_visible_bytes`.
    {
        use fs_protocol::files::{
            FileHistoryReadErrorV1 as E, FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V,
        };
        let fixture = Fixture::new().await;
        let V::Page(page) = fixture
            .fs
            .read_file_history(fixture.ino, Q::Page { cursor: None })
            .unwrap()
        else {
            panic!()
        };
        let mut version = page.versions[0].clone();
        version.id = fs_protocol::FileVersionId::new();
        version.timestamp_ms = 0;
        version.parents = vec![fixture.original];
        fixture.history.append(version).unwrap();
        assert_eq!(
            fixture.fs.read_file_history(
                fixture.ino,
                Q::Page {
                    cursor: Some((page.scope.frontier, fixture.original))
                }
            ),
            Err(E::Stale)
        );
        fixture.endpoint.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn history_read_current_winner_is_separate_from_exact_restore_receipt() {
    use fs_protocol::files::{FileHistoryReadQueryV1 as Q, FileHistoryReadValueV1 as V};
    let fixture = Fixture::new().await;
    let request = fixture.request();
    let applied = fixture.fs.checked_history_restore(fixture.ino, request);
    let Outcome::Applied { receipt } = &applied else {
        panic!("{applied:?}")
    };
    fixture
        .fs
        .commit_content(fixture.ino, b"later converged content")
        .unwrap();
    let V::Page(page) = fixture
        .fs
        .read_file_history(fixture.ino, Q::Page { cursor: None })
        .unwrap()
    else {
        panic!()
    };
    let current = page.current_revision.unwrap();
    assert_eq!(
        current,
        fixture
            .fs
            .namespace
            .lock()
            .unwrap()
            .content_revision_for(fixture.ino)
            .unwrap()
    );
    assert_ne!(current.version_id, receipt.restored_revision.version_id);
    assert_eq!(
        fixture
            .fs
            .checked_history_result(fixture.ino, request.intent_id),
        applied
    );
    assert_eq!(
        fixture
            .fs
            .read_file_history_preview(fixture.ino, page.scope, fixture.original)
            .unwrap(),
        b"first"
    );
    fixture.endpoint.close().await;
}

/// An existing `.qdoc` record with two document versions: v1 holds blocks "alpha" (in
/// part bold) and "beta"; v2, written through the FUSE commit path, extends
/// "beta".
struct TiloDocVersions {
    fixture: Fixture,
    v1: fs_protocol::FileVersionId,
    v1_bytes: Vec<u8>,
    v2_bytes: Vec<u8>,
    alpha: String,
}

impl TiloDocVersions {
    async fn new() -> Self {
        let fixture = Fixture::new().await;
        fixture
            .fs
            .rename_named(
                crate::namespace::ROOT_INO,
                "test.bin",
                crate::namespace::ROOT_INO,
                "a.qdoc",
            )
            .unwrap();
        let record = fixture.fs.ns().record_id_for(fixture.ino).unwrap();
        let mut document = tilo_doc::Document::new();
        let alpha = document.insert_block(None, "paragraph");
        document.splice_text(&alpha, 0, 0, "alpha");
        document.add_mark(&alpha, 0, 3, "strong", true);
        let beta = document.insert_block(Some(&alpha), "paragraph");
        document.splice_text(&beta, 0, 0, "beta");
        let first = document.save();
        assert_eq!(fixture.fs.commit_bytes(fixture.ino, &first), Ok(()));
        let heads = fixture.history.snapshot(record).unwrap().heads;
        assert_eq!(heads.len(), 1);
        let first_version = heads[0];
        document.splice_text(&beta, 4, 0, " gamma");
        let second = document.save();
        assert_eq!(fixture.fs.commit_bytes(fixture.ino, &second), Ok(()));
        Self {
            fixture,
            v1: first_version,
            v1_bytes: first,
            v2_bytes: second,
            alpha,
        }
    }

    fn restore(&self, operation: Operation) -> Outcome {
        let fs = &self.fixture.fs;
        let request = Request {
            schema_version: 1,
            intent_id: uuid::Uuid::new_v4(),
            review: fs.checked_history_review(self.fixture.ino).unwrap(),
            operation,
        };
        fs.checked_history_restore(self.fixture.ino, request)
    }

    fn current(&self) -> Vec<u8> {
        self.fixture
            .fs
            .read_full_blocking(self.fixture.ino)
            .unwrap()
    }
}

fn blocks(bytes: &[u8]) -> Vec<String> {
    let document = tilo_doc::Document::load(bytes).unwrap();
    document
        .blocks_in_order()
        .iter()
        .map(|id| document.block_text(id).unwrap())
        .collect()
}

fn snapshot_of(bytes: &[u8]) -> tilo_doc::DocSnapshot {
    tilo_doc::Document::load(bytes).unwrap().materialize()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restores_tilo_doc_as_new_crdt_revision() {
    let versions = TiloDocVersions::new().await;
    let fixture = &versions.fixture;
    let record = fixture.fs.ns().record_id_for(fixture.ino).unwrap();
    let before = fixture.history.snapshot(record).unwrap();
    let Outcome::Applied { receipt } = versions.restore(Operation::Restore {
        version_id: versions.v1,
    }) else {
        panic!("a tilo-doc restores");
    };

    let restored = versions.current();
    assert_eq!(snapshot_of(&restored), snapshot_of(&versions.v1_bytes));
    assert_eq!(blocks(&restored), vec!["alpha", "beta"]);
    assert_ne!(
        restored, versions.v1_bytes,
        "the restore is a new revision, not v1's bytes"
    );
    let mut restored_doc = automerge::AutoCommit::load(&restored).unwrap();
    let v2_heads = automerge::AutoCommit::load(&versions.v2_bytes)
        .unwrap()
        .get_heads();
    for head in &v2_heads {
        assert!(
            restored_doc.get_change_by_hash(head).is_some(),
            "the restored document descends from v2"
        );
    }
    assert_ne!(restored_doc.get_heads(), v2_heads);

    let after = fixture.history.snapshot(record).unwrap();
    assert_eq!(after.versions.len(), before.versions.len() + 1);
    let version = after
        .versions
        .iter()
        .find(|v| v.id == receipt.restored_revision.version_id)
        .unwrap();
    assert_eq!(
        version.provenance,
        FileProvenance::Restore { from: versions.v1 }
    );
    assert_eq!(version.parents, before.heads);
    assert_eq!(
        version.content_hash,
        fixture
            .fs
            .ns()
            .hash_for(fixture.ino)
            .map(|hash| ContentHash(*hash.as_bytes()))
    );
    assert_eq!(receipt.restored_revision.byte_len, restored.len() as u64);
    assert_eq!(
        fixture
            .fs
            .history_version_preview(fixture.ino, version.id)
            .unwrap(),
        restored,
        "the restored revision is a fetchable payload"
    );

    // Undo is a restore of the prior revision, along the same CRDT route.
    let Outcome::Applied { receipt: undone } = versions.restore(Operation::Undo {
        restore_intent: receipt.intent_id,
    }) else {
        panic!("the tilo-doc restore undoes");
    };
    let current = versions.current();
    assert_eq!(snapshot_of(&current), snapshot_of(&versions.v2_bytes));
    let mut current_doc = automerge::AutoCommit::load(&current).unwrap();
    for head in restored_doc.get_heads() {
        assert!(current_doc.get_change_by_hash(&head).is_some());
    }
    assert_eq!(undone.prior_revision, receipt.restored_revision);
    fixture.endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_preserves_concurrent_peer_edit() {
    let versions = TiloDocVersions::new().await;
    // A peer holding v2 extends "alpha" while the restore happens here.
    let peer_bytes = {
        let mut peer = tilo_doc::Document::load(&versions.v2_bytes).unwrap();
        peer.splice_text(&versions.alpha, 5, 0, "!");
        peer.save()
    };
    assert!(matches!(
        versions.restore(Operation::Restore {
            version_id: versions.v1
        }),
        Outcome::Applied { .. }
    ));
    let mut merged = automerge::AutoCommit::load(&versions.current()).unwrap();
    let mut peer = automerge::AutoCommit::load(&peer_bytes).unwrap();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut merged, &mut peer).unwrap();
    assert_eq!(
        blocks(&merged.save()),
        vec!["alpha!", "beta"],
        "the peer's edit survives the restore and the restore survives the merge"
    );
    versions.fixture.endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_file_with_document_bytes_restores() {
    let fixture = Fixture::new().await;
    // `test.bin` is opaque by name, whatever its bytes parse as.
    let bytes = tilo_doc::Document::new().save();
    fixture.fs.commit_content(fixture.ino, &bytes).unwrap();
    let request = fixture.request();
    let Outcome::Applied { receipt } = fixture.fs.checked_history_restore(fixture.ino, request)
    else {
        panic!("an opaque file restores its bytes");
    };
    assert_eq!(
        fixture.fs.read_full_blocking(fixture.ino).unwrap(),
        b"first"
    );
    let state = fixture.history.snapshot(request.review.record_id).unwrap();
    let original = state
        .versions
        .iter()
        .find(|v| v.id == fixture.original)
        .unwrap();
    assert_eq!(
        receipt.restored_revision.content_hash, original.content_hash,
        "an opaque restore puts the selected bytes back"
    );
    fixture.endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn namespace_documents_still_have_no_checked_restore_route() {
    let fixture = Fixture::new().await;
    let document = fixture
        .fs
        .ns()
        .create_document(crate::namespace::ROOT_INO, "report.md", "md", 1)
        .unwrap();
    assert!(matches!(
        fixture.fs.checked_history_review(document),
        Err(Outcome::UnsupportedOwner { .. })
    ));
    fixture.endpoint.close().await;
}

/// Write a checked restore request from another thread, as a FUSE worker
/// would. A request that waited for its download would never answer while
/// `gate` holds the job, so the wait is bounded; on timeout the gate opens
/// so the worker can finish before the failure is reported.
fn submit_unblocked(
    fs: &QuickFS,
    ino: u64,
    request: &[u8],
    gate: &tokio::sync::Semaphore,
) -> Result<(), i32> {
    tokio::task::block_in_place(|| {
        std::thread::scope(|scope| {
            let (answered, answer) = std::sync::mpsc::channel();
            let _worker =
                scope.spawn(move || answered.send(fs.checked_restore_xattr(ino, request)));
            let result = answer.recv_timeout(tilo_test_env::hang_bound());
            if result.is_err() {
                gate.add_permits(1);
            }
            result.expect("the restore request waited for its download")
        })
    })
}

async fn job_settled(
    fs: &QuickFS,
    ino: u64,
    intent: uuid::Uuid,
) -> Option<fs_protocol::files::HistoryJobStateV1> {
    let started = std::time::Instant::now();
    loop {
        let state = fs.history_job(ino, intent);
        if !matches!(
            state,
            Some(fs_protocol::files::HistoryJobStateV1::Hydrating { .. })
        ) {
            return state;
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the history job never settled: {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Move the original version's bytes off this device onto a peer it knows:
/// its content id, the peer, the router serving it and the peer's store.
async fn original_only_on_a_peer(
    fixture: &Fixture,
) -> (
    Hash,
    Endpoint,
    iroh::protocol::Router,
    crate::store::ContentStore,
) {
    let first = fixture
        .history
        .snapshot(fixture.fs.ns().record_id_for(fixture.ino).unwrap())
        .unwrap()
        .versions
        .iter()
        .find(|version| version.id == fixture.original)
        .and_then(|version| version.content_hash)
        .map(|hash| Hash::from(hash.0))
        .unwrap();
    let source = Endpoint::bind(presets::Minimal).await.unwrap();
    let source_store = crate::store::ContentStore::memory();
    assert_eq!(
        source_store.write_content(b"first").await.unwrap(),
        Some(first)
    );
    let router = iroh::protocol::Router::builder(source.clone())
        .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
        .spawn();
    let hydrator = &fixture.fs.hydrator;
    let manifest = hydrator.manifest_index(first).await.unwrap();
    let mut evicted: std::collections::HashSet<Hash> = manifest
        .chunks()
        .iter()
        .map(|chunk| Hash::from(chunk.hash))
        .collect();
    evicted.insert(first);
    hydrator.store.evict_blobs(&evicted).await.unwrap();
    assert_eq!(hydrator.local_len(first).await, None);
    hydrator.add_peer(source.addr());
    (first, source, router, source_store)
}

/// Restoring a version whose bytes only a peer holds never waits for them on
/// the FUSE path. The request answers `EINPROGRESS` while a history job
/// downloads them and other reads proceed; cancelling the job leaves the file
/// and its history untouched; the same request commits once the job is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_does_not_park_fuse_worker() {
    use fs_protocol::files::HistoryJobStateV1 as JobState;
    let fixture = Fixture::new().await;
    let request = fixture.request();
    let record = request.review.record_id;
    let (first, source, router, _source_store) = original_only_on_a_peer(&fixture).await;
    let hydrator = Arc::clone(&fixture.fs.hydrator);

    let gate = fixture.fs.history_jobs.hold();
    let bytes = serde_json::to_vec(&request).unwrap();
    let history_before = fixture.history.snapshot(record).unwrap();
    let namespace_before = fixture.fs.ns().save();

    // Phase `the_request_answers_while_its_content_downloads`.
    assert_eq!(
        submit_unblocked(&fixture.fs, fixture.ino, &bytes, &gate),
        Err(libc::EINPROGRESS)
    );
    assert_eq!(
        fixture.fs.history_job(fixture.ino, request.intent_id),
        Some(JobState::Hydrating { done: 0, total: 0 })
    );
    assert_eq!(
        fixture
            .fs
            .checked_history_result(fixture.ino, request.intent_id),
        Outcome::RecoveryPending {
            intent_id: request.intent_id
        }
    );
    assert_eq!(
        fixture.fs.read_full_blocking(fixture.ino).unwrap(),
        b"second"
    );

    // Phase `cancelling_the_download_writes_nothing`.
    assert_eq!(
        fixture
            .fs
            .cancel_history_job(fixture.ino, request.intent_id),
        Ok(())
    );
    assert_eq!(
        fixture.fs.history_job(fixture.ino, request.intent_id),
        Some(JobState::Cancelled)
    );
    assert_eq!(
        submit_unblocked(&fixture.fs, fixture.ino, &bytes, &gate),
        Err(EIO)
    );
    assert_eq!(fixture.history.snapshot(record).unwrap(), history_before);
    assert_eq!(fixture.fs.ns().save(), namespace_before);
    assert_eq!(fixture.journal.lock().unwrap().retained_count(), 0);
    assert_eq!(hydrator.local_len(first).await, None);

    // Phase `the_same_request_commits_once_its_content_is_local`.
    assert_eq!(
        submit_unblocked(&fixture.fs, fixture.ino, &bytes, &gate),
        Err(libc::EINPROGRESS)
    );
    gate.add_permits(1);
    assert_eq!(
        job_settled(&fixture.fs, fixture.ino, request.intent_id).await,
        Some(JobState::Ready)
    );
    assert_eq!(
        submit_unblocked(&fixture.fs, fixture.ino, &bytes, &gate),
        Ok(())
    );
    let Outcome::Applied { receipt } = fixture
        .fs
        .checked_history_result(fixture.ino, request.intent_id)
    else {
        panic!("the committed restore has a receipt");
    };
    assert_eq!(receipt.selected_version, fixture.original);
    assert_eq!(receipt.restored_revision.byte_len, 5);
    assert_eq!(
        fixture.fs.read_full_blocking(fixture.ino).unwrap(),
        b"first"
    );
    assert_eq!(
        fixture.history.snapshot(record).unwrap().versions.len(),
        history_before.versions.len().saturating_add(1)
    );
    assert_eq!(fixture.fs.history_job(fixture.ino, request.intent_id), None);

    router.shutdown().await.unwrap();
    source.close().await;
    fixture.endpoint.close().await;
}
