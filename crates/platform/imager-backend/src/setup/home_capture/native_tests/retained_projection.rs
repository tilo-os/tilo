//! Actual signed service / root-mounted FUSE boundary qualification only.
//! This does NOT manufacture InstalledRetainedLaunchInput or claim certified
//! Shell/installed-source admission. Those separate native fixtures stay required.
use super::super::runtime::TrustedCaptureService;
use launcher::{
    owned_service::{
        self, RetainedIoAdmission, RetainedIoAuthority, RetainedIoCredentials, ServiceAuthority,
        ServiceCancellation, ServiceEndpoint, ServiceLaunch, ServiceLeaseKey, SupervisedRun,
        TargetProjectionAdmission, TargetProjectionAuthority, TargetProjectionCredentials,
        TargetProjectionEndpoint,
    },
    run::{IdentitySource, LaunchMode, RunOptions},
};
use runtime_fs::{
    DirEntry, EntryKind, Metadata, OpenMode,
    selected_client::SelectedFileConnection,
    workspace_server::{self, WorkspaceBackend, WorkspaceFileBackend},
};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const SOURCE_UID: u32 = 31001;
const SOURCE_GID: u32 = 31001;
const SOURCE_GROUP: u32 = 31003;
const FILE_OWNER: u32 = 31004;
const TARGET_UID: u32 = 31002;
const TARGET_GID: u32 = 31002;
const CHILD: &str = "setup::home_capture::native_tests::retained_projection::target_stdio_child";
const NAME: &str = "selected.txt";
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn assert_kernel_credentials(stream: &UnixStream, uid: u32, gid: u32, groups: &[u32]) {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    assert_eq!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &raw mut length,
            )
        },
        0
    );
    assert_eq!(length as usize, std::mem::size_of::<libc::ucred>());
    assert_eq!((peer.uid, peer.gid), (uid, gid));
    assert!(peer.pid > 1);
    let mut status = String::new();
    File::open(format!("/proc/{}/status", peer.pid))
        .unwrap()
        .take(16385)
        .read_to_string(&mut status)
        .unwrap();
    assert!(status.len() <= 16384);
    let observed: Vec<u32> = status
        .lines()
        .find_map(|line| line.strip_prefix("Groups:"))
        .expect("actual live peer group list observable")
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(observed, groups);
    for (name, id) in [("Uid:", uid), ("Gid:", gid)] {
        let ids: Vec<u32> = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(ids, [id; 4], "real/effective/saved/filesystem IDs agree");
    }
    for name in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .trim();
        assert_eq!(
            u64::from_str_radix(value, 16).unwrap(),
            0,
            "worker retained {name}"
        );
    }
    assert_eq!(
        status
            .lines()
            .find_map(|line| line.strip_prefix("NoNewPrivs:"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        status
            .lines()
            .find_map(|line| line.strip_prefix("Seccomp:"))
            .unwrap()
            .trim(),
        "2"
    );
}

// These are explicitly disposable native fixture credentials, not a person
// proof or retained installed consent. Production admission isn't overridden.
struct FixtureAuthority(Arc<AtomicBool>);
impl ServiceAuthority for FixtureAuthority {
    fn check(&self) -> io::Result<()> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(denied())
        }
    }
}
impl RetainedIoAuthority for FixtureAuthority {
    fn source_credentials(&self) -> io::Result<RetainedIoCredentials> {
        self.check()?;
        RetainedIoCredentials::admitted(SOURCE_UID, SOURCE_GID, &[SOURCE_GROUP])
    }
    fn target_uid(&self) -> u32 {
        TARGET_UID
    }
    fn context_sha256(&self) -> [u8; 32] {
        [21; 32]
    }
}
impl TargetProjectionAuthority for FixtureAuthority {
    fn projection_mode(&self) -> owned_service::TargetProjectionMode {
        owned_service::TargetProjectionMode::SelectedFile
    }
    fn target_credentials(&self) -> io::Result<TargetProjectionCredentials> {
        self.check()?;
        TargetProjectionCredentials::admitted(TARGET_UID, TARGET_GID)
    }
    fn context_sha256(&self) -> [u8; 32] {
        [22; 32]
    }
    fn source_view_sha256(&self) -> [u8; 32] {
        [21; 32]
    }
    fn presentation_name(&self) -> io::Result<OsString> {
        self.check()?;
        Ok(NAME.into())
    }
}

