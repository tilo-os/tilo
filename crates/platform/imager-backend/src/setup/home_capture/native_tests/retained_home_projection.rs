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
    directory_connection::{DirectoryConnection, ROOT_DIRECTORY},
    workspace_server::{self, WorkspaceBackend, WorkspaceFileBackend},
};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStringExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
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
const CHILD: &str =
    "setup::home_capture::native_tests::retained_home_projection::target_stdio_child";
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
struct FixtureAuthority(Arc<AtomicBool>, File);
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
    fn native_owner_sha256(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        Ok([23; 32])
    }
    fn removal_namespace(&self) -> io::Result<File> {
        self.check()?;
        self.1.try_clone()
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
        owned_service::TargetProjectionMode::HomeDirectory
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

fn bounded_fixture_error(error: &launcher::run::RunError) -> (String, bool) {
    struct Text {
        value: String,
        truncated: bool,
    }
    impl std::fmt::Write for Text {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let mut remaining = 2048 - self.value.len();
            if value.len() > remaining {
                while !value.is_char_boundary(remaining) {
                    remaining -= 1;
                }
                self.value.push_str(&value[..remaining]);
                self.truncated = true;
                return Err(std::fmt::Error);
            }
            self.value.push_str(value);
            Ok(())
        }
    }
    let mut text = Text {
        value: String::with_capacity(2048),
        truncated: false,
    };
    let _ = std::fmt::write(&mut text, format_args!("{error}"));
    (text.value, text.truncated)
}
fn observe_canonical_worker_debug(root: &Path) {
    // Fixture-only: generated source bytes, IdentitySource None and public SDK.
    // Read the existing canonical bounded managed-child log, never add a
    // worker channel or alter confinement. Capture may still finish asynchronously.
    let path = root.join("launcher-debug-output.log");
    let result: io::Result<serde_json::Value> = (|| {
        let mut file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.uid() != rustix::process::geteuid().as_raw()
            || before.nlink() != 1
            || before.mode() & 0o077 != 0
            || before.len() > 1024 * 1024
        {
            return Err(denied());
        }
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(before.len().saturating_sub(16384)))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(16385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let after = std::fs::symlink_metadata(&path)?;
        let held_after = file.metadata()?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino())
            || (before.dev(), before.ino()) != (held_after.dev(), held_after.ino())
        {
            return Err(denied());
        }
        Ok(serde_json::json!({"schema":1,"bytes":before.len(),
            "tail_truncated":before.len()>16384,
            "capture_changed_during_read":(before.len(),before.mtime(),before.mtime_nsec(),before.ctime(),before.ctime_nsec()) != (after.len(),after.mtime(),after.mtime_nsec(),after.ctime(),after.ctime_nsec()),
            "text":String::from_utf8_lossy(&bytes)}))
    })();
    match result {
        Ok(value) => eprintln!("RETAINED_CANONICAL_WORKER_DEBUG {value}"),
        Err(error) => eprintln!(
            "RETAINED_CANONICAL_WORKER_DEBUG {}",
            serde_json::json!({"schema":1,"read_error_kind":format!("{:?}",error.kind()),"errno":error.raw_os_error()})
        ),
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
        let cancelled_before = self.cancel.is_cancelled();
        self.cancel.cancel();
        let Some(job) = self.job.take() else {
            return Ok(());
        };
        let finished_before_join = job.is_finished();
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
        let (result_kind, phase, io_kind, errno) = match &run.result {
            Ok(_) => ("quiescent", "none", None, None),
            Err(launcher::run::RunError::Io(error)) => (
                "io",
                "none",
                Some(format!("{:?}", error.kind())),
                error.raw_os_error(),
            ),
            Err(launcher::run::RunError::Spawn(error)) => (
                "spawn",
                "none",
                Some(format!("{:?}", error.kind())),
                error.raw_os_error(),
            ),
            Err(launcher::run::RunError::InvalidServiceLaunch(message)) => (
                "invalid-service-launch",
                if message == "provider exit was not a clean checkpoint" {
                    "provider-not-clean-checkpoint"
                } else {
                    "other-fixed-refusal"
                },
                None,
                None,
            ),
            Err(launcher::run::RunError::Manifest(_)) => ("manifest", "none", None, None),
            Err(launcher::run::RunError::Seccomp(_)) => ("seccomp", "none", None, None),
            Err(launcher::run::RunError::RuntimeBundleSignature { .. }) => {
                ("runtime-signature", "none", None, None)
            }
            Err(launcher::run::RunError::RuntimeBundleMissing { .. }) => {
                ("runtime-missing", "none", None, None)
            }
            Err(launcher::run::RunError::RuntimeLibraryMissing { .. }) => {
                ("runtime-library-missing", "none", None, None)
            }
            Err(_) => ("other-launch-refusal", "none", None, None),
        };
        // This test uses only disposable private source bytes, IdentitySource
        // None and public signed artifacts. Preserve its actual launch cause,
        // capped during formatting, in JSON rather than a UI/runtime logger.
        let diagnostic = run.result.as_ref().err().map(bounded_fixture_error);
        eprintln!(
            "RETAINED_SUPERVISOR_RESULT {}",
            serde_json::json!({
                "schema": 1, "result": result_kind, "phase": phase,
                "io_kind": io_kind, "errno": errno,
            "error_text": diagnostic.as_ref().map(|(text, _)| text),
            "error_text_truncated": diagnostic.as_ref().is_some_and(|(_, truncated)| *truncated),
                "cancelled_before_retire": cancelled_before,
                "finished_before_join": finished_before_join,
                "shutdown_present": run.shutdown.is_some(),
                "exact_shutdown_or_quiescent": matches,
            })
        );
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

// Numeric test-only observation of the same held descriptor and current kernel
// table used by production coordinate admission. No source names or bytes leave
// this disposable fixture, and the existing admission assertion stays intact.
fn observe_held_coordinate(role: &str, file: &File) {
    assert!(matches!(role, "source" | "runtime"));
    let metadata = file.metadata().unwrap();
    let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live owned descriptor and writable uninitialized output storage.
    assert_eq!(
        unsafe { libc::fstat(file.as_raw_fd(), raw.as_mut_ptr()) },
        0
    );
    // SAFETY: successful fstat initialized the output structure.
    let raw = unsafe { raw.assume_init() };
    let mut extended = std::mem::MaybeUninit::<libc::statx>::uninit();
    // SAFETY: live descriptor, empty C path with AT_EMPTY_PATH, output storage.
    assert_eq!(
        unsafe {
            libc::statx(
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_INO | libc::STATX_MNT_ID,
                extended.as_mut_ptr(),
            )
        },
        0
    );
    // SAFETY: successful statx initialized the output structure.
    let extended = unsafe { extended.assume_init() };
    let mut table = Vec::new();
    File::open("/proc/self/mountinfo")
        .unwrap()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut table)
        .unwrap();
    assert!(table.len() <= 4 * 1024 * 1024);
    let mut selected = Vec::new();
    for line in table.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split(|b| *b == b' ').collect();
        assert!(fields.len() >= 10);
        let id: u64 = std::str::from_utf8(fields[0]).unwrap().parse().unwrap();
        if id != extended.stx_mnt_id {
            continue;
        }
        let parent: u64 = std::str::from_utf8(fields[1]).unwrap().parse().unwrap();
        let device: Vec<_> = fields[2].split(|b| *b == b':').collect();
        assert_eq!(device.len(), 2);
        let major: u32 = std::str::from_utf8(device[0]).unwrap().parse().unwrap();
        let minor: u32 = std::str::from_utf8(device[1]).unwrap().parse().unwrap();
        selected.push((id, parent, major, minor));
    }
    eprintln!(
        "RETAINED_HELD_COORDINATE {}",
        serde_json::json!({
            "schema": 1, "role": role, "metadata_dev": metadata.dev(), "metadata_inode": metadata.ino(),
            "libc_dev": raw.st_dev, "libc_inode": raw.st_ino,
            "metadata_major": libc::major(metadata.dev()), "metadata_minor": libc::minor(metadata.dev()),
            "libc_major": libc::major(raw.st_dev), "libc_minor": libc::minor(raw.st_dev),
            "statx_mask": extended.stx_mask, "statx_mount_id": extended.stx_mnt_id,
            "statx_inode": extended.stx_ino, "statx_major": extended.stx_dev_major,
            "statx_minor": extended.stx_dev_minor, "selected_rows": selected,
            "mountinfo_bytes": table.len()
        })
    );
}

