use super::*;
use accounts::cloud_types::{FilesMode, set_files_mode};

pub(super) fn verify_process_handshake(service: &mut CloudService, mount: &Path) {
    // Preparation travels over the same child control pipe as file requests.
    // A just-finished upload/sync may still be flushing its metadata: retry only
    // the protocol's explicit busy errors, never an unexpected failure.
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    let token = loop {
        match service.request(
            CloudRequest::PrepareImport {
                account: "cloud-1".into(),
            },
            tilo_test_env::hang_bound(),
        ) {
            CloudResponse::ImportPrepared { token } => break token,
            CloudResponse::Error(error) => {
                assert!(
                    error.contains("is running") || error.contains("local changes"),
                    "{error}"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "preparation stayed busy: {error}"
                );
                std::thread::yield_now();
            }
            other => panic!("unexpected preparation response: {other:?}"),
        }
    };
    assert!(service.mounted().is_empty());
    assert!(!mount.exists());
    assert_eq!(
        service.request(
            CloudRequest::AbortImport {
                account: "cloud-1".into(),
                token
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Done
    );
    assert_eq!(
        std::fs::read_to_string(mount.join("Docs/hello.txt")).unwrap(),
        "hello from the cloud"
    );
    let CloudResponse::Mounts(mounts) =
        service.request(CloudRequest::List, tilo_test_env::hang_bound())
    else {
        panic!("a restored mount list")
    };
    assert!(mounts[0].paused);
}

fn prepare(service: &mut CloudService) -> String {
    match service.request(
        CloudRequest::PrepareImport {
            account: "files-1".into(),
        },
        tilo_test_env::hang_bound(),
    ) {
        CloudResponse::ImportPrepared { token } => token,
        other => panic!("preparation failed: {other:?}"),
    }
}

fn mode(root: &Path, mode: FilesMode) {
    let mut store = accounts::AccountStore::load_in(root);
    let mut account = store
        .get(&accounts::AccountId::from("files-1"))
        .unwrap()
        .clone();
    set_files_mode(&mut account.config, mode);
    store.upsert(account);
    store.save_in(root).unwrap();
}

#[test]
fn preparation_rejects_configuration_not_owned_by_the_active_mount() {
    let server = FakeDav::generic();
    server.put("a.txt", b"original");
    let mut account = serve(&server);
    let mut store = accounts::AccountStore::load_in(account.config_root.path());
    let original = store
        .get(&accounts::AccountId::from("files-1"))
        .unwrap()
        .clone();
    let mut changed = original.clone();
    changed.config["server_url"] = "https://changed.example/".into();
    store.upsert(changed);
    store.save_in(account.config_root.path()).unwrap();
    // No watcher/reconcile has replaced the live mount yet.
    assert!(matches!(
        account.service.request(
            CloudRequest::PrepareImport {
                account: "files-1".into()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    let ino = account.volume.resolve("a.txt").unwrap();
    let writer = account.volume.open_file(ino, true, false).unwrap();
    account.volume.write(writer, 0, b"retained").unwrap();
    account.volume.release(writer).unwrap();
    account.volume.upload_pending().unwrap();
    assert_eq!(server.get("a.txt").unwrap(), b"retained");
    store.upsert(original);
    store.save_in(account.config_root.path()).unwrap();
    account.service.shutdown();
}

#[test]
fn import_freeze_rejects_ordinary_mutations_and_new_writable_handles() {
    let server = FakeDav::generic();
    server.put("a.txt", b"original");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    let volume = &mount.volume;
    volume.sync_once().unwrap();
    let ino = volume.resolve("a.txt").unwrap();
    let reader = volume.open_file(ino, false, false).unwrap();
    volume.prepare_import().unwrap();
    assert_eq!(volume.open_file(ino, true, false), Err(libc::EROFS));
    assert_eq!(volume.write(reader, 0, b"changed"), Err(libc::EROFS));
    assert_eq!(volume.truncate(ino, 0), Err(libc::EROFS));
    assert_eq!(volume.create(ROOT_INO, "new.txt"), Err(libc::EROFS));
    assert_eq!(volume.mkdir(ROOT_INO, "new"), Err(libc::EROFS));
    assert_eq!(volume.unlink(ROOT_INO, "a.txt"), Err(libc::EROFS));
    assert_eq!(
        volume.rename(ROOT_INO, "a.txt", ROOT_INO, "b.txt"),
        Err(libc::EROFS)
    );
    assert_eq!(volume.read(reader, 0, 8).unwrap(), b"original");
    assert_eq!(server.get("a.txt").unwrap(), b"original");
    volume.shutdown();
}

#[test]
fn writable_handle_and_pending_queue_fail_without_losing_edits() {
    let server = FakeDav::generic();
    server.put("a.txt", b"original");
    let mut account = serve(&server);
    let ino = account.volume.resolve("a.txt").unwrap();
    let writer = account.volume.open_file(ino, true, false).unwrap();
    assert!(matches!(
        account.service.request(
            CloudRequest::PrepareImport {
                account: "files-1".into()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    // The failed freeze must not leave even an existing staged handle read-only.
    account.volume.write(writer, 0, b"modified").unwrap();
    account.volume.release(writer).unwrap();
    assert_eq!(account.volume.pending(), 1);
    assert!(matches!(
        account.service.request(
            CloudRequest::PrepareImport {
                account: "files-1".into()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    assert_eq!(account.volume.pending(), 1);
    let writer = account.volume.open_file(ino, true, false).unwrap();
    account.volume.write(writer, 0, b"retained").unwrap();
    account.volume.release(writer).unwrap();
    account.volume.upload_pending().unwrap();
    assert_eq!(server.get("a.txt").unwrap(), b"retained");
    let _ = prepare(&mut account.service);
    account.service.shutdown();
}

#[test]
fn mode_commit_consumes_hold_and_preserves_the_cache_directory() {
    let server = FakeDav::generic();
    server.put("a.txt", b"original");
    let mut account = serve(&server);
    let token = prepare(&mut account.service);
    assert_eq!(
        account.service.request(
            CloudRequest::BeginCommitImport {
                account: "files-1".into(),
                token: token.clone()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Done
    );
    assert!(account.service.mounted().is_empty());
    assert!(account.launcher.volume("files-1").is_none());
    account.service.reconcile();
    assert!(
        account.service.mounted().is_empty(),
        "reconcile remounted a held account"
    );
    mode(account.config_root.path(), FilesMode::Import);
    account.service.reconcile();
    assert!(account.service.mounted().is_empty());
    assert!(account.home.path().join("state").exists());
    assert_eq!(
        account.service.request(
            CloudRequest::CompleteImport {
                account: "files-1".into(),
                token
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Done
    );
    // Returning to Sync must mount immediately: a consumed hold cannot suppress it.
    mode(account.config_root.path(), FilesMode::Sync);
    account.service.reconcile();
    assert_eq!(account.service.mounted().len(), 1);
    account.service.shutdown();
}

#[test]
fn failed_metadata_save_abort_restores_prior_pause_and_sync_state() {
    for paused in [false, true] {
        let server = FakeDav::generic();
        server.put("a.txt", b"original");
        let mut account = serve(&server);
        if paused {
            assert_eq!(
                account.service.request(
                    CloudRequest::Pause {
                        account: "files-1".into()
                    },
                    tilo_test_env::hang_bound(),
                ),
                CloudResponse::Done
            );
        }
        let token = prepare(&mut account.service);
        assert_eq!(
            account.service.request(
                CloudRequest::BeginCommitImport {
                    account: "files-1".into(),
                    token: token.clone()
                },
                tilo_test_env::hang_bound(),
            ),
            CloudResponse::Done
        );
        let root = account.config_root.path();
        let mut store = accounts::AccountStore::load_in(root);
        let mut changed = store
            .get(&accounts::AccountId::from("files-1"))
            .unwrap()
            .clone();
        set_files_mode(&mut changed.config, FilesMode::Import);
        store.upsert(changed);
        // Force the production atomic account-store writer to fail at rename.
        // Restore the original metadata before asking the service to roll back.
        let metadata = accounts::store_path_in(root);
        let backup = metadata.with_extension("backup");
        std::fs::rename(&metadata, &backup).unwrap();
        std::fs::create_dir(&metadata).unwrap();
        assert!(store.save_in(root).is_err());
        std::fs::remove_dir(&metadata).unwrap();
        std::fs::rename(backup, metadata).unwrap();
        assert_eq!(
            account.service.request(
                CloudRequest::AbortImport {
                    account: "files-1".into(),
                    token
                },
                tilo_test_env::hang_bound(),
            ),
            CloudResponse::Done
        );
        let restored = account.launcher.volume("files-1").unwrap();
        assert_eq!(restored.is_paused(), paused);
        let ino = restored.resolve("a.txt").unwrap();
        let writer = restored.open_file(ino, true, false).unwrap();
        restored.write(writer, 0, b"restored").unwrap();
        restored.release(writer).unwrap();
        restored.upload_pending().unwrap();
        assert_eq!(server.get("a.txt").unwrap(), b"restored");
        account.service.shutdown();
    }
}

#[test]
fn interrupted_commit_has_explicit_recovery_before_and_after_metadata_save() {
    for saved in [false, true] {
        let server = FakeDav::generic();
        server.put("a.txt", b"original");
        let mut account = serve(&server);
        let token = prepare(&mut account.service);
        let writer =
            runtime_fs::cloud::import_writer_lock(account.config_root.path(), "files-1").unwrap();
        assert_eq!(
            account.service.request(
                CloudRequest::BeginCommitImport {
                    account: "files-1".into(),
                    token
                },
                tilo_test_env::hang_bound(),
            ),
            CloudResponse::Done
        );
        account.service.shutdown();
        account.service = CloudService::new(
            service_config(account.config_root.path(), account.home.path()),
            Box::new(account.launcher.clone()),
        );
        account.service.reconcile();
        assert!(account.service.mounted().is_empty());
        let recover = CloudRequest::RecoverImport {
            account: "files-1".into(),
        };
        assert!(matches!(
            account
                .service
                .request(recover.clone(), tilo_test_env::hang_bound()),
            CloudResponse::Error(_)
        ));
        if saved {
            mode(account.config_root.path(), FilesMode::Import);
        }
        drop(writer);
        assert_eq!(
            account
                .service
                .request(recover, tilo_test_env::hang_bound()),
            CloudResponse::Done
        );
        assert_eq!(account.service.mounted().len(), usize::from(!saved));
        if !saved {
            let restored = account.launcher.volume("files-1").unwrap();
            let ino = restored.resolve("a.txt").unwrap();
            let writer = restored.open_file(ino, true, false).unwrap();
            restored.write(writer, 0, b"retained").unwrap();
            restored.release(writer).unwrap();
            restored.upload_pending().unwrap();
            assert_eq!(server.get("a.txt").unwrap(), b"retained");
        }
        account.service.shutdown();
    }
}

#[test]
fn import_tokens_are_bound_to_account_and_public_configuration() {
    let server = FakeDav::generic();
    let mut account = serve(&server);
    let token = prepare(&mut account.service);
    assert!(matches!(
        account.service.request(
            CloudRequest::AbortImport {
                account: "another-account".into(),
                token: token.clone()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    let mut store = accounts::AccountStore::load_in(account.config_root.path());
    let mut config = store
        .get(&accounts::AccountId::from("files-1"))
        .unwrap()
        .clone();
    config.display_name = "Changed".into();
    store.upsert(config);
    store.save_in(account.config_root.path()).unwrap();
    assert!(matches!(
        account.service.request(
            CloudRequest::BeginCommitImport {
                account: "files-1".into(),
                token: token.clone()
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    assert!(matches!(
        account.service.request(
            CloudRequest::AbortImport {
                account: "files-1".into(),
                token
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    account.service.reconcile();
    assert_eq!(account.service.mounted().len(), 1);
    account.service.shutdown();
}
