#![cfg(unix)]
#![allow(clippy::unwrap_used, reason = "owned integration fixture assertions")]
use runtime_fs::selected_file::SelectedFileBackend;
use runtime_fs::workspace_server::{
    WorkspaceBackend, WorkspaceFileBackend, serve_workspace_connected,
};
use runtime_fs::{FsOwner, FsRight, NativeGrantAuthority, OpenMode};
use std::{
    os::{fd::IntoRawFd, unix::net::UnixStream},
    sync::Arc,
};

fn client(stream: UnixStream) -> runtime_fs::FsClient {
    let transport = runtime_portal::UnixCapnpClient::from_connected_fd(
        stream.into_raw_fd(),
        capnp_rpc::rpc_twoparty_capnp::Side::Client,
    )
    .unwrap();
    runtime_fs::FsClient::from_raw(transport.bootstrap())
}
#[tokio::test(flavor = "multi_thread")]
async fn selected_file_private_wire_revokes_existing_handle_and_has_no_sibling_access() {
    let disk = tempfile::tempdir().unwrap();
    let selected = disk.path().join("chosen.txt");
    std::fs::write(&selected, b"held selected bytes").unwrap();
    std::fs::write(disk.path().join("sibling.txt"), b"private sibling").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.editor".into(),
    };
    let file = authority
        .admit_file(
            std::fs::File::open(&selected).unwrap(),
            owner.clone(),
            vec![FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let backend = SelectedFileBackend::new(session, file.clone()).unwrap();
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(backend),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    tokio::task::LocalSet::new()
        .run_until(async move {
            let fs = client(client_stream);
            let rows = fs.list("").await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows.first().unwrap().name, "file");
            assert!(fs.open("../sibling.txt", OpenMode::Read).await.is_err());
            assert!(fs.stat("sibling.txt").await.is_err());
            assert!(fs.open("file", OpenMode::ReadWriteExisting).await.is_err());
            assert!(fs.open("file", OpenMode::Write).await.is_err());
            assert!(fs.open("file", OpenMode::Append).await.is_err());
            let handle = fs.open("file", OpenMode::Read).await.unwrap();
            assert_eq!(handle.read_all().await.unwrap(), b"held selected bytes");
            assert!(handle.write_at(0, b"must refuse").await.is_err());
            authority.revoke_file(&file).unwrap();
            assert!(handle.read_at(0, 64).await.is_err());
            assert!(handle.metadata().await.is_err());
            assert!(handle.flush().await.is_err());
            assert!(fs.stat("file").await.is_err());
            assert!(fs.open("file", OpenMode::Read).await.is_err());
            assert_eq!(std::fs::read(&selected).unwrap(), b"held selected bytes");
            assert_eq!(
                std::fs::read(disk.path().join("sibling.txt")).unwrap(),
                b"private sibling"
            );
            drop(server);
            assert!(
                tokio::time::timeout(tilo_test_env::hang_bound(), fs.list(""))
                    .await
                    .unwrap()
                    .is_err()
            );
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_file_real_native_write_is_bounded_and_revocation_is_terminal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let disk = tempfile::tempdir().unwrap();
    let selected = disk.path().join("chosen.txt");
    std::fs::write(&selected, b"existing source bytes").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.editor".into(),
    };
    let descriptor = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&selected)
        .unwrap();
    let file = authority
        .admit_file(
            descriptor,
            owner.clone(),
            vec![FsRight::Read, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let retained = Arc::clone(&alive);
    session
        .attach_lifetime(Arc::new(move || retained.load(Ordering::Acquire)))
        .unwrap();
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = CountedBackend {
        selected: SelectedFileBackend::new(session, file).unwrap(),
        writes: Arc::clone(&writes),
        gate: None,
    };
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(backend),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    tokio::task::LocalSet::new()
        .run_until(async move {
            let fs = client(client_stream);
            let write = fs.open("file", OpenMode::Write).await.unwrap();
            assert_eq!(
                std::fs::metadata(&selected).unwrap().len(),
                0,
                "canonical Write truncates"
            );
            assert!(
                write.read_at(0, 32).await.is_err(),
                "Write is write-only in the existing schema"
            );
            assert!(
                write
                    .write_at(0, &vec![7; runtime_fs::MAX_READ_CHUNK as usize + 1])
                    .await
                    .is_err()
            );
            assert_eq!(
                std::fs::metadata(&selected).unwrap().len(),
                0,
                "oversized write made no source mutation"
            );
            assert_eq!(
                writes.load(Ordering::Acquire),
                0,
                "oversized RPC never called native writer"
            );
            assert_eq!(write.write_at(0, b"new native bytes").await.unwrap(), 16);
            assert_eq!(writes.load(Ordering::Acquire), 1);
            write.flush().await.unwrap();
            assert_eq!(std::fs::read(&selected).unwrap(), b"new native bytes");
            let read = fs.open("file", OpenMode::Read).await.unwrap();
            assert_eq!(
                read.read_at(0, u32::MAX).await.unwrap(),
                b"new native bytes"
            );
            alive.store(false, Ordering::Release);
            assert!(write.write_at(0, b"must not replace").await.is_err());
            assert!(write.flush().await.is_err());
            alive.store(true, Ordering::Release);
            assert!(write.write_at(0, b"still retired").await.is_err());
            assert!(read.read_at(0, 64).await.is_err());
            assert!(fs.open("file", OpenMode::Read).await.is_err());
            assert_eq!(std::fs::read(&selected).unwrap(), b"new native bytes");
            drop(server);
        })
        .await;
}

struct CountedBackend {
    selected: SelectedFileBackend,
    writes: Arc<std::sync::atomic::AtomicUsize>,
    gate: Option<Arc<WriteGate>>,
}
impl WorkspaceBackend for CountedBackend {
    fn list(&self, path: &str) -> Result<Vec<runtime_fs::DirEntry>, String> {
        self.selected.list(path)
    }
    fn stat(&self, path: &str) -> Result<runtime_fs::Metadata, String> {
        self.selected.stat(path)
    }
    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        self.selected.read(path, offset, max_len)
    }
    fn open(
        &self,
        path: &str,
        mode: OpenMode,
    ) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        self.selected.open(path, mode).map(|opened| {
            opened.map(|file| {
                Arc::new(CountedFile {
                    file,
                    writes: Arc::clone(&self.writes),
                    gate: self.gate.clone(),
                }) as Arc<dyn WorkspaceFileBackend>
            })
        })
    }
}
struct CountedFile {
    file: Arc<dyn WorkspaceFileBackend>,
    writes: Arc<std::sync::atomic::AtomicUsize>,
    gate: Option<Arc<WriteGate>>,
}
impl WorkspaceFileBackend for CountedFile {
    fn read(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        self.file.read(offset, max_len)
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(gate) = &self.gate {
            gate.wait()?;
        }
        self.file.write(offset, bytes)
    }
    fn flush(&self) -> Result<(), String> {
        self.file.flush()
    }
    fn metadata(&self) -> Result<runtime_fs::Metadata, String> {
        self.file.metadata()
    }
}

