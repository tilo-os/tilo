//! An explicitly reviewed, operationless empty baseline. History is never proof.
use super::{NativeEpoch, PinnedEndpoint, StoreError, witness};
use crate::connection::{
    endpoint,
    model::{ConnectionId, Observation, ObservedTree},
    store::{ConnectionRecord, ConnectionStore, InitialEmptyBaselineV1},
    transaction::{AcceptedEmpty, TransactionOutcome, TransactionSession},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Owns the same retained native root/name pins, receiver and namespace reader
/// as the regular executor, without fabricating an operation or private object.
pub(in crate::connection) struct InitialEmptyProof<'a> {
    accepted: AcceptedEmpty,
    session: &'a TransactionSession,
    epoch: NativeEpoch<'a>,
    roots: witness::NamespaceWitness,
}
fn epoch<'a>(
    store: &ConnectionStore,
    record: &ConnectionRecord,
    session: &'a TransactionSession,
    endpoints: [&'a PinnedEndpoint; 2],
    cancel: &'a AtomicBool,
) -> Result<NativeEpoch<'a>, StoreError> {
    store.executor_session(record.id, session)?;
    session.require_idle()?;
    if cancel.load(Ordering::Acquire) {
        return Err(StoreError::Cancelled);
    }
    if [endpoints[0].identity(), endpoints[1].identity()]
        != [record.source.identity, record.destination.identity]
    {
        return Err(StoreError::IdentityChanged);
    }
    endpoints[0]
        .revalidate_for_write()
        .map_err(|_| StoreError::IdentityChanged)?;
    let mut epoch = NativeEpoch::begin_bound(
        endpoints[0],
        endpoints[1],
        None,
        Arc::new(super::super::OperationReads::new()),
    )
    .map_err(|_| StoreError::RecoveryRequired)?;
    epoch.cancel_read = Some(cancel);
    epoch.stop_read = Some(Arc::clone(&session.gate));
    epoch.check().map_err(|_| StoreError::RecoveryRequired)?;
    Ok(epoch)
}
impl<'a> InitialEmptyProof<'a> {
    fn capture(
        store: &ConnectionStore,
        accepted: AcceptedEmpty,
        session: &'a TransactionSession,
        endpoints: [&'a PinnedEndpoint; 2],
        cancel: &'a AtomicBool,
    ) -> Result<Self, StoreError> {
        accepted.validate(store, session)?;
        let epoch = epoch(store, &accepted.record, session, endpoints, cancel)?;
        let roots =
            witness::NamespaceWitness::empty_roots([&epoch.source_root, &epoch.target_root])
                .map_err(|_| StoreError::RecoveryRequired)?;
        let mut proof = Self {
            accepted,
            session,
            epoch,
            roots,
        };
        proof.fact(store)?;
        Ok(proof)
    }
    #[cfg(test)]
    pub(in crate::connection) fn revoke_for_test(&mut self) {
        self.epoch.cancel();
    }
    pub(in crate::connection) fn fact(
        &mut self,
        store: &ConnectionStore,
    ) -> Result<InitialEmptyBaselineV1, StoreError> {
        let fact = self.accepted.validate(store, self.session)?;
        self.roots
            .check_empty_roots(&mut self.epoch)
            .map_err(|_| StoreError::RecoveryRequired)?;
        // Original comparison digest is retained by the opaque acceptance, and
        // the store reducer joins it to the exact original empty connection.
        self.accepted.validate(store, self.session)?;
        Ok(fact)
    }
}
pub(in crate::connection) fn execute_empty(
    store: &mut ConnectionStore,
    accepted: AcceptedEmpty,
    session: &TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> TransactionOutcome {
    let id = accepted.record.id;
    let result = (|| {
        let mut proof =
            InitialEmptyProof::capture(store, accepted, session, [left, right], cancel)?;
        store.commit_initial_empty(&mut proof)?;
        #[cfg(test)]
        witness::test_cut("initial-empty-committed", &proof.epoch.source_root, c".");
        drop(proof);
        verified_current(store, id, session, left, right, cancel)
    })();
    result.unwrap_or_else(|error| super::executor_error(store, id, error))
}
/// A reopened baseline remains data. A new receiver and retained pins validate
/// the complete observation before the ordinary planner may select a real step.
pub(super) fn observe_current(
    store: &ConnectionStore,
    id: ConnectionId,
    session: &TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> Result<(ObservedTree, ObservedTree), StoreError> {
    store.operationless_baseline(id)?;
    let record = store.load_connection(id)?;
    let mut epoch = epoch(store, &record, session, [left, right], cancel)?;
    let source = endpoint::observe(left, cancel);
    let destination = endpoint::observe(right, cancel);
    epoch.check().map_err(|_| StoreError::RecoveryRequired)?;
    store.executor_session(id, session)?;
    if store.load_connection(id)? != record
        || !matches!(source.observation(), Observation::Complete)
        || !matches!(destination.observation(), Observation::Complete)
    {
        return Err(StoreError::RecoveryRequired);
    }
    Ok((source, destination))
}
/// A separate fresh final observation is required even immediately after commit.
pub(super) fn verified_current(
    store: &ConnectionStore,
    id: ConnectionId,
    session: &TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> super::ExecutorResult {
    store.operationless_baseline(id)?;
    let record = store.load_connection(id)?;
    let mut epoch = epoch(store, &record, session, [left, right], cancel)?;
    let mut roots =
        witness::NamespaceWitness::empty_roots([&epoch.source_root, &epoch.target_root])
            .map_err(|_| StoreError::RecoveryRequired)?;
    roots
        .check_empty_roots(&mut epoch)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let source = endpoint::observe(left, cancel);
    let destination = endpoint::observe(right, cancel);
    if !matches!(source.observation(), Observation::Complete)
        || !matches!(destination.observation(), Observation::Complete)
        || !source.entries().is_empty()
        || !destination.entries().is_empty()
    {
        return Err(StoreError::RecoveryRequired);
    }
    roots
        .check_empty_roots(&mut epoch)
        .map_err(|_| StoreError::RecoveryRequired)?;
    store.executor_session(id, session)?;
    if store.load_connection(id)? != record {
        return Err(StoreError::StaleRevision);
    }
    Ok(TransactionOutcome::VerifiedEmpty {
        revision: record.revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{
        endpoint::probe_endpoint,
        store::{ConnectionPolicy, EndpointBinding},
        transaction::{TransactionOutcome, execute_next_regular},
    };
    use std::os::unix::fs::PermissionsExt;

    struct HookReset;
    impl Drop for HookReset {
        fn drop(&mut self) {
            crate::connection::store::clear_native_baseline_write_for_test();
            crate::connection::store::storage_fault_for_test(None);
            witness::set_test_cut(None);
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        paths: [std::path::PathBuf; 2],
        endpoints: [PinnedEndpoint; 2],
        root: std::path::PathBuf,
        store: ConnectionStore,
        record: ConnectionRecord,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = super::super::tests::fixture();
            let paths = [temp.path().join("left"), temp.path().join("right")];
            for path in &paths {
                std::fs::create_dir(path).unwrap();
            }
            let endpoints = paths
                .each_ref()
                .map(|path| super::super::tests::endpoint(path));
            let [left, right] = paths.each_ref().map(|path| {
                let probe = probe_endpoint(path).unwrap();
                EndpointBinding {
                    identity: probe.identity,
                    locator: probe.locator,
                }
            });
            let root = temp.path().join("store");
            let mut store = ConnectionStore::open_at(&root).unwrap();
            let record = store
                .create_connection(
                    ConnectionId([157; 16]),
                    left,
                    right,
                    ConnectionPolicy {
                        deletion_retention_seconds: 2_592_000,
                    },
                )
                .unwrap()
                .value()
                .clone();
            Self {
                _temp: temp,
                paths,
                endpoints,
                root,
                store,
                record,
            }
        }
        fn accepted(&self, session: &TransactionSession, cancel: &AtomicBool) -> AcceptedEmpty {
            super::super::initial_preview(
                &self.store,
                self.record.id,
                &self.endpoints[0],
                &self.endpoints[1],
                cancel,
            )
            .unwrap()
            .accept_empty(&self.store, session)
            .unwrap()
        }
    }
    #[test]
    fn native_initial_empty_commit_has_no_receipt_and_restart_requires_fresh_observation() {
        let mut f = Fixture::new();
        let cancel = AtomicBool::new(false);
        let session = f.store.transaction_session(f.record.id).unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut f.store,
                f.record.id,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::InitialReview(_)
        ));
        assert!(
            f.store
                .load_baseline(f.record.id)
                .unwrap()
                .entries()
                .is_none()
        );
        let accepted = f.accepted(&session, &cancel);
        let generation = f.store.baseline_checkpoint_generation_for_test();
        f.store.force_baseline_checkpoint_for_test();
        assert!(matches!(
            execute_empty(
                &mut f.store,
                accepted,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::VerifiedEmpty { revision: 2 }
        ));
        assert_eq!(
            f.store.executor_position(f.record.id).unwrap(),
            (None, None)
        );
        assert!(f.store.baseline_checkpoint_generation_for_test() > generation);
        f.store.checkpoint().unwrap();
        drop(f.store);
        let mut store = ConnectionStore::open_at(&f.root).unwrap();
        let session = store.transaction_session(f.record.id).unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                f.record.id,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::VerifiedEmpty { revision: 2 }
        ));
        std::os::unix::fs::symlink("missing", f.paths[0].join("incomplete")).unwrap();
        assert!(!matches!(
            execute_next_regular(
                &mut store,
                f.record.id,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::VerifiedEmpty { .. }
        ));
        assert_eq!(store.executor_position(f.record.id).unwrap(), (None, None));
    }
    #[test]
    fn native_initial_empty_revalidates_root_events_cancel_fence_and_receiver_at_append() {
        for fault in 0..7 {
            let mut f = Fixture::new();
            let cancel = Arc::new(AtomicBool::new(false));
            let session = f.store.transaction_session(f.record.id).unwrap();
            let accepted = f.accepted(&session, &cancel);
            let root = f.paths[0].clone();
            let gate = Arc::clone(&session.gate);
            let stop = Arc::clone(&cancel);
            let reached = Arc::new(AtomicBool::new(false));
            let hit = Arc::clone(&reached);
            let _reset = HookReset;
            if fault == 0 {
                f.store.force_baseline_checkpoint_for_test();
            }
            crate::connection::store::before_native_baseline_write_for_test(
                fault == 6,
                move || {
                    hit.store(true, Ordering::Release);
                    match fault {
                        0 => {
                            std::fs::rename(&root, root.with_extension("retained")).unwrap();
                            std::fs::create_dir(&root).unwrap();
                        }
                        1 => {
                            std::fs::write(root.join("arrived"), b"changed").unwrap();
                        }
                        2 => {
                            std::fs::write(root.join("transient"), b"changed").unwrap();
                            std::fs::remove_file(root.join("transient")).unwrap();
                        }
                        3 => {
                            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000))
                                .unwrap()
                        }
                        4 => stop.store(true, Ordering::Release),
                        5 => gate.close(),
                        6 => {}
                        _ => unreachable!(),
                    }
                },
            );
            assert!(matches!(
                execute_empty(
                    &mut f.store,
                    accepted,
                    &session,
                    &f.endpoints[0],
                    &f.endpoints[1],
                    &cancel
                ),
                TransactionOutcome::RecoveryRequired {
                    operation: None,
                    ..
                }
            ));
            assert!(
                reached.load(Ordering::Acquire),
                "actual append cut reached {fault}"
            );
            if fault == 3 {
                std::fs::set_permissions(&f.paths[0], std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            drop(f.store);
            let store = ConnectionStore::open_at(&f.root).unwrap();
            assert!(
                store
                    .load_baseline(f.record.id)
                    .unwrap()
                    .entries()
                    .is_none(),
                "no append after refusal {fault}"
            );
            assert_eq!(store.executor_position(f.record.id).unwrap(), (None, None));
        }
    }
    #[test]
    fn native_initial_empty_changed_preview_offline_cancel_and_disconnect_cannot_commit() {
        for fault in 0..5 {
            let mut f = Fixture::new();
            let cancel = AtomicBool::new(false);
            let session = f.store.transaction_session(f.record.id).unwrap();
            let accepted = f.accepted(&session, &cancel);
            match fault {
                0 => std::fs::write(f.paths[0].join("new"), b"new").unwrap(),
                1 => std::os::unix::fs::symlink("missing", f.paths[0].join("incomplete")).unwrap(),
                2 => std::fs::rename(&f.paths[0], f.paths[0].with_extension("offline")).unwrap(),
                3 => cancel.store(true, Ordering::Release),
                4 => {
                    f.store
                        .request_disconnect(f.record.id, f.record.revision)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                execute_empty(
                    &mut f.store,
                    accepted,
                    &session,
                    &f.endpoints[0],
                    &f.endpoints[1],
                    &cancel
                ),
                TransactionOutcome::RecoveryRequired {
                    operation: None,
                    ..
                }
            ));
            assert!(
                f.store
                    .load_baseline(f.record.id)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert_eq!(
                f.store.executor_position(f.record.id).unwrap(),
                (None, None)
            );
        }
    }
    #[test]
    fn native_initial_empty_failed_fsync_ack_reopens_genuine_baseline_without_receipt() {
        let mut f = Fixture::new();
        let cancel = AtomicBool::new(false);
        let session = f.store.transaction_session(f.record.id).unwrap();
        let accepted = f.accepted(&session, &cancel);
        let _reset = HookReset;
        crate::connection::store::storage_fault_for_test(Some(0));
        assert!(matches!(
            execute_empty(
                &mut f.store,
                accepted,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::RecoveryRequired {
                operation: None,
                ..
            }
        ));
        crate::connection::store::storage_fault_for_test(None);
        assert!(
            f.store.load_connection(f.record.id).is_err(),
            "uncertain acknowledgement poisons the live owner"
        );
        drop(f.store);
        let mut store = ConnectionStore::open_at(&f.root).unwrap();
        // The injected error follows a real fsync; no error acknowledgement is
        // allowed to roll back this discoverable, genuinely verified fact.
        assert_eq!(
            store
                .load_connection(f.record.id)
                .unwrap()
                .baseline_revision,
            Some(2)
        );
        assert_eq!(store.executor_position(f.record.id).unwrap(), (None, None));
        let session = store.transaction_session(f.record.id).unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                f.record.id,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::VerifiedEmpty { revision: 2 }
        ));
    }
    #[test]
    fn native_initial_empty_final_observation_refuses_postcommit_arrival() {
        let mut f = Fixture::new();
        let cancel = AtomicBool::new(false);
        let session = f.store.transaction_session(f.record.id).unwrap();
        let accepted = f.accepted(&session, &cancel);
        let root = f.paths[0].clone();
        let reached = Arc::new(AtomicBool::new(false));
        let hit = Arc::clone(&reached);
        let _reset = HookReset;
        witness::set_test_cut(Some(Box::new(move |cut, _, _| {
            if cut == "initial-empty-committed" {
                hit.store(true, Ordering::Release);
                std::fs::write(root.join("after-commit"), b"new arrival").unwrap();
            }
        })));
        assert!(matches!(
            execute_empty(
                &mut f.store,
                accepted,
                &session,
                &f.endpoints[0],
                &f.endpoints[1],
                &cancel
            ),
            TransactionOutcome::RecoveryRequired {
                operation: None,
                ..
            }
        ));
        assert!(reached.load(Ordering::Acquire));
        assert_eq!(
            f.store
                .load_connection(f.record.id)
                .unwrap()
                .baseline_revision,
            Some(2)
        );
        assert_eq!(
            f.store.executor_position(f.record.id).unwrap(),
            (None, None)
        );
        assert_eq!(
            std::fs::read(f.paths[0].join("after-commit")).unwrap(),
            b"new arrival"
        );
        assert!(!f.paths[1].join("after-commit").exists());
    }
}
