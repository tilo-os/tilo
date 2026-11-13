#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "owned connected native capability assertions"
)]
use runtime_fs::directory_backend::{DirectoryStat, JobUpdate};
use runtime_fs::directory_client::{DirectoryClient, NativeJobClient};
use runtime_fs::operations::ItemState;
use runtime_fs::recovery::{OperationId, OperationOwner, OperationState};
use runtime_fs::{
    EntryName, FileHandle, FsOwner, FsRight, NativeGrantAuthority, OpenMode,
    directory::DirectoryCapability,
    workspace_server::{WorkspaceBackend, serve_workspace_connected},
};
use std::{
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    sync::Arc,
    time::Duration,
};
struct Backend(Arc<DirectoryCapability>);
impl WorkspaceBackend for Backend {
    fn directory(
        &self,
    ) -> Result<Option<Arc<dyn runtime_fs::directory_backend::WorkspaceDirectoryBackend>>, String>
    {
        Ok(Some(self.0.clone()))
    }
    fn list(&self, _: &str) -> Result<Vec<runtime_fs::DirEntry>, String> {
        Err("use native capability".into())
    }
    fn stat(&self, _: &str) -> Result<runtime_fs::Metadata, String> {
        Err("use native capability".into())
    }
    fn read(&self, _: &str, _: u64, _: u32) -> Result<Vec<u8>, String> {
        Err("use native capability".into())
    }
}
type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

fn finish(
    job: &mut NativeJobClient,
    state: ItemState,
    failed: usize,
) -> LocalFuture<'_, OperationId> {
    Box::pin(async move {
        let deadline = tokio::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .unwrap();
        let mut actual = None;
        let mut saw_item = false;
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "native job did not finish"
            );
            let poll = tokio::time::timeout_at(deadline, job.poll())
                .await
                .unwrap()
                .unwrap();
            if let Some(previous) = &actual {
                assert_eq!(previous, &poll.operation_id);
            } else {
                actual = Some(poll.operation_id);
            }
            for update in poll.updates {
                match update {
                    JobUpdate::Item { state: actual, .. } => {
                        assert_eq!(actual, state);
                        saw_item = true;
                    }
                    JobUpdate::Finished {
                        completed,
                        failed: reported,
                        cancelled,
                    } => {
                        assert_eq!(reported, failed);
                        assert_eq!(cancelled, 0);
                        assert_eq!(completed, 1_usize.checked_sub(failed).unwrap());
                        assert!(saw_item, "finished without an actual native item outcome");
                        return OperationId::from_bytes(actual.expect("polled operation"));
                    }
                    JobUpdate::Progress { .. } => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}
fn assert_listing<'a>(directory: &'a DirectoryClient, raw: &'a [u8]) -> LocalFuture<'a, ()> {
    Box::pin(async move {
        let page = directory.list(0).await.unwrap();
        assert!(
            page.entries
                .iter()
                .any(|entry| entry.name.as_os_str().as_encoded_bytes() == raw)
        );
        assert_eq!(page.entries.len(), 256);
        assert_ne!(page.next, 0);
        assert!(
            directory
                .list(page.next.checked_add(1).unwrap())
                .await
                .is_err()
        );
        let second = directory.list(page.next).await.unwrap();
        assert_eq!(second.entries.len(), 3);
        assert_eq!(second.next, 0);
        assert!(directory.list(page.next).await.is_err());
    })
}

fn assert_sdk_metadata<'a>(
    sdk_directory: &'a DirectoryClient,
    disk: &'a std::path::Path,
    name: &'a std::ffi::OsString,
) -> LocalFuture<'a, DirectoryStat> {
    Box::pin(async move {
        let sdk_page = sdk_directory.list(0).await.unwrap();
        let root_stat = sdk_directory.metadata().await.unwrap();
        let root_facts = std::fs::metadata(disk).unwrap();
        assert_eq!(root_stat.identity, [root_facts.dev(), root_facts.ino()]);
        let absent = EntryName::new("absent".into()).unwrap();
        assert!(sdk_directory.stat(&absent).await.unwrap().is_none());
        let observed = sdk_directory
            .stat(&EntryName::new(name.to_owned()).unwrap())
            .await
            .unwrap()
            .unwrap();
        let actual = std::fs::metadata(disk.join(name)).unwrap();
        assert_eq!(observed.identity, [actual.dev(), actual.ino()]);
        assert!(
            sdk_page
                .entries
                .iter()
                .any(|entry| entry.name.as_os_str() == name.as_os_str())
        );
        observed
    })
}

fn assert_write<'a>(
    directory: &'a DirectoryClient,
    name: &'a EntryName,
    identity: [u64; 2],
) -> LocalFuture<'a, FileHandle> {
    Box::pin(async move {
        let file = directory
            .open_file_expected(name, OpenMode::ReadWriteExisting, identity)
            .await
            .unwrap();
        assert_eq!(file.write_at(0, b"edited!!").await.unwrap(), 8);
        file
    })
}