struct WriteGate {
    release: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
    entered: tokio::sync::mpsc::UnboundedSender<()>,
}
impl WriteGate {
    fn wait(&self) -> Result<(), String> {
        self.entered.send(()).map_err(|error| error.to_string())?;
        let (released, deadline) = self
            .changed
            .wait_timeout_while(
                self.release.lock().unwrap(),
                tilo_test_env::hang_bound().saturating_mul(2),
                |released| !*released,
            )
            .unwrap();
        if deadline.timed_out() && !*released {
            return Err("owned fixture release deadline".into());
        }
        Ok(())
    }
    fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct ReleaseOnDrop(Arc<WriteGate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_native_pipeline_refuses_excess_write_before_copy_or_invocation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let disk = tempfile::tempdir().unwrap();
    let selected = disk.path().join("chosen.txt");
    std::fs::write(&selected, b"before").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.editor".into(),
    };
    let descriptor = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&selected)
        .unwrap();
    let file = authority
        .admit_file(
            descriptor,
            owner.clone(),
            vec![FsRight::Read, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let (entered, mut waiting) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(WriteGate {
        release: std::sync::Mutex::new(false),
        changed: std::sync::Condvar::new(),
        entered,
    });
    let release = ReleaseOnDrop(Arc::clone(&gate));
    let writes = Arc::new(AtomicUsize::new(0));
    let backend = CountedBackend {
        selected: SelectedFileBackend::new(session, file).unwrap(),
        writes: Arc::clone(&writes),
        gate: Some(Arc::clone(&gate)),
    };
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(backend),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    tokio::task::LocalSet::new()
        .run_until(async move {
            let fs = client(client_stream);
            let handle = fs.open("file", OpenMode::Write).await.unwrap();
            let mut pending = Vec::new();
            for index in 0..8_u8 {
                let handle = handle.clone();
                pending.push(tokio::task::spawn_local(async move {
                    handle.write_at(u64::from(index), &[b'a' + index]).await
                }));
            }
            for _ in 0..8 {
                tokio::time::timeout(tilo_test_env::hang_bound(), waiting.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(writes.load(Ordering::Acquire), 8);
            let payload = vec![9; runtime_fs::MAX_READ_CHUNK as usize];
            let error =
                tokio::time::timeout(tilo_test_env::hang_bound(), handle.write_at(20, &payload))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert!(error.to_string().contains("workspace backend busy"));
            assert_eq!(
                writes.load(Ordering::Acquire),
                8,
                "excess RPC never called blocked native provider"
            );
            assert_eq!(std::fs::metadata(&selected).unwrap().len(), 0);
            gate.release();
            for task in pending {
                assert_eq!(task.await.unwrap().unwrap(), 1);
            }
            assert_eq!(std::fs::read(&selected).unwrap(), b"abcdefgh");
            assert_eq!(
                handle.write_at(8, b"i").await.unwrap(),
                1,
                "released admission is reusable"
            );
            assert_eq!(writes.load(Ordering::Acquire), 9);
            assert_eq!(std::fs::read(&selected).unwrap(), b"abcdefghi");
            server.retire().unwrap();
            assert!(handle.metadata().await.is_err());
            drop(release);
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_selected_private_connection_retire_and_malformed_disconnect_report_outcomes() {
    use std::{io::Write, net::Shutdown};
    for termination in ["retire", "eof", "malformed"] {
        let disk = tempfile::tempdir().unwrap();
        let selected = disk.path().join("chosen.txt");
        std::fs::write(&selected, b"actual selected native bytes").unwrap();
        let authority = NativeGrantAuthority::new();
        let owner = FsOwner {
            workspace: "owned-room".into(),
            app_id: "so.tilo.editor".into(),
        };
        let file = authority
            .admit_file(
                std::fs::File::open(&selected).unwrap(),
                owner.clone(),
                vec![FsRight::Read],
            )
            .unwrap();
        let session = authority
            .bind(
                owner,
                uuid::Uuid::new_v4(),
                &[],
                std::slice::from_ref(&file),
            )
            .unwrap();
        let backend = SelectedFileBackend::new(session, file).unwrap();
        let (server_stream, client_stream) = UnixStream::pair().unwrap();
        let mut control = client_stream.try_clone().unwrap();
        let server = serve_workspace_connected(
            server_stream,
            Arc::new(backend),
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        tokio::task::LocalSet::new()
            .run_until(async move {
                let fs = client(client_stream);
                let handle = fs.open("file", OpenMode::Read).await.unwrap();
                assert_eq!(
                    handle.read_all().await.unwrap(),
                    b"actual selected native bytes"
                );
                if termination == "retire" {
                    server.retire().unwrap();
                } else {
                    // A partial Capnp header + EOF is an actual wire failure, not
                    // an injected worker result or a backend method refusal.
                    if termination == "malformed" {
                        control.write_all(&[255]).unwrap();
                    }
                    control.shutdown(Shutdown::Write).unwrap();
                    let outcome = tokio::time::timeout(
                        tilo_test_env::hang_bound(),
                        tokio::task::spawn_blocking(move || server.wait()),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    if termination == "malformed" {
                        assert!(outcome.is_err(), "wait reports malformed RPC termination");
                    } else {
                        outcome.unwrap(); // canonical RpcSystem treats ordinary peer EOF as clean.
                    }
                }
                assert!(handle.metadata().await.is_err());
            })
            .await;
    }
}

#[cfg(target_os = "linux")]
fn assert_concurrent_appends(
    append_a: &runtime_fs::FsAppendHandle,
    append_b: &runtime_fs::FsAppendHandle,
    moved: &std::path::Path,
) -> Vec<u8> {
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            for _ in 0..64 {
                assert_eq!(append_a.append(b"AAAA").unwrap(), 4);
            }
        });
        let b = scope.spawn(|| {
            for _ in 0..64 {
                assert_eq!(append_b.append(b"BBBB").unwrap(), 4);
            }
        });
        a.join().unwrap();
        b.join().unwrap();
    });
    let bytes = std::fs::read(moved).unwrap();
    assert_eq!(bytes.len(), 8 + 128 * 4);
    let chunks: Vec<_> = bytes.get(8..).unwrap().chunks_exact(4).collect();
    assert_eq!(chunks.iter().filter(|chunk| **chunk == b"AAAA").count(), 64);
    assert_eq!(chunks.iter().filter(|chunk| **chunk == b"BBBB").count(), 64);
    assert!(
        chunks
            .iter()
            .all(|chunk| *chunk == b"AAAA" || *chunk == b"BBBB")
    );
    bytes
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn existing_read_write_and_atomic_append_use_live_exact_inode_custody() {
    use std::os::unix::fs::MetadataExt;
    let disk = tempfile::tempdir().unwrap();
    let chosen = disk.path().join("chosen");
    let moved = disk.path().join("moved");
    std::fs::write(&chosen, b"original").unwrap();
    let original = std::fs::metadata(&chosen).unwrap().ino();
    let authority = NativeGrantAuthority::new();
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.editor".into(),
    };
    let descriptor = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&chosen)
        .unwrap();
    let file = authority
        .admit_file(
            descriptor,
            owner.clone(),
            vec![FsRight::Read, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let append_a = session.open_append(&file).unwrap();
    let append_b = session.open_append(&file).unwrap();
    // Admission never changes the original OFD's append flag.
    assert_eq!(session.write_at(&file, 0, b"O").unwrap(), 1);
    assert_eq!(std::fs::read(&chosen).unwrap(), b"Original");
    std::fs::rename(&chosen, &moved).unwrap();
    std::fs::write(&chosen, b"unrelated replacement").unwrap();
    let bytes = assert_concurrent_appends(&append_a, &append_b, &moved);
    let backend = SelectedFileBackend::new(session.clone(), file.clone()).unwrap();
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
            let raw: runtime_fs::WorkspaceFsClient = transport.bootstrap();
            let fs = runtime_fs::FsClient::from_raw(raw.clone());
            let rw = fs.open("file", OpenMode::ReadWriteExisting).await.unwrap();
            assert_eq!(rw.metadata().await.unwrap().size, bytes.len() as u64);
            assert_eq!(rw.read_at(0, 8).await.unwrap(), b"Original");
            assert_eq!(rw.write_at(0, b"edited!!").await.unwrap(), 8);
            assert!(rw.append(b"bad mode").await.is_err());
            let append = fs.open("file", OpenMode::Append).await.unwrap();
            assert!(append.write_at(0, b"bad offset").await.is_err());
            assert!(append.read_at(0, 8).await.is_err());
            assert_eq!(append.append(b"tail").await.unwrap(), 4);
            append.flush().await.unwrap();
            assert!(
                append
                    .append(&vec![0; runtime_fs::MAX_WRITE_CHUNK as usize + 1])
                    .await
                    .is_err()
            );
            let mut raw_open = raw.open_request();
            raw_open.get().set_path("file");
            raw_open.get().set_mode(runtime_fs::raw::OpenMode::Append);
            let reply = raw_open.send().promise.await.unwrap();
            let raw_handle = reply.get().unwrap().get_handle().unwrap();
            let mut oversized = raw_handle.append_request();
            oversized
                .get()
                .set_bytes(&vec![0; runtime_fs::MAX_WRITE_CHUNK as usize + 1]);
            assert!(oversized.send().promise.await.is_err());
            authority.revoke_file(&file).unwrap();
            assert!(rw.read_at(0, 8).await.is_err());
            assert!(rw.write_at(0, b"bad").await.is_err());
            assert!(append.append(b"bad").await.is_err());
            assert!(append.flush().await.is_err());
            assert!(append_a.append(b"bad native").is_err());
            assert!(fs.open("file", OpenMode::ReadWriteExisting).await.is_err());
            drop(fs);
        })
        .await;
    server.retire().unwrap();
    assert_eq!(std::fs::metadata(&moved).unwrap().ino(), original);
    assert_eq!(std::fs::read(&chosen).unwrap(), b"unrelated replacement");
    let final_bytes = std::fs::read(&moved).unwrap();
    assert_eq!(final_bytes.len(), bytes.len() + 4);
    assert_eq!(&final_bytes[..8], b"edited!!");
    assert_eq!(&final_bytes[bytes.len()..], b"tail");
}

#[cfg(target_os = "linux")]
#[test]
fn append_final_effect_gate_observes_independently_expired_lifetime() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("chosen");
    std::fs::write(&path, b"original").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = FsOwner {
        workspace: "owned-room".into(),
        app_id: "so.tilo.editor".into(),
    };
    let file = authority
        .admit_file(
            std::fs::OpenOptions::new().write(true).open(&path).unwrap(),
            owner.clone(),
            vec![FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let armed_lifetime = Arc::clone(&armed);
    let lifetime_calls = Arc::clone(&calls);
    session
        .attach_lifetime(Arc::new(move || {
            // Fast in-memory native lifetime: once armed, the first check observes
            // live custody and the next observes its independent retirement.
            !armed_lifetime.load(Ordering::Acquire)
                || lifetime_calls.fetch_add(1, Ordering::AcqRel) == 0
        }))
        .unwrap();
    let append = session.open_append(&file).unwrap();
    armed.store(true, Ordering::Release);
    assert!(matches!(
        append.append(b"must refuse"),
        Err(runtime_fs::FsError::Closed)
    ));
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    armed.store(false, Ordering::Release);
    assert!(matches!(
        append.append(b"still refuse"),
        Err(runtime_fs::FsError::Closed)
    ));
    assert!(matches!(
        session.open_append(&file),
        Err(runtime_fs::FsError::Closed)
    ));
}
