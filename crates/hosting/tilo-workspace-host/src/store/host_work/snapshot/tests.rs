use super::*;
use crate::store::SessionStore;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn fixture() -> (
    tempfile::TempDir,
    SessionStore,
    crate::https::Ingress,
    HostWorkInventory,
) {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionStore::create(&temp.path().join("owner")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    (temp, store, ingress, inventory)
}
fn disk(inventory: &HostWorkInventory) -> u64 {
    inventory.inner().state.lock().unwrap().disk
}
fn identity(workspace: u64, device: u8, id: u8, total: u64) -> StageIdentity {
    StageIdentity {
        workspace,
        device: [device; 32],
        delivery: [id; 16],
        digest: [3; 32],
        total,
    }
}

#[test]
fn snapshot_sidecar_prepare_preserves_current_until_explicit_finalize_and_refuses_unresolved_pending()
 {
    let (_temp, store, _ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([1; 16]).unwrap();
    assert!(lease.read_current().unwrap().is_none());
    lease.prepare(b"first exact signed pair").unwrap();
    assert!(lease.read_current().unwrap().is_none());
    assert_eq!(
        lease.read_pending().unwrap().unwrap().bytes(),
        b"first exact signed pair"
    );
    assert!(lease.prepare(b"replacement before reconciliation").is_err());
    lease.finalize().unwrap();
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"first exact signed pair"
    );
    lease
        .prepare(b"new pair retaining original previous anchor")
        .unwrap();
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"first exact signed pair"
    );
    lease.discard_pending().unwrap();
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"first exact signed pair"
    );
    lease
        .prepare(b"new pair retaining original previous anchor")
        .unwrap();
    lease.finalize().unwrap();
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"new pair retaining original previous anchor"
    );
    assert_eq!(disk(&inventory), 256 * 1024);
    lease.remove().unwrap();
    assert_eq!(disk(&inventory), 0);
}
#[test]
fn snapshot_sidecar_failed_barriers_preserve_files_and_quota_until_reconciled() {
    let (_temp, store, _ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([2; 16]).unwrap();
    lease.prepare(b"old pair").unwrap();
    lease.finalize().unwrap();
    directory.with_barrier_fault(|| {
        assert!(
            lease
                .prepare(b"durable candidate but directory uncertain")
                .is_err()
        );
        assert!(
            lease.read_pending().is_err(),
            "failed recovery barrier cannot expose candidate"
        );
    });
    assert_eq!(lease.read_current().unwrap().unwrap().bytes(), b"old pair");
    assert_eq!(
        lease.read_pending().unwrap().unwrap().bytes(),
        b"durable candidate but directory uncertain"
    );
    directory.with_barrier_fault(|| assert!(lease.finalize().is_err()));
    assert_eq!(disk(&inventory), 256 * 1024);
    drop(lease);
    drop(directory);
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([2; 16]).unwrap();
    assert!(lease.read_pending().unwrap().is_none());
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"durable candidate but directory uncertain"
    );
    directory.with_barrier_fault(|| assert!(lease.remove().is_err()));
    assert_eq!(
        disk(&inventory),
        256 * 1024,
        "uncertain cleanup retains disk reservation"
    );
    let lease = directory.reserve([2; 16]).unwrap();
    lease.remove().unwrap();
    assert_eq!(disk(&inventory), 0);
}
#[test]
fn snapshot_sidecar_recovery_charges_both_names_before_upload_admission_and_shares_quota() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([3; 16]).unwrap();
    lease.prepare(b"old pair").unwrap();
    lease.finalize().unwrap();
    lease.prepare(b"pending pair").unwrap();
    drop(lease);
    drop(directory);
    // A new process inventory must account sidecars even when only uploads open.
    let recovered = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let staging = recovered.staging_directory(&store.directory, 7).unwrap();
    assert_eq!(disk(&recovered), 256 * 1024);
    let mut leases = Vec::new();
    for id in 1..=3 {
        leases.push(
            staging
                .reserve(identity(7, id, id, 64 * 1024 * 1024))
                .unwrap(),
        );
    }
    assert!(matches!(
        staging.reserve(identity(7, 4, 4, 64 * 1024 * 1024)),
        Err(HostWorkFailure::Capacity)
    ));
    let snapshots = recovered.snapshot_directory(&store.directory, 7).unwrap();
    let lease = snapshots.reserve([3; 16]).unwrap();
    assert_eq!(lease.read_current().unwrap().unwrap().bytes(), b"old pair");
    assert_eq!(
        lease.read_pending().unwrap().unwrap().bytes(),
        b"pending pair"
    );
    assert_eq!(disk(&recovered), 3 * 64 * 1024 * 1024 + 256 * 1024);
    lease.remove().unwrap();
    leases.push(
        staging
            .reserve(identity(7, 4, 4, 64 * 1024 * 1024))
            .unwrap(),
    );
    assert!(matches!(
        snapshots.reserve([4; 16]),
        Err(HostWorkFailure::Capacity)
    ));
}
#[test]
fn snapshot_sidecar_fixed_slots_persistent_cancellation_and_read_backing_credit() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    assert!(directory.reserve([0; 16]).is_err());
    for id in 1..=32 {
        let lease = directory.reserve([id; 16]).unwrap();
        lease.prepare(b"inert pending pair").unwrap();
    }
    assert!(matches!(
        directory.reserve([33; 16]),
        Err(HostWorkFailure::Capacity)
    ));
    let lease = directory.reserve([1; 16]).unwrap();
    let duplicate = directory.reserve([1; 16]).unwrap();
    assert!(lease.finalize().is_err());
    drop(duplicate);
    assert!(lease.prepare(&vec![1; 131073]).is_err());
    lease.discard_pending().unwrap();
    lease.prepare(&vec![7; 131072]).unwrap();
    lease.finalize().unwrap();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let bytes = lease.read_current().unwrap().unwrap();
    assert_eq!(initial - pool.available_bytes(), 131072);
    lease.remove().unwrap();
    assert_eq!(bytes.bytes().len(), 131072);
    assert_eq!(bytes.bytes()[131071], 7);
    drop(bytes);
    assert_eq!(pool.available_bytes(), initial);
    let spare = directory.reserve([33; 16]).unwrap();
    spare.remove().unwrap();
}
#[test]
fn snapshot_sidecar_recovery_refuses_unknown_symlink_and_nonregular_entries() {
    for kind in ["unknown", "symlink", "fifo"] {
        let (_temp, store, _ingress, inventory) = fixture();
        let path = store.root().join("snapshots-v1");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = path.join("01010101010101010101010101010101.pending");
        match kind {
            "unknown" => {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(path.join("unknown"))
                    .unwrap();
            }
            "symlink" => std::os::unix::fs::symlink(store.root().join("outside"), &target).unwrap(),
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                // O_NONBLOCK refusal is required; no I/O ever reads this node.
            }
            _ => unreachable!(),
        }
        assert!(inventory.staging_directory(&store.directory, 7).is_err());
        assert!(
            inventory.try_large_decode().is_ok(),
            "failed scan released shared lock"
        );
    }
}

