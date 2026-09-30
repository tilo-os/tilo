//! Mandatory Files-to-kernel collaborative-mount contract.
//!
//! This is deliberately a separate test binary: it owns the workspace-root
//! environment for its lifetime and requires the real `/dev/fuse` release
//! runner. It exercises the production Files model and command path, not a
//! fabricated xattr fixture.

#![cfg(unix)]

use std::sync::{Arc, Mutex};

use file_explorer::model::{AvailabilityPolicy, Residency};
use file_explorer::{FileCommand, FileExplorerWidget};
use fs::availability::{RecordAvailability, RecordKind, Residency as FsResidency};
use fs::{AvailabilityRegistry, ContentStore, Heuristics, Hydrator, Namespace, QuickFS, ROOT_INO};
use fuser::{Config, MountOption};
use iroh::endpoint::{Endpoint, presets};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "the mandatory end-to-end FUSE contract is one lifecycle"
)]
async fn files_reads_live_projections_and_applies_folder_pin_to_future_descendants() {
    let fuse = match std::fs::metadata("/dev/fuse") {
        Ok(fuse) => fuse,
        Err(error) if std::env::var_os("TILO_FS_FUSE_REQUIRED").is_none() => {
            eprintln!("skipping: requires /dev/fuse + fusermount3 ({error})");
            return;
        }
        Err(error) => panic!("mandatory test requires /dev/fuse: {error}"),
    };
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
    let folder_ino = namespace
        .lock()
        .unwrap()
        .create_dir(ROOT_INO, "Offline", 1)
        .unwrap();
    let folder_record_id = namespace.lock().unwrap().record_id_for(folder_ino).unwrap();
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
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let quick_fs =
        QuickFS::new(namespace, hydrator, heuristics).with_availability(availability.clone());
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("tilo_files_live_test".into())];
    let session = fuser::spawn_mount(quick_fs, &mount, &config).unwrap();

    let deadline = tilo_test_env::hang_deadline();
    loop {
        if std::fs::read_dir(&mount).is_ok_and(|entries| entries.count() == expected.len() + 1) {
            break;
        }
        assert!(
            ui::time::now() < deadline,
            "live FUSE mount did not become ready"
        );
        tokio::task::yield_now().await;
    }

    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_WORKSPACE_ROOTS", &mount);
    let mut files = FileExplorerWidget::with_persistence(None, None);
    files.navigate(mount.clone());
    assert_eq!(files.entries().len(), expected.len() + 1);
    for entry in files.entries() {
        let Some((_, record_id, hash, bytes)) =
            expected.iter().find(|(name, ..)| name == &entry.name)
        else {
            assert_eq!(entry.name, "Offline");
            assert!(entry.is_dir);
            assert_eq!(entry.record_id, Some(*folder_record_id.0.as_bytes()));
            continue;
        };
        assert_eq!(entry.record_id, Some(*record_id.0.as_bytes()));
        assert_eq!(entry.content_hash, Some(*hash.as_bytes()));
        let projection = entry.availability.as_ref().unwrap();
        assert_eq!(projection.policy, AvailabilityPolicy::Automatic);
        assert_eq!(projection.residency, Residency::Resident { bytes: *bytes });
    }

    let folder_index = files
        .entries()
        .iter()
        .position(|entry| entry.name == "Offline")
        .unwrap();
    files.debug_select(&[folder_index]);
    assert!(files.apply_command(FileCommand::KeepOffline));
    let folder = files
        .entries()
        .iter()
        .find(|entry| entry.name == "Offline")
        .unwrap();
    let folder_availability = folder.availability.as_ref().unwrap();
    assert_eq!(folder_availability.policy, AvailabilityPolicy::Pinned);
    assert!(!folder_availability.policy_inherited);
    assert_eq!(
        host_fs::attributes::get(mount.join("Offline"), fs::syncstate::PIN_XATTR)
            .unwrap()
            .as_deref(),
        Some(b"pinned".as_slice())
    );

    // Create a descendant after the user pins its folder. This crosses the
    // real FUSE mutation path and proves that Files receives the inherited
    // policy badge for future entries, rather than only for a pre-seeded
    // availability record.
    let later = mount.join("Offline/later.txt");
    std::fs::write(&later, b"created after folder pin").unwrap();
    let child_deadline = tilo_test_env::hang_deadline();
    loop {
        if std::fs::read_dir(mount.join("Offline")).is_ok_and(|entries| entries.count() == 1) {
            break;
        }
        assert!(
            ui::time::now() < child_deadline,
            "future descendant did not appear"
        );
        tokio::task::yield_now().await;
    }
    files.navigate(mount.join("Offline"));
    let child = files
        .entries()
        .iter()
        .find(|entry| entry.name == "later.txt")
        .expect("Files sees a descendant created after pinning its folder");
    let child_availability = child.availability.as_ref().unwrap();
    assert_eq!(child_availability.policy, AvailabilityPolicy::Pinned);
    assert!(child_availability.policy_inherited);
    assert_eq!(
        child_availability.residency,
        Residency::Resident { bytes: 24 }
    );
    {
        let registry = availability.lock().unwrap();
        for (_, record_id, _, _) in &expected {
            assert_eq!(
                registry.get(*record_id).unwrap().policy,
                fs::availability::AvailabilityPolicy::Automatic
            );
        }
        assert_eq!(
            registry.get(folder_record_id).unwrap().policy,
            fs::availability::AvailabilityPolicy::Pinned
        );
        drop(registry);
    }

    files.navigate(mount.clone());
    files.debug_select(&[folder_index]);
    assert!(files.apply_command(FileCommand::RemoveOfflinePin));
    let folder = files
        .entries()
        .iter()
        .find(|entry| entry.name == "Offline")
        .unwrap();
    assert_eq!(
        folder.availability.as_ref().unwrap().policy,
        AvailabilityPolicy::Automatic
    );
    files.navigate(mount.join("Offline"));
    let child = files
        .entries()
        .iter()
        .find(|entry| entry.name == "later.txt")
        .unwrap();
    assert_eq!(
        child.availability.as_ref().unwrap().policy,
        AvailabilityPolicy::Automatic
    );
    assert!(!child.availability.as_ref().unwrap().policy_inherited);

    drop(files);
    drop(env);
    session.umount_and_join().unwrap();
    shutdown_endpoint.close().await;
    shutdown_store.store().shutdown().await.unwrap();
}
