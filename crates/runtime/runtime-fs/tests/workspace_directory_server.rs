#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "owned connected native capability assertions"
)]
use runtime_fs::operations::ItemState;
use runtime_fs::raw::{NativeItemState, native_job};
use runtime_fs::recovery::{OperationId, OperationOwner, OperationState};
use runtime_fs::{
    FsOwner, FsRight, NativeGrantAuthority,
    directory::DirectoryCapability,
    workspace_server::{WorkspaceBackend, serve_workspace_connected},
};
use std::{
    os::{
        fd::IntoRawFd,
        unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
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
    job: &native_job::Client,
    state: NativeItemState,
    failed: u64,
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
            let reply = tokio::time::timeout_at(deadline, job.poll_request().send().promise)
                .await
                .unwrap()
                .unwrap();
            let out = reply.get().unwrap();
            let bytes: [u8; 16] = out.get_operation_id().unwrap().try_into().unwrap();
            if let Some(previous) = &actual {
                assert_eq!(previous, &bytes);
            } else {
                actual = Some(bytes);
            }
            for row in out.get_updates().unwrap() {
                match row.which().unwrap() {
                    runtime_fs::raw::native_job_update::Item(item) => {
                        assert_eq!(item.unwrap().get_state().unwrap(), state);
                        saw_item = true;
                    }
                    runtime_fs::raw::native_job_update::Finished(done) => {
                        let done = done.unwrap();
                        assert_eq!(done.get_failed(), failed);
                        assert_eq!(done.get_cancelled(), 0);
                        assert_eq!(done.get_completed(), 1_u64.checked_sub(failed).unwrap());
                        assert!(saw_item, "finished without an actual native item outcome");
                        return OperationId::from_bytes(bytes);
                    }
                    runtime_fs::raw::native_job_update::Progress(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}
fn assert_raw_listing<'a>(
    directory: &'a runtime_fs::raw::directory_handle::Client,
    raw: &'a [u8],
) -> LocalFuture<'a, ()> {
    Box::pin(async move {
        let reply = directory.list_request().send().promise.await.unwrap();
        let rows = reply.get().unwrap().get_entries().unwrap();
        assert!(rows.iter().any(|entry| entry.get_name().unwrap() == raw));
        assert_eq!(rows.len(), 256);
        let next = reply.get().unwrap().get_next();
        assert_ne!(next, 0);
        let mut foreign = directory.list_request();
        foreign.get().set_cursor(next.checked_add(1).unwrap());
        assert!(foreign.send().promise.await.is_err());
        let mut continuation = directory.list_request();
        continuation.get().set_cursor(next);
        let second = continuation.send().promise.await.unwrap();
        assert_eq!(second.get().unwrap().get_entries().unwrap().len(), 3);
        assert_eq!(second.get().unwrap().get_next(), 0);
        let mut stale = directory.list_request();
        stale.get().set_cursor(next);
        assert!(stale.send().promise.await.is_err());
    })
}

fn assert_sdk_metadata<'a>(
    sdk_directory: &'a runtime_fs::directory_client::DirectoryClient,
    disk: &'a std::path::Path,
    name: &'a std::ffi::OsString,
) -> LocalFuture<'a, runtime_fs::directory_backend::DirectoryStat> {
    Box::pin(async move {
        let sdk_page = sdk_directory.list(0).await.unwrap();
        let root_stat = sdk_directory.metadata().await.unwrap();
        let root_facts = std::fs::metadata(disk).unwrap();
        assert_eq!(root_stat.identity, [root_facts.dev(), root_facts.ino()]);
        let absent = runtime_fs::EntryName::new("absent".into()).unwrap();
        assert!(sdk_directory.stat(&absent).await.unwrap().is_none());
        let observed = sdk_directory
            .stat(&runtime_fs::EntryName::new(name.to_owned()).unwrap())
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

fn assert_raw_write<'a>(
    directory: &'a runtime_fs::raw::directory_handle::Client,
    raw: &'a [u8],
    identity: [u64; 2],
) -> LocalFuture<'a, runtime_fs::raw::file_handle::Client> {
    Box::pin(async move {
        let mut open = directory.open_file_request();
        open.get().set_name(raw);
        open.get().set_expected_device(identity[0]);
        open.get().set_expected_inode(identity[1]);
        open.get()
            .set_mode(runtime_fs::raw::OpenMode::ReadWriteExisting);
        let reply = open.send().promise.await.unwrap();
        let file = reply.get().unwrap().get_handle().unwrap();
        let mut write = file.write_request();
        write.get().set_bytes(b"edited!!");
        assert_eq!(
            write
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_written(),
            8
        );
        file
    })
}

fn assert_durable_jobs<'a>(
    directory: &'a runtime_fs::raw::directory_handle::Client,
    disk: &'a std::path::Path,
    metadata: &'a std::path::Path,
    name: &'a std::ffi::OsString,
    store: &'a OperationOwner,
) -> LocalFuture<
    'a,
    (
        native_job::Client,
        runtime_fs::raw::directory_handle::Client,
    ),