struct Worker {
    cancel: ServiceCancellation,
    key: ServiceLeaseKey,
    working: tempfile::TempDir,
    job: Option<JoinHandle<SupervisedRun>>,
}
impl Worker {
    fn new(root: &Path) -> Self {
        Self {
            cancel: ServiceCancellation::default(),
            key: ServiceLeaseKey::default(),
            working: tempfile::Builder::new()
                .prefix("service-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in(root)
                .unwrap(),
            job: None,
        }
    }
    fn retire(&mut self) -> io::Result<()> {
        self.cancel.cancel();
        let Some(job) = self.job.take() else {
            return Ok(());
        };
        let run = job
            .join()
            .map_err(|_| io::Error::other("owned service supervisor panicked"))?;
        let matches = run.shutdown.as_ref().is_some_and(|r| {
            r.matches(
                &self.key,
                self.working.path(),
                owned_service::RETAINED_IO_PACKAGE_ID,
            )
        }) || run.result.as_ref().is_ok_and(|r| {
            r.matches(
                &self.key,
                self.working.path(),
                owned_service::RETAINED_IO_PACKAGE_ID,
            )
        });
        if !matches {
            return Err(io::Error::other(format!(
                "actual service shutdown receipt missing: {:?}",
                run.result.as_ref().err()
            )));
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.retire();
    }
}
fn launch(service: TrustedCaptureService, worker: &Worker, argument: &str) -> ServiceLaunch {
    let manifest = launcher::manifest::read_manifest(&service.manifest).unwrap();
    assert_eq!(manifest.sandbox.net.mode, "off");
    assert!(manifest.sandbox.net.hosts.is_empty());
    assert!(manifest.sandbox.portals.is_empty());
    ServiceLaunch {
        manifest: service.manifest.clone(),
        payload: service.payload.clone(),
        options: RunOptions {
            no_services: true,
            expected_package_hash: Some(service.package_hash.clone()),
            app_id: Some(owned_service::RETAINED_IO_PACKAGE_ID.into()),
            data_home: Some(worker.working.path().into()),
            identity: IdentitySource::None,
            mode: LaunchMode::Service {
                passthrough_argv: vec![argument.into()],
            },
            ..RunOptions::default()
        },
    }
}
fn source_worker(
    root: &Path,
    file: File,
    authority: Arc<FixtureAuthority>,
) -> (Worker, ServiceEndpoint) {
    let mut worker = Worker::new(root);
    let service = TrustedCaptureService::native()
        .expect("actual pinned signed source/target worker required");
    let admission = RetainedIoAdmission::admit(file, authority)
        .unwrap()
        .with_native_runtime(&service.root, &service.publisher)
        .unwrap()
        .with_installation(service.installed.clone(), &service.service_publisher)
        .unwrap();
    let launch = launch(service, &worker, owned_service::RETAINED_IO_WORKER_ARGUMENT);
    let (send, receive) = mpsc::sync_channel(1);
    let cancel = worker.cancel.clone();
    let key = worker.key.clone();
    worker.job = Some(std::thread::spawn(move || {
        owned_service::run_retained_io_connected(
            launch,
            admission,
            cancel,
            key,
            Some(Box::new(move |endpoint| {
                send.send(endpoint).map_err(io::Error::other)
            })),
        )
    }));
    let endpoint = receive
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("actual source namespace enrolled");
    (worker, endpoint)
}
fn target_worker(
    root: &Path,
    authority: Arc<FixtureAuthority>,
) -> (Worker, Arc<TargetProjectionEndpoint>) {
    let mut worker = Worker::new(root);
    let service = TrustedCaptureService::native().unwrap();
    let admission = TargetProjectionAdmission::admit(authority)
        .unwrap()
        .with_native_runtime(&service.root, &service.publisher)
        .unwrap()
        .with_installation(service.installed.clone(), &service.service_publisher)
        .unwrap();
    let launch = launch(service, &worker, "--tilo-retained-target-projection");
    let (send, receive) = mpsc::sync_channel(1);
    let cancel = worker.cancel.clone();
    let key = worker.key.clone();
    worker.job = Some(std::thread::spawn(move || {
        owned_service::run_target_projection_connected(
            launch,
            admission,
            cancel,
            key,
            Box::new(move |endpoint| send.send(Arc::new(endpoint)).map_err(io::Error::other)),
        )
    }));
    let endpoint = receive
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("actual target namespace enrolled");
    (worker, endpoint)
}

// A test transport adapter over the REAL source worker; not the production
// installed relay. Finite canonical server admission bounds all these callbacks.
struct Bridge {
    source: ServiceEndpoint,
    target: Arc<TargetProjectionEndpoint>,
    client: Mutex<Option<SelectedFileConnection>>,
}
impl Bridge {
    fn call<T>(
        &self,
        call: impl FnOnce(&SelectedFileConnection) -> io::Result<T>,
    ) -> Result<T, String> {
        let result: io::Result<T> = (|| {
            let client = self.client.lock().map_err(|_| denied())?;
            self.source.custody()?;
            self.target.check()?;
            let value = call(client.as_ref().ok_or_else(denied)?)?;
            self.source.custody()?;
            self.target.check()?;
            Ok(value)
        })();
        result.map_err(|e| e.to_string())
    }
}
struct Handle {
    bridge: Arc<Bridge>,
    id: u64,
}
impl WorkspaceFileBackend for Handle {
    fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        self.bridge.call(|c| c.read(self.id, offset, len))
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        self.bridge.call(|c| c.write(self.id, offset, bytes))
    }
    fn flush(&self) -> Result<(), String> {
        self.bridge.call(|c| c.flush(self.id))
    }
    fn metadata(&self) -> Result<Metadata, String> {
        self.bridge.call(|c| c.metadata_handle(self.id))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.bridge.call(|c| c.release(self.id));
    }
}
struct Backend(Arc<Bridge>);
impl WorkspaceBackend for Backend {
    fn open(
        &self,
        path: &str,
        mode: OpenMode,
    ) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        if path != "file" || matches!(mode, OpenMode::Append) {
            return Err("unsupported selection".into());
        }
        let id = self.0.call(|c| c.open(mode))?;
        Ok(Some(Arc::new(Handle {
            bridge: Arc::clone(&self.0),
            id,
        })))
    }
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        if !path.is_empty() {
            return Err("not directory".into());
        }
        Ok(vec![DirEntry {
            name: "file".into(),
            metadata: self.0.call(|c| c.metadata())?,
        }])
    }
    fn stat(&self, path: &str) -> Result<Metadata, String> {
        if path == "file" {
            self.0.call(|c| c.metadata())
        } else {
            Err("not selected".into())
        }
    }
    fn read(&self, path: &str, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        self.open(path, OpenMode::Read)?.unwrap().read(offset, len)
    }
}
struct Consumer {
    child: Child,
    _probe: tempfile::TempDir,
}
impl Drop for Consumer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn consumer(view: &File, state: &File, source: &File) -> (Consumer, UnixStream) {
    // The real qualification executable may live behind a root-private
    // ancestor. Stage only this actual executable under a separate root-owned
    // searchable probe directory; source/control workspaces remain 0700.
    let probe = tempfile::Builder::new()
        .prefix("tilo-target-probe-")
        .permissions(std::fs::Permissions::from_mode(0o711))
        .tempdir_in("/run")
        .unwrap();
    let executable = probe.path().join("actual-native-tests");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    parent
        .set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let descriptors = [
        view.as_raw_fd(),
        state.as_raw_fd(),
        source.as_raw_fd(),
        child.as_raw_fd(),
    ];
    let mut command = Command::new(executable);
    command
        .args(["--exact", CHILD, "--nocapture"])
        .env("TILO_TARGET_STDIO_FIXTURE", "1");
    unsafe {
        command.pre_exec(move || {
            // Duplicate away from the destination range before replacing any fd.
            let mut owned = Vec::new();
            for fd in descriptors {
                let copy = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 100);
                if copy < 0 {
                    return Err(io::Error::last_os_error());
                }
                owned.push(File::from_raw_fd(copy));
            }
            for (index, fd) in owned.iter().enumerate() {
                if libc::dup2(fd.as_raw_fd(), 70 + index as i32) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setgroups(0, std::ptr::null()) < 0
                || libc::setgid(TARGET_GID) < 0
                || libc::setuid(TARGET_UID) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    (
        Consumer {
            child: command.spawn().unwrap(),
            _probe: probe,
        },
        parent,
    )
}
#[test]
fn target_stdio_child() {
    if std::env::var_os("TILO_TARGET_STDIO_FIXTURE").is_none() {
        return;
    }
    let mut stream = unsafe { UnixStream::from_raw_fd(73) };
    assert_eq!(rustix::process::getuid().as_raw(), TARGET_UID);
    assert_eq!(rustix::process::geteuid().as_raw(), TARGET_UID);
    assert_eq!(rustix::process::getgid().as_raw(), TARGET_GID);
    assert_eq!(
        unsafe { libc::getgroups(0, std::ptr::null_mut()) },
        0,
        "normal target has no original groups"
    );
    let view = PathBuf::from("/proc/self/fd/70").join(NAME);
    let state = PathBuf::from("/proc/self/fd/71/personal-state");
    let bare_source = PathBuf::from("/proc/self/fd/72").join(NAME);
    assert!(
        std::fs::read(&bare_source).is_err(),
        "source requires original supplementary group"
    );
    assert_eq!(std::fs::read(&view).unwrap(), b"original source bytes");
    std::fs::write(&view, b"updated").unwrap();
    assert_eq!(std::fs::read(&view).unwrap(), b"updated");
    assert_eq!(std::fs::read(&state).unwrap(), b"ordinary target state");
    std::fs::write(&state, b"target still owns its state").unwrap();
    stream.write_all(b"W").unwrap();
    let mut signal = [0];
    stream.read_exact(&mut signal).unwrap();
    assert_eq!(&signal, b"R");
    assert!(
        std::fs::read(&view).is_err(),
        "revoked view refuses actual stdfs read"
    );
    assert_eq!(
        std::fs::read(&state).unwrap(),
        b"target still owns its state"
    );
    std::fs::write(&state, b"private state survives source revoke").unwrap();
    stream.write_all(b"E").unwrap();
}
fn chown(path: &Path, uid: u32, gid: u32) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}
#[test]
fn signed_private_projection_preserves_source_identity_and_normal_target_credentials() {
    assert_eq!(
        rustix::process::getuid().as_raw(),
        0,
        "disposable root native qualification required"
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let root = tempfile::Builder::new()
        .prefix("tilo-native-projection-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/run")
        .unwrap();
    let source_dir = root.path().join("source");
    std::fs::create_dir(&source_dir).unwrap();
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let source_path = source_dir.join(NAME);
    std::fs::write(&source_path, b"original source bytes").unwrap();
    // Neither source nor target owns this leaf. Only the original source
    // supplementary group can open it; target has no such group.
    chown(&source_path, FILE_OWNER, SOURCE_GROUP);
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o060)).unwrap();
    let before = std::fs::metadata(&source_path).unwrap();
    let state_dir = root.path().join("state");
    std::fs::create_dir(&state_dir).unwrap();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    chown(&state_dir, TARGET_UID, TARGET_GID);
    let state_path = state_dir.join("personal-state");
    std::fs::write(&state_path, b"ordinary target state").unwrap();
    chown(&state_path, TARGET_UID, TARGET_GID);
    let live = Arc::new(AtomicBool::new(true));
    let authority = Arc::new(FixtureAuthority(Arc::clone(&live)));
    let (mut source, endpoint) = source_worker(
        root.path(),
        File::options()
            .read(true)
            .write(true)
            .open(&source_path)
            .unwrap(),
        Arc::clone(&authority),
    );
    let deadline = tilo_test_env::hang_deadline();
    let source_stream = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => panic!("actual source endpoint: {e}"),
        }
    };
    assert_kernel_credentials(&source_stream, SOURCE_UID, SOURCE_GID, &[SOURCE_GROUP]);
    let client = SelectedFileConnection::connected(source_stream).unwrap();
    assert_eq!(client.metadata().unwrap().kind, EntryKind::File);
    let (mut target, projection) = target_worker(root.path(), authority);
    let target_stream = projection.connect_control().unwrap();
    assert_kernel_credentials(&target_stream, TARGET_UID, TARGET_GID, &[]);
    let bridge = Arc::new(Bridge {
        source: endpoint,
        target: Arc::clone(&projection),
        client: Mutex::new(Some(client)),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = workspace_server::serve_workspace_connected(
        target_stream,
        Arc::new(Backend(Arc::clone(&bridge))),
        runtime.handle().clone(),
    )
    .unwrap();
    projection.observe_ready().unwrap();
    let view = projection.held_view().unwrap();
    let (mut child, mut signal) = consumer(
        &view,
        &File::open(&state_dir).unwrap(),
        &File::open(&source_dir).unwrap(),
    );
    let mut response = [0];
    signal.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"W");
    let after = std::fs::metadata(&source_path).unwrap();
    assert_eq!(
        (
            after.dev(),
            after.ino(),
            after.uid(),
            after.gid(),
            after.mode()
        ),
        (
            before.dev(),
            before.ino(),
            before.uid(),
            before.gid(),
            before.mode()
        )
    );
    assert_eq!(
        after.len(),
        7,
        "canonical truncating write changed exact inode"
    );
    assert_eq!(std::fs::read(&source_path).unwrap(), b"updated");
    live.store(false, Ordering::Release);
    signal.write_all(b"R").unwrap();
    signal.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"E");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "owned target consumer must exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read(&state_path).unwrap(),
        b"private state survives source revoke"
    );
    drop(view);
    target.retire().unwrap();
    assert!(
        projection.check().is_err(),
        "dead namespace never grants a held export"
    );
    assert!(projection.held_view().is_err());
    server.retire().unwrap();
    drop(runtime);
    let client = bridge.client.lock().unwrap().take().unwrap();
    client.retire().unwrap();
    // Release the endpoint's held mounted view and all root transport custody
    // before retiring the original source service namespace.
    drop(bridge);
    drop(projection);
    source.retire().unwrap();
}
