#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "owned connected capability assertions")]

use runtime_fs::{
    FsOwner, FsRight, NativeGrantAuthority, directory::DirectoryWorkspaceBackend,
    selected_file::SelectedFileBackend, workspace_server::serve_workspace_connected,
};
use std::{
    fs::File,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::Arc,
};
use tilo_fs_client::{
    EntryName, OpenMode,
    directory_connection::{DirectoryConnection, ROOT_DIRECTORY},
    directory_types::{ItemState, JobUpdate},
    selected_client::SelectedFileConnection,
};

fn assert_failed_job(client: &DirectoryConnection, job: u64) {
    let deadline = tilo_test_env::hang_deadline();
    let mut failed = false;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "native refusal did not finish"
        );
        for update in client.poll(job).unwrap().updates {
            match update {
                JobUpdate::Progress { .. } => {}
                JobUpdate::Item {
                    item,
                    state,
                    destination,
                    error,
                    recoveries,
                } => {
                    assert_eq!(item, 0);
                    assert_eq!(state, ItemState::Failed);
                    assert!(destination.is_none());
                    assert!(error.is_some());
                    assert!(recoveries.is_empty());
                    failed = true;
                }
                JobUpdate::Finished {
                    completed,
                    failed: count,
                    cancelled,
                } => {
                    assert!(failed);
                    assert_eq!((completed, count, cancelled), (0, 1, 0));
                    return;
                }
            }
        }
    }
}

fn owner() -> FsOwner {
    FsOwner {
        workspace: "client-boundary".into(),
        app_id: "so.tilo.editor".into(),
    }
}

#[test]
fn mit_selected_client_keeps_only_the_admitted_inode_and_observes_revocation() {
    let disk = tempfile::tempdir().unwrap();
    let chosen = disk.path().join("chosen");
    std::fs::write(&chosen, b"held bytes").unwrap();
    std::fs::write(disk.path().join("sibling"), b"private bytes").unwrap();
    let authority = NativeGrantAuthority::new();
    let file = authority
        .admit_file(File::open(&chosen).unwrap(), owner(), vec![FsRight::Read])
        .unwrap();
    let session = authority
        .bind(
            owner(),
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let backend = SelectedFileBackend::new(session, file.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (host, app) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(host, Arc::new(backend), runtime.handle().clone()).unwrap();
    let client = SelectedFileConnection::connected(app).unwrap();
    let held = client.open(OpenMode::Read).unwrap();
    std::fs::rename(&chosen, disk.path().join("retained")).unwrap();
    std::fs::write(&chosen, b"replacement bytes").unwrap();
    assert_eq!(client.read(held, 0, 64).unwrap(), b"held bytes");
    assert!(client.open(OpenMode::ReadWriteExisting).is_err());
    assert!(client.write(held, 0, b"unauthorized").is_err());
    authority.revoke_file(&file).unwrap();
    assert!(client.check_scope(held).is_err());
    assert!(client.read(held, 0, 64).is_err());
    assert_eq!(std::fs::read(&chosen).unwrap(), b"replacement bytes");
    assert_eq!(
        std::fs::read(disk.path().join("sibling")).unwrap(),
        b"private bytes"
    );
    drop(client);
    server.retire().unwrap();
}

#[test]
fn mit_directory_client_cannot_promote_read_custody_to_creation_or_history_authority() {
    let disk = tempfile::tempdir().unwrap();
    std::fs::write(disk.path().join("chosen"), b"read only").unwrap();
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner(),
            vec![FsRight::Enumerate, FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(
            owner(),
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let journal = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let store =
        runtime_fs::recovery::OperationOwner::open(File::open(journal.path()).unwrap(), owner())
            .unwrap();
    authority.attach_operations(store).unwrap();
    let backend = DirectoryWorkspaceBackend::new(session, root.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (host, app) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(host, Arc::new(backend), runtime.handle().clone()).unwrap();
    let client = DirectoryConnection::connected(app).unwrap();
    let chosen = EntryName::new("chosen".into()).unwrap();
    let listing: runtime_fs::directory_backend::DirectoryRows =
        client.list(ROOT_DIRECTORY, 0).unwrap();
    assert_eq!(listing.entries.len(), 1);
    let held = client
        .open(ROOT_DIRECTORY, &chosen, OpenMode::Read)
        .unwrap();
    assert_eq!(client.read(held, 0, 64).unwrap(), b"read only");
    assert!(
        client
            .open(ROOT_DIRECTORY, &chosen, OpenMode::Write)
            .is_err()
    );
    let ungranted = EntryName::new("ungranted".into()).unwrap();
    let refused = client.create_file(ROOT_DIRECTORY, &ungranted).unwrap();
    assert_failed_job(&client, refused);
    assert!(client.recoveries(ROOT_DIRECTORY, &[]).is_err());
    let observed = client.stat(ROOT_DIRECTORY, &chosen).unwrap().unwrap();
    assert!(
        client
            .history_query(
                ROOT_DIRECTORY,
                &chosen,
                observed.identity,
                b"user.tilo.history"
            )
            .is_err()
    );
    assert!(!disk.path().join("ungranted").exists());
    authority.revoke_directory(&root).unwrap();
    assert!(client.read(held, 0, 64).is_err());
    assert!(client.list(ROOT_DIRECTORY, 0).is_err());
    assert_eq!(
        std::fs::read(disk.path().join("chosen")).unwrap(),
        b"read only"
    );
    drop(client);
    server.retire().unwrap();
}