fn assert_durable_jobs<'a>(
    directory: &'a DirectoryClient,
    disk: &'a std::path::Path,
    metadata: &'a std::path::Path,
    name: &'a EntryName,
    store: &'a OperationOwner,
) -> LocalFuture<'a, (NativeJobClient, DirectoryClient)> {
    Box::pin(async move {
        let victim = EntryName::new("victim".into()).unwrap();
        let mut job = directory.rename_no_replace(name, &victim).await.unwrap();
        let id = finish(&mut job, ItemState::Failed, 1).await;
        let report = store.report(&id).unwrap();
        assert_eq!(report.state, OperationState::Finished);
        assert_eq!(report.items.first().unwrap().state, ItemState::Failed);
        assert_eq!(std::fs::read(disk.join("victim")).unwrap(), b"victim");
        assert_eq!(
            std::fs::read(disk.join(name.as_os_str())).unwrap(),
            b"edited!!"
        );
        let mut create = directory
            .create_file(&EntryName::new("created".into()).unwrap())
            .await
            .unwrap();
        let id = finish(&mut create, ItemState::Completed, 0).await;
        let report = store.report(&id).unwrap();
        assert_eq!(report.items.first().unwrap().state, ItemState::Completed);
        assert!(
            metadata
                .join(format!(
                    "{}.receipt",
                    uuid::Uuid::from_bytes(*id.as_bytes())
                ))
                .is_file()
        );
        assert_eq!(std::fs::read(disk.join("created")).unwrap(), b"");
        let mut mkdir = directory
            .create_directory(&EntryName::new("folder".into()).unwrap())
            .await
            .unwrap();
        finish(&mut mkdir, ItemState::Completed, 0).await;
        let child = directory
            .directory(&EntryName::new("folder".into()).unwrap())
            .await
            .unwrap();
        assert_eq!(child.list(0).await.unwrap().entries.len(), 0);
        (job, child)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn directory_rpc_keeps_native_names_rights_and_actual_durable_job_outcomes() {
    let disk = tempfile::tempdir().unwrap();
    let metadata = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let raw = vec![b'f', 0xff];
    let name = std::ffi::OsString::from_vec(raw.clone());
    let entry = EntryName::new(name.clone()).unwrap();
    std::fs::write(disk.path().join(&name), b"original").unwrap();
    std::fs::write(disk.path().join("victim"), b"victim").unwrap();
    for index in 0..257 {
        std::fs::write(disk.path().join(format!("page-{index:03}")), b"").unwrap();
    }
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            std::fs::File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![
                FsRight::Enumerate,
                FsRight::Read,
                FsRight::Write,
                FsRight::Create,
                FsRight::Rename,
            ],
        )
        .unwrap();
    let session = authority
        .bind(
            owner.clone(),
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let store = OperationOwner::open(std::fs::File::open(metadata.path()).unwrap(), owner).unwrap();
    authority.attach_operations(Arc::clone(&store)).unwrap();
    let backend = Backend(Arc::new(
        DirectoryCapability::new(session, root.clone()).unwrap(),
    ));
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(backend),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let connection =
        runtime_fs::connection::Connection::connect("directory-server-client", client_stream)
            .unwrap();
    let sdk = runtime_fs::FsClient::from_object(runtime_fs::connection::Object::root(Arc::clone(
        &connection,
    )));
    let directory = sdk.directory().await.unwrap();
    assert_listing(&directory, &raw).await;
    let sdk_directory = sdk.directory().await.unwrap();
    let observed = assert_sdk_metadata(&sdk_directory, disk.path(), &name).await;
    let sdk_file = sdk_directory
        .open_file(&entry, OpenMode::ReadWriteExisting)
        .await
        .unwrap();
    assert_eq!(sdk_file.read_at(0, 8).await.unwrap(), b"original");
    let file = assert_write(&directory, &entry, observed.identity).await;
    assert!(
        runtime_fs::connection::Object::root(Arc::clone(&connection))
            .call(runtime_fs::wire::FsRequest::OpenDirectory {
                name: b"..".to_vec(),
            })
            .await
            .is_err(),
        "a traversal name is refused at the server"
    );
    let (dir_job, child) =
        assert_durable_jobs(&directory, disk.path(), metadata.path(), &entry, &store).await;
    authority.revoke_directory(&root).unwrap();
    assert!(sdk.directory().await.is_err());
    assert!(sdk_file.read_at(0, 8).await.is_err());
    assert!(sdk_directory.list(0).await.is_err());
    assert!(file.read_at(0, 8).await.is_err());
    assert!(child.list(0).await.is_err());
    assert!(directory.list(0).await.is_err());
    assert!(dir_job.cancel().await.is_err());
    assert_eq!(std::fs::read(disk.path().join(&name)).unwrap(), b"edited!!");
    server.retire().unwrap();
}
