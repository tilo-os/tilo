//! Qualified Linux fixtures: compiled/link-only on this host; no ignored or passing skips.
use super::*;
use crate::connection::{
    model::{ConnectionId, Decision, EntryState},
    store::{ConnectionPolicy, EndpointBinding, ReceiptStage},
    transaction::{TransactionOutcome, TransactionSession},
};
use std::{
    cell::Cell,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    rc::Rc,
    sync::atomic::Ordering,
};

struct Fixture {
    tree: tempfile::TempDir,
    roots: [PathBuf; 2],
    endpoints: [PinnedEndpoint; 2],
    store: ConnectionStore,
    id: ConnectionId,
    cancel: Arc<AtomicBool>,
    source: usize,
    leaf: &'static str,
}
impl Fixture {
    fn new(reverse: bool, nonempty: bool, nested: bool) -> Self {
        let tree = super::super::tests::fixture();
        let roots = [tree.path().join("left"), tree.path().join("right")];
        for root in &roots {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("anchor"), b"existing baseline").unwrap();
        }
        let endpoints = roots
            .each_ref()
            .map(|root| super::super::tests::endpoint(root));
        let binding = |path: &std::path::Path| {
            let probe = endpoint::probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let id = ConnectionId([119; 16]);
        let record = store
            .create_connection(
                id,
                binding(&roots[0]),
                binding(&roots[1]),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let session = store.transaction_session(id).unwrap();
        let value = Some(EntryState::File {
            sha256: super::super::Sha256::digest(b"existing baseline").into(),
            bytes: 17,
            executable: false,
        });
        let intent = OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"anchor").unwrap(),
            source_before: value.clone(),
            destination_before: value,
            decision: Decision::VerifyEqual,
        };
        let reviewed =
            super::super::initial_preview(&store, id, &endpoints[0], &endpoints[1], &cancel)
                .unwrap()
                .accept(intent)
                .unwrap();
        assert!(matches!(
            super::super::execute_reviewed_initial(
                &mut store,
                reviewed,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel
            ),
            TransactionOutcome::Verified {
                up_to_date: true,
                ..
            }
        ));
        let source = usize::from(reverse);
        let leaf = if nested { "a/b/dir" } else { "dir" };
        if nested {
            for root in &roots {
                std::fs::create_dir_all(root.join("a/b")).unwrap();
            }
        }
        std::fs::create_dir(roots[source].join(leaf)).unwrap();
        if nonempty {
            std::fs::write(roots[source].join(leaf).join("child"), b"future data").unwrap();
        }
        Self {
            tree,
            roots,
            endpoints,
            store,
            id,
            cancel,
            source,
            leaf,
        }
    }
    fn execute(&mut self) -> TransactionOutcome {
        let session = self.store.transaction_session(self.id).unwrap();
        execute_next_directory(
            &mut self.store,
            self.id,
            &session,
            &self.endpoints[0],
            &self.endpoints[1],
            &self.cancel,
        )
    }
    fn reopen(self) -> Self {
        let Self {
            tree,
            roots,
            endpoints,
            store,
            id,
            cancel,
            source,
            leaf,
        } = self;
        cancel.store(false, Ordering::Release);
        drop(store);
        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        Self {
            tree,
            roots,
            endpoints,
            store,
            id,
            cancel,
            source,
            leaf,
        }
    }
    fn namespace(&self) -> PathBuf {
        std::fs::read_dir(&self.roots[1 - self.source])
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".qos-operation-")
            })
            .expect("actual registered private namespace")
    }
    fn baseline_leaf(&self) -> bool {
        self.store
            .load_baseline(self.id)
            .unwrap()
            .entries()
            .unwrap()
            .get(&RelativePath::unix(self.leaf.as_bytes()).unwrap())
            == Some(&EntryState::Directory)
    }
    fn original_present(&self) {
        assert!(self.roots[self.source].join(self.leaf).is_dir());
        assert_eq!(
            std::fs::read(self.roots[self.source].join("anchor")).unwrap(),
            b"existing baseline"
        );
    }
}
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        DIRECTORY_TEST_HOOK.with(|slot| *slot.borrow_mut() = None);
        witness::set_test_cut(None);
    }
}
fn stop_at(fixture: &Fixture, point: &'static str) -> (Hooks, Rc<Cell<bool>>) {
    let hit = Rc::new(Cell::new(false));
    let seen = Rc::clone(&hit);
    let cancel = Arc::clone(&fixture.cancel);
    DIRECTORY_TEST_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |at| {
            if at == point {
                seen.set(true);
                cancel.store(true, Ordering::Release);
            }
        }));
    });
    (Hooks, hit)
}
#[test]
fn directory_native_executor_both_directions_empty_nonempty_and_safe_prefixes() {
    for reverse in [false, true] {
        for nonempty in [false, true] {
            for nested in [false, true] {
                let mut fixture = Fixture::new(reverse, nonempty, nested);
                let before = fixture
                    .store
                    .load_baseline(fixture.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .clone();
                assert!(
                    matches!(fixture.execute(),TransactionOutcome::Verified{up_to_date,..} if up_to_date!=nonempty)
                );
                assert!(fixture.baseline_leaf());
                fixture.original_present();
                let leaf = fixture.roots[1 - fixture.source].join(fixture.leaf);
                assert!(leaf.is_dir());
                assert_eq!(std::fs::read_dir(&leaf).unwrap().count(), 0);
                assert_eq!(
                    std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                let after = fixture
                    .store
                    .load_baseline(fixture.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .clone();
                for (path, value) in before {
                    assert_eq!(after.get(&path), Some(&value));
                }
                assert_eq!(after.len(), if nested { 4 } else { 2 });
            }
        }
    }
}
#[test]
fn directory_native_every_durable_cut_restarts_or_retains_uncertain_original() {
    for reverse in [false, true] {
        for point in [
            "intent-record",
            "namespace-reserved",
            "namespace",
            "intent-descriptor",
            "stage-created",
            "stage-descriptor",
            "prepared",
            "published-without-outcome",
            "outcome-descriptor",
            "published-record",
            "verified-record",
        ] {
            let mut fixture = Fixture::new(reverse, true, true);
            let (hooks, hit) = stop_at(&fixture, point);
            assert!(matches!(
                fixture.execute(),
                TransactionOutcome::RecoveryRequired { .. }
            ));
            assert!(hit.get(), "unreachable cut {point}");
            drop(hooks);
            let pending = fixture
                .store
                .executor_position(fixture.id)
                .ok()
                .and_then(|p| p.0);
            fixture = fixture.reopen();
            let id = fixture
                .store
                .executor_position(fixture.id)
                .unwrap()
                .0
                .unwrap();
            if let Some(previous) = pending {
                assert_eq!(previous, id);
            }
            let before = fixture.store.native_budget_for_test(id).unwrap();
            let result = fixture.execute();
            if matches!(
                point,
                "namespace" | "intent-descriptor" | "stage-created" | "stage-descriptor"
            ) {
                assert!(matches!(
                    result,
                    TransactionOutcome::RecoveryRequired { .. }
                ));
                assert!(!fixture.baseline_leaf());
                assert!(fixture.namespace().is_dir());
            } else {
                assert!(
                    matches!(
                        result,
                        TransactionOutcome::Verified {
                            up_to_date: false,
                            ..
                        }
                    ),
                    "restart must complete at {point}"
                );
                assert!(fixture.baseline_leaf());
            }
            let after = fixture.store.native_budget_for_test(id).unwrap();
            assert!(after.1 >= before.1);
            fixture.original_present();
        }
    }
}
#[test]
fn directory_native_generation_substitutions_and_competitor_are_refused() {
    for fault in [
        "source",
        "namespace",
        "stage",
        "intent",
        "staged-intent",
        "competitor",
    ] {
        let mut fixture = Fixture::new(false, true, false);
        let (hooks, hit) = stop_at(&fixture, "prepared");
        assert!(matches!(
            fixture.execute(),
            TransactionOutcome::RecoveryRequired { .. }
        ));
        assert!(hit.get());
        drop(hooks);
        let namespace = fixture.namespace();
        let path = match fault {
            "source" => fixture.roots[0].join("dir"),
            "namespace" => namespace.clone(),
            "competitor" => fixture.roots[1].join("dir"),
            name => namespace.join(name),
        };
        if fault == "competitor" {
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("marker"), b"competitor").unwrap();
        } else if matches!(fault, "intent" | "staged-intent") {
            let bytes = std::fs::read(&path).unwrap();
            std::fs::rename(&path, fixture.tree.path().join("retained-descriptor")).unwrap();
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            std::io::Write::write_all(&mut file, &bytes).unwrap();
            file.sync_all().unwrap();
            // Restart may accept identical canonical descriptor bytes with fresh observed inode proof.
        } else {
            std::fs::rename(&path, fixture.tree.path().join("retained-original")).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        fixture = fixture.reopen();
        let result = fixture.execute();
        if matches!(fault, "intent" | "staged-intent") {
            assert!(matches!(result, TransactionOutcome::Verified { .. }));
        } else {
            assert!(matches!(
                result,
                TransactionOutcome::RecoveryRequired { .. }
            ));
            assert!(!fixture.baseline_leaf());
        }
        if fault == "competitor" {
            assert_eq!(std::fs::read(path.join("marker")).unwrap(), b"competitor");
        }
    }
}
#[test]
fn directory_native_created_stage_metadata_children_and_descriptor_identity_refuse() {
    for fault in ["mode", "children", "intent", "staged-intent"] {
        let mut fixture = Fixture::new(false, true, false);
        let target = fixture.roots[1].clone();
        let hit = Rc::new(Cell::new(false));
        let seen = Rc::clone(&hit);
        let _hooks = Hooks;
        if matches!(fault, "mode" | "children") {
            DIRECTORY_TEST_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |point| {
                    if point != "stage-created" {
                        return;
                    }
                    seen.set(true);
                    let namespace = std::fs::read_dir(&target)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .find(|p| {
                            p.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".qos-operation-")
                        })
                        .unwrap();
                    let stage = namespace.join("stage");
                    if fault == "mode" {
                        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o500))
                            .unwrap();
                        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))
                            .unwrap();
                    } else {
                        std::fs::write(stage.join("child"), b"unowned").unwrap();
                        std::fs::remove_file(stage.join("child")).unwrap();
                    }
                }));
            });
        } else {
            witness::set_test_cut(Some(Box::new(move |point, directory, name| {
                if point != "descriptor-closed"
                    || name.to_str().ok() != Some(fault)
                    || seen.replace(true)
                {
                    return;
                }
                let path = PathBuf::from(format!(
                    "/proc/self/fd/{}",
                    super::super::AsRawFd::as_raw_fd(directory.file())
                ))
                .join(fault);
                let bytes = std::fs::read(&path).unwrap();
                std::fs::rename(&path, target.join("retained-descriptor")).unwrap();
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .unwrap();
                std::io::Write::write_all(&mut file, &bytes).unwrap();
                file.sync_all().unwrap();
            })));
        }
        assert!(matches!(
            fixture.execute(),
            TransactionOutcome::RecoveryRequired { .. }
        ));
        assert!(hit.get());
        assert!(!fixture.baseline_leaf());
        fixture.original_present();
    }
}
#[test]
fn directory_native_recovery_only_never_creates_outcome_or_advances_baseline() {
    for point in [
        "intent-record",
        "prepared",
        "published-without-outcome",
        "outcome-descriptor",
        "published-record",
        "verified-record",
    ] {
        let mut fixture = Fixture::new(false, true, false);
        let baseline = fixture
            .store
            .load_baseline(fixture.id)
            .unwrap()
            .entries()
            .unwrap()
            .clone();
        let (hooks, hit) = stop_at(&fixture, point);
        assert!(matches!(
            fixture.execute(),
            TransactionOutcome::RecoveryRequired { .. }
        ));
        assert!(hit.get());
        drop(hooks);
        fixture = fixture.reopen();
        let record = fixture.store.load_connection(fixture.id).unwrap();
        let id = fixture
            .store
            .executor_position(fixture.id)
            .unwrap()
            .0
            .unwrap();
        let published = fixture.roots[1].join("dir").exists();
        let outcome = if point == "intent-record" {
            None
        } else {
            Some(fixture.namespace().join("outcome").exists())
        };
        let session = fixture.store.transaction_session(fixture.id).unwrap();
        fixture
            .store
            .request_disconnect(fixture.id, record.revision)
            .unwrap();
        assert!(
            matches!(execute_next_directory(&mut fixture.store,fixture.id,&session,&fixture.endpoints[0],&fixture.endpoints[1],&fixture.cancel),TransactionOutcome::AccountedPending{operation:Some(receipt)} if receipt.id==id && receipt.stage==ReceiptStage::AccountedPending)
        );
        assert_eq!(fixture.roots[1].join("dir").exists(), published);
        if let Some(outcome) = outcome {
            assert_eq!(fixture.namespace().join("outcome").exists(), outcome);
        }
        assert_eq!(
            fixture
                .store
                .load_baseline(fixture.id)
                .unwrap()
                .entries()
                .unwrap(),
            &baseline
        );
        assert_eq!(fixture.store.executor_position(fixture.id).unwrap().0, None);
    }
}

