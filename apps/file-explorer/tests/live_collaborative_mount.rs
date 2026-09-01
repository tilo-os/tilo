//! Mandatory Files-to-kernel collaborative-mount contract.
//!
//! This is deliberately a separate test binary: it owns the workspace-root
//! environment for its lifetime and requires the real `/dev/fuse` release
//! runner. It exercises the production Files model and command path, not a
//! fabricated xattr fixture.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use file_explorer::model::{AvailabilityPolicy, Residency};
use file_explorer::{FileCommand, FileExplorerWidget};
use fs::availability::{RecordAvailability, RecordKind, Residency as FsResidency};
use fs::{AvailabilityRegistry, ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO};
use fuser::{Config, MountOption};
use iroh::endpoint::{Endpoint, presets};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Mandatory end-to-end FUSE contract is one lifecycle.
async fn files_reads_live_projections_and_applies_multi_selection_pin_policy() {
    let fuse = std::fs::metadata("/dev/fuse").expect("mandatory test requires /dev/fuse");
    assert!(std::os::unix::fs::FileTypeExt::is_char_device(
        &fuse.file_type()
    ));

    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let endpoint = Endpoint::bind(presets::N0).await.unwrap();
    let shutdown_endpoint = endpoint.clone();
    let store = Arc::new(ContentStore::memory());
    let shutdown_store = store.clone();
    let namespace = Arc::new(Mutex::new(
        Namespace::new("files-live-mount").expect("create test namespace"),
    ));
    let mut expected = Vec::new();
    for (name, body) in [
        ("first.png", b"first live payload".as_slice()),
        ("second.png", b"second live payload".as_slice()),
    ] {
        let hash = store.write_content(body).await.unwrap().unwrap();
        let ino = namespace
            .lock()
            .unwrap()
            .create_file(ROOT_INO, name, Some(&hash), body.len() as u64, 1)
            .unwrap();
        let record_id = namespace.lock().unwrap().record_id_for(ino).unwrap();
        expected.push((name, record_id, hash, body.len() as u64));
    }

    let mut availability =
        AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
    for (_, record_id, _, bytes) in &expected {
        let mut record = RecordAvailability::new(*record_id, RecordKind::File);
        record.residency = FsResidency::Resident { bytes: *bytes };
        availability.put(record).unwrap();
    }
    let availability = Arc::new(Mutex::new(availability));
    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let quick_fs =
        QuickFS::new(namespace, hydrator, heuristics).with_availability(availability.clone());
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("qos_files_live_test".into())];
    let session = fuser::spawn_mount2(quick_fs, &mount, &config).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_dir(&mount).is_ok_and(|entries| entries.count() == expected.len()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live FUSE mount did not become ready"
        );
        tokio::task::yield_now().await;
    }

    let previous_roots = std::env::var_os("QOS_WORKSPACE_ROOTS");
    unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", &mount) };
    let mut files = FileExplorerWidget::with_persistence(None, None);
    files.navigate(mount.clone());
    assert_eq!(files.entries().len(), 2);
    for entry in files.entries() {
        let (_, record_id, hash, bytes) = expected
            .iter()
            .find(|(name, ..)| name == &entry.name)
            .unwrap();
        assert_eq!(entry.record_id, Some(*record_id.0.as_bytes()));
        assert_eq!(entry.content_hash, Some(*hash.as_bytes()));
        let projection = entry.availability.as_ref().unwrap();
        assert_eq!(projection.policy, AvailabilityPolicy::Automatic);
        assert_eq!(projection.residency, Residency::Resident { bytes: *bytes });
    }

    files.debug_select(&[0, 1]);
    assert!(files.apply_command(FileCommand::KeepOffline));
    assert!(files.entries().iter().all(|entry| {
        entry
            .availability
            .as_ref()
            .is_some_and(|projection| projection.policy == AvailabilityPolicy::Pinned)
    }));
    {
        let registry = availability.lock().unwrap();
        for (_, record_id, _, _) in &expected {
            assert_eq!(
                registry.get(*record_id).unwrap().policy,
                fs::availability::AvailabilityPolicy::Pinned
            );
        }
        drop(registry);
    }

    assert!(files.apply_command(FileCommand::RemoveOfflinePin));
    assert!(files.entries().iter().all(|entry| {
        entry
            .availability
            .as_ref()
            .is_some_and(|projection| projection.policy == AvailabilityPolicy::Automatic)
    }));

    drop(files);
    match previous_roots {
        Some(value) => unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", value) },
        None => unsafe { std::env::remove_var("QOS_WORKSPACE_ROOTS") },
    }
    session.umount_and_join().unwrap();
    shutdown_endpoint.close().await;
    shutdown_store.store().shutdown().await.unwrap();
}