fn source_worker(
    root: &Path,
    file: File,
    authority: Arc<FixtureAuthority>,
) -> (Worker, ServiceEndpoint) {
    let mut worker = Worker::new(root);
    let service = TrustedCaptureService::native()
        .expect("actual pinned signed source/target worker required");
    observe_held_coordinate("source", &file);
    {
        // Match the real exclusion descriptor: runtime is metadata custody,
        // never an extra source grant or readable worker projection.
        let runtime = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&service.root)
            .unwrap();
        observe_held_coordinate("runtime", &runtime);
    }
    let admission = RetainedIoAdmission::admit(file, authority)
        .unwrap()
        .with_native_runtime(&service.root, &service.publisher)
        .unwrap()
        .with_installation(service.installed.clone(), &service.service_publisher)
        .unwrap();
    let launch = launch(
        service,
        &worker,
        owned_service::RETAINED_HOME_IO_WORKER_ARGUMENT,
    );
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
    client: Mutex<Option<DirectoryConnection>>,
}
impl Bridge {
    fn call<T>(
        &self,
        call: impl FnOnce(&DirectoryConnection) -> io::Result<T>,
    ) -> Result<T, String> {
        let result: io::Result<T> = (|| {
            let client = self.client.lock().map_err(|_| denied())?;
            self.source.custody()?;
            self.target.check()?;
            let value = call(client.as_ref().ok_or_else(denied)?);
            self.source.custody()?;
            self.target.check()?;
            value
        })();
        result.map_err(|e| e.to_string())
    }
}
struct Directory {
    bridge: Arc<Bridge>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspaceDirectoryBackend for Directory {
    fn open_cursor(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceDirectoryCursorBackend>, String>
    {
        let id = self.bridge.call(|c| c.open_cursor(self.id))?;
        Ok(Arc::new(Cursor {
            bridge: self.bridge.clone(),
            id,
        }))
    }
    fn check(&self) -> Result<(), String> {
        self.bridge.call(|c| c.metadata(self.id).map(|_| ()))
    }
    fn statfs(&self) -> Result<runtime_fs::directory_backend::DirectoryFilesystemStats, String> {
        self.bridge.call(|c| c.statfs(self.id))
    }
    fn metadata(&self) -> Result<runtime_fs::directory_backend::DirectoryStat, String> {
        self.bridge.call(|c| c.metadata(self.id))
    }
    fn stat(
        &self,
        name: &runtime_fs::EntryName,
    ) -> Result<Option<runtime_fs::directory_backend::DirectoryStat>, String> {
        self.bridge.call(|c| c.stat(self.id, name))
    }
    fn readlink(
        &self,
        name: &runtime_fs::EntryName,
        expected: [u64; 2],
    ) -> Result<Vec<u8>, String> {
        self.bridge.call(|c| c.readlink(self.id, name, expected))
    }
    fn pin_metadata(
        &self,
        name: &runtime_fs::EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceMetadataBackend>, String> {
        let id = self
            .bridge
            .call(|c| c.pin_metadata(self.id, name, expected))?;
        Ok(Arc::new(Pin {
            bridge: self.bridge.clone(),
            id,
        }))
    }
    fn list(&self, cursor: u64) -> Result<runtime_fs::directory_backend::DirectoryRows, String> {
        self.bridge.call(|c| c.list(self.id, cursor))
    }
    fn directory(
        &self,
        name: &runtime_fs::EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceDirectoryBackend>, String> {
        let id = self.bridge.call(|c| c.directory(self.id, name))?;
        Ok(Arc::new(Self {
            bridge: self.bridge.clone(),
            id,
        }))
    }
    fn open_file(
        &self,
        name: &runtime_fs::EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        let id = self
            .bridge
            .call(|c| c.open_expected(self.id, name, mode, expected))?;
        Ok(Arc::new(Handle {
            bridge: self.bridge.clone(),
            id,
            identity: expected,
        }))
    }
    fn create_file(
        &self,
        name: runtime_fs::EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceJobBackend>, String> {
        let id = self.bridge.call(|c| c.create_file(self.id, &name))?;
        Ok(Arc::new(Job {
            bridge: self.bridge.clone(),
            id,
        }))
    }
    fn create_directory(
        &self,
        name: runtime_fs::EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceJobBackend>, String> {
        let id = self.bridge.call(|c| c.create_directory(self.id, &name))?;
        Ok(Arc::new(Job {
            bridge: self.bridge.clone(),
            id,
        }))
    }
    fn rename_no_replace(
        &self,
        name: &runtime_fs::EntryName,
        new: runtime_fs::EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceJobBackend>, String> {
        let id = self
            .bridge
            .call(|c| c.rename_no_replace(self.id, name, &new))?;
        Ok(Arc::new(Job {
            bridge: self.bridge.clone(),
            id,
        }))
    }
}
struct Cursor {
    bridge: Arc<Bridge>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspaceDirectoryCursorBackend for Cursor {
    fn check(&self) -> Result<(), String> {
        self.bridge.call(|c| c.metadata(ROOT_DIRECTORY).map(|_| ()))
    }
    fn page(
        &self,
        sequence: u64,
    ) -> Result<runtime_fs::directory_backend::DirectoryStreamPage, String> {
        self.bridge.call(|c| c.cursor_page(self.id, sequence))
    }
}
impl Drop for Cursor {
    fn drop(&mut self) {
        let _ = self.bridge.call(|c| c.release_cursor(self.id));
    }
}
struct Pin {
    bridge: Arc<Bridge>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspaceMetadataBackend for Pin {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.bridge.call(|c| c.check_pin(self.id)))
    }
    fn metadata(&self) -> Result<runtime_fs::directory_backend::DirectoryStat, String> {
        self.bridge.call(|c| c.metadata_pin(self.id))
    }
}
impl Drop for Pin {
    fn drop(&mut self) {
        let _ = self.bridge.call(|c| c.release_pin(self.id));
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.id != ROOT_DIRECTORY {
            let _ = self.bridge.call(|c| c.release_directory(self.id));
        }
    }
}
struct Job {
    bridge: Arc<Bridge>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspaceJobBackend for Job {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.bridge.call(|c| c.check_job(self.id)))
    }
    fn poll(&self) -> Result<runtime_fs::directory_backend::JobPoll, String> {
        self.bridge.call(|c| c.poll(self.id))
    }
    fn cancel(&self) -> Result<(), String> {
        self.bridge.call(|c| c.cancel(self.id))
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.bridge.call(|c| c.release_job(self.id));
    }
}
struct Handle {
    bridge: Arc<Bridge>,
    id: u64,
    identity: [u64; 2],
}
impl WorkspaceFileBackend for Handle {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.bridge.call(|c| c.check_scope(self.id)))
    }
    fn native_identity(&self) -> Result<Option<[u64; 2]>, String> {
        self.bridge
            .call(|c| c.metadata_handle(self.id).map(|_| Some(self.identity)))
    }
    fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        self.bridge.call(|c| c.read(self.id, offset, len))
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        self.bridge.call(|c| c.write(self.id, offset, bytes))
    }
    fn append(&self, bytes: &[u8]) -> Result<u32, String> {
        self.bridge.call(|c| c.append(self.id, bytes))
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
    fn directory(
        &self,
    ) -> Result<Option<Arc<dyn runtime_fs::directory_backend::WorkspaceDirectoryBackend>>, String>
    {
        self.0.call(|c| c.metadata(ROOT_DIRECTORY))?;
        Ok(Some(Arc::new(Directory {
            bridge: self.0.clone(),
            id: ROOT_DIRECTORY,
        })))
    }
    fn list(&self, _: &str) -> Result<Vec<DirEntry>, String> {
        Err("raw capability required".into())
    }
    fn stat(&self, _: &str) -> Result<Metadata, String> {
        Err("raw capability required".into())
    }
    fn read(&self, _: &str, _: u64, _: u32) -> Result<Vec<u8>, String> {
        Err("raw capability required".into())
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
    // TempDir's creation mode is still restricted by the private 077 umask.
    // Make only this root-owned executable probe searchable after creation;
    // source, state, and control workspaces retain their private permissions.
    let probe_directory = File::open(probe.path()).unwrap();
    probe_directory
        .set_permissions(std::fs::Permissions::from_mode(0o711))
        .unwrap();
    let probe_facts = probe_directory.metadata().unwrap();
    let named_probe = std::fs::symlink_metadata(probe.path()).unwrap();
    assert_eq!(probe_facts.uid(), 0);
    assert_eq!(probe_facts.mode() & 0o7777, 0o711);
    assert_eq!(
        (probe_facts.dev(), probe_facts.ino()),
        (named_probe.dev(), named_probe.ino())
    );
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
        .env("TILO_HOME_TARGET_STDIO_FIXTURE", "1");
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
    if std::env::var_os("TILO_HOME_TARGET_STDIO_FIXTURE").is_none() {
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
    // Kernel FUSE statfs must traverse the actual source worker and guarded
    // relay; compare stable capacity/units, not concurrently changing free space.
    let observe_capacity = |descriptor| {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: fixture-owned live descriptors; successful fstatvfs fills stats.
        assert_eq!(unsafe { libc::fstatvfs(descriptor, stats.as_mut_ptr()) }, 0);
        unsafe { stats.assume_init() }
    };
    let projected_stats = observe_capacity(70);
    let actual_source_stats = observe_capacity(72);
    assert!(
        projected_stats.f_blocks > 0,
        "target Home cannot report default zero/disk-full capacity"
    );
    assert_eq!(projected_stats.f_blocks, actual_source_stats.f_blocks);
    assert_eq!(projected_stats.f_bsize, actual_source_stats.f_bsize);
    assert_eq!(projected_stats.f_frsize, actual_source_stats.f_frsize);
    assert_eq!(projected_stats.f_files, actual_source_stats.f_files);
    assert_eq!(projected_stats.f_namemax, actual_source_stats.f_namemax);
    assert!(projected_stats.f_bfree <= projected_stats.f_blocks);
    assert!(projected_stats.f_bavail <= projected_stats.f_bfree);

    assert!(
        std::fs::read(&bare_source).is_err(),
        "source requires original supplementary group"
    );
    assert_eq!(std::fs::read(&view).unwrap(), b"original source bytes");
    let held = File::options().read(true).write(true).open(&view).unwrap();
    use std::os::unix::fs::FileExt;
    assert_eq!(held.write_at(b"ORIGINAL", 0).unwrap(), 8);
    assert_eq!(std::fs::read(&view).unwrap(), b"ORIGINAL source bytes");
    std::fs::write(&view, b"updated").unwrap();
    let mut append = File::options().append(true).open(&view).unwrap();
    append.write_all(b"-append").unwrap();
    append.sync_all().unwrap();
    assert_eq!(std::fs::read(&view).unwrap(), b"updated-append");
    let directory = PathBuf::from("/proc/self/fd/70");
    assert_eq!(
        std::fs::read_link(directory.join("relative-file")).unwrap(),
        PathBuf::from(NAME)
    );
    assert_eq!(
        std::fs::read(directory.join("relative-file")).unwrap(),
        b"updated-append",
        "safe relative file link follows the same edited original inode"
    );
    let linked_file = std::fs::metadata(directory.join("relative-file")).unwrap();
    let original_file = std::fs::metadata(&view).unwrap();
    assert_eq!(
        (linked_file.dev(), linked_file.ino()),
        (original_file.dev(), original_file.ino()),
        "relative link names the same projected original inode"
    );
    assert_eq!(
        std::fs::read_link(directory.join("relative-directory")).unwrap(),
        PathBuf::from("source-nested")
    );
    assert_eq!(
        std::fs::read(directory.join("relative-directory/child")).unwrap(),
        b"original nested bytes"
    );
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(
        std::fs::read_link(directory.join("raw-link"))
            .unwrap()
            .as_os_str()
            .as_bytes(),
        b"raw-\xff-target",
        "target readlink preserves raw source bytes without opening its target"
    );
    assert!(
        std::fs::read(directory.join("unreadable")).is_err(),
        "actual original UID must still refuse root-private contents"
    );
    assert_eq!(
        std::fs::read(&view).unwrap(),
        b"updated-append",
        "one native file refusal does not retire Home"
    );
    let mut large = std::collections::HashSet::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if let Some(index) = name
            .to_str()
            .and_then(|name| name.strip_prefix("large-"))
            .and_then(|index| index.parse::<usize>().ok())
        {
            assert!(large.insert(index));
            assert_eq!(
                std::fs::read(entry.path()).unwrap_or_else(|error| {
                    panic!(
                        "large Home read failed after {} unique entries: index={index}, name={name:?}, error={error:?}",
                        large.len()
                    )
                }),
                format!("large-contents-{index:04}").as_bytes()
            );
        }
    }
    assert_eq!(
        large.len(),
        4608,
        "real target stdfs streams and reads every original beyond the resident node bound"
    );
    assert!(
        std::fs::symlink_metadata(directory.join("native-socket"))
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert!(
        std::fs::symlink_metadata(directory.join("native-fifo"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert!(File::open(directory.join("native-socket")).is_err());
    assert!(
        File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(directory.join("native-fifo"))
            .is_err()
    );
    let raw = std::ffi::OsString::from_vec(vec![b'n', 0xff]);
    let created = directory.join(&raw);
    let mut fresh = File::options()
        .write(true)
        .create_new(true)
        .open(&created)
        .unwrap();
    fresh
        .write_all(b"created through canonical durable job")
        .unwrap();
    fresh.sync_all().unwrap();
    assert!(
        File::options()
            .write(true)
            .create_new(true)
            .open(&created)
            .is_err()
    );
    std::fs::create_dir(directory.join("nested")).unwrap();
    std::fs::write(directory.join("nested/child"), b"nested bytes").unwrap();
    assert_eq!(
        std::fs::read(directory.join("nested/child")).unwrap(),
        b"nested bytes"
    );
    let renamed = directory.join("renamed");
    let old = std::ffi::CString::new(created.as_os_str().as_bytes()).unwrap();
    let new = std::ffi::CString::new(renamed.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                old.as_ptr(),
                libc::AT_FDCWD,
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        },
        0
    );
    assert_eq!(
        std::fs::read(&renamed).unwrap(),
        b"created through canonical durable job"
    );
    let source = std::ffi::CString::new(view.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        },
        -1
    );
    assert_eq!(std::fs::read(&view).unwrap(), b"updated-append");
    assert!(
        std::fs::read_dir(&directory)
            .unwrap()
            .map(|row| row.unwrap().file_name())
            .any(|n| n == std::ffi::OsStr::new("renamed"))
    );
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
fn signed_home_projection_preserves_directory_source_identity_and_normal_target_credentials() {
    full_home_fixture(false, true);
}
#[test]
fn signed_home_projection_native_alias_preserves_raw_links_same_file_edits_and_noexec() {
    full_home_fixture(true, true);
}
#[test]
fn signed_home_projection_native_readonly_alias_denies_execution_and_mutation() {
    full_home_fixture(true, false);
}
fn full_home_fixture(alias_mode: bool, writable: bool) {
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
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    chown(&source_dir, SOURCE_UID, SOURCE_GID);
    let source_path = source_dir.join(NAME);
    std::fs::write(&source_path, b"original source bytes").unwrap();
    // Neither source nor target owns this leaf. Only the original source
    // supplementary group can open it; target has no such group.
    chown(&source_path, FILE_OWNER, SOURCE_GROUP);
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o060)).unwrap();
    let before = std::fs::metadata(&source_path).unwrap();
    let unreadable = source_dir.join("unreadable");
    std::fs::write(&unreadable, b"root private bytes").unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _source_socket = UnixListener::bind(source_dir.join("native-socket")).unwrap();
    let fifo_name = std::ffi::CString::new(
        source_dir
            .join("native-fifo")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    for index in 0..4608 {
        let path = source_dir.join(format!("large-{index:04}"));
        std::fs::write(&path, format!("large-contents-{index:04}")).unwrap();
        chown(&path, SOURCE_UID, SOURCE_GID);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source_nested = source_dir.join("source-nested");
    std::fs::create_dir(&source_nested).unwrap();
    chown(&source_nested, SOURCE_UID, SOURCE_GID);
    std::fs::set_permissions(&source_nested, std::fs::Permissions::from_mode(0o700)).unwrap();
    let nested_child = source_nested.join("child");
    std::fs::write(&nested_child, b"original nested bytes").unwrap();
    chown(&nested_child, SOURCE_UID, SOURCE_GID);
    std::fs::set_permissions(&nested_child, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(NAME, source_dir.join("relative-file")).unwrap();
    std::os::unix::fs::symlink("source-nested", source_dir.join("relative-directory")).unwrap();
    std::os::unix::fs::symlink(
        OsString::from_vec(b"raw-\xff-target".to_vec()),
        source_dir.join("raw-link"),
    )
    .unwrap();
    if alias_mode {
        // This fixture's original namespace spelling is deliberately different
        // from its root-private backing-store path and the target private HOME.
        // This tests namespace presentation, never installed source admission.
        std::os::unix::fs::symlink(alias_path().join(NAME), source_dir.join("absolute-file"))
            .unwrap();
        std::os::unix::fs::symlink("/outside-home/secret", source_dir.join("outside-file"))
            .unwrap();
        let executable = source_dir.join("executable");
        std::fs::copy("/usr/bin/true", &executable).unwrap();
        chown(&executable, SOURCE_UID, SOURCE_GID);
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Bwrap resolves the held state descriptor to its named path after the
    // target UID drop. Keep target-owned state under a searchable parent, as
    // ordinary app state is, independently of the root-private source fixture.
    let target_state = tempfile::Builder::new()
        .prefix("tilo-native-target-state-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/run")
        .unwrap();
    let state_dir = target_state.path().to_path_buf();
    chown(&state_dir, TARGET_UID, TARGET_GID);
    let state_path = state_dir.join("personal-state");
    std::fs::write(&state_path, b"ordinary target state").unwrap();
    chown(&state_path, TARGET_UID, TARGET_GID);
    let live = Arc::new(AtomicBool::new(true));
    let records = root.path().join("protected-records");
    std::fs::create_dir(&records).unwrap();
    std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
    let authority = Arc::new(FixtureAuthority(
        Arc::clone(&live),
        File::open(records).unwrap(),
    ));
    let (mut source, endpoint) = source_worker(
        root.path(),
        File::open(&source_dir).unwrap(),
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
            Err(e) => {
                if source.retire().is_err() {
                    eprintln!("RETAINED_SOURCE_ENDPOINT_RETIREMENT_UNCONFIRMED");
                }
                observe_canonical_worker_debug(source.working.path());
                break Err::<_, io::Error>(e).expect("actual source endpoint");
            }
        }
    };
    assert_kernel_credentials(&source_stream, SOURCE_UID, SOURCE_GID, &[SOURCE_GROUP]);
    let client = DirectoryConnection::connected(source_stream).unwrap();
    assert_eq!(
        client.metadata(ROOT_DIRECTORY).unwrap().metadata.kind,
        EntryKind::Directory
    );
    let held_source = File::open(&source_dir).unwrap();
    let mut source_capacity = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: successful fstatvfs fills the fixture's held original directory stats.
    assert_eq!(
        unsafe { libc::fstatvfs(held_source.as_raw_fd(), source_capacity.as_mut_ptr()) },
        0
    );
    let source_capacity = unsafe { source_capacity.assume_init() };
    let rpc_capacity = client.statfs(ROOT_DIRECTORY).unwrap();
    rpc_capacity.validate().unwrap();
    assert!(rpc_capacity.blocks > 0);
    assert_eq!(rpc_capacity.blocks, source_capacity.f_blocks);
    assert_eq!(u64::from(rpc_capacity.block_size), source_capacity.f_bsize);
    assert_eq!(
        u64::from(rpc_capacity.fragment_size),
        source_capacity.f_frsize
    );
    assert_eq!(u64::from(rpc_capacity.name_max), source_capacity.f_namemax);
    for (name, target) in [
        ("relative-file", NAME.as_bytes()),
        ("relative-directory", b"source-nested".as_slice()),
        ("raw-link", b"raw-\xff-target".as_slice()),
    ] {
        let name = runtime_fs::EntryName::new(name.into()).unwrap();
        let link = client.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
        assert_eq!(
            client
                .readlink(ROOT_DIRECTORY, &name, link.identity)
                .unwrap(),
            target
        );
    }
    let cursor = client.open_cursor(ROOT_DIRECTORY).unwrap();
    let mut sequence = 0;
    let mut unreadable_observed = false;
    let mut count = 0;
    loop {
        let page = client.cursor_page(cursor, sequence).unwrap();
        assert!(page.entries.len() <= 128);
        for row in page.entries {
            assert!(
                row.stat.is_some(),
                "actual source UID can observe owned raw rows"
            );
            count += 1;
            unreadable_observed |= row.name.as_os_str() == std::ffi::OsStr::new("unreadable");
        }
        if page.done {
            break;
        }
        sequence = page.next_sequence;
    }
    client.release_cursor(cursor).unwrap();
    assert_eq!(
        count,
        if alias_mode { 4619 } else { 4616 },
        "all actual generated files, existing rows, nested directory and links are observed"
    );
    assert!(
        unreadable_observed,
        "source UID enumerates unreadable metadata without bytes"
    );
    // Open the actual original-UID iterator while search is permitted, then
    // remove search access to its parent. readDir on the held OFD still works;
    // nofollow child stat must be denied to this non-root source worker.
    let denied_cursor = client.open_cursor(ROOT_DIRECTORY).unwrap();
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o400)).unwrap();
    let denied_page = client.cursor_page(denied_cursor, 0);
    // Restore actual source policy before asserting, including a failed reply.
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let denied_page = denied_page.unwrap();
    assert!(!denied_page.entries.is_empty());
    assert!(
        denied_page.entries.iter().all(|row| row.stat.is_none()),
        "actual source-UID stat denial remains a visible raw-name row"
    );
    for row in denied_page.entries {
        assert!(
            std::fs::symlink_metadata(source_dir.join(row.name.as_os_str())).is_ok(),
            "denied names are actual existing entries, not vanished rows"
        );
    }
    client.release_cursor(denied_cursor).unwrap();
    assert!(client.metadata(ROOT_DIRECTORY).is_ok());
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
    if alias_mode {
        // Genuine signed source/target and FixtureAuthority boundary only.
        // This grants no installed Shell/ordinary GUI success receipt.
        let mut exposure = launcher::owned_service::target_projection::FixtureExposure::prepare(
            Arc::clone(&projection),
            writable,
        )
        .unwrap();
        exposure.verify_foreign_attached_clone_refused().unwrap();
        exposure.place().unwrap();
        assert_ne!(
            exposure.mount_id().unwrap(),
            exposure.original_mount_id().unwrap()
        );
        qualify_alias_consumer(
            &mut exposure,
            &state_dir,
            &source_dir,
            &source_path,
            &before,
            &live,
            writable,
        );
        drop(view);
        target.retire().unwrap();
        assert!(projection.check().is_err());
        assert!(projection.held_view().is_err());
        server.retire().unwrap();
        drop(runtime);
        let client = bridge.client.lock().unwrap().take().unwrap();
        client.retire().unwrap();
        drop(bridge);
        drop(projection);
        source.retire().unwrap();
        return;
    }
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
        14,
        "canonical RW/truncating/append IO changed exact inode"
    );
    assert_eq!(std::fs::read(&source_path).unwrap(), b"updated-append");
    let created = std::fs::metadata(source_dir.join("renamed")).unwrap();
    assert_eq!((created.uid(), created.gid()), (SOURCE_UID, SOURCE_GID));
    assert_eq!(
        std::fs::read(source_dir.join("nested/child")).unwrap(),
        b"nested bytes"
    );
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

const ALIAS_CHILD: &str =
    "setup::home_capture::native_tests::retained_home_projection::native_alias_stdio_child";
fn alias_path() -> PathBuf {
    PathBuf::from(OsString::from_vec(b"/home/original-\xff".to_vec()))
}

/// Narrow confinement qualification over the existing actual signed workers
/// and their real root-mounted FUSE view. Fixture authority is NOT installed
/// person/record/Shell authority and never bypasses those production checks.
fn qualify_alias_consumer(
    exposure: &mut launcher::owned_service::target_projection::FixtureExposure,
    state_dir: &Path,
    source_dir: &Path,
    source_path: &Path,
    before: &std::fs::Metadata,
    live: &Arc<AtomicBool>,
    writable: bool,
) {
    for repeated in [false, true] {
        qualify_alias_iteration(
            exposure,
            state_dir,
            source_dir,
            source_path,
            before,
            live,
            AliasIteration {
                writable,
                repeated,
                final_revoke: repeated,
            },
        );
        if !repeated {
            exposure.begin_next_consumer().unwrap();
        }
    }
}
struct AliasIteration {
    writable: bool,
    repeated: bool,
    final_revoke: bool,
}
fn qualify_alias_iteration(
    exposure: &mut launcher::owned_service::target_projection::FixtureExposure,
    state_dir: &Path,
    source_dir: &Path,
    source_path: &Path,
    before: &std::fs::Metadata,
    live: &Arc<AtomicBool>,
    iteration: AliasIteration,
) {
    let AliasIteration {
        writable,
        repeated,
        final_revoke,
    } = iteration;
    let probe = tempfile::Builder::new()
        .prefix("tilo-alias-probe-")
        .permissions(std::fs::Permissions::from_mode(0o755))
        .tempdir_in("/run")
        .unwrap();
    std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let executable = probe.path().join("actual-native-tests");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = File::open(state_dir).unwrap();
    let original = File::open(source_dir).unwrap();
    let alias =
        launcher::retained_view::OriginalHomeAlias::capture(&alias_path(), &original).unwrap();
    let mut argv = vec![
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--ro-bind".into(),
        "/usr".into(),
        "/usr".into(),
        "--ro-bind".into(),
        "/lib".into(),
        "/lib".into(),
        "--ro-bind".into(),
        "/lib64".into(),
        "/lib64".into(),
        "--ro-bind".into(),
        executable.to_str().unwrap().into(),
        "/fixture/actual-native-tests".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        if writable {
            "--bind-fd"
        } else {
            "--ro-bind-fd"
        }
        .into(),
        "70".into(),
        launcher::retained_view::SANDBOX_HOME.into(),
        "--bind-fd".into(),
        "71".into(),
        "/run/tilo/app".into(),
        "--clearenv".into(),
        "--setenv".into(),
        "TILO_NATIVE_HOME_ALIAS_FIXTURE".into(),
        "1".into(),
    ];
    argv.extend([
        "--setenv".into(),
        "TILO_NATIVE_HOME_ALIAS_REPEATED".into(),
        if repeated { "1" } else { "0" }.into(),
    ]);
    argv.extend([
        "--setenv".into(),
        "TILO_NATIVE_HOME_ALIAS_WRITABLE".into(),
        if writable { "1" } else { "0" }.into(),
    ]);
    for (key, value) in launcher::appdir::sandbox_env() {
        argv.extend(["--setenv".into(), key, value]);
    }
    // These are the exact production baseline/clone3 programs, not a weaker
    // test-specific filter. Bwrap consumes them only after mount setup.
    let mut baseline = tempfile::tempfile().unwrap();
    baseline
        .write_all(
            &launcher::seccomp::compile_baseline(launcher::seccomp::SeccompProfile::TiloDefault)
                .unwrap(),
        )
        .unwrap();
    let mut clone3 = tempfile::tempfile().unwrap();
    clone3
        .write_all(&launcher::seccomp::compile_clone3_compat().unwrap())
        .unwrap();
    use std::io::{Seek, SeekFrom};
    baseline.seek(SeekFrom::Start(0)).unwrap();
    clone3.seek(SeekFrom::Start(0)).unwrap();
    argv.extend([
        "--add-seccomp-fd".into(),
        "60".into(),
        "--add-seccomp-fd".into(),
        "61".into(),
    ]);
    let args = alias.qualification_arguments(&argv, writable).unwrap();
    argv.extend([
        "--args".into(),
        "59".into(),
        "--".into(),
        "/fixture/actual-native-tests".into(),
        "--exact".into(),
        ALIAS_CHILD.into(),
        "--nocapture".into(),
    ]);
    let (mut signal, child) = UnixStream::pair().unwrap();
    signal
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    signal
        .set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let (info, info_child) = UnixStream::pair().unwrap();
    info.set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    argv.splice(0..0, ["--info-fd".into(), "75".into()]);
    let view = exposure.file().unwrap();
    let pairs = [
        (info_child.as_raw_fd(), 75),
        (view.as_raw_fd(), 70),
        (state.as_raw_fd(), 71),
        (view.as_raw_fd(), 58),
        (args.as_raw_fd(), 59),
        (child.as_raw_fd(), 74),
        (baseline.as_raw_fd(), 60),
        (clone3.as_raw_fd(), 61),
    ];
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .args(&argv)
        .env_clear()
        .stdin(std::process::Stdio::null());
    unsafe {
        command.pre_exec(move || {
            let mut held = Vec::new();
            for (fd, destination) in pairs {
                let copy = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 100);
                if copy < 0 {
                    return Err(io::Error::last_os_error());
                }
                held.push((File::from_raw_fd(copy), destination));
            }
            for (file, destination) in &held {
                if libc::dup2(file.as_raw_fd(), *destination) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setgroups(0, std::ptr::null()) < 0
                || libc::setgid(TARGET_GID) < 0
                || libc::setuid(TARGET_UID) < 0
            {
                return Err(io::Error::last_os_error());
            }
            if rustix::process::getuid().as_raw() != TARGET_UID
                || rustix::process::geteuid().as_raw() != TARGET_UID
                || rustix::process::getgid().as_raw() != TARGET_GID
                || rustix::process::getegid().as_raw() != TARGET_GID
                || libc::getgroups(0, std::ptr::null_mut()) != 0
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
    }
    // Token is owned before any possible inherited view delivery.
    exposure.mark_possible_delivery().unwrap();
    let mut consumer = Consumer {
        child: command.spawn().unwrap(),
        _probe: probe,
    };
    drop(info_child);
    let mut info_bytes = Vec::new();
    info.take(16385).read_to_end(&mut info_bytes).unwrap();
    assert!(info_bytes.len() <= 16384);
    exposure
        .adopt_owned_child(&consumer.child, &info_bytes)
        .unwrap();
    let mut response = [0];
    signal.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"W");
    let after = std::fs::metadata(source_path).unwrap();
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
        std::fs::read(source_path).unwrap(),
        if writable {
            b"alias edits original".as_slice()
        } else {
            b"original source bytes".as_slice()
        }
    );
    if final_revoke {
        live.store(false, Ordering::Release);
    }
    signal
        .write_all(if final_revoke { b"R" } else { b"N" })
        .unwrap();
    signal.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"E");
    if final_revoke {
        assert!(
            !exposure.target_observed_stopped().unwrap(),
            "revocation denies source bytes while actual exposure retains target custody"
        );
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(status) = consumer.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual owned alias consumer must stop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read(state_dir.join("alias-private-state")).unwrap(),
        if final_revoke {
            b"private after revoke".as_slice()
        } else {
            b"private before revoke".as_slice()
        }
    );
    assert!(
        exposure.observed_stopped().unwrap(),
        "actual namespace init pidfd stopped"
    );
    if final_revoke {
        exposure.retire().unwrap();
    }
}

#[test]
fn native_alias_stdio_child() {
    if std::env::var_os("TILO_NATIVE_HOME_ALIAS_FIXTURE").is_none() {
        return;
    }
    let writable = match std::env::var("TILO_NATIVE_HOME_ALIAS_WRITABLE")
        .unwrap()
        .as_str()
    {
        "1" => true,
        "0" => false,
        _ => panic!("unknown native fixture participation"),
    };
    let repeated = std::env::var("TILO_NATIVE_HOME_ALIAS_REPEATED").unwrap() == "1";
    let expected = if writable && repeated {
        b"alias edits original".as_slice()
    } else {
        b"original source bytes".as_slice()
    };
    let mut signal = unsafe { UnixStream::from_raw_fd(74) };
    // Canonical bwrap preserves the ordinary invoking UID/GID. Its --dev
    // devpts preparation uses an intermediate namespace UID/GID 0, then
    // maps the final ordinary IDs to that intermediate identity.
    assert_eq!(rustix::process::getuid().as_raw(), TARGET_UID);
    assert_eq!(rustix::process::geteuid().as_raw(), TARGET_UID);
    assert_eq!(rustix::process::getgid().as_raw(), TARGET_GID);
    assert_eq!(rustix::process::getegid().as_raw(), TARGET_GID);
    assert_eq!(unsafe { libc::getgroups(0, std::ptr::null_mut()) }, 0);
    let uid_map = std::fs::read_to_string("/proc/self/uid_map").unwrap();
    let mapping: Vec<u32> = uid_map
        .split_whitespace()
        .map(|part| part.parse().unwrap())
        .collect();
    assert_eq!(mapping, [TARGET_UID, 0, 1]);
    let gid_map = std::fs::read_to_string("/proc/self/gid_map").unwrap();
    let mapping: Vec<u32> = gid_map
        .split_whitespace()
        .map(|part| part.parse().unwrap())
        .collect();
    assert_eq!(mapping, [TARGET_GID, 0, 1]);
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    for key in ["CapEff:", "CapPrm:", "CapInh:", "CapAmb:"] {
        let matches: Vec<_> = status
            .lines()
            .filter_map(|line| line.strip_prefix(key))
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(u64::from_str_radix(matches[0].trim(), 16).unwrap(), 0);
    }
    for (key, expected) in [("Seccomp:", "2"), ("NoNewPrivs:", "1")] {
        let values: Vec<_> = status
            .lines()
            .filter_map(|line| line.strip_prefix(key))
            .collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].trim(), expected);
    }
    // Source and root receipts are never inherited into this test consumer.
    // Close all nonstandard descriptors except its owned qualification socket
    // before actual Landlock enforcement.
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_close_range, 3_u32, 73_u32, 0_u32) },
        0
    );
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_close_range, 75_u32, u32::MAX, 0_u32) },
        0
    );
    let args = launcher::landlock_shim::ShimArgs::parse(&[
        "--ro".into(),
        "/usr".into(),
        "--ro".into(),
        "/lib".into(),
        "--ro".into(),
        "/lib64".into(),
        "--ro".into(),
        "/fixture".into(),
        "--rw".into(),
        "/run/tilo/app".into(),
        if writable {
            "--rw-noexec"
        } else {
            "--ro-noexec"
        }
        .into(),
        launcher::retained_view::SANDBOX_HOME.into(),
        "--".into(),
        "/fixture/actual-native-tests".into(),
    ])
    .unwrap();
    assert_eq!(
        launcher::landlock_shim::apply(&args),
        launcher::landlock::LandlockOutcome::FullyEnforced
    );
    for (key, value) in launcher::appdir::sandbox_env() {
        assert_eq!(std::env::var(key).unwrap(), value);
    }
    let original = alias_path().join(NAME);
    let fixed = Path::new(launcher::retained_view::SANDBOX_HOME).join(NAME);
    let absolute_link = Path::new(launcher::retained_view::SANDBOX_HOME).join("absolute-file");
    let relative_link = alias_path().join("relative-file");
    for directory in [
        alias_path(),
        PathBuf::from(launcher::retained_view::SANDBOX_HOME),
    ] {
        assert!(
            std::fs::read_dir(directory)
                .unwrap()
                .any(|entry| entry.unwrap().file_name() == NAME),
            "readonly data policy retains actual directory enumeration through both mounts"
        );
    }
    assert_eq!(
        std::fs::read_link(&absolute_link).unwrap().as_os_str(),
        original.as_os_str()
    );
    assert_eq!(std::fs::read(&absolute_link).unwrap(), expected);
    assert_eq!(std::fs::read(&relative_link).unwrap(), expected);
    let original_meta = std::fs::metadata(&original).unwrap();
    let fixed_meta = std::fs::metadata(&fixed).unwrap();
    assert_eq!(
        (original_meta.dev(), original_meta.ino()),
        (fixed_meta.dev(), fixed_meta.ino())
    );
    if writable {
        std::fs::write(&absolute_link, b"alias edits original").unwrap();
        assert_eq!(
            std::fs::read(&relative_link).unwrap(),
            b"alias edits original"
        );
        assert_eq!(std::fs::read(&fixed).unwrap(), b"alias edits original");
    } else {
        assert!(std::fs::write(&absolute_link, b"reader must not mutate").is_err());
        assert!(std::fs::remove_file(&relative_link).is_err());
        assert_eq!(std::fs::read(&fixed).unwrap(), b"original source bytes");
    }
    assert!(
        Command::new("/usr/bin/true").status().unwrap().success(),
        "required runtime binaries remain executable"
    );
    assert!(std::fs::read(alias_path().join("outside-file")).is_err());
    for executable in [
        alias_path().join("executable"),
        Path::new(launcher::retained_view::SANDBOX_HOME).join("executable"),
    ] {
        assert_eq!(
            std::fs::metadata(&executable).unwrap().mode() & 0o111,
            0o111
        );
        let error = Command::new(executable).status().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }
    let private = Path::new("/run/tilo/app/alias-private-state");
    std::fs::write(private, b"private before revoke").unwrap();
    signal.write_all(b"W").unwrap();
    let mut request = [0];
    signal.read_exact(&mut request).unwrap();
    if request == *b"N" {
        assert_eq!(
            std::fs::read(&fixed).unwrap(),
            if writable {
                b"alias edits original".as_slice()
            } else {
                expected
            }
        );
        signal.write_all(b"E").unwrap();
        return;
    }
    assert_eq!(&request, b"R");
    assert!(std::fs::read(&absolute_link).is_err());
    assert!(std::fs::write(&relative_link, b"after revoke").is_err());
    std::fs::write(private, b"private after revoke").unwrap();
    signal.write_all(b"E").unwrap();
}