#[test]
fn snapshot_sidecar_partial_pending_survives_restart_and_read_pressure_never_releases_disk() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([9; 16]).unwrap();
    let path = store
        .root()
        .join("snapshots-v1")
        .join("09090909090909090909090909090909.pending");
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap()
        .sync_all()
        .unwrap();
    drop(lease);
    drop(directory);
    let reopened = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let directory = reopened.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([9; 16]).unwrap();
    assert_eq!(disk(&reopened), 256 * 1024);
    assert!(lease.read_pending().unwrap().unwrap().bytes().is_empty());
    assert!(lease.finalize().is_err());
    assert!(
        lease
            .prepare(b"must not overwrite uncertain partial")
            .is_err()
    );
    lease.discard_pending().unwrap();
    lease.prepare(b"exact retained pair").unwrap();
    lease.finalize().unwrap();
    let pool = ingress.host_bytes();
    let pressure = pool
        .try_acquire_bulk(pool.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    assert!(matches!(
        lease.read_current(),
        Err(HostWorkFailure::Capacity)
    ));
    assert_eq!(disk(&reopened), 256 * 1024);
    drop(pressure);
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"exact retained pair"
    );
    lease.remove().unwrap();
}

#[test]
fn snapshot_sidecar_host_quota_includes_other_actual_owner_directories() {
    let temp = tempfile::tempdir().unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    let mut owners = Vec::new();
    let mut leases = Vec::new();
    for workspace in 1..=4 {
        let store = SessionStore::create(&temp.path().join(workspace.to_string())).unwrap();
        let directory = inventory
            .staging_directory(&store.directory, workspace)
            .unwrap();
        for id in 1..=4 {
            leases.push(
                directory
                    .reserve(identity(workspace, id, id, 64 * 1024 * 1024))
                    .unwrap(),
            );
        }
        owners.push(store);
    }
    let target = SessionStore::create(&temp.path().join("snapshot-owner")).unwrap();
    let directory = inventory.snapshot_directory(&target.directory, 5).unwrap();
    assert!(matches!(
        directory.reserve([1; 16]),
        Err(HostWorkFailure::Capacity)
    ));
    assert_eq!(disk(&inventory), 1024 * 1024 * 1024);
    leases.pop().unwrap().remove().unwrap();
    let lease = directory.reserve([1; 16]).unwrap();
    lease
        .prepare(b"real candidate in remaining shared disk reservation")
        .unwrap();
    assert_eq!(disk(&inventory), 960 * 1024 * 1024 + 256 * 1024);
    drop(lease);
    assert_eq!(disk(&inventory), 960 * 1024 * 1024 + 256 * 1024);
}

