use super::*;
#[test]
fn host_work_single_pool_credit_lasts_through_last_large_lease() {
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let initial = pool.available_bytes();
    let inventory = HostWorkInventory::new(&pool).unwrap();
    assert!(inventory.same_pool(&pool));
    assert!(pool.available_bytes() < initial);
    let alias = inventory.clone();
    let large = inventory.try_large_decode().unwrap();
    assert!(matches!(
        alias.try_large_decode(),
        Err(HostWorkFailure::Capacity)
    ));
    drop(inventory);
    drop(alias);
    assert!(pool.available_bytes() < initial);
    drop(large);
    assert_eq!(pool.available_bytes(), initial);
}
#[test]
fn host_work_cancel_releases_one_large_decode_and_pressure_refuses_startup() {
    let (ingress, _rx) = crate::https::ingress_channel();
    let pool = ingress.host_bytes();
    let inventory = HostWorkInventory::new(&pool).unwrap();
    let one = inventory.try_large_decode().unwrap();
    drop(one);
    let two = inventory.try_large_decode().unwrap();
    drop(two);
    drop(inventory);
    let bulk = pool
        .try_acquire_bulk(pool.available_bytes() - crate::host_bytes::CONTROL_RESERVE_BYTES)
        .unwrap();
    assert!(matches!(
        HostWorkInventory::new(&pool),
        Err(HostWorkFailure::Capacity)
    ));
    drop(bulk);
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
fn host_work_staging_quota_is_shared_and_cancellation_does_not_erase_reserved_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::store::SessionStore::create(&temp.path().join("one")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let directory = store.staging_directory(&inventory, 7).unwrap();
    let mut leases = Vec::new();
    for id in 1..=4 {
        leases.push(
            directory
                .reserve(identity(7, id, id, 64 * 1024 * 1024))
                .unwrap(),
        );
    }
    assert!(matches!(
        directory.reserve(identity(7, 5, 5, 1)),
        Err(HostWorkFailure::Capacity)
    ));
    let first = leases.remove(0);
    drop(first);
    assert!(
        matches!(
            directory.reserve(identity(7, 5, 5, 1)),
            Err(HostWorkFailure::Capacity)
        ),
        "dropping worker custody cannot erase possible physical staging"
    );
    let same = directory
        .reserve(identity(7, 1, 1, 64 * 1024 * 1024))
        .unwrap();
    let mut changed = identity(7, 1, 1, 64 * 1024 * 1024);
    changed.digest = [4; 32];
    assert!(matches!(
        directory.reserve(changed),
        Err(HostWorkFailure::Conflict)
    ));
    drop(same);
}
#[test]
fn host_work_staging_device_and_workspace_limits_use_actual_directory_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::store::SessionStore::create(&temp.path().join("one")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let directory = store.staging_directory(&inventory, 9).unwrap();
    let one = directory.reserve(identity(9, 1, 1, 1)).unwrap();
    let two = directory.reserve(identity(9, 1, 2, 1)).unwrap();
    assert!(matches!(
        directory.reserve(identity(9, 1, 3, 1)),
        Err(HostWorkFailure::Capacity)
    ));
    assert!(matches!(
        directory.reserve(identity(10, 2, 3, 1)),
        Err(HostWorkFailure::Conflict)
    ));
    drop(one);
    drop(two);
}

fn persisted_stage(store: &crate::store::SessionStore, id: StageIdentity, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = store.root().join("staging-v1");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stem = format!(
        "{}-{}",
        id.device
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        id.delivery
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.join(format!("{stem}.data")))
        .unwrap();
    file.write_all(bytes).unwrap();
    file.write_all(b"uncommitted suffix").unwrap();
    file.sync_all().unwrap();
    let manifest = crate::staging_manifest::StageManifest {
        identity: id,
        prefix: bytes.len() as u64,
        expires_at_ns: 1,
        prefix_digest: *blake3::hash(bytes).as_bytes(),
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.join(format!("{stem}.meta")))
        .unwrap();
    file.write_all(&manifest.encode().unwrap()).unwrap();
    file.sync_all().unwrap();
}
#[test]
fn host_work_reopen_charges_full_totals_and_truncates_only_uncommitted_suffix() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let store = crate::store::SessionStore::create(&root).unwrap();
    for id in 1..=4 {
        persisted_stage(
            &store,
            identity(7, id, id, 64 * 1024 * 1024),
            b"real durable prefix",
        );
    }
    drop(store);
    let store = crate::store::SessionStore::open(&root).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let directory = store.staging_directory(&inventory, 7).unwrap();
    assert!(
        matches!(
            directory.reserve(identity(7, 5, 5, 1)),
            Err(HostWorkFailure::Capacity)
        ),
        "recovered full totals consume workspace quota"
    );
    let lease = directory
        .reserve(identity(7, 1, 1, 64 * 1024 * 1024))
        .unwrap();
    let file = lease.open_data(false).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 19);
    drop(file);
    assert_eq!(
        lease.read_manifest().unwrap().unwrap().identity,
        *lease.identity()
    );
    lease.remove().unwrap();
    let next = directory.reserve(identity(7, 5, 5, 1)).unwrap();
    drop(next);
}
#[test]
fn host_work_scoped_files_manifest_barrier_and_cleanup_keep_quota_until_safe() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let store = crate::store::SessionStore::create(&temp.path().join("owner")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = HostWorkInventory::new(&ingress.host_bytes()).unwrap();
    let id = identity(7, 1, 1, 4);
    let lease = store.reserve_staging(&inventory, id).unwrap();
    let mut file = lease.open_data(true).unwrap();
    file.write_all(b"data").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let manifest = crate::staging_manifest::StageManifest {
        identity: id,
        prefix: 4,
        expires_at_ns: 1,
        prefix_digest: *blake3::hash(b"data").as_bytes(),
    };
    lease.write_manifest(&manifest).unwrap();
    assert_eq!(lease.read_manifest().unwrap(), Some(manifest));
    let other = store.reserve_staging(&inventory, id).unwrap();
    assert!(matches!(lease.remove(), Err(HostWorkFailure::Capacity)));
    other.remove().unwrap();
    assert_eq!(
        std::fs::read_dir(store.root().join("staging-v1"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn host_work_reopen_same_inventory_revalidates_persisted_prefix() {
    use std::io::{Seek, Write};
    let temp = tempfile::tempdir().unwrap();
    let store = crate::store::SessionStore::create(&temp.path().join("owner")).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    let id = identity(7, 1, 1, 64);
    drop(store.reserve_staging(&inventory, id).unwrap());
    persisted_stage(&store, id, b"data");
    let path = std::fs::read_dir(store.root().join("staging-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "data"))
        .unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.write_all(b"x").unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(
        store.staging_directory(&inventory, 7).is_err(),
        "same inventory reopen must validate actual retained prefix before fresh admission"
    );
}
#[test]
fn host_work_host_quota_is_shared_across_actual_root_directories() {
    let temp = tempfile::tempdir().unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    let mut stores = Vec::new();
    let mut leases = Vec::new();
    for room in 1..=4 {
        let store =
            crate::store::SessionStore::create(&temp.path().join(format!("owner-{room}"))).unwrap();
        let directory = store.staging_directory(&inventory, room).unwrap();
        for id in 1..=4 {
            leases.push(
                directory
                    .reserve(identity(room, id, id, 64 * 1024 * 1024))
                    .unwrap(),
            );
        }
        stores.push(store);
    }
    let fifth = crate::store::SessionStore::create(&temp.path().join("fifth")).unwrap();
    assert!(matches!(
        fifth.reserve_staging(&inventory, identity(5, 1, 1, 1)),
        Err(HostWorkFailure::Capacity)
    ));
}
#[test]
fn host_work_unknown_entry_and_symlink_refuse_without_deleting_user_files() {
    use std::os::unix::fs::symlink;
    for symbolic in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::store::SessionStore::create(&temp.path().join("owner")).unwrap();
        let (ingress, _rx) = crate::https::ingress_channel();
        let inventory = ingress.host_work();
        let id = identity(7, 1, 1, 64);
        persisted_stage(&store, id, b"data");
        let sentinel = temp.path().join("sentinel");
        std::fs::write(&sentinel, b"must survive").unwrap();
        if symbolic {
            let path = std::fs::read_dir(store.root().join("staging-v1"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|e| e == "data"))
                .unwrap();
            std::fs::remove_file(&path).unwrap();
            symlink(&sentinel, path).unwrap();
        } else {
            std::fs::write(store.root().join("staging-v1/unknown"), b"unknown").unwrap();
        }
        assert!(store.staging_directory(&inventory, 7).is_err());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"must survive");
    }
}

#[test]
fn host_work_special_fifo_refuses_without_blocking_shared_inventory() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
    for suffix in ["data", "meta"] {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::store::SessionStore::create(&temp.path().join("owner")).unwrap();
        let (ingress, _rx) = crate::https::ingress_channel();
        let inventory = ingress.host_work();
        drop(store.staging_directory(&inventory, 7).unwrap());
        let path = store.root().join("staging-v1").join(format!(
            "{}-{}.{suffix}",
            "01".repeat(32),
            "02".repeat(16)
        ));
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = mpsc::channel();
        let worker_inventory = inventory.clone();
        let worker = std::thread::spawn(move || {
            let result = store.staging_directory(&worker_inventory, 7).map(drop);
            send.send(result).unwrap();
        });
        let timely = receive.recv_timeout(Duration::from_millis(500));
        let result = match timely {
            Ok(result) => (true, result),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Release the original broken O_RDONLY open so even RED joins
                // its worker and leaves no hanging thread or locked inventory.
                let fd = unsafe {
                    libc::open(
                        name.as_ptr(),
                        libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                };
                assert!(fd >= 0, "blocked FIFO reader must be released");
                unsafe { libc::close(fd) };
                (
                    false,
                    receive
                        .recv_timeout(Duration::from_secs(5))
                        .expect("FIFO worker must terminate after controlled release"),
                )
            }
            Err(error) => panic!("recovery worker disappeared: {error}"),
        };
        worker.join().unwrap();
        assert!(
            result.1.is_err(),
            "special file must never become staging input"
        );
        assert!(
            inventory.try_large_decode().is_ok(),
            "failed recovery must release shared inventory mutex"
        );
        assert!(path.exists(), "unknown special file is not auto-deleted");
        assert!(
            result.0,
            "recognized .{suffix} FIFO blocked before regular-file rejection"
        );
    }
}

#[test]
fn host_work_neutral_root_uses_actual_held_directory_after_ambient_replacement() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("owner");
    let store = crate::store::SessionStore::create(&original).unwrap();
    let held = std::fs::File::open(&original).unwrap();
    let moved = temp.path().join("held-owner");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    let directory = inventory
        .staging_directory(&held, 7)
        .expect("neutral adapter must use actual borrowed directory custody");
    let lease = directory.reserve(identity(7, 1, 1, 64)).unwrap();
    let file = lease.open_data(true).unwrap();
    drop(file);
    assert!(moved.join("staging-v1").is_dir());
    assert!(!original.join("staging-v1").exists());
    assert_eq!(
        std::fs::read_dir(moved.join("staging-v1")).unwrap().count(),
        1
    );
    let native = store.staging_directory(&inventory, 7).unwrap();
    let duplicate = native.reserve(identity(7, 1, 1, 64)).unwrap();
    assert!(
        lease.remove().is_err(),
        "same held root shares exact active quota record"
    );
    duplicate.remove().unwrap();
    assert_eq!(
        std::fs::read_dir(moved.join("staging-v1")).unwrap().count(),
        0
    );
}
#[test]
fn host_work_neutral_root_rejects_non_directory_and_non_private_handles() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let public = temp.path().join("public");
    std::fs::create_dir(&public).unwrap();
    std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
    let file_path = temp.path().join("plain");
    std::fs::write(&file_path, b"not a directory").unwrap();
    let (ingress, _rx) = crate::https::ingress_channel();
    let inventory = ingress.host_work();
    assert!(
        inventory
            .staging_directory(&std::fs::File::open(&public).unwrap(), 7)
            .is_err()
    );
    assert!(
        inventory
            .staging_directory(&std::fs::File::open(&file_path).unwrap(), 7)
            .is_err()
    );
    assert!(!public.join("staging-v1").exists());
    assert_eq!(std::fs::read(file_path).unwrap(), b"not a directory");
}