fn prepared_fixture() -> Fixture {
    let mut fixture = Fixture::new(false, true, true);
    let (hooks, hit) = stop_at(&fixture, "prepared");
    assert!(matches!(
        fixture.execute(),
        TransactionOutcome::RecoveryRequired { .. }
    ));
    assert!(hit.get());
    drop(hooks);
    fixture.reopen()
}
fn continuation<'a>(
    store: &mut ConnectionStore,
    session: &TransactionSession,
    endpoints: &'a [PinnedEndpoint; 2],
    cancel: &'a AtomicBool,
    id: ConnectionId,
) -> (LiveOperation, DirectoryContinuation<'a>) {
    let pending = store.executor_position(id).unwrap().0.unwrap();
    let revision = store.load_connection(id).unwrap().revision;
    let mut context = store.open_recovery_context(pending, revision).unwrap();
    store
        .reserve_recovery_reads(
            &mut context,
            super::super::EXECUTOR_READ_WINDOW,
            Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )
        .unwrap();
    let records = store.recovery_namespace_records(&context).unwrap();
    let record = records
        .iter()
        .find(|record| record.operation() == pending)
        .unwrap();
    let epoch =
        NativeEpoch::begin_directory_recovery(&context, &endpoints[0], &endpoints[1], cancel)
            .unwrap();
    let mut proof = DirectoryContinuation::inspect(epoch, &context, record).unwrap();
    let live = store
        .resume_native_directory(&context, &mut proof, session, cancel)
        .unwrap();
    (live, proof)
}
#[test]
fn directory_native_actual_pass_and_request_exhaustion_are_sticky() {
    for requests in [false, true] {
        let mut fixture = prepared_fixture();
        let session = fixture.store.transaction_session(fixture.id).unwrap();
        let (live, mut proof) = continuation(
            &mut fixture.store,
            &session,
            &fixture.endpoints,
            &fixture.cancel,
            fixture.id,
        );
        if requests {
            let used = proof
                .owner
                .epoch
                .directory_requests
                .as_ref()
                .unwrap()
                .clone();
            while used.load(Ordering::Acquire) < 128 {
                proof
                    .owner
                    .epoch
                    .read_descriptor(&proof.owner.namespace, c"intent", &proof.owner.intent)
                    .unwrap();
            }
            assert!(
                proof
                    .owner
                    .epoch
                    .read_descriptor(&proof.owner.namespace, c"intent", &proof.owner.intent)
                    .is_err()
            );
            assert_eq!(used.load(Ordering::Acquire), 129);
        } else {
            while proof
                .owner
                .epoch
                .witness
                .as_ref()
                .unwrap()
                .passes_for_test()
                < 32
            {
                proof.owner.epoch.witness_checkpoint().unwrap();
            }
            assert!(proof.owner.epoch.witness_checkpoint().is_err());
        }
        assert!(proof.validate_live(&live).is_err());
        assert!(!fixture.baseline_leaf());
    }
}
#[test]
fn directory_native_final_prewrite_and_post_fsync_races_retain_no_ack() {
    for post_sync in [false, true] {
        let mut fixture = prepared_fixture();
        let session = fixture.store.transaction_session(fixture.id).unwrap();
        let (mut live, proof) = continuation(
            &mut fixture.store,
            &session,
            &fixture.endpoints,
            &fixture.cancel,
            fixture.id,
        );
        let mut outcome = proof
            .complete(&fixture.store, &live, &session, &fixture.cancel)
            .unwrap();
        for verified in [false, true] {
            fixture
                .store
                .record_directory_outcome(&mut live, &mut outcome, verified, &fixture.cancel)
                .unwrap();
        }
        let mut comparison = outcome
            .comparison(&fixture.store, &live, &fixture.cancel)
            .unwrap();
        let candidate = fixture
            .store
            .prepare_directory_baseline(&live, &mut comparison, &fixture.cancel)
            .unwrap();
        let target = fixture.roots[1].join(fixture.leaf);
        let retained = fixture.tree.path().join("retained-final-original");
        let hit = Rc::new(Cell::new(false));
        let called = Rc::clone(&hit);
        let _hooks = Hooks;
        crate::connection::store::before_native_baseline_write_for_test(false, move || {
            let mutate = move || {
                std::fs::rename(&target, &retained).unwrap();
                std::fs::create_dir(&target).unwrap();
                called.set(true);
            };
            if post_sync {
                let mut count = 0;
                let mut mutate = Some(mutate);
                witness::set_test_cut(Some(Box::new(move |point, _, _| {
                    if point == "checkpoint" {
                        count += 1;
                        if count == 3 {
                            mutate.take().unwrap()();
                        }
                    }
                })));
            } else {
                mutate();
            }
        });
        assert!(
            fixture
                .store
                .commit_directory_baseline(&mut live, candidate, &mut comparison, &fixture.cancel)
                .is_err()
        );
        assert!(hit.get());
        drop(comparison);
        drop(outcome);
        if post_sync {
            assert!(fixture.store.load_connection(fixture.id).is_err());
        }
        fixture = fixture.reopen();
        assert_eq!(fixture.baseline_leaf(), post_sync);
        assert_eq!(
            fixture
                .store
                .executor_position(fixture.id)
                .unwrap()
                .0
                .is_none(),
            post_sync
        );
    }
}
#[test]
fn directory_native_post_publication_child_is_future_data() {
    let mut fixture = Fixture::new(false, false, false);
    let path = fixture.roots[1].join("dir/child");
    let hit = Rc::new(Cell::new(false));
    let seen = Rc::clone(&hit);
    let _hooks = Hooks;
    DIRECTORY_TEST_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |point| {
            if point == "published-without-outcome" {
                std::fs::write(&path, b"future data").unwrap();
                seen.set(true);
            }
        }));
    });
    assert!(matches!(
        fixture.execute(),
        TransactionOutcome::Verified {
            up_to_date: false,
            ..
        }
    ));
    assert!(hit.get());
    assert!(fixture.baseline_leaf());
    assert!(
        !fixture
            .store
            .load_baseline(fixture.id)
            .unwrap()
            .entries()
            .unwrap()
            .contains_key(&RelativePath::unix(b"dir/child").unwrap())
    );
}
#[test]
fn directory_native_full_owner_schedule_retains_caps_through_final_append() {
    let mut fixture = prepared_fixture();
    let session = fixture.store.transaction_session(fixture.id).unwrap();
    let (mut live, proof) = continuation(
        &mut fixture.store,
        &session,
        &fixture.endpoints,
        &fixture.cancel,
        fixture.id,
    );
    let mut outcome = proof
        .complete(&fixture.store, &live, &session, &fixture.cancel)
        .unwrap();
    for verified in [false, true] {
        fixture
            .store
            .record_directory_outcome(&mut live, &mut outcome, verified, &fixture.cancel)
            .unwrap();
    }
    let mut comparison = outcome
        .comparison(&fixture.store, &live, &fixture.cancel)
        .unwrap();
    let candidate = fixture
        .store
        .prepare_directory_baseline(&live, &mut comparison, &fixture.cancel)
        .unwrap();
    fixture
        .store
        .commit_directory_baseline(&mut live, candidate, &mut comparison, &fixture.cancel)
        .unwrap();
    drop(comparison);
    assert!(
        outcome
            .owner
            .epoch
            .witness
            .as_ref()
            .unwrap()
            .passes_for_test()
            <= 32
    );
    assert!(
        outcome
            .owner
            .epoch
            .directory_requests
            .as_ref()
            .unwrap()
            .load(Ordering::Acquire)
            <= 128
    );
    assert!(outcome.owner.epoch.ancestor_chains <= 256);
    drop(outcome);
    assert!(fixture.baseline_leaf());
}