#[test]
fn parent_barrier_snapshot_existing_child_retry_stays_denied_until_root_sync_recovers() {
    let (_temp, store, _ingress, inventory) = fixture();
    super::super::staging::files::barrier_fault::with(&store.directory, || {
        assert!(
            open_root(&store.directory).is_err(),
            "actual mkdir must encounter failed parent barrier"
        );
        assert!(store.root().join("snapshots-v1").is_dir());
        assert!(
            open_root(&store.directory).is_err(),
            "existing child must retry the failed parent durability barrier"
        );
        assert!(open_root(&store.directory).is_err());
        assert_eq!(disk(&inventory), 0);
    });
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve([21; 16]).unwrap();
    lease
        .prepare(b"candidate after actual parent barrier recovery")
        .unwrap();
    lease.finalize().unwrap();
    assert_eq!(
        lease.read_current().unwrap().unwrap().bytes(),
        b"candidate after actual parent barrier recovery"
    );
    lease.remove().unwrap();
}

#[test]
fn parent_barrier_upload_existing_children_retry_stays_denied_until_root_sync_recovers() {
    let (_temp, store, _ingress, inventory) = fixture();
    super::super::staging::files::barrier_fault::with(&store.directory, || {
        for attempt in 0..3 {
            assert!(
                inventory.staging_directory(&store.directory, 7).is_err(),
                "retry {attempt} must not bypass an uncertain existing parent entry"
            );
        }
        assert!(store.root().join("staging-v1").is_dir());
        assert_eq!(disk(&inventory), 0);
    });
    let directory = inventory.staging_directory(&store.directory, 7).unwrap();
    let lease = directory.reserve(identity(7, 21, 21, 4)).unwrap();
    let mut file = lease.open_data(true).unwrap();
    std::io::Write::write_all(&mut file, b"real").unwrap();
    file.sync_all().unwrap();
    drop(file);
    lease.remove().unwrap();
    assert_eq!(disk(&inventory), 0);
}

#[test]
fn snapshot_existing_unknown_ids_do_not_consume_slots_credit_disk_or_files() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let initial = ingress.host_bytes().available_bytes();
    for id in 1..=64 {
        assert!(directory.open_existing([id; 16]).unwrap().is_none());
    }
    assert_eq!(disk(&inventory), 0);
    assert_eq!(ingress.host_bytes().available_bytes(), initial);
    assert_eq!(
        std::fs::read_dir(store.root().join("snapshots-v1"))
            .unwrap()
            .count(),
        0
    );
    for id in 1..=32 {
        let lease = directory.reserve([id; 16]).unwrap();
        lease.prepare(b"actual selected candidate").unwrap();
    }
    assert_eq!(disk(&inventory), 32 * SNAPSHOT_BYTES);
    assert!(matches!(
        directory.reserve([33; 16]),
        Err(HostWorkFailure::Capacity)
    ));
    for id in 33..=96 {
        assert!(directory.open_existing([id; 16]).unwrap().is_none());
    }
    assert_eq!(disk(&inventory), 32 * SNAPSHOT_BYTES);
    for id in 1..=32 {
        directory
            .open_existing([id; 16])
            .unwrap()
            .unwrap()
            .remove()
            .unwrap();
    }
    assert_eq!(disk(&inventory), 0);
    assert_eq!(ingress.host_bytes().available_bytes(), initial);
}

