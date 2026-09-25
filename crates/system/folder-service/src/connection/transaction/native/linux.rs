//! Handle-relative native primitives. No store receipts, cleanup or public
//! activation lives here. Relocation evidence is never sealed byte recovery.
pub(super) mod directory;
pub(in crate::connection::transaction) mod initial_baseline;
mod lease_process;
mod witness;
use super::descriptor as d;
use crate::connection::endpoint::{self, EndpointError, NativeDirectory, PinnedEndpoint};
use crate::connection::model::{ConnectionId, EndpointIdentity, NativeName, RelativePath};
use crate::connection::{
    store::{LiveOperation, NamespaceReservation, OperationId, StoreError},
    transaction::{StepAdmission, StepKind},
};
use crate::destination_security;
use lease_process::Receiver;
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Weak};
use std::time::Instant;

pub(in crate::connection::transaction) fn maybe_run_lease_receiver(args: &[String]) -> Option<i32> {
    lease_process::maybe_run(args)
}

#[cfg(test)]
pub(in crate::connection::transaction) fn initial_preview(
    store: &crate::connection::store::ConnectionStore,
    id: ConnectionId,
    source: &PinnedEndpoint,
    destination: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<crate::connection::transaction::InitialPreview, StoreError> {
    let source = endpoint::observe(source, cancel);
    let destination = endpoint::observe(destination, cancel);
    crate::connection::transaction::InitialPreview::from_observations(
        store,
        id,
        source,
        destination,
    )
}

#[cfg(test)]
pub(in crate::connection::transaction) fn restore_preview(
    store: &crate::connection::store::ConnectionStore,
    selected: crate::connection::store::RecoveryId,
    target: crate::connection::model::Side,
    conflict: Option<(crate::connection::store::ConflictId, u64)>,
    source: &PinnedEndpoint,
    destination: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<crate::connection::store::RestorePreview, StoreError> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(StoreError::Cancelled);
    }
    let source = endpoint::observe(source, cancel);
    let destination = endpoint::observe(destination, cancel);
    let preview = store.restore_preview(selected, target, conflict, source, destination)?;
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(StoreError::Cancelled);
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::endpoint::{
        Access, EndpointContext, probe_endpoint, verify_selected_write,
    };
    pub(super) fn endpoint(path: &std::path::Path) -> PinnedEndpoint {
        let probe = probe_endpoint(path).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        verify_selected_write(&endpoint).unwrap();
        endpoint
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BaselineFault {
        Healthy,
        Receiver,
        Root,
    }
    #[derive(Clone, Copy)]
    struct BaselineCase {
        checkpoint: bool,
        fault: BaselineFault,
    }
    #[test]
    #[ignore = "authorized Linux filesystem and helper required; execution remains UNRUN"]
    fn native_conflicts_preserve_actual_edit_edit_and_edit_delete_without_publication() {
        conflict_selected_cases(None);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and helper required; execution remains UNRUN"]
    fn native_baseline_commit_rechecks_native_proof_after_preparation() {
        for checkpoint in [false, true] {
            for fault in [
                BaselineFault::Healthy,
                BaselineFault::Receiver,
                BaselineFault::Root,
            ] {
                conflict_selected_cases(Some(BaselineCase { checkpoint, fault }));
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn conflict_selected_cases(baseline_case: Option<BaselineCase>) {
        use crate::connection::{
            model::{Conflict, Decision, EntryState},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, ReceiptStage,
            },
        };
        use std::sync::atomic::AtomicBool;
        for (absent, restart, restore_reverse, restore_restart) in [None, Some(0), Some(1)]
            .into_iter()
            .flat_map(|absent| {
                (0..14)
                    .filter(move |restart| *restart != 12 || absent != Some(0))
                    .map(move |restart| (absent, restart))
            })
            .flat_map(|(absent, restart)| {
                [false, true]
                    .into_iter()
                    .take(if absent.is_none() && restart == 0 {
                        2
                    } else {
                        1
                    })
                    .map(move |reverse| (absent, restart, reverse))
            })
            .flat_map(|(absent, restart, reverse)| {
                (0..74)
                    .filter(move |resume| {
                        (*resume < 3
                            || absent.is_none()
                            || matches!(*resume, 5 | 6)
                            || *resume >= 9)
                            && (*resume < 17
                                || absent.is_none()
                                || matches!(*resume, 26 | 28)
                                || (35..45).contains(resume)
                                || *resume >= 51)
                            && (*resume < 51 || absent.is_some())
                    })
                    .take(if restart == 0 { 74 } else { 1 })
                    .map(move |resume| (absent, restart, reverse, resume))
            })
            .filter(|case| baseline_case.is_none() || *case == (None, 0, false, 0))
        {
            let tree = fixture();
            let paths = [tree.path().join("left"), tree.path().join("right")];
            for path in &paths {
                std::fs::create_dir(path).unwrap();
            }
            let long = vec![b'x'; 300_000];
            let values: [&[u8]; 2] = if restart == 12 {
                [&long, b"destination edit"]
            } else {
                [b"source edit", b"destination edit"]
            };
            for path in &paths {
                std::fs::write(path.join("file"), b"baseline").unwrap();
            }
            let endpoints = [endpoint(&paths[0]), endpoint(&paths[1])];
            let binding = |path: &std::path::Path| {
                let probe = probe_endpoint(path).unwrap();
                EndpointBinding {
                    identity: probe.identity,
                    locator: probe.locator,
                }
            };
            let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            let record = store
                .create_connection(
                    ConnectionId([93; 16]),
                    binding(&paths[0]),
                    binding(&paths[1]),
                    ConnectionPolicy {
                        deletion_retention_seconds: 2_592_000,
                    },
                )
                .unwrap()
                .value()
                .clone();
            let cancel = AtomicBool::new(false);
            let session = store.transaction_session(record.id).unwrap();
            let baseline_value = EntryState::File {
                sha256: Sha256::digest(b"baseline").into(),
                bytes: 8,
                executable: false,
            };
            let initial = OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: record.baseline_revision,
                path: RelativePath::unix(b"file").unwrap(),
                source_before: Some(baseline_value.clone()),
                destination_before: Some(baseline_value.clone()),
                decision: Decision::VerifyEqual,
            };
            let review = initial_preview(&store, record.id, &endpoints[0], &endpoints[1], &cancel)
                .unwrap()
                .accept(initial.clone())
                .unwrap();
            let metadata =
                admission_metadata(&store, &initial, &endpoints[0], &endpoints[1], &cancel)
                    .unwrap();
            let mut initial_live = store
                .begin_native_reviewed(initial, &session, review, metadata)
                .unwrap();
            store
                .reserve_native_work(
                    &mut initial_live,
                    &session,
                    16 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let epoch =
                NativeEpoch::begin_connected(&initial_live, &endpoints[0], &endpoints[1], &cancel)
                    .unwrap();
            let mut equality = epoch.equal_evidence(&initial_live).unwrap();
            store
                .record_native_equal(&mut initial_live, &mut equality, &cancel)
                .unwrap();
            let mut comparison = equality.comparison(&store, &initial_live, &cancel).unwrap();
            let candidate = store
                .prepare_baseline(&initial_live, &mut comparison, &cancel)
                .unwrap();
            store
                .commit_baseline(&mut initial_live, candidate, &mut comparison, &cancel)
                .unwrap();
            drop(comparison);
            drop(equality);
            drop(initial_live);
            let record = store.load_connection(record.id).unwrap();
            for (index, path) in paths.iter().enumerate() {
                if absent == Some(index) {
                    std::fs::remove_file(path.join("file")).unwrap();
                } else {
                    std::fs::write(path.join("file"), values[index]).unwrap();
                }
            }
            let state = |index: usize| {
                (absent != Some(index)).then(|| EntryState::File {
                    sha256: Sha256::digest(values[index]).into(),
                    bytes: values[index].len() as u64,
                    executable: false,
                })
            };
            let proposal = OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: record.baseline_revision,
                path: RelativePath::unix(b"file").unwrap(),
                source_before: state(0),
                destination_before: state(1),
                decision: Decision::Conflict(if absent.is_some() {
                    Conflict::EditDelete
                } else {
                    Conflict::DivergentEdits
                }),
            };
            let session = store.transaction_session(record.id).unwrap();
            let metadata =
                admission_metadata(&store, &proposal, &endpoints[0], &endpoints[1], &cancel)
                    .unwrap();
            let mut live = store.begin_native(proposal, &session, metadata).unwrap();
            store
                .reserve_native_work(
                    &mut live,
                    &session,
                    32 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let mut capture =
                NativeEpoch::capture_conflict(&store, &live, &endpoints[0], &endpoints[1], &cancel)
                    .unwrap();
            store
                .record_native_capture(&mut live, &mut capture, &cancel)
                .unwrap();
            if matches!(restart, 2 | 6 | 8) {
                store
                    .preserve_native_conflict(&mut live, &mut capture, &cancel)
                    .unwrap();
            }
            if matches!(restart, 3 | 13) {
                crate::connection::store::after_native_bytes_for_test(|| {
                    panic!("injected stop after real durable Bytes, before NativeBound")
                });
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || store.preserve_native_conflict(&mut live, &mut capture, &cancel)
                    ))
                    .is_err()
                );
                assert_eq!(
                    store
                        .recovery_page(record.id, None, 8)
                        .unwrap()
                        .entries
                        .len(),
                    1
                );
            }
            if restart >= 7 {
                let current = store.load_connection(record.id).unwrap();
                store
                    .request_disconnect(record.id, current.revision)
                    .unwrap();
                assert!(session.require_idle().is_err());
                assert!(
                    NativeEpoch::capture_conflict(
                        &store,
                        &live,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel
                    )
                    .is_err(),
                    "no fresh snapshot after the fence"
                );
                for kind in [
                    StepKind::Namespace,
                    StepKind::Stage,
                    StepKind::Relocate,
                    StepKind::Preserve,
                    StepKind::Publish,
                ] {
                    assert!(session.admit_step(&store, &live, kind, &cancel).is_err());
                }
            }
            if restart != 0 {
                let id = live.id();
                store.checkpoint().unwrap();
                drop(capture);
                drop(live);
                drop(store);
                let damaged_side = match restart {
                    4 | 9 | 11 => Some(0),
                    5 | 10 => Some(1),
                    _ => None,
                };
                if let Some(side) = damaged_side {
                    if absent != Some(side) && restart != 11 {
                        std::fs::rename(
                            paths[side].join("file"),
                            paths[side].join("retained-original"),
                        )
                        .unwrap();
                    }
                    std::fs::write(
                        paths[side].join("file"),
                        if restart == 11 {
                            b"changed at-name bytes".as_slice()
                        } else if absent == Some(side) {
                            b"new competitor".as_slice()
                        } else {
                            values[side]
                        },
                    )
                    .unwrap();
                }
                let recovery_directory = tree.path().join("store/recovery").join("5d".repeat(16));
                if restart == 6 {
                    let name = format!("{}-0", id.serial);
                    std::fs::rename(
                        recovery_directory.join(&name),
                        recovery_directory.join("retained-original-artifact"),
                    )
                    .unwrap();
                    std::fs::write(recovery_directory.join(name), b"replacement private bytes")
                        .unwrap();
                }
                store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                let current = store.load_connection(record.id).unwrap();
                let mut context = store.open_recovery_context(id, current.revision).unwrap();
                store
                    .reserve_recovery_reads(
                        &mut context,
                        32 * 1024 * 1024,
                        Instant::now() + std::time::Duration::from_secs(30),
                        &cancel,
                    )
                    .unwrap();
                let recovered = NativeEpoch::recover_conflict(
                    &store,
                    &context,
                    &endpoints[0],
                    &endpoints[1],
                    &cancel,
                );
                if let Some(side) = damaged_side {
                    assert!(recovered.is_err());
                    if absent != Some(side) && restart != 11 {
                        assert_eq!(
                            std::fs::read(paths[side].join("retained-original")).unwrap(),
                            values[side]
                        );
                    }
                    assert_eq!(
                        std::fs::read(paths[side].join("file")).unwrap(),
                        if restart == 11 {
                            b"changed at-name bytes".as_slice()
                        } else if absent == Some(side) {
                            b"new competitor".as_slice()
                        } else {
                            values[side]
                        }
                    );
                    let other = 1 - side;
                    if absent == Some(other) {
                        assert!(!paths[other].join("file").exists());
                    } else {
                        assert_eq!(
                            std::fs::read(paths[other].join("file")).unwrap(),
                            values[other]
                        );
                    }
                    assert!(
                        store
                            .recovery_page(record.id, None, 8)
                            .unwrap()
                            .entries
                            .is_empty()
                    );
                    assert!(
                        store
                            .pending_page(record.id, None, 8)
                            .unwrap()
                            .entries
                            .iter()
                            .any(
                                |receipt| receipt.id == id && receipt.stage == ReceiptStage::Intent
                            )
                    );
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    continue;
                }
                capture = recovered.unwrap();
                if restart >= 7 {
                    assert!(store.transaction_session(record.id).is_err());
                    assert!(context.recovery_only());
                    assert!(
                        store
                            .resume_native_conflict(&context, &mut capture, &session, &cancel)
                            .is_err()
                    );
                    if restart == 12 {
                        crate::connection::store::after_capture_chunk_for_test(|| {
                            panic!("injected stop after actual first private prefix")
                        });
                        assert!(
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                || store.drain_native_capture(&mut context, &mut capture, &cancel)
                            ))
                            .is_err()
                        );
                        assert_eq!(
                            std::fs::read(recovery_directory.join(format!("{}-0", id.serial)))
                                .unwrap(),
                            &long[..256 * 1024]
                        );
                        store.checkpoint().unwrap();
                        drop(capture);
                        drop(context);
                        drop(store);
                        store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        let current = store.load_connection(record.id).unwrap();
                        context = store.open_recovery_context(id, current.revision).unwrap();
                        store
                            .reserve_recovery_reads(
                                &mut context,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        capture = NativeEpoch::recover_conflict(
                            &store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                    }
                    store
                        .drain_native_capture(&mut context, &mut capture, &cancel)
                        .unwrap();
                    let before = context.revision();
                    let entries = store.recovery_page(record.id, None, 8).unwrap().entries;
                    store
                        .drain_native_capture(&mut context, &mut capture, &cancel)
                        .unwrap();
                    assert_eq!(context.revision(), before);
                    assert_eq!(
                        store.recovery_page(record.id, None, 8).unwrap().entries,
                        entries
                    );
                    assert!(
                        context
                            .gate()
                            .stopped()
                            .load(std::sync::atomic::Ordering::Acquire)
                    );
                    assert_eq!(
                        entries.len(),
                        (if absent.is_some() { 1 } else { 2 }) + usize::from(restart == 12)
                    );
                    for entry in &entries {
                        if entry.state
                            == crate::connection::store::RecoveryState::RetainedIncomplete
                        {
                            assert_eq!(restart, 12);
                            assert_eq!(entry.id.slot, 0);
                            assert_eq!(
                                std::fs::read(recovery_directory.join(format!("{}-0", id.serial)))
                                    .unwrap(),
                                &long[..256 * 1024]
                            );
                            continue;
                        }
                        let index = usize::from(
                            entry.original_side != crate::connection::model::Side::Source,
                        );
                        assert_eq!(
                            std::fs::read(
                                recovery_directory.join(format!(
                                    "{}-{}",
                                    entry.id.operation.serial, entry.id.slot
                                ))
                            )
                            .unwrap(),
                            values[index]
                        );
                    }
                    for (index, path) in paths.iter().enumerate() {
                        if absent == Some(index) {
                            assert!(!path.join("file").exists());
                        } else {
                            assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                        }
                    }
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    let current = store.load_connection(record.id).unwrap();
                    let fence = store
                        .request_disconnect(record.id, current.revision)
                        .unwrap();
                    assert!(
                        store.finish_disconnect(&fence).is_err(),
                        "private accounting cannot erase pending uncertainty"
                    );
                    store.checkpoint().unwrap();
                    drop(capture);
                    drop(store);
                    let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    assert_eq!(
                        store.load_connection(record.id).unwrap().lifecycle,
                        crate::connection::store::Lifecycle::Disconnecting
                    );
                    assert!(
                        store
                            .pending_page(record.id, None, 8)
                            .unwrap()
                            .entries
                            .iter()
                            .any(|receipt| receipt.id == id)
                    );
                    assert_eq!(
                        store.recovery_page(record.id, None, 8).unwrap().entries,
                        entries
                    );
                    continue;
                }
                let session = store.transaction_session(record.id).unwrap();
                live = store
                    .resume_native_conflict(&context, &mut capture, &session, &cancel)
                    .unwrap();
                assert!(
                    NativeEpoch::begin_connected(&live, &endpoints[0], &endpoints[1], &cancel)
                        .is_err()
                );
                if restart == 6 {
                    let before = store.recovery_page(record.id, None, 8).unwrap().entries;
                    assert!(
                        store
                            .preserve_native_conflict(&mut live, &mut capture, &cancel)
                            .is_err()
                    );
                    assert_eq!(
                        store.recovery_page(record.id, None, 8).unwrap().entries,
                        before
                    );
                    assert_eq!(
                        std::fs::read(recovery_directory.join(format!("{}-0", id.serial))).unwrap(),
                        b"replacement private bytes"
                    );
                    assert_eq!(
                        std::fs::read(recovery_directory.join("retained-original-artifact"))
                            .unwrap(),
                        values[usize::from(absent == Some(0))]
                    );
                    for (index, path) in paths.iter().enumerate() {
                        if absent == Some(index) {
                            assert!(!path.join("file").exists());
                        } else {
                            assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                        }
                    }
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    continue;
                }
            }
            store
                .preserve_native_conflict(&mut live, &mut capture, &cancel)
                .unwrap();
            let before_repeat = live.revision();
            let slots_before_repeat = store.recovery_page(record.id, None, 8).unwrap().entries;
            store
                .preserve_native_conflict(&mut live, &mut capture, &cancel)
                .unwrap();
            assert_eq!(
                live.revision(),
                before_repeat,
                "paid exact reread must not append another slot or bound fact"
            );
            assert_eq!(
                store.recovery_page(record.id, None, 8).unwrap().entries,
                slots_before_repeat
            );
            store
                .record_native_conflict(&mut live, &mut capture, &cancel)
                .unwrap();
            assert_eq!(
                store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
                ReceiptStage::CapturedConflict
            );
            let entries = store.recovery_page(record.id, None, 8).unwrap().entries;
            assert_eq!(entries.len(), if absent.is_some() { 1 } else { 2 });
            for entry in entries {
                let bytes = std::fs::read(
                    tree.path()
                        .join("store/recovery")
                        .join("5d".repeat(16))
                        .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot)),
                )
                .unwrap();
                let index =
                    usize::from(entry.original_side != crate::connection::model::Side::Source);
                assert_eq!(bytes, values[index]);
            }
            for (index, path) in paths.iter().enumerate() {
                if absent == Some(index) {
                    assert!(!path.join("file").exists());
                } else {
                    assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                }
                assert_eq!(
                    std::fs::read_dir(path).unwrap().count(),
                    usize::from(absent != Some(index)),
                    "capture makes no endpoint namespace"
                );
            }
            assert_eq!(
                store
                    .load_baseline(record.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&RelativePath::unix(b"file").unwrap()),
                Some(&baseline_value)
            );
            store.checkpoint().unwrap();
            drop(capture);
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert_eq!(
                store.pending_page(record.id, None, 8).unwrap().entries[0].stage,
                ReceiptStage::CapturedConflict
            );
            assert_eq!(
                store
                    .load_baseline(record.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&RelativePath::unix(b"file").unwrap()),
                Some(&baseline_value)
            );
            if restart <= 1 {
                let mut store = store;
                use crate::connection::model::Side as ModelSide;
                let selected_side = if absent == Some(0) || restore_reverse {
                    ModelSide::Destination
                } else {
                    ModelSide::Source
                };
                let target = if selected_side == ModelSide::Source {
                    ModelSide::Destination
                } else {
                    ModelSide::Source
                };
                let target_index = usize::from(target != ModelSide::Source);
                let selected = store
                    .recovery_page(record.id, None, 8)
                    .unwrap()
                    .entries
                    .into_iter()
                    .find(|entry| entry.original_side == selected_side)
                    .unwrap();
                let conflict = store
                    .conflict_record(crate::connection::store::ConflictId {
                        connection: record.id,
                        serial: selected.id.operation.serial,
                    })
                    .unwrap();
                if restore_restart >= 51 {
                    let stopped = (61..69).contains(&restore_restart);
                    let phase = if stopped {
                        restore_restart - 8
                    } else {
                        restore_restart
                    };
                    let choices = store.tombstone_choices(conflict.id).unwrap();
                    assert_eq!(choices.len(), 1);
                    let tombstone_replace = phase == 51 || (53..=58).contains(&phase);
                    let collision_case = restore_restart >= 69;
                    let target_index = if tombstone_replace || collision_case {
                        1 - absent.unwrap()
                    } else {
                        absent.unwrap()
                    };
                    let target = if target_index == 0 {
                        ModelSide::Source
                    } else {
                        ModelSide::Destination
                    };
                    let preview = store
                        .tombstone_preview(
                            choices[0].clone(),
                            target,
                            (conflict.id, conflict.revision),
                            endpoint::observe(&endpoints[0], &cancel),
                            endpoint::observe(&endpoints[1], &cancel),
                        )
                        .unwrap();
                    let metadata = admission_metadata(
                        &store,
                        preview.intent(),
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    )
                    .unwrap();
                    let reviewed = if tombstone_replace {
                        preview.accept_replacement()
                    } else {
                        preview.accept()
                    };
                    let session = store.transaction_session(record.id).unwrap();
                    let mut live = store
                        .begin_native_selected(reviewed, &session, metadata)
                        .unwrap();
                    store
                        .reserve_native_work(
                            &mut live,
                            &session,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    assert!(
                        NativeEpoch::selected_restore(
                            &mut store,
                            &live,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel
                        )
                        .is_err(),
                        "file selection producer must reject a tombstone kind"
                    );
                    let mut restore = NativeEpoch::selected_tombstone(
                        &mut store,
                        &live,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    )
                    .unwrap();
                    if collision_case {
                        let mut collision = restore.into_collision(&store, &live).unwrap();
                        store
                            .record_selected_collision(&mut live, &mut collision, &cancel)
                            .unwrap();
                        let id = live.id();
                        if restore_restart == 71 {
                            crate::connection::store::after_native_bytes_for_test(|| {
                                panic!("tombstone collision Bytes before native binding")
                            });
                            assert!(
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store
                                    .preserve_selected_collision(
                                        &mut live,
                                        &mut collision,
                                        &cancel
                                    )))
                                .is_err()
                            );
                        }
                        if matches!(restore_restart, 70 | 71 | 73) {
                            drop(collision);
                            drop(store);
                            let retained = tree.path().join("retained-tombstone-collision-target");
                            if restore_restart == 73 {
                                std::fs::rename(paths[target_index].join("file"), &retained)
                                    .unwrap();
                                std::fs::write(
                                    paths[target_index].join("file"),
                                    values[target_index],
                                )
                                .unwrap();
                            }
                            store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                            let current = store.load_connection(record.id).unwrap();
                            let mut context =
                                store.open_recovery_context(id, current.revision).unwrap();
                            store
                                .reserve_recovery_reads(
                                    &mut context,
                                    32 * 1024 * 1024,
                                    Instant::now() + std::time::Duration::from_secs(30),
                                    &cancel,
                                )
                                .unwrap();
                            let recovered = NativeEpoch::recover_selected_collision(
                                &mut store,
                                &context,
                                &endpoints[0],
                                &endpoints[1],
                                &cancel,
                            );
                            if restore_restart == 73 {
                                assert!(recovered.is_err());
                                assert_eq!(std::fs::read(&retained).unwrap(), values[target_index]);
                                assert_eq!(
                                    std::fs::read(paths[target_index].join("file")).unwrap(),
                                    values[target_index]
                                );
                                assert!(!paths[1 - target_index].join("file").exists());
                                assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                                assert!(store.selected_collision_accounting(id).unwrap().is_none());
                                assert_eq!(
                                    store
                                        .load_baseline(record.id)
                                        .unwrap()
                                        .entries()
                                        .unwrap()
                                        .get(&RelativePath::unix(b"file").unwrap()),
                                    Some(&baseline_value)
                                );
                                continue;
                            }
                            collision = recovered.unwrap();
                            let session = store.transaction_session(record.id).unwrap();
                            live = store
                                .resume_selected_collision(
                                    &context,
                                    &mut collision,
                                    &session,
                                    &cancel,
                                )
                                .unwrap();
                        }
                        store
                            .preserve_selected_collision(&mut live, &mut collision, &cancel)
                            .unwrap();
                        let counted = store.load_connection(record.id).unwrap();
                        let slots = store.recovery_page(record.id, None, 16).unwrap().entries;
                        store
                            .preserve_selected_collision(&mut live, &mut collision, &cancel)
                            .unwrap();
                        assert_eq!(store.load_connection(record.id).unwrap(), counted);
                        assert_eq!(
                            store.recovery_page(record.id, None, 16).unwrap().entries,
                            slots
                        );
                        let accounted = store
                            .complete_selected_collision(&mut live, &mut collision, &cancel)
                            .unwrap()
                            .value()
                            .clone();
                        assert!(
                            store
                                .tombstone_choices(accounted.id)
                                .unwrap()
                                .contains(&choices[0])
                        );
                        assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                        assert!(store.selected_application(id).unwrap().is_none());
                        let current = store.load_connection(record.id).unwrap();
                        assert_eq!(
                            store.selected_collision_accounting(id).unwrap(),
                            Some(accounted.clone())
                        );
                        assert_eq!(store.load_connection(record.id).unwrap(), current);
                        for (index, path) in paths.iter().enumerate() {
                            if index == target_index {
                                assert_eq!(
                                    std::fs::read(path.join("file")).unwrap(),
                                    values[index]
                                );
                            } else {
                                assert!(!path.join("file").exists());
                            }
                            assert_eq!(
                                std::fs::read_dir(path).unwrap().count(),
                                usize::from(index == target_index)
                            );
                        }
                        for entry in store.recovery_page(record.id, None, 16).unwrap().entries {
                            assert_eq!(
                                std::fs::read(
                                    tree.path()
                                        .join("store/recovery")
                                        .join("5d".repeat(16))
                                        .join(format!(
                                            "{}-{}",
                                            entry.id.operation.serial, entry.id.slot
                                        ))
                                )
                                .unwrap(),
                                values[target_index]
                            );
                        }
                        drop(collision);
                        store.checkpoint().unwrap();
                        drop(store);
                        store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        assert!(
                            store
                                .tombstone_choices(accounted.id)
                                .unwrap()
                                .contains(&choices[0])
                        );
                        if restore_restart == 72 {
                            let preview = store
                                .tombstone_preview(
                                    choices[0].clone(),
                                    target,
                                    (accounted.id, accounted.revision),
                                    endpoint::observe(&endpoints[0], &cancel),
                                    endpoint::observe(&endpoints[1], &cancel),
                                )
                                .unwrap();
                            let metadata = admission_metadata(
                                &store,
                                preview.intent(),
                                &endpoints[0],
                                &endpoints[1],
                                &cancel,
                            )
                            .unwrap();
                            let session = store.transaction_session(record.id).unwrap();
                            let mut next = store
                                .begin_native_selected(
                                    preview.accept_replacement(),
                                    &session,
                                    metadata,
                                )
                                .unwrap();
                            store
                                .reserve_native_work(
                                    &mut next,
                                    &session,
                                    32 * 1024 * 1024,
                                    Instant::now() + std::time::Duration::from_secs(30),
                                    &cancel,
                                )
                                .unwrap();
                            let mut restore = NativeEpoch::selected_tombstone(
                                &mut store,
                                &next,
                                &endpoints[0],
                                &endpoints[1],
                                &cancel,
                            )
                            .unwrap();
                            restore
                                .prepare(&mut store, &mut next, &session, &cancel)
                                .unwrap();
                            let name = restore.namespace_name().unwrap().to_owned();
                            restore
                                .apply_tombstone(&mut store, &mut next, &session, &cancel)
                                .unwrap();
                            store.finish_selected(&mut next, restore, &cancel).unwrap();
                            assert!(!paths[target_index].join("file").exists());
                            assert_eq!(
                                std::fs::read(paths[target_index].join(name).join("displaced"))
                                    .unwrap(),
                                values[target_index]
                            );
                        }
                        assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                        assert!(!store.conflict_record(accounted.id).unwrap().resolved);
                        assert_eq!(
                            store
                                .load_baseline(record.id)
                                .unwrap()
                                .entries()
                                .unwrap()
                                .get(&RelativePath::unix(b"file").unwrap()),
                            Some(&baseline_value)
                        );
                        continue;
                    }
                    restore
                        .prepare(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    let namespace = paths[target_index].join(restore.namespace_name().unwrap());
                    assert!(!namespace.join("stage").exists());
                    assert!(!namespace.join("staged-intent").exists());
                    if phase >= 53 {
                        if (54..=58).contains(&phase) {
                            restore
                                .relocate_selected(&mut store, &mut live, &session, &cancel)
                                .unwrap();
                        }
                        if phase == 55 {
                            crate::connection::store::after_native_bytes_for_test(|| {
                                panic!("tombstone Bytes before native binding")
                            });
                            assert!(
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| restore
                                    .preserve_displaced(&mut store, &mut live, &session, &cancel)))
                                .is_err()
                            );
                        }
                        if (56..=58).contains(&phase) {
                            restore
                                .preserve_displaced(&mut store, &mut live, &session, &cancel)
                                .unwrap();
                        }
                        if matches!(phase, 57 | 58 | 60) {
                            let step = session
                                .admit_selected_step(
                                    &store,
                                    &live,
                                    &mut restore,
                                    StepKind::Publish,
                                    &cancel,
                                )
                                .unwrap();
                            restore.epoch.check_absent(&restore.target).unwrap();
                            restore.published = true;
                            restore
                                .write_selected_outcome(&store, &live, &step)
                                .unwrap();
                            drop(step);
                            if phase == 58 {
                                store
                                    .record_selected_outcome(
                                        &mut live,
                                        &mut restore,
                                        false,
                                        &cancel,
                                    )
                                    .unwrap();
                                store
                                    .record_selected_outcome(&mut live, &mut restore, true, &cancel)
                                    .unwrap();
                            }
                        }
                        let id = live.id();
                        drop(restore);
                        drop(store);
                        store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        // This old session is retained solely to assert that the
                        // subsequently closed gate cannot admit active work.
                        let session = store.transaction_session(record.id).unwrap();
                        let current = store.load_connection(record.id).unwrap();
                        let fence = if stopped {
                            Some(
                                store
                                    .request_disconnect(record.id, current.revision)
                                    .unwrap(),
                            )
                        } else {
                            None
                        };
                        let current = store.load_connection(record.id).unwrap();
                        let mut context =
                            store.open_recovery_context(id, current.revision).unwrap();
                        store
                            .reserve_recovery_reads(
                                &mut context,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        if let Some(fence) = fence {
                            assert!(matches!(
                                store.transaction_session(record.id),
                                Err(StoreError::Disconnected)
                            ));
                            assert!(
                                NativeEpoch::recover_selected_tombstone(
                                    &mut store,
                                    &context,
                                    &endpoints[0],
                                    &endpoints[1],
                                    &cancel
                                )
                                .is_err()
                            );
                            let mut boundary = NativeEpoch::inspect_selected_boundary(
                                &mut store,
                                &context,
                                &endpoints[0],
                                &endpoints[1],
                                &cancel,
                            )
                            .unwrap();
                            if (54..=58).contains(&phase) {
                                let mut reader = boundary.drain_reader(&store, &context).unwrap();
                                assert!(
                                    store
                                        .drain_native_recovery(&mut context, &mut reader, &cancel)
                                        .is_err()
                                );
                                store
                                    .drain_selected_recovery(&mut context, &mut reader, &cancel)
                                    .unwrap();
                                let accounted = store.load_connection(record.id).unwrap();
                                let slots =
                                    store.recovery_page(record.id, None, 16).unwrap().entries;
                                store
                                    .drain_selected_recovery(&mut context, &mut reader, &cancel)
                                    .unwrap();
                                assert_eq!(store.load_connection(record.id).unwrap(), accounted);
                                assert_eq!(
                                    store.recovery_page(record.id, None, 16).unwrap().entries,
                                    slots
                                );
                                assert_eq!(
                                    std::fs::read(namespace.join("displaced")).unwrap(),
                                    values[target_index]
                                );
                            }
                            store
                                .record_selected_boundary(&context, &mut boundary, &cancel)
                                .unwrap();
                            assert!(session.require_idle().is_err());
                            assert!(store.selected_application(id).unwrap().is_none());
                            assert_eq!(
                                store
                                    .pending_page(record.id, None, 16)
                                    .unwrap()
                                    .entries
                                    .into_iter()
                                    .find(|receipt| receipt.id == id)
                                    .unwrap()
                                    .stage,
                                ReceiptStage::AccountedPending
                            );
                            store.finish_disconnect(&fence).unwrap();
                            if phase == 53 {
                                assert_eq!(
                                    std::fs::read(paths[target_index].join("file")).unwrap(),
                                    values[target_index]
                                );
                            } else {
                                assert!(!paths[target_index].join("file").exists());
                            }
                            if !tombstone_replace {
                                assert_eq!(
                                    std::fs::read(paths[1 - target_index].join("file")).unwrap(),
                                    values[1 - target_index]
                                );
                            }
                            for entry in store.recovery_page(record.id, None, 16).unwrap().entries {
                                assert_eq!(
                                    std::fs::read(
                                        tree.path()
                                            .join("store/recovery")
                                            .join("5d".repeat(16))
                                            .join(format!(
                                                "{}-{}",
                                                entry.id.operation.serial, entry.id.slot
                                            ))
                                    )
                                    .unwrap(),
                                    values[usize::from(entry.original_side != ModelSide::Source)]
                                );
                            }
                            assert!(!namespace.join("stage").exists());
                            assert!(!namespace.join("staged-intent").exists());
                            assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                            assert_eq!(
                                store
                                    .load_baseline(record.id)
                                    .unwrap()
                                    .entries()
                                    .unwrap()
                                    .get(&RelativePath::unix(b"file").unwrap()),
                                Some(&baseline_value)
                            );
                            store.checkpoint().unwrap();
                            drop(store);
                            let store =
                                ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                            assert_eq!(
                                store.load_connection(record.id).unwrap().lifecycle,
                                crate::connection::store::Lifecycle::Disconnected
                            );
                            assert_eq!(
                                store
                                    .pending_page(record.id, None, 16)
                                    .unwrap()
                                    .entries
                                    .into_iter()
                                    .find(|receipt| receipt.id == id)
                                    .unwrap()
                                    .stage,
                                ReceiptStage::AccountedPending
                            );
                            assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                            assert_eq!(
                                store
                                    .load_baseline(record.id)
                                    .unwrap()
                                    .entries()
                                    .unwrap()
                                    .get(&RelativePath::unix(b"file").unwrap()),
                                Some(&baseline_value)
                            );
                            continue;
                        }
                        restore = NativeEpoch::recover_selected_tombstone(
                            &mut store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        live = store
                            .resume_native_selected(&context, &mut restore, &session, &cancel)
                            .unwrap();
                        assert_eq!(live.id(), id);
                        restore
                            .complete_recovered(&mut store, &mut live, &session, &cancel)
                            .unwrap();
                    } else {
                        restore
                            .apply_tombstone(&mut store, &mut live, &session, &cancel)
                            .unwrap();
                    }
                    let receipt = store
                        .finish_selected(&mut live, restore, &cancel)
                        .unwrap()
                        .value()
                        .clone();
                    assert_eq!(receipt.stage, ReceiptStage::SelectedApplied);
                    assert!(!paths[target_index].join("file").exists());
                    if tombstone_replace {
                        assert_eq!(
                            std::fs::read(namespace.join("displaced")).unwrap(),
                            values[target_index]
                        );
                        let versions = store.recovery_page(record.id, None, 16).unwrap().entries;
                        let copy = versions
                            .iter()
                            .find(|entry| entry.id.operation == live.id())
                            .unwrap();
                        assert_eq!(
                            std::fs::read(
                                tree.path()
                                    .join("store/recovery")
                                    .join("5d".repeat(16))
                                    .join(format!("{}-{}", live.id().serial, copy.id.slot))
                            )
                            .unwrap(),
                            values[target_index]
                        );
                    } else {
                        assert_eq!(
                            std::fs::read(paths[1 - target_index].join("file")).unwrap(),
                            values[1 - target_index]
                        );
                        assert!(!namespace.join("displaced").exists());
                    }
                    let current = store.load_connection(record.id).unwrap();
                    assert_eq!(
                        store.selected_application(live.id()).unwrap(),
                        Some(receipt)
                    );
                    assert_eq!(store.load_connection(record.id).unwrap(), current);
                    assert!(!store.conflict_record(conflict.id).unwrap().resolved);
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    store.checkpoint().unwrap();
                    drop(store);
                    let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    assert!(store.selected_application(live.id()).unwrap().is_some());
                    assert!(!paths[target_index].join("file").exists());
                    assert!(!store.conflict_record(conflict.id).unwrap().resolved);
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    if tombstone_replace {
                        let current = store.load_connection(record.id).unwrap();
                        let equal = OperationIntent {
                            connection: record.id,
                            expected_revision: current.revision,
                            expected_baseline: current.baseline_revision,
                            path: RelativePath::unix(b"file").unwrap(),
                            source_before: None,
                            destination_before: None,
                            decision: Decision::VerifyEqual,
                        };
                        let session = store.transaction_session(record.id).unwrap();
                        let admission = admission_metadata(
                            &store,
                            &equal,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        let mut equal = store.begin_native(equal, &session, admission).unwrap();
                        store
                            .reserve_native_work(
                                &mut equal,
                                &session,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        let epoch = NativeEpoch::begin_connected(
                            &equal,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        let mut evidence = epoch.equal_evidence(&equal).unwrap();
                        store
                            .record_native_equal(&mut equal, &mut evidence, &cancel)
                            .unwrap();
                        let mut compared = evidence.comparison(&store, &equal, &cancel).unwrap();
                        let candidate = store
                            .prepare_baseline(&equal, &mut compared, &cancel)
                            .unwrap();
                        store
                            .commit_baseline(&mut equal, candidate, &mut compared, &cancel)
                            .unwrap();
                        assert!(store.conflict_record(conflict.id).unwrap().resolved);
                        assert!(
                            store
                                .load_baseline(record.id)
                                .unwrap()
                                .entries()
                                .unwrap()
                                .is_empty()
                        );
                        assert_eq!(
                            std::fs::read(namespace.join("displaced")).unwrap(),
                            values[target_index]
                        );
                        drop(compared);
                        drop(evidence);
                        store.checkpoint().unwrap();
                        drop(store);
                        let mut store =
                            ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        let current = store.load_connection(record.id).unwrap();
                        let mut context = store
                            .open_recovery_context(equal.id(), current.revision)
                            .unwrap();
                        store
                            .reserve_recovery_reads(
                                &mut context,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        let mut final_epoch = NativeEpoch::begin_recovery(
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        assert!(
                            final_epoch
                                .final_comparison(&store, &context, &cancel)
                                .unwrap()
                        );
                    }
                    continue;
                }
                let preview = restore_preview(
                    &store,
                    selected.id,
                    target,
                    Some((conflict.id, conflict.revision)),
                    &endpoints[0],
                    &endpoints[1],
                    &cancel,
                )
                .unwrap();
                let metadata = admission_metadata(
                    &store,
                    preview.intent(),
                    &endpoints[0],
                    &endpoints[1],
                    &cancel,
                )
                .unwrap();
                let reviewed = if absent == Some(target_index)
                    || ((17..=25).contains(&restore_restart) || restore_restart >= 45)
                {
                    preview.accept()
                } else {
                    preview.accept_replacement()
                };
                let session = store.transaction_session(record.id).unwrap();
                let mut live = store
                    .begin_native_selected(reviewed, &session, metadata)
                    .unwrap();
                store
                    .reserve_native_work(
                        &mut live,
                        &session,
                        32 * 1024 * 1024,
                        Instant::now() + std::time::Duration::from_secs(30),
                        &cancel,
                    )
                    .unwrap();
                assert!(
                    NativeEpoch::begin_connected(&live, &endpoints[0], &endpoints[1], &cancel)
                        .is_err()
                );
                if restart == 1 {
                    let selected_index = usize::from(selected_side != ModelSide::Source);
                    let retained = paths[selected_index].join("original-generation");
                    std::fs::rename(paths[selected_index].join("file"), &retained).unwrap();
                    std::fs::write(paths[selected_index].join("file"), values[selected_index])
                        .unwrap();
                    assert!(
                        NativeEpoch::selected_restore(
                            &mut store,
                            &live,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel
                        )
                        .is_err(),
                        "same bytes under a different post-Intent generation cannot replace atomic admission history"
                    );
                    assert_eq!(std::fs::read(retained).unwrap(), values[selected_index]);
                    assert_eq!(
                        std::fs::read(paths[selected_index].join("file")).unwrap(),
                        values[selected_index]
                    );
                    assert!(store.native_namespace_records(&live).unwrap().is_empty());
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    continue;
                }
                if (35..45).contains(&restore_restart) {
                    let reserved = if matches!(restore_restart, 36 | 38..=44) {
                        let mut proof = NativeEpoch::selected_restore(
                            &mut store,
                            &live,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        let reservation = store
                            .reserve_selected_namespace(&mut live, &mut proof)
                            .unwrap();
                        proof.validate(&store, &live).unwrap();
                        let name = reservation.name();
                        assert!(!paths[target_index].join(&name).exists());
                        drop(proof);
                        Some(name)
                    } else {
                        None
                    };
                    let id = live.id();
                    store.checkpoint().unwrap();
                    drop(session);
                    drop(store);
                    if matches!(restore_restart, 39 | 40) {
                        let name = paths[target_index].join(reserved.as_ref().unwrap());
                        std::fs::create_dir(&name).unwrap();
                        if restore_restart == 40 {
                            std::fs::write(name.join("unknown"), b"unowned").unwrap();
                        }
                    }
                    let chosen = tree
                        .path()
                        .join("store/recovery")
                        .join("5d".repeat(16))
                        .join(format!(
                            "{}-{}",
                            selected.id.operation.serial, selected.id.slot
                        ));
                    let retained = tree.path().join("untouched-original");
                    let changed = match restore_restart {
                        41 => Some(paths[1 - target_index].join("file")),
                        42 => Some(chosen.clone()),
                        44 => Some(paths[target_index].join("file")),
                        _ => None,
                    };
                    let changed_bytes = changed.as_ref().filter(|path| path.exists()).map(|path| {
                        let bytes = std::fs::read(path).unwrap();
                        std::fs::rename(path, &retained).unwrap();
                        std::fs::write(path, &bytes).unwrap();
                        bytes
                    });
                    let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    let session = store.transaction_session(record.id).unwrap();
                    let current = store.load_connection(record.id).unwrap();
                    let fence = (restore_restart >= 37).then(|| {
                        store
                            .request_disconnect(record.id, current.revision)
                            .unwrap()
                    });
                    let current = store.load_connection(record.id).unwrap();
                    let mut context = store.open_recovery_context(id, current.revision).unwrap();
                    store
                        .reserve_recovery_reads(
                            &mut context,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    if restore_restart == 43 {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let proof = NativeEpoch::inspect_selected_untouched(
                        &mut store,
                        &context,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    );
                    if restore_restart >= 39
                        && !(restore_restart == 44 && absent == Some(target_index))
                    {
                        assert!(proof.is_err());
                        if let Some(bytes) = changed_bytes {
                            assert_eq!(std::fs::read(&retained).unwrap(), bytes);
                            assert_eq!(std::fs::read(changed.unwrap()).unwrap(), bytes);
                        }
                        if restore_restart == 39 {
                            assert_eq!(
                                std::fs::read_dir(
                                    paths[target_index].join(reserved.as_ref().unwrap())
                                )
                                .unwrap()
                                .count(),
                                0
                            );
                        }
                        if restore_restart == 40 {
                            assert_eq!(
                                std::fs::read(
                                    paths[target_index]
                                        .join(reserved.as_ref().unwrap())
                                        .join("unknown")
                                )
                                .unwrap(),
                                b"unowned"
                            );
                        }
                        assert_eq!(
                            store
                                .pending_page(record.id, None, 16)
                                .unwrap()
                                .entries
                                .iter()
                                .find(|entry| entry.id == id)
                                .unwrap()
                                .stage,
                            ReceiptStage::Intent
                        );
                    } else {
                        let mut proof = proof.unwrap();
                        if let Some(fence) = fence {
                            assert!(
                                store
                                    .resume_selected_untouched(
                                        &context, &mut proof, &session, &cancel
                                    )
                                    .is_err()
                            );
                            store
                                .record_selected_untouched(&context, &mut proof, &cancel)
                                .unwrap();
                            store.finish_disconnect(&fence).unwrap();
                            assert_eq!(
                                store.load_connection(record.id).unwrap().lifecycle,
                                crate::connection::store::Lifecycle::Disconnected
                            );
                        } else {
                            live = store
                                .resume_selected_untouched(&context, &mut proof, &session, &cancel)
                                .unwrap();
                            drop(proof);
                            assert_eq!(live.id(), id);
                            let mut restore = NativeEpoch::selected_restore(
                                &mut store,
                                &live,
                                &endpoints[0],
                                &endpoints[1],
                                &cancel,
                            )
                            .unwrap();
                            restore
                                .prepare(&mut store, &mut live, &session, &cancel)
                                .unwrap();
                            let attempts = store
                                .native_namespace_records(&live)
                                .unwrap()
                                .into_iter()
                                .filter(|value| value.operation() == id)
                                .collect::<Vec<_>>();
                            assert_eq!(
                                attempts.len(),
                                1,
                                "reuse the exact absent reservation instead of abandoning an extra attempt"
                            );
                            if let Some(name) = &reserved {
                                assert_eq!(attempts[0].name(), *name);
                            }
                            if absent == Some(target_index) {
                                restore
                                    .publish_absent(&mut store, &mut live, &session, &cancel)
                                    .unwrap();
                            } else {
                                restore
                                    .replace(&mut store, &mut live, &session, &cancel)
                                    .unwrap();
                            }
                            store.finish_selected(&mut live, restore, &cancel).unwrap();
                            assert_eq!(
                                std::fs::read(paths[target_index].join("file")).unwrap(),
                                values[usize::from(selected_side != ModelSide::Source)]
                            );
                        }
                    }
                    assert_eq!(
                        std::fs::read(&chosen).unwrap(),
                        values[usize::from(selected_side != ModelSide::Source)]
                    );
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    assert!(!store.conflict_record(conflict.id).unwrap().resolved);
                    store.checkpoint().unwrap();
                    drop(store);
                    let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    assert_eq!(
                        std::fs::read(&chosen).unwrap(),
                        values[usize::from(selected_side != ModelSide::Source)]
                    );
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    continue;
                }
                let mut restore = NativeEpoch::selected_restore(
                    &mut store,
                    &live,
                    &endpoints[0],
                    &endpoints[1],
                    &cancel,
                )
                .unwrap();
                if (17..=25).contains(&restore_restart) || restore_restart >= 45 {
                    let mut collision = restore.into_collision(&store, &live).unwrap();
                    store
                        .record_selected_collision(&mut live, &mut collision, &cancel)
                        .unwrap();
                    if restore_restart >= 45 {
                        let id = live.id();
                        if restore_restart == 46 {
                            crate::connection::store::after_native_bytes_for_test(|| {
                                panic!("selected captured bytes before stopped binding")
                            });
                            assert!(
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store
                                    .preserve_selected_collision(
                                        &mut live,
                                        &mut collision,
                                        &cancel
                                    )))
                                .is_err()
                            );
                        }
                        drop(collision);
                        store.checkpoint().unwrap();
                        drop(store);
                        drop(session);
                        let mut store =
                            ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        let current = store.load_connection(record.id).unwrap();
                        let fence = store
                            .request_disconnect(record.id, current.revision)
                            .unwrap();
                        let current = store.load_connection(record.id).unwrap();
                        let mut context =
                            store.open_recovery_context(id, current.revision).unwrap();
                        store
                            .reserve_recovery_reads(
                                &mut context,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        assert!(
                            NativeEpoch::recover_selected_collision(
                                &mut store,
                                &context,
                                &endpoints[0],
                                &endpoints[1],
                                &cancel
                            )
                            .is_err()
                        );
                        let private = tree.path().join("store/recovery").join("5d".repeat(16));
                        let chosen = private.join(format!(
                            "{}-{}",
                            selected.id.operation.serial, selected.id.slot
                        ));
                        let retained = tree.path().join("captured-original");
                        let changed = match restore_restart {
                            48 => Some(chosen.clone()),
                            49 => Some(paths[target_index].join("file")),
                            _ => None,
                        };
                        let changed_bytes = changed.as_ref().map(|path| {
                            let bytes = std::fs::read(path).unwrap();
                            std::fs::rename(path, &retained).unwrap();
                            std::fs::write(path, &bytes).unwrap();
                            bytes
                        });
                        if restore_restart == 50 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        let proof = NativeEpoch::inspect_selected_collision_drain(
                            &mut store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        );
                        if restore_restart >= 48 {
                            assert!(proof.is_err());
                            if let Some(bytes) = changed_bytes {
                                assert_eq!(std::fs::read(&retained).unwrap(), bytes);
                                assert_eq!(std::fs::read(changed.unwrap()).unwrap(), bytes);
                            }
                        } else {
                            let mut proof = proof.unwrap();
                            assert!(
                                store
                                    .drain_native_capture(&mut context, &mut proof.capture, &cancel)
                                    .is_err()
                            );
                            if restore_restart == 47 {
                                crate::connection::store::after_capture_chunk_for_test(|| {
                                    panic!("selected stopped copy before acknowledgement")
                                });
                                assert!(
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        store.drain_selected_capture(
                                            &mut context,
                                            &mut proof,
                                            &cancel,
                                        )
                                    }))
                                    .is_err()
                                );
                                assert_eq!(
                                    std::fs::read(private.join(format!("{}-0", id.serial)))
                                        .unwrap(),
                                    values[0]
                                );
                            }
                            store
                                .drain_selected_capture(&mut context, &mut proof, &cancel)
                                .unwrap();
                            let before = store.load_connection(record.id).unwrap();
                            let slots = store.recovery_page(record.id, None, 16).unwrap().entries;
                            store
                                .drain_selected_capture(&mut context, &mut proof, &cancel)
                                .unwrap();
                            assert_eq!(store.load_connection(record.id).unwrap(), before);
                            assert_eq!(
                                store.recovery_page(record.id, None, 16).unwrap().entries,
                                slots
                            );
                            for entry in slots.iter().filter(|entry| {
                                entry.id.operation == id
                                    && entry.state
                                        == crate::connection::store::RecoveryState::BytesVerified
                            }) {
                                assert_eq!(
                                    std::fs::read(
                                        private.join(format!("{}-{}", id.serial, entry.id.slot))
                                    )
                                    .unwrap(),
                                    values[usize::from(entry.original_side != ModelSide::Source)]
                                );
                            }
                            if restore_restart == 47 {
                                let entry = slots.iter().find(|entry| entry.id.operation == id && entry.state == crate::connection::store::RecoveryState::RetainedIncomplete).unwrap();
                                assert_eq!(
                                    std::fs::read(
                                        private.join(format!("{}-{}", id.serial, entry.id.slot))
                                    )
                                    .unwrap(),
                                    values[0]
                                );
                            }
                        }
                        assert!(store.finish_disconnect(&fence).is_err());
                        assert!(store.selected_collision_accounting(id).unwrap().is_none());
                        for (index, path) in paths.iter().enumerate() {
                            assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                        }
                        assert_eq!(
                            std::fs::read(&chosen).unwrap(),
                            values[usize::from(selected_side != ModelSide::Source)]
                        );
                        assert_eq!(
                            store
                                .load_baseline(record.id)
                                .unwrap()
                                .entries()
                                .unwrap()
                                .get(&RelativePath::unix(b"file").unwrap()),
                            Some(&baseline_value)
                        );
                        assert!(!store.conflict_record(conflict.id).unwrap().resolved);
                        store.checkpoint().unwrap();
                        drop(store);
                        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        assert_eq!(
                            store.load_connection(record.id).unwrap().lifecycle,
                            crate::connection::store::Lifecycle::Disconnecting
                        );
                        assert!(
                            store
                                .pending_page(record.id, None, 16)
                                .unwrap()
                                .entries
                                .iter()
                                .any(|entry| entry.id == id)
                        );
                        assert_eq!(
                            std::fs::read(&chosen).unwrap(),
                            values[usize::from(selected_side != ModelSide::Source)]
                        );
                        continue;
                    }
                    if restore_restart == 19 {
                        store
                            .preserve_selected_collision(&mut live, &mut collision, &cancel)
                            .unwrap();
                    }
                    if restore_restart == 20 {
                        crate::connection::store::after_native_bytes_for_test(|| {
                            panic!("selected collision Bytes before Bound")
                        });
                        assert!(
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store
                                .preserve_selected_collision(&mut live, &mut collision, &cancel)))
                            .is_err()
                        );
                    }
                    let mut changed_collision = None;
                    if (21..=23).contains(&restore_restart) {
                        let changed = if restore_restart == 21 {
                            tree.path()
                                .join("store/recovery")
                                .join("5d".repeat(16))
                                .join(format!(
                                    "{}-{}",
                                    selected.id.operation.serial, selected.id.slot
                                ))
                        } else {
                            paths[if restore_restart == 22 {
                                target_index
                            } else {
                                1 - target_index
                            }]
                            .join("file")
                        };
                        let bytes = std::fs::read(&changed).unwrap();
                        let retained = tree.path().join("retained-collision-generation");
                        std::fs::rename(&changed, &retained).unwrap();
                        std::fs::write(&changed, &bytes).unwrap();
                        changed_collision = Some((changed, retained, bytes));
                    }
                    if restore_restart >= 18 {
                        let id = live.id();
                        drop(collision);
                        drop(store);
                        store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        if restore_restart == 24 {
                            let current = store.load_connection(record.id).unwrap();
                            store
                                .request_disconnect(record.id, current.revision)
                                .unwrap();
                        }
                        let current = store.load_connection(record.id).unwrap();
                        let mut context =
                            store.open_recovery_context(id, current.revision).unwrap();
                        store
                            .reserve_recovery_reads(
                                &mut context,
                                32 * 1024 * 1024,
                                Instant::now() + std::time::Duration::from_secs(30),
                                &cancel,
                            )
                            .unwrap();
                        if restore_restart == 25 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        if restore_restart >= 21 {
                            let before = store.load_connection(record.id).unwrap();
                            let entries = store.recovery_page(record.id, None, 16).unwrap().entries;
                            assert!(
                                NativeEpoch::recover_selected_collision(
                                    &mut store,
                                    &context,
                                    &endpoints[0],
                                    &endpoints[1],
                                    &cancel
                                )
                                .is_err()
                            );
                            assert_eq!(store.load_connection(record.id).unwrap(), before);
                            assert_eq!(
                                store.recovery_page(record.id, None, 16).unwrap().entries,
                                entries
                            );
                            for (index, path) in paths.iter().enumerate() {
                                assert_eq!(
                                    std::fs::read(path.join("file")).unwrap(),
                                    values[index]
                                );
                                assert_eq!(std::fs::read_dir(path).unwrap().count(), 1);
                            }
                            for entry in store.recovery_page(record.id, None, 16).unwrap().entries {
                                let bytes = std::fs::read(
                                    tree.path()
                                        .join("store/recovery")
                                        .join("5d".repeat(16))
                                        .join(format!(
                                            "{}-{}",
                                            entry.id.operation.serial, entry.id.slot
                                        )),
                                )
                                .unwrap();
                                assert_eq!(
                                    bytes,
                                    values[usize::from(entry.original_side != ModelSide::Source)]
                                );
                            }
                            if let Some((changed, retained, bytes)) = changed_collision {
                                assert_eq!(std::fs::read(changed).unwrap(), bytes);
                                assert_eq!(std::fs::read(retained).unwrap(), bytes);
                            }
                            assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                            assert_eq!(
                                store
                                    .load_baseline(record.id)
                                    .unwrap()
                                    .entries()
                                    .unwrap()
                                    .get(&RelativePath::unix(b"file").unwrap()),
                                Some(&baseline_value)
                            );
                            continue;
                        }
                        collision = NativeEpoch::recover_selected_collision(
                            &mut store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel,
                        )
                        .unwrap();
                        let session = store.transaction_session(record.id).unwrap();
                        live = store
                            .resume_selected_collision(&context, &mut collision, &session, &cancel)
                            .unwrap();
                    }
                    store
                        .preserve_selected_collision(&mut live, &mut collision, &cancel)
                        .unwrap();
                    let before_repeat = live.revision();
                    let entries = store.recovery_page(record.id, None, 16).unwrap().entries;
                    store
                        .preserve_selected_collision(&mut live, &mut collision, &cancel)
                        .unwrap();
                    assert_eq!(live.revision(), before_repeat);
                    assert_eq!(
                        store.recovery_page(record.id, None, 16).unwrap().entries,
                        entries
                    );
                    let accounted = store
                        .complete_selected_collision(&mut live, &mut collision, &cancel)
                        .unwrap()
                        .value()
                        .clone();
                    assert!(accounted.preserved.contains(&selected.id));
                    assert_eq!(accounted.preserved.len(), 3);
                    assert!(!accounted.resolved);
                    let before_repeat = store.load_connection(record.id).unwrap();
                    let entries = store.recovery_page(record.id, None, 16).unwrap().entries;
                    for _ in 0..2 {
                        assert_eq!(
                            store.selected_collision_accounting(live.id()).unwrap(),
                            Some(accounted.clone())
                        );
                    }
                    assert_eq!(store.load_connection(record.id).unwrap(), before_repeat);
                    assert_eq!(
                        store.recovery_page(record.id, None, 16).unwrap().entries,
                        entries
                    );
                    assert_eq!(
                        store.conflict_record(conflict.id).unwrap(),
                        conflict,
                        "collision does not resolve the earlier bound conflict"
                    );
                    for (index, path) in paths.iter().enumerate() {
                        assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                        assert_eq!(
                            std::fs::read_dir(path).unwrap().count(),
                            1,
                            "default occupied restore makes no namespace or endpoint mutation"
                        );
                    }
                    for entry in store.recovery_page(record.id, None, 16).unwrap().entries {
                        let bytes = std::fs::read(
                            tree.path()
                                .join("store/recovery")
                                .join("5d".repeat(16))
                                .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot)),
                        )
                        .unwrap();
                        assert_eq!(
                            bytes,
                            values[usize::from(entry.original_side != ModelSide::Source)]
                        );
                    }
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    store.checkpoint().unwrap();
                    drop(collision);
                    drop(store);
                    let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    assert_eq!(store.conflict_record(accounted.id).unwrap(), accounted);
                    assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                    continue;
                }
                restore
                    .prepare(&mut store, &mut live, &session, &cancel)
                    .unwrap();
                let staged_path = paths[target_index]
                    .join(restore.namespace_name().unwrap())
                    .join("stage");
                assert_eq!(
                    std::fs::read(&staged_path).unwrap(),
                    values[usize::from(selected_side != ModelSide::Source)]
                );
                if restore_restart >= 26 {
                    let namespace = paths[target_index].join(restore.namespace_name().unwrap());
                    if restore_restart == 27 || restore_restart >= 29 {
                        restore
                            .relocate_selected(&mut store, &mut live, &session, &cancel)
                            .unwrap();
                    }
                    let partial = if restore_restart == 29 {
                        let reservation = store
                            .reserve_selected_recovery(&mut live, &mut restore)
                            .unwrap();
                        crate::connection::store::partial_native_recovery_for_test(
                            &mut store,
                            &reservation,
                            b"part",
                        )
                        .unwrap();
                        Some(
                            store
                                .native_recovery_slots(&live)
                                .unwrap()
                                .into_iter()
                                .find(|entry| {
                                    entry.state == crate::connection::store::RecoveryState::Reserved
                                })
                                .unwrap()
                                .id,
                        )
                    } else {
                        None
                    };
                    if restore_restart == 28 {
                        if absent == Some(target_index) {
                            restore
                                .publish_absent(&mut store, &mut live, &session, &cancel)
                                .unwrap();
                        } else {
                            restore
                                .replace(&mut store, &mut live, &session, &cancel)
                                .unwrap();
                        }
                    }
                    let id = live.id();
                    drop(restore);
                    drop(store);
                    store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    let current = store.load_connection(record.id).unwrap();
                    let fence = store
                        .request_disconnect(record.id, current.revision)
                        .unwrap();
                    let current = store.load_connection(record.id).unwrap();
                    let mut context = store.open_recovery_context(id, current.revision).unwrap();
                    store
                        .reserve_recovery_reads(
                            &mut context,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    assert!(
                        NativeEpoch::recover_selected(
                            &mut store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel
                        )
                        .is_err()
                    );
                    let mut boundary = NativeEpoch::inspect_selected_boundary(
                        &mut store,
                        &context,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    )
                    .unwrap();
                    if restore_restart >= 30 {
                        let before = store.load_connection(record.id).unwrap();
                        let entries = store.recovery_page(record.id, None, 16).unwrap().entries;
                        let selected_index = usize::from(selected_side != ModelSide::Source);
                        let changed = match restore_restart {
                            30 => Some(
                                tree.path()
                                    .join("store/recovery")
                                    .join("5d".repeat(16))
                                    .join(format!(
                                        "{}-{}",
                                        selected.id.operation.serial, selected.id.slot
                                    )),
                            ),
                            31 => Some(namespace.join("displaced")),
                            32 => Some(paths[selected_index].join("file")),
                            _ => None,
                        };
                        let changed = changed.map(|changed| {
                            let bytes = std::fs::read(&changed).unwrap();
                            let retained = tree.path().join("retained-selected-drain-generation");
                            std::fs::rename(&changed, &retained).unwrap();
                            std::fs::write(&changed, &bytes).unwrap();
                            (changed, retained, bytes)
                        });
                        if restore_restart == 33 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        if restore_restart == 34 {
                            let remaining = context.reads().credit.lock().unwrap().available;
                            context.reads().charge(remaining).unwrap();
                        }
                        assert!(
                            boundary.drain_reader(&store, &context).is_err(),
                            "stopped selected drain must retain current chosen and native guards, cancellation and paid credit"
                        );
                        if restore_restart == 33 {
                            cancel.store(false, std::sync::atomic::Ordering::Release);
                            assert!(
                                boundary.drain_reader(&store, &context).is_err(),
                                "cancelled selected inspection never reopens"
                            );
                        }
                        assert_eq!(store.load_connection(record.id).unwrap(), before);
                        assert_eq!(
                            store.recovery_page(record.id, None, 16).unwrap().entries,
                            entries
                        );
                        assert_eq!(
                            std::fs::read(paths[selected_index].join("file")).unwrap(),
                            values[selected_index]
                        );
                        assert!(!paths[target_index].join("file").exists());
                        assert_eq!(
                            std::fs::read(namespace.join("stage")).unwrap(),
                            values[selected_index]
                        );
                        assert_eq!(
                            std::fs::read(namespace.join("displaced")).unwrap(),
                            values[target_index]
                        );
                        if let Some((changed, retained, bytes)) = changed {
                            assert_eq!(std::fs::read(changed).unwrap(), bytes);
                            assert_eq!(std::fs::read(retained).unwrap(), bytes);
                        }
                        for entry in entries {
                            assert_eq!(
                                std::fs::read(
                                    tree.path()
                                        .join("store/recovery")
                                        .join("5d".repeat(16))
                                        .join(format!(
                                            "{}-{}",
                                            entry.id.operation.serial, entry.id.slot
                                        ))
                                )
                                .unwrap(),
                                values[usize::from(entry.original_side != ModelSide::Source)]
                            );
                        }
                        assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                        assert_eq!(
                            store
                                .load_baseline(record.id)
                                .unwrap()
                                .entries()
                                .unwrap()
                                .get(&RelativePath::unix(b"file").unwrap()),
                            Some(&baseline_value)
                        );
                        continue;
                    }
                    if restore_restart != 26 && absent != Some(target_index) {
                        let mut reader = boundary.drain_reader(&store, &context).unwrap();
                        assert!(
                            store
                                .drain_native_recovery(&mut context, &mut reader, &cancel)
                                .is_err(),
                            "ordinary drain rejects selected evidence"
                        );
                        let copied = store
                            .drain_selected_recovery(&mut context, &mut reader, &cancel)
                            .unwrap();
                        let recovered = store
                            .recovery_page(record.id, None, 16)
                            .unwrap()
                            .entries
                            .into_iter()
                            .find(|entry| {
                                entry.id.operation == id
                                    && entry.state
                                        == crate::connection::store::RecoveryState::BytesVerified
                            })
                            .unwrap()
                            .id;
                        assert_eq!(
                            std::fs::read(
                                tree.path()
                                    .join("store/recovery")
                                    .join("5d".repeat(16))
                                    .join(format!(
                                        "{}-{}",
                                        recovered.operation.serial, recovered.slot
                                    ))
                            )
                            .unwrap(),
                            values[target_index]
                        );
                        let before_repeat = store.load_connection(record.id).unwrap();
                        let entries = store.recovery_page(record.id, None, 16).unwrap().entries;
                        let repeated = store
                            .drain_selected_recovery(&mut context, &mut reader, &cancel)
                            .unwrap();
                        assert_eq!(store.load_connection(record.id).unwrap(), before_repeat);
                        assert_eq!(
                            store.recovery_page(record.id, None, 16).unwrap().entries,
                            entries
                        );
                        drop(repeated);
                        drop(copied);
                    }
                    store
                        .record_selected_boundary(&context, &mut boundary, &cancel)
                        .unwrap();
                    assert_eq!(
                        store
                            .pending_page(record.id, None, 16)
                            .unwrap()
                            .entries
                            .into_iter()
                            .find(|entry| entry.id == id)
                            .unwrap()
                            .stage,
                        ReceiptStage::AccountedPending
                    );
                    assert!(session.require_idle().is_err());
                    store.finish_disconnect(&fence).unwrap();
                    let selected_index = usize::from(selected_side != ModelSide::Source);
                    assert_eq!(
                        std::fs::read(paths[selected_index].join("file")).unwrap(),
                        values[selected_index]
                    );
                    if matches!(restore_restart, 27 | 29) {
                        assert!(!paths[target_index].join("file").exists());
                    } else if restore_restart == 28 {
                        assert_eq!(
                            std::fs::read(paths[target_index].join("file")).unwrap(),
                            values[selected_index]
                        );
                    } else if absent == Some(target_index) {
                        assert!(!paths[target_index].join("file").exists());
                    } else {
                        assert_eq!(
                            std::fs::read(paths[target_index].join("file")).unwrap(),
                            values[target_index]
                        );
                    }
                    if restore_restart == 28 {
                        assert!(!namespace.join("stage").exists());
                    } else {
                        assert_eq!(
                            std::fs::read(namespace.join("stage")).unwrap(),
                            values[selected_index]
                        );
                    }
                    if restore_restart != 26 && absent != Some(target_index) {
                        assert_eq!(
                            std::fs::read(namespace.join("displaced")).unwrap(),
                            values[target_index]
                        );
                    }
                    for entry in store.recovery_page(record.id, None, 16).unwrap().entries {
                        let bytes = std::fs::read(
                            tree.path()
                                .join("store/recovery")
                                .join("5d".repeat(16))
                                .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot)),
                        )
                        .unwrap();
                        if Some(entry.id) == partial {
                            assert_eq!(bytes, b"part");
                            assert_eq!(
                                entry.state,
                                crate::connection::store::RecoveryState::RetainedIncomplete
                            );
                        } else {
                            assert_eq!(
                                bytes,
                                values[usize::from(entry.original_side != ModelSide::Source)]
                            );
                        }
                    }
                    assert_eq!(store.conflict_record(conflict.id).unwrap(), conflict);
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    store.checkpoint().unwrap();
                    drop(boundary);
                    drop(store);
                    let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    assert_eq!(
                        store.load_connection(record.id).unwrap().lifecycle,
                        crate::connection::store::Lifecycle::Disconnected
                    );
                    assert_eq!(
                        store
                            .pending_page(record.id, None, 16)
                            .unwrap()
                            .entries
                            .into_iter()
                            .find(|entry| entry.id == id)
                            .unwrap()
                            .stage,
                        ReceiptStage::AccountedPending
                    );
                    continue;
                }
                if restore_restart >= 9 {
                    let namespace = paths[target_index].join(restore.namespace_name().unwrap());
                    let selected_path = tree
                        .path()
                        .join("store/recovery")
                        .join("5d".repeat(16))
                        .join(format!(
                            "{}-{}",
                            selected.id.operation.serial, selected.id.slot
                        ));
                    let selected_index = usize::from(selected_side != ModelSide::Source);
                    let mut retained = Vec::new();
                    let mut changed = Vec::new();
                    let substituted = match restore_restart {
                        9 => Some(selected_path.clone()),
                        10 => Some(staged_path.clone()),
                        11 => Some(paths[selected_index].join("file")),
                        12 => Some(paths[target_index].join("file")),
                        13 => Some(namespace.join("intent")),
                        _ => None,
                    };
                    if let Some(path) = substituted {
                        let old = std::fs::read(&path).ok();
                        if let Some(bytes) = &old {
                            let saved = tree.path().join("retained-selected-substitution");
                            std::fs::rename(&path, &saved).unwrap();
                            retained.push((saved, bytes.clone()));
                        }
                        let replacement = if restore_restart <= 11 {
                            old.unwrap()
                        } else {
                            b"later occupant or descriptor".to_vec()
                        };
                        std::fs::write(&path, &replacement).unwrap();
                        changed.push((path, replacement));
                    }
                    if restore_restart == 14 {
                        let saved = tree.path().join("retained-selected-namespace");
                        let intent_bytes = std::fs::read(namespace.join("intent")).unwrap();
                        std::fs::rename(&namespace, &saved).unwrap();
                        std::fs::create_dir(&namespace).unwrap();
                        retained.push((saved.join("intent"), intent_bytes));
                        retained.push((saved.join("stage"), values[selected_index].to_vec()));
                    }
                    let id = live.id();
                    drop(restore);
                    drop(store);
                    store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    if restore_restart == 15 {
                        let current = store.load_connection(record.id).unwrap();
                        store
                            .request_disconnect(record.id, current.revision)
                            .unwrap();
                    }
                    let current = store.load_connection(record.id).unwrap();
                    let mut context = store.open_recovery_context(id, current.revision).unwrap();
                    store
                        .reserve_recovery_reads(
                            &mut context,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    if restore_restart == 16 {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let before = store.load_connection(record.id).unwrap();
                    assert!(
                        NativeEpoch::recover_selected(
                            &mut store,
                            &context,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel
                        )
                        .is_err()
                    );
                    assert_eq!(store.load_connection(record.id).unwrap(), before);
                    for (path, bytes) in retained.iter().chain(changed.iter()) {
                        assert_eq!(&std::fs::read(path).unwrap(), bytes);
                    }
                    if restore_restart != 11 {
                        assert_eq!(
                            std::fs::read(paths[selected_index].join("file")).unwrap(),
                            values[selected_index]
                        );
                    }
                    if restore_restart != 12 {
                        if absent == Some(target_index) {
                            assert!(!paths[target_index].join("file").exists());
                        } else {
                            assert_eq!(
                                std::fs::read(paths[target_index].join("file")).unwrap(),
                                values[target_index]
                            );
                        }
                    }
                    if restore_restart != 9 {
                        assert_eq!(
                            std::fs::read(&selected_path).unwrap(),
                            values[selected_index]
                        );
                    }
                    if restore_restart != 10 && restore_restart != 14 {
                        assert_eq!(std::fs::read(&staged_path).unwrap(), values[selected_index]);
                    }
                    if restore_restart == 14 {
                        assert!(std::fs::read_dir(&namespace).unwrap().next().is_none());
                    }
                    assert_eq!(
                        store
                            .load_baseline(record.id)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .get(&RelativePath::unix(b"file").unwrap()),
                        Some(&baseline_value)
                    );
                    continue;
                }
                let mut partial_selected = None;
                if matches!(restore_restart, 3 | 4 | 7 | 8)
                    || (matches!(restore_restart, 5 | 6) && absent != Some(target_index))
                {
                    restore
                        .relocate_selected(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    if matches!(restore_restart, 4..=6) {
                        restore
                            .preserve_displaced(&mut store, &mut live, &session, &cancel)
                            .unwrap();
                    }
                    if restore_restart == 7 {
                        crate::connection::store::after_native_bytes_for_test(|| {
                            panic!("selected private Bytes before NativeBound acknowledgement")
                        });
                        let interrupted =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                restore.preserve_displaced(&mut store, &mut live, &session, &cancel)
                            }));
                        assert!(interrupted.is_err());
                    }
                    if restore_restart == 8 {
                        let slot = store
                            .reserve_selected_recovery(&mut live, &mut restore)
                            .unwrap();
                        crate::connection::store::partial_native_recovery_for_test(
                            &mut store, &slot, b"partial",
                        )
                        .unwrap();
                        partial_selected = Some(
                            store
                                .native_recovery_slots(&live)
                                .unwrap()
                                .into_iter()
                                .find(|entry| {
                                    entry.state == crate::connection::store::RecoveryState::Reserved
                                })
                                .unwrap()
                                .id,
                        );
                    }
                }
                if matches!(restore_restart, 1 | 3 | 4 | 7 | 8) {
                    let id = live.id();
                    drop(restore);
                    drop(store);
                    store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    let current = store.load_connection(record.id).unwrap();
                    let mut context = store.open_recovery_context(id, current.revision).unwrap();
                    store
                        .reserve_recovery_reads(
                            &mut context,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    let session = store.transaction_session(record.id).unwrap();
                    restore = NativeEpoch::recover_selected(
                        &mut store,
                        &context,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    )
                    .unwrap();
                    live = store
                        .resume_native_selected(&context, &mut restore, &session, &cancel)
                        .unwrap();
                    assert_eq!(
                        std::fs::read(&staged_path).unwrap(),
                        values[usize::from(selected_side != ModelSide::Source)]
                    );
                    if restore_restart >= 3 {
                        restore
                            .complete_recovered(&mut store, &mut live, &session, &cancel)
                            .unwrap();
                    }
                }
                let session = store.transaction_session(record.id).unwrap();
                if restore_restart == 5 {
                    restore
                        .publish_selected_bytes(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                }
                if restore_restart == 6 {
                    BEFORE_PUBLICATION_REREAD.with(|hook| {
                        *hook.borrow_mut() = Some(Box::new(|| {
                            panic!("selected rename before published reread/outcome")
                        }));
                    });
                    let interrupted =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            restore.publish_selected_bytes(&mut store, &mut live, &session, &cancel)
                        }));
                    assert!(interrupted.is_err());
                    assert!(
                        !paths[target_index]
                            .join(restore.namespace_name().unwrap())
                            .join("outcome")
                            .exists()
                    );
                }
                let selected_bytes = values[usize::from(selected_side != ModelSide::Source)];
                if restore_restart != 6 && !restore.published && absent == Some(target_index) {
                    restore
                        .publish_absent(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    assert_eq!(
                        std::fs::read(paths[target_index].join("file")).unwrap(),
                        selected_bytes
                    );
                    assert_eq!(
                        store
                            .pending_page(record.id, None, 8)
                            .unwrap()
                            .entries
                            .into_iter()
                            .find(|receipt| receipt.id == live.id())
                            .unwrap()
                            .stage,
                        ReceiptStage::Verified
                    );
                } else if restore_restart != 6 && !restore.published {
                    restore
                        .replace(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    assert_eq!(
                        std::fs::read(paths[target_index].join("file")).unwrap(),
                        selected_bytes
                    );
                    let versions = store.recovery_page(record.id, None, 8).unwrap().entries;
                    let displaced = versions
                        .iter()
                        .find(|entry| entry.id.operation == live.id())
                        .unwrap();
                    assert_eq!(
                        std::fs::read(
                            tree.path()
                                .join("store/recovery")
                                .join("5d".repeat(16))
                                .join(format!(
                                    "{}-{}",
                                    displaced.id.operation.serial, displaced.id.slot
                                ))
                        )
                        .unwrap(),
                        values[target_index]
                    );
                }
                if let Some(partial) = partial_selected {
                    let private = tree.path().join("store/recovery").join("5d".repeat(16));
                    assert_eq!(
                        std::fs::read(
                            private.join(format!("{}-{}", partial.operation.serial, partial.slot))
                        )
                        .unwrap(),
                        b"partial"
                    );
                    let slots = store.native_recovery_slots(&live).unwrap();
                    assert_eq!(
                        slots
                            .iter()
                            .find(|entry| entry.id == partial)
                            .unwrap()
                            .state,
                        crate::connection::store::RecoveryState::RetainedIncomplete
                    );
                    let complete = slots
                        .iter()
                        .find(|entry| {
                            entry.id != partial
                                && entry.state
                                    == crate::connection::store::RecoveryState::BytesVerified
                        })
                        .unwrap();
                    assert_eq!(
                        std::fs::read(private.join(format!(
                            "{}-{}",
                            complete.id.operation.serial, complete.id.slot
                        )))
                        .unwrap(),
                        values[target_index]
                    );
                }
                for (index, path) in paths.iter().enumerate() {
                    if index == target_index {
                        assert_eq!(std::fs::read(path.join("file")).unwrap(), selected_bytes);
                    } else {
                        assert_eq!(std::fs::read(path.join("file")).unwrap(), values[index]);
                    }
                }
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&RelativePath::unix(b"file").unwrap()),
                    Some(&baseline_value)
                );
                if matches!(restore_restart, 2 | 5 | 6) {
                    let id = live.id();
                    drop(restore);
                    drop(store);
                    store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                    let current = store.load_connection(record.id).unwrap();
                    let mut context = store.open_recovery_context(id, current.revision).unwrap();
                    store
                        .reserve_recovery_reads(
                            &mut context,
                            32 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel,
                        )
                        .unwrap();
                    let session = store.transaction_session(record.id).unwrap();
                    restore = NativeEpoch::recover_selected(
                        &mut store,
                        &context,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel,
                    )
                    .unwrap();
                    live = store
                        .resume_native_selected(&context, &mut restore, &session, &cancel)
                        .unwrap();
                    restore
                        .complete_recovered(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    let accounted = store.load_connection(record.id).unwrap();
                    restore
                        .complete_recovered(&mut store, &mut live, &session, &cancel)
                        .unwrap();
                    assert_eq!(
                        store.load_connection(record.id).unwrap(),
                        accounted,
                        "exact repeated reconstruction appends no event or slot"
                    );
                    assert_eq!(
                        std::fs::read(paths[target_index].join("file")).unwrap(),
                        selected_bytes
                    );
                }
                let applied = store.finish_selected(&mut live, restore, &cancel).unwrap();
                assert_eq!(applied.value().stage, ReceiptStage::SelectedApplied);
                assert!(
                    !store.conflict_record(conflict.id).unwrap().resolved,
                    "target-only application cannot resolve the bound conflict"
                );
                assert_eq!(
                    store.load_connection(record.id).unwrap().revision,
                    applied.value().revision
                );
                assert!(
                    store.native_namespace_records(&live).is_err(),
                    "completed selected live token cannot admit another native step"
                );
                store.checkpoint().unwrap();
                drop(store);
                let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                assert_eq!(
                    store
                        .pending_page(record.id, None, 8)
                        .unwrap()
                        .entries
                        .into_iter()
                        .find(|receipt| receipt.id == live.id())
                        .unwrap()
                        .stage,
                    ReceiptStage::SelectedApplied
                );
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&RelativePath::unix(b"file").unwrap()),
                    Some(&baseline_value)
                );
                let current = store.load_connection(record.id).unwrap();
                let chosen = EntryState::File {
                    sha256: Sha256::digest(selected_bytes).into(),
                    bytes: selected_bytes.len() as u64,
                    executable: false,
                };
                let equal = OperationIntent {
                    connection: record.id,
                    expected_revision: current.revision,
                    expected_baseline: current.baseline_revision,
                    path: RelativePath::unix(b"file").unwrap(),
                    source_before: Some(chosen.clone()),
                    destination_before: Some(chosen.clone()),
                    decision: Decision::VerifyEqual,
                };
                let session = store.transaction_session(record.id).unwrap();
                let admission =
                    admission_metadata(&store, &equal, &endpoints[0], &endpoints[1], &cancel)
                        .unwrap();
                let mut equal = store.begin_native(equal, &session, admission).unwrap();
                store
                    .reserve_native_work(
                        &mut equal,
                        &session,
                        32 * 1024 * 1024,
                        Instant::now() + std::time::Duration::from_secs(30),
                        &cancel,
                    )
                    .unwrap();
                let epoch =
                    NativeEpoch::begin_connected(&equal, &endpoints[0], &endpoints[1], &cancel)
                        .unwrap();
                let mut evidence = epoch.equal_evidence(&equal).unwrap();
                store
                    .record_native_equal(&mut equal, &mut evidence, &cancel)
                    .unwrap();
                let mut compared = evidence.comparison(&store, &equal, &cancel).unwrap();
                let candidate = store
                    .prepare_baseline(&equal, &mut compared, &cancel)
                    .unwrap();
                if let Some(case) = baseline_case {
                    let before = store.load_connection(record.id).unwrap();
                    let old_conflict = store.conflict_record(conflict.id).unwrap();
                    let attention = store
                        .selected_attention_snapshot_for_test(record.id)
                        .unwrap();
                    assert!(!attention.is_empty());
                    let receipts = store.pending_page(record.id, None, 128).unwrap().entries;
                    let private = store.recovery_page(record.id, None, 128).unwrap().entries;
                    let private_bytes: Vec<_> = private
                        .iter()
                        .map(|entry| {
                            let path = tree
                                .path()
                                .join("store/recovery")
                                .join("5d".repeat(16))
                                .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot));
                            let bytes = std::fs::read(&path).unwrap();
                            (path, bytes)
                        })
                        .collect();
                    let generation = store.baseline_checkpoint_generation_for_test();
                    if case.checkpoint {
                        store.force_baseline_checkpoint_for_test();
                    }
                    let invoked = std::rc::Rc::new(std::cell::Cell::new(false));
                    let called = invoked.clone();
                    let root = paths[1].clone();
                    let retained = tree.path().join("retained-baseline-root");
                    let moved = retained.clone();
                    let bytes = selected_bytes.to_vec();
                    crate::connection::store::before_native_baseline_write_for_test(
                        case.fault == BaselineFault::Receiver,
                        move || {
                            called.set(true);
                            if case.fault == BaselineFault::Root {
                                std::fs::rename(&root, &moved).unwrap();
                                std::fs::create_dir(&root).unwrap();
                                std::fs::write(root.join("file"), bytes).unwrap();
                            }
                        },
                    );
                    let result =
                        store.commit_baseline(&mut equal, candidate, &mut compared, &cancel);
                    assert!(
                        invoked.get(),
                        "fault must occur after actual append preparation"
                    );
                    assert_eq!(
                        store.baseline_checkpoint_generation_for_test() > generation,
                        case.checkpoint
                    );
                    if case.fault != BaselineFault::Healthy {
                        assert!(result.is_err());
                        assert_eq!(store.load_connection(record.id).unwrap(), before);
                        assert_eq!(store.conflict_record(conflict.id).unwrap(), old_conflict);
                        assert_eq!(
                            store
                                .selected_attention_snapshot_for_test(record.id)
                                .unwrap(),
                            attention
                        );
                        assert_eq!(
                            store.pending_page(record.id, None, 128).unwrap().entries,
                            receipts
                        );
                        drop(compared);
                        drop(evidence);
                        store.checkpoint().unwrap();
                        drop(store);
                        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        assert_eq!(store.load_connection(record.id).unwrap(), before);
                        assert_eq!(store.conflict_record(conflict.id).unwrap(), old_conflict);
                        assert_eq!(
                            store
                                .selected_attention_snapshot_for_test(record.id)
                                .unwrap(),
                            attention
                        );
                        assert_eq!(
                            store.pending_page(record.id, None, 128).unwrap().entries,
                            receipts
                        );
                        assert_eq!(
                            store.recovery_page(record.id, None, 128).unwrap().entries,
                            private
                        );
                        assert_eq!(
                            store
                                .load_baseline(record.id)
                                .unwrap()
                                .entries()
                                .unwrap()
                                .get(&RelativePath::unix(b"file").unwrap()),
                            Some(&baseline_value)
                        );
                        for (path, bytes) in private_bytes {
                            assert_eq!(std::fs::read(path).unwrap(), bytes);
                        }
                        for path in &paths {
                            assert_eq!(std::fs::read(path.join("file")).unwrap(), selected_bytes);
                        }
                        if case.fault == BaselineFault::Root {
                            assert_eq!(
                                std::fs::read(retained.join("file")).unwrap(),
                                selected_bytes
                            );
                        }
                        continue;
                    }
                    result.unwrap();
                    assert!(
                        store
                            .selected_attention_snapshot_for_test(record.id)
                            .unwrap()
                            .is_empty()
                    );
                } else {
                    store
                        .commit_baseline(&mut equal, candidate, &mut compared, &cancel)
                        .unwrap();
                }
                assert!(store.conflict_record(conflict.id).unwrap().resolved);
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&RelativePath::unix(b"file").unwrap()),
                    Some(&chosen)
                );
            }
        }
    }
    thread_local! { static FAIL_STAGE_SPACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    pub(super) fn take_stage_space_failure() -> bool {
        FAIL_STAGE_SPACE.with(|flag| flag.replace(false))
    }
    #[test]
    #[ignore = "authorized disposable bounded ENOSPC volume/helper required; UNRUN"]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn native_executor_enospc_retains_proposal_and_pending_names() {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
            transaction::{TransactionOutcome, execute_next_regular, execute_reviewed_initial},
        };
        use std::io::Read;
        for physical in [false, true] {
            let tree = fixture();
            let pressure = physical.then(|| {
                tempfile::tempdir_in(
                    std::env::var_os("QOS_ENOSPC_FIXTURE")
                        .expect("separately authorized disposable filesystem, at most 128 MiB"),
                )
                .unwrap()
            });
            let source_path = tree.path().join("source");
            let target_path = pressure.as_ref().map_or_else(
                || tree.path().join("target"),
                |root| root.path().join("target"),
            );
            std::fs::create_dir(&source_path).unwrap();
            std::fs::create_dir(&target_path).unwrap();
            std::fs::write(source_path.join("file"), b"keep proposal").unwrap();
            let endpoints = [endpoint(&source_path), endpoint(&target_path)];
            let binding = |path: &std::path::Path| {
                let probe = probe_endpoint(path).unwrap();
                EndpointBinding {
                    identity: probe.identity,
                    locator: probe.locator,
                }
            };
            let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            let record = store
                .create_connection(
                    ConnectionId([99; 16]),
                    binding(&source_path),
                    binding(&target_path),
                    ConnectionPolicy {
                        deletion_retention_seconds: 2_592_000,
                    },
                )
                .unwrap()
                .value()
                .clone();
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let session = store.transaction_session(record.id).unwrap();
            let TransactionOutcome::InitialReview(preview) = execute_next_regular(
                &mut store,
                record.id,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel,
            ) else {
                panic!("review");
            };
            let reviewed = preview
                .accept(OperationIntent {
                    connection: record.id,
                    expected_revision: record.revision,
                    expected_baseline: None,
                    path: RelativePath::unix(b"file").unwrap(),
                    source_before: Some(EntryState::File {
                        sha256: Sha256::digest(b"keep proposal").into(),
                        bytes: 13,
                        executable: false,
                    }),
                    destination_before: None,
                    decision: Decision::Propagate {
                        from: ModelSide::Source,
                    },
                })
                .unwrap();
            let filler = if let Some(root) = &pressure {
                assert_ne!(
                    std::fs::metadata(root.path()).unwrap().dev(),
                    std::fs::metadata(tree.path()).unwrap().dev(),
                    "never fill the store/source fixture filesystem"
                );
                let name = CString::new(root.path().as_os_str().as_bytes()).unwrap();
                let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
                // SAFETY: terminated path and writable statvfs output; initialized on success.
                assert_eq!(
                    unsafe { libc::statvfs(name.as_ptr(), status.as_mut_ptr()) },
                    0
                );
                let status = unsafe { status.assume_init() };
                assert!(
                    u128::from(status.f_blocks) * u128::from(status.f_frsize) <= 128 * 1024 * 1024,
                    "only a dedicated small qualification filesystem may be filled"
                );
                let mut file = File::create(root.path().join("filler")).unwrap();
                let mut bytes = vec![0; 64 * 1024];
                File::open("/dev/urandom")
                    .unwrap()
                    .read_exact(&mut bytes)
                    .unwrap();
                let mut total = 0_u64;
                loop {
                    assert!(
                        total <= 128 * 1024 * 1024,
                        "physical ENOSPC must occur within the checked filesystem bound"
                    );
                    match file.write(&bytes) {
                        Ok(0) => panic!("no progress without physical ENOSPC"),
                        Ok(count) => total += count as u64,
                        Err(error) if error.raw_os_error() == Some(libc::ENOSPC) => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => panic!("unexpected pressure error: {error}"),
                    }
                }
                Some(file)
            } else {
                FAIL_STAGE_SPACE.with(|flag| flag.set(true));
                None
            };
            let result = execute_reviewed_initial(
                &mut store,
                reviewed,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel,
            );
            assert!(matches!(
                result,
                TransactionOutcome::RecoveryRequired {
                    operation: Some(_),
                    ..
                }
            ));
            if !physical {
                assert!(
                    !FAIL_STAGE_SPACE.with(std::cell::Cell::get),
                    "the actual stage write boundary must consume the injection"
                );
            }
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            assert!(!target_path.join("file").exists());
            assert_eq!(
                std::fs::read(source_path.join("file")).unwrap(),
                b"keep proposal"
            );
            let pending = store.executor_position(record.id).unwrap().0.unwrap();
            let retained: Vec<_> = std::fs::read_dir(&target_path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            drop(filler);
            if let Some(root) = &pressure {
                std::fs::remove_file(root.path().join("filler")).unwrap();
            }
            store.checkpoint().unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert_eq!(store.executor_position(record.id).unwrap().0, Some(pending));
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            for name in retained {
                assert!(
                    target_path.join(name).exists(),
                    "failure must not purge created namespace clues"
                );
            }
            assert_eq!(
                std::fs::read(source_path.join("file")).unwrap(),
                b"keep proposal"
            );
            assert!(!target_path.join("file").exists());
        }
    }
    // Only the isolated crash test sets these variables. Normal fixtures never
    // pause or manufacture a recovery capability; the parent reopens the real store.
    fn crash_ready(root: &std::path::Path, id: OperationId, reverse: bool, boundary: &str) {
        if std::env::var("QOS_CONNECTED_CRASH_KIND").ok().as_deref() != Some(boundary) {
            return;
        }
        let ready = std::env::var_os("QOS_CONNECTED_CRASH_READY").unwrap();
        let temporary = std::path::PathBuf::from(&ready).with_extension("writing");
        let mut file = File::create(&temporary).unwrap();
        let body = serde_json::json!({ "root": root.as_os_str().as_bytes(), "connection": id.connection.0, "serial": id.serial, "reverse": reverse });
        file.write_all(&serde_json::to_vec(&body).unwrap()).unwrap();
        file.sync_all().unwrap();
        std::fs::rename(temporary, ready).unwrap();
        loop {
            std::thread::park();
        }
    }
    #[test]
    #[ignore = "isolated child kill/reopen on authorized Linux native fixture/helper only; UNRUN, not power-loss qualification"]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn native_executor_process_restart_boundaries() {
        use crate::connection::{
            store::ConnectionStore,
            transaction::{TransactionOutcome, execute_next_regular},
        };
        use std::os::unix::ffi::OsStringExt;
        const ENTRY: &str = "connection::transaction::native::linux::tests::native_executor_process_restart_boundaries";
        if let Ok(case) = std::env::var("QOS_CONNECTED_CRASH_KIND") {
            let (kind, boundary) = case.split_once('-').unwrap();
            let boundary: u8 = boundary.parse().unwrap();
            let reverse = std::env::var("QOS_CONNECTED_CRASH_REVERSE").unwrap() == "1";
            match kind {
                "addition" => reopened_addition_case(
                    reverse,
                    if boundary <= 5 { boundary } else { 0 },
                    RestartDamage::None,
                ),
                "replacement" => reopened_replacement_case(reverse, boundary, false),
                "deletion" => connected_addition_case(reverse, Some(boundary), false),
                _ => panic!("unknown crash fixture"),
            }
            panic!("requested crash boundary was not reached");
        }
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for reverse in [false, true] {
            for case in (0..=8)
                .map(|n| format!("addition-{n}"))
                .chain((0..=10).map(|n| format!("replacement-{n}")))
                .chain((1..=10).map(|n| format!("deletion-{n}")))
            {
                let control = fixture();
                let ready = control.path().join("ready");
                let mut child = Child(
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", ENTRY, "--ignored", "--nocapture"])
                        .env("QOS_CONNECTED_CRASH_KIND", &case)
                        .env("QOS_CONNECTED_CRASH_READY", &ready)
                        .env(
                            "QOS_CONNECTED_CRASH_REVERSE",
                            if reverse { "1" } else { "0" },
                        )
                        .spawn()
                        .unwrap(),
                );
                let deadline = Instant::now() + std::time::Duration::from_secs(60);
                while !ready.exists() {
                    assert!(
                        child.0.try_wait().unwrap().is_none(),
                        "child exited before {case}"
                    );
                    assert!(
                        Instant::now() < deadline,
                        "bounded child setup timed out at {case}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
                child.0.kill().unwrap();
                assert!(!child.0.wait().unwrap().success());
                let root_bytes: Vec<u8> = serde_json::from_value(body["root"].clone()).unwrap();
                let root = std::path::PathBuf::from(std::ffi::OsString::from_vec(root_bytes));
                let id = OperationId {
                    connection: ConnectionId(
                        serde_json::from_value(body["connection"].clone()).unwrap(),
                    ),
                    serial: body["serial"].as_u64().unwrap(),
                };
                let source = endpoint(&root.join("source"));
                let target = endpoint(&root.join("target"));
                let (left, right) = if reverse {
                    (&target, &source)
                } else {
                    (&source, &target)
                };
                let mut store = ConnectionStore::open_at(&root.join("store")).unwrap();
                assert_eq!(
                    store
                        .load_baseline(id.connection)
                        .unwrap()
                        .entries()
                        .is_some(),
                    case == "addition-6" || case.starts_with("deletion")
                );
                let session = store.transaction_session(id.connection).unwrap();
                let cancel = std::sync::atomic::AtomicBool::new(false);
                assert!(
                    matches!(execute_next_regular(&mut store, id.connection, &session, left, right, &cancel),
                    TransactionOutcome::Verified { operation, up_to_date: true } if operation.id == id),
                    "fresh continuation after actual child kill: {case}"
                );
                if case.starts_with("deletion") {
                    assert!(!root.join("source/file").exists());
                    assert!(!root.join("target/file").exists());
                } else {
                    assert_eq!(
                        std::fs::read(root.join("source/file")).unwrap(),
                        b"new bytes"
                    );
                    assert_eq!(
                        std::fs::read(root.join("target/file")).unwrap(),
                        b"new bytes"
                    );
                }
                let versions = store
                    .recovery_page(id.connection, None, 128)
                    .unwrap()
                    .entries;
                if case.starts_with("replacement") || case.starts_with("deletion") {
                    let original: &[u8] = if case.starts_with("deletion") {
                        b"new bytes"
                    } else {
                        b"old bytes"
                    };
                    let prefix = &original[..3];
                    let mut complete = false;
                    for entry in &versions {
                        let file = root
                            .join("store/recovery")
                            .join("34".repeat(16))
                            .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot));
                        let bytes = std::fs::read(file).unwrap();
                        assert!(bytes == prefix || bytes == original);
                        complete |= bytes == original;
                    }
                    assert!(complete, "actual previous target is independently retained");
                } else {
                    assert!(versions.is_empty());
                }
                let baseline = store.load_baseline(id.connection).unwrap();
                store.checkpoint().unwrap();
                drop(store);
                let mut store = ConnectionStore::open_at(&root.join("store")).unwrap();
                let session = store.transaction_session(id.connection).unwrap();
                assert!(matches!(
                    execute_next_regular(&mut store, id.connection, &session, left, right, &cancel),
                    TransactionOutcome::Verified {
                        up_to_date: true,
                        ..
                    }
                ));
                assert_eq!(store.load_baseline(id.connection).unwrap(), baseline);
                assert_eq!(
                    store
                        .recovery_page(id.connection, None, 128)
                        .unwrap()
                        .entries
                        .len(),
                    versions.len()
                );
                // Own only this child-created disposable fixture after all handles close.
                drop(store);
                drop(source);
                drop(target);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem/helper and bounded capacity qualification required; UNRUN"]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn native_executor_namespace_capacity_retains_all_versions() {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, ReceiptStage,
            },
            transaction::{TransactionOutcome, execute_next_regular, execute_reviewed_initial},
        };
        let tree = fixture();
        let paths = [tree.path().join("left"), tree.path().join("right")];
        for path in &paths {
            std::fs::create_dir(path).unwrap();
        }
        let endpoints = [endpoint(&paths[0]), endpoint(&paths[1])];
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([98; 16]),
                binding(&paths[0]),
                binding(&paths[1]),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let session = store.transaction_session(record.id).unwrap();
        for n in 0..128 {
            let bytes = format!("version-{n:03}");
            std::fs::write(paths[0].join("file"), bytes.as_bytes()).unwrap();
            let result = execute_next_regular(
                &mut store,
                record.id,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel,
            );
            let result = if n == 0 {
                let TransactionOutcome::InitialReview(preview) = result else {
                    panic!("initial review");
                };
                let reviewed = preview
                    .accept(OperationIntent {
                        connection: record.id,
                        expected_revision: record.revision,
                        expected_baseline: None,
                        path: RelativePath::unix(b"file").unwrap(),
                        source_before: Some(EntryState::File {
                            sha256: Sha256::digest(bytes.as_bytes()).into(),
                            bytes: bytes.len() as u64,
                            executable: false,
                        }),
                        destination_before: None,
                        decision: Decision::Propagate {
                            from: ModelSide::Source,
                        },
                    })
                    .unwrap();
                execute_reviewed_initial(
                    &mut store,
                    reviewed,
                    &session,
                    &endpoints[0],
                    &endpoints[1],
                    &cancel,
                )
            } else {
                result
            };
            assert!(
                matches!(
                    result,
                    TransactionOutcome::Verified {
                        up_to_date: true,
                        ..
                    }
                ),
                "bounded namespace {n}"
            );
            assert_eq!(
                std::fs::read(paths[1].join("file")).unwrap(),
                bytes.as_bytes()
            );
        }
        let baseline = store.load_baseline(record.id).unwrap();
        std::fs::write(paths[0].join("file"), b"version-128").unwrap();
        let TransactionOutcome::RecoveryRequired {
            operation: Some(id),
            ..
        } = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        )
        else {
            panic!("capacity exhaustion must retain pending instead of deleting old namespaces");
        };
        assert_eq!(
            store.executor_receipt(id).unwrap().stage,
            ReceiptStage::Intent
        );
        assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
        assert_eq!(std::fs::read_dir(&paths[1]).unwrap().count(), 129);
        assert_eq!(
            std::fs::read(paths[0].join("file")).unwrap(),
            b"version-128"
        );
        assert_eq!(
            std::fs::read(paths[1].join("file")).unwrap(),
            b"version-127"
        );
        let entries = store.recovery_page(record.id, None, 128).unwrap().entries;
        assert_eq!(entries.len(), 127);
        let mut actual = std::collections::BTreeSet::new();
        for entry in &entries {
            let file = tree
                .path()
                .join("store/recovery")
                .join("62".repeat(16))
                .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot));
            actual.insert(std::fs::read(file).unwrap());
        }
        assert_eq!(
            actual,
            (0..127)
                .map(|n| format!("version-{n:03}").into_bytes())
                .collect()
        );
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(
            matches!(execute_next_regular(&mut store, record.id, &session, &endpoints[0], &endpoints[1], &cancel),
            TransactionOutcome::RecoveryRequired { operation: Some(operation), .. } if operation == id)
        );
        assert_eq!(store.load_baseline(record.id).unwrap(), baseline);
        assert_eq!(
            store
                .recovery_page(record.id, None, 128)
                .unwrap()
                .entries
                .len(),
            127
        );
        assert_eq!(std::fs::read_dir(&paths[1]).unwrap().count(), 129);
        assert_eq!(
            std::fs::read(paths[0].join("file")).unwrap(),
            b"version-128"
        );
        assert_eq!(
            std::fs::read(paths[1].join("file")).unwrap(),
            b"version-127"
        );
    }
    fn native_count_and_guard_limits_case() {
        use crate::connection::model::Observation;
        let tree = fixture();
        for n in 0..4096 {
            std::fs::write(tree.path().join(format!("entry-{n:04}")), b"v").unwrap();
        }
        let endpoint = endpoint(tree.path());
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let complete = endpoint::observe(&endpoint, &cancel);
        assert!(matches!(complete.observation(), Observation::Complete));
        assert_eq!(complete.entries().len(), 4096);
        std::fs::write(tree.path().join("entry-over-cap"), b"retain excess").unwrap();
        let limited = endpoint::observe(&endpoint, &cancel);
        assert!(!matches!(limited.observation(), Observation::Complete));
        assert_eq!(std::fs::read_dir(tree.path()).unwrap().count(), 4097);
        assert_eq!(
            std::fs::read(tree.path().join("entry-over-cap")).unwrap(),
            b"retain excess"
        );
        assert_eq!(std::fs::read(tree.path().join("entry-0000")).unwrap(), b"v");
        assert_eq!(std::fs::read(tree.path().join("entry-4095")).unwrap(), b"v");
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let mut guards = Vec::new();
        for n in 0..4 {
            let parent = epoch
                .pin_parent(
                    Side::Source,
                    &RelativePath::unix(format!("entry-{n:04}").as_bytes()).unwrap(),
                )
                .unwrap();
            guards.push(epoch.guard_regular(&parent).unwrap());
        }
        for guard in &guards {
            assert_eq!(
                epoch.capture(guard).unwrap().digest,
                <[u8; 32]>::from(Sha256::digest(b"v"))
            );
        }
        let fifth = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"entry-0004").unwrap())
            .unwrap();
        assert!(epoch.guard_regular(&fifth).is_err());
        assert!(epoch.check().is_err());
        assert_eq!(std::fs::read_dir(tree.path()).unwrap().count(), 4097);
        for n in 0..5 {
            assert_eq!(
                std::fs::read(tree.path().join(format!("entry-{n:04}"))).unwrap(),
                b"v"
            );
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn executor_live_writer_case(mapping: bool) {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, ReceiptStage,
            },
            transaction::{TransactionOutcome, execute_next_regular, execute_reviewed_initial},
        };
        let name = "x".repeat(255);
        let tree = fixture();
        let paths = [tree.path().join("left"), tree.path().join("right")];
        for path in &paths {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(paths[0].join(&name), b"guarded").unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths[0].join(&name))
            .unwrap();
        let region = if mapping {
            // SAFETY: seven initialized bytes in a disposable test regular file;
            // retained until the matching munmap below, including after fd close.
            let value = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    7,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    writer.as_raw_fd(),
                    0,
                )
            };
            assert_ne!(value, libc::MAP_FAILED);
            Some(value)
        } else {
            None
        };
        let writer = if mapping {
            drop(writer);
            None
        } else {
            Some(writer)
        };
        let endpoints = [endpoint(&paths[0]), endpoint(&paths[1])];
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([97; 16]),
                binding(&paths[0]),
                binding(&paths[1]),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let session = store.transaction_session(record.id).unwrap();
        let TransactionOutcome::InitialReview(preview) = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        ) else {
            panic!("review");
        };
        let reviewed = preview
            .accept(OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: None,
                path: RelativePath::unix(name.as_bytes()).unwrap(),
                source_before: Some(EntryState::File {
                    sha256: Sha256::digest(b"guarded").into(),
                    bytes: 7,
                    executable: false,
                }),
                destination_before: None,
                decision: Decision::Propagate {
                    from: ModelSide::Source,
                },
            })
            .unwrap();
        let TransactionOutcome::RecoveryRequired {
            operation: Some(id),
            ..
        } = execute_reviewed_initial(
            &mut store,
            reviewed,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        )
        else {
            panic!(
                "a retained writable description or mapping cannot acquire native stable authority"
            );
        };
        assert_eq!(
            store.executor_receipt(id).unwrap().stage,
            ReceiptStage::Intent
        );
        assert!(store.load_baseline(record.id).unwrap().entries().is_none());
        assert!(!paths[1].join(&name).exists());
        assert_eq!(std::fs::read(paths[0].join(&name)).unwrap(), b"guarded");
        if let Some(region) = region {
            // SAFETY: exact live mapping above; no reference remains afterward.
            assert_eq!(unsafe { libc::munmap(region, 7) }, 0);
        }
        drop(writer);
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(
            matches!(execute_next_regular(&mut store, record.id, &session, &endpoints[0], &endpoints[1], &cancel),
            TransactionOutcome::Verified { operation, up_to_date: true } if operation.id == id)
        );
        assert_eq!(std::fs::read(paths[0].join(&name)).unwrap(), b"guarded");
        assert_eq!(std::fs::read(paths[1].join(&name)).unwrap(), b"guarded");
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep exact existing-parent fixture setup, durable boundaries and retained-byte assertions together."
    )]
    fn executor_nested_initial_case(
        reverse: bool,
        equal: bool,
        unrelated: bool,
        damaged_side: Option<usize>,
    ) {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
            transaction::{TransactionOutcome, execute_next_regular, execute_reviewed_initial},
        };
        let tree = fixture();
        let paths = [tree.path().join("left"), tree.path().join("right")];
        for path in &paths {
            std::fs::create_dir_all(path.join("a/b")).unwrap();
        }
        let source_side = usize::from(reverse);
        std::fs::write(paths[source_side].join("a/b/file"), b"nested").unwrap();
        if equal {
            std::fs::write(paths[1 - source_side].join("a/b/file"), b"nested").unwrap();
        }
        if unrelated {
            for (index, root) in paths.iter().enumerate() {
                std::fs::write(root.join("unrelated"), [index as u8]).unwrap();
            }
        }
        let endpoints = [endpoint(&paths[0]), endpoint(&paths[1])];
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([96; 16]),
                binding(&paths[0]),
                binding(&paths[1]),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let session = store.transaction_session(record.id).unwrap();
        let TransactionOutcome::InitialReview(preview) = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        ) else {
            panic!("review");
        };
        let path = RelativePath::unix(b"a/b/file").unwrap();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"nested").into(),
            bytes: 6,
            executable: false,
        });
        let reviewed = preview
            .accept(OperationIntent {
                connection: record.id,
                expected_revision: record.revision,
                expected_baseline: None,
                path: path.clone(),
                source_before: if !reverse || equal {
                    value.clone()
                } else {
                    None
                },
                destination_before: if reverse || equal {
                    value.clone()
                } else {
                    None
                },
                decision: if equal {
                    Decision::VerifyEqual
                } else {
                    Decision::Propagate {
                        from: if reverse {
                            ModelSide::Destination
                        } else {
                            ModelSide::Source
                        },
                    }
                },
            })
            .unwrap();
        if let Some(side) = damaged_side {
            let root = paths[side].clone();
            let invoked = std::rc::Rc::new(std::cell::Cell::new(false));
            let called = invoked.clone();
            BEFORE_PUBLICATION_REREAD.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    called.set(true);
                    std::fs::rename(root.join("a"), root.join("retained-a")).unwrap();
                    std::fs::create_dir_all(root.join("a/b")).unwrap();
                    std::fs::write(root.join("a/b/file"), b"competitor").unwrap();
                }));
            });
            assert!(matches!(
                execute_reviewed_initial(
                    &mut store,
                    reviewed,
                    &session,
                    &endpoints[0],
                    &endpoints[1],
                    &cancel
                ),
                TransactionOutcome::RecoveryRequired {
                    operation: Some(_),
                    ..
                }
            ));
            assert!(
                invoked.get(),
                "actual publication reread boundary must be reached"
            );
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            assert!(store.executor_position(record.id).unwrap().0.is_some());
            assert_eq!(
                std::fs::read(paths[side].join("retained-a/b/file")).unwrap(),
                b"nested"
            );
            assert_eq!(
                std::fs::read(paths[side].join("a/b/file")).unwrap(),
                b"competitor"
            );
            assert_eq!(
                std::fs::read(paths[1 - side].join("a/b/file")).unwrap(),
                b"nested"
            );
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            assert!(store.executor_position(record.id).unwrap().0.is_some());
            assert_eq!(
                std::fs::read(paths[side].join("retained-a/b/file")).unwrap(),
                b"nested"
            );
            return;
        }
        assert!(
            matches!(execute_reviewed_initial(&mut store, reviewed, &session, &endpoints[0], &endpoints[1], &cancel), TransactionOutcome::Verified { up_to_date, .. } if up_to_date != unrelated)
        );
        let baseline = store.load_baseline(record.id).unwrap();
        let rows = baseline.entries().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.get(&RelativePath::unix(b"a").unwrap()),
            Some(&EntryState::Directory)
        );
        assert_eq!(
            rows.get(&RelativePath::unix(b"a/b").unwrap()),
            Some(&EntryState::Directory)
        );
        assert_eq!(rows.get(&path), value.as_ref());
        for endpoint in &paths {
            assert_eq!(std::fs::read(endpoint.join("a/b/file")).unwrap(), b"nested");
        }
        std::fs::write(paths[source_side].join("a/b/second"), b"shared").unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                record.id,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel
            ),
            TransactionOutcome::Verified { .. }
        ));
        assert_eq!(
            store
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            4
        );
        std::fs::write(paths[source_side].join("a/b/file"), b"updated").unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                record.id,
                &session,
                &endpoints[0],
                &endpoints[1],
                &cancel
            ),
            TransactionOutcome::Verified { .. }
        ));
        for root in &paths {
            assert_eq!(std::fs::read(root.join("a/b/file")).unwrap(), b"updated");
            assert_eq!(std::fs::read(root.join("a/b/second")).unwrap(), b"shared");
        }
        if unrelated {
            for (index, root) in paths.iter().enumerate() {
                assert_eq!(
                    std::fs::read(root.join("unrelated")).unwrap(),
                    [index as u8]
                );
            }
        }
        let expected = store.load_baseline(record.id).unwrap();
        assert_eq!(expected.entries().unwrap().len(), 4);
        drop(store);
        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        assert_eq!(
            store.load_baseline(record.id).unwrap().entries(),
            expected.entries()
        );
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn executor_verified_equality_case(stopped: bool, damage: u8) {
        use crate::connection::{
            model::{Decision, EntryState},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, Lifecycle, OperationIntent,
                ReceiptStage,
            },
            transaction::{TransactionOutcome, execute_next_regular},
        };
        let tree = fixture();
        let paths = [tree.path().join("left"), tree.path().join("right")];
        for path in &paths {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("file"), b"equal").unwrap();
        }
        let endpoints = [endpoint(&paths[0]), endpoint(&paths[1])];
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([95; 16]),
                binding(&paths[0]),
                binding(&paths[1]),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"equal").into(),
            bytes: 5,
            executable: false,
        });
        let proposal = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"file").unwrap(),
            source_before: value.clone(),
            destination_before: value.clone(),
            decision: Decision::VerifyEqual,
        };
        let review = initial_preview(&store, record.id, &endpoints[0], &endpoints[1], &cancel)
            .unwrap()
            .accept(proposal.clone())
            .unwrap();
        let metadata =
            admission_metadata(&store, &proposal, &endpoints[0], &endpoints[1], &cancel).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        let mut live = store
            .begin_native_reviewed(proposal, &session, review, metadata)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                EXECUTOR_READ_WINDOW,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let epoch =
            NativeEpoch::begin_connected(&live, &endpoints[0], &endpoints[1], &cancel).unwrap();
        let mut evidence = epoch.equal_evidence(&live).unwrap();
        store
            .record_native_equal(&mut live, &mut evidence, &cancel)
            .unwrap();
        let id = live.id();
        drop(evidence);
        assert_eq!(
            store.executor_receipt(id).unwrap().stage,
            ReceiptStage::Verified
        );
        assert!(store.load_baseline(record.id).unwrap().entries().is_none());
        store.checkpoint().unwrap();
        drop(store);
        drop(session);
        match damage {
            1 => std::fs::write(paths[0].join("file"), b"later").unwrap(),
            2 => {
                std::fs::rename(paths[1].join("file"), tree.path().join("original")).unwrap();
                std::fs::write(paths[1].join("file"), b"equal").unwrap();
            }
            3 => {
                std::fs::rename(&paths[0], tree.path().join("original-parent")).unwrap();
                std::fs::create_dir(&paths[0]).unwrap();
                std::fs::write(paths[0].join("file"), b"equal").unwrap();
            }
            4 => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(paths[0].join("file"), std::fs::Permissions::from_mode(0))
                    .unwrap();
            }
            _ => {}
        }
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        let current = store.load_connection(record.id).unwrap();
        let fence = stopped.then(|| {
            store
                .request_disconnect(record.id, current.revision)
                .unwrap()
        });
        if damage == 5 {
            let current = store.load_connection(record.id).unwrap();
            let mut context = store.open_recovery_context(id, current.revision).unwrap();
            store
                .reserve_recovery_reads(
                    &mut context,
                    EXECUTOR_READ_WINDOW,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let epoch =
                NativeEpoch::begin_recovery(&context, &endpoints[0], &endpoints[1], &cancel)
                    .unwrap();
            let mut evidence = epoch
                .inspect_never_begun(&store, &context, &cancel)
                .unwrap();
            evidence.epoch.leases.revoke();
            assert!(
                evidence.validate(&store, &context).is_err(),
                "receiver revocation after reacquisition cannot preserve live authority"
            );
            assert!(
                store
                    .resume_native_untouched(&context, &mut evidence, &session, &cancel)
                    .is_err()
            );
            assert!(
                store
                    .record_never_begun(&context, &mut evidence, &cancel)
                    .is_err()
            );
            assert_eq!(
                store.executor_receipt(id).unwrap().stage,
                ReceiptStage::Verified
            );
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            for path in &paths {
                assert_eq!(std::fs::read(path.join("file")).unwrap(), b"equal");
            }
            return;
        }
        let result = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        );
        if damage == 4 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                paths[0].join("file"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        if damage != 0 {
            assert!(
                matches!(result, TransactionOutcome::RecoveryRequired { operation: Some(operation), .. } if operation == id)
            );
            assert_eq!(
                store.executor_receipt(id).unwrap().stage,
                ReceiptStage::Verified
            );
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            assert_eq!(store.executor_position(record.id).unwrap().0, Some(id));
            if damage == 2 {
                assert_eq!(
                    std::fs::read(tree.path().join("original")).unwrap(),
                    b"equal"
                );
            }
            if damage == 3 {
                assert_eq!(
                    std::fs::read(tree.path().join("original-parent/file")).unwrap(),
                    b"equal"
                );
            }
        } else if let Some(fence) = fence {
            assert!(
                matches!(result, TransactionOutcome::AccountedPending { operation: Some(ref receipt) } if receipt.id == id)
            );
            assert_eq!(
                store.executor_receipt(id).unwrap().stage,
                ReceiptStage::AccountedPending
            );
            assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            store.finish_disconnect(&fence).unwrap();
            assert_eq!(
                store.load_connection(record.id).unwrap().lifecycle,
                Lifecycle::Disconnected
            );
        } else {
            assert!(
                matches!(result, TransactionOutcome::Verified { operation: ref receipt, up_to_date: true } if receipt.id == id)
            );
            assert_eq!(
                store.executor_receipt(id).unwrap().stage,
                ReceiptStage::BaselineCommitted
            );
            assert_eq!(
                store
                    .load_baseline(record.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&RelativePath::unix(b"file").unwrap()),
                value.as_ref()
            );
        }
        assert_eq!(
            std::fs::read(paths[0].join("file")).unwrap(),
            if damage == 1 { b"later" } else { b"equal" }
        );
        assert_eq!(std::fs::read(paths[1].join("file")).unwrap(), b"equal");
        assert!(
            store
                .recovery_page(record.id, None, 16)
                .unwrap()
                .entries
                .is_empty()
        );
        let expected = store.executor_receipt(id).unwrap();
        store.checkpoint().unwrap();
        drop(store);
        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        assert_eq!(store.executor_receipt(id).unwrap(), expected);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and helper required; execution remains UNRUN"]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn native_executor_requires_review_and_progresses_two_regular_files() {
        native_count_and_guard_limits_case();
        for reverse in [false, true] {
            for equal in [false, true] {
                for unrelated in [false, true] {
                    executor_nested_initial_case(reverse, equal, unrelated, None);
                }
            }
        }
        for reverse in [false, true] {
            for side in 0..2 {
                executor_nested_initial_case(reverse, false, false, Some(side));
            }
        }
        for mapping in [false, true] {
            executor_live_writer_case(mapping);
        }
        for stopped in [false, true] {
            for damage in 0..6 {
                executor_verified_equality_case(stopped, damage);
            }
        }
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
            transaction::{TransactionOutcome, execute_next_regular, execute_reviewed_initial},
        };
        use std::sync::atomic::AtomicBool;
        let tree = fixture();
        let left = tree.path().join("left");
        let right = tree.path().join("right");
        std::fs::create_dir(&left).unwrap();
        std::fs::create_dir(&right).unwrap();
        std::fs::write(left.join("a"), b"alpha").unwrap();
        std::fs::write(left.join("b"), b"beta").unwrap();
        let endpoints = [endpoint(&left), endpoint(&right)];
        let cancel = AtomicBool::new(false);
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([94; 16]),
                binding(&left),
                binding(&right),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap()
            .value()
            .clone();
        let session = store.transaction_session(record.id).unwrap();
        let TransactionOutcome::InitialReview(preview) = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        ) else {
            panic!("no automatic initial acceptance");
        };
        assert_eq!(store.load_connection(record.id).unwrap(), record);
        assert!(
            store
                .pending_page(record.id, None, 16)
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(!right.join("a").exists());
        assert!(!right.join("b").exists());
        let proposed = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"a").unwrap(),
            source_before: Some(EntryState::File {
                sha256: Sha256::digest(b"alpha").into(),
                bytes: 5,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate {
                from: ModelSide::Source,
            },
        };
        let reviewed = preview.accept(proposed).unwrap();
        let first = execute_reviewed_initial(
            &mut store,
            reviewed,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        );
        assert!(matches!(
            first,
            TransactionOutcome::Verified {
                up_to_date: false,
                ..
            }
        ));
        assert_eq!(std::fs::read(right.join("a")).unwrap(), b"alpha");
        assert!(!right.join("b").exists());
        let baseline = store.load_baseline(record.id).unwrap();
        assert_eq!(baseline.entries().unwrap().len(), 1);
        let second = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        );
        assert!(matches!(
            second,
            TransactionOutcome::Verified {
                up_to_date: true,
                ..
            }
        ));
        assert_eq!(std::fs::read(right.join("a")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(right.join("b")).unwrap(), b"beta");
        let final_baseline = store.load_baseline(record.id).unwrap();
        assert_eq!(final_baseline.entries().unwrap().len(), 2);
        assert_eq!(
            final_baseline
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap()),
            baseline
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap())
        );
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                record.id,
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
        for reverse in [false, true] {
            let writer = if reverse { &right } else { &left };
            let bytes: &[u8] = if reverse {
                b"right replacement"
            } else {
                b"left replacement"
            };
            std::fs::write(writer.join("a"), bytes).unwrap();
            assert!(matches!(
                execute_next_regular(
                    &mut store,
                    record.id,
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
            for path in [&left, &right] {
                assert_eq!(std::fs::read(path.join("a")).unwrap(), bytes);
            }
            std::fs::remove_file(writer.join("a")).unwrap();
            assert!(matches!(
                execute_next_regular(
                    &mut store,
                    record.id,
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
            assert!(!left.join("a").exists());
            assert!(!right.join("a").exists());
            assert!(
                !store
                    .load_baseline(record.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .contains_key(&RelativePath::unix(b"a").unwrap())
            );
            std::fs::write(writer.join("a"), b"again").unwrap();
            assert!(matches!(
                execute_next_regular(
                    &mut store,
                    record.id,
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
            for path in [&left, &right] {
                assert_eq!(std::fs::read(path.join("a")).unwrap(), b"again");
            }
        }
        let before_conflict = store.load_baseline(record.id).unwrap();
        std::fs::write(left.join("a"), b"left choice").unwrap();
        std::fs::write(right.join("a"), b"right choice").unwrap();
        std::fs::write(left.join("b"), b"independent").unwrap();
        let TransactionOutcome::Conflict(conflict) = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        ) else {
            panic!("actual divergent versions must be captured");
        };
        assert!(!conflict.resolved);
        assert_eq!(store.load_baseline(record.id).unwrap(), before_conflict);
        assert_eq!(std::fs::read(left.join("a")).unwrap(), b"left choice");
        assert_eq!(std::fs::read(right.join("a")).unwrap(), b"right choice");
        let TransactionOutcome::Verified {
            operation: independent,
            up_to_date: false,
        } = execute_next_regular(
            &mut store,
            record.id,
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        )
        else {
            panic!("independent file completes while conflict attention blocks Up to date");
        };
        assert_eq!(independent.intent.path, RelativePath::unix(b"b").unwrap());
        assert_eq!(std::fs::read(right.join("b")).unwrap(), b"independent");
        assert_eq!(
            store
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap()),
            before_conflict
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap())
        );
        let chosen = store
            .recovery_page(record.id, None, 128)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| {
                entry.id.operation
                    == OperationId {
                        connection: conflict.id.connection,
                        serial: conflict.id.serial,
                    }
                    && entry.original_side == ModelSide::Source
            })
            .unwrap();
        let current = store.load_connection(record.id).unwrap();
        let mut context = store
            .open_recovery_context(independent.id, current.revision)
            .unwrap();
        store
            .reserve_recovery_reads(
                &mut context,
                EXECUTOR_READ_WINDOW,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let mut epoch =
            NativeEpoch::begin_observation(&context, &endpoints[0], &endpoints[1], &cancel)
                .unwrap();
        let (source, destination) = epoch.observe_pair(&store, &context, &cancel).unwrap();
        drop(epoch);
        let preview = store
            .restore_preview(
                chosen.id,
                ModelSide::Destination,
                Some((conflict.id, conflict.revision)),
                source,
                destination,
            )
            .unwrap();
        let selected = resolve_regular(
            &mut store,
            preview.accept_replacement(),
            &session,
            &endpoints[0],
            &endpoints[1],
            &cancel,
        );
        assert!(matches!(
            selected,
            TransactionOutcome::Verified {
                up_to_date: false,
                ..
            }
        ));
        assert_eq!(std::fs::read(right.join("a")).unwrap(), b"left choice");
        assert!(!store.conflict_record(conflict.id).unwrap().resolved);
        assert_eq!(
            store
                .load_baseline(record.id)
                .unwrap()
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap()),
            before_conflict
                .entries()
                .unwrap()
                .get(&RelativePath::unix(b"a").unwrap())
        );
        assert!(matches!(
            execute_next_regular(
                &mut store,
                record.id,
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
        assert!(store.conflict_record(conflict.id).unwrap().resolved);
        let entries = store.recovery_page(record.id, None, 128).unwrap().entries;
        for entry in entries.iter().filter(|entry| {
            entry.id.operation
                == OperationId {
                    connection: conflict.id.connection,
                    serial: conflict.id.serial,
                }
        }) {
            let bytes = std::fs::read(
                tree.path()
                    .join("store/recovery")
                    .join("5e".repeat(16))
                    .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot)),
            )
            .unwrap();
            assert_eq!(
                bytes,
                if entry.original_side == ModelSide::Source {
                    b"left choice".as_slice()
                } else {
                    b"right choice".as_slice()
                }
            );
        }
        store.checkpoint().unwrap();
        drop(store);
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(matches!(
            execute_next_regular(
                &mut store,
                record.id,
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
    }
    // This body uses real Linux endpoints, leases, relocation and private store
    // rereads. Compile/link only until native execution is explicitly authorized.
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_preservation_binds_actual_displacement_and_retains_all_bytes() {
        connected_preservation_case(PreservationCase::Success);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_late_disconnect_retains_durable_bytes_without_bound_acknowledgement() {
        connected_preservation_case(PreservationCase::DisconnectAfterBytes);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_source_loss_stops_before_displacement() {
        connected_preservation_case(PreservationCase::MissingSource);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_final_ownership_race_is_incomplete_and_retains_bytes() {
        connected_preservation_case(PreservationCase::OwnershipRace);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_replacement_publishes_and_freshly_verifies_with_retained_original() {
        connected_preservation_case(PreservationCase::Publish);
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_additions_both_directions_publish_without_dummy_recovery() {
        for reverse in [false, true] {
            connected_addition_case(reverse, None, false);
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_connected_deletions_both_directions_commit_empty_baseline_with_recovery() {
        for reverse in [false, true] {
            connected_addition_case(reverse, Some(0), false);
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_reopened_addition_reconstructs_staged_and_published_without_receipt_authority() {
        for reverse in [false, true] {
            for published in 0..=5 {
                reopened_addition_case(reverse, published, RestartDamage::None);
            }
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and helper required; current execution UNRUN"]
    fn native_reopened_addition_accounts_staged_and_published_boundaries_after_disconnect() {
        for reverse in [false, true] {
            for published in 0..=5 {
                reopened_addition_case(reverse, published, RestartDamage::Disconnect);
            }
        }
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RestartDamage {
        None,
        Source,
        Competitor,
        SameBytesOtherObject,
        UnknownChild,
        Descriptor,
        DescriptorOversize,
        SameBytesDescriptor,
        SameBytesNamespace,
        Stopped,
        Disconnect,
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_reopened_addition_refuses_changed_objects_clues_children_and_stop_latch() {
        for published in [0, 1, 2] {
            for damage in [
                RestartDamage::Source,
                RestartDamage::Competitor,
                RestartDamage::SameBytesOtherObject,
                RestartDamage::UnknownChild,
                RestartDamage::Descriptor,
                RestartDamage::DescriptorOversize,
                RestartDamage::SameBytesNamespace,
                RestartDamage::Stopped,
            ] {
                reopened_addition_case(false, published, damage);
            }
        }
    }
    #[test]
    #[ignore = "HOLD: authorized native filesystem/helper; fresh descriptor observations UNRUN"]
    fn native_fresh_restart_accepts_same_canonical_descriptor_bytes_but_not_payload_or_namespace_substitution()
     {
        for reverse in [false, true] {
            for published in [0, 1, 2] {
                reopened_addition_case(reverse, published, RestartDamage::SameBytesDescriptor);
                reopened_addition_case(reverse, published, RestartDamage::SameBytesNamespace);
                reopened_addition_case(reverse, published, RestartDamage::SameBytesOtherObject);
            }
        }
    }
    fn reopened_addition_case(reverse: bool, published: u8, damage: RestartDamage) {
        reopened_addition_at_path(reverse, published, damage, false);
    }
    #[test]
    #[ignore = "authorized native filesystem and helper required; nested restart bodies UNRUN"]
    fn native_initial_nested_addition_reopens_durable_boundaries() {
        for reverse in [false, true] {
            for published in 0..=5 {
                for damage in [
                    RestartDamage::None,
                    RestartDamage::Disconnect,
                    RestartDamage::Stopped,
                ] {
                    reopened_addition_at_path(reverse, published, damage, true);
                }
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Retain actual original/reopened native owners through all regular publication boundaries."
    )]
    fn reopened_addition_at_path(
        reverse: bool,
        published: u8,
        damage: RestartDamage,
        nested: bool,
    ) {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, ReceiptStage,
            },
        };
        use std::sync::atomic::AtomicBool;
        let leaf = if nested { "a/b/file" } else { "file" };
        let tree = fixture();
        let source_path = tree.path().join("source");
        let target_path = tree.path().join("target");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        if nested {
            for root in [&source_path, &target_path] {
                std::fs::create_dir_all(root.join("a/b")).unwrap();
            }
        }
        std::fs::write(source_path.join(leaf), b"new bytes").unwrap();
        let source_endpoint = endpoint(&source_path);
        let target_endpoint = endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let (left, right) = if reverse {
            (binding(&target_path), binding(&source_path))
        } else {
            (binding(&source_path), binding(&target_path))
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([52; 16]),
                left,
                right,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let session = store.transaction_session(record.id).unwrap();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"new bytes").into(),
            bytes: 9,
            executable: false,
        });
        let (source_before, destination_before, from, to) = if reverse {
            (None, value, ModelSide::Destination, ModelSide::Source)
        } else {
            (value, None, ModelSide::Source, ModelSide::Destination)
        };
        let operation = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(leaf.as_bytes()).unwrap(),
            source_before,
            destination_before,
            decision: Decision::Propagate { from },
        };
        let cancel = AtomicBool::new(false);
        let (left, right) = if reverse {
            (&target_endpoint, &source_endpoint)
        } else {
            (&source_endpoint, &target_endpoint)
        };
        let reviewed = initial_preview(&store, record.id, left, right, &cancel)
            .unwrap()
            .accept(operation.clone())
            .unwrap();
        let admission = admission_metadata(&store, &operation, left, right, &cancel).unwrap();
        let mut live = store
            .begin_native_reviewed(operation, &session, reviewed, admission)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                if nested {
                    EXECUTOR_READ_WINDOW
                } else {
                    16 * 1024 * 1024
                },
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        crash_ready(tree.path(), live.id(), reverse, "addition-7");
        let reservation = store.reserve_native_namespace(&mut live, to).unwrap();
        crash_ready(tree.path(), live.id(), reverse, "addition-8");
        let private_name = reservation.name();
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        let source = epoch.pin_parent(Side::Source, &live.intent().path).unwrap();
        let target = epoch.pin_parent(Side::Target, &live.intent().path).unwrap();
        let source_guard = epoch.guard_regular(&source).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .unwrap();
        let private = epoch
            .create_registered_namespace(
                &live,
                &reservation,
                &source,
                &target,
                &source_guard,
                None,
                step,
            )
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Stage, &cancel)
            .unwrap();
        let staged = epoch
            .stage_connected(&live, &source_guard, &private, step)
            .unwrap();
        let prepared = epoch.prepared(&private, Some(&staged)).unwrap();
        store
            .prepare_native(&mut live, reservation, prepared)
            .unwrap();
        let id = live.id();
        let original_nonce = epoch.nonce;
        if published == 1 {
            let absent = epoch.observe_absent(&target).unwrap();
            let publication = epoch
                .publish_absent(staged, &target, absent, &source_guard)
                .unwrap();
            drop(publication);
        } else if published >= 2 {
            let absent = epoch.observe_absent(&target).unwrap();
            let step = session
                .admit_step(&store, &live, StepKind::Publish, &cancel)
                .unwrap();
            let mut outcome = epoch
                .publish_connected(
                    &live,
                    &source_guard,
                    &target,
                    &private,
                    staged,
                    Some(absent),
                    None,
                    step,
                )
                .unwrap();
            if published >= 3 {
                store
                    .record_native_outcome(&mut live, &mut outcome, false, &cancel)
                    .unwrap();
            }
            if published >= 4 {
                store
                    .record_native_outcome(&mut live, &mut outcome, true, &cancel)
                    .unwrap();
            }
            if published == 5 {
                let mut comparison = outcome.comparison(&store, &live, &cancel).unwrap();
                let candidate = store
                    .prepare_baseline(&live, &mut comparison, &cancel)
                    .unwrap();
                drop(candidate);
            }
            drop(outcome);
        } else {
            drop(staged);
        }
        assert_eq!(
            target_path.join(&private_name).join("outcome").exists(),
            published >= 2
        );
        let expected_stage = if published >= 4 {
            ReceiptStage::Verified
        } else if published == 3 {
            ReceiptStage::Published
        } else {
            ReceiptStage::PreparedNoDisplacement
        };
        assert_eq!(
            store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
            expected_stage
        );
        drop(epoch);
        drop(source_guard);
        let original_intent = private.descriptor.as_ref().unwrap().2.clone();
        drop(private);
        crash_ready(tree.path(), id, reverse, &format!("addition-{published}"));
        drop(store);
        let held = tree.path().join("retained-original");
        let private_path = target_path.join(&private_name);
        match damage {
            RestartDamage::Source => {
                std::fs::rename(source_path.join(leaf), &held).unwrap();
                std::fs::write(source_path.join(leaf), b"later source").unwrap();
            }
            RestartDamage::Competitor => {
                if published != 0 {
                    std::fs::rename(target_path.join(leaf), &held).unwrap();
                }
                std::fs::write(target_path.join(leaf), b"competitor").unwrap();
            }
            RestartDamage::SameBytesOtherObject => {
                let path = if published == 0 {
                    private_path.join("stage")
                } else {
                    target_path.join(leaf)
                };
                std::fs::rename(&path, &held).unwrap();
                std::fs::write(path, b"new bytes").unwrap();
            }
            RestartDamage::UnknownChild => {
                std::fs::write(private_path.join("user-file"), b"unowned").unwrap();
            }
            RestartDamage::Descriptor => {
                std::fs::rename(private_path.join("intent"), &held).unwrap();
                std::fs::write(private_path.join("intent"), b"replaced clue").unwrap();
            }
            RestartDamage::SameBytesDescriptor => {
                use std::os::unix::fs::PermissionsExt;
                for name in ["intent", "staged-intent", "outcome"] {
                    let path = private_path.join(name);
                    if path.exists() {
                        let bytes = std::fs::read(&path).unwrap();
                        std::fs::rename(&path, tree.path().join(format!("retained-{name}")))
                            .unwrap();
                        std::fs::write(&path, bytes).unwrap();
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                            .unwrap();
                    }
                }
            }
            RestartDamage::SameBytesNamespace => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::rename(&private_path, &held).unwrap();
                std::fs::create_dir(&private_path).unwrap();
                std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                for entry in std::fs::read_dir(&held).unwrap() {
                    let entry = entry.unwrap();
                    // Move actual payload to preserve its identity: this isolates
                    // refusal of the replaced namespace generation itself.
                    std::fs::rename(entry.path(), private_path.join(entry.file_name())).unwrap();
                }
            }
            RestartDamage::DescriptorOversize => {
                std::fs::rename(private_path.join("intent"), &held).unwrap();
                std::fs::write(private_path.join("intent"), vec![b'x'; 16 * 1024 + 1]).unwrap();
            }
            RestartDamage::None | RestartDamage::Stopped | RestartDamage::Disconnect => {}
        }
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        assert!(store.check_native(&live).is_err());
        assert!(
            store
                .load_baseline(id.connection)
                .unwrap()
                .entries()
                .is_none()
        );
        let current = store.load_connection(id.connection).unwrap();
        let fence = (damage == RestartDamage::Disconnect).then(|| {
            store
                .request_disconnect(id.connection, current.revision)
                .unwrap()
        });
        let current = store.load_connection(id.connection).unwrap();
        let mut context = store.open_recovery_context(id, current.revision).unwrap();
        store
            .reserve_recovery_reads(
                &mut context,
                if nested {
                    EXECUTOR_READ_WINDOW
                } else {
                    32 * 1024 * 1024
                },
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let record = store
            .recovery_namespace_records(&context)
            .unwrap()
            .into_iter()
            .find(|record| record.name() == private_name)
            .unwrap();
        let epoch =
            NativeEpoch::begin_recovery(&context, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        assert_ne!(epoch.nonce, original_nonce);
        let inspected = epoch.inspect_regular(&context, &record);
        if !matches!(
            damage,
            RestartDamage::None
                | RestartDamage::SameBytesDescriptor
                | RestartDamage::Stopped
                | RestartDamage::Disconnect
        ) {
            assert!(inspected.is_err());
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert_eq!(
                store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
                expected_stage
            );
            if matches!(
                damage,
                RestartDamage::Source | RestartDamage::SameBytesOtherObject
            ) || (damage == RestartDamage::Competitor && published != 0)
            {
                assert_eq!(std::fs::read(&held).unwrap(), b"new bytes");
            }
            if damage == RestartDamage::Source {
                assert_eq!(
                    std::fs::read(source_path.join(leaf)).unwrap(),
                    b"later source"
                );
            }
            if damage == RestartDamage::Descriptor {
                assert_eq!(
                    std::fs::read(private_path.join("intent")).unwrap(),
                    b"replaced clue"
                );
                assert_eq!(std::fs::read(&held).unwrap(), original_intent);
            }
            if damage == RestartDamage::DescriptorOversize {
                assert_eq!(
                    std::fs::read(private_path.join("intent")).unwrap(),
                    vec![b'x'; 16 * 1024 + 1]
                );
                assert_eq!(std::fs::read(&held).unwrap(), original_intent);
            }
            if damage == RestartDamage::Competitor {
                assert_eq!(
                    std::fs::read(target_path.join(leaf)).unwrap(),
                    b"competitor"
                );
            }
            if damage == RestartDamage::UnknownChild {
                assert_eq!(
                    std::fs::read(private_path.join("user-file")).unwrap(),
                    b"unowned"
                );
            }
            return;
        }
        let mut recovered = inspected.unwrap();
        if let Some(fence) = fence {
            let mut boundary = recovered.safe_boundary(&context, &cancel).unwrap();
            let ack = store
                .record_boundary(&context, &mut boundary, &cancel)
                .unwrap();
            assert_eq!(ack.value().stage, ReceiptStage::AccountedPending);
            assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"new bytes");
            if published == 0 {
                assert!(!target_path.join(leaf).exists());
                assert_eq!(
                    std::fs::read(private_path.join("stage")).unwrap(),
                    b"new bytes"
                );
            } else {
                assert_eq!(std::fs::read(target_path.join(leaf)).unwrap(), b"new bytes");
            }
            assert_eq!(
                std::fs::read(private_path.join("intent")).unwrap(),
                original_intent
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            store.finish_disconnect(&fence).unwrap();
            store.checkpoint().unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert_eq!(
                store.load_connection(id.connection).unwrap().lifecycle,
                crate::connection::store::Lifecycle::Disconnected
            );
            assert_eq!(
                store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
                ReceiptStage::AccountedPending
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            return;
        }
        let session = store.transaction_session(id.connection).unwrap();
        if damage == RestartDamage::Stopped {
            session.request_disconnect();
            assert!(
                store
                    .resume_native(&context, &mut recovered, &session, &cancel)
                    .is_err()
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert_eq!(target_path.join(leaf).exists(), published != 0);
            assert_eq!(
                target_path.join(&private_name).join("stage").exists(),
                published == 0
            );
            return;
        }
        let mut resumed = store
            .resume_native(&context, &mut recovered, &session, &cancel)
            .unwrap();
        let step = session
            .admit_step(&store, &resumed, StepKind::Publish, &cancel)
            .unwrap();
        let mut outcome = recovered.complete_addition(&resumed, step).unwrap();
        if published < 3 {
            store
                .record_native_outcome(&mut resumed, &mut outcome, false, &cancel)
                .unwrap();
        }
        if published < 4 {
            store
                .record_native_outcome(&mut resumed, &mut outcome, true, &cancel)
                .unwrap();
        }
        let mut comparison = outcome.comparison(&store, &resumed, &cancel).unwrap();
        let candidate = store
            .prepare_baseline(&resumed, &mut comparison, &cancel)
            .unwrap();
        store
            .commit_baseline(&mut resumed, candidate, &mut comparison, &cancel)
            .unwrap();
        drop(comparison);
        drop(outcome);
        drop(recovered);
        crash_ready(tree.path(), id, reverse, "addition-6");
        assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"new bytes");
        assert_eq!(std::fs::read(target_path.join(leaf)).unwrap(), b"new bytes");
        assert!(
            store
                .pending_page(id.connection, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            store
                .load_baseline(id.connection)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            if nested { 3 } else { 1 }
        );
        assert!(
            store
                .recovery_page(id.connection, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(!target_path.join(&private_name).join("stage").exists());
        assert!(!target_path.join(&private_name).join("displaced").exists());
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_reopened_replacement_retains_and_rebinds_actual_quarantine_and_private_artifact() {
        reopened_replacement_case(false, 11, false);
        for reverse in [false, true] {
            for boundary in 0..=10 {
                reopened_replacement_case(reverse, boundary, false);
            }
        }
    }
    #[test]
    #[ignore = "HOLD: actual Linux provider/helper; bounded multi-epoch accounting bodies UNRUN"]
    fn native_missing_k_many_partial_slots_settle_monotonically_across_fresh_epochs() {
        for partials in [32, 127] {
            reopened_replacement_with_partials(false, 1, true, partials);
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep bounded real recovery epochs, durable budget progression and terminal reopen assertions together."
    )]
    fn drain_partial_epochs(
        tree: &std::path::Path,
        id: OperationId,
        source: &PinnedEndpoint,
        target: &PinnedEndpoint,
        private_name: &str,
        partials: usize,
    ) {
        use crate::connection::store::{ConnectionStore, RecoveryState};
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let (original, mut previous_budget) = {
            let store = ConnectionStore::open_at(&tree.join("store")).unwrap();
            (
                store.executor_receipt(id).unwrap(),
                store.native_budget_for_test(id).unwrap(),
            )
        };
        let mut previous = 0;
        let mut finished = false;
        let mut terminal_receipt = None;
        for attempt in 0..16 {
            let mut store = ConnectionStore::open_at(&tree.join("store")).unwrap();
            let current = store.load_connection(id.connection).unwrap();
            if attempt == 0 {
                store
                    .request_disconnect(id.connection, current.revision)
                    .unwrap();
            }
            let current = store.load_connection(id.connection).unwrap();
            let mut context = store.open_recovery_context(id, current.revision).unwrap();
            assert!(context.recovery_only());
            assert_eq!(context.id(), id);
            assert_eq!(store.native_budget_for_test(id).unwrap(), previous_budget);
            // Another persisted reservation against this same operation budget;
            // reopening never refunds unused read credit or creates a new operation.
            store
                .reserve_recovery_reads(
                    &mut context,
                    4 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let expected_budget = (
                previous_budget.0,
                previous_budget.1 + 4 * 1024 * 1024,
                previous_budget.2 + 1,
            );
            assert_eq!(store.native_budget_for_test(id).unwrap(), expected_budget);
            let record = store
                .recovery_namespace_records(&context)
                .unwrap()
                .into_iter()
                .find(|row| row.name() == private_name)
                .unwrap();
            let epoch = NativeEpoch::begin_recovery(&context, source, target, &cancel).unwrap();
            let recovered = epoch.inspect_regular(&context, &record).unwrap();
            let mut proof = recovered.safe_boundary(&context, &cancel).unwrap();
            let mut reader = proof.drain_reader(&store, &context).unwrap();
            let drained = store.drain_native_recovery(&mut context, &mut reader, &cancel);
            drop(reader);
            if drained.is_ok() {
                terminal_receipt = Some(
                    store
                        .record_boundary(&context, &mut proof, &cancel)
                        .unwrap()
                        .value()
                        .clone(),
                );
                finished = true;
            }
            drop((drained, proof, context, store));
            let reopened = ConnectionStore::open_at(&tree.join("store")).unwrap();
            let rows = reopened
                .recovery_page(id.connection, None, 128)
                .unwrap()
                .entries;
            let settled = rows
                .iter()
                .filter(|row| row.state == RecoveryState::RetainedIncomplete)
                .count();
            assert!(settled >= previous && settled <= partials);
            if !finished {
                assert!(
                    settled > previous,
                    "each exhausted epoch must make durable monotonic progress"
                );
            }
            previous = settled;
            let (pending, latest) = reopened.executor_position(id.connection).unwrap();
            assert_eq!(latest, Some(id));
            let receipt = reopened.executor_receipt(id).unwrap();
            assert_eq!(receipt.id, id);
            assert_eq!(receipt.intent, original.intent);
            assert_eq!(
                reopened.native_budget_for_test(id).unwrap(),
                expected_budget
            );
            previous_budget = expected_budget;
            if finished {
                assert_eq!(pending, None);
                assert_eq!(
                    receipt.stage,
                    crate::connection::store::ReceiptStage::AccountedPending
                );
                assert_eq!(Some(&receipt), terminal_receipt.as_ref());
            } else {
                assert_eq!(pending, Some(id));
                assert_ne!(
                    receipt.stage,
                    crate::connection::store::ReceiptStage::AccountedPending
                );
            }
            assert!(
                reopened
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert_eq!(
                std::fs::read(tree.join("source/file")).unwrap(),
                b"new bytes"
            );
            assert!(!tree.join("target/file").exists());
            assert_eq!(
                std::fs::read(tree.join("target").join(private_name).join("stage")).unwrap(),
                b"new bytes"
            );
            assert_eq!(
                std::fs::read(tree.join("target").join(private_name).join("displaced")).unwrap(),
                b"old bytes"
            );
            if finished {
                assert_eq!(settled, partials);
                assert_eq!(rows.len(), partials + 1);
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.state == RecoveryState::BytesVerified)
                        .count(),
                    1
                );
                break;
            }
        }
        assert!(
            finished,
            "bounded epochs must settle existing partial history without propagation"
        );
    }
    #[test]
    #[ignore = "authorized Linux filesystem/helper required; execution UNRUN"]
    fn native_reopened_replacement_accounts_staged_quarantine_and_publication_after_disconnect() {
        for reverse in [false, true] {
            for boundary in 0..=10 {
                reopened_replacement_case(reverse, boundary, true);
            }
        }
    }
    fn reopened_replacement_case(reverse: bool, boundary: u8, disconnect: bool) {
        reopened_replacement_with_partials(reverse, boundary, disconnect, 0);
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Preserve the exact native restart states and their durable assertions together."
    )]
    fn reopened_replacement_with_partials(
        reverse: bool,
        boundary: u8,
        disconnect: bool,
        partials: usize,
    ) {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
        };
        use std::sync::atomic::AtomicBool;
        let tree = fixture();
        let source_path = tree.path().join("source");
        let target_path = tree.path().join("target");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        std::fs::write(source_path.join("file"), b"new bytes").unwrap();
        std::fs::write(target_path.join("file"), b"old bytes").unwrap();
        let source_endpoint = endpoint(&source_path);
        let target_endpoint = endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let (left, right) = if reverse {
            (binding(&target_path), binding(&source_path))
        } else {
            (binding(&source_path), binding(&target_path))
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([52; 16]),
                left,
                right,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let session = store.transaction_session(record.id).unwrap();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"new bytes").into(),
            bytes: 9,
            executable: false,
        });
        let old = Some(EntryState::File {
            sha256: Sha256::digest(b"old bytes").into(),
            bytes: 9,
            executable: false,
        });
        let (source_before, destination_before, from, to) = if reverse {
            (old, value, ModelSide::Destination, ModelSide::Source)
        } else {
            (value, old, ModelSide::Source, ModelSide::Destination)
        };
        let operation = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"file").unwrap(),
            source_before,
            destination_before,
            decision: Decision::Propagate { from },
        };
        let cancel = AtomicBool::new(false);
        let (left, right) = if reverse {
            (&target_endpoint, &source_endpoint)
        } else {
            (&source_endpoint, &target_endpoint)
        };
        let reviewed = initial_preview(&store, record.id, left, right, &cancel)
            .unwrap()
            .accept(operation.clone())
            .unwrap();
        let admission = admission_metadata(&store, &operation, left, right, &cancel).unwrap();
        let mut live = store
            .begin_native_reviewed(operation, &session, reviewed, admission)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                16 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let reservation = store.reserve_native_namespace(&mut live, to).unwrap();
        let private_name = reservation.name();
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        let source = epoch.pin_parent(Side::Source, &live.intent().path).unwrap();
        let target = epoch.pin_parent(Side::Target, &live.intent().path).unwrap();
        let source_guard = epoch.guard_regular(&source).unwrap();
        let target_guard = epoch.guard_regular(&target).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .unwrap();
        let private = epoch
            .create_registered_namespace(
                &live,
                &reservation,
                &source,
                &target,
                &source_guard,
                Some(&target_guard),
                step,
            )
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Stage, &cancel)
            .unwrap();
        let staged = epoch
            .stage_connected(&live, &source_guard, &private, step)
            .unwrap();
        let prepared = epoch.prepared(&private, Some(&staged)).unwrap();
        let prepared = store
            .prepare_native(&mut live, reservation, prepared)
            .unwrap();
        let id = live.id();
        let mut staged = Some(staged);
        if boundary != 0 {
            let displaced = if boundary == 1 {
                epoch
                    .relocate_noreplace(&target, &private, &target_guard)
                    .unwrap()
            } else {
                let step = session
                    .admit_step(&store, &live, StepKind::Relocate, &cancel)
                    .unwrap();
                epoch
                    .relocate_connected(
                        &live,
                        &target,
                        &private,
                        &source_guard,
                        &target_guard,
                        &prepared,
                        step,
                    )
                    .unwrap()
            };
            if boundary >= 3 {
                let guard = epoch.guard_displaced(&displaced).unwrap();
                let slot = store
                    .reserve_native_recovery(
                        &mut live,
                        to,
                        crate::connection::store::RecoveryReason::DisplacedVersion,
                    )
                    .unwrap();
                if boundary == 5 || boundary == 11 {
                    // Exact real reserved file with a simulated interrupted copy prefix.
                    crate::connection::store::partial_native_recovery_for_test(
                        &mut store, &slot, b"old",
                    )
                    .unwrap();
                    if boundary == 11 {
                        use crate::connection::store::{RecoveryId, RecoveryReason, RecoveryState};
                        store
                            .retain_native_incomplete(
                                &mut live,
                                RecoveryId {
                                    operation: id,
                                    slot: 0,
                                },
                            )
                            .unwrap();
                        for n in 1..128 {
                            let reserved = store
                                .reserve_native_recovery(
                                    &mut live,
                                    to,
                                    RecoveryReason::DisplacedVersion,
                                )
                                .unwrap();
                            crate::connection::store::partial_native_recovery_for_test(
                                &mut store, &reserved, b"old",
                            )
                            .unwrap();
                            store
                                .retain_native_incomplete(
                                    &mut live,
                                    RecoveryId {
                                        operation: id,
                                        slot: n,
                                    },
                                )
                                .unwrap();
                        }
                        assert!(
                            store
                                .reserve_native_recovery(
                                    &mut live,
                                    to,
                                    RecoveryReason::DisplacedVersion
                                )
                                .is_err()
                        );
                        let entries = store
                            .recovery_page(id.connection, None, 128)
                            .unwrap()
                            .entries;
                        assert_eq!(entries.len(), 128);
                        assert!(
                            entries
                                .iter()
                                .all(|entry| entry.state == RecoveryState::RetainedIncomplete)
                        );
                        assert!(
                            store
                                .load_baseline(id.connection)
                                .unwrap()
                                .entries()
                                .is_none()
                        );
                        assert_eq!(
                            std::fs::read(source_path.join("file")).unwrap(),
                            b"new bytes"
                        );
                        assert!(!target_path.join("file").exists());
                        assert_eq!(
                            std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
                            b"new bytes"
                        );
                        assert_eq!(
                            std::fs::read(target_path.join(&private_name).join("displaced"))
                                .unwrap(),
                            b"old bytes"
                        );
                        store.checkpoint().unwrap();
                        drop(store);
                        let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                        assert_eq!(store.executor_position(id.connection).unwrap().0, Some(id));
                        assert_eq!(
                            store
                                .recovery_page(id.connection, None, 128)
                                .unwrap()
                                .entries,
                            entries
                        );
                        for entry in entries {
                            assert_eq!(
                                std::fs::read(
                                    tree.path()
                                        .join("store/recovery")
                                        .join("34".repeat(16))
                                        .join(format!("{}-{}", id.serial, entry.id.slot))
                                )
                                .unwrap(),
                                b"old"
                            );
                        }
                        assert!(
                            store
                                .load_baseline(id.connection)
                                .unwrap()
                                .entries()
                                .is_none()
                        );
                        return;
                    }
                } else {
                    let step = session
                        .admit_step(&store, &live, StepKind::Preserve, &cancel)
                        .unwrap();
                    let mut reader = epoch
                        .recovery_source(&live, &private, &displaced, &guard, step)
                        .unwrap();
                    if boundary == 4 {
                        let disconnect_session = store.transaction_session(id.connection).unwrap();
                        crate::connection::store::after_native_bytes_for_test(move || {
                            disconnect_session.request_disconnect();
                        });
                    }
                    let bound = store.preserve_native(&mut live, slot, &mut reader, &cancel);
                    drop(reader);
                    if boundary == 4 {
                        assert!(bound.is_err());
                    } else {
                        let bound = bound.unwrap();
                        if boundary >= 6 {
                            let sealed = epoch
                                .seal_displacement(&live, displaced, guard, &private, bound)
                                .unwrap();
                            epoch.check_absent(&target).unwrap(); // Read-only vacancy check after relocation.
                            if boundary == 6 {
                                let vacancy = epoch
                                    .replacement_vacancy(&live, &target, &private, &sealed)
                                    .unwrap();
                                let publication = epoch
                                    .publish_to_vacancy(
                                        staged.take().unwrap(),
                                        &target,
                                        vacancy,
                                        &source_guard,
                                    )
                                    .unwrap();
                                drop(publication);
                                drop(sealed);
                            } else {
                                let step = session
                                    .admit_step(&store, &live, StepKind::Publish, &cancel)
                                    .unwrap();
                                let mut outcome = epoch
                                    .publish_connected(
                                        &live,
                                        &source_guard,
                                        &target,
                                        &private,
                                        staged.take().unwrap(),
                                        None,
                                        Some(sealed),
                                        step,
                                    )
                                    .unwrap();
                                if boundary >= 8 {
                                    store
                                        .record_native_outcome(
                                            &mut live,
                                            &mut outcome,
                                            false,
                                            &cancel,
                                        )
                                        .unwrap();
                                }
                                if boundary >= 9 {
                                    store
                                        .record_native_outcome(
                                            &mut live,
                                            &mut outcome,
                                            true,
                                            &cancel,
                                        )
                                        .unwrap();
                                }
                                if boundary == 10 {
                                    let mut comparison =
                                        outcome.comparison(&store, &live, &cancel).unwrap();
                                    drop(
                                        store
                                            .prepare_baseline(&live, &mut comparison, &cancel)
                                            .unwrap(),
                                    );
                                }
                                drop(outcome);
                            }
                        }
                    }
                }
            }
        }
        for _ in 0..partials {
            let slot = store
                .reserve_native_recovery(
                    &mut live,
                    to,
                    crate::connection::store::RecoveryReason::DisplacedVersion,
                )
                .unwrap();
            crate::connection::store::partial_native_recovery_for_test(&mut store, &slot, b"old")
                .unwrap();
        }
        let original_nonce = epoch.nonce;
        drop(epoch);
        drop(staged);
        drop(target_guard);
        drop(source_guard);
        drop(private);
        crash_ready(tree.path(), id, reverse, &format!("replacement-{boundary}"));
        drop(store);
        if partials > 0 {
            drain_partial_epochs(
                tree.path(),
                id,
                &source_endpoint,
                &target_endpoint,
                &private_name,
                partials,
            );
            return;
        }
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        assert!(store.check_native(&live).is_err());
        let current = store.load_connection(id.connection).unwrap();
        let fence = disconnect.then(|| {
            store
                .request_disconnect(id.connection, current.revision)
                .unwrap()
        });
        let current = store.load_connection(id.connection).unwrap();
        let mut context = store.open_recovery_context(id, current.revision).unwrap();
        store
            .reserve_recovery_reads(
                &mut context,
                48 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let record = store
            .recovery_namespace_records(&context)
            .unwrap()
            .into_iter()
            .find(|record| record.name() == private_name)
            .unwrap();
        let epoch =
            NativeEpoch::begin_recovery(&context, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        assert_ne!(epoch.nonce, original_nonce);
        let mut recovered = epoch.inspect_regular(&context, &record).unwrap();
        if let Some(fence) = fence {
            let mut proof = recovered.safe_boundary(&context, &cancel).unwrap();
            if boundary != 0 {
                let mut reader = proof.drain_reader(&store, &context).unwrap();
                drop(
                    store
                        .drain_native_recovery(&mut context, &mut reader, &cancel)
                        .unwrap(),
                );
                let count = store
                    .recovery_page(id.connection, None, 8)
                    .unwrap()
                    .entries
                    .len();
                let revision = context.revision();
                drop(
                    store
                        .drain_native_recovery(&mut context, &mut reader, &cancel)
                        .unwrap(),
                );
                assert_eq!(context.revision(), revision);
                assert_eq!(
                    store
                        .recovery_page(id.connection, None, 8)
                        .unwrap()
                        .entries
                        .len(),
                    count
                );
                let versions = store.recovery_page(id.connection, None, 8).unwrap();
                let current = versions
                    .entries
                    .iter()
                    .rev()
                    .find(|entry| {
                        entry.state == crate::connection::store::RecoveryState::BytesVerified
                    })
                    .unwrap();
                let connection_name: String =
                    id.connection
                        .0
                        .iter()
                        .fold(String::new(), |mut encoded, byte| {
                            use std::fmt::Write as _;
                            write!(&mut encoded, "{byte:02x}").unwrap();
                            encoded
                        });
                assert_eq!(
                    std::fs::read(
                        tree.path()
                            .join("store/recovery")
                            .join(connection_name)
                            .join(format!("{}-{}", id.serial, current.id.slot))
                    )
                    .unwrap(),
                    b"old bytes"
                );
                if boundary == 5 {
                    assert_eq!(
                        versions.entries[0].state,
                        crate::connection::store::RecoveryState::RetainedIncomplete
                    );
                }
            }
            store
                .record_boundary(&context, &mut proof, &cancel)
                .unwrap();
            assert_eq!(
                std::fs::read(source_path.join("file")).unwrap(),
                b"new bytes"
            );
            let private_path = target_path.join(&private_name);
            if boundary == 0 {
                assert_eq!(
                    std::fs::read(target_path.join("file")).unwrap(),
                    b"old bytes"
                );
            } else {
                assert_eq!(
                    std::fs::read(private_path.join("displaced")).unwrap(),
                    b"old bytes"
                );
            }
            if boundary >= 6 {
                assert_eq!(
                    std::fs::read(target_path.join("file")).unwrap(),
                    b"new bytes"
                );
            } else {
                assert_eq!(
                    std::fs::read(private_path.join("stage")).unwrap(),
                    b"new bytes"
                );
            }
            if (1..=5).contains(&boundary) {
                assert!(!target_path.join("file").exists());
            }
            if boundary >= 3 {
                let connection_name: String =
                    id.connection
                        .0
                        .iter()
                        .fold(String::new(), |mut encoded, byte| {
                            use std::fmt::Write as _;
                            write!(&mut encoded, "{byte:02x}").unwrap();
                            encoded
                        });
                let artifact = tree
                    .path()
                    .join("store/recovery")
                    .join(connection_name)
                    .join(format!("{}-0", id.serial));
                assert_eq!(
                    std::fs::read(artifact).unwrap(),
                    if boundary == 5 {
                        b"old".as_slice()
                    } else {
                        b"old bytes"
                    }
                );
            }
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            store.finish_disconnect(&fence).unwrap();
            store.checkpoint().unwrap();
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert_eq!(
                store.load_connection(id.connection).unwrap().lifecycle,
                crate::connection::store::Lifecycle::Disconnected
            );
            assert_eq!(
                store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
                crate::connection::store::ReceiptStage::AccountedPending
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            return;
        }
        let session = store.transaction_session(id.connection).unwrap();
        let mut live = store
            .resume_native(&context, &mut recovered, &session, &cancel)
            .unwrap();
        let mut outcome = recovered
            .complete_regular(&mut store, &mut live, &session, &cancel)
            .unwrap();
        if boundary < 8 {
            store
                .record_native_outcome(&mut live, &mut outcome, false, &cancel)
                .unwrap();
        }
        if boundary < 9 {
            store
                .record_native_outcome(&mut live, &mut outcome, true, &cancel)
                .unwrap();
        }
        let mut comparison = outcome.comparison(&store, &live, &cancel).unwrap();
        let candidate = store
            .prepare_baseline(&live, &mut comparison, &cancel)
            .unwrap();
        store
            .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
            .unwrap();
        assert_eq!(
            std::fs::read(source_path.join("file")).unwrap(),
            b"new bytes"
        );
        assert_eq!(
            std::fs::read(target_path.join("file")).unwrap(),
            b"new bytes"
        );
        assert_eq!(
            std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
            b"old bytes"
        );
        let versions = store.recovery_page(id.connection, None, 8).unwrap();
        assert_eq!(versions.entries.len(), if boundary == 5 { 2 } else { 1 });
        if boundary == 5 {
            assert_eq!(
                versions.entries[0].state,
                crate::connection::store::RecoveryState::RetainedIncomplete
            );
        }
        let connection_name: String =
            id.connection
                .0
                .iter()
                .fold(String::new(), |mut encoded, byte| {
                    use std::fmt::Write as _;
                    write!(&mut encoded, "{byte:02x}").unwrap();
                    encoded
                });
        let recovery_directory = tree.path().join("store/recovery").join(connection_name);
        let artifact =
            recovery_directory.join(format!("{}-{}", id.serial, u8::from(boundary == 5)));
        assert_eq!(std::fs::read(artifact).unwrap(), b"old bytes");
        if boundary == 5 {
            assert_eq!(
                std::fs::read(recovery_directory.join(format!("{}-0", id.serial))).unwrap(),
                b"old"
            );
        }
        assert!(
            store
                .pending_page(id.connection, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            store
                .load_baseline(id.connection)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_reopened_deletion_rebinds_tombstones_quarantine_and_recovery_before_baseline() {
        for reverse in [false, true] {
            for boundary in 1..=10 {
                connected_addition_case(reverse, Some(boundary), false);
            }
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem/helper required; execution UNRUN"]
    fn native_reopened_deletion_accounts_actual_tombstone_and_quarantine_without_baseline() {
        for reverse in [false, true] {
            for boundary in 1..=10 {
                connected_addition_case(reverse, Some(boundary), true);
            }
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem/helper required; execution UNRUN"]
    fn native_failed_paid_deletion_preflight_refuses_all_steps_and_retains_exact_bytes() {
        for reverse in [false, true] {
            connected_addition_case(reverse, Some(11), false);
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn connected_addition_case(reverse: bool, deletion: Option<u8>, disconnect: bool) {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, ReceiptStage,
            },
        };
        use std::sync::atomic::AtomicBool;
        let tree = fixture();
        let source_path = tree.path().join("source");
        let target_path = tree.path().join("target");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        std::fs::write(source_path.join("file"), b"new bytes").unwrap();
        let source_endpoint = endpoint(&source_path);
        let target_endpoint = endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let (left, right) = if reverse {
            (binding(&target_path), binding(&source_path))
        } else {
            (binding(&source_path), binding(&target_path))
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([52; 16]),
                left,
                right,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let session = store.transaction_session(record.id).unwrap();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"new bytes").into(),
            bytes: 9,
            executable: false,
        });
        let (source_before, destination_before, from, to) = if reverse {
            (None, value, ModelSide::Destination, ModelSide::Source)
        } else {
            (value, None, ModelSide::Source, ModelSide::Destination)
        };
        let operation = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"file").unwrap(),
            source_before,
            destination_before,
            decision: Decision::Propagate { from },
        };
        let cancel = AtomicBool::new(false);
        let (left, right) = if reverse {
            (&target_endpoint, &source_endpoint)
        } else {
            (&source_endpoint, &target_endpoint)
        };
        let reviewed = initial_preview(&store, record.id, left, right, &cancel)
            .unwrap()
            .accept(operation.clone())
            .unwrap();
        let admission = admission_metadata(&store, &operation, left, right, &cancel).unwrap();
        let mut live = store
            .begin_native_reviewed(operation, &session, reviewed, admission)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                16 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let reservation = store.reserve_native_namespace(&mut live, to).unwrap();
        let private_name = reservation.name();
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        let source = epoch.pin_parent(Side::Source, &live.intent().path).unwrap();
        let target = epoch.pin_parent(Side::Target, &live.intent().path).unwrap();
        let source_guard = epoch.guard_regular(&source).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .unwrap();
        let private = epoch
            .create_registered_namespace(
                &live,
                &reservation,
                &source,
                &target,
                &source_guard,
                None,
                step,
            )
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Stage, &cancel)
            .unwrap();
        let staged = epoch
            .stage_connected(&live, &source_guard, &private, step)
            .unwrap();
        let prepared = epoch.prepared(&private, Some(&staged)).unwrap();
        store
            .prepare_native(&mut live, reservation, prepared)
            .unwrap();
        let absent = epoch.observe_absent(&target).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Publish, &cancel)
            .unwrap();
        let mut outcome = epoch
            .publish_connected(
                &live,
                &source_guard,
                &target,
                &private,
                staged,
                Some(absent),
                None,
                step,
            )
            .unwrap();
        store
            .record_native_outcome(&mut live, &mut outcome, false, &cancel)
            .unwrap();
        store
            .record_native_outcome(&mut live, &mut outcome, true, &cancel)
            .unwrap();
        assert_eq!(
            std::fs::read(source_path.join("file")).unwrap(),
            b"new bytes"
        );
        assert_eq!(
            std::fs::read(target_path.join("file")).unwrap(),
            b"new bytes"
        );
        assert!(!target_path.join(&private_name).join("stage").exists());
        assert!(!target_path.join(&private_name).join("displaced").exists());
        assert!(target_path.join(&private_name).join("outcome").is_file());
        assert!(
            store
                .recovery_page(live.id().connection, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            store
                .pending_page(live.id().connection, None, 8)
                .unwrap()
                .entries[0]
                .stage,
            ReceiptStage::Verified
        );
        assert!(
            store
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .is_none()
        );
        let mut comparison = outcome.comparison(&store, &live, &cancel).unwrap();
        let candidate = store
            .prepare_baseline(&live, &mut comparison, &cancel)
            .unwrap();
        assert!(
            store
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .is_none()
        );
        store
            .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
            .unwrap();
        drop(comparison);
        drop(outcome);
        assert_eq!(
            store
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .unwrap()
                .get(&live.intent().path),
            Some(&EntryState::File {
                sha256: Sha256::digest(b"new bytes").into(),
                bytes: 9,
                executable: false
            })
        );
        assert!(
            store
                .pending_page(live.id().connection, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        drop(epoch);
        drop(source_guard);
        drop(store);
        let mut reopened = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        assert_eq!(
            reopened
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            1
        );
        let current = reopened.load_connection(live.id().connection).unwrap();
        let mut context = reopened
            .open_recovery_context(live.id(), current.revision)
            .unwrap();
        reopened
            .reserve_recovery_reads(
                &mut context,
                16 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let mut final_epoch =
            NativeEpoch::begin_recovery(&context, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        assert!(
            final_epoch
                .final_comparison(&reopened, &context, &cancel)
                .unwrap()
        );
        if let Some(restart) = deletion {
            drop(final_epoch);
            // User's deletion is the proposal; transaction code only relocates.
            std::fs::remove_file(source_path.join("file")).unwrap();
            let current = reopened.load_connection(live.id().connection).unwrap();
            let session = reopened.transaction_session(current.id).unwrap();
            let value = if restart == 11 {
                // A changed surviving proposal must fail the paid baseline join
                // before any deletion namespace or relocation can be admitted.
                std::fs::rename(
                    target_path.join("file"),
                    target_path.join("retained-before-edit"),
                )
                .unwrap();
                std::fs::write(target_path.join("file"), b"bad bytes").unwrap();
                Some(EntryState::File {
                    sha256: Sha256::digest(b"bad bytes").into(),
                    bytes: 9,
                    executable: false,
                })
            } else {
                Some(EntryState::File {
                    sha256: Sha256::digest(b"new bytes").into(),
                    bytes: 9,
                    executable: false,
                })
            };
            let (source_before, destination_before) = if reverse {
                (value, None)
            } else {
                (None, value)
            };
            let deletion_intent = OperationIntent {
                connection: current.id,
                expected_revision: current.revision,
                expected_baseline: current.baseline_revision,
                path: RelativePath::unix(b"file").unwrap(),
                source_before,
                destination_before,
                decision: Decision::RecoverDeletion { at: to },
            };
            let admission =
                admission_metadata(&reopened, &deletion_intent, left, right, &cancel).unwrap();
            let mut deletion_live = reopened
                .begin_native(deletion_intent, &session, admission)
                .unwrap();
            assert!(
                NativeEpoch::begin_connected(
                    &deletion_live,
                    &source_endpoint,
                    &target_endpoint,
                    &cancel
                )
                .is_err()
            );
            for kind in [
                StepKind::Namespace,
                StepKind::Stage,
                StepKind::Relocate,
                StepKind::Preserve,
                StepKind::Publish,
            ] {
                assert!(
                    session
                        .admit_step(&reopened, &deletion_live, kind, &cancel)
                        .is_err()
                );
            }
            if restart == 11 {
                assert!(
                    reopened
                        .reserve_native_work(
                            &mut deletion_live,
                            &session,
                            16 * 1024 * 1024,
                            Instant::now() + std::time::Duration::from_secs(30),
                            &cancel
                        )
                        .is_err()
                );
                assert!(
                    NativeEpoch::begin_connected(
                        &deletion_live,
                        &source_endpoint,
                        &target_endpoint,
                        &cancel
                    )
                    .is_err()
                );
                for kind in [
                    StepKind::Namespace,
                    StepKind::Stage,
                    StepKind::Relocate,
                    StepKind::Preserve,
                    StepKind::Publish,
                ] {
                    assert!(
                        session
                            .admit_step(&reopened, &deletion_live, kind, &cancel)
                            .is_err()
                    );
                }
                assert!(!source_path.join("file").exists());
                assert_eq!(
                    std::fs::read(target_path.join("file")).unwrap(),
                    b"bad bytes"
                );
                assert_eq!(
                    std::fs::read(target_path.join("retained-before-edit")).unwrap(),
                    b"new bytes"
                );
                assert_eq!(
                    std::fs::read_dir(&target_path)
                        .unwrap()
                        .filter(|entry| entry
                            .as_ref()
                            .unwrap()
                            .file_name()
                            .as_bytes()
                            .starts_with(b".qos-operation-"))
                        .count(),
                    1
                );
                assert!(
                    reopened
                        .recovery_page(current.id, None, 8)
                        .unwrap()
                        .entries
                        .is_empty()
                );
                assert_eq!(
                    reopened
                        .load_baseline(current.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&deletion_live.intent().path),
                    Some(&EntryState::File {
                        sha256: Sha256::digest(b"new bytes").into(),
                        bytes: 9,
                        executable: false
                    })
                );
                return;
            }
            assert_eq!(
                std::fs::read(target_path.join("file")).unwrap(),
                b"new bytes"
            );
            reopened
                .reserve_native_work(
                    &mut deletion_live,
                    &session,
                    16 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let reserved = reopened
                .reserve_native_namespace(&mut deletion_live, to)
                .unwrap();
            let deletion_name = reserved.name();
            let mut epoch = NativeEpoch::begin_connected(
                &deletion_live,
                &source_endpoint,
                &target_endpoint,
                &cancel,
            )
            .unwrap();
            let source = epoch
                .pin_parent(Side::Source, &deletion_live.intent().path)
                .unwrap();
            let target = epoch
                .pin_parent(Side::Target, &deletion_live.intent().path)
                .unwrap();
            let target_guard = epoch.guard_regular(&target).unwrap();
            epoch
                .verify_deletion_scope(&reopened, &deletion_live, &cancel)
                .unwrap();
            let step = session
                .admit_step(&reopened, &deletion_live, StepKind::Namespace, &cancel)
                .unwrap();
            let private = epoch
                .create_deletion_namespace(
                    &deletion_live,
                    &reserved,
                    &source,
                    &target,
                    &target_guard,
                    step,
                )
                .unwrap();
            let prepared = epoch.prepared(&private, None).unwrap();
            let prepared = reopened
                .prepare_native(&mut deletion_live, reserved, prepared)
                .unwrap();
            if restart >= 6 {
                if restart >= 7 {
                    let displaced = if restart == 7 {
                        epoch
                            .relocate_noreplace(&target, &private, &target_guard)
                            .unwrap()
                    } else {
                        let step = session
                            .admit_step(&reopened, &deletion_live, StepKind::Relocate, &cancel)
                            .unwrap();
                        epoch
                            .relocate_deletion(
                                &deletion_live,
                                &source,
                                &target,
                                &private,
                                &target_guard,
                                &prepared,
                                step,
                            )
                            .unwrap()
                    };
                    if restart >= 9 {
                        let guard = epoch.guard_displaced(&displaced).unwrap();
                        let slot = reopened
                            .reserve_native_recovery(
                                &mut deletion_live,
                                to,
                                crate::connection::store::RecoveryReason::NormalDeletion,
                            )
                            .unwrap();
                        if restart == 9 {
                            crate::connection::store::partial_native_recovery_for_test(
                                &mut reopened,
                                &slot,
                                b"new",
                            )
                            .unwrap();
                        } else {
                            let step = session
                                .admit_step(&reopened, &deletion_live, StepKind::Preserve, &cancel)
                                .unwrap();
                            let mut reader = epoch
                                .recovery_source(&deletion_live, &private, &displaced, &guard, step)
                                .unwrap();
                            let disconnect_session = reopened
                                .transaction_session(deletion_live.id().connection)
                                .unwrap();
                            crate::connection::store::after_native_bytes_for_test(move || {
                                disconnect_session.request_disconnect()
                            });
                            assert!(
                                reopened
                                    .preserve_native(&mut deletion_live, slot, &mut reader, &cancel)
                                    .is_err()
                            );
                        }
                    }
                }
                drop(epoch);
                drop(target_guard);
                drop(private);
                crash_ready(
                    tree.path(),
                    deletion_live.id(),
                    reverse,
                    &format!("deletion-{restart}"),
                );
                drop(reopened);
                finish_reopened_deletion_case(
                    tree.path(),
                    &source_path,
                    &target_path,
                    &source_endpoint,
                    &target_endpoint,
                    deletion_live.id(),
                    &deletion_name,
                    restart,
                    disconnect,
                    &cancel,
                );
                return;
            }
            let step = session
                .admit_step(&reopened, &deletion_live, StepKind::Relocate, &cancel)
                .unwrap();
            let displaced = epoch
                .relocate_deletion(
                    &deletion_live,
                    &source,
                    &target,
                    &private,
                    &target_guard,
                    &prepared,
                    step,
                )
                .unwrap();
            let displaced_guard = epoch.guard_displaced(&displaced).unwrap();
            let slot = reopened
                .reserve_native_recovery(
                    &mut deletion_live,
                    to,
                    crate::connection::store::RecoveryReason::NormalDeletion,
                )
                .unwrap();
            let step = session
                .admit_step(&reopened, &deletion_live, StepKind::Preserve, &cancel)
                .unwrap();
            let mut reader = epoch
                .recovery_source(&deletion_live, &private, &displaced, &displaced_guard, step)
                .unwrap();
            let bound = reopened
                .preserve_native(&mut deletion_live, slot, &mut reader, &cancel)
                .unwrap();
            drop(reader);
            let sealed = epoch
                .seal_displacement(&deletion_live, displaced, displaced_guard, &private, bound)
                .unwrap();
            if restart != 0 {
                if restart >= 2 {
                    let step = session
                        .admit_step(&reopened, &deletion_live, StepKind::Publish, &cancel)
                        .unwrap();
                    let mut outcome = epoch
                        .deletion_outcome(&deletion_live, &source, &target, &private, sealed, step)
                        .unwrap();
                    if restart >= 3 {
                        reopened
                            .record_native_deletion(
                                &mut deletion_live,
                                &mut outcome,
                                false,
                                &cancel,
                            )
                            .unwrap();
                    }
                    if restart >= 4 {
                        reopened
                            .record_native_deletion(&mut deletion_live, &mut outcome, true, &cancel)
                            .unwrap();
                    }
                    if restart == 5 {
                        let mut comparison = outcome
                            .comparison(&reopened, &deletion_live, &cancel)
                            .unwrap();
                        drop(
                            reopened
                                .prepare_baseline(&deletion_live, &mut comparison, &cancel)
                                .unwrap(),
                        );
                    }
                    drop(outcome);
                } else {
                    drop(sealed);
                }
                drop(epoch);
                drop(target_guard);
                drop(private);
                crash_ready(
                    tree.path(),
                    deletion_live.id(),
                    reverse,
                    &format!("deletion-{restart}"),
                );
                drop(reopened);
                finish_reopened_deletion_case(
                    tree.path(),
                    &source_path,
                    &target_path,
                    &source_endpoint,
                    &target_endpoint,
                    deletion_live.id(),
                    &deletion_name,
                    restart,
                    disconnect,
                    &cancel,
                );
                return;
            }
            let step = session
                .admit_step(&reopened, &deletion_live, StepKind::Publish, &cancel)
                .unwrap();
            let mut outcome = epoch
                .deletion_outcome(&deletion_live, &source, &target, &private, sealed, step)
                .unwrap();
            reopened
                .record_native_deletion(&mut deletion_live, &mut outcome, false, &cancel)
                .unwrap();
            reopened
                .record_native_deletion(&mut deletion_live, &mut outcome, true, &cancel)
                .unwrap();
            assert_eq!(
                reopened
                    .load_baseline(current.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .len(),
                1
            );
            let mut comparison = outcome
                .comparison(&reopened, &deletion_live, &cancel)
                .unwrap();
            let candidate = reopened
                .prepare_baseline(&deletion_live, &mut comparison, &cancel)
                .unwrap();
            assert_eq!(
                reopened
                    .load_baseline(current.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .len(),
                1
            );
            reopened
                .commit_baseline(&mut deletion_live, candidate, &mut comparison, &cancel)
                .unwrap();
            drop(comparison);
            drop(outcome);
            drop(epoch);
            drop(target_guard);
            assert!(!source_path.join("file").exists());
            assert!(!target_path.join("file").exists());
            assert!(!target_path.join(&deletion_name).join("stage").exists());
            assert_eq!(
                std::fs::read(target_path.join(&deletion_name).join("displaced")).unwrap(),
                b"new bytes"
            );
            let connection_name: String =
                current
                    .id
                    .0
                    .iter()
                    .fold(String::new(), |mut encoded, byte| {
                        use std::fmt::Write as _;
                        write!(&mut encoded, "{byte:02x}").unwrap();
                        encoded
                    });
            let artifact = tree
                .path()
                .join("store/recovery")
                .join(connection_name)
                .join(format!("{}-0", deletion_live.id().serial));
            assert_eq!(std::fs::read(&artifact).unwrap(), b"new bytes");
            drop(reopened);
            let mut reopened = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert!(
                reopened
                    .load_baseline(current.id)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .is_empty()
            );
            let current = reopened.load_connection(current.id).unwrap();
            let mut recovery = reopened
                .open_recovery_context(deletion_live.id(), current.revision)
                .unwrap();
            reopened
                .reserve_recovery_reads(
                    &mut recovery,
                    4 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let mut epoch =
                NativeEpoch::begin_recovery(&recovery, &source_endpoint, &target_endpoint, &cancel)
                    .unwrap();
            assert!(
                epoch
                    .final_comparison(&reopened, &recovery, &cancel)
                    .unwrap()
            );
            assert_eq!(std::fs::read(artifact).unwrap(), b"new bytes");
            return;
        }
        std::fs::write(source_path.join("file"), b"later edit").unwrap();
        assert!(
            !final_epoch
                .final_comparison(&reopened, &context, &cancel)
                .unwrap()
        );
        assert_eq!(
            std::fs::read(target_path.join("file")).unwrap(),
            b"new bytes"
        );
        assert_eq!(
            reopened
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .unwrap()
                .get(&live.intent().path),
            Some(&EntryState::File {
                sha256: Sha256::digest(b"new bytes").into(),
                bytes: 9,
                executable: false
            })
        );
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn finish_reopened_deletion_case(
        tree: &std::path::Path,
        source_path: &std::path::Path,
        target_path: &std::path::Path,
        source_endpoint: &PinnedEndpoint,
        target_endpoint: &PinnedEndpoint,
        id: OperationId,
        deletion_name: &str,
        restart: u8,
        disconnect: bool,
        cancel: &std::sync::atomic::AtomicBool,
    ) {
        use crate::connection::store::ConnectionStore;
        let mut reopened = ConnectionStore::open_at(&tree.join("store")).unwrap();
        let current = reopened.load_connection(id.connection).unwrap();
        let fence = disconnect.then(|| {
            reopened
                .request_disconnect(id.connection, current.revision)
                .unwrap()
        });
        let current = reopened.load_connection(id.connection).unwrap();
        let mut context = reopened
            .open_recovery_context(id, current.revision)
            .unwrap();
        reopened
            .reserve_recovery_reads(
                &mut context,
                32 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let record = reopened
            .recovery_namespace_records(&context)
            .unwrap()
            .into_iter()
            .find(|record| record.name() == deletion_name)
            .unwrap();
        let epoch =
            NativeEpoch::begin_recovery(&context, source_endpoint, target_endpoint, &cancel)
                .unwrap();
        let mut recovered = epoch
            .inspect_deletion(&context, &record, &reopened, &cancel)
            .unwrap();
        if let Some(fence) = fence {
            let mut boundary = recovered.safe_boundary(&context, cancel).unwrap();
            if restart != 6 {
                let mut reader = boundary.drain_reader(&reopened, &context).unwrap();
                drop(
                    reopened
                        .drain_native_recovery(&mut context, &mut reader, cancel)
                        .unwrap(),
                );
                let revision = context.revision();
                drop(
                    reopened
                        .drain_native_recovery(&mut context, &mut reader, cancel)
                        .unwrap(),
                );
                assert_eq!(context.revision(), revision);
                let versions = reopened.recovery_page(id.connection, None, 128).unwrap();
                let current = versions
                    .entries
                    .iter()
                    .rev()
                    .find(|entry| {
                        entry.id.operation == id
                            && entry.state == crate::connection::store::RecoveryState::BytesVerified
                    })
                    .unwrap();
                let connection_name: String =
                    id.connection
                        .0
                        .iter()
                        .fold(String::new(), |mut encoded, byte| {
                            use std::fmt::Write as _;
                            write!(&mut encoded, "{byte:02x}").unwrap();
                            encoded
                        });
                assert_eq!(
                    std::fs::read(
                        tree.join("store/recovery")
                            .join(connection_name)
                            .join(format!("{}-{}", id.serial, current.id.slot))
                    )
                    .unwrap(),
                    b"new bytes"
                );
                if restart == 9 {
                    assert_eq!(
                        versions
                            .entries
                            .iter()
                            .find(|entry| entry.id.operation == id && entry.id.slot == 0)
                            .unwrap()
                            .state,
                        crate::connection::store::RecoveryState::RetainedIncomplete
                    );
                }
            }
            reopened
                .record_boundary(&context, &mut boundary, cancel)
                .unwrap();
            assert!(!source_path.join("file").exists());
            if restart == 6 {
                assert_eq!(
                    std::fs::read(target_path.join("file")).unwrap(),
                    b"new bytes"
                );
            } else {
                assert!(!target_path.join("file").exists());
                assert_eq!(
                    std::fs::read(target_path.join(deletion_name).join("displaced")).unwrap(),
                    b"new bytes"
                );
            }
            if !matches!(restart, 6..=8) {
                let connection_name: String =
                    id.connection
                        .0
                        .iter()
                        .fold(String::new(), |mut encoded, byte| {
                            use std::fmt::Write as _;
                            write!(&mut encoded, "{byte:02x}").unwrap();
                            encoded
                        });
                let artifact = tree
                    .join("store/recovery")
                    .join(connection_name)
                    .join(format!("{}-0", id.serial));
                assert_eq!(
                    std::fs::read(artifact).unwrap(),
                    if restart == 9 {
                        b"new".as_slice()
                    } else {
                        b"new bytes"
                    }
                );
            }
            assert_eq!(
                reopened
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&RelativePath::unix(b"file").unwrap()),
                Some(&crate::connection::model::EntryState::File {
                    sha256: Sha256::digest(b"new bytes").into(),
                    bytes: 9,
                    executable: false
                })
            );
            reopened.finish_disconnect(&fence).unwrap();
            reopened.checkpoint().unwrap();
            drop(reopened);
            let reopened = ConnectionStore::open_at(&tree.join("store")).unwrap();
            assert_eq!(
                reopened
                    .pending_page(id.connection, None, 8)
                    .unwrap()
                    .entries[0]
                    .stage,
                crate::connection::store::ReceiptStage::AccountedPending
            );
            assert_eq!(
                reopened.load_connection(id.connection).unwrap().lifecycle,
                crate::connection::store::Lifecycle::Disconnected
            );
            assert_eq!(
                reopened
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .len(),
                1
            );
            return;
        }
        let session = reopened.transaction_session(current.id).unwrap();
        let mut live = reopened
            .resume_native_deletion(&context, &mut recovered, &session, &cancel)
            .unwrap();
        let mut outcome = recovered
            .complete_deletion(&mut reopened, &mut live, &session, &cancel)
            .unwrap();
        if !matches!(restart, 3..=5) {
            reopened
                .record_native_deletion(&mut live, &mut outcome, false, &cancel)
                .unwrap();
        }
        if !matches!(restart, 4 | 5) {
            reopened
                .record_native_deletion(&mut live, &mut outcome, true, &cancel)
                .unwrap();
        }
        assert_eq!(
            reopened
                .load_baseline(current.id)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            1
        );
        let mut comparison = outcome.comparison(&reopened, &live, &cancel).unwrap();
        let candidate = reopened
            .prepare_baseline(&live, &mut comparison, &cancel)
            .unwrap();
        reopened
            .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
            .unwrap();
        assert!(
            reopened
                .load_baseline(current.id)
                .unwrap()
                .entries()
                .unwrap()
                .is_empty()
        );
        assert!(!source_path.join("file").exists());
        assert!(!target_path.join("file").exists());
        assert_eq!(
            std::fs::read(target_path.join(&deletion_name).join("displaced")).unwrap(),
            b"new bytes"
        );
        let connection_name: String =
            current
                .id
                .0
                .iter()
                .fold(String::new(), |mut encoded, byte| {
                    use std::fmt::Write as _;
                    write!(&mut encoded, "{byte:02x}").unwrap();
                    encoded
                });
        let recovery_directory = tree.join("store/recovery").join(connection_name);
        assert_eq!(
            std::fs::read(recovery_directory.join(format!(
                "{}-{}",
                live.id().serial,
                u8::from(restart == 9)
            )))
            .unwrap(),
            b"new bytes"
        );
        if restart == 9 {
            assert_eq!(
                std::fs::read(recovery_directory.join(format!("{}-0", live.id().serial))).unwrap(),
                b"new"
            );
            assert_eq!(
                reopened
                    .recovery_page(current.id, None, 128)
                    .unwrap()
                    .entries
                    .iter()
                    .find(|entry| entry.id.operation == live.id() && entry.id.slot == 0)
                    .unwrap()
                    .state,
                crate::connection::store::RecoveryState::RetainedIncomplete
            );
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and helper required; execution UNRUN"]
    fn native_conflict_retains_competitor_stage_and_actual_quarantine_versions() {
        for reverse in [false, true] {
            for changed in [false, true] {
                for restart in [false, true] {
                    connected_preservation_case(PreservationCase::CapturedQuarantine {
                        reverse,
                        changed,
                        restart,
                    });
                }
            }
        }
    }
    #[derive(Clone, Copy)]
    enum PreservationCase {
        CapturedQuarantine {
            reverse: bool,
            changed: bool,
            restart: bool,
        },
        Success,
        DisconnectAfterBytes,
        MissingSource,
        OwnershipRace,
        Publish,
    }
    #[expect(
        clippy::too_many_lines,
        reason = "The real-tree fixture retains all guards while testing byte preservation, refusal and observer substitution."
    )]
    fn connected_preservation_case(case: PreservationCase) {
        let reverse = matches!(
            case,
            PreservationCase::CapturedQuarantine { reverse: true, .. }
        );
        let stop_after_bytes = matches!(case, PreservationCase::DisconnectAfterBytes);
        use crate::connection::transaction::StepKind;
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{
                ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent, RecoveryReason,
            },
        };
        use std::sync::atomic::AtomicBool;
        let tree = fixture();
        let source_path = tree.path().join("source-tree");
        let target_path = tree.path().join("target-tree");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        std::fs::write(source_path.join("file"), b"proposal").unwrap();
        std::fs::write(target_path.join("file"), b"original").unwrap();
        let source_endpoint = endpoint(&source_path);
        let target_endpoint = endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([51; 16]),
                if reverse {
                    binding(&target_path)
                } else {
                    binding(&source_path)
                },
                if reverse {
                    binding(&source_path)
                } else {
                    binding(&target_path)
                },
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let session = store.transaction_session(record.id).unwrap();
        let content = |bytes: &[u8]| {
            Some(EntryState::File {
                sha256: Sha256::digest(bytes).into(),
                bytes: bytes.len() as u64,
                executable: false,
            })
        };
        let operation = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"file").unwrap(),
            source_before: content(if reverse { b"original" } else { b"proposal" }),
            destination_before: content(if reverse { b"proposal" } else { b"original" }),
            decision: Decision::Propagate {
                from: if reverse {
                    ModelSide::Destination
                } else {
                    ModelSide::Source
                },
            },
        };
        let cancel = AtomicBool::new(false);
        let (left, right) = if reverse {
            (&target_endpoint, &source_endpoint)
        } else {
            (&source_endpoint, &target_endpoint)
        };
        let reviewed = initial_preview(&store, record.id, left, right, &cancel)
            .unwrap()
            .accept(operation.clone())
            .unwrap();
        let admission = admission_metadata(&store, &operation, left, right, &cancel).unwrap();
        let mut live = store
            .begin_native_reviewed(operation, &session, reviewed, admission)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                16 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let reservation = store
            .reserve_native_namespace(
                &mut live,
                if reverse {
                    ModelSide::Source
                } else {
                    ModelSide::Destination
                },
            )
            .unwrap();
        let private_name = reservation.name();
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        let source = epoch.pin_parent(Side::Source, &live.intent().path).unwrap();
        let target = epoch.pin_parent(Side::Target, &live.intent().path).unwrap();
        let source_guard = epoch.guard_regular(&source).unwrap();
        let target_guard = epoch.guard_regular(&target).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .unwrap();
        let private = epoch
            .create_registered_namespace(
                &live,
                &reservation,
                &source,
                &target,
                &source_guard,
                Some(&target_guard),
                step,
            )
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Stage, &cancel)
            .unwrap();
        let staged = epoch
            .stage_connected(&live, &source_guard, &private, step)
            .unwrap();
        let prepared = epoch.prepared(&private, Some(&staged)).unwrap();
        let prepared = store
            .prepare_native(&mut live, reservation, prepared)
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Relocate, &cancel)
            .unwrap();
        if matches!(case, PreservationCase::MissingSource) {
            std::fs::rename(
                source_path.join("file"),
                source_path.join("retained-source"),
            )
            .unwrap();
        }
        let result = epoch.relocate_connected(
            &live,
            &target,
            &private,
            &source_guard,
            &target_guard,
            &prepared,
            step,
        );
        if matches!(case, PreservationCase::MissingSource) {
            assert!(result.is_err());
            assert_eq!(
                std::fs::read(target_path.join("file")).unwrap(),
                b"original"
            );
            assert_eq!(
                std::fs::read(source_path.join("retained-source")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
                b"proposal"
            );
            assert!(!target_path.join(&private_name).join("displaced").exists());
            assert!(
                store
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            return;
        }
        let displaced = result.unwrap();
        let displaced_guard = epoch.guard_displaced(&displaced).unwrap();
        if let PreservationCase::CapturedQuarantine {
            changed, restart, ..
        } = case
        {
            drop(displaced_guard);
            drop(displaced);
            drop(staged);
            drop(source_guard);
            drop(target_guard);
            drop(epoch);
            std::fs::write(target_path.join("file"), b"competitor").unwrap();
            let quarantine: &[u8] = if changed {
                b"changed quarantine"
            } else {
                b"original"
            };
            if changed {
                std::fs::write(
                    target_path.join(&private_name).join("displaced"),
                    quarantine,
                )
                .unwrap();
            }
            let mut capture =
                NativeEpoch::capture_conflict(&store, &live, left, right, &cancel).unwrap();
            assert_eq!(capture.snapshot().versions.len(), 4);
            store
                .record_native_capture(&mut live, &mut capture, &cancel)
                .unwrap();
            if restart {
                let id = live.id();
                store.checkpoint().unwrap();
                drop(capture);
                drop(live);
                drop(session);
                drop(store);
                store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
                let current = store.load_connection(id.connection).unwrap();
                let mut recovery_context =
                    store.open_recovery_context(id, current.revision).unwrap();
                store
                    .reserve_recovery_reads(
                        &mut recovery_context,
                        32 * 1024 * 1024,
                        Instant::now() + std::time::Duration::from_secs(30),
                        &cancel,
                    )
                    .unwrap();
                capture =
                    NativeEpoch::recover_conflict(&store, &recovery_context, left, right, &cancel)
                        .unwrap();
                let session = store.transaction_session(id.connection).unwrap();
                live = store
                    .resume_native_conflict(&recovery_context, &mut capture, &session, &cancel)
                    .unwrap();
            }
            store
                .preserve_native_conflict(&mut live, &mut capture, &cancel)
                .unwrap();
            store
                .record_native_conflict(&mut live, &mut capture, &cancel)
                .unwrap();
            let entries = store
                .recovery_page(live.id().connection, None, 8)
                .unwrap()
                .entries;
            assert_eq!(entries.len(), 4);
            let mut actual = entries
                .iter()
                .map(|entry| {
                    std::fs::read(
                        tree.path()
                            .join("store/recovery")
                            .join("33".repeat(16))
                            .join(format!("{}-{}", entry.id.operation.serial, entry.id.slot)),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            actual.sort();
            let mut expected = vec![
                b"competitor".to_vec(),
                quarantine.to_vec(),
                b"proposal".to_vec(),
                b"proposal".to_vec(),
            ];
            expected.sort();
            assert_eq!(actual, expected);
            assert_eq!(
                std::fs::read(source_path.join("file")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(target_path.join("file")).unwrap(),
                b"competitor"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
                quarantine
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
                b"proposal"
            );
            assert!(
                store
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            store.checkpoint().unwrap();
            drop(capture);
            drop(store);
            let store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            assert_eq!(
                store
                    .recovery_page(ConnectionId([51; 16]), None, 8)
                    .unwrap()
                    .entries
                    .len(),
                4
            );
            return;
        }
        let slot = store
            .reserve_native_recovery(
                &mut live,
                ModelSide::Destination,
                RecoveryReason::DisplacedVersion,
            )
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Preserve, &cancel)
            .unwrap();
        let mut reader = epoch
            .recovery_source(&live, &private, &displaced, &displaced_guard, step)
            .unwrap();
        if stop_after_bytes {
            crate::connection::store::after_native_bytes_for_test(move || {
                session.request_disconnect();
            });
        }
        let result = store.preserve_native(&mut live, slot, &mut reader, &cancel);
        drop(reader);
        if stop_after_bytes {
            assert!(result.is_err());
            let versions = store
                .recovery_page(live.id().connection, None, 128)
                .unwrap();
            assert_eq!(versions.entries.len(), 1);
            assert_eq!(
                versions.entries[0].state,
                crate::connection::store::RecoveryState::BytesVerified
            );
            assert_eq!(
                std::fs::read(
                    tree.path()
                        .join("store/recovery")
                        .join("33".repeat(16))
                        .join("1-0")
                )
                .unwrap(),
                b"original"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
                b"original"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
                b"proposal"
            );
            assert!(!target_path.join("file").exists());
            assert!(
                store
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            return;
        }
        let bound = result.unwrap();
        let sealed = epoch
            .seal_displacement(&live, displaced, displaced_guard, &private, bound)
            .unwrap();
        if matches!(case, PreservationCase::Publish) {
            let session = store.transaction_session(live.id().connection).unwrap();
            epoch.check_absent(&target).unwrap();
            let step = session
                .admit_step(&store, &live, StepKind::Publish, &cancel)
                .unwrap();
            let mut outcome = epoch
                .publish_connected(
                    &live,
                    &source_guard,
                    &target,
                    &private,
                    staged,
                    None,
                    Some(sealed),
                    step,
                )
                .unwrap();
            store
                .record_native_outcome(&mut live, &mut outcome, false, &cancel)
                .unwrap();
            store
                .record_native_outcome(&mut live, &mut outcome, true, &cancel)
                .unwrap();
            assert_eq!(
                std::fs::read(target_path.join("file")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
                b"original"
            );
            assert!(!target_path.join(&private_name).join("stage").exists());
            assert!(target_path.join(&private_name).join("outcome").is_file());
            assert!(
                store
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            let mut comparison = outcome.comparison(&store, &live, &cancel).unwrap();
            let candidate = store
                .prepare_baseline(&live, &mut comparison, &cancel)
                .unwrap();
            assert!(
                store
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            store
                .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
                .unwrap();
            drop(comparison);
            drop(outcome);
            drop(store);
            let reopened = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
            let pending = reopened
                .pending_page(live.id().connection, None, 8)
                .unwrap();
            assert!(pending.entries.is_empty());
            assert_eq!(
                reopened
                    .load_baseline(live.id().connection)
                    .unwrap()
                    .entries()
                    .unwrap()
                    .get(&live.intent().path),
                Some(&EntryState::File {
                    sha256: Sha256::digest(b"proposal").into(),
                    bytes: 8,
                    executable: false
                })
            );
            return;
        }
        assert!(!target_path.join("file").exists());
        assert_eq!(
            std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            std::fs::read(source_path.join("file")).unwrap(),
            b"proposal"
        );
        assert!(
            store
                .load_baseline(live.id().connection)
                .unwrap()
                .entries()
                .is_none()
        );
        let names = epoch
            .owned_names(
                &live,
                Side::Target,
                store.native_namespace_records(&live).unwrap(),
            )
            .unwrap();
        let clean = endpoint::observe_connected(&target_endpoint, &names, &cancel);
        assert!(matches!(
            clean.observation(),
            crate::connection::model::Observation::Complete
        ));
        if matches!(case, PreservationCase::OwnershipRace) {
            let unknown = target_path.join(&private_name).join("late-user-data");
            let hook_path = unknown.clone();
            // First validate_all, exact-name exclusion, then final validate_all.
            // Mutate only after the final enumeration and named-object reads.
            AFTER_OWNED_READS.with(|hook| {
                *hook.borrow_mut() = Some((
                    2,
                    Box::new(move || {
                        std::fs::write(hook_path, b"late retained bytes").unwrap();
                    }),
                ));
            });
            let raced = endpoint::observe_connected(&target_endpoint, &names, &cancel);
            assert!(AFTER_OWNED_READS.with(|hook| hook.borrow().is_none()));
            assert!(matches!(
                raced.observation(),
                crate::connection::model::Observation::Incomplete(_)
            ));
            assert_eq!(std::fs::read(&unknown).unwrap(), b"late retained bytes");
            let visible = endpoint::observe_connected(&target_endpoint, &names, &cancel);
            assert!(
                visible
                    .entries()
                    .contains_key(&RelativePath::unix(private_name.as_bytes()).unwrap())
            );
            assert!(matches!(
                visible.observation(),
                crate::connection::model::Observation::Incomplete(_)
            ));
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("stage")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(target_path.join(&private_name).join("displaced")).unwrap(),
                b"original"
            );
            return;
        }
        std::fs::write(target_path.join(".qos-operation-user-file"), b"user bytes").unwrap();
        let ordinary = endpoint::observe(&target_endpoint, &cancel);
        let connected = endpoint::observe_connected(&target_endpoint, &names, &cancel);
        let private_key = RelativePath::unix(private_name.as_bytes()).unwrap();
        assert!(ordinary.entries().contains_key(&private_key));
        assert!(!connected.entries().contains_key(&private_key));
        assert!(
            connected
                .entries()
                .contains_key(&RelativePath::unix(b".qos-operation-user-file").unwrap())
        );
        assert!(matches!(
            connected.observation(),
            crate::connection::model::Observation::Incomplete(_)
        ));
        std::fs::rename(
            target_path.join(&private_name),
            target_path.join("retained-private"),
        )
        .unwrap();
        std::fs::create_dir(target_path.join(&private_name)).unwrap();
        std::fs::write(
            target_path.join(&private_name).join("user-data"),
            b"untouched",
        )
        .unwrap();
        let changed = endpoint::observe_connected(&target_endpoint, &names, &cancel);
        assert!(matches!(
            changed.observation(),
            crate::connection::model::Observation::Incomplete(_)
        ));
        assert!(changed.entries().contains_key(&private_key));
        assert_eq!(
            std::fs::read(target_path.join(&private_name).join("user-data")).unwrap(),
            b"untouched"
        );
    }
    fn binding() -> OperationBinding {
        OperationBinding {
            _connection: ConnectionId([1; 16]),
            _operation: [2; 16],
            _store_session: [3; 16],
            _revision: 1,
            _fence: 0,
        }
    }
    thread_local! { static BEFORE_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::connection::transaction) fn run_before_relocate() {
        BEFORE_RELOCATE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    thread_local! { static BEFORE_PUBLICATION_REREAD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::connection::transaction) fn run_before_publication_reread() {
        BEFORE_PUBLICATION_REREAD.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_final_publication_rejects_substituted_destination_ancestry() {
        let tree = fixture();
        // The source is deliberately outside the ancestry being substituted.
        std::fs::write(tree.path().join("source"), b"proposal").unwrap();
        std::fs::create_dir(tree.path().join("destination-parent")).unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        let target = epoch
            .pin_parent(
                Side::Target,
                &RelativePath::unix(b"destination-parent/file").unwrap(),
            )
            .unwrap();
        let absent = epoch.observe_absent(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let stage = epoch.stage_from_guard(&guard, &private).unwrap();
        let expected = stage.guard.actual.clone();
        let raced_root = tree.path().to_owned();
        BEFORE_PUBLICATION_REREAD.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(
                    raced_root.join("destination-parent"),
                    raced_root.join("saved-parent"),
                )
                .unwrap();
                std::fs::create_dir(raced_root.join("destination-parent")).unwrap();
                std::fs::write(raced_root.join("destination-parent/file"), b"raced").unwrap();
            }));
        });
        let result = epoch.publish_absent(stage, &target, absent, &guard);
        assert!(matches!(
            result,
            Err(Failure {
                _cause: EndpointError::IdentityChanged,
                _boundary: Boundary::Uncertain,
                ..
            })
        ));
        assert_eq!(epoch.boundary, Boundary::Uncertain);
        assert!(epoch.check().is_err());
        assert_eq!(
            std::fs::read(tree.path().join("source")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            std::fs::read(tree.path().join("saved-parent/file")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            object(&File::open(tree.path().join("saved-parent/file")).unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read(tree.path().join("destination-parent/file")).unwrap(),
            b"raced"
        );
    }
    thread_local! { static FAIL_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    pub(in crate::connection::transaction) fn take_barrier_failure() -> bool {
        FAIL_BARRIER.with(|value| value.replace(false))
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_publication_is_actual_and_barrier_failure_preserves_every_name() {
        for fail in [false, true] {
            let tree = fixture();
            std::fs::write(tree.path().join("source"), b"proposal").unwrap();
            let endpoint = endpoint(tree.path());
            let mut epoch = NativeEpoch::begin(
                &endpoint,
                &endpoint,
                binding(),
                super::super::OperationReads::funded_for_test(),
            )
            .unwrap();
            let source = epoch
                .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
                .unwrap();
            let guard = epoch.guard_regular(&source).unwrap();
            let dest = epoch
                .pin_parent(Side::Target, &RelativePath::unix(b"destination").unwrap())
                .unwrap();
            let absent = epoch.observe_absent(&dest).unwrap();
            let private = epoch.create_namespace(&dest).unwrap();
            let stage = epoch.stage_from_guard(&guard, &private).unwrap();
            let expected = stage.guard.actual.clone();
            FAIL_BARRIER.with(|value| value.set(fail));
            let published = epoch.publish_absent(stage, &dest, absent, &guard);
            if fail {
                assert!(matches!(
                    published,
                    Err(Failure {
                        _boundary: Boundary::Uncertain,
                        ..
                    })
                ));
                assert!(epoch.check().is_err());
            } else {
                let published = published.unwrap();
                assert_eq!(published._identity, expected);
                assert_eq!(
                    published._bytes.digest,
                    <[u8; 32]>::from(Sha256::digest(b"proposal"))
                );
            }
            assert_eq!(
                std::fs::read(tree.path().join("source")).unwrap(),
                b"proposal"
            );
            assert_eq!(
                std::fs::read(tree.path().join("destination")).unwrap(),
                b"proposal"
            );
        }
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_raced_writable_inode_is_preserved_as_actual_quarantine() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        std::fs::write(tree.path().join("racer"), b"new").unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(tree.path().join("racer"))
            .unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let target = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let expected_actual = object(&writer).unwrap();
        let path = tree.path().to_owned();
        BEFORE_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(path.join("live"), path.join("old")).unwrap();
                std::fs::rename(path.join("racer"), path.join("live")).unwrap();
            }));
        });
        let displaced = epoch.relocate_noreplace(&target, &private, &guard).unwrap();
        assert!(!displaced.matches_guard);
        assert_eq!(displaced.actual, expected_actual);
        writer.write_all_at(b"changed", 0).unwrap();
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private._name.to_str().unwrap())
                    .join("displaced")
            )
            .unwrap(),
            b"changed"
        );
        assert_eq!(std::fs::read(tree.path().join("old")).unwrap(), b"old");
        assert!(epoch.guard_displaced(&displaced).is_err());
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_source_disappearance_and_missing_parent_do_not_publish() {
        let tree = fixture();
        std::fs::write(tree.path().join("source"), b"source").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        std::fs::rename(tree.path().join("source"), tree.path().join("saved")).unwrap();
        assert!(epoch.capture(&guard).is_err());
        drop(epoch);
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        assert!(
            epoch
                .pin_parent(Side::Target, &RelativePath::unix(b"missing/child").unwrap())
                .is_err()
        );
        assert!(!tree.path().join("missing").exists());
        assert_eq!(std::fs::read(tree.path().join("saved")).unwrap(), b"source");
    }
    thread_local! { static AFTER_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::connection::transaction) fn run_after_relocate() {
        AFTER_RELOCATE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_revocation_during_namespace_drain_keeps_accounted_quarantine() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let target = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let captured = tree
            .path()
            .join(private._name.to_str().unwrap())
            .join("displaced");
        let writer_path = captured.clone();
        AFTER_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                use std::os::unix::fs::OpenOptionsExt;
                assert!(
                    std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(writer_path)
                        .is_err()
                );
            }));
        });
        let displaced = epoch.relocate_noreplace(&target, &private, &guard).unwrap();
        assert!(displaced.matches_guard);
        assert_eq!(epoch.boundary, Boundary::DurableQuarantine);
        assert!(epoch.check().is_err());
        assert_eq!(std::fs::read(captured).unwrap(), b"old");
        assert!(!tree.path().join("live").exists());
    }
    pub(super) fn fixture() -> tempfile::TempDir {
        tempfile::tempdir_in(
            std::env::var_os("QOS_ENDPOINT_FIXTURE").expect("authorized disposable native volume"),
        )
        .unwrap()
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_exclusive_stage_and_absent_publication_preserve_raced_live() {
        let tree = fixture();
        std::fs::write(tree.path().join("source"), b"proposal").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        let dest = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"destination").unwrap())
            .unwrap();
        let absent = epoch.observe_absent(&dest).unwrap();
        let private = epoch.create_namespace(&dest).unwrap();
        let stage = epoch.stage_from_guard(&guard, &private).unwrap();
        assert_eq!(
            open_at(
                private.directory.file(),
                c"stage",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        std::fs::write(tree.path().join("destination"), b"raced").unwrap();
        assert!(epoch.publish_absent(stage, &dest, absent, &guard).is_err());
        assert_eq!(
            std::fs::read(tree.path().join("destination")).unwrap(),
            b"raced"
        );
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private._name.to_str().unwrap())
                    .join("stage")
            )
            .unwrap(),
            b"proposal"
        );
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_actual_displacement_keeps_unexpected_directory_and_old_bytes() {
        let tree = fixture();
        std::fs::write(tree.path().join("live"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let parent = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&parent).unwrap();
        let private = epoch.create_namespace(&parent).unwrap();
        let raced_root = tree.path().to_owned();
        BEFORE_RELOCATE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(raced_root.join("live"), raced_root.join("original")).unwrap();
                std::fs::create_dir(raced_root.join("live")).unwrap();
                std::fs::write(raced_root.join("live/child"), b"raced tree").unwrap();
            }));
        });
        let moved = epoch.relocate_noreplace(&parent, &private, &guard).unwrap();
        assert!(!moved.matches_guard);
        assert_eq!(moved.actual.kind, libc::S_IFDIR);
        assert_eq!(
            std::fs::read(
                tree.path()
                    .join(private._name.to_str().unwrap())
                    .join("displaced/child")
            )
            .unwrap(),
            b"raced tree"
        );
        assert_eq!(std::fs::read(tree.path().join("original")).unwrap(), b"old");
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_missing_source_parent_substitution_and_cancel_refuse_publication() {
        let tree = fixture();
        std::fs::create_dir(tree.path().join("parent")).unwrap();
        std::fs::write(tree.path().join("parent/source"), b"old").unwrap();
        let endpoint = endpoint(tree.path());
        let mut epoch = NativeEpoch::begin(
            &endpoint,
            &endpoint,
            binding(),
            super::super::OperationReads::funded_for_test(),
        )
        .unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"parent/source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        std::fs::rename(tree.path().join("parent"), tree.path().join("moved")).unwrap();
        std::fs::create_dir(tree.path().join("parent")).unwrap();
        assert!(epoch.capture(&guard).is_err());
        epoch.cancel();
        assert!(epoch.check().is_err());
        assert_eq!(
            std::fs::read(tree.path().join("moved/source")).unwrap(),
            b"old"
        );
    }
}

/// Context is data, never mutation authority. All evidence below additionally
/// holds a fresh unpredictable epoch and actual retained native identities.
struct OperationBinding {
    _connection: ConnectionId,
    _operation: [u8; 16],
    _store_session: [u8; 16],
    _revision: u64,
    _fence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::connection::transaction) enum Boundary {
    Untouched,
    PrivateStaged,
    DurableQuarantine,
    Published,
    Uncertain,
}
#[derive(Debug)]
pub(in crate::connection::transaction) struct Failure {
    _cause: EndpointError,
    _boundary: Boundary,
    _os_error: Option<i32>,
}
type Result<T> = std::result::Result<T, Failure>;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::connection::transaction) enum Side {
    Source,
    Target,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ObjectId {
    dev: u64,
    ino: u64,
    kind: u32,
    generation: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
}
impl Stamp {
    fn new(meta: &Metadata) -> Self {
        Self {
            length: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            links: meta.nlink(),
        }
    }
    fn relocated_eq(&self, other: &Self) -> bool {
        self.length == other.length
            && self.modified == other.modified
            && self.mode == other.mode
            && self.uid == other.uid
            && self.gid == other.gid
            && self.links == other.links
    }
}
pub(in crate::connection::transaction) struct ParentGuard {
    directory: NativeDirectory,
    name: CString,
    epoch: [u8; 16],
    side: Side,
    chain: Vec<ObjectId>,
    path: Option<RelativePath>,
}
pub(in crate::connection::transaction) struct StableFileGuard {
    parent: ParentGuard,
    file: Arc<File>,
    actual: ObjectId,
    stamp: Stamp,
    executable: bool,
}
pub(in crate::connection::transaction) struct PrivateNamespace {
    directory: NativeDirectory,
    _name: CString,
    epoch: [u8; 16],
    identity: ObjectId,
    intended_parent: ObjectId,
    intended_name: CString,
    descriptor: Option<(d::Header, d::Intent, Vec<u8>)>,
}
pub(in crate::connection::transaction) struct AbsentName {
    epoch: [u8; 16],
    parent: ObjectId,
    name: CString,
}
enum PublicationVacancy {
    Original(AbsentName),
    Sealed {
        epoch: [u8; 16],
        parent: ObjectId,
        name: CString,
    },
}
pub(in crate::connection::transaction) struct Capture {
    digest: [u8; 32],
    length: u64,
    executable: bool,
}
struct SourceBinding {
    file: Arc<File>,
    stamp: Stamp,
    executable: bool,
    parent: NativeDirectory,
    name: CString,
    actual: ObjectId,
    side: Side,
}
pub(in crate::connection::transaction) struct StagedFile {
    source: SourceBinding,
    guard: StableFileGuard,
    bytes: Capture,
    namespace: NativeDirectory,
    intended_parent: ObjectId,
    intended_name: CString,
    descriptor: Option<Vec<u8>>,
}
pub(in crate::connection::transaction) struct SelectedStage {
    guard: StableFileGuard,
    bytes: Capture,
    descriptor: Vec<u8>,
}
pub(in crate::connection::transaction) struct RelocatedObject {
    epoch: [u8; 16],
    actual: ObjectId,
    retained: File,
    namespace: NativeDirectory,
    original_parent: NativeDirectory,
    _original_name: CString,
    matches_guard: bool,
    descriptor: Option<Vec<u8>>,
}
pub(in crate::connection::transaction) struct NativePublication {
    _source: SourceBinding,
    epoch: [u8; 16],
    _identity: ObjectId,
    _bytes: Capture,
    _parent: NativeDirectory,
    _name: CString,
    guard: StableFileGuard,
    stage_descriptor: Option<Vec<u8>>,
}
#[expect(
    clippy::struct_excessive_bools,
    reason = "These are independent observed namespace-presence or epoch hazard facts, not mutually exclusive states."
)]
pub(in crate::connection::transaction) struct NativeEpoch<'a> {
    witness: Option<witness::NamespaceWitness>,
    directory_requests: Option<Arc<std::sync::atomic::AtomicU16>>,
    directory_ancestors: Option<directory::DirectoryAncestors>,
    ancestor_chains: u16,
    ancestors: Option<super::ExistingAncestorProof>,
    cancel_read: Option<&'a std::sync::atomic::AtomicBool>,
    stop_read: Option<Arc<super::super::AdmissionGate>>,
    source: &'a PinnedEndpoint,
    target: &'a PinnedEndpoint,
    source_root: NativeDirectory,
    target_root: NativeDirectory,
    identities: [EndpointIdentity; 2],
    _binding: Option<OperationBinding>,
    nonce: [u8; 16],
    leases: Receiver,
    deadline: Instant,
    read_budget: Arc<super::ReadBudget>,
    lifetime: Arc<()>,
    boundary: Boundary,
    poisoned: bool,
    relocated: bool,
    namespace_created: bool,
    deletion_scope: bool,
    connected: Option<ConnectedBinding>,
}
pub(super) fn random_id() -> io::Result<[u8; 16]> {
    let mut bytes = [0; 16];
    // SAFETY: correctly sized writable buffer; kernel entropy, no path fallback.
    let result = unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if result != bytes.len() as isize || bytes == [0; 16] {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes)
}
fn object(file: &File) -> std::result::Result<ObjectId, EndpointError> {
    let metadata = file.metadata()?;
    Ok(ObjectId {
        dev: metadata.dev(),
        ino: metadata.ino(),
        kind: metadata.mode() & libc::S_IFMT,
        generation: Sha256::digest(endpoint::native_object_generation(file)?).into(),
    })
}
fn open_at(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
    // SAFETY: live parent, bounded NUL-terminated single component and fixed mode.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returns a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
#[derive(Clone, Copy)]
enum CreatedMode {
    Private,
    Executable,
}
impl CreatedMode {
    fn from_executable(executable: bool) -> Self {
        if executable {
            Self::Executable
        } else {
            Self::Private
        }
    }
    fn bits(self) -> libc::mode_t {
        match self {
            Self::Private => 0o600,
            Self::Executable => 0o700,
        }
    }
}
fn create_regular_at(parent: &File, name: &CStr, mode: CreatedMode) -> io::Result<File> {
    // SAFETY: retained parent, single component, fixed exclusive flags and a closed mode set.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
            mode.bits(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: exclusive successful open returned a newly owned writable description.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn rename_noreplace(from: &File, name: &CStr, to: &File, slot: &CStr) -> io::Result<()> {
    // SAFETY: retained same-mount directory fds and validated single components.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.as_raw_fd(),
            name.as_ptr(),
            to.as_raw_fd(),
            slot.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn sync_directory(directory: &File) -> io::Result<()> {
    open_at(directory, c".", libc::O_RDONLY | libc::O_DIRECTORY)?.sync_all()
}
impl<'a> NativeEpoch<'a> {
    fn begin(
        source: &'a PinnedEndpoint,
        target: &'a PinnedEndpoint,
        binding: OperationBinding,
        reads: Arc<super::OperationReads>,
    ) -> Result<Self> {
        Self::begin_bound(source, target, Some(binding), reads)
    }
    fn begin_bound(
        source: &'a PinnedEndpoint,
        target: &'a PinnedEndpoint,
        binding: Option<OperationBinding>,
        reads: Arc<super::OperationReads>,
    ) -> Result<Self> {
        let fail = |cause| Failure {
            _cause: cause,
            _boundary: Boundary::Untouched,
            _os_error: None,
        };
        let source_root = source.native_directory(false).map_err(fail)?;
        let target_root = target.native_directory(true).map_err(fail)?;
        let nonce = random_id().map_err(|e| fail(e.into()))?;
        let leases = Receiver::start(nonce).map_err(|e| {
            let os_error = e.raw_os_error();
            let cause = if e.kind() == io::ErrorKind::WouldBlock {
                EndpointError::Unverified
            } else {
                e.into()
            };
            Failure {
                _cause: cause,
                _boundary: Boundary::Untouched,
                _os_error: os_error,
            }
        })?;
        Ok(Self {
            witness: None,
            directory_requests: None,
            directory_ancestors: None,
            cancel_read: None,
            stop_read: None,
            source,
            target,
            source_root,
            target_root,
            identities: [source.identity(), target.identity()],
            _binding: binding,
            nonce,
            leases,
            deadline: Instant::now() + lease_process::WORK,
            read_budget: Arc::new(super::ReadBudget {
                reads,
                deadline: Instant::now() + lease_process::WORK,
            }),
            lifetime: Arc::new(()),
            boundary: Boundary::Untouched,
            poisoned: false,
            relocated: false,
            namespace_created: false,
            deletion_scope: false,
            connected: None,
            ancestors: None,
            ancestor_chains: 0,
        })
    }
    fn failure(&mut self, cause: EndpointError) -> Failure {
        self.poisoned = true;
        self.leases.revoke();
        Failure {
            _cause: cause,
            _boundary: self.boundary,
            _os_error: None,
        }
    }
    fn ep<T>(&mut self, result: std::result::Result<T, EndpointError>) -> Result<T> {
        result.map_err(|e| self.failure(e))
    }
    fn io<T>(&mut self, result: io::Result<T>) -> Result<T> {
        result.map_err(|e| {
            let os_error = e.raw_os_error();
            let cause = if e.kind() == io::ErrorKind::AlreadyExists {
                EndpointError::Unstable
            } else if e.kind() == io::ErrorKind::WouldBlock {
                EndpointError::Unverified
            } else {
                e.into()
            };
            let mut failure = self.failure(cause);
            failure._os_error = os_error;
            failure
        })
    }
    pub(in crate::connection::transaction) fn cancel(&mut self) {
        self.poisoned = true;
        self.leases.revoke();
    }
    pub(in crate::connection::transaction) fn check(&mut self) -> Result<()> {
        if self
            .directory_requests
            .as_ref()
            .is_some_and(|count| count.load(std::sync::atomic::Ordering::Acquire) > 128)
        {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        if self.poisoned
            || Instant::now() >= self.deadline
            || self
                .cancel_read
                .is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Acquire))
            || self
                .stop_read
                .as_ref()
                .is_some_and(|gate| gate.stopped().load(std::sync::atomic::Ordering::Acquire))
        {
            return Err(self.failure(EndpointError::Cancelled));
        }
        let result = self
            .source
            .revalidate()
            .and_then(|()| self.target.revalidate_for_write());
        self.ep(result)?;
        let result = self.leases.check();
        self.io(result)
    }
    fn parent_check(&mut self, parent: &ParentGuard) -> Result<()> {
        if parent.epoch != self.nonce {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.ep(parent.directory.validate(parent.side == Side::Target))
    }
    fn namespace_check(&mut self, private: &PrivateNamespace) -> Result<()> {
        if private.epoch != self.nonce {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.ep(private.directory.validate(true))?;
        self.io(destination_security::check_private_file(
            private.directory.file(),
        ))?;
        if self.ep(object(private.directory.file()))? != private.identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        Ok(())
    }
    pub(in crate::connection::transaction) fn pin_parent(
        &mut self,
        side: Side,
        path: &RelativePath,
    ) -> Result<ParentGuard> {
        self.check()?;
        if let Some(proof) = &self.directory_ancestors {
            if &proof.admission.path == path {
                return proof.parent(side);
            }
        }
        if let Some(proof) = &self.ancestors {
            if &proof.admission.path == path {
                let original = proof
                    .parents
                    .get(usize::from(side == Side::Target))
                    .ok_or_else(|| Failure {
                        _cause: EndpointError::Unverified,
                        _boundary: self.boundary,
                        _os_error: None,
                    })?;
                return Ok(ParentGuard {
                    directory: original.directory.clone(),
                    name: original.name.clone(),
                    epoch: original.epoch,
                    side,
                    chain: original.chain.clone(),
                    path: original.path.clone(),
                });
            }
        }
        self.pin_parent_sample(side, path).map(|(guard, _)| guard)
    }
    fn pin_parent_sample(
        &mut self,
        side: Side,
        path: &RelativePath,
    ) -> Result<(ParentGuard, Vec<Stamp>)> {
        self.check()?;
        let Some((last, ancestors)) = path.components().split_last() else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let mut directory = if side == Side::Source {
            self.source_root.clone()
        } else {
            self.target_root.clone()
        };
        let mut stamps = vec![Stamp::new(&self.io(directory.file().metadata())?)];
        let mut chain = vec![self.ep(object(directory.file()))?];
        for name in ancestors {
            let NativeName::Unix(bytes) = name else {
                return Err(self.failure(EndpointError::Unsupported));
            };
            directory = self.ep(directory.child(OsStr::from_bytes(bytes), side == Side::Target))?;
            chain.push(self.ep(object(directory.file()))?);
            stamps.push(Stamp::new(&self.io(directory.file().metadata())?));
        }
        let NativeName::Unix(bytes) = last else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let name =
            CString::new(bytes.as_slice()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        self.check()?;
        Ok((
            ParentGuard {
                directory,
                name,
                side,
                epoch: self.nonce,
                chain,
                path: Some(path.clone()),
            },
            stamps,
        ))
    }
    pub(in crate::connection::transaction) fn guard_regular(
        &mut self,
        parent: &ParentGuard,
    ) -> Result<StableFileGuard> {
        self.check()?;
        self.parent_check(parent)?;
        let before = self.io(open_at(parent.directory.file(), &parent.name, libc::O_PATH))?;
        if !self.io(before.metadata())?.is_file() {
            return Err(self.failure(EndpointError::Unsupported));
        }
        let identity = self.ep(object(&before))?;
        let file = self.io(open_at(
            parent.directory.file(),
            &parent.name,
            libc::O_RDONLY,
        ))?;
        self.ep(endpoint::native_same_mount(parent.directory.file(), &file))?;
        if self.ep(object(&file))? != identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let executable = self.ep(endpoint::native_regular_metadata(&file))?;
        let result = self.leases.acquire(file);
        let file = self.io(result)?;
        let stamp = Stamp::new(&self.io(file.metadata())?);
        let guard = StableFileGuard {
            parent: ParentGuard {
                directory: parent.directory.clone(),
                name: parent.name.clone(),
                epoch: self.nonce,
                side: parent.side,
                chain: parent.chain.clone(),
                path: parent.path.clone(),
            },
            file,
            actual: identity,
            stamp,
            executable,
        };
        self.validate_guard(&guard, true)?;
        Ok(guard)
    }
    fn validate_guard(&mut self, guard: &StableFileGuard, at_name: bool) -> Result<()> {
        self.check()?;
        self.parent_check(&guard.parent)?;
        if self.ep(object(&guard.file))? != guard.actual
            || Stamp::new(&self.io(guard.file.metadata())?) != guard.stamp
            || self.ep(endpoint::native_regular_metadata(&guard.file))? != guard.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        if at_name {
            let named = self.io(open_at(
                guard.parent.directory.file(),
                &guard.parent.name,
                libc::O_PATH,
            ))?;
            if self.ep(object(&named))? != guard.actual {
                return Err(self.failure(EndpointError::IdentityChanged));
            }
        }
        Ok(())
    }
    fn read_hash(&mut self, file: &File, mut output: Option<&mut File>) -> Result<Capture> {
        let mut hash = Sha256::new();
        let mut offset = 0_u64;
        let mut buffer = [0_u8; 16384];
        loop {
            self.check()?;
            self.read_budget
                .charge(buffer.len() as u64)
                .map_err(|()| self.failure(EndpointError::LimitExceeded))?;
            let read = self.io(file.read_at(&mut buffer, offset))?;
            if read == 0 {
                break;
            }
            let bytes = buffer
                .get(..read)
                .ok_or_else(|| self.failure(EndpointError::LimitExceeded))?;
            hash.update(bytes);
            if let Some(destination) = &mut output {
                #[cfg(test)]
                if tests::take_stage_space_failure() {
                    self.io::<()>(Err(io::Error::from_raw_os_error(libc::ENOSPC)))?;
                }
                self.io(destination.write_all(bytes))?;
            }
            offset = offset
                .checked_add(read as u64)
                .ok_or_else(|| self.failure(EndpointError::LimitExceeded))?;
        }
        Ok(Capture {
            digest: hash.finalize().into(),
            length: offset,
            executable: self.ep(endpoint::native_regular_metadata(file))?,
        })
    }
    pub(in crate::connection::transaction) fn capture(
        &mut self,
        guard: &StableFileGuard,
    ) -> Result<Capture> {
        self.validate_guard(guard, true)?;
        let capture = self.read_hash(&guard.file, None)?;
        self.validate_guard(guard, true)?;
        if capture.length != guard.stamp.length {
            return Err(self.failure(EndpointError::Unstable));
        }
        Ok(capture)
    }
    fn barriers(&mut self, first: &NativeDirectory, second: &NativeDirectory) -> Result<()> {
        // Barrier failure is always uncertain, even when the prior syscall was
        // successful. Never downgrade it to the preceding durable boundary.
        self.boundary = Boundary::Uncertain;
        #[cfg(test)]
        if tests::take_barrier_failure() {
            return Err(self.failure(EndpointError::Offline));
        }
        self.io(sync_directory(first.file()))?;
        let a = self.io(first.file().metadata())?;
        let b = self.io(second.file().metadata())?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            self.io(sync_directory(second.file()))?;
        }
        Ok(())
    }
    pub(in crate::connection::transaction) fn observe_absent(
        &mut self,
        parent: &ParentGuard,
    ) -> Result<AbsentName> {
        self.check()?;
        self.parent_check(parent)?;
        if parent.side != Side::Target || self.relocated {
            return Err(self.failure(EndpointError::Unsupported));
        }
        self.check_absent(parent)?;
        Ok(AbsentName {
            epoch: self.nonce,
            parent: self.ep(object(parent.directory.file()))?,
            name: parent.name.clone(),
        })
    }
    /// Fresh read-only name inspection; unlike `AbsentName` this grants no publication authority.
    fn check_absent(&mut self, parent: &ParentGuard) -> Result<()> {
        self.check()?;
        self.parent_check(parent)?;
        match open_at(parent.directory.file(), &parent.name, libc::O_PATH) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(self.failure(error.into())),
            Ok(_) => return Err(self.failure(EndpointError::Unstable)),
        }
        self.parent_check(parent)?;
        self.check()
    }
    #[cfg(test)]
    fn create_namespace(&mut self, target: &ParentGuard) -> Result<PrivateNamespace> {
        self.check()?;
        self.parent_check(target)?;
        if self._binding.is_none() || target.side != Side::Target || self.namespace_created {
            return Err(self.failure(EndpointError::Unsupported));
        }
        let random = self.io(random_id())?;
        let mut label = String::from(".qos-operation-");
        use std::fmt::Write as _;
        for byte in random {
            write!(label, "{byte:02x}").map_err(|_| self.failure(EndpointError::Unsupported))?;
        }
        let name =
            CString::new(label.as_bytes()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        let root = self.target_root.clone();
        self.namespace_created = true;
        self.io(destination_security::create_directory(
            root.file(),
            OsStr::from_bytes(name.to_bytes()),
        ))?;
        let directory = self.ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
        self.io(destination_security::check_private_file(directory.file()))?;
        let identity = self.ep(object(directory.file()))?;
        let private = PrivateNamespace {
            directory,
            identity,
            _name: name,
            epoch: self.nonce,
            intended_parent: self.ep(object(target.directory.file()))?,
            intended_name: target.name.clone(),
            descriptor: None,
        };
        // Bounded native intent, not a store intent acknowledgement. Persist exact
        // destination native name and parent generation before any live mutation.
        let mut intent = self.io(open_at(
            private.directory.file(),
            c"intent",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        let parent_identity = self.ep(object(target.directory.file()))?;
        let binding = self._binding.as_ref().ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: self.boundary,
            _os_error: None,
        })?;
        let mut descriptor_bytes = Vec::new();
        for bytes in [
            b"QNI1".as_slice(),
            &self.nonce,
            &binding._connection.0,
            &binding._operation,
            &binding._store_session,
            &binding._revision.to_le_bytes(),
            &binding._fence.to_le_bytes(),
            &self.identities[0].volume,
            &self.identities[0].root,
            &self.identities[1].volume,
            &self.identities[1].root,
            &private.identity.generation,
            &parent_identity.generation,
            target.name.to_bytes(),
        ] {
            descriptor_bytes.extend_from_slice(bytes);
        }
        self.io(intent.write_all(&descriptor_bytes))?;
        self.io(intent.sync_all())?;
        self.barriers(&private.directory, &root)?;
        self.boundary = Boundary::Untouched;
        self.namespace_check(&private)?;
        self.check()?;
        Ok(private)
    }
    fn stage_from_guard(
        &mut self,
        source: &StableFileGuard,
        private: &PrivateNamespace,
    ) -> Result<StagedFile> {
        self.validate_guard(source, true)?;
        self.namespace_check(private)?;
        let mode = CreatedMode::from_executable(source.executable);
        self.witness_create(c"stage")?;
        let mut file = self.io(create_regular_at(private.directory.file(), c"stage", mode))?;
        let created = self.ep(object(&file))?;
        self.ep(endpoint::native_same_mount(private.directory.file(), &file))?;
        self.io(destination_security::check_private_file(&file))?;
        let bytes = self.read_hash(&source.file, Some(&mut file))?;
        self.validate_guard(source, true)?;
        self.io(file.sync_all())?;
        self.io(destination_security::check_private_file(&file))?;
        if self.ep(object(&file))? != created
            || self.ep(endpoint::native_regular_metadata(&file))? != source.executable
            || bytes.executable != source.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let created_stamp = Stamp::new(&self.io(file.metadata())?);
        drop(file); // No writable fd survives lease acquisition.
        #[cfg(test)]
        witness::test_cut("stage-closed", &private.directory, c"stage");
        let parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"stage".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
            chain: vec![],
            path: None,
        };
        let guard = self.guard_regular(&parent)?;
        super::witness::created_regular_join(
            &descriptor_object(&created),
            &descriptor_stamp(&created_stamp),
            &descriptor_object(&guard.actual),
            &descriptor_stamp(&guard.stamp),
            source.executable,
        )
        .map_err(|_| self.failure(EndpointError::IdentityChanged))?;
        let verified = self.capture(&guard)?;
        if bytes.digest != verified.digest
            || bytes.length != verified.length
            || bytes.executable != verified.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.witness_checkpoint()?;
        let descriptor_bytes = self.created_stage_descriptor(source, private, &created, &bytes)?;
        self.barriers(&private.directory, &private.directory)?;
        self.boundary = Boundary::PrivateStaged;
        self.namespace_check(private)?;
        self.validate_guard(source, true)?;
        Ok(StagedFile {
            source: SourceBinding {
                file: Arc::clone(&source.file),
                stamp: source.stamp.clone(),
                executable: source.executable,
                parent: source.parent.directory.clone(),
                name: source.parent.name.clone(),
                actual: source.actual.clone(),
                side: source.parent.side,
            },
            guard,
            bytes,
            namespace: private.directory.clone(),
            intended_parent: private.intended_parent.clone(),
            intended_name: private.intended_name.clone(),
            descriptor: descriptor_bytes,
        })
    }
    fn created_stage_descriptor(
        &mut self,
        source: &StableFileGuard,
        private: &PrivateNamespace,
        created: &ObjectId,
        bytes: &Capture,
    ) -> Result<Option<Vec<u8>>> {
        let encoded = if let Some((header, intent, intent_bytes)) = &private.descriptor {
            let decoded =
                d::decode(intent_bytes).map_err(|_| self.failure(EndpointError::Unstable))?;
            let body = d::Body::Stage(d::Stage {
                intent: decoded.digest,
                object: descriptor_object(&created),
                content: descriptor_content(&bytes),
                source: d::Source::Endpoint {
                    side: intent.direction,
                    parent: descriptor_object(&self.ep(object(source.parent.directory.file()))?),
                    object: descriptor_object(&source.actual),
                    stamp: descriptor_stamp(&source.stamp),
                },
            });
            let encoded = if self.witness.is_some() {
                let d::Body::Stage(stage) = &body else {
                    return Err(self.failure(EndpointError::Unverified));
                };
                d::encode_created_stage(header, stage)
            } else {
                d::encode(header, &body)
            }
            .map_err(|_| self.failure(EndpointError::Unstable))?;
            d::decode(&encoded)
                .and_then(|value| value.joined(&decoded, None, None))
                .map_err(|_| self.failure(EndpointError::Unstable))?;
            self.write_descriptor(&private.directory, c"staged-intent", &encoded)?;
            Some(encoded)
        } else {
            let mut descriptor = self.io(open_at(
                private.directory.file(),
                c"staged-intent",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            ))?;
            let mut encoded = Vec::new();
            for value in [
                b"QNS1".as_slice(),
                &self.nonce,
                &created.generation,
                &bytes.digest,
                &bytes.length.to_le_bytes(),
                &[u8::from(bytes.executable)],
            ] {
                encoded.extend_from_slice(value);
            }
            self.io(descriptor.write_all(&encoded))?;
            self.io(descriptor.sync_all())?;
            None
        };
        Ok(encoded)
    }
    fn relocate_noreplace(
        &mut self,
        target: &ParentGuard,
        private: &PrivateNamespace,
        expected: &StableFileGuard,
    ) -> Result<RelocatedObject> {
        self.check()?;
        self.parent_check(target)?;
        self.namespace_check(private)?;
        if target.side != Side::Target
            || private.intended_name != target.name
            || private.intended_parent != self.ep(object(target.directory.file()))?
            || expected.parent.epoch != self.nonce
            || expected.parent.name != target.name
            || self.ep(object(expected.parent.directory.file()))?
                != self.ep(object(target.directory.file()))?
            || self.relocated
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        // Check retained bytes, not the path occupant: rename can still capture a
        // raced replacement. The result below identifies that actual object.
        self.validate_guard(expected, false)?;
        #[cfg(test)]
        tests::run_before_relocate();
        self.boundary = Boundary::Uncertain;
        self.witness_move(false)?;
        self.io(rename_noreplace(
            target.directory.file(),
            &target.name,
            private.directory.file(),
            c"displaced",
        ))?;
        self.relocated = true;
        self.boundary = Boundary::Uncertain;
        #[cfg(test)]
        tests::run_after_relocate();
        let retained = self.io(open_at(
            private.directory.file(),
            c"displaced",
            libc::O_PATH,
        ))?;
        self.ep(endpoint::native_same_mount(
            private.directory.file(),
            &retained,
        ))?;
        let actual = self.ep(object(&retained))?;
        let stamp = Stamp::new(&self.io(retained.metadata())?);
        let matches_guard = actual == expected.actual && stamp.relocated_eq(&expected.stamp);
        self.barriers(&target.directory, &private.directory)?;
        self.parent_check(target)?;
        self.namespace_check(private)?;
        let named = self.io(open_at(
            private.directory.file(),
            c"displaced",
            libc::O_PATH,
        ))?;
        if self.ep(object(&named))? != actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        // Close the at-name observation before the checkpoint opens its own pair.
        drop(named);
        self.boundary = Boundary::DurableQuarantine;
        self.witness_checkpoint()?;
        // A break does not erase durable actual-object accounting. No success
        // receipt or publish authority follows from this preserved-name result.
        let result = self.leases.check();
        if result.is_err() {
            self.cancel();
        }
        Ok(RelocatedObject {
            epoch: self.nonce,
            actual,
            retained,
            namespace: private.directory.clone(),
            original_parent: target.directory.clone(),
            _original_name: target.name.clone(),
            matches_guard,
            descriptor: None,
        })
    }
    pub(in crate::connection::transaction) fn guard_displaced(
        &mut self,
        displaced: &RelocatedObject,
    ) -> Result<StableFileGuard> {
        if displaced.epoch != self.nonce
            || !displaced.matches_guard
            || displaced.actual.kind != libc::S_IFREG
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let parent = ParentGuard {
            directory: displaced.namespace.clone(),
            name: c"displaced".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
            chain: vec![],
            path: None,
        };
        let guard = self.guard_regular(&parent)?;
        if guard.actual != displaced.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        Ok(guard)
    }
    /// Primitive addition entrypoint. Connected replacement uses the sealed wrapper.
    #[cfg(test)]
    fn publish_absent(
        &mut self,
        staged: StagedFile,
        target: &ParentGuard,
        absent: AbsentName,
        source: &StableFileGuard,
    ) -> Result<NativePublication> {
        self.publish_to_vacancy(staged, target, PublicationVacancy::Original(absent), source)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the admitted rename, retained-object reread and explicit observation-fd closure in one proof scope."
    )]
    fn publish_to_vacancy(
        &mut self,
        mut staged: StagedFile,
        target: &ParentGuard,
        vacancy: PublicationVacancy,
        source: &StableFileGuard,
    ) -> Result<NativePublication> {
        let (epoch, parent, name, sealed_replacement) = match vacancy {
            PublicationVacancy::Original(absent) => {
                (absent.epoch, absent.parent, absent.name, false)
            }
            PublicationVacancy::Sealed {
                epoch,
                parent,
                name,
            } => (epoch, parent, name, true),
        };
        self.check()?;
        self.parent_check(target)?;
        if self.relocated != sealed_replacement
            || staged.source.actual != source.actual
            || staged.source.name != source.parent.name
            || staged.source.side != source.parent.side
            || self.ep(object(staged.source.parent.file()))?
                != self.ep(object(source.parent.directory.file()))?
            || staged.intended_name != target.name
            || staged.intended_parent != parent
            || target.side != Side::Target
            || epoch != self.nonce
            || name != target.name
            || parent != self.ep(object(target.directory.file()))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.validate_guard(source, true)?;
        self.validate_guard(&staged.guard, true)?;
        let fresh = self.capture(source)?;
        if fresh.digest != staged.bytes.digest
            || fresh.length != staged.bytes.length
            || fresh.executable != staged.bytes.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.boundary = Boundary::Uncertain;
        self.witness_move(true)?;
        self.io(rename_noreplace(
            staged.namespace.file(),
            c"stage",
            target.directory.file(),
            &target.name,
        ))?;
        self.boundary = Boundary::Uncertain;
        self.barriers(&staged.namespace, &target.directory)?;
        self.parent_check(target)?;
        let actual = self.io(open_at(
            target.directory.file(),
            &target.name,
            libc::O_RDONLY,
        ))?;
        if self.ep(object(&actual))? != staged.guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let before = Stamp::new(&self.io(actual.metadata())?);
        if !before.relocated_eq(&staged.guard.stamp) {
            return Err(self.failure(EndpointError::Unstable));
        }
        #[cfg(test)]
        tests::run_before_publication_reread();
        let bytes = self.read_hash(&actual, None)?;
        if before != Stamp::new(&self.io(actual.metadata())?) {
            return Err(self.failure(EndpointError::Unstable));
        }
        if bytes.digest != staged.bytes.digest
            || bytes.length != staged.bytes.length
            || bytes.executable != staged.bytes.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        // The leased staged guard retains the object; this reread fd is temporary.
        drop(actual);
        self.validate_guard(source, true)?;
        self.check()?;
        let named = self.io(open_at(target.directory.file(), &target.name, libc::O_PATH))?;
        if self.ep(object(&named))? != staged.guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        drop(named);
        // The reread and source postflight can take time. A retained directory
        // fd still names a detached old parent, so recheck its ancestry/security
        // at the final proof boundary rather than trusting the earlier check.
        self.parent_check(target)?;
        self.boundary = Boundary::Published;
        self.witness_checkpoint()?;
        // Retain the same leased object; only the proven rename changes its
        // parent/name and ctime snapshot. This acquires no fifth guard.
        staged.guard.stamp = before;
        staged.guard.parent = ParentGuard {
            directory: target.directory.clone(),
            name: target.name.clone(),
            epoch: target.epoch,
            side: target.side,
            chain: target.chain.clone(),
            path: target.path.clone(),
        };
        Ok(NativePublication {
            _source: staged.source,
            epoch: self.nonce,
            _identity: staged.guard.actual.clone(),
            _bytes: bytes,
            _parent: target.directory.clone(),
            _name: target.name.clone(),
            guard: staged.guard,
            stage_descriptor: staged.descriptor,
        })
    }
}

struct ConnectedBinding {
    selected_action: Option<crate::connection::store::ExecutionAction>,
    capture_only: bool,
    id: OperationId,
    lifetime: Weak<()>,
    admission: u64,
    fence: u64,
    direction: usize,
    intent: crate::connection::store::OperationIntent,
}
fn descriptor_object(value: &ObjectId) -> d::Object {
    d::Object {
        dev: value.dev,
        ino: value.ino,
        kind: value.kind,
        generation: value.generation,
    }
}
fn descriptor_stamp(value: &Stamp) -> d::Stamp {
    d::Stamp {
        length: value.length,
        modified: value.modified,
        changed: value.changed,
        mode: value.mode,
        uid: value.uid,
        gid: value.gid,
        links: value.links,
    }
}
fn descriptor_content(value: &Capture) -> d::Content {
    d::Content {
        digest: value.digest,
        length: value.length,
        executable: value.executable,
    }
}
fn descriptor_endpoint(value: EndpointIdentity) -> [u8; 64] {
    let mut result = [0; 64];
    result[..32].copy_from_slice(&value.volume);
    result[32..].copy_from_slice(&value.root);
    result
}
impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn begin_connected(
        live: &LiveOperation,
        source: &'e PinnedEndpoint,
        target: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> Result<Self> {
        if live.native_kind().is_some() {
            return Err(Failure {
                _cause: EndpointError::Unverified,
                _boundary: Boundary::Untouched,
                _os_error: None,
            });
        }
        Self::begin_connected_mode(live, source, target, cancel, false)
    }
    fn begin_connected_mode(
        live: &LiveOperation,
        source: &'e PinnedEndpoint,
        target: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
        capture_only: bool,
    ) -> Result<Self> {
        use crate::connection::model::{Decision, Side as ModelSide};
        let failure = || Failure {
            _cause: EndpointError::Unverified,
            _boundary: Boundary::Untouched,
            _os_error: None,
        };
        if !(live.action().is_reconciliation()
            || capture_only
                && matches!(
                    live.action(),
                    crate::connection::store::ExecutionAction::Captured
                ))
        {
            return Err(failure());
        }
        let direction = if capture_only {
            0
        } else {
            match live.intent().decision {
                Decision::VerifyEqual
                | Decision::Propagate {
                    from: ModelSide::Source,
                }
                | Decision::RecoverDeletion {
                    at: ModelSide::Destination,
                } => 0,
                Decision::Propagate {
                    from: ModelSide::Destination,
                }
                | Decision::RecoverDeletion {
                    at: ModelSide::Source,
                } => 1,
                _ => return Err(failure()),
            }
        };
        if cancel.load(std::sync::atomic::Ordering::Acquire)
            || live
                .gate()
                .stopped()
                .load(std::sync::atomic::Ordering::Acquire)
            || live.lifetime().strong_count() == 0
            || (!capture_only
                && matches!(live.intent().decision, Decision::RecoverDeletion { .. })
                && !live.deletion_baseline_checked())
            || Some(&source.identity()) != live.endpoints().get(direction)
            || Some(&target.identity()) != live.endpoints().get(1 - direction)
        {
            return Err(failure());
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&live.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            source,
            target,
            OperationBinding {
                _connection: live.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: live.admission(),
                _fence: live.fence(),
            },
            Arc::clone(live.operation_reads()),
        )?;
        epoch.cancel_read = Some(cancel);
        epoch.stop_read = Some(live.gate());
        epoch.connected = Some(ConnectedBinding {
            selected_action: None,
            capture_only,
            id: live.id(),
            lifetime: live.lifetime().clone(),
            admission: live.admission(),
            fence: live.fence(),
            direction,
            intent: live.intent().clone(),
        });
        if !capture_only
            && matches!(
                live.intent().decision,
                Decision::VerifyEqual | Decision::Propagate { .. }
            )
        {
            if live.native_kind()
                == Some(crate::connection::store::NativeOperationKind::DirectoryV1)
            {
                epoch.directory_requests = Some(Arc::new(std::sync::atomic::AtomicU16::new(0)));
                epoch.prove_directory_ancestors(live.native_identity())?;
            } else {
                epoch.prove_existing_ancestors(live.native_identity())?;
            }
        }
        Ok(epoch)
    }
    fn connected_check(&mut self, live: &LiveOperation) -> Result<()> {
        if self.directory_requests.is_some()
            != (live.native_kind()
                == Some(crate::connection::store::NativeOperationKind::DirectoryV1))
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        if (self
            .connected
            .as_ref()
            .is_none_or(|binding| !binding.capture_only && binding.selected_action.is_none())
            && matches!(
                live.intent().decision,
                crate::connection::model::Decision::RecoverDeletion { .. }
            )
            && !live.deletion_baseline_checked())
            || !self.connected.as_ref().is_some_and(|binding| {
                (live.action().is_reconciliation()
                    || binding.selected_action.as_ref() == Some(live.action())
                    || (binding.capture_only
                        && matches!(
                            live.action(),
                            crate::connection::store::ExecutionAction::Captured
                        )))
                    && binding.id == live.id()
                    && binding.lifetime.ptr_eq(live.lifetime())
                    && binding.lifetime.strong_count() != 0
                    && binding.admission == live.admission()
                    && binding.fence == live.fence()
                    && &binding.intent == live.intent()
            })
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.validate_existing_ancestors(false)?;
        self.check()
    }
    fn admitted(
        &mut self,
        live: &LiveOperation,
        step: &StepAdmission,
        kind: StepKind,
    ) -> Result<()> {
        if self
            .connected
            .as_ref()
            .is_some_and(|binding| binding.capture_only || binding.selected_action.is_some())
        {
            return Err(self.failure(EndpointError::Unverified));
        }

        self.connected_check(live)?;
        step.validate(live.id(), live.lifetime(), live.revision(), kind)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        if step.stopped().load(std::sync::atomic::Ordering::Acquire) {
            return Err(self.failure(EndpointError::Cancelled));
        }
        Ok(())
    }
    fn state(&mut self, guard: &StableFileGuard) -> Result<d::State> {
        let bytes = self.capture(guard)?;
        Ok(d::State::Regular {
            object: descriptor_object(&guard.actual),
            stamp: descriptor_stamp(&guard.stamp),
            content: descriptor_content(&bytes),
        })
    }
    fn matches_plan(
        state: &d::State,
        planned: Option<&crate::connection::model::EntryState>,
    ) -> bool {
        use crate::connection::model::EntryState;
        match (state, planned) {
            (d::State::Absent, None) => true,
            (
                d::State::Regular { content, .. },
                Some(EntryState::File {
                    sha256,
                    bytes,
                    executable,
                }),
            ) => {
                content.digest == *sha256
                    && content.length == *bytes
                    && content.executable == *executable
            }
            _ => false,
        }
    }
    fn write_descriptor(
        &mut self,
        directory: &NativeDirectory,
        name: &CStr,
        bytes: &[u8],
    ) -> Result<()> {
        self.check()?;
        let valid = if self.directory_requests.is_some() {
            d::directory::decode(bytes).is_ok()
        } else {
            d::decode(bytes).is_ok()
        };
        if !valid {
            return Err(self.failure(EndpointError::Unsupported));
        }
        self.witness_create(name)?;
        let mut file = self.io(open_at(
            directory.file(),
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        let created = self.ep(object(&file))?;
        self.io(destination_security::check_private_file(&file))?;
        #[cfg(test)]
        witness::test_cut("descriptor-prewrite", directory, name);
        self.io(file.write_all(bytes))?;
        self.io(file.sync_all())?;
        let stamp = Stamp::new(&self.io(file.metadata())?);
        if self.ep(object(&file))? != created {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        drop(file);
        #[cfg(test)]
        witness::test_cut("descriptor-closed", directory, name);
        if self.witness.is_some() {
            let evidence = self.descriptor_snapshot(directory, name, bytes)?;
            if evidence.identity != created || evidence.stamp != stamp {
                return Err(self.failure(EndpointError::IdentityChanged));
            }
            let result = self
                .witness
                .as_mut()
                .ok_or_else(|| Failure {
                    _cause: EndpointError::Unverified,
                    _boundary: self.boundary,
                    _os_error: None,
                })?
                .remember_created(name, evidence);
            self.io(result)?;
        }
        self.barriers(directory, directory)?;
        self.witness_checkpoint()
    }
    fn read_descriptor(
        &mut self,
        directory: &NativeDirectory,
        name: &CStr,
        expected: &[u8],
    ) -> Result<()> {
        if self.witness.is_some() {
            let current = self.descriptor_snapshot(directory, name, expected)?;
            let witness = self.witness.as_mut().ok_or_else(|| Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.boundary,
                _os_error: None,
            })?;
            if let Some(original) = witness.descriptor(name) {
                if original.identity != current.identity
                    || original.stamp != current.stamp
                    || original.bytes != current.bytes
                {
                    return Err(self.failure(EndpointError::IdentityChanged));
                }
            } else {
                if !witness.restarted {
                    return Err(self.failure(EndpointError::Unverified));
                }
                let result = witness.remember_observed(name, current);
                self.io(result)?;
            }
            return self.check();
        }
        self.check()?;
        let file = self.io(open_at(directory.file(), name, libc::O_RDONLY))?;
        self.io(destination_security::check_private_file(&file))?;
        let before = Stamp::new(&self.io(file.metadata())?);
        if before.length > d::MAX_BYTES as u64 || before.length != expected.len() as u64 {
            return Err(self.failure(EndpointError::Unstable));
        }
        let mut bytes = vec![0; expected.len() + 1];
        self.charge(bytes.len() as u64)?;
        let count = self.io(file.read_at(&mut bytes, 0))?;
        if count != expected.len()
            || bytes.get(..count) != Some(expected)
            || Stamp::new(&self.io(file.metadata())?) != before
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.check()
    }
    fn charge(&mut self, count: u64) -> Result<()> {
        self.check()?;
        self.read_budget
            .charge(count)
            .map_err(|()| self.failure(EndpointError::LimitExceeded))?;
        Ok(())
    }
    pub(in crate::connection::transaction) fn create_registered_namespace(
        &mut self,
        live: &LiveOperation,
        reservation: &NamespaceReservation,
        source: &ParentGuard,
        target: &ParentGuard,
        source_guard: &StableFileGuard,
        target_guard: Option<&StableFileGuard>,
        step: StepAdmission,
    ) -> Result<PrivateNamespace> {
        self.create_connected_namespace(
            live,
            reservation,
            source,
            target,
            Some(source_guard),
            target_guard,
            step,
        )
    }
    pub(in crate::connection::transaction) fn create_deletion_namespace(
        &mut self,
        live: &LiveOperation,
        reservation: &NamespaceReservation,
        source: &ParentGuard,
        target: &ParentGuard,
        target_guard: &StableFileGuard,
        step: StepAdmission,
    ) -> Result<PrivateNamespace> {
        if !self.deletion_scope
            || !matches!(
                live.intent().decision,
                crate::connection::model::Decision::RecoverDeletion { .. }
            )
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.create_connected_namespace(
            live,
            reservation,
            source,
            target,
            None,
            Some(target_guard),
            step,
        )
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    fn create_connected_namespace(
        &mut self,
        live: &LiveOperation,
        reservation: &NamespaceReservation,
        source: &ParentGuard,
        target: &ParentGuard,
        source_guard: Option<&StableFileGuard>,
        target_guard: Option<&StableFileGuard>,
        step: StepAdmission,
    ) -> Result<PrivateNamespace> {
        self.admitted(live, &step, StepKind::Namespace)?;
        self.parent_check(source)?;
        self.parent_check(target)?;
        let header = reservation
            .header(live, self.nonce)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        if self.namespace_created
            || source.side != Side::Source
            || target.side != Side::Target
            || source.path.as_ref() != Some(&live.intent().path)
            || target.path.as_ref() != Some(&live.intent().path)
            || source_guard.is_some_and(|guard| {
                guard.parent.path != source.path || guard.parent.side != Side::Source
            })
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let direction = self
            .connected
            .as_ref()
            .ok_or_else(|| Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.boundary,
                _os_error: None,
            })?
            .direction;
        let source_state = if let Some(guard) = source_guard {
            self.state(guard)?
        } else {
            if !matches!(
                live.intent().decision,
                crate::connection::model::Decision::RecoverDeletion { .. }
            ) {
                return Err(self.failure(EndpointError::Unverified));
            }
            self.check_absent(source)?;
            d::State::Absent
        };
        if source_guard.is_some() {
            let name = CString::new(reservation.name())
                .map_err(|_| self.failure(EndpointError::Unsupported))?;
            let witness = self.io(witness::NamespaceWitness::new(
                &self.target_root.clone(),
                target,
                name,
                true,
            ))?;
            self.witness = Some(witness);
        }
        let target_state = if let Some(guard) = target_guard {
            if guard.parent.path != target.path || guard.parent.side != Side::Target {
                return Err(self.failure(EndpointError::Unverified));
            }
            self.state(guard)?
        } else {
            self.check_absent(target)?;
            d::State::Absent
        };
        let mut expected = [d::State::Absent, d::State::Absent];
        *expected
            .get_mut(direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))? = source_state;
        *expected
            .get_mut(1 - direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))? = target_state;
        if !Self::matches_plan(&expected[0], live.intent().source_before.as_ref())
            || !Self::matches_plan(&expected[1], live.intent().destination_before.as_ref())
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.admitted(live, &step, StepKind::Namespace)?;
        let name = CString::new(reservation.name())
            .map_err(|_| self.failure(EndpointError::Unsupported))?;
        let root = self.target_root.clone();
        self.namespace_created = true;
        self.io(destination_security::create_directory(
            root.file(),
            OsStr::from_bytes(name.to_bytes()),
        ))?;
        let directory = self.ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
        self.witness_watch_namespace(&directory)?;
        self.io(destination_security::check_private_file(directory.file()))?;
        let identity = self.ep(object(directory.file()))?;
        self.witness_checkpoint()?;
        let mut chains = [Vec::new(), Vec::new()];
        *chains
            .get_mut(direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))? =
            source.chain.iter().map(descriptor_object).collect();
        *chains
            .get_mut(1 - direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))? =
            target.chain.iter().map(descriptor_object).collect();
        let intent = d::Intent {
            endpoints: live.endpoints().map(descriptor_endpoint),
            direction: direction as u8,
            decision: if source_guard.is_some() {
                if direction == 0 { 1 } else { 2 }
            } else if direction == 0 {
                4
            } else {
                3
            },
            namespace: descriptor_object(&identity),
            path: live.intent().path.clone(),
            chains,
            expected,
            selection: d::Selection::None,
        };
        let bytes = d::encode(&header, &d::Body::Intent(intent.clone()))
            .map_err(|_| self.failure(EndpointError::Unsupported))?;
        self.write_descriptor(&directory, c"intent", &bytes)?;
        self.barriers(&directory, &root)?;
        let private = PrivateNamespace {
            directory,
            _name: name,
            epoch: self.nonce,
            identity,
            intended_parent: self.ep(object(target.directory.file()))?,
            intended_name: target.name.clone(),
            descriptor: Some((header, intent, bytes)),
        };
        self.namespace_check(&private)?;
        self.parent_check(source)?;
        self.parent_check(target)?;
        self.boundary = Boundary::Untouched;
        Ok(private)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the single-step admission and retain it until this native step returns."
    )]
    pub(in crate::connection::transaction) fn stage_connected(
        &mut self,
        live: &LiveOperation,
        source: &StableFileGuard,
        private: &PrivateNamespace,
        step: StepAdmission,
    ) -> Result<StagedFile> {
        self.admitted(live, &step, StepKind::Stage)?;
        if private.descriptor.is_none() {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.stage_from_guard(source, private)
    }
    pub(in crate::connection::transaction) fn prepared<'a>(
        &'a mut self,
        private: &'a PrivateNamespace,
        staged: Option<&'a StagedFile>,
    ) -> Result<super::NativePrepared<'a, 'e>> {
        self.namespace_check(private)?;
        let Some((_, intent, _)) = &private.descriptor else {
            return Err(self.failure(EndpointError::Unverified));
        };
        if staged.is_none() != matches!(intent.decision, 3 | 4) {
            return Err(self.failure(EndpointError::Unverified));
        }
        Ok(super::NativePrepared {
            epoch: self,
            namespace: private,
            staged,
        })
    }
}

impl super::NativePrepared<'_, '_> {
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.connected_check(live)?;
            self.epoch.namespace_check(self.namespace)?;
            let Some((_, _, bytes)) = &self.namespace.descriptor else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            self.epoch
                .read_descriptor(&self.namespace.directory, c"intent", bytes)?;
            let Some(staged) = self.staged else {
                let value =
                    d::decode(bytes).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
                if !matches!(
                    value.body,
                    d::Body::Intent(d::Intent {
                        decision: 3 | 4,
                        ..
                    })
                ) {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                }
                return self.epoch.check();
            };
            let Some(bytes) = &staged.descriptor else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            if self
                .epoch
                .witness
                .as_ref()
                .is_none_or(|witness| !witness.created_preparation())
                || d::decode(bytes)
                    .map_err(|_| self.epoch.failure(EndpointError::Unverified))?
                    .profile
                    != d::Profile::CreatedRegular
            {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            if self.epoch.ep(object(staged.namespace.file()))? != self.namespace.identity {
                return Err(self.epoch.failure(EndpointError::IdentityChanged));
            }
            let source = StableFileGuard {
                parent: ParentGuard {
                    directory: staged.source.parent.clone(),
                    name: staged.source.name.clone(),
                    epoch: self.epoch.nonce,
                    side: staged.source.side,
                    chain: Vec::new(),
                    path: None,
                },
                file: Arc::clone(&staged.source.file),
                actual: staged.source.actual.clone(),
                stamp: staged.source.stamp.clone(),
                executable: staged.source.executable,
            };
            self.epoch.validate_guard(&source, true)?;
            self.epoch.validate_guard(&staged.guard, true)?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"staged-intent", bytes)?;
            self.epoch.namespace_check(self.namespace)?;
            self.epoch.witness_checkpoint()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn descriptors(
        &self,
    ) -> std::result::Result<(Vec<u8>, Option<Vec<u8>>), StoreError> {
        let (_, _, intent) = self
            .namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((
            intent.clone(),
            self.staged.and_then(|value| value.descriptor.clone()),
        ))
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn relocate_connected(
        &mut self,
        live: &LiveOperation,
        target: &ParentGuard,
        private: &PrivateNamespace,
        source: &StableFileGuard,
        expected: &StableFileGuard,
        prepared: &crate::connection::store::DurableAck<crate::connection::store::PreparedNative>,
        step: StepAdmission,
    ) -> Result<RelocatedObject> {
        self.relocate_connected_inner(
            live,
            target,
            private,
            &source.parent,
            Some(source),
            expected,
            prepared,
            step,
        )
    }
    pub(in crate::connection::transaction) fn relocate_deletion(
        &mut self,
        live: &LiveOperation,
        source: &ParentGuard,
        target: &ParentGuard,
        private: &PrivateNamespace,
        expected: &StableFileGuard,
        prepared: &crate::connection::store::DurableAck<crate::connection::store::PreparedNative>,
        step: StepAdmission,
    ) -> Result<RelocatedObject> {
        if !self.deletion_scope {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.relocate_connected_inner(
            live, target, private, source, None, expected, prepared, step,
        )
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    fn relocate_connected_inner(
        &mut self,
        live: &LiveOperation,
        target: &ParentGuard,
        private: &PrivateNamespace,
        source_parent: &ParentGuard,
        source: Option<&StableFileGuard>,
        expected: &StableFileGuard,
        prepared: &crate::connection::store::DurableAck<crate::connection::store::PreparedNative>,
        step: StepAdmission,
    ) -> Result<RelocatedObject> {
        self.admitted(live, &step, StepKind::Relocate)?;
        let Some((header, intent, intent_bytes)) = &private.descriptor else {
            return Err(self.failure(EndpointError::Unverified));
        };
        prepared
            .value()
            .validate(live, header)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let (ack_intent, ack_stage) = prepared.value().descriptors();
        if ack_intent != intent_bytes {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.read_descriptor(&private.directory, c"intent", intent_bytes)?;
        let intent_descriptor =
            d::decode(intent_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let stage = ack_stage
            .map(d::decode)
            .transpose()
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        if let Some(bytes) = ack_stage {
            self.read_descriptor(&private.directory, c"staged-intent", bytes)?;
        }
        if source_parent.side != Side::Source
            || source_parent.path.as_ref() != Some(&live.intent().path)
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let source_state = if let Some(source) = source {
            self.state(source)?
        } else {
            if !matches!(intent.decision, 3 | 4) {
                return Err(self.failure(EndpointError::Unverified));
            }
            self.check_absent(source_parent)?;
            d::State::Absent
        };
        if Some(&source_state) != intent.expected.get(usize::from(intent.direction)) {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.admitted(live, &step, StepKind::Relocate)?;
        let mut actual = self.relocate_noreplace(target, private, expected)?;
        let stamp = Stamp::new(&self.io(actual.retained.metadata())?);
        let mut header = header.clone();
        header.progress = live.revision();
        let bytes = d::encode(
            &header,
            &d::Body::Displaced(d::Displaced {
                intent: intent_descriptor.digest,
                stage: stage.as_ref().map(|value| value.digest),
                object: descriptor_object(&actual.actual),
                stamp: descriptor_stamp(&stamp),
                matches_expected: actual.matches_guard,
                side: 1 - intent.direction,
                parent: descriptor_object(&private.intended_parent),
                leaf: NativeName::Unix(private.intended_name.to_bytes().to_vec()),
                barriers: 3,
            }),
        )
        .map_err(|_| self.failure(EndpointError::Unstable))?;
        d::decode(&bytes)
            .and_then(|value| value.joined(&intent_descriptor, stage.as_ref(), None))
            .map_err(|_| self.failure(EndpointError::Unstable))?;
        self.write_descriptor(&private.directory, c"displaced-intent", &bytes)?;
        self.namespace_check(private)?;
        self.parent_check(target)?;
        actual.descriptor = Some(bytes);
        self.boundary = Boundary::DurableQuarantine;
        Ok(actual)
    }
    pub(in crate::connection::transaction) fn recovery_source<'a>(
        &'a mut self,
        live: &LiveOperation,
        private: &'a PrivateNamespace,
        displaced: &'a RelocatedObject,
        guard: &'a StableFileGuard,
        step: StepAdmission,
    ) -> Result<super::NativeRecoveryReader<'a, 'e>> {
        self.admitted(live, &step, StepKind::Preserve)?;
        if !displaced.matches_guard
            || displaced.epoch != self.nonce
            || displaced.actual != guard.actual
            || displaced.descriptor.is_none()
            || private.descriptor.is_none()
            || self.ep(object(displaced.namespace.file()))? != private.identity
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.namespace_check(private)?;
        let content = descriptor_content(&self.capture(guard)?);
        Ok(super::NativeRecoveryReader {
            selected_reader: None,
            epoch: self,
            namespace: private,
            displaced,
            guard,
            admission: step,
            content,
        })
    }
    pub(in crate::connection::transaction) fn seal_displacement(
        &mut self,
        live: &LiveOperation,
        displaced: RelocatedObject,
        guard: StableFileGuard,
        private: &PrivateNamespace,
        recovery: crate::connection::store::DurableAck<crate::connection::store::BoundRecovery>,
    ) -> Result<super::SealedDisplacement> {
        self.connected_check(live)?;
        self.namespace_check(private)?;
        self.validate_guard(&guard, true)?;
        self.ep(displaced.original_parent.validate(true))?;
        if !displaced.matches_guard
            || displaced.epoch != self.nonce
            || displaced.actual != guard.actual
            || self.ep(object(displaced.namespace.file()))? != private.identity
            || self.ep(object(&displaced.retained))? != guard.actual
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let Some((_, intent, bytes)) = &private.descriptor else {
            return Err(self.failure(EndpointError::Unverified));
        };
        let decoded = d::decode(bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let content = descriptor_content(&self.capture(&guard)?);
        let fact = recovery
            .value()
            .validate(live, self.nonce)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        if fact.intent != decoded.digest
            || fact.side != 1 - intent.direction
            || Some(&fact.endpoint) != intent.endpoints.get(usize::from(fact.side))
            || fact.object != descriptor_object(&guard.actual)
            || fact.content != content
            || fact.role != 0
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.read_descriptor(
            &private.directory,
            c"displaced-intent",
            displaced.descriptor.as_ref().ok_or_else(|| Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.boundary,
                _os_error: None,
            })?,
        )?;
        self.namespace_check(private)?;
        self.ep(displaced.original_parent.validate(true))?;
        self.witness_checkpoint()?;
        Ok(super::SealedDisplacement {
            displaced,
            guard,
            recovery,
            lifetime: live.lifetime().clone(),
        })
    }
}
impl super::NativeRecoveryReader<'_, '_> {
    pub(in crate::connection) fn checkpoint(&mut self) -> std::result::Result<(), StoreError> {
        self.validate()?;
        self.epoch
            .witness_checkpoint()
            .map_err(|_| StoreError::RecoveryRequired)
    }

    pub(in crate::connection) fn validate_for(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        if let Some(reader) = &mut self.selected_reader {
            reader.validate(store, live)?;
            if self
                .epoch
                .connected
                .as_ref()
                .is_none_or(|binding| binding.selected_action.as_ref() != Some(live.action()))
            {
                return Err(StoreError::RecoveryRequired);
            }
            self.epoch
                .connected_check(live)
                .map_err(|_| StoreError::RecoveryRequired)?;
            self.admission.validate(
                live.id(),
                live.lifetime(),
                live.revision(),
                StepKind::Preserve,
            )?;
        } else {
            self.epoch
                .admitted(live, &self.admission, StepKind::Preserve)
                .map_err(|_| StoreError::RecoveryRequired)?;
        }
        self.validate()
    }
    pub(in crate::connection) fn advance_selected(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        if let Some(reader) = &mut self.selected_reader {
            reader.advance(store, live, previous)?;
        }
        self.validate()
    }
    pub(in crate::connection) fn validate(&mut self) -> std::result::Result<(), StoreError> {
        if let Some(reader) = &mut self.selected_reader {
            reader.retained_check()?;
        }
        let result = (|| {
            if self
                .admission
                .stopped()
                .load(std::sync::atomic::Ordering::Acquire)
                || self
                    .epoch
                    .connected
                    .as_ref()
                    .is_none_or(|binding| binding.lifetime.strong_count() == 0)
            {
                return Err(self.epoch.failure(EndpointError::Cancelled));
            }
            self.epoch.namespace_check(self.namespace)?;
            self.epoch.validate_guard(self.guard, true)?;
            self.epoch
                .ep(self.displaced.original_parent.validate(true))?;
            let epoch_nonce = self.epoch.nonce;
            if self.displaced.actual != self.guard.actual
                || self.displaced.epoch != epoch_nonce
                || self.epoch.ep(object(&self.displaced.retained))? != self.guard.actual
            {
                return Err(self.epoch.failure(EndpointError::IdentityChanged));
            }
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn read_chunk(
        &mut self,
        offset: u64,
        buffer: &mut [u8],
    ) -> std::result::Result<usize, StoreError> {
        if buffer.len() > 256 * 1024 || offset > self.content.length {
            return Err(StoreError::LimitExceeded);
        }
        self.validate()?;
        self.charge_store_read(buffer.len() as u64)?;
        let count = self
            .guard
            .file
            .read_at(buffer, offset)
            .map_err(StoreError::from)?;
        self.validate()?;
        Ok(count)
    }
    pub(in crate::connection) fn charge_store_read(
        &mut self,
        count: u64,
    ) -> std::result::Result<(), StoreError> {
        if self
            .admission
            .stopped()
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(StoreError::Cancelled);
        }
        self.epoch
            .charge(count)
            .map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn content(&self) -> &d::Content {
        &self.content
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    pub(in crate::connection) fn epoch(&self) -> [u8; 16] {
        self.epoch.nonce
    }
    pub(in crate::connection) fn fact(
        &self,
        id: crate::connection::store::RecoveryId,
        artifact: d::Artifact,
    ) -> std::result::Result<(u32, d::RecoveryFact, Vec<u8>), StoreError> {
        let (header, intent, bytes) = self
            .namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let descriptor = d::decode(bytes).map_err(|_| StoreError::RecoveryRequired)?;
        if header.connection != id.operation.connection.0 || header.operation != id.operation.serial
        {
            return Err(StoreError::StaleRevision);
        }
        let side = 1 - intent.direction;
        let fact = d::RecoveryFact {
            intent: descriptor.digest,
            recovery: d::Recovery {
                connection: id.operation.connection.0,
                operation: id.operation.serial,
                slot: id.slot,
            },
            side,
            role: 0,
            endpoint: *intent
                .endpoints
                .get(usize::from(side))
                .ok_or(StoreError::Corrupt)?,
            object: descriptor_object(&self.guard.actual),
            content: self.content.clone(),
            artifact,
        };
        Ok((
            u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?,
            fact,
            self.displaced
                .descriptor
                .clone()
                .ok_or(StoreError::RecoveryRequired)?,
        ))
    }
}

impl NativeEpoch<'_> {
    pub(in crate::connection::transaction) fn owned_names(
        &mut self,
        live: &LiveOperation,
        side: Side,
        records: Vec<crate::connection::store::NativeNamespaceRecord>,
    ) -> Result<super::NativeOwnedNames> {
        self.connected_check(live)?;
        if records.len() > 128 {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        for record in &records {
            record
                .validate(live)
                .map_err(|_| self.failure(EndpointError::Unverified))?;
        }
        let direction = self
            .connected
            .as_ref()
            .ok_or_else(|| Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.boundary,
                _os_error: None,
            })?
            .direction;
        let model_side = if side == Side::Source {
            direction
        } else {
            1 - direction
        } as u8;
        let root = if side == Side::Source {
            self.source_root.clone()
        } else {
            self.target_root.clone()
        };
        Ok(super::NativeOwnedNames {
            records: records
                .into_iter()
                .filter(|record| record.side() == model_side)
                .collect(),
            root: self.ep(object(root.file()))?,
            endpoint: descriptor_endpoint(if side == Side::Source {
                self.identities[0]
            } else {
                self.identities[1]
            }),
            budget: Arc::clone(&self.read_budget),
            epoch_lifetime: Arc::downgrade(&self.lifetime),
            directory_requests: self
                .directory_requests
                .as_ref()
                .map(|count| (live.id(), Arc::clone(count))),
            store_lifetime: live.lifetime().clone(),
        })
    }
}
#[cfg(test)]
type OwnershipReadHook = (usize, Box<dyn FnOnce()>);
#[cfg(test)]
thread_local! {
    static AFTER_OWNED_READS: std::cell::RefCell<Option<OwnershipReadHook>> =
        const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn after_owned_reads() {
    let callback = AFTER_OWNED_READS.with(|hook| {
        let mut hook = hook.borrow_mut();
        match hook.as_mut() {
            Some((0, _)) => hook.take().map(|(_, callback)| callback),
            Some((remaining, _)) => {
                *remaining -= 1;
                None
            }
            None => None,
        }
    });
    if let Some(callback) = callback {
        callback();
    }
}

/// Retain the directory handle and content-change stamp throughout a scan.
/// Stamp includes length, mtime, ctime, mode, owner and link count, matching or
/// exceeding ordinary observation's metadata/change-token checks on Linux.
pub(super) struct OwnedSnapshot {
    directory: NativeDirectory,
    identity: ObjectId,
    stamp: Stamp,
}
impl OwnedSnapshot {
    fn capture(directory: NativeDirectory) -> std::result::Result<Self, EndpointError> {
        let identity = object(directory.file())?;
        let stamp = Stamp::new(&directory.file().metadata()?);
        Ok(Self {
            directory,
            identity,
            stamp,
        })
    }
    fn validate(&self) -> std::result::Result<(), EndpointError> {
        self.directory.validate(false)?;
        if object(self.directory.file())? != self.identity
            || Stamp::new(&self.directory.file().metadata()?) != self.stamp
        {
            return Err(EndpointError::Unstable);
        }
        Ok(())
    }
}

impl super::NativeOwnedNames {
    pub(in crate::connection) fn observation(&self) -> super::NativeOwnedObservation<'_> {
        super::NativeOwnedObservation {
            names: self,
            snapshots: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        }
    }
    pub(in crate::connection) fn charge(
        &self,
        count: u64,
    ) -> std::result::Result<(), EndpointError> {
        if self.epoch_lifetime.strong_count() == 0 || self.store_lifetime.strong_count() == 0 {
            return Err(EndpointError::Unverified);
        }
        self.budget
            .charge(count)
            .map_err(|()| EndpointError::LimitExceeded)
    }
    fn read_clue(
        &self,
        directory: &NativeDirectory,
        name: &CStr,
    ) -> std::result::Result<Vec<u8>, EndpointError> {
        let bytes = self.read_clue_bytes(directory, name)?;
        d::decode(&bytes).map_err(|_| EndpointError::Unverified)?;
        Ok(bytes)
    }
    fn read_clue_bytes(
        &self,
        directory: &NativeDirectory,
        name: &CStr,
    ) -> std::result::Result<Vec<u8>, EndpointError> {
        self.charge(0)?;
        let file = open_at(directory.file(), name, libc::O_RDONLY)?;
        destination_security::check_private_file(&file)?;
        let before = Stamp::new(&file.metadata()?);
        if before.length > d::MAX_BYTES as u64 {
            return Err(EndpointError::LimitExceeded);
        }
        let length = usize::try_from(before.length).map_err(|_| EndpointError::LimitExceeded)?;
        let mut bytes = vec![0; length + 1];
        self.charge(bytes.len() as u64)?;
        let count = file.read_at(&mut bytes, 0)?;
        if count != length || Stamp::new(&file.metadata()?) != before {
            return Err(EndpointError::Unstable);
        }
        bytes.truncate(count);
        Ok(bytes)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Exact namespace, descriptor and child identity checks are one fail-closed observer validation."
    )]
    fn validate_record(
        &self,
        root: &NativeDirectory,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> std::result::Result<OwnedSnapshot, EndpointError> {
        if record.native_kind() == Some(crate::connection::store::NativeOperationKind::DirectoryV1)
        {
            return self.validate_directory_record(root, record);
        }
        self.charge(0)?;
        root.validate(false)?;
        if object(root.file())? != self.root {
            return Err(EndpointError::IdentityChanged);
        }
        let name = record.name();
        let directory = root.child(OsStr::new(&name), false)?;
        let snapshot = OwnedSnapshot::capture(directory.clone())?;
        destination_security::check_private_file(directory.file())?;
        let (intent_bytes, stage_bytes) = record
            .descriptors()
            .map_err(|_| EndpointError::Unverified)?;
        let intent = d::decode(intent_bytes).map_err(|_| EndpointError::Unverified)?;
        let d::Body::Intent(body) = &intent.body else {
            return Err(EndpointError::Unverified);
        };
        if intent.header.namespace_name() != name
            || body.endpoints.get(usize::from(record.side())) != Some(&self.endpoint)
            || body
                .chains
                .get(usize::from(record.side()))
                .ok_or(EndpointError::Unverified)?
                .first()
                != Some(&descriptor_object(&self.root))
            || descriptor_object(&object(directory.file())?) != body.namespace
            || self.read_clue(&directory, c"intent")? != intent_bytes
        {
            return Err(EndpointError::IdentityChanged);
        }
        let staged = if let Some(bytes) = stage_bytes {
            if self.read_clue(&directory, c"staged-intent")? != bytes {
                return Err(EndpointError::IdentityChanged);
            }
            Some(d::decode(bytes).map_err(|_| EndpointError::Unverified)?)
        } else {
            None
        };
        if let Some(stage) = &staged {
            stage
                .joined(&intent, None, None)
                .map_err(|_| EndpointError::Unverified)?;
        }
        let readable = open_at(directory.file(), c".", libc::O_RDONLY | libc::O_DIRECTORY)?;
        if object(&readable)? != object(directory.file())? {
            return Err(EndpointError::IdentityChanged);
        }
        let mut names = std::collections::BTreeSet::new();
        for child in cap_std::fs::Dir::from_std_file(readable).entries()? {
            self.charge(0)?;
            let name = child?.file_name();
            if names.len() >= 6
                || !matches!(
                    name.as_bytes(),
                    b"intent"
                        | b"staged-intent"
                        | b"stage"
                        | b"displaced-intent"
                        | b"displaced"
                        | b"outcome"
                )
            {
                return Err(EndpointError::Unverified);
            }
            names.insert(name);
        }
        if (staged.is_some()
            && !names.contains(OsStr::new("stage"))
            && !names.contains(OsStr::new("outcome")))
            || (staged.is_none() && names.contains(OsStr::new("staged-intent")))
        {
            return Err(EndpointError::Unverified);
        }
        let displaced = if names.contains(OsStr::new("displaced-intent")) {
            let bytes = self.read_clue(&directory, c"displaced-intent")?;
            let value = d::decode(&bytes).map_err(|_| EndpointError::Unverified)?;
            if !matches!(value.body, d::Body::Displaced(_)) {
                return Err(EndpointError::Unverified);
            }
            value
                .joined(&intent, staged.as_ref(), None)
                .map_err(|_| EndpointError::Unverified)?;
            Some(value)
        } else {
            None
        };
        if names.contains(OsStr::new("stage")) {
            let Some(d::Descriptor {
                body: d::Body::Stage(stage),
                ..
            }) = &staged
            else {
                return Err(EndpointError::Unverified);
            };
            let file = open_at(directory.file(), c"stage", libc::O_PATH)?;
            if descriptor_object(&object(&file)?) != stage.object {
                return Err(EndpointError::IdentityChanged);
            }
        }
        if names.contains(OsStr::new("displaced")) {
            let Some(d::Descriptor {
                body: d::Body::Displaced(displaced),
                ..
            }) = &displaced
            else {
                return Err(EndpointError::Unverified);
            };
            let file = open_at(directory.file(), c"displaced", libc::O_PATH)?;
            if descriptor_object(&object(&file)?) != displaced.object {
                return Err(EndpointError::IdentityChanged);
            }
        } else if displaced.is_some() {
            return Err(EndpointError::Unverified);
        }
        if names.contains(OsStr::new("outcome")) {
            let bytes = self.read_clue(&directory, c"outcome")?;
            let value = d::decode(&bytes).map_err(|_| EndpointError::Unverified)?;
            if !matches!(value.body, d::Body::Outcome(_)) {
                return Err(EndpointError::Unverified);
            }
            value
                .joined(&intent, staged.as_ref(), displaced.as_ref())
                .map_err(|_| EndpointError::Unverified)?;
        }
        #[cfg(test)]
        after_owned_reads();
        directory.validate(false)?;
        if descriptor_object(&object(directory.file())?) != body.namespace {
            return Err(EndpointError::IdentityChanged);
        }
        snapshot.validate()?;
        root.validate(false)?;
        self.charge(0)?;
        Ok(snapshot)
    }
}
impl super::NativeOwnedObservation<'_> {
    pub(in crate::connection) fn charge(
        &self,
        count: u64,
    ) -> std::result::Result<(), EndpointError> {
        self.names.charge(count)
    }
    fn validate_record(
        &self,
        root: &NativeDirectory,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> std::result::Result<(), EndpointError> {
        let snapshot = self.names.validate_record(root, record)?;
        let mut snapshots = self.snapshots.borrow_mut();
        if let Some(previous) = snapshots.get(&record.name()) {
            previous.validate()?;
            if snapshot.identity != previous.identity || snapshot.stamp != previous.stamp {
                return Err(EndpointError::Unstable);
            }
        } else {
            if snapshots.len() >= 128 {
                return Err(EndpointError::LimitExceeded);
            }
            snapshots.insert(record.name(), snapshot);
        }
        Ok(())
    }
    pub(in crate::connection) fn validate_all(
        &self,
        root: &NativeDirectory,
    ) -> std::result::Result<(), EndpointError> {
        if self.names.records.len() > 128 {
            return Err(EndpointError::LimitExceeded);
        }
        self.charge(0)?;
        if object(root.file())? != self.names.root {
            return Err(EndpointError::IdentityChanged);
        }
        for record in &self.names.records {
            self.validate_record(root, record)?;
        }
        // Recheck earlier records after later records, just as the ordinary
        // observer rechecks early directory snapshots after later siblings.
        for snapshot in self.snapshots.borrow().values() {
            self.charge(0)?;
            snapshot.validate()?;
        }
        Ok(())
    }
    pub(in crate::connection) fn excludes(
        &self,
        root: &NativeDirectory,
        name: &OsStr,
    ) -> std::result::Result<bool, EndpointError> {
        let Some(record) = self
            .names
            .records
            .iter()
            .find(|record| OsStr::new(&record.name()) == name)
        else {
            // A reserved-looking but unregistered name is visible evidence of
            // unadopted work, never an exclusion or authority to mutate it.
            return if name.as_bytes().starts_with(b".qos-operation-") {
                Err(EndpointError::Unverified)
            } else {
                Ok(false)
            };
        };
        self.validate_record(root, record)?;
        Ok(true)
    }
}

impl<'e> NativeEpoch<'e> {
    fn sealed_check(
        &mut self,
        live: &LiveOperation,
        private: &PrivateNamespace,
        sealed: &super::SealedDisplacement,
    ) -> Result<()> {
        self.connected_check(live)?;
        if !sealed.lifetime.ptr_eq(live.lifetime())
            || sealed.displaced.epoch != self.nonce
            || sealed.displaced.actual != sealed.guard.actual
            || self.ep(object(sealed.displaced.namespace.file()))? != private.identity
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.validate_guard(&sealed.guard, true)?;
        let fact = sealed
            .recovery
            .value()
            .validate(live, self.nonce)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        if fact.object != descriptor_object(&sealed.guard.actual)
            || fact.content != descriptor_content(&self.capture(&sealed.guard)?)
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.namespace_check(private)
    }
    fn replacement_vacancy(
        &mut self,
        live: &LiveOperation,
        target: &ParentGuard,
        private: &PrivateNamespace,
        sealed: &super::SealedDisplacement,
    ) -> Result<PublicationVacancy> {
        self.sealed_check(live, private, sealed)?;
        if target.side != Side::Target || !self.relocated {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.check_absent(target)?;
        Ok(PublicationVacancy::Sealed {
            epoch: self.nonce,
            parent: self.ep(object(target.directory.file()))?,
            name: target.name.clone(),
        })
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    #[cfg(test)]
    pub(in crate::connection::transaction) fn publish_connected<'a>(
        &'a mut self,
        live: &LiveOperation,
        source: &'a StableFileGuard,
        target: &'a ParentGuard,
        private: &'a PrivateNamespace,
        staged: StagedFile,
        absent: Option<AbsentName>,
        displaced: Option<super::SealedDisplacement>,
        step: StepAdmission,
    ) -> Result<super::NativeOutcome<'a, 'e>> {
        self.admitted(live, &step, StepKind::Publish)?;
        self.namespace_check(private)?;
        if let Some(sealed) = &displaced {
            self.sealed_check(live, private, sealed)?;
        }
        if self.ep(object(staged.namespace.file()))? != private.identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.admitted(live, &step, StepKind::Publish)?;
        let vacancy = match (absent, displaced.as_ref()) {
            (Some(absent), None) => PublicationVacancy::Original(absent),
            (None, Some(sealed)) => self.replacement_vacancy(live, target, private, sealed)?,
            _ => return Err(self.failure(EndpointError::Unverified)),
        };
        let publication = self.publish_to_vacancy(staged, target, vacancy, source)?;
        self.outcome_from_publication(
            live,
            source,
            target,
            private,
            publication,
            displaced,
            None,
            &step,
        )
    }
    fn outcome_from_publication<'a>(
        &'a mut self,
        live: &LiveOperation,
        source: &'a StableFileGuard,
        target: &'a ParentGuard,
        private: &'a PrivateNamespace,
        publication: NativePublication,
        displaced: Option<super::SealedDisplacement>,
        existing: Option<Vec<u8>>,
        step: &StepAdmission,
    ) -> Result<super::NativeOutcome<'a, 'e>> {
        // Keep admission through native barriers, final rereads and outcome fsync.
        self.admitted(live, step, StepKind::Publish)?;
        let (mut header, intent_body, intent_bytes) = private
            .descriptor
            .clone()
            .ok_or_else(|| self.failure(EndpointError::Unverified))?;
        let intent =
            d::decode(&intent_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let stage = d::decode(
            publication
                .stage_descriptor
                .as_ref()
                .ok_or_else(|| self.failure(EndpointError::Unverified))?,
        )
        .map_err(|_| self.failure(EndpointError::Unverified))?;
        let source_state = self.state(source)?;
        let target_state = self.state(&publication.guard)?;
        let states = if intent_body.direction == 0 {
            [source_state, target_state]
        } else {
            [target_state, source_state]
        };
        let mut references = Vec::new();
        let displaced_descriptor = if let Some(sealed) = &displaced {
            self.sealed_check(live, private, sealed)?;
            let fact = sealed
                .recovery
                .value()
                .validate(live, self.nonce)
                .map_err(|_| self.failure(EndpointError::Unverified))?;
            references.push(d::Reference {
                slot: fact.recovery.slot,
                side: fact.side,
                role: fact.role,
                binding: fact
                    .binding_digest()
                    .map_err(|_| self.failure(EndpointError::Unverified))?,
            });
            Some(
                d::decode(
                    sealed
                        .displaced
                        .descriptor
                        .as_ref()
                        .ok_or_else(|| self.failure(EndpointError::Unverified))?,
                )
                .map_err(|_| self.failure(EndpointError::Unverified))?,
            )
        } else {
            None
        };
        header.progress = live.revision();
        let body = d::Body::Outcome(d::Outcome {
            intent: intent.digest,
            stage: Some(stage.digest),
            displaced: displaced_descriptor.as_ref().map(|value| value.digest),
            states,
            kind: u8::from(displaced.is_some()),
            barriers: 3,
            recovery: references,
            sequence: 1,
        });
        let reconstructed = existing.is_some();
        let descriptor = if let Some(bytes) = existing {
            self.read_descriptor(&private.directory, c"outcome", &bytes)?;
            if d::decode(&bytes)
                .map_err(|_| self.failure(EndpointError::Unverified))?
                .body
                != body
            {
                return Err(self.failure(EndpointError::Unstable));
            }
            bytes
        } else {
            let bytes =
                d::encode(&header, &body).map_err(|_| self.failure(EndpointError::Unverified))?;
            self.write_descriptor(&private.directory, c"outcome", &bytes)?;
            bytes
        };
        d::decode(&descriptor)
            .and_then(|value| value.joined(&intent, Some(&stage), displaced_descriptor.as_ref()))
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let mut proof = super::NativeOutcome {
            reconstructed,
            epoch: self,
            source,
            target,
            namespace: private,
            publication,
            displaced,
            descriptor,
        };
        proof
            .validate(live)
            .map_err(|_| proof.epoch.failure(EndpointError::Unverified))?;
        Ok(proof)
    }
}
impl super::NativeOutcome<'_, '_> {
    pub(in crate::connection) fn reconstructed(&self) -> bool {
        self.reconstructed
    }
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.connected_check(live)?;
            self.epoch.namespace_check(self.namespace)?;
            self.epoch.parent_check(self.target)?;
            self.epoch.validate_guard(self.source, true)?;
            self.epoch.validate_guard(&self.publication.guard, true)?;
            if self.publication.epoch != self.epoch.nonce {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            if let Some(sealed) = &self.displaced {
                self.epoch.sealed_check(live, self.namespace, sealed)?;
            }
            let descriptor = d::decode(&self.descriptor)
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            let d::Body::Outcome(body) = descriptor.body else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            let Some(direction) = self
                .epoch
                .connected
                .as_ref()
                .map(|binding| binding.direction)
            else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            if body.states.get(direction) != Some(&self.epoch.state(self.source)?)
                || body.states.get(1 - direction)
                    != Some(&self.epoch.state(&self.publication.guard)?)
            {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            self.epoch
                .read_descriptor(&self.namespace.directory, c"outcome", &self.descriptor)?;
            self.epoch.parent_check(self.target)?;
            self.epoch.validate_guard(self.source, true)?;
            self.epoch.validate_guard(&self.publication.guard, true)?;
            self.epoch.witness_checkpoint()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn facts(
        &self,
    ) -> std::result::Result<(u32, Vec<u8>, Option<Vec<u8>>), StoreError> {
        let value = d::decode(&self.descriptor).map_err(|_| StoreError::Corrupt)?;
        Ok((
            u32::try_from(value.header.attempt).map_err(|_| StoreError::Corrupt)?,
            self.descriptor.clone(),
            self.displaced
                .as_ref()
                .and_then(|sealed| sealed.displaced.descriptor.clone()),
        ))
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
}

impl<'e> NativeEpoch<'e> {
    #[cfg(test)]
    pub(in crate::connection::transaction) fn deletion_outcome<'a>(
        &'a mut self,
        live: &LiveOperation,
        source: &'a ParentGuard,
        target: &'a ParentGuard,
        private: &'a PrivateNamespace,
        displaced: super::SealedDisplacement,
        step: StepAdmission,
    ) -> Result<super::NativeDeletion<'a, 'e>> {
        self.deletion_outcome_with_existing(live, source, target, private, displaced, None, step)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    fn deletion_outcome_with_existing<'a>(
        &'a mut self,
        live: &LiveOperation,
        source: &'a ParentGuard,
        target: &'a ParentGuard,
        private: &'a PrivateNamespace,
        displaced: super::SealedDisplacement,
        existing: Option<Vec<u8>>,
        step: StepAdmission,
    ) -> Result<super::NativeDeletion<'a, 'e>> {
        if !self.deletion_scope {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.admitted(live, &step, StepKind::Publish)?;
        self.sealed_check(live, private, &displaced)?;
        self.check_absent(source)?;
        self.check_absent(target)?;
        let (mut header, intent_body, intent_bytes) = private
            .descriptor
            .clone()
            .ok_or_else(|| self.failure(EndpointError::Unverified))?;
        if !matches!(intent_body.decision, 3 | 4)
            || source.path.as_ref() != Some(&live.intent().path)
            || target.path.as_ref() != Some(&live.intent().path)
            || source.side != Side::Source
            || target.side != Side::Target
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let intent =
            d::decode(&intent_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let relocated = d::decode(
            displaced
                .displaced
                .descriptor
                .as_ref()
                .ok_or_else(|| self.failure(EndpointError::Unverified))?,
        )
        .map_err(|_| self.failure(EndpointError::Unverified))?;
        let fact = displaced
            .recovery
            .value()
            .validate(live, self.nonce)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let reference = d::Reference {
            slot: fact.recovery.slot,
            side: fact.side,
            role: fact.role,
            binding: fact
                .binding_digest()
                .map_err(|_| self.failure(EndpointError::Unverified))?,
        };
        header.progress = live.revision();
        let body = d::Body::Outcome(d::Outcome {
            intent: intent.digest,
            stage: None,
            displaced: Some(relocated.digest),
            states: [d::State::Absent, d::State::Absent],
            kind: 2,
            barriers: 3,
            recovery: vec![reference],
            sequence: 1,
        });
        let reconstructed = existing.is_some();
        let descriptor = if let Some(bytes) = existing {
            self.read_descriptor(&private.directory, c"outcome", &bytes)?;
            if d::decode(&bytes)
                .map_err(|_| self.failure(EndpointError::Unverified))?
                .body
                != body
            {
                return Err(self.failure(EndpointError::Unstable));
            }
            bytes
        } else {
            d::encode(&header, &body).map_err(|_| self.failure(EndpointError::Unverified))?
        };
        d::decode(&descriptor)
            .and_then(|value| value.joined(&intent, None, Some(&relocated)))
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        self.barriers(&target.directory, &private.directory)?;
        self.admitted(live, &step, StepKind::Publish)?;
        if !reconstructed {
            self.write_descriptor(&private.directory, c"outcome", &descriptor)?;
        }
        let mut proof = super::NativeDeletion {
            reconstructed,
            epoch: self,
            source,
            target,
            namespace: private,
            displaced,
            descriptor,
        };
        proof
            .validate(live)
            .map_err(|_| proof.epoch.failure(EndpointError::Unverified))?;
        Ok(proof)
    }
}
impl super::NativeDeletion<'_, '_> {
    pub(in crate::connection) fn reconstructed(&self) -> bool {
        self.reconstructed
    }
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.connected_check(live)?;
            self.epoch
                .sealed_check(live, self.namespace, &self.displaced)?;
            self.epoch.check_absent(self.source)?;
            self.epoch.check_absent(self.target)?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"outcome", &self.descriptor)?;
            self.epoch.parent_check(self.source)?;
            self.epoch.parent_check(self.target)?;
            self.epoch.check_absent(self.source)?;
            self.epoch.check_absent(self.target)?;
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn facts(
        &self,
    ) -> std::result::Result<(u32, Vec<u8>, Option<Vec<u8>>), StoreError> {
        let value = d::decode(&self.descriptor).map_err(|_| StoreError::Corrupt)?;
        Ok((
            u32::try_from(value.header.attempt).map_err(|_| StoreError::Corrupt)?,
            self.descriptor.clone(),
            self.displaced.displaced.descriptor.clone(),
        ))
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
}

fn observe_comparison(
    epoch: &mut NativeEpoch<'_>,
    store: &crate::connection::store::ConnectionStore,
    live: &LiveOperation,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<Option<crate::connection::model::EntryState>, StoreError> {
    use crate::connection::model::Observation;
    store.check_native(live)?;
    let source_names = epoch
        .owned_names(live, Side::Source, store.native_namespace_records(live)?)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let target_names = epoch
        .owned_names(live, Side::Target, store.native_namespace_records(live)?)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let source = endpoint::observe_connected(epoch.source, &source_names, cancel);
    let target = endpoint::observe_connected(epoch.target, &target_names, cancel);
    if !matches!(source.observation(), Observation::Complete)
        || !matches!(target.observation(), Observation::Complete)
        || source.endpoint().identity != epoch.identities[0]
        || target.endpoint().identity != epoch.identities[1]
    {
        return Err(StoreError::RecoveryRequired);
    }
    let path = &live.intent().path;
    if source.entries().get(path) != target.entries().get(path) {
        return Err(StoreError::RecoveryRequired);
    }
    epoch
        .connected_check(live)
        .map_err(|_| StoreError::RecoveryRequired)?;
    Ok(source.entries().get(path).cloned())
}
impl super::CompletionEvidence for super::NativeOutcome<'_, '_> {
    #[cfg(test)]
    fn revoke_for_test(&mut self) {
        self.epoch.leases.revoke();
    }

    fn fresh(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<Option<crate::connection::model::EntryState>, StoreError> {
        let result = (|| {
            self.validate(live)?;
            let value = observe_comparison(self.epoch, store, live, cancel)?;
            self.validate(live)?;
            Ok(value)
        })();
        if result.is_err() && self.epoch.ancestors.is_some() {
            self.epoch.failure(EndpointError::Unstable);
        }
        result
    }
    fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
}
impl super::CompletionEvidence for super::NativeDeletion<'_, '_> {
    #[cfg(test)]
    fn revoke_for_test(&mut self) {
        self.epoch.leases.revoke();
    }

    fn fresh(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<Option<crate::connection::model::EntryState>, StoreError> {
        self.validate(live)?;
        let value = observe_comparison(self.epoch, store, live, cancel)?;
        self.validate(live)?;
        Ok(value)
    }
    fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
}
impl super::NativeOutcome<'_, '_> {
    pub(in crate::connection::transaction) fn comparison<'a>(
        &'a mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::VerifiedComparison<'a>, StoreError> {
        let existing_ancestors = self.epoch.comparison_ancestors(store, live, cancel)?;
        let value = super::CompletionEvidence::fresh(self, store, live, cancel)?;
        Ok(super::VerifiedComparison {
            existing_ancestors,
            evidence: self,
            id: live.id(),
            revision: live.revision(),
            fence: live.fence(),
            lifetime: live.lifetime().clone(),
            value,
        })
    }
}
impl super::NativeDeletion<'_, '_> {
    pub(in crate::connection::transaction) fn comparison<'a>(
        &'a mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::VerifiedComparison<'a>, StoreError> {
        let existing_ancestors = None;
        let value = super::CompletionEvidence::fresh(self, store, live, cancel)?;
        Ok(super::VerifiedComparison {
            existing_ancestors,
            evidence: self,
            id: live.id(),
            revision: live.revision(),
            fence: live.fence(),
            lifetime: live.lifetime().clone(),
            value,
        })
    }
}
impl super::VerifiedComparison<'_> {
    pub(in crate::connection) fn existing_ancestors(
        &self,
    ) -> Option<&crate::connection::store::ExistingAncestorPatchV1> {
        self.existing_ancestors.as_ref()
    }
    #[cfg(test)]
    pub(in crate::connection) fn revoke_for_test(&mut self) {
        self.evidence.revoke_for_test();
    }

    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        if self.id != live.id()
            || self.revision != live.revision()
            || self.fence != live.fence()
            || !self.lifetime.ptr_eq(live.lifetime())
            || self.lifetime.strong_count() == 0
        {
            return Err(StoreError::StaleRevision);
        }
        if self.evidence.fresh(store, live, cancel)? != self.value {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    pub(in crate::connection) fn value(&self) -> Option<&crate::connection::model::EntryState> {
        self.value.as_ref()
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.evidence.deadline()
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn begin_recovery(
        context: &crate::connection::store::RecoveryContext,
        source: &'e PinnedEndpoint,
        target: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> Result<Self> {
        if context.native_kind().is_some() {
            return Err(Failure {
                _cause: EndpointError::Unverified,
                _boundary: Boundary::Untouched,
                _os_error: None,
            });
        }
        Self::begin_recovery_mode(context, source, target, cancel, false)
    }
    fn begin_recovery_mode(
        context: &crate::connection::store::RecoveryContext,
        source: &'e PinnedEndpoint,
        target: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
        capture_only: bool,
    ) -> Result<Self> {
        use crate::connection::model::{Decision, Side as ModelSide};
        let failure = || Failure {
            _cause: EndpointError::Unverified,
            _boundary: Boundary::Untouched,
            _os_error: None,
        };
        if !(context.action().is_reconciliation()
            || capture_only
                && matches!(
                    context.action(),
                    crate::connection::store::ExecutionAction::Captured
                ))
        {
            return Err(failure());
        }
        let direction = if capture_only {
            0
        } else {
            match context.intent().decision {
                Decision::Propagate {
                    from: ModelSide::Destination,
                }
                | Decision::RecoverDeletion {
                    at: ModelSide::Source,
                } => 1,
                _ => 0,
            }
        };
        if cancel.load(std::sync::atomic::Ordering::Acquire)
            || (!context.recovery_only()
                && context
                    .gate()
                    .stopped()
                    .load(std::sync::atomic::Ordering::Acquire))
            || context.lifetime().strong_count() == 0
            || Some(&source.identity()) != context.endpoints().get(direction)
            || Some(&target.identity()) != context.endpoints().get(1 - direction)
        {
            return Err(failure());
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&context.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            source,
            target,
            OperationBinding {
                _connection: context.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: context.admission(),
                _fence: context.fence(),
            },
            Arc::clone(context.reads()),
        )?;
        epoch.cancel_read = Some(cancel);
        if context.native_kind() == Some(crate::connection::store::NativeOperationKind::DirectoryV1)
        {
            epoch.directory_requests = Some(Arc::new(std::sync::atomic::AtomicU16::new(0)));
        }
        if !context.recovery_only() {
            epoch.stop_read = Some(context.gate());
        }
        epoch.connected = Some(ConnectedBinding {
            selected_action: None,
            capture_only,
            id: context.id(),
            lifetime: context.lifetime().clone(),
            admission: context.admission(),
            fence: context.fence(),
            direction,
            intent: context.intent().clone(),
        });
        if !capture_only
            && !context.recovery_only()
            && matches!(
                context.intent().decision,
                Decision::VerifyEqual | Decision::Propagate { .. }
            )
        {
            if context.native_kind()
                == Some(crate::connection::store::NativeOperationKind::DirectoryV1)
            {
                epoch.directory_requests = Some(Arc::new(std::sync::atomic::AtomicU16::new(0)));
                epoch.prove_directory_ancestors(context.native_identity())?;
            } else {
                epoch.prove_existing_ancestors(context.native_identity())?;
            }
        }
        Ok(epoch)
    }
    fn recovery_check(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> Result<()> {
        if self.directory_requests.is_some()
            != (context.native_kind()
                == Some(crate::connection::store::NativeOperationKind::DirectoryV1))
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        if !self.connected.as_ref().is_some_and(|binding| {
            (context.action().is_reconciliation()
                || (binding.capture_only
                    && matches!(
                        context.action(),
                        crate::connection::store::ExecutionAction::Captured
                    ))
                || binding
                    .selected_action
                    .as_ref()
                    .is_some_and(|action| action == context.action()))
                && binding.id == context.id()
                && binding.lifetime.ptr_eq(context.lifetime())
                && binding.lifetime.strong_count() != 0
                && binding.admission == context.admission()
                && binding.fence == context.fence()
                && &binding.intent == context.intent()
        }) {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.validate_existing_ancestors(false)?;
        self.check()
    }
    fn owned_recovery_names(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        side: Side,
        records: Vec<crate::connection::store::NativeNamespaceRecord>,
    ) -> Result<super::NativeOwnedNames> {
        self.recovery_check(context)?;
        if records.len() > 128 {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        for record in &records {
            record
                .validate_recovery(context)
                .map_err(|_| self.failure(EndpointError::Unverified))?;
        }
        let Some(direction) = self.connected.as_ref().map(|binding| binding.direction) else {
            return Err(self.failure(EndpointError::Unverified));
        };
        let model_side = if side == Side::Source {
            direction
        } else {
            1 - direction
        } as u8;
        let root = if side == Side::Source {
            self.source_root.clone()
        } else {
            self.target_root.clone()
        };
        Ok(super::NativeOwnedNames {
            records: records
                .into_iter()
                .filter(|record| record.side() == model_side)
                .collect(),
            root: self.ep(object(root.file()))?,
            endpoint: descriptor_endpoint(if side == Side::Source {
                self.identities[0]
            } else {
                self.identities[1]
            }),
            budget: Arc::clone(&self.read_budget),
            epoch_lifetime: Arc::downgrade(&self.lifetime),
            directory_requests: self
                .directory_requests
                .as_ref()
                .map(|count| (context.id(), Arc::clone(count))),
            store_lifetime: context.lifetime().clone(),
        })
    }
    /// A separate fresh bounded observation after commit; never grants mutation authority.
    pub(in crate::connection::transaction) fn final_comparison(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<bool, StoreError> {
        use crate::connection::model::Observation;
        store.check_final_context(context)?;
        self.recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let source_names = self
            .owned_recovery_names(
                context,
                Side::Source,
                store.recovery_namespace_records(context)?,
            )
            .map_err(|_| StoreError::RecoveryRequired)?;
        let target_names = self
            .owned_recovery_names(
                context,
                Side::Target,
                store.recovery_namespace_records(context)?,
            )
            .map_err(|_| StoreError::RecoveryRequired)?;
        let source = endpoint::observe_connected(self.source, &source_names, cancel);
        let target = endpoint::observe_connected(self.target, &target_names, cancel);
        if !matches!(source.observation(), Observation::Complete)
            || !matches!(target.observation(), Observation::Complete)
        {
            return Err(StoreError::RecoveryRequired);
        }
        let baseline = store.load_recovery_baseline(context, self.deadline, cancel)?;
        self.recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        store.check_final_context(context)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        Ok(source.entries() == target.entries() && baseline.entries() == Some(source.entries()))
    }
}

impl<'e> NativeEpoch<'e> {
    /// Reacquire two real leases and inspect the exact registered addition. No
    /// serialized session, epoch, receipt stage or matching digest admits work.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    pub(in crate::connection::transaction) fn inspect_regular(
        mut self,
        context: &crate::connection::store::RecoveryContext,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> Result<super::RecoveredEvidence<'e>> {
        self.recovery_check(context)?;
        record
            .validate_recovery(context)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let (intent_bytes, stage_bytes) = record
            .descriptors()
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let intent_bytes = intent_bytes.to_vec();
        let stage_bytes = stage_bytes
            .ok_or_else(|| self.failure(EndpointError::Unverified))?
            .to_vec();
        let intent =
            d::decode(&intent_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let stage = d::decode(&stage_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        stage
            .joined(&intent, None, None)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let (d::Body::Intent(body), d::Body::Stage(staged)) = (&intent.body, &stage.body) else {
            return Err(self.failure(EndpointError::Unverified));
        };
        if stage.profile != d::Profile::CreatedRegular {
            return Err(self.failure(EndpointError::Unverified));
        }
        let direction = usize::from(body.direction);
        if !matches!(body.decision, 1 | 2)
            || direction > 1
            || record.side() != (1 - direction) as u8
            || intent.header.connection != context.id().connection.0
            || intent.header.operation != context.id().serial
            || intent.header.admission != context.admission()
            || intent.header.baseline != context.baseline()
            || intent.header.fence > context.fence()
            || (!context.recovery_only() && intent.header.fence != context.fence())
            || intent.header.namespace_name() != record.name()
            || body.path != context.intent().path
            || body.endpoints != context.endpoints().map(descriptor_endpoint)
            || !Self::matches_plan(&body.expected[0], context.intent().source_before.as_ref())
            || !Self::matches_plan(
                &body.expected[1],
                context.intent().destination_before.as_ref(),
            )
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let source_parent = self.pin_parent(Side::Source, &body.path)?;
        let target = self.pin_parent(Side::Target, &body.path)?;
        if source_parent
            .chain
            .iter()
            .map(descriptor_object)
            .collect::<Vec<_>>()
            != *body
                .chains
                .get(direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
            || target
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *body
                    .chains
                    .get(1 - direction)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let name =
            CString::new(record.name()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        let witness = self.io(witness::NamespaceWitness::new(
            &self.target_root.clone(),
            &target,
            name,
            false,
        ))?;
        self.witness = Some(witness);
        let source = self.guard_regular(&source_parent)?;
        if self.state(&source)?
            != *body
                .expected
                .get(direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let name =
            CString::new(record.name()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        let directory = self.ep(self
            .target_root
            .child(OsStr::from_bytes(name.to_bytes()), true))?;
        self.witness_watch_namespace(&directory)?;
        let identity = self.ep(object(directory.file()))?;
        if descriptor_object(&identity) != body.namespace {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let private = PrivateNamespace {
            directory,
            _name: name,
            epoch: self.nonce,
            identity,
            intended_parent: self.ep(object(target.directory.file()))?,
            intended_name: target.name.clone(),
            descriptor: Some((intent.header.clone(), body.clone(), intent_bytes.clone())),
        };
        self.namespace_check(&private)?;
        self.read_descriptor(&private.directory, c"intent", &intent_bytes)?;
        self.read_descriptor(&private.directory, c"staged-intent", &stage_bytes)?;
        let shape = self.regular_names(&private)?;
        super::witness::regular_restart(
            [
                shape.stage,
                shape.displaced,
                shape.displaced_intent,
                shape.outcome,
            ],
            body.expected.get(1 - direction) != Some(&d::State::Absent),
        )
        .map_err(|_| self.failure(EndpointError::Unverified))?;
        let mut inventory = vec![b"intent".as_slice(), b"staged-intent"];
        if shape.stage {
            inventory.push(b"stage");
        }
        if shape.displaced {
            inventory.push(b"displaced");
        }
        if shape.displaced_intent {
            inventory.push(b"displaced-intent");
        }
        if shape.outcome {
            inventory.push(b"outcome");
        }
        let result = self
            .witness
            .as_mut()
            .ok_or_else(|| Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.boundary,
                _os_error: None,
            })?
            .set_inventory(&inventory);
        self.io(result)?;
        let staged_at_private = shape.stage;
        let (displaced, displaced_descriptor) = if shape.displaced {
            let (displaced, guard) = self.reconstruct_displacement(
                &private,
                &target,
                &intent,
                Some(&stage),
                shape.displaced_intent,
            )?;
            let descriptor = d::decode(
                displaced
                    .descriptor
                    .as_ref()
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?,
            )
            .map_err(|_| self.failure(EndpointError::Unverified))?;
            (Some((displaced, guard)), Some(descriptor))
        } else {
            if shape.displaced_intent
                || (!shape.stage
                    && *body
                        .expected
                        .get(1 - direction)
                        .ok_or_else(|| self.failure(EndpointError::Unverified))?
                        != d::State::Absent)
            {
                return Err(self.failure(EndpointError::Unverified));
            }
            (None, None)
        };
        let outcome = if shape.outcome {
            let bytes = self.load_descriptor(&private.directory, c"outcome")?;
            let value = d::decode(&bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
            value
                .joined(&intent, Some(&stage), displaced_descriptor.as_ref())
                .map_err(|_| self.failure(EndpointError::Unverified))?;
            if !matches!(&value.body, d::Body::Outcome(body) if body.kind == u8::from(displaced.is_some()))
                || record.outcome().is_some_and(|canonical| canonical != bytes)
            {
                return Err(self.failure(EndpointError::Unverified));
            }
            Some(bytes)
        } else {
            if record.outcome().is_some() {
                return Err(self.failure(EndpointError::Unverified));
            }
            None
        };
        let target_guard = if shape.stage
            && displaced.is_none()
            && *body
                .expected
                .get(1 - direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
                != d::State::Absent
        {
            let guard = self.guard_regular(&target)?;
            if self.state(&guard)?
                != *body
                    .expected
                    .get(1 - direction)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?
            {
                return Err(self.failure(EndpointError::Unstable));
            }
            Some(guard)
        } else {
            None
        };
        let stage_parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"stage".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
            chain: Vec::new(),
            path: None,
        };
        let guard = if staged_at_private {
            if target_guard.is_none() {
                self.check_absent(&target)?;
            }
            self.guard_regular(&stage_parent)?
        } else {
            self.guard_regular(&target)?
        };
        let bytes = self.capture(&guard)?;
        if guard.stamp.mode
            != (libc::S_IFREG | CreatedMode::from_executable(staged.content.executable).bits())
            || descriptor_object(&guard.actual) != staged.object
            || descriptor_content(&bytes) != staged.content
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let binding = SourceBinding {
            file: Arc::clone(&source.file),
            stamp: source.stamp.clone(),
            executable: source.executable,
            parent: source.parent.directory.clone(),
            name: source.parent.name.clone(),
            actual: source.actual.clone(),
            side: Side::Source,
        };
        self.barriers(&private.directory, &target.directory)?;
        if shape.displaced && !shape.displaced_intent {
            let (actual, guard) = displaced
                .as_ref()
                .ok_or_else(|| self.failure(EndpointError::Unverified))?;
            let bytes = actual
                .descriptor
                .as_ref()
                .ok_or_else(|| self.failure(EndpointError::Unverified))?;
            self.witness_checkpoint()?;
            let permit = self.accounting_repair_permit(context, &private, guard, bytes)?;
            self.repair_displaced_descriptor(context, &private, guard, bytes, permit)?;
        }
        self.namespace_created = true;
        let (staged, published) = if staged_at_private {
            (
                Some(StagedFile {
                    source: binding,
                    guard,
                    bytes,
                    namespace: private.directory.clone(),
                    intended_parent: private.intended_parent.clone(),
                    intended_name: private.intended_name.clone(),
                    descriptor: Some(stage_bytes),
                }),
                None,
            )
        } else {
            self.boundary = Boundary::Published;
            (
                None,
                Some(NativePublication {
                    _source: binding,
                    epoch: self.nonce,
                    _identity: guard.actual.clone(),
                    _bytes: bytes,
                    _parent: target.directory.clone(),
                    _name: target.name.clone(),
                    guard,
                    stage_descriptor: Some(stage_bytes),
                }),
            )
        };
        let mut evidence = super::RecoveredEvidence {
            epoch: self,
            context_revision: context.revision(),
            source,
            target,
            namespace: private,
            staged,
            published,
            outcome,
            target_guard,
            displaced,
        };
        evidence
            .validate(context)
            .map_err(|_| evidence.epoch.failure(EndpointError::Unverified))?;
        Ok(evidence)
    }
    fn load_descriptor(&mut self, directory: &NativeDirectory, name: &CStr) -> Result<Vec<u8>> {
        self.check()?;
        let file = self.io(open_at(directory.file(), name, libc::O_RDONLY))?;
        self.io(destination_security::check_private_file(&file))?;
        let before = Stamp::new(&self.io(file.metadata())?);
        if before.length > d::MAX_BYTES as u64 {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        let mut bytes = vec![
            0;
            usize::try_from(before.length)
                .map_err(|_| self.failure(EndpointError::LimitExceeded))?
                + 1
        ];
        self.charge(bytes.len() as u64)?;
        let read = self.io(file.read_at(&mut bytes, 0))?;
        if read as u64 != before.length || Stamp::new(&self.io(file.metadata())?) != before {
            return Err(self.failure(EndpointError::Unstable));
        }
        drop(file); // The following snapshot may use two transient observation fds.
        bytes.truncate(read);
        d::decode(&bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        if self.witness.is_some() {
            self.read_descriptor(directory, name, &bytes)?;
        }
        self.check()?;
        Ok(bytes)
    }
    fn regular_names(&mut self, private: &PrivateNamespace) -> Result<RegularNames> {
        self.namespace_names(private, true)
    }
    fn namespace_names(
        &mut self,
        private: &PrivateNamespace,
        has_stage: bool,
    ) -> Result<RegularNames> {
        self.namespace_check(private)?;
        let readable = self.io(open_at(
            private.directory.file(),
            c".",
            libc::O_RDONLY | libc::O_DIRECTORY,
        ))?;
        if self.ep(object(&readable))? != private.identity {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let entries = self.io(cap_std::fs::Dir::from_std_file(readable).entries())?;
        let mut names = std::collections::BTreeSet::new();
        for entry in entries {
            self.check()?;
            let name = self.io(entry)?.file_name();
            if names.len() >= 6
                || !matches!(
                    name.as_bytes(),
                    b"intent"
                        | b"staged-intent"
                        | b"stage"
                        | b"outcome"
                        | b"displaced"
                        | b"displaced-intent"
                )
            {
                return Err(self.failure(EndpointError::Unverified));
            }
            names.insert(name);
        }
        if !names.contains(OsStr::new("intent"))
            || names.contains(OsStr::new("staged-intent")) != has_stage
            || (!has_stage && names.contains(OsStr::new("stage")))
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.namespace_check(private)?;
        let staged = names.contains(OsStr::new("stage"));
        let outcome = names.contains(OsStr::new("outcome"));
        if staged && outcome {
            return Err(self.failure(EndpointError::Unverified));
        }
        Ok(RegularNames {
            stage: staged,
            outcome,
            displaced: names.contains(OsStr::new("displaced")),
            displaced_intent: names.contains(OsStr::new("displaced-intent")),
        })
    }
}
#[derive(PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "These are independent observed namespace-presence or epoch hazard facts, not mutually exclusive states."
)]
struct RegularNames {
    stage: bool,
    outcome: bool,
    displaced: bool,
    displaced_intent: bool,
}
impl<'e> super::RecoveredEvidence<'e> {
    fn expected_names(&self) -> RegularNames {
        RegularNames {
            stage: self.staged.is_some(),
            outcome: self.outcome.is_some(),
            displaced: self.displaced.is_some(),
            displaced_intent: self.displaced.is_some(),
        }
    }
    pub(in crate::connection) fn validate_live(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_contents()
    }
    pub(in crate::connection) fn prepared_facts(
        &self,
    ) -> std::result::Result<(u32, Vec<u8>, Vec<u8>), StoreError> {
        let (header, _, intent) = self
            .namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let stage = self
            .staged
            .as_ref()
            .and_then(|stage| stage.descriptor.clone())
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((
            u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?,
            intent.clone(),
            stage,
        ))
    }
    pub(in crate::connection) fn validate(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if context.revision() != self.context_revision {
            return Err(StoreError::StaleRevision);
        }
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_contents()
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep guarded source, namespace, displaced bytes and final ancestry revalidation in one ordered proof scope."
    )]
    fn validate_contents(&mut self) -> std::result::Result<(), StoreError> {
        let result = (|| {
            let (_, body, intent_bytes) = self
                .namespace
                .descriptor
                .clone()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"intent", &intent_bytes)?;
            if self.epoch.state(&self.source)?
                != *body
                    .expected
                    .get(usize::from(body.direction))
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
            {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            if self.epoch.regular_names(&self.namespace)? != self.expected_names() {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            let (guard, bytes) = match (&self.staged, &self.published) {
                (Some(stage), None) => {
                    if let Some(guard) = &self.target_guard {
                        if self.epoch.state(guard)?
                            != *body
                                .expected
                                .get(1 - usize::from(body.direction))
                                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                        {
                            return Err(self.epoch.failure(EndpointError::Unstable));
                        }
                    } else {
                        self.epoch.check_absent(&self.target)?;
                    }
                    (&stage.guard, stage.descriptor.as_deref())
                }
                (None, Some(published)) => {
                    (&published.guard, published.stage_descriptor.as_deref())
                }
                _ => return Err(self.epoch.failure(EndpointError::Unverified)),
            };
            let bytes = bytes.ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"staged-intent", bytes)?;
            let descriptor =
                d::decode(bytes).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            let d::Body::Stage(stage) = descriptor.body else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            if descriptor_object(&guard.actual) != stage.object
                || descriptor_content(&self.epoch.capture(guard)?) != stage.content
            {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            if let Some(bytes) = &self.outcome {
                self.epoch
                    .read_descriptor(&self.namespace.directory, c"outcome", bytes)?;
                let value =
                    d::decode(bytes).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
                let d::Body::Outcome(outcome) = value.body else {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                };
                let direction = usize::from(body.direction);
                if *outcome
                    .states
                    .get(direction)
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                    != self.epoch.state(&self.source)?
                    || *outcome
                        .states
                        .get(1 - direction)
                        .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                        != self.epoch.state(guard)?
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            if let Some((displaced, guard)) = &self.displaced {
                self.epoch.validate_guard(guard, true)?;
                let bytes = displaced
                    .descriptor
                    .as_ref()
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
                self.epoch.read_descriptor(
                    &self.namespace.directory,
                    c"displaced-intent",
                    bytes,
                )?;
                if displaced.actual != guard.actual
                    || descriptor_content(&self.epoch.capture(guard)?)
                        != match &*body
                            .expected
                            .get(1 - usize::from(body.direction))
                            .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                        {
                            d::State::Regular { content, .. } => content.clone(),
                            _ => return Err(self.epoch.failure(EndpointError::Unverified)),
                        }
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            self.epoch.parent_check(&self.target)?;
            self.epoch.validate_guard(&self.source, true)?;
            self.epoch.validate_guard(guard, true)?;
            self.epoch.namespace_check(&self.namespace)?;
            self.epoch.witness_checkpoint()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the one-shot admission or candidate even when validation only borrows its fields."
    )]
    #[cfg(test)]
    pub(in crate::connection::transaction) fn complete_addition<'a>(
        &'a mut self,
        live: &LiveOperation,
        step: StepAdmission,
    ) -> Result<super::NativeOutcome<'a, 'e>> {
        if self.displaced.is_some() || self.target_guard.is_some() {
            return Err(self.epoch.failure(EndpointError::Unverified));
        }
        self.epoch.admitted(live, &step, StepKind::Publish)?;
        if live.revision() != self.context_revision {
            return Err(self.epoch.failure(EndpointError::Unverified));
        }
        if self.epoch.regular_names(&self.namespace)? != self.expected_names() {
            return Err(self.epoch.failure(EndpointError::Unstable));
        }
        let publication = if let Some(stage) = self.staged.take() {
            let absent = self.epoch.observe_absent(&self.target)?;
            self.epoch.publish_to_vacancy(
                stage,
                &self.target,
                PublicationVacancy::Original(absent),
                &self.source,
            )?
        } else {
            self.published
                .take()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
        };
        self.epoch
            .barriers(&self.namespace.directory, &self.target.directory)?;
        self.epoch.outcome_from_publication(
            live,
            &self.source,
            &self.target,
            &self.namespace,
            publication,
            None,
            self.outcome.take(),
            &step,
        )
    }
}

impl NativeEpoch<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained descriptor, object, byte and ancestor checks together during displacement reconstruction."
    )]
    fn reconstruct_displacement(
        &mut self,
        private: &PrivateNamespace,
        target: &ParentGuard,
        intent: &d::Descriptor,
        stage: Option<&d::Descriptor>,
        has_descriptor: bool,
    ) -> Result<(RelocatedObject, StableFileGuard)> {
        let d::Body::Intent(body) = &intent.body else {
            return Err(self.failure(EndpointError::Unverified));
        };
        let parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"displaced".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
            chain: Vec::new(),
            path: None,
        };
        let guard = self.guard_regular(&parent)?;
        let actual = self.capture(&guard)?;
        let d::State::Regular {
            object: expected,
            stamp,
            content,
        } = &*body
            .expected
            .get(1 - usize::from(body.direction))
            .ok_or_else(|| self.failure(EndpointError::Unverified))?
        else {
            return Err(self.failure(EndpointError::Unverified));
        };
        let current = descriptor_stamp(&guard.stamp);
        // rename can change ctime; every other portable snapshot field and the
        // kernel object generation must still match the approved actual object.
        if descriptor_object(&guard.actual) != *expected
            || descriptor_content(&actual) != *content
            || current.length != stamp.length
            || current.modified != stamp.modified
            || current.mode != stamp.mode
            || current.uid != stamp.uid
            || current.gid != stamp.gid
            || current.links != stamp.links
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        self.barriers(&private.directory, &target.directory)?;
        let bytes = if has_descriptor {
            let bytes = self.load_descriptor(&private.directory, c"displaced-intent")?;
            let value = d::decode(&bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
            value
                .joined(intent, stage, None)
                .map_err(|_| self.failure(EndpointError::Unverified))?;
            let d::Body::Displaced(displaced) = value.body else {
                return Err(self.failure(EndpointError::Unverified));
            };
            if displaced.object != descriptor_object(&guard.actual)
                || displaced.stamp != current
                || !displaced.matches_expected
                || displaced.barriers != 3
            {
                return Err(self.failure(EndpointError::Unstable));
            }
            bytes
        } else {
            let mut header = intent.header.clone();
            header.epoch = self.nonce;
            let body = d::Body::Displaced(d::Displaced {
                intent: intent.digest,
                stage: stage.map(|value| value.digest),
                object: descriptor_object(&guard.actual),
                stamp: current,
                matches_expected: true,
                side: 1 - body.direction,
                parent: descriptor_object(&private.intended_parent),
                leaf: NativeName::Unix(private.intended_name.to_bytes().to_vec()),
                barriers: 3,
            });
            let bytes =
                d::encode(&header, &body).map_err(|_| self.failure(EndpointError::Unverified))?;
            d::decode(&bytes)
                .and_then(|value| value.joined(intent, stage, None))
                .map_err(|_| self.failure(EndpointError::Unverified))?;
            if self.witness.is_none() {
                self.write_descriptor(&private.directory, c"displaced-intent", &bytes)?;
            }
            bytes
        };
        self.validate_guard(&guard, true)?;
        self.namespace_check(private)?;
        self.parent_check(target)?;
        let retained = self.io(open_at(
            private.directory.file(),
            c"displaced",
            libc::O_PATH,
        ))?;
        if self.ep(object(&retained))? != guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.relocated = true;
        self.boundary = Boundary::DurableQuarantine;
        Ok((
            RelocatedObject {
                epoch: self.nonce,
                actual: guard.actual.clone(),
                retained,
                namespace: private.directory.clone(),
                original_parent: target.directory.clone(),
                _original_name: target.name.clone(),
                matches_guard: true,
                descriptor: Some(bytes),
            },
            guard,
        ))
    }
}
impl<'e> super::RecoveredEvidence<'e> {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    pub(in crate::connection::transaction) fn complete_regular<'a>(
        &'a mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeOutcome<'a, 'e>, StoreError> {
        use crate::connection::store::{RecoveryReason, RecoveryState};
        self.validate_live(live)?;
        if self.target_guard.is_some() {
            let prepared = store.recovered_prepared(live, self)?;
            let step = session.admit_step(store, live, StepKind::Relocate, cancel)?;
            let expected = self
                .target_guard
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            let displaced = self
                .epoch
                .relocate_connected(
                    live,
                    &self.target,
                    &self.namespace,
                    &self.source,
                    expected,
                    &prepared,
                    step,
                )
                .map_err(|_| StoreError::RecoveryRequired)?;
            let guard = self
                .epoch
                .guard_displaced(&displaced)
                .map_err(|_| StoreError::RecoveryRequired)?;
            self.displaced = Some((displaced, guard));
            self.target_guard = None;
        }
        let sealed = if let Some((displaced, guard)) = self.displaced.take() {
            let direction = self
                .epoch
                .connected
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?
                .direction;
            let side = if direction == 0 {
                crate::connection::model::Side::Destination
            } else {
                crate::connection::model::Side::Source
            };
            let expected = if direction == 0 {
                live.intent().destination_before.clone()
            } else {
                live.intent().source_before.clone()
            };
            let mut existing = None;
            for slot in store.native_recovery_slots(live)? {
                if slot.state == RecoveryState::Reserved {
                    store.retain_native_incomplete(live, slot.id)?;
                } else if slot.state == RecoveryState::BytesVerified {
                    if slot.original_side != side || slot.content != expected {
                        return Err(StoreError::RecoveryRequired);
                    }
                    if existing.is_none() {
                        existing = Some(slot.id);
                    }
                }
            }
            let reservation = if existing.is_none() {
                Some(store.reserve_native_recovery(live, side, RecoveryReason::DisplacedVersion)?)
            } else {
                None
            };
            let step = session.admit_step(store, live, StepKind::Preserve, cancel)?;
            let mut reader = self
                .epoch
                .recovery_source(live, &self.namespace, &displaced, &guard, step)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let bound = if let Some(id) = existing {
                store.rebind_native_recovery(live, id, &mut reader, cancel)?
            } else {
                store.preserve_native(
                    live,
                    reservation.ok_or(StoreError::RecoveryRequired)?,
                    &mut reader,
                    cancel,
                )?
            };
            drop(reader);
            Some(
                self.epoch
                    .seal_displacement(live, displaced, guard, &self.namespace, bound)
                    .map_err(|_| StoreError::RecoveryRequired)?,
            )
        } else {
            None
        };
        let step = session.admit_step(store, live, StepKind::Publish, cancel)?;
        self.epoch
            .admitted(live, &step, StepKind::Publish)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let publication = if let Some(stage) = self.staged.take() {
            let vacancy = if let Some(sealed) = &sealed {
                self.epoch
                    .replacement_vacancy(live, &self.target, &self.namespace, sealed)
                    .map_err(|_| StoreError::RecoveryRequired)?
            } else {
                PublicationVacancy::Original(
                    self.epoch
                        .observe_absent(&self.target)
                        .map_err(|_| StoreError::RecoveryRequired)?,
                )
            };
            self.epoch
                .publish_to_vacancy(stage, &self.target, vacancy, &self.source)
                .map_err(|_| StoreError::RecoveryRequired)?
        } else {
            self.published.take().ok_or(StoreError::RecoveryRequired)?
        };
        self.epoch
            .barriers(&self.namespace.directory, &self.target.directory)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.epoch
            .outcome_from_publication(
                live,
                &self.source,
                &self.target,
                &self.namespace,
                publication,
                sealed,
                self.outcome.take(),
                &step,
            )
            .map_err(|_| StoreError::RecoveryRequired)
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn verify_deletion_scope(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        use crate::connection::model::Observation;
        self.connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let names = self
            .owned_names(live, Side::Source, store.native_namespace_records(live)?)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let observed = endpoint::observe_connected(self.source, &names, cancel);
        if !matches!(observed.observation(), Observation::Complete)
            || observed.endpoint().identity != self.identities[0]
            || observed.entries().contains_key(&live.intent().path)
        {
            return Err(StoreError::RecoveryRequired);
        }
        self.connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.deletion_scope = true;
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    pub(in crate::connection::transaction) fn inspect_deletion(
        mut self,
        context: &crate::connection::store::RecoveryContext,
        record: &crate::connection::store::NativeNamespaceRecord,
        store: &crate::connection::store::ConnectionStore,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<super::RecoveredDeletion<'e>> {
        use crate::connection::model::Observation;
        self.recovery_check(context)?;
        record
            .validate_recovery(context)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let (intent_bytes, stage) = record
            .descriptors()
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let intent =
            d::decode(intent_bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        let d::Body::Intent(body) = &intent.body else {
            return Err(self.failure(EndpointError::Unverified));
        };
        let direction = usize::from(body.direction);
        if stage.is_some()
            || !matches!(body.decision, 3 | 4)
            || direction > 1
            || record.side() != (1 - direction) as u8
            || intent.header.connection != context.id().connection.0
            || intent.header.operation != context.id().serial
            || intent.header.admission != context.admission()
            || intent.header.baseline != context.baseline()
            || intent.header.fence > context.fence()
            || (!context.recovery_only() && intent.header.fence != context.fence())
            || intent.header.namespace_name() != record.name()
            || body.path != context.intent().path
            || body.endpoints != context.endpoints().map(descriptor_endpoint)
            || *body
                .expected
                .get(direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
                != d::State::Absent
            || !Self::matches_plan(&body.expected[0], context.intent().source_before.as_ref())
            || !Self::matches_plan(
                &body.expected[1],
                context.intent().destination_before.as_ref(),
            )
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let source = self.pin_parent(Side::Source, &body.path)?;
        let target = self.pin_parent(Side::Target, &body.path)?;
        if source
            .chain
            .iter()
            .map(descriptor_object)
            .collect::<Vec<_>>()
            != *body
                .chains
                .get(direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
            || target
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *body
                    .chains
                    .get(1 - direction)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.check_absent(&source)?;
        let name =
            CString::new(record.name()).map_err(|_| self.failure(EndpointError::Unverified))?;
        let directory = self.ep(self
            .target_root
            .child(OsStr::from_bytes(name.to_bytes()), true))?;
        let identity = self.ep(object(directory.file()))?;
        if descriptor_object(&identity) != body.namespace {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let private = PrivateNamespace {
            directory,
            _name: name,
            epoch: self.nonce,
            identity,
            intended_parent: self.ep(object(target.directory.file()))?,
            intended_name: target.name.clone(),
            descriptor: Some((intent.header.clone(), body.clone(), intent_bytes.to_vec())),
        };
        self.namespace_check(&private)?;
        self.read_descriptor(&private.directory, c"intent", intent_bytes)?;
        let shape = self.namespace_names(&private, false)?;
        let (displaced, target_guard) = if shape.displaced {
            self.check_absent(&target)?;
            (
                Some(self.reconstruct_displacement(
                    &private,
                    &target,
                    &intent,
                    None,
                    shape.displaced_intent,
                )?),
                None,
            )
        } else {
            if shape.displaced_intent || shape.outcome {
                return Err(self.failure(EndpointError::Unverified));
            }
            let guard = self.guard_regular(&target)?;
            if self.state(&guard)?
                != *body
                    .expected
                    .get(1 - direction)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?
            {
                return Err(self.failure(EndpointError::Unstable));
            }
            (None, Some(guard))
        };
        let outcome = if shape.outcome {
            let bytes = self.load_descriptor(&private.directory, c"outcome")?;
            let displaced = displaced
                .as_ref()
                .and_then(|(actual, _)| actual.descriptor.as_ref())
                .ok_or_else(|| self.failure(EndpointError::Unverified))?;
            let displaced =
                d::decode(displaced).map_err(|_| self.failure(EndpointError::Unverified))?;
            let value = d::decode(&bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
            value
                .joined(&intent, None, Some(&displaced))
                .map_err(|_| self.failure(EndpointError::Unverified))?;
            if !matches!(&value.body, d::Body::Outcome(body) if body.kind == 2 && body.states == [d::State::Absent, d::State::Absent])
                || record.outcome().is_some_and(|canonical| canonical != bytes)
            {
                return Err(self.failure(EndpointError::Unverified));
            }
            Some(bytes)
        } else {
            if record.outcome().is_some() {
                return Err(self.failure(EndpointError::Unverified));
            }
            None
        };
        self.barriers(&private.directory, &target.directory)?;
        let records = store
            .recovery_namespace_records(context)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let names = self.owned_recovery_names(context, Side::Source, records)?;
        let observed = endpoint::observe_connected(self.source, &names, cancel);
        if !matches!(observed.observation(), Observation::Complete)
            || observed.entries().contains_key(&body.path)
            || observed.endpoint().identity != self.identities[0]
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.deletion_scope = true;
        self.namespace_created = true;
        let mut proof = super::RecoveredDeletion {
            epoch: self,
            context_revision: context.revision(),
            source,
            target,
            namespace: private,
            target_guard,
            displaced,
            outcome,
        };
        proof
            .validate(context)
            .map_err(|_| proof.epoch.failure(EndpointError::Unverified))?;
        Ok(proof)
    }
}
impl<'e> super::RecoveredDeletion<'e> {
    pub(in crate::connection) fn validate(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if context.revision() != self.context_revision {
            return Err(StoreError::StaleRevision);
        }
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_contents()
    }
    pub(in crate::connection) fn validate_live(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_contents()
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    pub(in crate::connection) fn prepared_facts(
        &self,
    ) -> std::result::Result<(u32, Vec<u8>), StoreError> {
        let (header, _, intent) = self
            .namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((
            u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?,
            intent.clone(),
        ))
    }
    fn validate_contents(&mut self) -> std::result::Result<(), StoreError> {
        let result = (|| {
            if !self.epoch.deletion_scope {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            self.epoch.check_absent(&self.source)?;
            let (_, body, bytes) = self
                .namespace
                .descriptor
                .clone()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"intent", &bytes)?;
            let expected = RegularNames {
                stage: false,
                outcome: self.outcome.is_some(),
                displaced: self.displaced.is_some(),
                displaced_intent: self.displaced.is_some(),
            };
            if self.epoch.namespace_names(&self.namespace, false)? != expected {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            if let Some(guard) = &self.target_guard {
                if self.displaced.is_some()
                    || self.epoch.state(guard)?
                        != *body
                            .expected
                            .get(1 - usize::from(body.direction))
                            .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            } else {
                self.epoch.check_absent(&self.target)?;
            }
            if let Some((displaced, guard)) = &self.displaced {
                let bytes = displaced
                    .descriptor
                    .as_ref()
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
                self.epoch.read_descriptor(
                    &self.namespace.directory,
                    c"displaced-intent",
                    bytes,
                )?;
                let d::State::Regular { content, .. } = &*body
                    .expected
                    .get(1 - usize::from(body.direction))
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                else {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                };
                if guard.actual != displaced.actual
                    || descriptor_content(&self.epoch.capture(guard)?) != *content
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            if let Some(bytes) = &self.outcome {
                self.epoch
                    .read_descriptor(&self.namespace.directory, c"outcome", bytes)?;
            }
            self.epoch.check_absent(&self.source)?;
            self.epoch.parent_check(&self.target)?;
            self.epoch.namespace_check(&self.namespace)?;
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection::transaction) fn complete_deletion<'a>(
        &'a mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeDeletion<'a, 'e>, StoreError> {
        use crate::connection::store::{RecoveryReason, RecoveryState};
        self.validate_live(live)?;
        self.epoch.verify_deletion_scope(store, live, cancel)?;
        if self.target_guard.is_some() {
            let prepared = store.recovered_deletion_prepared(live, self)?;
            let step = session.admit_step(store, live, StepKind::Relocate, cancel)?;
            let expected = self
                .target_guard
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            let displaced = self
                .epoch
                .relocate_deletion(
                    live,
                    &self.source,
                    &self.target,
                    &self.namespace,
                    expected,
                    &prepared,
                    step,
                )
                .map_err(|_| StoreError::RecoveryRequired)?;
            let guard = self
                .epoch
                .guard_displaced(&displaced)
                .map_err(|_| StoreError::RecoveryRequired)?;
            self.displaced = Some((displaced, guard));
            self.target_guard = None;
        }
        let (displaced, guard) = self.displaced.take().ok_or(StoreError::RecoveryRequired)?;
        let direction = self
            .epoch
            .connected
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?
            .direction;
        let side = if direction == 0 {
            crate::connection::model::Side::Destination
        } else {
            crate::connection::model::Side::Source
        };
        let expected = if direction == 0 {
            live.intent().destination_before.clone()
        } else {
            live.intent().source_before.clone()
        };
        let mut existing = None;
        for slot in store.native_recovery_slots(live)? {
            if slot.state == RecoveryState::Reserved {
                store.retain_native_incomplete(live, slot.id)?;
            } else if slot.state == RecoveryState::BytesVerified {
                if slot.original_side != side || slot.content != expected {
                    return Err(StoreError::RecoveryRequired);
                }
                if existing.is_none() {
                    existing = Some(slot.id);
                }
            }
        }
        let reserved = if existing.is_none() {
            Some(store.reserve_native_recovery(live, side, RecoveryReason::NormalDeletion)?)
        } else {
            None
        };
        let step = session.admit_step(store, live, StepKind::Preserve, cancel)?;
        let mut reader = self
            .epoch
            .recovery_source(live, &self.namespace, &displaced, &guard, step)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let bound = if let Some(id) = existing {
            store.rebind_native_recovery(live, id, &mut reader, cancel)?
        } else {
            store.preserve_native(
                live,
                reserved.ok_or(StoreError::RecoveryRequired)?,
                &mut reader,
                cancel,
            )?
        };
        drop(reader);
        let sealed = self
            .epoch
            .seal_displacement(live, displaced, guard, &self.namespace, bound)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.epoch.verify_deletion_scope(store, live, cancel)?;
        let step = session.admit_step(store, live, StepKind::Publish, cancel)?;
        self.epoch
            .deletion_outcome_with_existing(
                live,
                &self.source,
                &self.target,
                &self.namespace,
                sealed,
                self.outcome.take(),
                step,
            )
            .map_err(|_| StoreError::RecoveryRequired)
    }
}

#[cfg(test)]
mod equality_tests {
    use super::*;
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn native_matching_edits_patch_two_files_without_namespace_mutation_and_compare_freshly() {
        use crate::connection::{
            model::{Decision, EntryState},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
        };
        use std::sync::atomic::AtomicBool;
        let tree = tests::fixture();
        let source_path = tree.path().join("equal-source");
        let target_path = tree.path().join("equal-target");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        for path in [&source_path, &target_path] {
            std::fs::write(path.join("a"), b"old a").unwrap();
            std::fs::write(path.join("b"), b"old b").unwrap();
        }
        let source = tests::endpoint(&source_path);
        let target = tests::endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = endpoint::probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let connection = ConnectionId([53; 16]);
        store
            .create_connection(
                connection,
                binding(&source_path),
                binding(&target_path),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let cancel = AtomicBool::new(false);
        let mut last = None;
        for (name, bytes) in [
            (b"a".as_slice(), b"old a".as_slice()),
            (b"b", b"old b"),
            (b"a", b"new a"),
            (b"b", b"new b"),
        ] {
            if bytes == b"new a" {
                for path in [&source_path, &target_path] {
                    std::fs::write(path.join("a"), b"new a").unwrap();
                    std::fs::write(path.join("b"), b"new b").unwrap();
                }
            }
            let record = store.load_connection(connection).unwrap();
            let session = store.transaction_session(connection).unwrap();
            let value = Some(EntryState::File {
                sha256: Sha256::digest(bytes).into(),
                bytes: bytes.len() as u64,
                executable: false,
            });
            let intent = OperationIntent {
                connection,
                expected_revision: record.revision,
                expected_baseline: record.baseline_revision,
                path: RelativePath::unix(name).unwrap(),
                source_before: value.clone(),
                destination_before: value,
                decision: Decision::VerifyEqual,
            };
            let admission = admission_metadata(&store, &intent, &source, &target, &cancel).unwrap();
            let mut live = if record.baseline_revision.is_none() {
                let accepted = initial_preview(&store, connection, &source, &target, &cancel)
                    .unwrap()
                    .accept(intent.clone())
                    .unwrap();
                store
                    .begin_native_reviewed(intent, &session, accepted, admission)
                    .unwrap()
            } else {
                store.begin_native(intent, &session, admission).unwrap()
            };
            store
                .reserve_native_work(
                    &mut live,
                    &session,
                    16 * 1024 * 1024,
                    Instant::now() + std::time::Duration::from_secs(30),
                    &cancel,
                )
                .unwrap();
            let epoch = NativeEpoch::begin_connected(&live, &source, &target, &cancel).unwrap();
            let mut equal = epoch.equal_evidence(&live).unwrap();
            store
                .record_native_equal(&mut live, &mut equal, &cancel)
                .unwrap();
            let mut comparison = equal.comparison(&store, &live, &cancel).unwrap();
            let candidate = store
                .prepare_baseline(&live, &mut comparison, &cancel)
                .unwrap();
            store
                .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
                .unwrap();
            if bytes == b"new a" {
                assert_eq!(
                    store
                        .load_baseline(connection)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .get(&RelativePath::unix(b"b").unwrap()),
                    Some(&EntryState::File {
                        sha256: Sha256::digest(b"old b").into(),
                        bytes: 5,
                        executable: false
                    })
                );
            }
            assert!(
                store
                    .pending_page(connection, None, 8)
                    .unwrap()
                    .entries
                    .is_empty()
            );
            assert!(
                store
                    .recovery_page(connection, None, 8)
                    .unwrap()
                    .entries
                    .is_empty()
            );
            last = Some(live.id());
        }
        for path in [&source_path, &target_path] {
            assert_eq!(std::fs::read_dir(path).unwrap().count(), 2);
            assert_eq!(std::fs::read(path.join("a")).unwrap(), b"new a");
            assert_eq!(std::fs::read(path.join("b")).unwrap(), b"new b");
        }
        drop(store);
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store.load_connection(connection).unwrap();
        let mut context = store
            .open_recovery_context(last.unwrap(), record.revision)
            .unwrap();
        store
            .reserve_recovery_reads(
                &mut context,
                8 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let mut epoch = NativeEpoch::begin_recovery(&context, &source, &target, &cancel).unwrap();
        assert!(epoch.final_comparison(&store, &context, &cancel).unwrap());
        std::fs::write(source_path.join("b"), b"later edit").unwrap();
        assert!(!epoch.final_comparison(&store, &context, &cancel).unwrap());
        assert_eq!(std::fs::read(target_path.join("b")).unwrap(), b"new b");
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn equal_evidence(
        mut self,
        live: &LiveOperation,
    ) -> Result<super::NativeEquality<'e>> {
        use crate::connection::model::{Decision, EntryState};
        self.connected_check(live)?;
        if live.intent().decision != Decision::VerifyEqual
            || live.intent().source_before != live.intent().destination_before
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let source = self.pin_parent(Side::Source, &live.intent().path)?;
        let target = self.pin_parent(Side::Target, &live.intent().path)?;
        let (source_guard, target_guard) = match &live.intent().source_before {
            Some(EntryState::File { .. }) => (
                Some(self.guard_regular(&source)?),
                Some(self.guard_regular(&target)?),
            ),
            None => {
                self.check_absent(&source)?;
                self.check_absent(&target)?;
                (None, None)
            }
            _ => return Err(self.failure(EndpointError::Unsupported)),
        };
        let mut proof = super::NativeEquality {
            epoch: self,
            source,
            target,
            source_guard,
            target_guard,
        };
        proof
            .validate(live)
            .map_err(|_| proof.epoch.failure(EndpointError::Unverified))?;
        Ok(proof)
    }
}
impl super::NativeEquality<'_> {
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.connected_check(live)?;
            if live.intent().decision != crate::connection::model::Decision::VerifyEqual {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            match (&self.source_guard, &self.target_guard) {
                (Some(source), Some(target)) => {
                    let source = self.epoch.state(source)?;
                    let target = self.epoch.state(target)?;
                    if !NativeEpoch::matches_plan(&source, live.intent().source_before.as_ref())
                        || !NativeEpoch::matches_plan(
                            &target,
                            live.intent().destination_before.as_ref(),
                        )
                        || live.intent().source_before != live.intent().destination_before
                    {
                        return Err(self.epoch.failure(EndpointError::Unstable));
                    }
                }
                (None, None)
                    if live.intent().source_before.is_none()
                        && live.intent().destination_before.is_none() =>
                {
                    self.epoch.check_absent(&self.source)?;
                    self.epoch.check_absent(&self.target)?;
                }
                _ => return Err(self.epoch.failure(EndpointError::Unverified)),
            }
            self.epoch.parent_check(&self.source)?;
            self.epoch.parent_check(&self.target)?;
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    pub(in crate::connection::transaction) fn comparison<'a>(
        &'a mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::VerifiedComparison<'a>, StoreError> {
        let existing_ancestors = self.epoch.comparison_ancestors(store, live, cancel)?;
        let value = super::CompletionEvidence::fresh(self, store, live, cancel)?;
        Ok(super::VerifiedComparison {
            existing_ancestors,
            evidence: self,
            id: live.id(),
            revision: live.revision(),
            fence: live.fence(),
            lifetime: live.lifetime().clone(),
            value,
        })
    }
}
impl super::CompletionEvidence for super::NativeEquality<'_> {
    #[cfg(test)]
    fn revoke_for_test(&mut self) {
        self.epoch.leases.revoke();
    }

    fn fresh(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<Option<crate::connection::model::EntryState>, StoreError> {
        let result = (|| {
            self.validate(live)?;
            let value = observe_comparison(&mut self.epoch, store, live, cancel)?;
            self.validate(live)?;
            Ok(value)
        })();
        if result.is_err() && self.epoch.ancestors.is_some() {
            self.epoch.failure(EndpointError::Unstable);
        }
        result
    }
    fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    #[ignore = "authorized Linux filesystem required; metadata capture grants no native authority"]
    fn native_admission_metadata_rejects_same_bytes_new_generation_before_intent() {
        use crate::connection::{
            model::{Decision, EntryState, Side as ModelSide},
            store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
        };
        let tree = tests::fixture();
        let source_path = tree.path().join("admit-source");
        let target_path = tree.path().join("admit-target");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&target_path).unwrap();
        std::fs::write(source_path.join("file"), b"proposal").unwrap();
        let source = tests::endpoint(&source_path);
        let target = tests::endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = endpoint::probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([54; 16]),
                binding(&source_path),
                binding(&target_path),
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"file").unwrap(),
            source_before: Some(EntryState::File {
                sha256: Sha256::digest(b"proposal").into(),
                bytes: 8,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate {
                from: ModelSide::Source,
            },
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let reviewed = initial_preview(&store, record.id, &source, &target, &cancel)
            .unwrap()
            .accept(intent.clone())
            .unwrap();
        let admission = admission_metadata(&store, &intent, &source, &target, &cancel).unwrap();
        std::fs::rename(
            source_path.join("file"),
            source_path.join("retained-original"),
        )
        .unwrap();
        std::fs::write(source_path.join("file"), b"proposal").unwrap();
        let session = store.transaction_session(record.id).unwrap();
        assert!(
            store
                .begin_native_reviewed(intent, &session, reviewed, admission)
                .is_err()
        );
        assert!(
            store
                .pending_page(record.id, None, 8)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            std::fs::read(source_path.join("file")).unwrap(),
            b"proposal"
        );
        assert_eq!(
            std::fs::read(source_path.join("retained-original")).unwrap(),
            b"proposal"
        );
        assert_eq!(std::fs::read_dir(target_path).unwrap().count(), 0);
    }
}

pub(in crate::connection::transaction) struct AdmissionPath {
    directory: NativeDirectory,
    name: CString,
    chain: Vec<d::Object>,
    present: Option<(File, ObjectId, Stamp, bool)>,
}
fn planning_check(
    deadline: Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<(), EndpointError> {
    if Instant::now() >= deadline || cancel.load(std::sync::atomic::Ordering::Acquire) {
        Err(EndpointError::Cancelled)
    } else {
        Ok(())
    }
}
impl AdmissionPath {
    fn capture(
        endpoint: &PinnedEndpoint,
        path: &RelativePath,
        deadline: Instant,
        cancel: &std::sync::atomic::AtomicBool,
        writable: bool,
    ) -> std::result::Result<Self, EndpointError> {
        planning_check(deadline, cancel)?;
        let Some((leaf, ancestors)) = path.components().split_last() else {
            return Err(EndpointError::Unsupported);
        };
        if path.components().len() > 64 {
            return Err(EndpointError::LimitExceeded);
        }
        let mut directory = endpoint.native_directory(writable)?;
        let mut chain = vec![descriptor_object(&object(directory.file())?)];
        for component in ancestors {
            planning_check(deadline, cancel)?;
            let NativeName::Unix(bytes) = component else {
                return Err(EndpointError::Unsupported);
            };
            directory = directory.child(OsStr::from_bytes(bytes), writable)?;
            chain.push(descriptor_object(&object(directory.file())?));
        }
        let NativeName::Unix(bytes) = leaf else {
            return Err(EndpointError::Unsupported);
        };
        let name = CString::new(bytes.as_slice()).map_err(|_| EndpointError::Unsupported)?;
        planning_check(deadline, cancel)?;
        let present = match open_at(directory.file(), &name, libc::O_RDONLY) {
            Ok(file) => {
                endpoint::native_same_mount(directory.file(), &file)?;
                let executable = endpoint::native_regular_metadata(&file)?;
                let identity = object(&file)?;
                let stamp = Stamp::new(&file.metadata()?);
                Some((file, identity, stamp, executable))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let result = Self {
            directory,
            name,
            chain,
            present,
        };
        result.validate(deadline, cancel)?;
        Ok(result)
    }
    fn validate(
        &self,
        deadline: Instant,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), EndpointError> {
        planning_check(deadline, cancel)?;
        self.directory.validate(false)?;
        match &self.present {
            Some((file, identity, stamp, executable)) => {
                let named = open_at(self.directory.file(), &self.name, libc::O_PATH)?;
                if object(&named)? != *identity
                    || object(file)? != *identity
                    || Stamp::new(&file.metadata()?) != *stamp
                    || endpoint::native_regular_metadata(file)? != *executable
                {
                    return Err(EndpointError::Unstable);
                }
            }
            None => match open_at(self.directory.file(), &self.name, libc::O_PATH) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => return Err(EndpointError::Unstable),
                Err(error) => return Err(error.into()),
            },
        }
        planning_check(deadline, cancel)?;
        self.directory.validate(false)
    }
    fn planned_state(
        &self,
        planned: Option<&crate::connection::model::EntryState>,
    ) -> std::result::Result<d::State, EndpointError> {
        match (&self.present, planned) {
            (None, None) => Ok(d::State::Absent),
            (
                Some((_, identity, stamp, executable)),
                Some(crate::connection::model::EntryState::File {
                    sha256,
                    bytes,
                    executable: expected,
                }),
            ) if stamp.length == *bytes && executable == expected => Ok(d::State::Regular {
                object: descriptor_object(identity),
                stamp: descriptor_stamp(stamp),
                content: d::Content {
                    digest: *sha256,
                    length: *bytes,
                    executable: *expected,
                },
            }),
            _ => Err(EndpointError::Unstable),
        }
    }
}
/// Read-only metadata correlation under the pre-Intent planning allowance. No
/// leases/helper, payload reads, approval, canonical read credit or live proof.
pub(in crate::connection::transaction) fn admission_metadata<'e>(
    store: &crate::connection::store::ConnectionStore,
    intent: &crate::connection::store::OperationIntent,
    source: &'e PinnedEndpoint,
    target: &'e PinnedEndpoint,
    cancel: &'e std::sync::atomic::AtomicBool,
) -> std::result::Result<super::NativeAdmission<'e>, StoreError> {
    let (record, lifetime, decision) = store.native_admission_basis(intent)?;
    if source.identity() != record.source.identity
        || target.identity() != record.destination.identity
    {
        return Err(StoreError::IdentityChanged);
    }
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    let left = AdmissionPath::capture(
        source,
        &intent.path,
        deadline,
        cancel,
        matches!(decision, 2 | 3),
    )
    .map_err(|_| StoreError::RecoveryRequired)?;
    let right = AdmissionPath::capture(
        target,
        &intent.path,
        deadline,
        cancel,
        !matches!(decision, 2 | 3),
    )
    .map_err(|_| StoreError::RecoveryRequired)?;
    let states = [
        left.planned_state(intent.source_before.as_ref()),
        right.planned_state(intent.destination_before.as_ref()),
    ];
    let [left_state, right_state] = states;
    let snapshot = d::Admission {
        connection: intent.connection.0,
        operation: 0,
        revision: record.revision,
        fence: record.fence_generation,
        baseline: record.baseline_revision,
        endpoints: [
            descriptor_endpoint(record.source.identity),
            descriptor_endpoint(record.destination.identity),
        ],
        direction: u8::from(matches!(decision, 2 | 3)),
        decision,
        path: intent.path.clone(),
        chains: [left.chain.clone(), right.chain.clone()],
        states: [
            left_state.map_err(|_| StoreError::RecoveryRequired)?,
            right_state.map_err(|_| StoreError::RecoveryRequired)?,
        ],
    };
    Ok(super::NativeAdmission {
        record,
        intent: intent.clone(),
        lifetime,
        endpoints: [source, target],
        paths: [left, right],
        snapshot,
        deadline,
        cancel,
    })
}
impl super::NativeAdmission<'_> {
    pub(in crate::connection) fn consume(
        mut self,
        store: &crate::connection::store::ConnectionStore,
        intent: &crate::connection::store::OperationIntent,
        serial: u64,
    ) -> std::result::Result<Vec<u8>, StoreError> {
        if self.intent != *intent
            || self.lifetime.strong_count() == 0
            || !store.same_native_lifetime(&self.lifetime)
            || store.load_connection(intent.connection)? != self.record
        {
            return Err(StoreError::StaleRevision);
        }
        for (index, (endpoint, path)) in self.endpoints.iter().zip(&self.paths).enumerate() {
            endpoint
                .revalidate()
                .map_err(|_| StoreError::IdentityChanged)?;
            path.validate(self.deadline, self.cancel)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let fresh = AdmissionPath::capture(
                endpoint,
                &intent.path,
                self.deadline,
                self.cancel,
                index != usize::from(self.snapshot.direction),
            )
            .map_err(|_| StoreError::RecoveryRequired)?;
            if fresh.chain != path.chain {
                return Err(StoreError::IdentityChanged);
            }
            fresh
                .validate(self.deadline, self.cancel)
                .map_err(|_| StoreError::RecoveryRequired)?;
            path.validate(self.deadline, self.cancel)
                .map_err(|_| StoreError::RecoveryRequired)?;
        }
        self.snapshot.operation = serial;
        self.snapshot
            .encode()
            .map_err(|_| StoreError::LimitExceeded)
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn inspect_never_begun(
        mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> Result<super::NeverBegun<'e>> {
        if !context.action().is_reconciliation() {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.inspect_untouched_inner(store, context, cancel)
    }
    fn inspect_untouched_inner(
        mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> Result<super::NeverBegun<'e>> {
        self.cancel_read = Some(cancel);
        self.recovery_check(context)?;
        let identity = store
            .never_begun_identity(context)
            .map_err(|_| self.failure(EndpointError::Unverified))?;
        let direction = usize::from(identity.direction);
        if identity.connection != context.id().connection.0
            || identity.operation != context.id().serial
            || identity.revision.checked_add(1) != Some(context.admission())
            || identity.baseline != context.baseline()
            || identity.endpoints != context.endpoints().map(descriptor_endpoint)
            || identity.path != context.intent().path
            || identity.fence > context.fence()
            || (!context.recovery_only() && identity.fence != context.fence())
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let source = self.pin_parent(Side::Source, &identity.path)?;
        let target = self.pin_parent(Side::Target, &identity.path)?;
        if source
            .chain
            .iter()
            .map(descriptor_object)
            .collect::<Vec<_>>()
            != *identity
                .chains
                .get(direction)
                .ok_or_else(|| self.failure(EndpointError::Unverified))?
            || target
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *identity
                    .chains
                    .get(1 - direction)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        let source_guard = match *identity
            .states
            .get(direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            d::State::Absent => {
                self.check_absent(&source)?;
                None
            }
            d::State::Regular { .. } => Some(self.guard_regular(&source)?),
            _ => return Err(self.failure(EndpointError::Unverified)),
        };
        let target_guard = match *identity
            .states
            .get(1 - direction)
            .ok_or_else(|| self.failure(EndpointError::Unverified))?
        {
            d::State::Absent => {
                self.check_absent(&target)?;
                None
            }
            d::State::Regular { .. } => Some(self.guard_regular(&target)?),
            _ => return Err(self.failure(EndpointError::Unverified)),
        };
        let mut reservations = Vec::new();
        for record in store
            .recovery_namespace_records(context)
            .map_err(|_| self.failure(EndpointError::Unverified))?
        {
            if record
                .uncreated_for(context)
                .map_err(|_| self.failure(EndpointError::Unverified))?
            {
                reservations.push(record);
            }
        }
        let mut proof = super::NeverBegun {
            epoch: self,
            revision: context.revision(),
            identity,
            source,
            target,
            source_guard,
            target_guard,
            reservations,
        };
        proof
            .validate_join(store, context)
            .map_err(|_| proof.epoch.failure(EndpointError::Unverified))?;
        Ok(proof)
    }
}
impl super::NeverBegun<'_> {
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if !context.action().is_reconciliation() {
            return Err(StoreError::RecoveryRequired);
        }
        self.validate_join(store, context)
    }
    fn validate_join(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != context.revision()
            || store.never_begun_identity(context)? != self.identity
        {
            return Err(StoreError::StaleRevision);
        }
        self.validate_native(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        store.check_recovery_context(context)
    }
    fn validate_native(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> Result<()> {
        self.epoch.recovery_check(context)?;
        let direction = usize::from(self.identity.direction);
        for (parent, guard, expected) in [
            (
                &self.source,
                &self.source_guard,
                &*self
                    .identity
                    .states
                    .get(direction)
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?,
            ),
            (
                &self.target,
                &self.target_guard,
                &*self
                    .identity
                    .states
                    .get(1 - direction)
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?,
            ),
        ] {
            self.epoch.parent_check(parent)?;
            match (guard, expected) {
                (Some(guard), d::State::Regular { .. })
                    if self.epoch.state(guard)? == *expected => {}
                (None, d::State::Absent) => {
                    self.epoch.check_absent(parent)?;
                }
                _ => return Err(self.epoch.failure(EndpointError::Unstable)),
            }
        }
        for reservation in &self.reservations {
            if !reservation
                .uncreated_for(context)
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?
            {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            let name = CString::new(reservation.name())
                .map_err(|_| self.epoch.failure(EndpointError::Unsupported))?;
            let root = if usize::from(reservation.side()) == direction {
                &self.epoch.source_root
            } else {
                &self.epoch.target_root
            };
            match open_at(root.file(), &name, libc::O_PATH) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => return Err(self.epoch.failure(EndpointError::Unverified)),
                Err(error) => return Err(self.epoch.failure(error.into())),
            }
        }
        self.epoch.recovery_check(context)
    }
}

#[cfg(test)]
mod never_begun_tests {
    use super::*;
    use crate::connection::{
        model::{Decision, EntryState, Side as ModelSide},
        store::{
            ConnectionPolicy, ConnectionStore, EndpointBinding, Lifecycle, OperationIntent,
            ReceiptStage,
        },
    };
    #[test]
    #[ignore = "authorized Linux filesystem/helper required; compile/link only under current holds"]
    fn native_never_begun_resumes_same_operation_or_accounts_disconnect_without_adopting_names() {
        for reverse in [false, true] {
            for reserved in [false, true] {
                for disconnect in [false, true] {
                    case(reverse, reserved, disconnect, 0);
                }
            }
            for damage in 1..=7 {
                case(reverse, true, false, damage);
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep real-state boundary setup, continuation/refusal and exact retained-byte assertions together."
    )]
    fn case(reverse: bool, reserved: bool, disconnect: bool, damage: u8) {
        let leaf = if damage == 0 { "file" } else { "folder/file" };
        let tree = tests::fixture();
        let source_path = tree.path().join("source");
        let target_path = tree.path().join("target");
        for path in [&source_path, &target_path] {
            std::fs::create_dir_all(path.join("folder")).unwrap();
        }
        std::fs::write(source_path.join(leaf), b"proposal").unwrap();
        let source_endpoint = tests::endpoint(&source_path);
        let target_endpoint = tests::endpoint(&target_path);
        let binding = |path: &std::path::Path| {
            let probe = endpoint::probe_endpoint(path).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let (left_binding, right_binding) = if reverse {
            (binding(&target_path), binding(&source_path))
        } else {
            (binding(&source_path), binding(&target_path))
        };
        let (left, right) = if reverse {
            (&target_endpoint, &source_endpoint)
        } else {
            (&source_endpoint, &target_endpoint)
        };
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let record = store
            .create_connection(
                ConnectionId([55; 16]),
                left_binding,
                right_binding,
                ConnectionPolicy {
                    deletion_retention_seconds: 2_592_000,
                },
            )
            .unwrap();
        let record = record.value();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"proposal").into(),
            bytes: 8,
            executable: false,
        });
        let (source_before, destination_before, from, to) = if reverse {
            (None, value, ModelSide::Destination, ModelSide::Source)
        } else {
            (value, None, ModelSide::Source, ModelSide::Destination)
        };
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(leaf.as_bytes()).unwrap(),
            source_before,
            destination_before,
            decision: Decision::Propagate { from },
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let accepted = initial_preview(&store, record.id, left, right, &cancel)
            .unwrap()
            .accept(intent.clone())
            .unwrap();
        let metadata = admission_metadata(&store, &intent, left, right, &cancel).unwrap();
        let session = store.transaction_session(record.id).unwrap();
        let mut live = store
            .begin_native_reviewed(intent, &session, accepted, metadata)
            .unwrap();
        store
            .reserve_native_work(
                &mut live,
                &session,
                4 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let name = reserved.then(|| {
            store
                .reserve_native_namespace(&mut live, to)
                .unwrap()
                .name()
        });
        let id = live.id();
        store.checkpoint().unwrap();
        drop(store);
        drop(session);
        match damage {
            1 => {
                std::fs::rename(source_path.join(leaf), source_path.join("retained")).unwrap();
                std::fs::write(source_path.join(leaf), b"proposal").unwrap();
            }
            2 => {
                std::fs::rename(
                    source_path.join("folder"),
                    source_path.join("retained-folder"),
                )
                .unwrap();
                std::fs::create_dir(source_path.join("folder")).unwrap();
                std::fs::write(source_path.join(leaf), b"proposal").unwrap();
            }
            3 => {
                std::fs::rename(
                    source_path.join("folder"),
                    source_path.join("retained-folder"),
                )
                .unwrap();
            }
            4 | 5 => {
                let private = target_path.join(name.as_ref().unwrap());
                std::fs::create_dir(&private).unwrap();
                if damage == 5 {
                    std::fs::write(private.join("unknown"), b"unowned bytes").unwrap();
                }
            }
            _ => {}
        }
        let mut store = ConnectionStore::open_at(&tree.path().join("store")).unwrap();
        let current = store.load_connection(id.connection).unwrap();
        let session = store.transaction_session(id.connection).unwrap();
        let fence = disconnect.then(|| {
            store
                .request_disconnect(id.connection, current.revision)
                .unwrap()
        });
        let current = store.load_connection(id.connection).unwrap();
        let mut context = store.open_recovery_context(id, current.revision).unwrap();
        store
            .reserve_recovery_reads(
                &mut context,
                16 * 1024 * 1024,
                Instant::now() + std::time::Duration::from_secs(30),
                &cancel,
            )
            .unwrap();
        let epoch =
            NativeEpoch::begin_recovery(&context, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        if damage == 6 {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        if damage == 7 {
            session.request_disconnect();
        }
        let result = epoch.inspect_never_begun(&store, &context, &cancel);
        if damage != 0 {
            assert!(result.is_err());
            assert_eq!(
                store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
                ReceiptStage::Intent
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert!(!target_path.join(leaf).exists());
            match damage {
                1 => {
                    assert_eq!(
                        std::fs::read(source_path.join("retained")).unwrap(),
                        b"proposal"
                    );
                    assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"proposal");
                }
                2 | 3 => {
                    assert_eq!(
                        std::fs::read(source_path.join("retained-folder/file")).unwrap(),
                        b"proposal"
                    );
                    if damage == 2 {
                        assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"proposal");
                    }
                }
                _ => assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"proposal"),
            }
            if damage == 4 {
                assert_eq!(
                    std::fs::read_dir(target_path.join(name.as_ref().unwrap()))
                        .unwrap()
                        .count(),
                    0
                );
            }
            if damage == 5 {
                assert_eq!(
                    std::fs::read(target_path.join(name.as_ref().unwrap()).join("unknown"))
                        .unwrap(),
                    b"unowned bytes"
                );
            }
            return;
        }
        let mut proof = result.unwrap();
        if let Some(fence) = fence {
            assert!(
                store
                    .resume_native_untouched(&context, &mut proof, &session, &cancel)
                    .is_err()
            );
            let accounted = store
                .record_never_begun(&context, &mut proof, &cancel)
                .unwrap();
            assert_eq!(accounted.value().stage, ReceiptStage::AccountedPending);
            store.finish_disconnect(&fence).unwrap();
            assert_eq!(
                store.load_connection(id.connection).unwrap().lifecycle,
                Lifecycle::Disconnected
            );
            assert!(
                store
                    .load_baseline(id.connection)
                    .unwrap()
                    .entries()
                    .is_none()
            );
            assert!(!target_path.join(leaf).exists());
            assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"proposal");
            assert_eq!(
                store.pending_page(id.connection, None, 8).unwrap().entries[0].stage,
                ReceiptStage::AccountedPending
            );
            return;
        }
        let mut live = store
            .resume_native_untouched(&context, &mut proof, &session, &cancel)
            .unwrap();
        assert_eq!(live.id(), id);
        drop(proof);
        let reservation = store.reserve_native_namespace(&mut live, to).unwrap();
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint, &cancel)
                .unwrap();
        let source = epoch.pin_parent(Side::Source, &live.intent().path).unwrap();
        let target = epoch.pin_parent(Side::Target, &live.intent().path).unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Namespace, &cancel)
            .unwrap();
        let private = epoch
            .create_registered_namespace(&live, &reservation, &source, &target, &guard, None, step)
            .unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Stage, &cancel)
            .unwrap();
        let staged = epoch
            .stage_connected(&live, &guard, &private, step)
            .unwrap();
        let prepared = epoch.prepared(&private, Some(&staged)).unwrap();
        store
            .prepare_native(&mut live, reservation, prepared)
            .unwrap();
        let absent = epoch.observe_absent(&target).unwrap();
        let step = session
            .admit_step(&store, &live, StepKind::Publish, &cancel)
            .unwrap();
        let mut outcome = epoch
            .publish_connected(
                &live,
                &guard,
                &target,
                &private,
                staged,
                Some(absent),
                None,
                step,
            )
            .unwrap();
        store
            .record_native_outcome(&mut live, &mut outcome, false, &cancel)
            .unwrap();
        store
            .record_native_outcome(&mut live, &mut outcome, true, &cancel)
            .unwrap();
        let mut comparison = outcome.comparison(&store, &live, &cancel).unwrap();
        let candidate = store
            .prepare_baseline(&live, &mut comparison, &cancel)
            .unwrap();
        store
            .commit_baseline(&mut live, candidate, &mut comparison, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(source_path.join(leaf)).unwrap(), b"proposal");
        assert_eq!(std::fs::read(target_path.join(leaf)).unwrap(), b"proposal");
        assert_eq!(
            store
                .load_baseline(id.connection)
                .unwrap()
                .entries()
                .unwrap()
                .len(),
            1
        );
        if let Some(name) = name {
            assert!(!target_path.join(name).exists());
        }
    }
}

impl<'e> super::RecoveredEvidence<'e> {
    pub(in crate::connection::transaction) fn safe_boundary(
        mut self,
        context: &crate::connection::store::RecoveryContext,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeSafeBoundary<'e>, StoreError> {
        self.epoch.cancel_read = Some(cancel);
        self.validate(context)?;
        self.epoch
            .barriers(&self.namespace.directory, &self.target.directory)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate(context)?;
        Ok(if self.published.is_some() {
            super::NativeSafeBoundary::PublishedAccounted(super::OwnedBoundary::Regular(self))
        } else if self.displaced.is_some() {
            super::NativeSafeBoundary::DisplacedAccounted(super::OwnedBoundary::Regular(self))
        } else {
            super::NativeSafeBoundary::StagedOnly(super::OwnedBoundary::Regular(self))
        })
    }
}
impl<'e> super::RecoveredDeletion<'e> {
    pub(in crate::connection::transaction) fn safe_boundary(
        mut self,
        context: &crate::connection::store::RecoveryContext,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeSafeBoundary<'e>, StoreError> {
        self.epoch.cancel_read = Some(cancel);
        self.validate(context)?;
        self.epoch
            .barriers(&self.namespace.directory, &self.target.directory)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate(context)?;
        Ok(if self.outcome.is_some() {
            super::NativeSafeBoundary::PublishedAccounted(super::OwnedBoundary::Deletion(self))
        } else if self.displaced.is_some() {
            super::NativeSafeBoundary::DisplacedAccounted(super::OwnedBoundary::Deletion(self))
        } else {
            super::NativeSafeBoundary::StagedOnly(super::OwnedBoundary::Deletion(self))
        })
    }
}
impl super::NativeSafeBoundary<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(u8, Option<(u32, Option<Vec<u8>>, Option<Vec<u8>>)>), StoreError>
    {
        use super::{NativeSafeBoundary as Boundary, OwnedBoundary};
        let (kind, proof) = match self {
            Boundary::NeverBegun(proof) => {
                proof.validate(store, context)?;
                return Ok((0, None));
            }
            Boundary::StagedOnly(proof) => (1, proof),
            Boundary::DisplacedAccounted(proof) => (2, proof),
            Boundary::PublishedAccounted(proof) => (3, proof),
            Boundary::Uncertain => return Err(StoreError::RecoveryRequired),
        };
        let (actual_kind, namespace, displaced, outcome) = match proof {
            OwnedBoundary::Regular(proof) => {
                proof.validate(context)?;
                (
                    if proof.published.is_some() {
                        3
                    } else if proof.displaced.is_some() {
                        2
                    } else {
                        1
                    },
                    &proof.namespace,
                    proof
                        .displaced
                        .as_ref()
                        .and_then(|(value, _)| value.descriptor.clone()),
                    proof.outcome.clone(),
                )
            }
            OwnedBoundary::Deletion(proof) => {
                proof.validate(context)?;
                (
                    if proof.outcome.is_some() {
                        3
                    } else if proof.displaced.is_some() {
                        2
                    } else {
                        1
                    },
                    &proof.namespace,
                    proof
                        .displaced
                        .as_ref()
                        .and_then(|(value, _)| value.descriptor.clone()),
                    proof.outcome.clone(),
                )
            }
        };
        if actual_kind != kind {
            return Err(StoreError::RecoveryRequired);
        }
        let (header, _, _) = namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let attempt = u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?;
        store.check_recovery_context(context)?;
        Ok((kind, Some((attempt, displaced, outcome))))
    }
}

impl<'e> super::NativeSafeBoundary<'e> {
    fn drain_parts(
        &mut self,
    ) -> std::result::Result<
        (
            &mut NativeEpoch<'e>,
            &PrivateNamespace,
            &RelocatedObject,
            &StableFileGuard,
        ),
        StoreError,
    > {
        use super::{NativeSafeBoundary as Boundary, OwnedBoundary};
        let (Boundary::DisplacedAccounted(proof) | Boundary::PublishedAccounted(proof)) = self
        else {
            return Err(StoreError::RecoveryRequired);
        };
        match proof {
            OwnedBoundary::Regular(proof) => {
                let (displaced, guard) = proof
                    .displaced
                    .as_ref()
                    .ok_or(StoreError::RecoveryRequired)?;
                Ok((&mut proof.epoch, &proof.namespace, displaced, guard))
            }
            OwnedBoundary::Deletion(proof) => {
                let (displaced, guard) = proof
                    .displaced
                    .as_ref()
                    .ok_or(StoreError::RecoveryRequired)?;
                Ok((&mut proof.epoch, &proof.namespace, displaced, guard))
            }
        }
    }
    pub(in crate::connection::transaction) fn drain_reader<'a>(
        &'a mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<super::NativeDrainReader<'a, 'e>, StoreError> {
        if !context.recovery_only() {
            return Err(StoreError::RecoveryRequired);
        }
        self.validate(store, context)?;
        let (epoch, _, _, guard) = self.drain_parts()?;
        let captured_bytes = descriptor_content(
            &epoch
                .capture(guard)
                .map_err(|_| StoreError::RecoveryRequired)?,
        );
        Ok(super::NativeDrainReader {
            boundary: super::NativeDrainBoundary::Ordinary(self),
            content: captured_bytes,
        })
    }
}
impl super::NativeDrainReader<'_, '_> {
    pub(in crate::connection) fn validate_for(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if !context.recovery_only() {
            return Err(StoreError::RecoveryRequired);
        }
        self.boundary.validate(store, context)?;
        self.validate()
    }
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Join distinct durable correlation fields to their canonical operation, endpoint or current proof; similarly named fields are not interchangeable."
    )]
    pub(in crate::connection) fn validate(&mut self) -> std::result::Result<(), StoreError> {
        let (epoch, namespace, displaced, guard) = self.boundary.drain_parts()?;
        epoch
            .namespace_check(namespace)
            .map_err(|_| StoreError::RecoveryRequired)?;
        epoch
            .validate_guard(guard, true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        epoch
            .ep(displaced.original_parent.validate(true))
            .map_err(|_| StoreError::RecoveryRequired)?;
        if displaced.actual != guard.actual
            || displaced.epoch != epoch.nonce
            || object(&displaced.retained).map_err(|_| StoreError::RecoveryRequired)?
                != guard.actual
        {
            return Err(StoreError::IdentityChanged);
        }
        epoch.check().map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn advance(
        &mut self,
        previous: u64,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        self.boundary.advance(previous, context)?;
        self.validate()
    }
    pub(in crate::connection) fn read_chunk(
        &mut self,
        offset: u64,
        bytes: &mut [u8],
    ) -> std::result::Result<usize, StoreError> {
        if bytes.len() > 256 * 1024 || offset > self.content.length {
            return Err(StoreError::LimitExceeded);
        }
        self.validate()?;
        let (epoch, _, _, guard) = self.boundary.drain_parts()?;
        epoch
            .charge(bytes.len() as u64)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let count = guard.file.read_at(bytes, offset)?;
        self.validate()?;
        Ok(count)
    }
    pub(in crate::connection) fn content(&self) -> &d::Content {
        &self.content
    }
    pub(in crate::connection) fn deadline(&mut self) -> std::result::Result<Instant, StoreError> {
        Ok(self.boundary.drain_parts()?.0.deadline)
    }
    pub(in crate::connection) fn source(
        &mut self,
    ) -> std::result::Result<(u32, Vec<u8>, u8), StoreError> {
        let (_, namespace, displaced, _) = self.boundary.drain_parts()?;
        let (header, intent, _) = namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((
            u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?,
            displaced
                .descriptor
                .clone()
                .ok_or(StoreError::RecoveryRequired)?,
            1 - intent.direction,
        ))
    }
    pub(in crate::connection) fn fact(
        &mut self,
        id: crate::connection::store::RecoveryId,
        artifact: d::Artifact,
    ) -> std::result::Result<d::RecoveryFact, StoreError> {
        let content = self.content.clone();
        let (_, namespace, _, guard) = self.boundary.drain_parts()?;
        let (header, intent, bytes) = namespace
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        if header.connection != id.operation.connection.0 || header.operation != id.operation.serial
        {
            return Err(StoreError::StaleRevision);
        }
        let descriptor = d::decode(bytes).map_err(|_| StoreError::Corrupt)?;
        let side = 1 - intent.direction;
        Ok(d::RecoveryFact {
            intent: descriptor.digest,
            recovery: d::Recovery {
                connection: id.operation.connection.0,
                operation: id.operation.serial,
                slot: id.slot,
            },
            side,
            role: 0,
            endpoint: *intent
                .endpoints
                .get(usize::from(side))
                .ok_or(StoreError::Corrupt)?,
            object: descriptor_object(&guard.actual),
            content,
            artifact,
        })
    }
}

impl<'e> NativeEpoch<'e> {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    pub(in crate::connection::transaction) fn capture_conflict(
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        source: &'e PinnedEndpoint,
        destination: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeConflictCapture<'e>, StoreError> {
        use crate::connection::model::Observation;
        store.check_native(live)?;
        let mut epoch = Self::begin_connected_mode(live, source, destination, cancel, true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let parents = [
            epoch
                .pin_parent(Side::Source, &live.intent().path)
                .map_err(|_| StoreError::RecoveryRequired)?,
            epoch
                .pin_parent(Side::Target, &live.intent().path)
                .map_err(|_| StoreError::RecoveryRequired)?,
        ];
        let mut guards = Vec::new();
        let mut versions = Vec::new();
        for (index, parent) in parents.iter().enumerate() {
            epoch
                .parent_check(parent)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let state = match open_at(parent.directory.file(), &parent.name, libc::O_PATH) {
                Ok(file) => {
                    if !file.metadata()?.is_file() {
                        return Err(StoreError::RecoveryRequired);
                    }
                    let guard = epoch
                        .guard_regular(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    let state = epoch
                        .state(&guard)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    guards.push(Some(guard));
                    state
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let names = epoch
                        .owned_names(live, parent.side, store.native_namespace_records(live)?)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    let endpoint = if index == 0 { source } else { destination };
                    let observed = endpoint::observe_connected(endpoint, &names, cancel);
                    if !matches!(observed.observation(), Observation::Complete)
                        || observed.entries().contains_key(&live.intent().path)
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    epoch
                        .check_absent(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    guards.push(None);
                    d::State::Absent
                }
                Err(error) => return Err(error.into()),
            };
            versions.push(d::CapturedVersion {
                side: u8::try_from(index).map_err(|_| StoreError::Corrupt)?,
                role: 0,
                state,
                displaced: None,
            });
        }
        let records = store.native_namespace_records(live)?;
        for record in &records {
            record.validate(live)?;
        }
        let (namespaces, extra) = epoch.capture_namespaces(
            records,
            live.id(),
            live.admission(),
            live.fence(),
            &live.intent().path,
            &parents,
        )?;
        let mut pairs = versions
            .into_iter()
            .zip(guards)
            .chain(extra)
            .collect::<Vec<_>>();
        pairs.sort_by_key(|(version, _)| (version.side, version.role));
        let (versions, guards) = pairs.into_iter().unzip();
        let snapshot = d::ConflictSnapshot {
            connection: live.id().connection.0,
            operation: live.id().serial,
            admission: live.admission(),
            revision: live.revision(),
            fence: live.fence(),
            baseline: live.intent().expected_baseline,
            endpoints: [
                descriptor_endpoint(live.endpoints()[0]),
                descriptor_endpoint(live.endpoints()[1]),
            ],
            path: live.intent().path.clone(),
            chains: [
                parents[0].chain.iter().map(descriptor_object).collect(),
                parents[1].chain.iter().map(descriptor_object).collect(),
            ],
            versions,
        };
        snapshot.encode().map_err(|_| StoreError::Corrupt)?;
        let mut proof = super::NativeConflictCapture {
            epoch,
            revision: live.revision(),
            snapshot,
            parents,
            guards,
            namespaces,
        };
        proof.kind()?;
        proof.validate(live)?;
        Ok(proof)
    }
}
impl super::NativeConflictCapture<'_> {
    pub(in crate::connection) fn kind(&self) -> std::result::Result<u8, StoreError> {
        let at_name = |side| {
            self.snapshot
                .versions
                .iter()
                .find(|version| version.side == side && version.role == 0)
                .map(|version| &version.state)
                .ok_or(StoreError::Corrupt)
        };
        let content = self
            .snapshot
            .versions
            .iter()
            .filter_map(|version| match &version.state {
                d::State::Regular { content, .. } => Some(content),
                _ => None,
            })
            .collect::<Vec<_>>();
        match (at_name(0)?, at_name(1)?) {
            (d::State::Regular { content: a, .. }, d::State::Regular { content: b, .. })
                if a != b =>
            {
                Ok(0)
            }
            (d::State::Absent, d::State::Regular { .. })
            | (d::State::Regular { .. }, d::State::Absent) => Ok(1),
            _ if content
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left != right)) =>
            {
                Ok(0)
            }
            _ => Err(StoreError::RecoveryRequired),
        }
    }
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != live.revision()
            || self.snapshot.operation != live.id().serial
            || self.snapshot.connection != live.id().connection.0
            || self.snapshot.fence != live.fence()
        {
            return Err(StoreError::StaleRevision);
        }
        self.epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_guards()?;
        self.epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)
    }
    fn validate_guards(&mut self) -> std::result::Result<(), StoreError> {
        if self.snapshot.versions.len() != self.guards.len() {
            return Err(StoreError::RecoveryRequired);
        }
        for namespace in &self.namespaces {
            self.epoch.validate_capture_namespace(namespace)?;
        }
        for parent in &self.parents {
            self.epoch
                .parent_check(parent)
                .map_err(|_| StoreError::RecoveryRequired)?;
        }
        for (guard, version) in self.guards.iter().zip(&self.snapshot.versions) {
            let parent = self
                .parents
                .get(usize::from(version.side))
                .ok_or(StoreError::Corrupt)?;
            match (guard, &version.state) {
                (Some(guard), d::State::Regular { .. }) => {
                    if self
                        .epoch
                        .state(guard)
                        .map_err(|_| StoreError::RecoveryRequired)?
                        != version.state
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                }
                (None, d::State::Absent) => self
                    .epoch
                    .check_absent(parent)
                    .map_err(|_| StoreError::RecoveryRequired)?,
                _ => return Err(StoreError::RecoveryRequired),
            }
        }
        Ok(())
    }
    pub(in crate::connection) fn validate_recovery(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != context.revision()
            || self.snapshot.operation != context.id().serial
            || self.snapshot.connection != context.id().connection.0
            || (self.snapshot.fence != context.fence()
                && (!context.recovery_only() || self.snapshot.fence > context.fence()))
        {
            return Err(StoreError::StaleRevision);
        }
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_guards()?;
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn advance(
        &mut self,
        live: &LiveOperation,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != previous || previous.checked_add(1) != Some(live.revision()) {
            return Err(StoreError::StaleRevision);
        }
        self.revision = live.revision();
        self.validate(live)
    }
    pub(in crate::connection) fn advance_recovery(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != previous || previous.checked_add(1) != Some(context.revision()) {
            return Err(StoreError::StaleRevision);
        }
        self.revision = context.revision();
        self.validate_recovery(context)
    }
    pub(in crate::connection) fn snapshot(&self) -> &d::ConflictSnapshot {
        &self.snapshot
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    pub(in crate::connection) fn read_chunk(
        &mut self,
        index: usize,
        offset: u64,
        buffer: &mut [u8],
    ) -> std::result::Result<usize, StoreError> {
        let guard = self
            .guards
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(StoreError::RecoveryRequired)?;
        self.epoch
            .validate_guard(guard, true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.epoch
            .read_budget
            .charge(buffer.len() as u64)
            .map_err(|()| StoreError::LimitExceeded)?;
        let count = guard.file.read_at(buffer, offset)?;
        self.epoch
            .validate_guard(guard, true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        Ok(count)
    }
}

impl<'e> NativeEpoch<'e> {
    /// Reopen a captured at-name version under fresh leases and the same paid operation.
    /// QNC1 supplies only the exact historical comparison target.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    pub(in crate::connection::transaction) fn recover_conflict(
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        source: &'e PinnedEndpoint,
        destination: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeConflictCapture<'e>, StoreError> {
        use crate::connection::model::Observation;
        store.check_recovery_context(context)?;
        let snapshot = store.native_capture_snapshot(context)?;
        if snapshot.fence != context.fence()
            && (!context.recovery_only() || snapshot.fence > context.fence())
        {
            return Err(StoreError::RecoveryRequired);
        }
        let mut epoch = Self::begin_recovery_mode(context, source, destination, cancel, true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let parents = [
            epoch
                .pin_parent(Side::Source, &snapshot.path)
                .map_err(|_| StoreError::RecoveryRequired)?,
            epoch
                .pin_parent(Side::Target, &snapshot.path)
                .map_err(|_| StoreError::RecoveryRequired)?,
        ];
        let mut guards = Vec::new();
        for (index, parent) in parents.iter().enumerate() {
            if parent
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *snapshot.chains.get(index).ok_or(StoreError::Corrupt)?
            {
                return Err(StoreError::RecoveryRequired);
            }
            let version = snapshot
                .versions
                .iter()
                .find(|version| usize::from(version.side) == index && version.role == 0)
                .ok_or(StoreError::Corrupt)?;
            let guard = match &version.state {
                d::State::Regular { .. } => {
                    let guard = epoch
                        .guard_regular(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    if epoch
                        .state(&guard)
                        .map_err(|_| StoreError::RecoveryRequired)?
                        != version.state
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    Some(guard)
                }
                d::State::Absent => {
                    let names = epoch
                        .owned_recovery_names(
                            context,
                            parent.side,
                            store.recovery_namespace_records(context)?,
                        )
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    let endpoint = if index == 0 { source } else { destination };
                    let observed = endpoint::observe_connected(endpoint, &names, cancel);
                    if !matches!(observed.observation(), Observation::Complete)
                        || observed.entries().contains_key(&snapshot.path)
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    epoch
                        .check_absent(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    None
                }
                _ => return Err(StoreError::RecoveryRequired),
            };
            guards.push(guard);
        }
        let records = store.recovery_namespace_records(context)?;
        for record in &records {
            record.validate_recovery(context)?;
        }
        let (namespaces, extra) = epoch.capture_namespaces(
            records,
            context.id(),
            context.admission(),
            snapshot.fence,
            &snapshot.path,
            &parents,
        )?;
        let mut pairs = snapshot
            .versions
            .iter()
            .filter(|version| version.role == 0)
            .cloned()
            .zip(guards)
            .chain(extra)
            .collect::<Vec<_>>();
        pairs.sort_by_key(|(version, _)| (version.side, version.role));
        let (actual, guards): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        if actual != snapshot.versions {
            return Err(StoreError::RecoveryRequired);
        }
        let mut proof = super::NativeConflictCapture {
            epoch,
            revision: context.revision(),
            snapshot,
            parents,
            guards,
            namespaces,
        };
        proof.validate_recovery(context)?;
        store.check_recovery_context(context)?;
        Ok(proof)
    }
}

pub(in crate::connection::transaction) struct CaptureNamespace {
    private: PrivateNamespace,
    stage: Option<Vec<u8>>,
    displaced: Option<Vec<u8>>,
    outcome: Option<Vec<u8>>,
    shape: RegularNames,
}
impl NativeEpoch<'_> {
    fn validate_capture_namespace(
        &mut self,
        namespace: &CaptureNamespace,
    ) -> std::result::Result<(), StoreError> {
        let (_, _, intent) = namespace
            .private
            .descriptor
            .as_ref()
            .ok_or(StoreError::Corrupt)?;
        self.namespace_check(&namespace.private)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.read_descriptor(&namespace.private.directory, c"intent", intent)
            .map_err(|_| StoreError::RecoveryRequired)?;
        for (name, bytes) in [
            (c"staged-intent", &namespace.stage),
            (c"displaced-intent", &namespace.displaced),
            (c"outcome", &namespace.outcome),
        ] {
            if let Some(bytes) = bytes {
                self.read_descriptor(&namespace.private.directory, name, bytes)
                    .map_err(|_| StoreError::RecoveryRequired)?;
            }
        }
        if self
            .namespace_names(&namespace.private, namespace.stage.is_some())
            .map_err(|_| StoreError::RecoveryRequired)?
            != namespace.shape
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn capture_namespaces(
        &mut self,
        records: Vec<crate::connection::store::NativeNamespaceRecord>,
        id: OperationId,
        admission: u64,
        fence: u64,
        path: &RelativePath,
        parents: &[ParentGuard; 2],
    ) -> std::result::Result<
        (
            Vec<CaptureNamespace>,
            Vec<(d::CapturedVersion, Option<StableFileGuard>)>,
        ),
        StoreError,
    > {
        let mut namespaces = Vec::new();
        let mut extra = Vec::new();
        for record in records {
            if record.operation() != id {
                continue;
            }
            let root = if record.side() == 0 {
                self.source_root.clone()
            } else {
                self.target_root.clone()
            };
            let name = CString::new(record.name()).map_err(|_| StoreError::Corrupt)?;
            let (intent_bytes, stage_bytes) = match record.descriptors() {
                Ok((intent, stage)) => (intent.to_vec(), stage.map(<[u8]>::to_vec)),
                Err(_) => match open_at(root.file(), &name, libc::O_PATH) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    _ => return Err(StoreError::RecoveryRequired),
                },
            };
            let intent = d::decode(&intent_bytes).map_err(|_| StoreError::Corrupt)?;
            let d::Body::Intent(body) = &intent.body else {
                return Err(StoreError::Corrupt);
            };
            let side = usize::from(record.side());
            if intent.header.connection != id.connection.0
                || intent.header.operation != id.serial
                || intent.header.admission != admission
                || intent.header.fence != fence
                || intent.header.namespace_name() != record.name()
                || body.path != *path
                || body.endpoints != self.identities.map(descriptor_endpoint)
                || side != 1 - usize::from(body.direction)
                || parents.iter().enumerate().any(|(index, parent)| {
                    body.chains.get(index).is_none_or(|chain| {
                        parent
                            .chain
                            .iter()
                            .map(descriptor_object)
                            .collect::<Vec<_>>()
                            != *chain
                    })
                })
            {
                return Err(StoreError::RecoveryRequired);
            }
            let directory = root
                .child(OsStr::from_bytes(name.to_bytes()), true)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let identity = object(directory.file()).map_err(|_| StoreError::RecoveryRequired)?;
            if descriptor_object(&identity) != body.namespace {
                return Err(StoreError::RecoveryRequired);
            }
            let parent = parents.get(side).ok_or(StoreError::Corrupt)?;
            let private = PrivateNamespace {
                directory,
                _name: name,
                epoch: self.nonce,
                identity,
                intended_parent: object(parent.directory.file())
                    .map_err(|_| StoreError::RecoveryRequired)?,
                intended_name: parent.name.clone(),
                descriptor: Some((intent.header.clone(), body.clone(), intent_bytes)),
            };
            let stage = stage_bytes
                .as_deref()
                .map(d::decode)
                .transpose()
                .map_err(|_| StoreError::Corrupt)?;
            if let Some(stage) = &stage {
                stage
                    .joined(&intent, None, None)
                    .map_err(|_| StoreError::Corrupt)?;
            }
            let shape = self
                .namespace_names(&private, stage.is_some())
                .map_err(|_| StoreError::RecoveryRequired)?;
            if shape.displaced != shape.displaced_intent {
                return Err(StoreError::RecoveryRequired);
            }
            let displaced = if shape.displaced {
                Some(
                    self.load_descriptor(&private.directory, c"displaced-intent")
                        .map_err(|_| StoreError::RecoveryRequired)?,
                )
            } else {
                None
            };
            let displaced_value = displaced
                .as_deref()
                .map(d::decode)
                .transpose()
                .map_err(|_| StoreError::Corrupt)?;
            if let Some(value) = &displaced_value {
                value
                    .joined(&intent, stage.as_ref(), None)
                    .map_err(|_| StoreError::Corrupt)?;
            }
            let outcome = if shape.outcome {
                Some(
                    self.load_descriptor(&private.directory, c"outcome")
                        .map_err(|_| StoreError::RecoveryRequired)?,
                )
            } else {
                None
            };
            if let Some(bytes) = &outcome {
                d::decode(bytes)
                    .and_then(|value| {
                        value.joined(&intent, stage.as_ref(), displaced_value.as_ref())
                    })
                    .map_err(|_| StoreError::Corrupt)?;
            }
            if record
                .outcome()
                .is_some_and(|bytes| Some(bytes) != outcome.as_deref())
            {
                return Err(StoreError::RecoveryRequired);
            }
            for (role, name, present) in [
                (1, c"displaced", shape.displaced),
                (2, c"stage", shape.stage),
            ] {
                if !present {
                    continue;
                }
                let at = ParentGuard {
                    directory: private.directory.clone(),
                    name: name.to_owned(),
                    epoch: self.nonce,
                    side: if side == 0 {
                        Side::Source
                    } else {
                        Side::Target
                    },
                    chain: Vec::new(),
                    path: None,
                };
                let guard = self
                    .guard_regular(&at)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                let captured_state = self
                    .state(&guard)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                let expected = if role == 1 {
                    match &displaced_value.as_ref().ok_or(StoreError::Corrupt)?.body {
                        d::Body::Displaced(value)
                            if value.side == record.side() && value.barriers == 3 =>
                        {
                            &value.object
                        }
                        _ => return Err(StoreError::Corrupt),
                    }
                } else {
                    match &stage.as_ref().ok_or(StoreError::Corrupt)?.body {
                        d::Body::Stage(value) => &value.object,
                        _ => return Err(StoreError::Corrupt),
                    }
                };
                if descriptor_object(&guard.actual) != *expected {
                    return Err(StoreError::RecoveryRequired);
                }
                extra.push((
                    d::CapturedVersion {
                        side: record.side(),
                        role,
                        state: captured_state,
                        displaced: (role == 1)
                            .then(|| displaced_value.as_ref().map(|value| value.digest))
                            .flatten(),
                    },
                    Some(guard),
                ));
                if extra.len() > 2 {
                    return Err(StoreError::LimitExceeded);
                }
            }
            let namespace = CaptureNamespace {
                private,
                stage: stage_bytes,
                displaced,
                outcome,
                shape,
            };
            self.validate_capture_namespace(&namespace)?;
            namespaces.push(namespace);
        }
        Ok((namespaces, extra))
    }
}
impl super::NativeConflictCapture<'_> {
    pub(in crate::connection) fn displaced(&self) -> Vec<Vec<u8>> {
        self.namespaces
            .iter()
            .filter_map(|namespace| namespace.displaced.clone())
            .collect()
    }
    pub(in crate::connection) fn origin(
        &self,
        index: usize,
    ) -> std::result::Result<(u32, Vec<u8>), StoreError> {
        let version = self
            .snapshot
            .versions
            .get(index)
            .ok_or(StoreError::Corrupt)?;
        let Some(digest) = version.displaced else {
            return Ok((0, Vec::new()));
        };
        for namespace in &self.namespaces {
            if let Some(bytes) = &namespace.displaced {
                let value = d::decode(bytes).map_err(|_| StoreError::Corrupt)?;
                if value.digest == digest {
                    return Ok((
                        u32::try_from(value.header.attempt).map_err(|_| StoreError::Corrupt)?,
                        bytes.clone(),
                    ));
                }
            }
        }
        Err(StoreError::RecoveryRequired)
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn selected_restore(
        store: &mut crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        Self::selected_source(store, live, left, right, cancel, false)
    }
    pub(in crate::connection::transaction) fn selected_tombstone(
        store: &mut crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        Self::selected_source(store, live, left, right, cancel, true)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn selected_source(
        store: &mut crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
        tombstone: bool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        let (target_side, _, _, selection) = store.selected_native_scope(live)?;
        if matches!(selection, d::Selection::Tombstone { .. }) != tombstone {
            return Err(StoreError::RecoveryRequired);
        }
        let identity = store.selected_native_identity(live)?;
        if identity.endpoints != live.endpoints().map(descriptor_endpoint) {
            return Err(StoreError::IdentityChanged);
        }
        let direction = usize::from(1 - target_side);
        let (source, target) = if direction == 0 {
            (left, right)
        } else {
            (right, left)
        };
        if *live.endpoints().get(direction).ok_or(StoreError::Corrupt)? != source.identity()
            || *live
                .endpoints()
                .get(1 - direction)
                .ok_or(StoreError::Corrupt)?
                != target.identity()
        {
            return Err(StoreError::IdentityChanged);
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&live.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            source,
            target,
            OperationBinding {
                _connection: live.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: live.admission(),
                _fence: live.fence(),
            },
            Arc::clone(live.operation_reads()),
        )
        .map_err(|_| StoreError::RecoveryRequired)?;
        epoch.cancel_read = Some(cancel);
        epoch.stop_read = Some(live.gate());
        epoch.connected = Some(ConnectedBinding {
            selected_action: Some(live.action().clone()),
            capture_only: false,
            id: live.id(),
            lifetime: live.lifetime().clone(),
            admission: live.admission(),
            fence: live.fence(),
            direction,
            intent: live.intent().clone(),
        });
        epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let source = epoch
            .pin_parent(Side::Source, &live.intent().path)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let target = epoch
            .pin_parent(Side::Target, &live.intent().path)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let mut guards = Vec::new();
        for parent in [&source, &target] {
            let side = if parent.side == Side::Source {
                direction
            } else {
                1 - direction
            };
            if parent
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *identity.chains.get(side).ok_or(StoreError::Corrupt)?
            {
                return Err(StoreError::IdentityChanged);
            }
            let planned = if side == 0 {
                &live.intent().source_before
            } else {
                &live.intent().destination_before
            };
            if planned.is_some() {
                let guard = epoch
                    .guard_regular(parent)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                let actual = epoch
                    .state(&guard)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                if !Self::matches_plan(&actual, planned.as_ref())
                    || actual != *identity.states.get(side).ok_or(StoreError::Corrupt)?
                {
                    return Err(StoreError::RecoveryRequired);
                }
                guards.push(Some(guard));
            } else {
                if *identity.states.get(side).ok_or(StoreError::Corrupt)? != d::State::Absent {
                    return Err(StoreError::RecoveryRequired);
                }
                let names = epoch
                    .owned_names(live, parent.side, store.native_namespace_records(live)?)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                let endpoint = if parent.side == Side::Source {
                    epoch.source
                } else {
                    epoch.target
                };
                let observed = endpoint::observe_connected(endpoint, &names, cancel);
                if !matches!(
                    observed.observation(),
                    crate::connection::model::Observation::Complete
                ) || observed.entries().contains_key(&live.intent().path)
                {
                    return Err(StoreError::RecoveryRequired);
                }
                epoch
                    .check_absent(parent)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                guards.push(None);
            }
        }
        let reader = store.open_selected_source(live, epoch.deadline, cancel)?;
        let target_guard = guards.pop().ok_or(StoreError::Corrupt)?;
        let source_guard = guards.pop().ok_or(StoreError::Corrupt)?;
        let mut proof = super::NativeRestore {
            published: false,
            outcome: None,
            relocated: None,
            sealed: None,
            epoch,
            revision: live.revision(),
            reader,
            source,
            target,
            source_guard,
            target_guard,
            private: None,
            staged: None,
        };
        proof.validate(store, live)?;
        Ok(proof)
    }
}
impl super::NativeRestore<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        store.check_native(live)?;
        if self.revision != live.revision() {
            return Err(StoreError::StaleRevision);
        }
        self.reader.validate(store, live)?;
        let (target, _, _, selection) = store.selected_native_scope(live)?;
        self.validate_native(live, usize::from(1 - target), &selection)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    fn validate_native(
        &mut self,
        live: &LiveOperation,
        direction: usize,
        selection: &d::Selection,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.connected_check(live)?;
            for (parent, guard, index) in [
                (&self.source, self.source_guard.as_ref(), direction),
                (&self.target, self.target_guard.as_ref(), 1 - direction),
            ] {
                self.epoch.parent_check(parent)?;
                if self.published && parent.side == Side::Target {
                    if matches!(selection, d::Selection::Tombstone { .. }) {
                        self.epoch.check_absent(parent)?;
                        continue;
                    }
                    let stage = self
                        .staged
                        .as_ref()
                        .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
                    self.epoch.validate_guard(&stage.guard, true)?;
                    continue;
                }
                if self.epoch.relocated && parent.side == Side::Target {
                    self.epoch.check_absent(parent)?;
                    continue;
                }
                let state = if let Some(guard) = guard {
                    self.epoch.validate_guard(guard, true)?;
                    self.epoch.state(guard)?
                } else {
                    self.epoch.check_absent(parent)?;
                    d::State::Absent
                };
                let planned = if index == 0 {
                    &live.intent().source_before
                } else {
                    &live.intent().destination_before
                };
                if !NativeEpoch::matches_plan(&state, planned.as_ref()) {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            if let Some(private) = &self.private {
                self.epoch.namespace_check(private)?;
                if self.epoch.relocated && self.relocated.is_none() && self.sealed.is_none() {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                }
                if let Some((displaced, guard)) = &self.relocated {
                    self.epoch.validate_guard(guard, true)?;
                    if displaced.actual != guard.actual || !displaced.matches_guard {
                        return Err(self.epoch.failure(EndpointError::Unstable));
                    }
                    self.epoch.read_descriptor(
                        &private.directory,
                        c"displaced-intent",
                        displaced.descriptor.as_ref().ok_or_else(|| Failure {
                            _cause: EndpointError::Unverified,
                            _boundary: Boundary::Uncertain,
                            _os_error: None,
                        })?,
                    )?;
                }
                if let Some(sealed) = &self.sealed {
                    self.epoch.sealed_check(live, private, sealed)?;
                }
                let Some((_, intent, bytes)) = &private.descriptor else {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                };
                if &intent.selection != selection {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                }
                self.epoch
                    .read_descriptor(&private.directory, c"intent", bytes)?;
                if let Some(stage) = &self.staged {
                    self.epoch.validate_guard(&stage.guard, true)?;
                    let current = self.epoch.capture(&stage.guard)?;
                    if descriptor_content(&current) != descriptor_content(&stage.bytes) {
                        return Err(self.epoch.failure(EndpointError::Unstable));
                    }
                    self.epoch.read_descriptor(
                        &private.directory,
                        c"staged-intent",
                        &stage.descriptor,
                    )?;
                }
                if let Some(bytes) = &self.outcome {
                    self.epoch
                        .read_descriptor(&private.directory, c"outcome", bytes)?;
                    let outcome = d::decode(bytes)
                        .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
                    let d::Body::Outcome(body) = outcome.body else {
                        return Err(self.epoch.failure(EndpointError::Unverified));
                    };
                    let target = if matches!(selection, d::Selection::Tombstone { .. }) {
                        self.epoch.check_absent(&self.target)?;
                        d::State::Absent
                    } else {
                        self.epoch.state(
                            &self
                                .staged
                                .as_ref()
                                .ok_or_else(|| Failure {
                                    _cause: EndpointError::Unverified,
                                    _boundary: Boundary::Uncertain,
                                    _os_error: None,
                                })?
                                .guard,
                        )?
                    };
                    if *body
                        .states
                        .get(1 - direction)
                        .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                        != target
                    {
                        return Err(self.epoch.failure(EndpointError::Unstable));
                    }
                }
            }
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn after_applied(
        &mut self,
        live: &LiveOperation,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != previous
            || previous.checked_add(1) != Some(live.revision())
            || !self.published
            || self.outcome.is_none()
        {
            return Err(StoreError::StaleRevision);
        }
        self.reader.retained_check()?;
        let (_, intent, _) = self
            .private
            .as_ref()
            .and_then(|private| private.descriptor.as_ref())
            .ok_or(StoreError::RecoveryRequired)?;
        let direction = usize::from(intent.direction);
        let selection = intent.selection.clone();
        self.validate_native(live, direction, &selection)
    }
    pub(in crate::connection) fn advance(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        if self.revision != previous || previous.checked_add(1) != Some(live.revision()) {
            return Err(StoreError::StaleRevision);
        }
        self.reader.advance(store, live, previous)?;
        self.revision = live.revision();
        self.validate(store, live)
    }
    pub(in crate::connection) fn prepared_descriptors(
        &self,
    ) -> std::result::Result<(Vec<u8>, Option<Vec<u8>>), StoreError> {
        let private = self.private.as_ref().ok_or(StoreError::RecoveryRequired)?;
        let (_, _, intent) = private
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let stage = self.staged.as_ref().map(|stage| stage.descriptor.clone());
        if stage.is_none()
            && !matches!(
                private
                    .descriptor
                    .as_ref()
                    .map(|(_, body, _)| &body.selection),
                Some(d::Selection::Tombstone { .. })
            )
        {
            return Err(StoreError::RecoveryRequired);
        }
        Ok((intent.clone(), stage))
    }
    #[cfg(test)]
    fn namespace_name(&self) -> std::result::Result<&str, StoreError> {
        self.private
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?
            ._name
            .to_str()
            .map_err(|_| StoreError::Corrupt)
    }
    fn prepare(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        let (target_side, replace, decision, selection) = store.selected_native_scope(live)?;
        if self.private.is_some()
            || self.staged.is_some()
            || (!replace && self.target_guard.is_some())
        {
            return Err(StoreError::RecoveryRequired);
        }
        let reserved = store.reserve_selected_namespace(live, self)?;
        let step = session.admit_selected_step(store, live, self, StepKind::Namespace, cancel)?;
        step.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Namespace,
        )?;
        let direction = usize::from(1 - target_side);
        let header = reserved.header(live, self.epoch.nonce)?;
        let result = (|| {
            self.epoch.connected_check(live)?;
            let mut expected = [d::State::Absent, d::State::Absent];
            if let Some(guard) = &self.source_guard {
                *expected
                    .get_mut(direction)
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))? =
                    self.epoch.state(guard)?;
            }
            if let Some(guard) = &self.target_guard {
                *expected
                    .get_mut(1 - direction)
                    .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))? =
                    self.epoch.state(guard)?;
            }
            let mut chains = [Vec::new(), Vec::new()];
            *chains
                .get_mut(direction)
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))? =
                self.source.chain.iter().map(descriptor_object).collect();
            *chains
                .get_mut(1 - direction)
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))? =
                self.target.chain.iter().map(descriptor_object).collect();
            let name = CString::new(reserved.name())
                .map_err(|_| self.epoch.failure(EndpointError::Unsupported))?;
            let root = self.epoch.target_root.clone();
            self.epoch.namespace_created = true;
            self.epoch.io(destination_security::create_directory(
                root.file(),
                OsStr::from_bytes(name.to_bytes()),
            ))?;
            let directory = self
                .epoch
                .ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
            self.epoch
                .io(destination_security::check_private_file(directory.file()))?;
            let identity = self.epoch.ep(object(directory.file()))?;
            let intent = d::Intent {
                endpoints: live.endpoints().map(descriptor_endpoint),
                direction: direction as u8,
                decision,
                namespace: descriptor_object(&identity),
                path: live.intent().path.clone(),
                chains,
                expected,
                selection,
            };
            let bytes = d::encode(&header, &d::Body::Intent(intent.clone()))
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch.write_descriptor(&directory, c"intent", &bytes)?;
            self.epoch.barriers(&directory, &root)?;
            let private = PrivateNamespace {
                directory,
                _name: name,
                epoch: self.epoch.nonce,
                identity,
                intended_parent: self.epoch.ep(object(self.target.directory.file()))?,
                intended_name: self.target.name.clone(),
                descriptor: Some((header, intent, bytes)),
            };
            self.epoch.namespace_check(&private)?;
            Ok(private)
        })();
        self.private = Some(result.map_err(|_: Failure| StoreError::RecoveryRequired)?);
        drop(step);
        self.validate(store, live)?;
        if matches!(
            self.private
                .as_ref()
                .and_then(|private| private.descriptor.as_ref())
                .map(|(_, body, _)| &body.selection),
            Some(d::Selection::Tombstone { .. })
        ) {
            return store.record_selected_prepared(live, self, reserved);
        }
        let step = session.admit_selected_step(store, live, self, StepKind::Stage, cancel)?;
        step.validate(live.id(), live.lifetime(), live.revision(), StepKind::Stage)?;
        self.stage_private(store, live)?;
        drop(step);
        store.record_selected_prepared(live, self, reserved)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn stage_private(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        let (id, artifact, selected) = self.reader.binding()?;
        let private = self.private.as_ref().ok_or(StoreError::RecoveryRequired)?;
        let mut file = self
            .epoch
            .io(open_at(
                private.directory.file(),
                c"stage",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            ))
            .map_err(|_| StoreError::RecoveryRequired)?;
        destination_security::check_private_file(&file)?;
        let mut digest = Sha256::new();
        let mut offset = 0_u64;
        let mut buffer = vec![0; 256 * 1024];
        loop {
            self.epoch
                .connected_check(live)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let count = self.reader.read_chunk(store, live, offset, &mut buffer)?;
            if count == 0 {
                break;
            }
            let chunk = buffer.get(..count).ok_or(StoreError::Corrupt)?;
            file.write_all(chunk)?;
            digest.update(chunk);
            offset = offset
                .checked_add(count as u64)
                .ok_or(StoreError::LimitExceeded)?;
            if offset > selected.length {
                return Err(StoreError::RecoveryRequired);
            }
        }
        let digest: [u8; 32] = digest.finalize().into();
        if offset != selected.length || digest != selected.digest {
            return Err(StoreError::RecoveryRequired);
        }
        // SAFETY: exclusive private stage fd, preserving only portable executable semantics.
        if unsafe {
            libc::fchmod(
                file.as_raw_fd(),
                if selected.executable { 0o700 } else { 0o600 },
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        file.sync_all()?;
        drop(file);
        self.reader.validate(store, live)?;
        let parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"stage".to_owned(),
            epoch: self.epoch.nonce,
            side: Side::Target,
            chain: vec![],
            path: None,
        };
        let guard = self
            .epoch
            .guard_regular(&parent)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let bytes = self
            .epoch
            .capture(&guard)
            .map_err(|_| StoreError::RecoveryRequired)?;
        if descriptor_content(&bytes) != selected {
            return Err(StoreError::RecoveryRequired);
        }
        let (header, _, intent) = private
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let intent = d::decode(intent).map_err(|_| StoreError::Corrupt)?;
        let body = d::Body::Stage(d::Stage {
            intent: intent.digest,
            object: descriptor_object(&guard.actual),
            content: selected.clone(),
            source: d::Source::Recovery {
                recovery: d::Recovery {
                    connection: id.operation.connection.0,
                    operation: id.operation.serial,
                    slot: id.slot,
                },
                artifact,
                content: selected,
            },
        });
        let descriptor = d::encode(header, &body).map_err(|_| StoreError::Corrupt)?;
        d::decode(&descriptor)
            .and_then(|stage| stage.joined(&intent, None, None))
            .map_err(|_| StoreError::Corrupt)?;
        self.epoch
            .write_descriptor(&private.directory, c"staged-intent", &descriptor)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.epoch
            .barriers(&private.directory, &private.directory)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.epoch.boundary = Boundary::PrivateStaged;
        self.staged = Some(SelectedStage {
            guard,
            bytes,
            descriptor,
        });
        self.validate(store, live)
    }
}

impl super::NativeRestore<'_> {
    pub(in crate::connection) fn outcome_descriptor(
        &self,
    ) -> std::result::Result<Vec<u8>, StoreError> {
        self.outcome.clone().ok_or(StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn outcome_displaced(
        &self,
    ) -> std::result::Result<Option<Vec<u8>>, StoreError> {
        self.sealed
            .as_ref()
            .map(|sealed| {
                sealed
                    .displaced
                    .descriptor
                    .clone()
                    .ok_or(StoreError::RecoveryRequired)
            })
            .transpose()
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.epoch.deadline
    }
    fn publish_absent(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        if self.target_guard.is_some() || self.sealed.is_some() || self.epoch.relocated {
            return Err(StoreError::RecoveryRequired);
        }
        self.publish_selected(store, live, session, cancel)
    }
    fn publish_selected_bytes(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        store.check_selected_prepared(live, self)?;
        if self.published
            || self.outcome.is_some()
            || self.sealed.is_some() != self.epoch.relocated
            || (self.target_guard.is_some() && self.sealed.is_none())
        {
            return Err(StoreError::RecoveryRequired);
        }
        let step = session.admit_selected_step(store, live, self, StepKind::Publish, cancel)?;
        step.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Publish,
        )?;
        let result = (|| {
            self.epoch.connected_check(live)?;
            let private = self
                .private
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            let stage = self
                .staged
                .as_mut()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch.namespace_check(private)?;
            self.epoch.parent_check(&self.target)?;
            if let Some(sealed) = &self.sealed {
                self.epoch.sealed_check(live, private, sealed)?;
            }
            self.epoch.check_absent(&self.target)?;
            self.epoch.validate_guard(&stage.guard, true)?;
            if private.intended_name != self.target.name
                || private.intended_parent != self.epoch.ep(object(self.target.directory.file()))?
            {
                return Err(self.epoch.failure(EndpointError::IdentityChanged));
            }
            self.epoch.boundary = Boundary::Uncertain;
            self.epoch.io(rename_noreplace(
                private.directory.file(),
                c"stage",
                self.target.directory.file(),
                &self.target.name,
            ))?;
            self.epoch
                .barriers(&private.directory, &self.target.directory)?;
            self.epoch.parent_check(&self.target)?;
            let actual = self.epoch.io(open_at(
                self.target.directory.file(),
                &self.target.name,
                libc::O_RDONLY,
            ))?;
            if self.epoch.ep(object(&actual))? != stage.guard.actual {
                return Err(self.epoch.failure(EndpointError::IdentityChanged));
            }
            let before = Stamp::new(&self.epoch.io(actual.metadata())?);
            if !before.relocated_eq(&stage.guard.stamp) {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            #[cfg(test)]
            tests::run_before_publication_reread();
            let bytes = self.epoch.read_hash(&actual, None)?;
            if before != Stamp::new(&self.epoch.io(actual.metadata())?)
                || descriptor_content(&bytes) != descriptor_content(&stage.bytes)
            {
                return Err(self.epoch.failure(EndpointError::Unstable));
            }
            stage.guard.stamp = before;
            stage.guard.parent = ParentGuard {
                directory: self.target.directory.clone(),
                name: self.target.name.clone(),
                epoch: self.epoch.nonce,
                side: Side::Target,
                chain: self.target.chain.clone(),
                path: self.target.path.clone(),
            };
            self.epoch.validate_guard(&stage.guard, true)?;
            self.epoch.parent_check(&self.target)?;
            self.epoch.check()
        })();
        result.map_err(|_: Failure| StoreError::RecoveryRequired)?;
        self.published = true;
        self.write_selected_outcome(store, live, &step)?;
        drop(step);
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep native guards alive through ordered preservation, fresh rereads, barriers and durable acknowledgements."
    )]
    fn write_selected_outcome(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        step: &StepAdmission,
    ) -> std::result::Result<(), StoreError> {
        step.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Publish,
        )?;
        if !self.published || self.outcome.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        self.validate(store, live)?;
        let result = (|| {
            let private = self
                .private
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;

            let (mut header, body, intent) = private
                .descriptor
                .clone()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            let intent =
                d::decode(&intent).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            let tombstone = matches!(body.selection, d::Selection::Tombstone { .. });
            let staged = self
                .staged
                .as_ref()
                .map(|stage| d::decode(&stage.descriptor))
                .transpose()
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            if staged.is_none() != tombstone {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            let source = if let Some(guard) = &self.source_guard {
                self.epoch.state(guard)?
            } else {
                self.epoch.check_absent(&self.source)?;
                d::State::Absent
            };
            let target = if tombstone {
                self.epoch.check_absent(&self.target)?;
                d::State::Absent
            } else {
                self.epoch.state(
                    &self
                        .staged
                        .as_ref()
                        .ok_or_else(|| Failure {
                            _cause: EndpointError::Unverified,
                            _boundary: Boundary::Uncertain,
                            _os_error: None,
                        })?
                        .guard,
                )?
            };
            let states = if body.direction == 0 {
                [source, target]
            } else {
                [target, source]
            };
            header.progress = live.revision();
            let mut references = Vec::new();
            let displaced =
                if let Some(sealed) = &self.sealed {
                    self.epoch.sealed_check(live, private, sealed)?;
                    let fact = sealed
                        .recovery
                        .value()
                        .validate(live, self.epoch.nonce)
                        .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
                    references.push(d::Reference {
                        slot: fact.recovery.slot,
                        side: fact.side,
                        role: fact.role,
                        binding: fact
                            .binding_digest()
                            .map_err(|_| self.epoch.failure(EndpointError::Unverified))?,
                    });
                    Some(
                        d::decode(sealed.displaced.descriptor.as_deref().ok_or_else(|| {
                            Failure {
                                _cause: EndpointError::Unverified,
                                _boundary: Boundary::Uncertain,
                                _os_error: None,
                            }
                        })?)
                        .map_err(|_| self.epoch.failure(EndpointError::Unverified))?,
                    )
                } else {
                    None
                };
            let body = d::Body::Outcome(d::Outcome {
                intent: intent.digest,
                stage: staged.as_ref().map(|value| value.digest),
                displaced: displaced.as_ref().map(|value| value.digest),
                states,
                kind: if tombstone {
                    2
                } else {
                    u8::from(displaced.is_some())
                },
                barriers: 3,
                recovery: references,
                sequence: 1,
            });
            let bytes = d::encode(&header, &body)
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            d::decode(&bytes)
                .and_then(|outcome| outcome.joined(&intent, staged.as_ref(), displaced.as_ref()))
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .write_descriptor(&private.directory, c"outcome", &bytes)?;
            self.epoch.parent_check(&self.source)?;
            self.epoch.parent_check(&self.target)?;
            self.epoch.check()?;
            Ok(bytes)
        })();
        self.outcome = Some(result.map_err(|_: Failure| StoreError::RecoveryRequired)?);
        self.epoch.boundary = Boundary::Published;
        self.validate(store, live)?;
        Ok(())
    }
    fn publish_selected(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.publish_selected_bytes(store, live, session, cancel)?;
        store.record_selected_outcome(live, self, false, cancel)?;
        store.record_selected_outcome(live, self, true, cancel)
    }
}

impl super::NativeRestore<'_> {
    fn relocate_selected(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        store.check_selected_displace_prepared(live, self)?;
        if self.target_guard.is_none() || self.epoch.relocated || self.published {
            return Err(StoreError::RecoveryRequired);
        }
        let step = session.admit_selected_step(store, live, self, StepKind::Relocate, cancel)?;
        step.validate(
            live.id(),
            live.lifetime(),
            live.revision(),
            StepKind::Relocate,
        )?;
        let result = (|| {
            let private = self
                .private
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            let expected = self
                .target_guard
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;

            let (mut header, body, intent) = private
                .descriptor
                .clone()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            let intent =
                d::decode(&intent).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            let staged = self
                .staged
                .as_ref()
                .map(|stage| d::decode(&stage.descriptor))
                .transpose()
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            if staged.is_none() && !matches!(body.selection, d::Selection::Tombstone { .. }) {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            self.epoch.connected_check(live)?;
            let mut displaced = self
                .epoch
                .relocate_noreplace(&self.target, private, expected)?;
            let stamp = Stamp::new(&self.epoch.io(displaced.retained.metadata())?);
            header.progress = live.revision();
            let bytes = d::encode(
                &header,
                &d::Body::Displaced(d::Displaced {
                    intent: intent.digest,
                    stage: staged.as_ref().map(|value| value.digest),
                    object: descriptor_object(&displaced.actual),
                    stamp: descriptor_stamp(&stamp),
                    matches_expected: displaced.matches_guard,
                    side: 1 - body.direction,
                    parent: descriptor_object(&private.intended_parent),
                    leaf: NativeName::Unix(private.intended_name.to_bytes().to_vec()),
                    barriers: 3,
                }),
            )
            .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            d::decode(&bytes)
                .and_then(|value| value.joined(&intent, staged.as_ref(), None))
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .write_descriptor(&private.directory, c"displaced-intent", &bytes)?;
            displaced.descriptor = Some(bytes);
            let guard = self.epoch.guard_displaced(&displaced)?;
            self.epoch.parent_check(&self.source)?;
            self.epoch.parent_check(&self.target)?;
            self.epoch.boundary = Boundary::DurableQuarantine;
            Ok((displaced, guard))
        })();
        self.relocated = Some(result.map_err(|_: Failure| StoreError::RecoveryRequired)?);
        drop(step);
        self.validate(store, live)?;
        Ok(())
    }
}

impl<'e> NativeEpoch<'e> {
    /// Historical QNI/QNS names select what to inspect; paid fresh private and
    /// endpoint guards are retained before the store can admit continuation.
    pub(in crate::connection::transaction) fn recover_selected(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        if matches!(
            store.selected_recovery_scope(context)?.3,
            d::Selection::Tombstone { .. }
        ) {
            return Err(StoreError::RecoveryRequired);
        }
        Self::recover_selected_mode(store, context, left, right, cancel, false)
    }
    pub(in crate::connection::transaction) fn recover_selected_tombstone(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        if !matches!(
            store.selected_recovery_scope(context)?.3,
            d::Selection::Tombstone { .. }
        ) {
            return Err(StoreError::RecoveryRequired);
        }
        Self::recover_selected_mode(store, context, left, right, cancel, false)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    fn recover_selected_mode(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
        stopped: bool,
    ) -> std::result::Result<super::NativeRestore<'e>, StoreError> {
        let (target_side, _, _, selection) = store.selected_recovery_scope(context)?;
        if context.recovery_only() != stopped {
            return Err(StoreError::RecoveryRequired);
        }
        let direction = usize::from(1 - target_side);
        let (source, target) = if direction == 0 {
            (left, right)
        } else {
            (right, left)
        };
        if *context
            .endpoints()
            .get(direction)
            .ok_or(StoreError::Corrupt)?
            != source.identity()
            || *context
                .endpoints()
                .get(1 - direction)
                .ok_or(StoreError::Corrupt)?
                != target.identity()
        {
            return Err(StoreError::IdentityChanged);
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&context.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            source,
            target,
            OperationBinding {
                _connection: context.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: context.admission(),
                _fence: context.fence(),
            },
            Arc::clone(context.reads()),
        )
        .map_err(|_| StoreError::RecoveryRequired)?;
        epoch.cancel_read = Some(cancel);
        if !stopped {
            epoch.stop_read = Some(context.gate());
        }
        epoch.connected = Some(ConnectedBinding {
            selected_action: Some(context.action().clone()),
            capture_only: false,
            id: context.id(),
            lifetime: context.lifetime().clone(),
            admission: context.admission(),
            fence: context.fence(),
            direction,
            intent: context.intent().clone(),
        });
        let mut records = store
            .recovery_namespace_records(context)?
            .into_iter()
            .filter(|record| record.operation() == context.id());
        let record = records.next().ok_or(StoreError::RecoveryRequired)?;
        if records.next().is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        record.validate_recovery(context)?;
        let (intent_bytes, stage_bytes) = record.descriptors()?;
        let intent_bytes = intent_bytes.to_vec();
        let stage_bytes = stage_bytes.map(<[u8]>::to_vec);
        let intent = d::decode(&intent_bytes).map_err(|_| StoreError::Corrupt)?;
        let stage = stage_bytes
            .as_ref()
            .map(|bytes| d::decode(bytes))
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        let d::Body::Intent(body) = &intent.body else {
            return Err(StoreError::Corrupt);
        };
        let tombstone = matches!(selection, d::Selection::Tombstone { .. });
        if stage.is_some() == tombstone {
            return Err(StoreError::RecoveryRequired);
        }
        if let Some(stage) = &stage {
            stage
                .joined(&intent, None, None)
                .map_err(|_| StoreError::Corrupt)?;
        }
        if body.selection != selection
            || usize::from(body.direction) != direction
            || record.side() != target_side
            || intent.header.connection != context.id().connection.0
            || intent.header.operation != context.id().serial
            || intent.header.admission != context.admission()
            || intent.header.baseline != context.baseline()
            || (intent.header.fence != context.fence()
                && (!stopped || intent.header.fence > context.fence()))
            || intent.header.namespace_name() != record.name()
            || body.path != context.intent().path
            || body.endpoints != context.endpoints().map(descriptor_endpoint)
            || !Self::matches_plan(&body.expected[0], context.intent().source_before.as_ref())
            || !Self::matches_plan(
                &body.expected[1],
                context.intent().destination_before.as_ref(),
            )
        {
            return Err(StoreError::RecoveryRequired);
        }
        let result = (|| {
            epoch.recovery_check(context)?;
            let source = epoch.pin_parent(Side::Source, &body.path)?;
            let target = epoch.pin_parent(Side::Target, &body.path)?;
            if source
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *body
                    .chains
                    .get(direction)
                    .ok_or_else(|| epoch.failure(EndpointError::Unverified))?
                || target
                    .chain
                    .iter()
                    .map(descriptor_object)
                    .collect::<Vec<_>>()
                    != *body
                        .chains
                        .get(1 - direction)
                        .ok_or_else(|| epoch.failure(EndpointError::Unverified))?
            {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
            let name = CString::new(record.name())
                .map_err(|_| epoch.failure(EndpointError::Unsupported))?;
            let directory = epoch.ep(epoch
                .target_root
                .child(OsStr::from_bytes(name.to_bytes()), true))?;
            let identity = epoch.ep(object(directory.file()))?;
            if descriptor_object(&identity) != body.namespace {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
            let private = PrivateNamespace {
                directory,
                _name: name,
                epoch: epoch.nonce,
                identity,
                intended_parent: epoch.ep(object(target.directory.file()))?,
                intended_name: target.name.clone(),
                descriptor: Some((intent.header.clone(), body.clone(), intent_bytes.clone())),
            };
            epoch.namespace_check(&private)?;
            epoch.read_descriptor(&private.directory, c"intent", &intent_bytes)?;
            if let Some(bytes) = &stage_bytes {
                epoch.read_descriptor(&private.directory, c"staged-intent", bytes)?;
            }
            let shape = epoch.namespace_names(&private, !tombstone)?;
            let relocated = if shape.displaced {
                Some(epoch.reconstruct_displacement(
                    &private,
                    &target,
                    &intent,
                    stage.as_ref(),
                    shape.displaced_intent,
                )?)
            } else {
                if shape.displaced_intent
                    || (!tombstone
                        && !shape.stage
                        && *body
                            .expected
                            .get(1 - direction)
                            .ok_or_else(|| epoch.failure(EndpointError::Unverified))?
                            != d::State::Absent)
                {
                    return Err(epoch.failure(EndpointError::Unverified));
                }
                None
            };
            let displaced_descriptor = relocated
                .as_ref()
                .map(|(value, _)| {
                    d::decode(value.descriptor.as_deref().ok_or_else(|| Failure {
                        _cause: EndpointError::Unverified,
                        _boundary: Boundary::Uncertain,
                        _os_error: None,
                    })?)
                    .map_err(|_| Failure {
                        _cause: EndpointError::Unverified,
                        _boundary: Boundary::Uncertain,
                        _os_error: None,
                    })
                })
                .transpose()?;
            let outcome = if shape.outcome {
                let bytes = epoch.load_descriptor(&private.directory, c"outcome")?;
                let value =
                    d::decode(&bytes).map_err(|_| epoch.failure(EndpointError::Unverified))?;
                value
                    .joined(&intent, stage.as_ref(), displaced_descriptor.as_ref())
                    .map_err(|_| epoch.failure(EndpointError::Unverified))?;
                if shape.stage || record.outcome().is_some_and(|canonical| canonical != bytes) {
                    return Err(epoch.failure(EndpointError::Unverified));
                }
                Some(bytes)
            } else {
                if record.outcome().is_some() {
                    return Err(epoch.failure(EndpointError::Unverified));
                }
                None
            };
            let mut guards = Vec::new();
            for (parent, side) in [(&source, direction), (&target, 1 - direction)] {
                if parent.side == Side::Target
                    && (relocated.is_some()
                        || if tombstone {
                            outcome.is_some()
                        } else {
                            !shape.stage
                        })
                {
                    if tombstone {
                        epoch.check_absent(parent)?;
                    }
                    guards.push(None);
                    continue;
                }
                if *body
                    .expected
                    .get(side)
                    .ok_or_else(|| epoch.failure(EndpointError::Unverified))?
                    == d::State::Absent
                {
                    epoch.check_absent(parent)?;
                    guards.push(None);
                } else {
                    let guard = epoch.guard_regular(parent)?;
                    if epoch.state(&guard)?
                        != *body
                            .expected
                            .get(side)
                            .ok_or_else(|| epoch.failure(EndpointError::Unverified))?
                    {
                        return Err(epoch.failure(EndpointError::Unstable));
                    }
                    guards.push(Some(guard));
                }
            }
            let staged = if let Some(stage) = &stage {
                let d::Body::Stage(staged) = &stage.body else {
                    return Err(epoch.failure(EndpointError::Unverified));
                };
                let stage_parent = ParentGuard {
                    directory: private.directory.clone(),
                    name: c"stage".to_owned(),
                    epoch: epoch.nonce,
                    side: Side::Target,
                    chain: Vec::new(),
                    path: None,
                };
                if shape.stage && relocated.is_some() {
                    epoch.check_absent(&target)?;
                }
                let guard =
                    epoch.guard_regular(if shape.stage { &stage_parent } else { &target })?;
                let bytes = epoch.capture(&guard)?;
                if descriptor_object(&guard.actual) != staged.object
                    || descriptor_content(&bytes) != staged.content
                {
                    return Err(epoch.failure(EndpointError::Unstable));
                }
                Some(SelectedStage {
                    guard,
                    bytes,
                    descriptor: stage_bytes
                        .clone()
                        .ok_or_else(|| epoch.failure(EndpointError::Unverified))?,
                })
            } else {
                None
            };
            let published = if tombstone {
                outcome.is_some()
            } else {
                !shape.stage
            };
            epoch.barriers(&private.directory, &target.directory)?;
            epoch.namespace_created = true;
            epoch.boundary = if published {
                Boundary::Published
            } else if relocated.is_some() {
                Boundary::DurableQuarantine
            } else {
                Boundary::PrivateStaged
            };
            let target_guard = guards
                .pop()
                .ok_or_else(|| epoch.failure(EndpointError::Unverified))?;
            let source_guard = guards
                .pop()
                .ok_or_else(|| epoch.failure(EndpointError::Unverified))?;
            Ok((
                source,
                target,
                private,
                source_guard,
                target_guard,
                staged,
                published,
                relocated,
                outcome,
            ))
        })();
        let (
            source,
            target,
            private,
            source_guard,
            target_guard,
            staged,
            published,
            relocated,
            outcome,
        ) = result.map_err(|_: Failure| StoreError::RecoveryRequired)?;
        let reader = store.inspect_selected_source(context, epoch.deadline, cancel)?;
        let mut proof = super::NativeRestore {
            published,
            outcome,
            relocated,
            sealed: None,
            epoch,
            revision: context.revision(),
            reader,
            source,
            target,
            source_guard,
            target_guard,
            private: Some(private),
            staged,
        };
        proof.validate_recovery(store, context)?;
        Ok(proof)
    }
}
impl super::NativeRestore<'_> {
    pub(in crate::connection) fn validate_recovery(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        let (target, _, _, selection) = store.selected_recovery_scope(context)?;
        if self.revision != context.revision() || self.sealed.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        self.reader.validate_recovery(store, context)?;
        self.validate_recovery_native(context, target, &selection)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    fn validate_recovery_native(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
        target: u8,
        selection: &d::Selection,
    ) -> std::result::Result<(), StoreError> {
        let result = (|| {
            self.epoch.recovery_check(context)?;
            let private = self
                .private
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            let (_, body, bytes) = private
                .descriptor
                .as_ref()
                .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?;
            if &body.selection != selection {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            for (parent, guard, side) in [
                (
                    &self.source,
                    self.source_guard.as_ref(),
                    usize::from(1 - target),
                ),
                (
                    &self.target,
                    self.target_guard.as_ref(),
                    usize::from(target),
                ),
            ] {
                self.epoch.parent_check(parent)?;
                if parent.side == Side::Target && self.published {
                    continue;
                }
                if parent.side == Side::Target && self.relocated.is_some() {
                    self.epoch.check_absent(parent)?;
                    continue;
                }
                let state = if let Some(guard) = guard {
                    self.epoch.validate_guard(guard, true)?;
                    self.epoch.state(guard)?
                } else {
                    self.epoch.check_absent(parent)?;
                    d::State::Absent
                };
                if state
                    != *body
                        .expected
                        .get(side)
                        .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            self.epoch.namespace_check(private)?;
            self.epoch
                .read_descriptor(&private.directory, c"intent", bytes)?;
            let tombstone = matches!(selection, d::Selection::Tombstone { .. });
            if self.staged.is_some() == tombstone {
                return Err(self.epoch.failure(EndpointError::Unverified));
            }
            if let Some(stage) = &self.staged {
                self.epoch.validate_guard(&stage.guard, true)?;
                if descriptor_content(&self.epoch.capture(&stage.guard)?)
                    != descriptor_content(&stage.bytes)
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
                self.epoch.read_descriptor(
                    &private.directory,
                    c"staged-intent",
                    &stage.descriptor,
                )?;
            } else {
                self.epoch.namespace_names(private, false)?;
                if self.published {
                    self.epoch.check_absent(&self.target)?;
                }
            }
            if let Some((displaced, guard)) = &self.relocated {
                self.epoch.validate_guard(guard, true)?;
                if displaced.actual != guard.actual || !displaced.matches_guard {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
                self.epoch.read_descriptor(
                    &private.directory,
                    c"displaced-intent",
                    displaced.descriptor.as_deref().ok_or_else(|| Failure {
                        _cause: EndpointError::Unverified,
                        _boundary: Boundary::Uncertain,
                        _os_error: None,
                    })?,
                )?;
            }
            if let Some(bytes) = &self.outcome {
                self.epoch
                    .read_descriptor(&private.directory, c"outcome", bytes)?;
                let value =
                    d::decode(bytes).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
                let d::Body::Outcome(body) = value.body else {
                    return Err(self.epoch.failure(EndpointError::Unverified));
                };
                let actual = if let Some(stage) = &self.staged {
                    self.epoch.state(&stage.guard)?
                } else {
                    self.epoch.check_absent(&self.target)?;
                    d::State::Absent
                };
                if !self.published
                    || *body
                        .states
                        .get(usize::from(target))
                        .ok_or_else(|| self.epoch.failure(EndpointError::Unverified))?
                        != actual
                {
                    return Err(self.epoch.failure(EndpointError::Unstable));
                }
            }
            self.epoch.recovery_check(context)
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn activate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.reader.activate(store, live)?;
        self.validate(store, live)
    }
}

impl super::NativeRestore<'_> {
    fn preserve_displaced(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        if self.relocated.is_some() {
            use crate::connection::store::RecoveryState;
            let (target, _, _, _) = store.selected_native_scope(live)?;
            let side = if target == 0 {
                crate::connection::model::Side::Source
            } else {
                crate::connection::model::Side::Destination
            };
            let expected = if target == 0 {
                live.intent().source_before.clone()
            } else {
                live.intent().destination_before.clone()
            };
            let mut existing = None;
            for slot in store.native_recovery_slots(live)? {
                if slot.state == RecoveryState::Reserved {
                    if self.published {
                        return Err(StoreError::RecoveryRequired);
                    }
                    let previous = live.revision();
                    store.retain_native_incomplete(live, slot.id)?;
                    self.advance(store, live, previous)?;
                } else if slot.state == RecoveryState::BytesVerified {
                    if slot.original_side != side || slot.content != expected {
                        return Err(StoreError::RecoveryRequired);
                    }
                    if existing.replace(slot.id).is_some() {
                        return Err(StoreError::RecoveryRequired);
                    }
                }
            }
            let reservation = if existing.is_none() {
                if self.published {
                    return Err(StoreError::RecoveryRequired);
                }
                Some(store.reserve_selected_recovery(live, self)?)
            } else {
                None
            };
            let step =
                session.admit_selected_step(store, live, self, StepKind::Preserve, cancel)?;
            let previous = live.revision();
            let private = self.private.as_ref().ok_or(StoreError::RecoveryRequired)?;
            let (displaced, guard) = self
                .relocated
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            let content = descriptor_content(
                &self
                    .epoch
                    .capture(guard)
                    .map_err(|_| StoreError::RecoveryRequired)?,
            );
            let mut reader = super::NativeRecoveryReader {
                selected_reader: Some(&mut self.reader),
                epoch: &mut self.epoch,
                namespace: private,
                displaced,
                guard,
                admission: step,
                content,
            };
            let bound = if let Some(id) = existing {
                store.rebind_native_recovery(live, id, &mut reader, cancel)?
            } else {
                store.preserve_native(
                    live,
                    reservation.ok_or(StoreError::RecoveryRequired)?,
                    &mut reader,
                    cancel,
                )?
            };
            drop(reader);
            let allowed = if existing.is_some() { 1 } else { 2 };
            if live
                .revision()
                .checked_sub(previous)
                .is_none_or(|delta| delta > allowed)
            {
                return Err(StoreError::StaleRevision);
            }
            self.revision = live.revision();
            let (displaced, guard) = self.relocated.take().ok_or(StoreError::RecoveryRequired)?;
            self.sealed = Some(
                self.epoch
                    .seal_displacement(live, displaced, guard, private, bound)
                    .map_err(|_| StoreError::RecoveryRequired)?,
            );
        }
        self.validate(store, live)
    }
    fn replace(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.relocate_selected(store, live, session, cancel)?;
        self.preserve_displaced(store, live, session, cancel)?;
        self.publish_selected(store, live, session, cancel)
    }
    fn complete_recovered(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.preserve_displaced(store, live, session, cancel)?;
        if !self.published {
            if matches!(
                store.selected_native_scope(live)?.3,
                d::Selection::Tombstone { .. }
            ) {
                return self.apply_tombstone(store, live, session, cancel);
            }
            if self.target_guard.is_some() && self.sealed.is_none() {
                return self.replace(store, live, session, cancel);
            }
            return self.publish_selected(store, live, session, cancel);
        }
        if self.outcome.is_none() {
            let step = session.admit_selected_step(store, live, self, StepKind::Publish, cancel)?;
            self.write_selected_outcome(store, live, &step)?;
            drop(step);
        }
        store.record_recovered_selected_outcome(live, self, cancel)
    }
}

impl<'e> super::NativeRestore<'e> {
    pub(in crate::connection) fn into_collision(
        mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<super::NativeSelectedCollision<'e>, StoreError> {
        self.validate(store, live)?;
        let (target, replace, _, _) = store.selected_native_scope(live)?;
        if replace
            || self.target_guard.is_none()
            || self.private.is_some()
            || self.staged.is_some()
            || self.relocated.is_some()
            || self.sealed.is_some()
            || self.published
            || store
                .native_namespace_records(live)?
                .iter()
                .any(|record| record.operation() == live.id())
        {
            return Err(StoreError::RecoveryRequired);
        }
        let (parents, guards) = if target == 1 {
            (
                [self.source, self.target],
                vec![self.source_guard, self.target_guard],
            )
        } else {
            (
                [self.target, self.source],
                vec![self.target_guard, self.source_guard],
            )
        };
        let mut versions = Vec::new();
        for (index, guard) in guards.iter().enumerate() {
            let state = if let Some(guard) = guard {
                self.epoch
                    .state(guard)
                    .map_err(|_| StoreError::RecoveryRequired)?
            } else {
                self.epoch
                    .check_absent(&*parents.get(index).ok_or(StoreError::Corrupt)?)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                d::State::Absent
            };
            versions.push(d::CapturedVersion {
                side: u8::try_from(index).map_err(|_| StoreError::Corrupt)?,
                role: 0,
                state,
                displaced: None,
            });
        }
        let snapshot = d::ConflictSnapshot {
            connection: live.id().connection.0,
            operation: live.id().serial,
            admission: live.admission(),
            revision: live.revision(),
            fence: live.fence(),
            baseline: live.intent().expected_baseline,
            endpoints: live.endpoints().map(descriptor_endpoint),
            path: live.intent().path.clone(),
            chains: [
                parents[0].chain.iter().map(descriptor_object).collect(),
                parents[1].chain.iter().map(descriptor_object).collect(),
            ],
            versions,
        };
        snapshot.encode().map_err(|_| StoreError::Corrupt)?;
        self.epoch
            .connected
            .as_mut()
            .ok_or(StoreError::RecoveryRequired)?
            .capture_only = true;
        let capture = super::NativeConflictCapture {
            epoch: self.epoch,
            revision: live.revision(),
            snapshot,
            parents,
            guards,
            namespaces: Vec::new(),
        };
        let mut proof = super::NativeSelectedCollision {
            capture,
            selected: self.reader,
        };
        proof.validate(store, live)?;
        Ok(proof)
    }
}
impl<'e> super::NativeSelectedCollision<'e> {
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.selected.validate(store, live)?;
        self.capture.validate(live)
    }
    pub(in crate::connection) fn parts(
        &mut self,
    ) -> (
        &mut super::NativeConflictCapture<'e>,
        &mut crate::connection::store::VerifiedSelection<'e>,
    ) {
        (&mut self.capture, &mut self.selected)
    }
    pub(in crate::connection) fn captured(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        previous: u64,
    ) -> std::result::Result<(), StoreError> {
        use crate::connection::store::ExecutionAction;
        let binding = self
            .capture
            .epoch
            .connected
            .as_mut()
            .ok_or(StoreError::RecoveryRequired)?;
        match (&binding.selected_action, live.action()) {
            (
                Some(ExecutionAction::Selected(before)),
                ExecutionAction::SelectedCollision(after),
            ) if before == after => {}
            _ => return Err(StoreError::RecoveryRequired),
        }
        binding.selected_action = Some(live.action().clone());
        self.selected.advance(store, live, previous)?;
        self.capture.advance(live, previous)
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn recover_selected_collision(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeSelectedCollision<'e>, StoreError> {
        Self::recover_selected_collision_mode(store, context, left, right, cancel, false)
    }
    pub(in crate::connection::transaction) fn inspect_selected_collision_drain(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeSelectedCollision<'e>, StoreError> {
        Self::recover_selected_collision_mode(store, context, left, right, cancel, true)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Keep retained native guards and exact descriptor, generation, ancestry and current-context joins in one proof scope."
    )]
    fn recover_selected_collision_mode(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
        stopped: bool,
    ) -> std::result::Result<super::NativeSelectedCollision<'e>, StoreError> {
        store.check_recovery_context(context)?;
        let snapshot = store.selected_collision_snapshot(context)?;
        if context.recovery_only() != stopped
            || (if stopped {
                snapshot.fence >= context.fence()
            } else {
                snapshot.fence != context.fence()
            })
            || snapshot.versions.len() != 2
            || snapshot.versions.iter().any(|version| version.role != 0)
            || !store.recovery_namespace_records(context)?.is_empty()
        {
            return Err(StoreError::RecoveryRequired);
        }
        if context.endpoints() != &[left.identity(), right.identity()] {
            return Err(StoreError::IdentityChanged);
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&context.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            left,
            right,
            OperationBinding {
                _connection: context.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: context.admission(),
                _fence: context.fence(),
            },
            Arc::clone(context.reads()),
        )
        .map_err(|_| StoreError::RecoveryRequired)?;
        epoch.cancel_read = Some(cancel);
        if !stopped {
            epoch.stop_read = Some(context.gate());
        }
        epoch.connected = Some(ConnectedBinding {
            selected_action: Some(context.action().clone()),
            capture_only: true,
            id: context.id(),
            lifetime: context.lifetime().clone(),
            admission: context.admission(),
            fence: context.fence(),
            direction: 0,
            intent: context.intent().clone(),
        });
        epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let parents = [
            epoch
                .pin_parent(Side::Source, &snapshot.path)
                .map_err(|_| StoreError::RecoveryRequired)?,
            epoch
                .pin_parent(Side::Target, &snapshot.path)
                .map_err(|_| StoreError::RecoveryRequired)?,
        ];
        let mut guards = Vec::new();
        for (index, parent) in parents.iter().enumerate() {
            if parent
                .chain
                .iter()
                .map(descriptor_object)
                .collect::<Vec<_>>()
                != *snapshot.chains.get(index).ok_or(StoreError::Corrupt)?
            {
                return Err(StoreError::IdentityChanged);
            }
            let version = snapshot
                .versions
                .iter()
                .find(|version| usize::from(version.side) == index)
                .ok_or(StoreError::Corrupt)?;
            let guard = match &version.state {
                d::State::Regular { .. } => {
                    let guard = epoch
                        .guard_regular(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    if epoch
                        .state(&guard)
                        .map_err(|_| StoreError::RecoveryRequired)?
                        != version.state
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    Some(guard)
                }
                d::State::Absent => {
                    let names = epoch
                        .owned_recovery_names(context, parent.side, Vec::new())
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    let observed = endpoint::observe_connected(
                        if index == 0 { left } else { right },
                        &names,
                        cancel,
                    );
                    if !matches!(
                        observed.observation(),
                        crate::connection::model::Observation::Complete
                    ) || observed.entries().contains_key(&snapshot.path)
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    epoch
                        .check_absent(parent)
                        .map_err(|_| StoreError::RecoveryRequired)?;
                    None
                }
                _ => return Err(StoreError::RecoveryRequired),
            };
            guards.push(guard);
        }
        let selected = store.inspect_selected_source(context, epoch.deadline, cancel)?;
        let capture = super::NativeConflictCapture {
            epoch,
            revision: context.revision(),
            snapshot,
            parents,
            guards,
            namespaces: Vec::new(),
        };
        let mut proof = super::NativeSelectedCollision { capture, selected };
        proof.validate_recovery(store, context)?;
        Ok(proof)
    }
}
impl super::NativeSelectedCollision<'_> {
    pub(in crate::connection) fn validate_recovery(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        if self.capture.snapshot != store.selected_collision_snapshot(context)? {
            return Err(StoreError::StaleRevision);
        }
        self.selected.validate_recovery(store, context)?;
        self.capture.validate_recovery(context)
    }
    pub(in crate::connection) fn activate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.selected.activate(store, live)?;
        self.validate(store, live)
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn inspect_selected_boundary(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::NativeSelectedBoundary<'e>, StoreError> {
        let proof = Self::recover_selected_mode(store, context, left, right, cancel, true)?;
        let mut boundary = super::NativeSelectedBoundary { proof };
        boundary.validate(store, context)?;
        Ok(boundary)
    }
}
impl super::NativeSelectedBoundary<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(u8, u32, Option<Vec<u8>>, Option<Vec<u8>>), StoreError> {
        if !context.recovery_only() {
            return Err(StoreError::RecoveryRequired);
        }
        self.proof.validate_recovery(store, context)?;
        let private = self
            .proof
            .private
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let (header, _, _) = private
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let kind = if self.proof.published {
            3
        } else if self.proof.relocated.is_some() {
            2
        } else {
            1
        };
        let displaced = self
            .proof
            .relocated
            .as_ref()
            .and_then(|(value, _)| value.descriptor.clone());
        Ok((
            kind,
            u32::try_from(header.attempt).map_err(|_| StoreError::Corrupt)?,
            displaced,
            self.proof.outcome.clone(),
        ))
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.proof.epoch.deadline
    }
    pub(in crate::connection) fn retained_check(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        self.proof.reader.retained_check()?;
        let private = self
            .proof
            .private
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let (_, body, _) = private
            .descriptor
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let target = 1 - body.direction;
        let selection = body.selection.clone();
        self.proof
            .validate_recovery_native(context, target, &selection)
    }
}

impl<'e> super::NativeSelectedBoundary<'e> {
    fn drain_parts(
        &mut self,
    ) -> std::result::Result<
        (
            &mut NativeEpoch<'e>,
            &PrivateNamespace,
            &RelocatedObject,
            &StableFileGuard,
        ),
        StoreError,
    > {
        self.proof.reader.retained_check()?;
        let (displaced, guard) = self
            .proof
            .relocated
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        let private = self
            .proof
            .private
            .as_ref()
            .ok_or(StoreError::RecoveryRequired)?;
        Ok((&mut self.proof.epoch, private, displaced, guard))
    }
    pub(in crate::connection::transaction) fn drain_reader<'a>(
        &'a mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<super::NativeDrainReader<'a, 'e>, StoreError> {
        self.validate(store, context)?;
        let (epoch, _, _, guard) = self.drain_parts()?;
        let captured_bytes = descriptor_content(
            &epoch
                .capture(guard)
                .map_err(|_| StoreError::RecoveryRequired)?,
        );
        Ok(super::NativeDrainReader {
            boundary: super::NativeDrainBoundary::Selected(self),
            content: captured_bytes,
        })
    }
}
impl<'e> super::NativeDrainBoundary<'_, 'e> {
    fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        match self {
            Self::Ordinary(proof) => proof.validate(store, context).map(|_| ()),
            Self::Selected(proof) => proof.validate(store, context).map(|_| ()),
        }
    }
    fn drain_parts(
        &mut self,
    ) -> std::result::Result<
        (
            &mut NativeEpoch<'e>,
            &PrivateNamespace,
            &RelocatedObject,
            &StableFileGuard,
        ),
        StoreError,
    > {
        match self {
            Self::Ordinary(proof) => proof.drain_parts(),
            Self::Selected(proof) => proof.drain_parts(),
        }
    }
    fn advance(
        &mut self,
        previous: u64,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        use super::{NativeSafeBoundary as Boundary, OwnedBoundary};
        if previous.checked_add(1) != Some(context.revision()) || !context.recovery_only() {
            return Err(StoreError::StaleRevision);
        }
        match self {
            Self::Ordinary(boundary) => {
                let (Boundary::DisplacedAccounted(proof) | Boundary::PublishedAccounted(proof)) =
                    &mut **boundary
                else {
                    return Err(StoreError::RecoveryRequired);
                };
                let (revision, epoch) = match proof {
                    OwnedBoundary::Regular(proof) => {
                        (&mut proof.context_revision, &mut proof.epoch)
                    }
                    OwnedBoundary::Deletion(proof) => {
                        (&mut proof.context_revision, &mut proof.epoch)
                    }
                };
                if *revision != previous {
                    return Err(StoreError::StaleRevision);
                }
                epoch
                    .recovery_check(context)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                *revision = context.revision();
            }
            Self::Selected(boundary) => {
                if boundary.proof.revision != previous {
                    return Err(StoreError::StaleRevision);
                }
                boundary
                    .proof
                    .epoch
                    .recovery_check(context)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                boundary
                    .proof
                    .reader
                    .advance_inspection(previous, context)?;
                boundary.proof.revision = context.revision();
            }
        }
        Ok(())
    }
}
impl super::NativeDrainReader<'_, '_> {
    pub(in crate::connection) fn is_selected(&self) -> bool {
        matches!(self.boundary, super::NativeDrainBoundary::Selected(_))
    }
}

impl<'e> NativeEpoch<'e> {
    pub(in crate::connection::transaction) fn inspect_selected_untouched(
        store: &mut crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> std::result::Result<super::SelectedNeverBegun<'e>, StoreError> {
        store.selected_recovery_scope(context)?;
        let history = store.never_begun_identity(context)?;
        let direction = usize::from(history.direction);
        let (source, target) = if direction == 0 {
            (left, right)
        } else {
            (right, left)
        };
        if *context
            .endpoints()
            .get(direction)
            .ok_or(StoreError::Corrupt)?
            != source.identity()
            || *context
                .endpoints()
                .get(1 - direction)
                .ok_or(StoreError::Corrupt)?
                != target.identity()
        {
            return Err(StoreError::IdentityChanged);
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&context.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            source,
            target,
            OperationBinding {
                _connection: context.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: context.admission(),
                _fence: context.fence(),
            },
            Arc::clone(context.reads()),
        )
        .map_err(|_| StoreError::RecoveryRequired)?;
        epoch.cancel_read = Some(cancel);
        if !context.recovery_only() {
            epoch.stop_read = Some(context.gate());
        }
        epoch.connected = Some(ConnectedBinding {
            selected_action: Some(context.action().clone()),
            capture_only: false,
            id: context.id(),
            lifetime: context.lifetime().clone(),
            admission: context.admission(),
            fence: context.fence(),
            direction,
            intent: context.intent().clone(),
        });
        let native = epoch
            .inspect_untouched_inner(store, context, cancel)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let selected = store.inspect_selected_source(context, native.deadline(), cancel)?;
        let mut proof = super::SelectedNeverBegun { native, selected };
        proof.validate(store, context)?;
        Ok(proof)
    }
}
impl super::SelectedNeverBegun<'_> {
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.native.deadline()
    }
    pub(in crate::connection) fn validate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        store.selected_recovery_scope(context)?;
        self.selected.validate_recovery(store, context)?;
        self.native.validate_join(store, context)
    }
    pub(in crate::connection) fn retained_check(
        &mut self,
        context: &crate::connection::store::RecoveryContext,
    ) -> std::result::Result<(), StoreError> {
        self.selected.retained_check()?;
        self.native
            .validate_native(context)
            .map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn activate(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.selected.activate(store, live)
    }
}

impl super::NativeRestore<'_> {
    pub(in crate::connection) fn require_uncreated_namespace(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        if self.private.is_some() || self.staged.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        let name = CString::new(record.name()).map_err(|_| StoreError::Corrupt)?;
        match open_at(self.epoch.target_root.file(), &name, libc::O_PATH) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.validate(store, live),
            _ => Err(StoreError::RecoveryRequired),
        }
    }
}

impl super::NativeRestore<'_> {
    fn apply_tombstone(
        &mut self,
        store: &mut crate::connection::store::ConnectionStore,
        live: &mut LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<(), StoreError> {
        self.validate(store, live)?;
        if !matches!(
            store.selected_native_scope(live)?.3,
            d::Selection::Tombstone { .. }
        ) || self.staged.is_some()
            || self.published
        {
            return Err(StoreError::RecoveryRequired);
        }
        if self.target_guard.is_some() && self.sealed.is_none() && self.relocated.is_none() {
            self.relocate_selected(store, live, session, cancel)?;
        }
        self.preserve_displaced(store, live, session, cancel)?;
        store.check_selected_prepared(live, self)?;
        let step = session.admit_selected_step(store, live, self, StepKind::Publish, cancel)?;
        self.epoch
            .check_absent(&self.target)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let private = self.private.as_ref().ok_or(StoreError::RecoveryRequired)?;
        self.epoch
            .barriers(&private.directory, &self.target.directory)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.published = true;
        self.write_selected_outcome(store, live, &step)?;
        drop(step);
        store.record_selected_outcome(live, self, false, cancel)?;
        store.record_selected_outcome(live, self, true, cancel)
    }
}

// Finite credit per executor invocation. Reopen reserves a new durable window;
// phases within this invocation share the same canonical paid context.
const EXECUTOR_READ_WINDOW: u64 = 64 * 1024 * 1024;
type ExecutorResult =
    std::result::Result<crate::connection::transaction::TransactionOutcome, StoreError>;
fn executor_error(
    store: &crate::connection::store::ConnectionStore,
    id: ConnectionId,
    error: StoreError,
) -> crate::connection::transaction::TransactionOutcome {
    crate::connection::transaction::TransactionOutcome::RecoveryRequired {
        operation: store
            .executor_position(id)
            .ok()
            .and_then(|position| position.0),
        error,
    }
}
pub(in crate::connection) fn execute_next_regular(
    store: &mut crate::connection::store::ConnectionStore,
    id: ConnectionId,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> crate::connection::transaction::TransactionOutcome {
    let result = execute_next_inner(store, id, session, left, right, cancel);
    result.unwrap_or_else(|error| executor_error(store, id, error))
}
pub(in crate::connection) fn execute_reviewed_initial(
    store: &mut crate::connection::store::ConnectionStore,
    reviewed: crate::connection::transaction::ReviewedInitial,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> crate::connection::transaction::TransactionOutcome {
    let intent = reviewed.selection.clone();
    let id = intent.connection;
    let result = (|| {
        reviewed.validate(store, &intent)?;
        store.executor_session(id, session)?;
        session.require_idle()?;
        if store.executor_position(id)?.0.is_some() {
            return Err(StoreError::RecoveryRequired);
        }
        if let Some(reason) = reviewed.regular_prerequisite(store)? {
            return Ok(
                crate::connection::transaction::TransactionOutcome::StructuralPending(reason),
            );
        }
        let metadata = admission_metadata(store, &intent, left, right, cancel)?;
        let mut live = store.begin_native_reviewed(intent, session, reviewed, metadata)?;
        store.reserve_native_work(
            &mut live,
            session,
            EXECUTOR_READ_WINDOW,
            Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )?;
        execute_admitted(store, live, session, left, right, cancel)
    })();
    result.unwrap_or_else(|error| executor_error(store, id, error))
}
pub(in crate::connection) fn resolve_regular(
    store: &mut crate::connection::store::ConnectionStore,
    reviewed: crate::connection::store::ReviewedSelection,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> crate::connection::transaction::TransactionOutcome {
    let intent = reviewed.intent().clone();
    let id = intent.connection;
    let result = (|| {
        store.executor_session(id, session)?;
        session.require_idle()?;
        let metadata = admission_metadata(store, &intent, left, right, cancel)?;
        let mut live = store.begin_native_selected(reviewed, session, metadata)?;
        store.reserve_native_work(
            &mut live,
            session,
            EXECUTOR_READ_WINDOW,
            Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )?;
        execute_selected_admitted(store, live, session, left, right, cancel)
    })();
    result.unwrap_or_else(|error| executor_error(store, id, error))
}
fn execute_next_inner(
    store: &mut crate::connection::store::ConnectionStore,
    id: ConnectionId,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> ExecutorResult {
    execute_next_inner_mode(store, id, session, left, right, cancel, false)
}
#[expect(
    clippy::too_many_lines,
    reason = "Pending-first observation and typed planning share one bounded executor dispatch"
)]
fn execute_next_inner_mode(
    store: &mut crate::connection::store::ConnectionStore,
    id: ConnectionId,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
    directory_only: bool,
) -> ExecutorResult {
    use crate::connection::{
        store::Lifecycle,
        transaction::{RegularPlan, TransactionOutcome},
    };
    store.executor_session(id, session)?;
    let record = store.load_connection(id)?;
    if record.source.identity != left.identity() || record.destination.identity != right.identity()
    {
        return Err(StoreError::IdentityChanged);
    }
    let (pending, latest) = store.executor_position(id)?;
    if let Some(id) = pending {
        return execute_pending(store, id, session, left, right, cancel, true);
    }
    if record.lifecycle != Lifecycle::Active {
        return Ok(TransactionOutcome::AccountedPending {
            operation: latest.map(|id| store.executor_receipt(id)).transpose()?,
        });
    }
    session.require_idle()?;
    let absent = || {
        crate::connection::model::VerifiedBaseline::absent(
            id,
            record.source.identity,
            record.destination.identity,
        )
    };
    let (source, destination, baseline) = if let Some(id) = latest {
        let mut context = store.open_recovery_context(id, record.revision)?;
        store.reserve_recovery_reads(
            &mut context,
            EXECUTOR_READ_WINDOW,
            Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )?;
        let mut epoch = NativeEpoch::begin_observation(&context, left, right, cancel)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let (source, destination) = epoch.observe_pair(store, &context, cancel)?;
        let baseline = if record.baseline_revision.is_some() {
            store.load_recovery_baseline(&context, epoch.deadline, cancel)?
        } else {
            absent()
        };
        (source, destination, baseline)
    } else {
        if record.baseline_revision.is_some() {
            store.operationless_baseline(id)?;
            let (source, destination) =
                initial_baseline::observe_current(store, id, session, left, right, cancel)?;
            (source, destination, store.load_baseline(id)?)
        } else {
            (
                endpoint::observe(left, cancel),
                endpoint::observe(right, cancel),
                absent(),
            )
        }
    };
    if record.baseline_revision.is_some() {
        use crate::connection::transaction::directory::{DirectoryPlan, plan_directory};
        match plan_directory(store, id, &baseline, &source, &destination)? {
            DirectoryPlan::Ready(intent) => {
                let admission = directory::NativeDirectoryAdmission::capture(
                    store,
                    &intent,
                    [left, right],
                    cancel,
                )?;
                let mut live = store.begin_native_directory(intent, session, admission)?;
                #[cfg(test)]
                directory::test_point("intent-record");
                store.reserve_native_work(
                    &mut live,
                    session,
                    EXECUTOR_READ_WINDOW,
                    Instant::now() + std::time::Duration::from_secs(30),
                    cancel,
                )?;
                return directory::execute_admitted(store, live, session, left, right, cancel);
            }
            DirectoryPlan::StructuralPending(reason) if directory_only => {
                return Ok(TransactionOutcome::StructuralPending(reason));
            }
            DirectoryPlan::Settled if directory_only => {
                return match latest {
                    Some(operation) => executor_verified(store, operation, left, right, cancel),
                    None => {
                        initial_baseline::verified_current(store, id, session, left, right, cancel)
                    }
                };
            }
            _ => (),
        }
    }
    if directory_only {
        return Ok(TransactionOutcome::AccountedPending {
            operation: latest.map(|id| store.executor_receipt(id)).transpose()?,
        });
    }
    match crate::connection::transaction::plan_regular(store, id, &baseline, source, destination)? {
        RegularPlan::InitialReview(preview) => Ok(TransactionOutcome::InitialReview(preview)),
        RegularPlan::StructuralPending(reason) => Ok(TransactionOutcome::StructuralPending(reason)),
        RegularPlan::Ready(intent) => {
            let metadata = admission_metadata(store, &intent, left, right, cancel)?;
            let mut live = store.begin_native(intent, session, metadata)?;
            store.reserve_native_work(
                &mut live,
                session,
                EXECUTOR_READ_WINDOW,
                Instant::now() + std::time::Duration::from_secs(30),
                cancel,
            )?;
            execute_admitted(store, live, session, left, right, cancel)
        }
        RegularPlan::Pending => Ok(TransactionOutcome::AccountedPending {
            operation: latest.map(|id| store.executor_receipt(id)).transpose()?,
        }),
        RegularPlan::Settled => match latest {
            Some(operation) => executor_verified(store, operation, left, right, cancel),
            None => initial_baseline::verified_current(store, id, session, left, right, cancel),
        },
    }
}
impl<'e> NativeEpoch<'e> {
    fn begin_observation(
        context: &crate::connection::store::RecoveryContext,
        left: &'e PinnedEndpoint,
        right: &'e PinnedEndpoint,
        cancel: &'e std::sync::atomic::AtomicBool,
    ) -> Result<Self> {
        if context.endpoints() != &[left.identity(), right.identity()] {
            return Err(Failure {
                _cause: EndpointError::IdentityChanged,
                _boundary: Boundary::Untouched,
                _os_error: None,
            });
        }
        let mut operation = [0; 16];
        operation[..8].copy_from_slice(&context.id().serial.to_le_bytes());
        let mut epoch = Self::begin(
            left,
            right,
            OperationBinding {
                _connection: context.id().connection,
                _operation: operation,
                _store_session: [0; 16],
                _revision: context.admission(),
                _fence: context.fence(),
            },
            Arc::clone(context.reads()),
        )?;
        epoch.cancel_read = Some(cancel);
        if context.native_kind() == Some(crate::connection::store::NativeOperationKind::DirectoryV1)
        {
            epoch.directory_requests = Some(Arc::new(std::sync::atomic::AtomicU16::new(0)));
        }
        if !context.recovery_only() {
            epoch.stop_read = Some(context.gate());
        }
        epoch.connected = Some(ConnectedBinding {
            selected_action: Some(context.action().clone()),
            capture_only: false,
            id: context.id(),
            lifetime: context.lifetime().clone(),
            admission: context.admission(),
            fence: context.fence(),
            direction: 0,
            intent: context.intent().clone(),
        });
        epoch.recovery_check(context)?;
        Ok(epoch)
    }
    fn observe_pair(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        context: &crate::connection::store::RecoveryContext,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<
        (
            crate::connection::model::ObservedTree,
            crate::connection::model::ObservedTree,
        ),
        StoreError,
    > {
        let source = self
            .owned_recovery_names(
                context,
                Side::Source,
                store.recovery_namespace_records(context)?,
            )
            .map_err(|_| StoreError::RecoveryRequired)?;
        let target = self
            .owned_recovery_names(
                context,
                Side::Target,
                store.recovery_namespace_records(context)?,
            )
            .map_err(|_| StoreError::RecoveryRequired)?;
        let result = (
            endpoint::observe_connected(self.source, &source, cancel),
            endpoint::observe_connected(self.target, &target, cancel),
        );
        self.recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        Ok(result)
    }
}
fn executor_verified(
    store: &mut crate::connection::store::ConnectionStore,
    id: crate::connection::store::OperationId,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> ExecutorResult {
    let current = store.load_connection(id.connection)?;
    let context = store.open_recovery_context(id, current.revision)?;
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(StoreError::Cancelled);
    }
    let up_to_date = if store.final_context_ready(&context)? {
        let mut epoch = NativeEpoch::begin_observation(&context, left, right, cancel)
            .map_err(|_| StoreError::RecoveryRequired)?;
        epoch.final_comparison(store, &context, cancel)?
    } else {
        false
    };
    Ok(
        crate::connection::transaction::TransactionOutcome::Verified {
            operation: store.executor_receipt(id)?,
            up_to_date,
        },
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "One exhaustive dispatcher makes action-specific authority and recovery-only branch exclusions auditable."
)]
fn execute_admitted(
    store: &mut crate::connection::store::ConnectionStore,
    mut live: LiveOperation,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> ExecutorResult {
    use crate::connection::{
        model::{Decision, Side as ModelSide},
        transaction::TransactionOutcome,
    };
    let decision = live.intent().decision.clone();
    if !live.action().is_reconciliation() {
        return Err(StoreError::RecoveryRequired);
    }
    if matches!(decision, Decision::Conflict(_)) {
        let mut capture = NativeEpoch::capture_conflict(store, &live, left, right, cancel)?;
        store.record_native_capture(&mut live, &mut capture, cancel)?;
        store.preserve_native_conflict(&mut live, &mut capture, cancel)?;
        let conflict = store
            .record_native_conflict(&mut live, &mut capture, cancel)?
            .value()
            .clone();
        return Ok(TransactionOutcome::Conflict(conflict));
    }
    let reverse = matches!(
        decision,
        Decision::Propagate {
            from: ModelSide::Destination
        } | Decision::RecoverDeletion {
            at: ModelSide::Source
        }
    );
    let (source_endpoint, target_endpoint) = if reverse {
        (right, left)
    } else {
        (left, right)
    };
    let mut epoch = NativeEpoch::begin_connected(&live, source_endpoint, target_endpoint, cancel)
        .map_err(|_| StoreError::RecoveryRequired)?;
    if decision == Decision::VerifyEqual {
        let mut evidence = epoch
            .equal_evidence(&live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        // A resumed equality still owns a newly acquired paid proof. Skip only
        // the duplicate durable fact, never its fresh comparison or commit checks.
        if store.executor_receipt(live.id())?.stage
            != crate::connection::store::ReceiptStage::Verified
        {
            store.record_native_equal(&mut live, &mut evidence, cancel)?;
        }
        let mut comparison = evidence.comparison(store, &live, cancel)?;
        let candidate = store.prepare_baseline(&live, &mut comparison, cancel)?;
        store.commit_baseline(&mut live, candidate, &mut comparison, cancel)?;
        drop(comparison);
        drop(evidence);
        return executor_verified(store, live.id(), left, right, cancel);
    }
    let source = epoch
        .pin_parent(Side::Source, &live.intent().path)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let target = epoch
        .pin_parent(Side::Target, &live.intent().path)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let target_side = if reverse {
        ModelSide::Source
    } else {
        ModelSide::Destination
    };
    let reservation = store.reserve_executor_namespace(&mut live, target_side)?;
    match decision {
        Decision::Propagate { .. } => {
            let source_guard = epoch
                .guard_regular(&source)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let expected_target = if reverse {
                live.intent().source_before.as_ref()
            } else {
                live.intent().destination_before.as_ref()
            };
            let target_guard = if expected_target.is_some() {
                Some(
                    epoch
                        .guard_regular(&target)
                        .map_err(|_| StoreError::RecoveryRequired)?,
                )
            } else {
                epoch
                    .check_absent(&target)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                None
            };
            let step = session.admit_step(store, &live, StepKind::Namespace, cancel)?;
            let private = epoch
                .create_registered_namespace(
                    &live,
                    &reservation,
                    &source,
                    &target,
                    &source_guard,
                    target_guard.as_ref(),
                    step,
                )
                .map_err(|_| StoreError::RecoveryRequired)?;
            let step = session.admit_step(store, &live, StepKind::Stage, cancel)?;
            let staged = epoch
                .stage_connected(&live, &source_guard, &private, step)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let prepared = epoch
                .prepared(&private, Some(&staged))
                .map_err(|_| StoreError::RecoveryRequired)?;
            store.prepare_native(&mut live, reservation, prepared)?;
        }
        Decision::RecoverDeletion { .. } => {
            epoch.verify_deletion_scope(store, &mut live, cancel)?;
            let target_guard = epoch
                .guard_regular(&target)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let step = session.admit_step(store, &live, StepKind::Namespace, cancel)?;
            let private = epoch
                .create_deletion_namespace(
                    &live,
                    &reservation,
                    &source,
                    &target,
                    &target_guard,
                    step,
                )
                .map_err(|_| StoreError::RecoveryRequired)?;
            let prepared = epoch
                .prepared(&private, None)
                .map_err(|_| StoreError::RecoveryRequired)?;
            store.prepare_native(&mut live, reservation, prepared)?;
        }
        _ => return Err(StoreError::RecoveryRequired),
    }
    // One canonical operation and read window; fresh reconstruction reacquires
    // all guards before relocation/publication rather than trusting preparation.
    drop(epoch);
    drop(source);
    drop(target);
    execute_pending(store, live.id(), session, left, right, cancel, false)
}
fn execute_selected_admitted(
    store: &mut crate::connection::store::ConnectionStore,
    mut live: LiveOperation,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
) -> ExecutorResult {
    use crate::connection::transaction::TransactionOutcome;
    let (_, replace, _, selection) = store.selected_native_scope(&live)?;
    let tombstone = matches!(selection, d::Selection::Tombstone { .. });
    let mut proof = if tombstone {
        NativeEpoch::selected_tombstone(store, &live, left, right, cancel)?
    } else {
        NativeEpoch::selected_restore(store, &live, left, right, cancel)?
    };
    if !replace && proof.target_guard.is_some() {
        let mut collision = proof.into_collision(store, &live)?;
        store.record_selected_collision(&mut live, &mut collision, cancel)?;
        store.preserve_selected_collision(&mut live, &mut collision, cancel)?;
        return Ok(TransactionOutcome::Conflict(
            store
                .complete_selected_collision(&mut live, &mut collision, cancel)?
                .value()
                .clone(),
        ));
    }
    proof.prepare(store, &mut live, session, cancel)?;
    if tombstone {
        proof.apply_tombstone(store, &mut live, session, cancel)?;
    } else if proof.target_guard.is_some() {
        proof.replace(store, &mut live, session, cancel)?;
    } else {
        proof.publish_absent(store, &mut live, session, cancel)?;
    }
    let operation = store
        .finish_selected(&mut live, proof, cancel)?
        .value()
        .clone();
    Ok(TransactionOutcome::Verified {
        operation,
        up_to_date: false,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "One exhaustive dispatcher makes action-specific authority and recovery-only branch exclusions auditable."
)]
fn execute_pending(
    store: &mut crate::connection::store::ConnectionStore,
    id: crate::connection::store::OperationId,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &std::sync::atomic::AtomicBool,
    reserve: bool,
) -> ExecutorResult {
    use crate::connection::{
        model::{Decision, Side as ModelSide},
        store::{ExecutionAction, ReceiptStage},
        transaction::TransactionOutcome,
    };
    let current = store.load_connection(id.connection)?;
    let mut context = store.open_recovery_context(id, current.revision)?;
    if store.legacy_native_pending(id)? {
        return Err(StoreError::LegacyNativePending);
    }
    if reserve {
        store.reserve_recovery_reads(
            &mut context,
            EXECUTOR_READ_WINDOW,
            Instant::now() + std::time::Duration::from_secs(30),
            cancel,
        )?;
    }
    if context.native_kind() == Some(crate::connection::store::NativeOperationKind::DirectoryV1) {
        return directory::execute_pending(store, &context, session, left, right, cancel);
    }
    let action = context.action().clone();
    if matches!(action, ExecutionAction::Captured) {
        let mut proof = NativeEpoch::recover_conflict(store, &context, left, right, cancel)?;
        if context.recovery_only() {
            store.drain_native_capture(&mut context, &mut proof, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let mut live = store.resume_native_conflict(&context, &mut proof, session, cancel)?;
        store.preserve_native_conflict(&mut live, &mut proof, cancel)?;
        return Ok(TransactionOutcome::Conflict(
            store
                .record_native_conflict(&mut live, &mut proof, cancel)?
                .value()
                .clone(),
        ));
    }
    if matches!(action, ExecutionAction::SelectedCollision(_)) {
        let mut proof = if context.recovery_only() {
            NativeEpoch::inspect_selected_collision_drain(store, &context, left, right, cancel)?
        } else {
            NativeEpoch::recover_selected_collision(store, &context, left, right, cancel)?
        };
        if context.recovery_only() {
            store.drain_selected_capture(&mut context, &mut proof, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let mut live = store.resume_selected_collision(&context, &mut proof, session, cancel)?;
        store.preserve_selected_collision(&mut live, &mut proof, cancel)?;
        return Ok(TransactionOutcome::Conflict(
            store
                .complete_selected_collision(&mut live, &mut proof, cancel)?
                .value()
                .clone(),
        ));
    }
    let mut records = store
        .recovery_namespace_records(&context)?
        .into_iter()
        .filter(|record| record.operation() == id);
    let record = records.next();
    if records.next().is_some() {
        return Err(StoreError::RecoveryRequired);
    }
    let prepared = record
        .as_ref()
        .is_some_and(|record| record.descriptors().is_ok());
    if matches!(action, ExecutionAction::Selected(_)) {
        if !prepared {
            let mut proof =
                NativeEpoch::inspect_selected_untouched(store, &context, left, right, cancel)?;
            if context.recovery_only() {
                store.record_selected_untouched(&context, &mut proof, cancel)?;
                return Ok(TransactionOutcome::AccountedPending {
                    operation: Some(store.executor_receipt(id)?),
                });
            }
            let live = store.resume_selected_untouched(&context, &mut proof, session, cancel)?;
            drop(proof);
            return execute_selected_admitted(store, live, session, left, right, cancel);
        }
        if context.recovery_only() {
            let mut boundary =
                NativeEpoch::inspect_selected_boundary(store, &context, left, right, cancel)?;
            let (_, _, displaced, _) = boundary.validate(store, &context)?;
            if displaced.is_some() {
                let mut reader = boundary.drain_reader(store, &context)?;
                store.drain_selected_recovery(&mut context, &mut reader, cancel)?;
            }
            store.record_selected_boundary(&context, &mut boundary, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let tombstone = matches!(
            store.selected_recovery_scope(&context)?.3,
            d::Selection::Tombstone { .. }
        );
        let mut proof = if tombstone {
            NativeEpoch::recover_selected_tombstone(store, &context, left, right, cancel)?
        } else {
            NativeEpoch::recover_selected(store, &context, left, right, cancel)?
        };
        let mut live = store.resume_native_selected(&context, &mut proof, session, cancel)?;
        proof.complete_recovered(store, &mut live, session, cancel)?;
        let operation = store
            .finish_selected(&mut live, proof, cancel)?
            .value()
            .clone();
        return Ok(TransactionOutcome::Verified {
            operation,
            up_to_date: false,
        });
    }
    if !action.is_reconciliation() {
        return Err(StoreError::RecoveryRequired);
    }
    let reverse = matches!(
        context.intent().decision,
        Decision::Propagate {
            from: ModelSide::Destination
        } | Decision::RecoverDeletion {
            at: ModelSide::Source
        }
    );
    let (source, target) = if reverse {
        (right, left)
    } else {
        (left, right)
    };
    let epoch = NativeEpoch::begin_recovery(&context, source, target, cancel)
        .map_err(|_| StoreError::RecoveryRequired)?;
    if !prepared {
        let mut proof = epoch
            .inspect_never_begun(store, &context, cancel)
            .map_err(|_| StoreError::RecoveryRequired)?;
        if context.recovery_only() {
            store.record_never_begun(&context, &mut proof, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let live = store.resume_native_untouched(&context, &mut proof, session, cancel)?;
        drop(proof);
        return execute_admitted(store, live, session, left, right, cancel);
    }
    let record = record.ok_or(StoreError::RecoveryRequired)?;
    if matches!(context.intent().decision, Decision::RecoverDeletion { .. }) {
        let mut proof = epoch
            .inspect_deletion(&context, &record, store, cancel)
            .map_err(|_| StoreError::RecoveryRequired)?;
        if context.recovery_only() {
            let mut boundary = proof.safe_boundary(&context, cancel)?;
            if matches!(
                boundary,
                super::NativeSafeBoundary::DisplacedAccounted(_)
                    | super::NativeSafeBoundary::PublishedAccounted(_)
            ) {
                let mut reader = boundary.drain_reader(store, &context)?;
                store.drain_native_recovery(&mut context, &mut reader, cancel)?;
            }
            store.record_boundary(&context, &mut boundary, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let mut live = store.resume_native_deletion(&context, &mut proof, session, cancel)?;
        let mut outcome = proof.complete_deletion(store, &mut live, session, cancel)?;
        if !matches!(
            store.executor_receipt(id)?.stage,
            ReceiptStage::Published | ReceiptStage::Verified
        ) {
            store.record_native_deletion(&mut live, &mut outcome, false, cancel)?;
        }
        if store.executor_receipt(id)?.stage != ReceiptStage::Verified {
            store.record_native_deletion(&mut live, &mut outcome, true, cancel)?;
        }
        let mut comparison = outcome.comparison(store, &live, cancel)?;
        let candidate = store.prepare_baseline(&live, &mut comparison, cancel)?;
        store.commit_baseline(&mut live, candidate, &mut comparison, cancel)?;
    } else {
        let mut proof = epoch
            .inspect_regular(&context, &record)
            .map_err(|_| StoreError::RecoveryRequired)?;
        if context.recovery_only() {
            let mut boundary = proof.safe_boundary(&context, cancel)?;
            if matches!(
                boundary,
                super::NativeSafeBoundary::DisplacedAccounted(_)
                    | super::NativeSafeBoundary::PublishedAccounted(_)
            ) {
                let mut reader = boundary.drain_reader(store, &context)?;
                store.drain_native_recovery(&mut context, &mut reader, cancel)?;
            }
            store.record_boundary(&context, &mut boundary, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let mut live = store.resume_native(&context, &mut proof, session, cancel)?;
        let mut outcome = proof.complete_regular(store, &mut live, session, cancel)?;
        if !matches!(
            store.executor_receipt(id)?.stage,
            ReceiptStage::Published | ReceiptStage::Verified
        ) {
            store.record_native_outcome(&mut live, &mut outcome, false, cancel)?;
        }
        if store.executor_receipt(id)?.stage != ReceiptStage::Verified {
            store.record_native_outcome(&mut live, &mut outcome, true, cancel)?;
        }
        let mut comparison = outcome.comparison(store, &live, cancel)?;
        let candidate = store.prepare_baseline(&live, &mut comparison, cancel)?;
        store.commit_baseline(&mut live, candidate, &mut comparison, cancel)?;
    }
    executor_verified(store, id, left, right, cancel)
}

impl NativeEpoch<'_> {
    /// A separate per-epoch work bound, never fictional payload-byte credit.
    /// Actual descriptor, payload and page requests still use `OperationReads`.
    fn admit_ancestor_chain(&mut self, depth: usize) -> Result<()> {
        self.check()?;
        if !(1..=64).contains(&depth) || self.ancestor_chains >= 256 {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        self.ancestor_chains += 1;
        Ok(())
    }
    fn prove_existing_ancestors(&mut self, bytes: Option<&[u8]>) -> Result<()> {
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let admission =
            d::Admission::decode(bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        if admission.path.components().len() == 1 {
            return Ok(());
        }
        let binding = self.connected.as_ref().ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: self.boundary,
            _os_error: None,
        })?;
        if admission.connection != binding.id.connection.0
            || admission.operation != binding.id.serial
            || admission.revision.checked_add(1) != Some(binding.admission)
            || admission.fence != binding.fence
            || admission.baseline != binding.intent.expected_baseline
            || admission.path != binding.intent.path
            || usize::from(admission.direction) != binding.direction
            || admission.endpoints.get(usize::from(admission.direction))
                != Some(&descriptor_endpoint(self.identities[0]))
            || admission
                .endpoints
                .get(1 - usize::from(admission.direction))
                != Some(&descriptor_endpoint(self.identities[1]))
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.admit_ancestor_chain(admission.path.components().len())?;
        let (source, _) = self.pin_parent_sample(Side::Source, &admission.path)?;
        self.admit_ancestor_chain(admission.path.components().len())?;
        let (target, _) = self.pin_parent_sample(Side::Target, &admission.path)?;
        self.ancestors = Some(super::ExistingAncestorProof {
            admission,
            parents: [source, target],
            stability: None,
        });
        self.validate_existing_ancestors(false)
    }
    fn validate_existing_ancestors(&mut self, start_window: bool) -> Result<()> {
        if self.directory_requests.is_some() {
            return self.validate_directory_ancestors(start_window);
        }
        let Some(mut proof) = self.ancestors.take() else {
            return Ok(());
        };
        let result = (|| {
            let mut stamps = [Vec::new(), Vec::new()];
            for (index, ((original, stamp_slot), side)) in proof
                .parents
                .iter()
                .zip(&mut stamps)
                .zip([Side::Source, Side::Target])
                .enumerate()
            {
                self.admit_ancestor_chain(proof.admission.path.components().len())?;
                self.parent_check(original)?;
                // One full temporary chain at a time, never a repin per prefix.
                let (fresh, observed) = self.pin_parent_sample(side, &proof.admission.path)?;
                let logical = if index == 0 {
                    usize::from(proof.admission.direction)
                } else {
                    1 - usize::from(proof.admission.direction)
                };
                let expected = proof
                    .admission
                    .chains
                    .get(logical)
                    .ok_or_else(|| self.failure(EndpointError::Unverified))?;
                if original
                    .chain
                    .iter()
                    .map(descriptor_object)
                    .collect::<Vec<_>>()
                    != *expected
                    || fresh
                        .chain
                        .iter()
                        .map(descriptor_object)
                        .collect::<Vec<_>>()
                        != *expected
                    || proof
                        .stability
                        .as_ref()
                        .and_then(|expected| expected.get(index))
                        .is_some_and(|expected| *expected != observed)
                {
                    return Err(self.failure(EndpointError::IdentityChanged));
                }
                self.parent_check(original)?;
                self.parent_check(&fresh)?;
                *stamp_slot = observed;
            }
            if start_window && proof.stability.is_none() {
                proof.stability = Some(stamps);
            }
            self.check()
        })();
        self.ancestors = Some(proof);
        result
    }
    fn comparison_ancestors(
        &mut self,
        store: &crate::connection::store::ConnectionStore,
        live: &LiveOperation,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<Option<crate::connection::store::ExistingAncestorPatchV1>, StoreError>
    {
        self.connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate_existing_ancestors(true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let patch = store.native_ancestor_patch(live, self.deadline, cancel)?;
        if let Some(patch) = &patch {
            let proof = self
                .ancestors
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            let encoded = proof.admission.encode().map_err(|_| StoreError::Corrupt)?;
            if patch.admission != <[u8; 32]>::from(Sha256::digest(encoded)) {
                return Err(StoreError::RecoveryRequired);
            }
        }
        self.connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        Ok(patch)
    }
}

#[cfg(test)]
mod initial_parent_tests {
    use super::*;
    use crate::connection::{
        model::{Decision, EntryState},
        store::{ConnectionPolicy, ConnectionStore, EndpointBinding, OperationIntent},
        transaction::TransactionOutcome,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    fn change_parent(root: &std::path::Path, fault: u8) {
        use std::os::unix::fs::{PermissionsExt, symlink};
        if fault == 6 {
            std::fs::write(root.join("a/b/competitor"), b"unrelated").unwrap();
            return;
        }
        if fault == 5 {
            std::fs::set_permissions(root.join("a"), std::fs::Permissions::from_mode(0o1700))
                .unwrap();
            return;
        }
        std::fs::rename(root.join("a"), root.join("retained-a")).unwrap();
        match fault {
            1 => {
                std::fs::create_dir_all(root.join("a/b")).unwrap();
                std::fs::write(root.join("a/b/file"), b"equal").unwrap();
            }
            2 => (),
            3 => symlink(root.join("retained-a"), root.join("a")).unwrap(),
            4 => std::fs::write(root.join("a"), b"competitor").unwrap(),
            _ => panic!("fault"),
        }
    }

    #[test]
    #[ignore = "authorized native filesystem/helper required; source and link evidence only, UNRUN"]
    fn native_existing_ancestors_restart_and_refusal_matrix() {
        for side in 0..2 {
            for boundary in 0..=5 {
                for fault in 0..=5 {
                    existing_parent_boundary(side, boundary, fault);
                }
                if boundary == 5 {
                    for fault in 6..=8 {
                        existing_parent_boundary(side, boundary, fault);
                    }
                }
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Retain actual native owners through the explicit initial-parent durable-boundary matrix."
    )]
    fn existing_parent_boundary(side: usize, boundary: u8, fault: u8) {
        let tree = tests::fixture();
        let roots = [tree.path().join("left"), tree.path().join("right")];
        for root in &roots {
            std::fs::create_dir_all(root.join("a/b")).unwrap();
            std::fs::write(root.join("a/b/file"), b"equal").unwrap();
        }
        let endpoints = [tests::endpoint(&roots[0]), tests::endpoint(&roots[1])];
        let binding = |root: &std::path::Path| {
            let probe = endpoint::probe_endpoint(root).unwrap();
            EndpointBinding {
                identity: probe.identity,
                locator: probe.locator,
            }
        };
        let store_path = tree.path().join("store");
        let mut store = ConnectionStore::open_at(&store_path).unwrap();
        let record = store
            .create_connection(
                ConnectionId([107; 16]),
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
        let session = store.transaction_session(record.id).unwrap();
        let value = Some(EntryState::File {
            sha256: Sha256::digest(b"equal").into(),
            bytes: 5,
            executable: false,
        });
        let intent = OperationIntent {
            connection: record.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"a/b/file").unwrap(),
            source_before: value.clone(),
            destination_before: value,
            decision: Decision::VerifyEqual,
        };
        let reviewed = initial_preview(&store, record.id, &endpoints[0], &endpoints[1], &cancel)
            .unwrap()
            .accept(intent.clone())
            .unwrap();
        let metadata =
            admission_metadata(&store, &intent, &endpoints[0], &endpoints[1], &cancel).unwrap();
        if boundary == 0 && fault != 0 {
            change_parent(&roots[side], fault);
        }
        let admitted = store.begin_native_reviewed(intent, &session, reviewed, metadata);
        if boundary == 0 && fault != 0 {
            assert!(admitted.is_err());
            assert_eq!(store.load_connection(record.id).unwrap(), record);
            assert_eq!(store.executor_position(record.id).unwrap(), (None, None));
        } else {
            let mut live = admitted.unwrap();
            if boundary >= 2 {
                store
                    .reserve_native_work(
                        &mut live,
                        &session,
                        EXECUTOR_READ_WINDOW,
                        Instant::now() + std::time::Duration::from_secs(30),
                        &cancel,
                    )
                    .unwrap();
            }
            if boundary >= 3 {
                let epoch =
                    NativeEpoch::begin_connected(&live, &endpoints[0], &endpoints[1], &cancel)
                        .unwrap();
                let mut proof = epoch.equal_evidence(&live).unwrap();
                store
                    .record_native_equal(&mut live, &mut proof, &cancel)
                    .unwrap();
                if boundary >= 4 {
                    let mut comparison = proof.comparison(&store, &live, &cancel).unwrap();
                    let candidate = store
                        .prepare_baseline(&live, &mut comparison, &cancel)
                        .unwrap();
                    if boundary == 5 {
                        let root = roots[side].clone();
                        let invoked = std::rc::Rc::new(std::cell::Cell::new(false));
                        let called = invoked.clone();
                        let cancel_at_commit = cancel.clone();
                        let reads = live.operation_reads().clone();
                        crate::connection::store::before_native_baseline_write_for_test(
                            false,
                            move || {
                                called.set(true);
                                match fault {
                                    0 => (),
                                    7 => cancel_at_commit.store(true, Ordering::Release),
                                    8 => {
                                        for chunk in [4 * 1024 * 1024, 4096, 1] {
                                            while reads.charge(chunk).is_ok() {}
                                        }
                                    }
                                    _ => change_parent(&root, fault),
                                }
                            },
                        );
                        let result =
                            store.commit_baseline(&mut live, candidate, &mut comparison, &cancel);
                        assert!(invoked.get());
                        assert_eq!(result.is_ok(), fault == 0);
                    }
                }
            }
            if (1..=4).contains(&boundary) && fault != 0 {
                change_parent(&roots[side], fault);
            }
            if boundary != 5 || fault != 0 {
                assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            }
            drop(store);
            let mut store = ConnectionStore::open_at(&store_path).unwrap();
            if fault == 0 {
                let endpoints = [tests::endpoint(&roots[0]), tests::endpoint(&roots[1])];
                let session = store.transaction_session(record.id).unwrap();
                assert!(matches!(
                    execute_next_regular(
                        &mut store,
                        record.id,
                        &session,
                        &endpoints[0],
                        &endpoints[1],
                        &cancel
                    ),
                    TransactionOutcome::Verified { .. }
                ));
                assert_eq!(
                    store
                        .load_baseline(record.id)
                        .unwrap()
                        .entries()
                        .unwrap()
                        .len(),
                    3
                );
                for root in &roots {
                    assert_eq!(std::fs::read(root.join("a/b/file")).unwrap(), b"equal");
                }
            } else {
                assert!(store.load_baseline(record.id).unwrap().entries().is_none());
                assert!(store.executor_position(record.id).unwrap().0.is_some());
                // Retained endpoint handles cannot reauthorize a changed edge.
                if fault <= 5 || fault == 7 {
                    let session = store.transaction_session(record.id).unwrap();
                    assert!(matches!(
                        execute_next_regular(
                            &mut store,
                            record.id,
                            &session,
                            &endpoints[0],
                            &endpoints[1],
                            &cancel
                        ),
                        TransactionOutcome::RecoveryRequired { .. }
                    ));
                }
                assert!(store.load_baseline(record.id).unwrap().entries().is_none());
            }
        }
        if fault != 0 {
            let retained = if fault >= 5 {
                roots[side].join("a/b/file")
            } else {
                roots[side].join("retained-a/b/file")
            };
            assert_eq!(std::fs::read(retained).unwrap(), b"equal");
            if fault == 4 {
                assert_eq!(std::fs::read(roots[side].join("a")).unwrap(), b"competitor");
            }
        }
        assert_eq!(cancel.load(Ordering::Acquire), fault == 7);
    }
}
