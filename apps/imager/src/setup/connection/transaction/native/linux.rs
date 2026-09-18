//! Handle-relative regular-file primitives. No store receipts, cleanup or public
//! activation lives here. Relocation evidence is never sealed byte recovery.
mod lease_process;
use super::descriptor as d;
use crate::setup::connection::endpoint::{self, EndpointError, NativeDirectory, PinnedEndpoint};
use crate::setup::connection::model::{ConnectionId, EndpointIdentity, NativeName, RelativePath};
use crate::setup::connection::{
    store::{LiveOperation, NamespaceReservation, OperationId, StoreError},
    transaction::{StepAdmission, StepKind},
};
use crate::setup::destination_security;
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

pub(in crate::setup::connection::transaction) fn maybe_run_lease_receiver(
    args: &[String],
) -> Option<i32> {
    lease_process::maybe_run(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::connection::endpoint::{
        Access, EndpointContext, probe_endpoint, verify_selected_write,
    };
    fn endpoint(path: &std::path::Path) -> PinnedEndpoint {
        let probe = probe_endpoint(path).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        verify_selected_write(&endpoint).unwrap();
        endpoint
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
    #[derive(Clone, Copy)]
    enum PreservationCase {
        Success,
        DisconnectAfterBytes,
        MissingSource,
        OwnershipRace,
    }
    #[expect(
        clippy::too_many_lines,
        reason = "The real-tree fixture retains all guards while testing byte preservation, refusal and observer substitution."
    )]
    fn connected_preservation_case(case: PreservationCase) {
        let stop_after_bytes = matches!(case, PreservationCase::DisconnectAfterBytes);
        use crate::setup::connection::transaction::StepKind;
        use crate::setup::connection::{
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
                binding(&source_path),
                binding(&target_path),
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
        let mut live = store
            .begin_native(
                OperationIntent {
                    connection: record.id,
                    expected_revision: record.revision,
                    expected_baseline: None,
                    path: RelativePath::unix(b"file").unwrap(),
                    source_before: content(b"proposal"),
                    destination_before: content(b"original"),
                    decision: Decision::Propagate {
                        from: ModelSide::Source,
                    },
                },
                &session,
            )
            .unwrap();
        let reservation = store
            .reserve_native_namespace(&mut live, ModelSide::Destination)
            .unwrap();
        let private_name = reservation.name();
        let cancel = AtomicBool::new(false);
        let mut epoch =
            NativeEpoch::begin_connected(&live, &source_endpoint, &target_endpoint).unwrap();
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
            crate::setup::connection::store::after_native_bytes_for_test(move || {
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
                crate::setup::connection::store::RecoveryState::BytesVerified
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
        let _sealed = epoch
            .seal_displacement(&live, displaced, displaced_guard, &private, bound)
            .unwrap();
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
            crate::setup::connection::model::Observation::Complete
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
                crate::setup::connection::model::Observation::Incomplete(_)
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
                crate::setup::connection::model::Observation::Incomplete(_)
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
            crate::setup::connection::model::Observation::Incomplete(_)
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
            crate::setup::connection::model::Observation::Incomplete(_)
        ));
        assert!(changed.entries().contains_key(&private_key));
        assert_eq!(
            std::fs::read(target_path.join(&private_name).join("user-data")).unwrap(),
            b"untouched"
        );
    }
    fn binding() -> OperationBinding {
        OperationBinding {
            connection: ConnectionId([1; 16]),
            operation: [2; 16],
            store_session: [3; 16],
            revision: 1,
            fence: 0,
        }
    }
    thread_local! { static BEFORE_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_before_relocate() {
        BEFORE_RELOCATE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }
    thread_local! { static BEFORE_PUBLICATION_REREAD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_before_publication_reread() {
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
                cause: EndpointError::IdentityChanged,
                boundary: Boundary::Uncertain,
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
    pub(in crate::setup::connection::transaction) fn take_barrier_failure() -> bool {
        FAIL_BARRIER.with(|value| value.replace(false))
    }
    #[test]
    #[ignore = "authorized Linux filesystem and QOS_LEASE_HELPER executable required"]
    fn native_publication_is_actual_and_barrier_failure_preserves_every_name() {
        for fail in [false, true] {
            let tree = fixture();
            std::fs::write(tree.path().join("source"), b"proposal").unwrap();
            let endpoint = endpoint(tree.path());
            let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
                        boundary: Boundary::Uncertain,
                        ..
                    })
                ));
                assert!(epoch.check().is_err());
            } else {
                let published = published.unwrap();
                assert_eq!(published.identity, expected);
                assert_eq!(
                    published.bytes.digest,
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
                    .join(private.name.to_str().unwrap())
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let source = epoch
            .pin_parent(Side::Source, &RelativePath::unix(b"source").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&source).unwrap();
        std::fs::rename(tree.path().join("source"), tree.path().join("saved")).unwrap();
        assert!(epoch.capture(&guard).is_err());
        drop(epoch);
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        assert!(
            epoch
                .pin_parent(Side::Target, &RelativePath::unix(b"missing/child").unwrap())
                .is_err()
        );
        assert!(!tree.path().join("missing").exists());
        assert_eq!(std::fs::read(tree.path().join("saved")).unwrap(), b"source");
    }
    thread_local! { static AFTER_RELOCATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
    pub(in crate::setup::connection::transaction) fn run_after_relocate() {
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
        let target = epoch
            .pin_parent(Side::Target, &RelativePath::unix(b"live").unwrap())
            .unwrap();
        let guard = epoch.guard_regular(&target).unwrap();
        let private = epoch.create_namespace(&target).unwrap();
        let captured = tree
            .path()
            .join(private.name.to_str().unwrap())
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
    fn fixture() -> tempfile::TempDir {
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
                    .join(private.name.to_str().unwrap())
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
                    .join(private.name.to_str().unwrap())
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
        let mut epoch = NativeEpoch::begin(&endpoint, &endpoint, binding()).unwrap();
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
    connection: ConnectionId,
    operation: [u8; 16],
    store_session: [u8; 16],
    revision: u64,
    fence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup::connection::transaction) enum Boundary {
    Untouched,
    PrivateStaged,
    DurableQuarantine,
    Published,
    Uncertain,
}
#[derive(Debug)]
pub(in crate::setup::connection::transaction) struct Failure {
    cause: EndpointError,
    boundary: Boundary,
    os_error: Option<i32>,
}
impl Failure {
    pub(in crate::setup::connection::transaction) fn os_error(&self) -> Option<i32> {
        self.os_error
    }
    pub(in crate::setup::connection::transaction) fn cause(&self) -> EndpointError {
        self.cause
    }
    pub(in crate::setup::connection::transaction) fn boundary(&self) -> Boundary {
        self.boundary
    }
}
type Result<T> = std::result::Result<T, Failure>;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::setup::connection::transaction) enum Side {
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
struct Stamp {
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
pub(in crate::setup::connection::transaction) struct ParentGuard {
    directory: NativeDirectory,
    name: CString,
    epoch: [u8; 16],
    side: Side,
    chain: Vec<ObjectId>,
    path: Option<RelativePath>,
}
pub(in crate::setup::connection::transaction) struct StableFileGuard {
    parent: ParentGuard,
    file: Arc<File>,
    actual: ObjectId,
    stamp: Stamp,
    executable: bool,
}
pub(in crate::setup::connection::transaction) struct PrivateNamespace {
    directory: NativeDirectory,
    name: CString,
    epoch: [u8; 16],
    identity: ObjectId,
    intended_parent: ObjectId,
    intended_name: CString,
    descriptor: Option<(d::Header, d::Intent, Vec<u8>)>,
}
pub(in crate::setup::connection::transaction) struct AbsentName {
    epoch: [u8; 16],
    parent: ObjectId,
    name: CString,
}
pub(in crate::setup::connection::transaction) struct Capture {
    digest: [u8; 32],
    length: u64,
    executable: bool,
}
struct SourceBinding {
    parent: NativeDirectory,
    name: CString,
    actual: ObjectId,
    side: Side,
}
pub(in crate::setup::connection::transaction) struct StagedFile {
    source: SourceBinding,
    guard: StableFileGuard,
    bytes: Capture,
    namespace: NativeDirectory,
    intended_parent: ObjectId,
    intended_name: CString,
    descriptor: Option<Vec<u8>>,
}
pub(in crate::setup::connection::transaction) struct RelocatedObject {
    epoch: [u8; 16],
    actual: ObjectId,
    retained: File,
    namespace: NativeDirectory,
    original_parent: NativeDirectory,
    original_name: CString,
    matches_guard: bool,
    descriptor: Option<Vec<u8>>,
}
pub(in crate::setup::connection::transaction) struct NativePublication {
    source: SourceBinding,
    epoch: [u8; 16],
    identity: ObjectId,
    bytes: Capture,
    parent: NativeDirectory,
    name: CString,
}
pub(in crate::setup::connection::transaction) struct NativeEpoch<'a> {
    source: &'a PinnedEndpoint,
    target: &'a PinnedEndpoint,
    source_root: NativeDirectory,
    target_root: NativeDirectory,
    identities: [EndpointIdentity; 2],
    binding: OperationBinding,
    nonce: [u8; 16],
    leases: Receiver,
    deadline: Instant,
    read_budget: Arc<super::ReadBudget>,
    lifetime: Arc<()>,
    boundary: Boundary,
    poisoned: bool,
    relocated: bool,
    namespace_created: bool,
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
    ) -> Result<Self> {
        let fail = |cause| Failure {
            cause,
            boundary: Boundary::Untouched,
            os_error: None,
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
                cause,
                boundary: Boundary::Untouched,
                os_error,
            }
        })?;
        Ok(Self {
            source,
            target,
            source_root,
            target_root,
            identities: [source.identity(), target.identity()],
            binding,
            nonce,
            leases,
            deadline: Instant::now() + lease_process::WORK,
            read_budget: Arc::new(super::ReadBudget {
                reads: Arc::new(super::OperationReads::new()),
                deadline: Instant::now() + lease_process::WORK,
            }),
            lifetime: Arc::new(()),
            boundary: Boundary::Untouched,
            poisoned: false,
            relocated: false,
            namespace_created: false,
            connected: None,
        })
    }
    fn failure(&mut self, cause: EndpointError) -> Failure {
        self.poisoned = true;
        self.leases.revoke();
        Failure {
            cause,
            boundary: self.boundary,
            os_error: None,
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
            failure.os_error = os_error;
            failure
        })
    }
    pub(in crate::setup::connection::transaction) fn cancel(&mut self) {
        self.poisoned = true;
        self.leases.revoke();
    }
    pub(in crate::setup::connection::transaction) fn check(&mut self) -> Result<()> {
        if self.poisoned || Instant::now() >= self.deadline {
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
    pub(in crate::setup::connection::transaction) fn pin_parent(
        &mut self,
        side: Side,
        path: &RelativePath,
    ) -> Result<ParentGuard> {
        self.check()?;
        let Some((last, ancestors)) = path.components().split_last() else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let mut directory = if side == Side::Source {
            self.source_root.clone()
        } else {
            self.target_root.clone()
        };
        let mut chain = vec![self.ep(object(directory.file()))?];
        for name in ancestors {
            let NativeName::Unix(bytes) = name else {
                return Err(self.failure(EndpointError::Unsupported));
            };
            directory = self.ep(directory.child(OsStr::from_bytes(bytes), side == Side::Target))?;
            chain.push(self.ep(object(directory.file()))?);
        }
        let NativeName::Unix(bytes) = last else {
            return Err(self.failure(EndpointError::Unsupported));
        };
        let name =
            CString::new(bytes.as_slice()).map_err(|_| self.failure(EndpointError::Unsupported))?;
        self.check()?;
        Ok(ParentGuard {
            directory,
            name,
            side,
            epoch: self.nonce,
            chain,
            path: Some(path.clone()),
        })
    }
    pub(in crate::setup::connection::transaction) fn guard_regular(
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
    pub(in crate::setup::connection::transaction) fn capture(
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
    pub(in crate::setup::connection::transaction) fn observe_absent(
        &mut self,
        parent: &ParentGuard,
    ) -> Result<AbsentName> {
        self.check()?;
        self.parent_check(parent)?;
        if parent.side != Side::Target || self.relocated {
            return Err(self.failure(EndpointError::Unsupported));
        }
        match open_at(parent.directory.file(), &parent.name, libc::O_PATH) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(self.failure(error.into())),
            Ok(_) => return Err(self.failure(EndpointError::Unstable)),
        }
        self.parent_check(parent)?;
        Ok(AbsentName {
            epoch: self.nonce,
            parent: self.ep(object(parent.directory.file()))?,
            name: parent.name.clone(),
        })
    }
    fn create_namespace(&mut self, target: &ParentGuard) -> Result<PrivateNamespace> {
        self.check()?;
        self.parent_check(target)?;
        if target.side != Side::Target || self.namespace_created {
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
            name,
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
        let mut descriptor_bytes = Vec::new();
        for bytes in [
            b"QNI1".as_slice(),
            &self.nonce,
            &self.binding.connection.0,
            &self.binding.operation,
            &self.binding.store_session,
            &self.binding.revision.to_le_bytes(),
            &self.binding.fence.to_le_bytes(),
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
        let mut file = self.io(open_at(
            private.directory.file(),
            c"stage",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        self.io(destination_security::check_private_file(&file))?;
        let bytes = self.read_hash(&source.file, Some(&mut file))?;
        self.validate_guard(source, true)?;
        // Only portable executable semantics; ownership/labels remain local.
        // SAFETY: exclusive generated stage fd; private host-local permissions.
        let result = unsafe {
            libc::fchmod(
                file.as_raw_fd(),
                if bytes.executable { 0o700 } else { 0o600 },
            )
        };
        if result != 0 {
            return Err(self.failure(io::Error::last_os_error().into()));
        }
        self.io(file.sync_all())?;
        drop(file); // read lease cannot coexist with our own writable description.
        let parent = ParentGuard {
            directory: private.directory.clone(),
            name: c"stage".to_owned(),
            epoch: self.nonce,
            side: Side::Target,
            chain: vec![],
            path: None,
        };
        let guard = self.guard_regular(&parent)?;
        let verified = self.capture(&guard)?;
        if bytes.digest != verified.digest
            || bytes.length != verified.length
            || bytes.executable != verified.executable
        {
            return Err(self.failure(EndpointError::Unstable));
        }
        let descriptor_bytes = if let Some((header, intent, intent_bytes)) = &private.descriptor {
            let decoded =
                d::decode(intent_bytes).map_err(|_| self.failure(EndpointError::Unstable))?;
            let body = d::Body::Stage(d::Stage {
                intent: decoded.digest,
                object: descriptor_object(&guard.actual),
                content: descriptor_content(&bytes),
                source: d::Source::Endpoint {
                    side: intent.direction,
                    parent: descriptor_object(&self.ep(object(source.parent.directory.file()))?),
                    object: descriptor_object(&source.actual),
                    stamp: descriptor_stamp(&source.stamp),
                },
            });
            let encoded =
                d::encode(header, &body).map_err(|_| self.failure(EndpointError::Unstable))?;
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
                &guard.actual.generation,
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
        self.barriers(&private.directory, &private.directory)?;
        self.boundary = Boundary::PrivateStaged;
        self.namespace_check(private)?;
        self.validate_guard(source, true)?;
        Ok(StagedFile {
            source: SourceBinding {
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
        self.boundary = Boundary::DurableQuarantine;
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
            original_name: target.name.clone(),
            matches_guard,
            descriptor: None,
        })
    }
    pub(in crate::setup::connection::transaction) fn guard_displaced(
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
    /// Only original additions are publishable in this slice. Relocation never
    /// manufactures the reviewed store recovery acknowledgement needed to replace.
    fn publish_absent(
        &mut self,
        staged: StagedFile,
        target: &ParentGuard,
        absent: AbsentName,
        source: &StableFileGuard,
    ) -> Result<NativePublication> {
        self.check()?;
        self.parent_check(target)?;
        if self.relocated
            || staged.source.actual != source.actual
            || staged.source.name != source.parent.name
            || staged.source.side != source.parent.side
            || self.ep(object(staged.source.parent.file()))?
                != self.ep(object(source.parent.directory.file()))?
            || staged.intended_name != target.name
            || staged.intended_parent != absent.parent
            || target.side != Side::Target
            || absent.epoch != self.nonce
            || absent.name != target.name
            || absent.parent != self.ep(object(target.directory.file()))?
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        drop(absent); // consume the one-shot observed-absence evidence.
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
        self.validate_guard(source, true)?;
        self.check()?;
        let named = self.io(open_at(target.directory.file(), &target.name, libc::O_PATH))?;
        if self.ep(object(&named))? != staged.guard.actual {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        // The reread and source postflight can take time. A retained directory
        // fd still names a detached old parent, so recheck its ancestry/security
        // at the final proof boundary rather than trusting the earlier check.
        self.parent_check(target)?;
        self.boundary = Boundary::Published;
        Ok(NativePublication {
            source: staged.source,
            epoch: self.nonce,
            identity: staged.guard.actual,
            bytes,
            parent: target.directory.clone(),
            name: target.name.clone(),
        })
    }
}

struct ConnectedBinding {
    id: OperationId,
    lifetime: Weak<()>,
    admission: u64,
    fence: u64,
    direction: usize,
    intent: crate::setup::connection::store::OperationIntent,
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
    pub(in crate::setup::connection::transaction) fn begin_connected(
        live: &LiveOperation,
        source: &'e PinnedEndpoint,
        target: &'e PinnedEndpoint,
    ) -> Result<Self> {
        use crate::setup::connection::model::{Decision, Side as ModelSide};
        let failure = || Failure {
            cause: EndpointError::Unverified,
            boundary: Boundary::Untouched,
            os_error: None,
        };
        let direction = match live.intent().decision {
            Decision::Propagate {
                from: ModelSide::Source,
            } => 0,
            Decision::Propagate {
                from: ModelSide::Destination,
            } => 1,
            _ => return Err(failure()),
        };
        if live.lifetime().strong_count() == 0
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
                connection: live.id().connection,
                operation,
                store_session: [0; 16],
                revision: live.admission(),
                fence: live.fence(),
            },
        )?;
        epoch.read_budget = Arc::new(super::ReadBudget::for_operation(
            live.operation_reads(),
            epoch.deadline,
        ));
        epoch.connected = Some(ConnectedBinding {
            id: live.id(),
            lifetime: live.lifetime().clone(),
            admission: live.admission(),
            fence: live.fence(),
            direction,
            intent: live.intent().clone(),
        });
        Ok(epoch)
    }
    fn connected_check(&mut self, live: &LiveOperation) -> Result<()> {
        if !self.connected.as_ref().is_some_and(|binding| {
            binding.id == live.id()
                && binding.lifetime.ptr_eq(live.lifetime())
                && binding.lifetime.strong_count() != 0
                && binding.admission == live.admission()
                && binding.fence == live.fence()
                && &binding.intent == live.intent()
        }) {
            return Err(self.failure(EndpointError::Unverified));
        }
        self.check()
    }
    fn admitted(
        &mut self,
        live: &LiveOperation,
        step: &StepAdmission,
        kind: StepKind,
    ) -> Result<()> {
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
        planned: Option<&crate::setup::connection::model::EntryState>,
    ) -> bool {
        use crate::setup::connection::model::EntryState;
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
        if d::decode(bytes).is_err() {
            return Err(self.failure(EndpointError::Unsupported));
        }
        let mut file = self.io(open_at(
            directory.file(),
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        ))?;
        self.io(destination_security::check_private_file(&file))?;
        self.io(file.write_all(bytes))?;
        self.io(file.sync_all())?;
        self.barriers(directory, directory)?;
        self.check()
    }
    fn read_descriptor(
        &mut self,
        directory: &NativeDirectory,
        name: &CStr,
        expected: &[u8],
    ) -> Result<()> {
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the single-step admission and retain it until this native step returns."
    )]
    pub(in crate::setup::connection::transaction) fn create_registered_namespace(
        &mut self,
        live: &LiveOperation,
        reservation: &NamespaceReservation,
        source: &ParentGuard,
        target: &ParentGuard,
        source_guard: &StableFileGuard,
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
            || source_guard.parent.path != source.path
            || source_guard.parent.side != Side::Source
        {
            return Err(self.failure(EndpointError::Unverified));
        }
        let direction = self
            .connected
            .as_ref()
            .ok_or_else(|| Failure {
                cause: EndpointError::Unverified,
                boundary: self.boundary,
                os_error: None,
            })?
            .direction;
        let source_state = self.state(source_guard)?;
        let target_state = if let Some(guard) = target_guard {
            if guard.parent.path != target.path || guard.parent.side != Side::Target {
                return Err(self.failure(EndpointError::Unverified));
            }
            self.state(guard)?
        } else {
            self.observe_absent(target)?;
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
        self.io(destination_security::check_private_file(directory.file()))?;
        let identity = self.ep(object(directory.file()))?;
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
            decision: if direction == 0 { 1 } else { 2 },
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
            name,
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
    pub(in crate::setup::connection::transaction) fn stage_connected(
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
    pub(in crate::setup::connection::transaction) fn prepared<'a>(
        &'a mut self,
        private: &'a PrivateNamespace,
        staged: Option<&'a StagedFile>,
    ) -> Result<super::NativePrepared<'a, 'e>> {
        self.namespace_check(private)?;
        if private.descriptor.is_none() || staged.is_none() {
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
    pub(in crate::setup::connection) fn validate(
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
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            let Some(bytes) = &staged.descriptor else {
                return Err(self.epoch.failure(EndpointError::Unverified));
            };
            if self.epoch.ep(object(staged.namespace.file()))? != self.namespace.identity {
                return Err(self.epoch.failure(EndpointError::IdentityChanged));
            }
            self.epoch.validate_guard(&staged.guard, true)?;
            self.epoch
                .read_descriptor(&self.namespace.directory, c"staged-intent", bytes)?;
            self.epoch.namespace_check(self.namespace)?;
            self.epoch.check()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::setup::connection) fn descriptors(
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consume the single-step admission and retain it until this native step returns."
    )]
    pub(in crate::setup::connection::transaction) fn relocate_connected(
        &mut self,
        live: &LiveOperation,
        target: &ParentGuard,
        private: &PrivateNamespace,
        source: &StableFileGuard,
        expected: &StableFileGuard,
        prepared: &crate::setup::connection::store::DurableAck<
            crate::setup::connection::store::PreparedNative,
        >,
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
        if source.parent.side != Side::Source
            || source.parent.path.as_ref() != Some(&live.intent().path)
            || Some(&self.state(source)?) != intent.expected.get(usize::from(intent.direction))
        {
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
    pub(in crate::setup::connection::transaction) fn recovery_source<'a>(
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
            epoch: self,
            namespace: private,
            displaced,
            guard,
            admission: step,
            content,
        })
    }
    pub(in crate::setup::connection::transaction) fn seal_displacement(
        &mut self,
        live: &LiveOperation,
        displaced: RelocatedObject,
        guard: StableFileGuard,
        private: &PrivateNamespace,
        recovery: crate::setup::connection::store::DurableAck<
            crate::setup::connection::store::BoundRecovery,
        >,
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
                cause: EndpointError::Unverified,
                boundary: self.boundary,
                os_error: None,
            })?,
        )?;
        self.namespace_check(private)?;
        self.ep(displaced.original_parent.validate(true))?;
        self.check()?;
        Ok(super::SealedDisplacement {
            displaced,
            guard,
            recovery,
            lifetime: live.lifetime().clone(),
        })
    }
}
impl super::NativeRecoveryReader<'_, '_> {
    pub(in crate::setup::connection) fn validate_for(
        &mut self,
        live: &LiveOperation,
    ) -> std::result::Result<(), StoreError> {
        self.epoch
            .admitted(live, &self.admission, StepKind::Preserve)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.validate()
    }
    pub(in crate::setup::connection) fn validate(&mut self) -> std::result::Result<(), StoreError> {
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
    pub(in crate::setup::connection) fn read_chunk(
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
    pub(in crate::setup::connection) fn charge_store_read(
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
    pub(in crate::setup::connection) fn content(&self) -> &d::Content {
        &self.content
    }
    pub(in crate::setup::connection) fn epoch(&self) -> [u8; 16] {
        self.epoch.nonce
    }
    pub(in crate::setup::connection) fn fact(
        &self,
        id: crate::setup::connection::store::RecoveryId,
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
    pub(in crate::setup::connection::transaction) fn owned_names(
        &mut self,
        live: &LiveOperation,
        side: Side,
        records: Vec<crate::setup::connection::store::NativeNamespaceRecord>,
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
                cause: EndpointError::Unverified,
                boundary: self.boundary,
                os_error: None,
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
    pub(in crate::setup::connection) fn observation(&self) -> super::NativeOwnedObservation<'_> {
        super::NativeOwnedObservation {
            names: self,
            snapshots: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        }
    }
    pub(in crate::setup::connection) fn charge(
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
        d::decode(&bytes).map_err(|_| EndpointError::Unverified)?;
        Ok(bytes)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "Exact namespace, descriptor and child identity checks are one fail-closed observer validation."
    )]
    fn validate_record(
        &self,
        root: &NativeDirectory,
        record: &crate::setup::connection::store::NativeNamespaceRecord,
    ) -> std::result::Result<OwnedSnapshot, EndpointError> {
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
    pub(in crate::setup::connection) fn charge(
        &self,
        count: u64,
    ) -> std::result::Result<(), EndpointError> {
        self.names.charge(count)
    }
    fn validate_record(
        &self,
        root: &NativeDirectory,
        record: &crate::setup::connection::store::NativeNamespaceRecord,
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
    pub(in crate::setup::connection) fn validate_all(
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
    pub(in crate::setup::connection) fn excludes(
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