#[test]
fn snapshot_existing_lease_preserves_exclusivity_and_physical_custody_on_drop() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let original = directory.reserve([31; 16]).unwrap();
    original.prepare(b"original canonical").unwrap();
    original.finalize().unwrap();
    original.prepare(b"actual renewal candidate").unwrap();
    let duplicate = directory.open_existing([31; 16]).unwrap().unwrap();
    assert!(duplicate.same_pool(&ingress.host_bytes()));
    assert!(matches!(
        original.finalize(),
        Err(HostWorkFailure::Capacity)
    ));
    assert!(matches!(
        duplicate.read_pending(),
        Err(HostWorkFailure::Capacity)
    ));
    drop(original);
    assert_eq!(
        duplicate.read_current().unwrap().unwrap().bytes(),
        b"original canonical"
    );
    assert_eq!(
        duplicate.read_pending().unwrap().unwrap().bytes(),
        b"actual renewal candidate"
    );
    drop(duplicate);
    assert_eq!(disk(&inventory), SNAPSHOT_BYTES);
    let held = directory.open_existing([31; 16]).unwrap().unwrap();
    held.finalize().unwrap();
    assert_eq!(
        held.read_current().unwrap().unwrap().bytes(),
        b"actual renewal candidate"
    );
    held.remove().unwrap();
    assert!(directory.open_existing([31; 16]).unwrap().is_none());
    assert_eq!(disk(&inventory), 0);
}

#[test]
fn snapshot_existing_recovered_canonical_and_partial_pending_use_existing_charge() {
    let (_temp, store, ingress, inventory) = fixture();
    let directory = inventory.snapshot_directory(&store.directory, 7).unwrap();
    let old = directory.reserve([41; 16]).unwrap();
    old.prepare(b"durable canonical").unwrap();
    old.finalize().unwrap();
    old.prepare(b"unresolved durable pending").unwrap();
    drop(old);
    let partial = directory.reserve([42; 16]).unwrap();
    let name = Name::new(&[42; 16], true);
    let file = optional(
        &directory.directory,
        name.cstr(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )
    .unwrap()
    .unwrap();
    file.sync_all().unwrap();
    drop(file);
    directory_barrier(&directory.directory).unwrap();
    drop(partial);
    drop(directory);
    // Model startup reconciliation against actual original-pool inventory.
    let recovered = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let directory = recovered.snapshot_directory(&store.directory, 7).unwrap();
    assert_eq!(disk(&recovered), 2 * SNAPSHOT_BYTES);
    let old = directory.open_existing([41; 16]).unwrap().unwrap();
    assert_eq!(
        old.read_current().unwrap().unwrap().bytes(),
        b"durable canonical"
    );
    assert_eq!(
        old.read_pending().unwrap().unwrap().bytes(),
        b"unresolved durable pending"
    );
    let partial = directory.open_existing([42; 16]).unwrap().unwrap();
    assert!(partial.read_pending().unwrap().unwrap().bytes().is_empty());
    assert!(partial.finalize().is_err());
    assert_eq!(disk(&recovered), 2 * SNAPSHOT_BYTES);
    assert!(directory.open_existing([43; 16]).unwrap().is_none());
    drop(old);
    drop(partial);
    assert_eq!(disk(&recovered), 2 * SNAPSHOT_BYTES);
    directory
        .open_existing([41; 16])
        .unwrap()
        .unwrap()
        .remove()
        .unwrap();
    directory
        .open_existing([42; 16])
        .unwrap()
        .unwrap()
        .remove()
        .unwrap();
    assert_eq!(disk(&recovered), 0);
}

#[test]
fn snapshot_existing_zero_or_reclaimed_directory_cannot_allocate_a_record() {
    let (temp, store, _ingress, inventory) = fixture();
    let old = inventory.snapshot_directory(&store.directory, 7).unwrap();
    assert!(matches!(
        old.open_existing([0; 16]),
        Err(HostWorkFailure::Conflict)
    ));
    let other = SessionStore::create(&temp.path().join("other-owner")).unwrap();
    let _current = inventory.snapshot_directory(&other.directory, 8).unwrap();
    // The bounded inventory may reclaim an empty workspace slot. An old handle
    // must not silently reintroduce it or reserve an unknown pin.
    assert!(matches!(
        old.open_existing([1; 16]),
        Err(HostWorkFailure::Unavailable)
    ));
    assert_eq!(disk(&inventory), 0);
    assert!(
        inventory.try_large_decode().is_ok(),
        "lookup failure releases inventory mutex"
    );
}