> {
    Box::pin(async move {
        let mut collision = directory.rename_no_replace_request();
        collision
            .get()
            .set_name(std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str()));
        collision.get().set_new_name(b"victim");
        let reply = collision.send().promise.await.unwrap();
        let job = reply.get().unwrap().get_job().unwrap();
        let id = finish(&job, NativeItemState::Failed, 1).await;
        let report = store.report(&id).unwrap();
        assert_eq!(report.state, OperationState::Finished);
        assert_eq!(report.items.first().unwrap().state, ItemState::Failed);
        assert_eq!(std::fs::read(disk.join("victim")).unwrap(), b"victim");
        assert_eq!(std::fs::read(disk.join(name)).unwrap(), b"edited!!");
        let mut create = directory.create_file_request();
        create.get().set_name(b"created");
        let reply = create.send().promise.await.unwrap();
        let job = reply.get().unwrap().get_job().unwrap();
        let id = finish(&job, NativeItemState::Completed, 0).await;
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
        let mut mkdir = directory.create_directory_request();
        mkdir.get().set_name(b"folder");
        let reply = mkdir.send().promise.await.unwrap();
        let job = reply.get().unwrap().get_job().unwrap();
        finish(&job, NativeItemState::Completed, 0).await;
        let mut open = directory.open_directory_request();
        open.get().set_name(b"folder");
        let reply = open.send().promise.await.unwrap();
        let child = reply.get().unwrap().get_handle().unwrap();
        assert_eq!(
            child
                .list_request()
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_entries()
                .unwrap()
                .len(),
            0
        );
        (job, child)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_directory_rpc_keeps_native_names_rights_and_actual_durable_job_outcomes() {
    let disk = tempfile::tempdir().unwrap();
    let metadata = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let raw = vec![b'f', 0xff];
    let name = std::ffi::OsString::from_vec(raw.clone());
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
    tokio::task::LocalSet::new()
        .run_until(async {
            let transport = runtime_portal::UnixCapnpClient::from_connected_fd(
                client_stream.into_raw_fd(),
                capnp_rpc::rpc_twoparty_capnp::Side::Client,
            )
            .unwrap();
            let fs: runtime_fs::WorkspaceFsClient = transport.bootstrap();
            let sdk = runtime_fs::FsClient::from_raw(fs.clone());
            let reply = fs.directory_request().send().promise.await.unwrap();
            let directory = reply.get().unwrap().get_handle().unwrap();
            assert_raw_listing(&directory, &raw).await;
            let sdk_directory = sdk.directory().await.unwrap();
            let observed = assert_sdk_metadata(&sdk_directory, disk.path(), &name).await;
            let sdk_file = sdk_directory
                .open_file(
                    &runtime_fs::EntryName::new(name.clone()).unwrap(),
                    runtime_fs::OpenMode::ReadWriteExisting,
                )
                .await
                .unwrap();
            assert_eq!(sdk_file.read_at(0, 8).await.unwrap(), b"original");
            let file = assert_raw_write(&directory, &raw, observed.identity).await;
            let mut bad = directory.open_directory_request();
            bad.get().set_name(b"..");
            assert!(bad.send().promise.await.is_err());
            let (job, child) =
                assert_durable_jobs(&directory, disk.path(), metadata.path(), &name, &store).await;
            authority.revoke_directory(&root).unwrap();
            assert!(sdk.directory().await.is_err());
            assert!(sdk_file.read_at(0, 8).await.is_err());
            assert!(sdk_directory.list(0).await.is_err());
            let mut read = file.read_request();
            read.get().set_max_len(8);
            assert!(read.send().promise.await.is_err());
            assert!(child.list_request().send().promise.await.is_err());
            assert!(directory.list_request().send().promise.await.is_err());
            assert!(job.cancel_request().send().promise.await.is_err());
            assert_eq!(std::fs::read(disk.path().join(&name)).unwrap(), b"edited!!");
        })
        .await;
    server.retire().unwrap();
}