#[test]
fn directory_native_untouched_recovery_uses_fresh_stamp_and_original_generation() {
    for fault in ["before-capture", "replacement", "during-window"] {
        let mut fixture = Fixture::new(false, false, false);
        let (hooks, hit) = stop_at(&fixture, "intent-record");
        assert!(matches!(
            fixture.execute(),
            TransactionOutcome::RecoveryRequired { .. }
        ));
        assert!(hit.get());
        drop(hooks);
        let source = fixture.roots[fixture.source].join(fixture.leaf);
        if fault == "before-capture" {
            std::fs::write(source.join("child"), b"future data").unwrap();
        } else if fault == "replacement" {
            std::fs::rename(&source, fixture.tree.path().join("retained-source")).unwrap();
            std::fs::create_dir(&source).unwrap();
        }
        fixture = fixture.reopen();
        let observed = Rc::new(Cell::new(false));
        let changed = Rc::clone(&observed);
        let _hooks = Hooks;
        if fault == "during-window" {
            DIRECTORY_TEST_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |point| {
                    if point == "untouched-captured" {
                        std::fs::write(source.join("child"), b"racing data").unwrap();
                        changed.set(true);
                    }
                }));
            });
        }
        let result = fixture.execute();
        if fault == "before-capture" {
            assert!(matches!(
                result,
                TransactionOutcome::Verified {
                    up_to_date: false,
                    ..
                }
            ));
            assert!(fixture.baseline_leaf());
        } else {
            assert!(matches!(
                result,
                TransactionOutcome::RecoveryRequired { .. }
            ));
            assert!(!fixture.baseline_leaf());
            assert!(!fixture.roots[1].join(fixture.leaf).exists());
        }
        if fault == "during-window" {
            assert!(observed.get());
        }
    }
}
