use launcher::{
    owned_service::{
        CaptureAdmission, ServiceAuthority, ServiceCancellation, ServiceLaunch, ServiceLeaseKey,
    },
    run::{IdentitySource, LaunchMode, RunOptions},
};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const PACKAGE: &str = "so.tilo.imager.home-export";
struct Authority;
impl ServiceAuthority for Authority {
    fn check(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Model the broker-private snapshot ancestor after native admission. The
/// selected descriptor remains valid while the original user cannot resolve
/// the snapshot's ambient pathname. Restore even when qualification panics.
struct InaccessibleAncestor {
    path: PathBuf,
    permissions: std::fs::Permissions,
}
impl InaccessibleAncestor {
    fn deny(path: &Path) -> io::Result<Self> {
        let permissions = std::fs::metadata(path)?.permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o0))?;
        Ok(Self {
            path: path.into(),
            permissions,
        })
    }
}
impl Drop for InaccessibleAncestor {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
    }
}

fn signed_runtime(root: &Path) -> (PathBuf, String) {
    let launcher = root.join("tilo-launcher");
    std::fs::copy(tilo_test_env::package_binary!("tilo-launcher"), &launcher).unwrap();
    // Strip only the fixture copy's debug sections; executable code is the actual built launcher.
    assert!(
        std::process::Command::new("strip")
            .arg("--strip-debug")
            .arg(&launcher)
            .status()
            .unwrap()
            .success()
    );
    let bwrap = root.join("bwrap");
    std::fs::copy("/usr/bin/bwrap", &bwrap).unwrap();
    let mut libraries = Vec::new();
    for executable in [&launcher, &bwrap, &PathBuf::from("/usr/bin/tar")] {
        let output = std::process::Command::new("ldd")
            .arg(executable)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("not found"));
        for line in text.lines() {
            if let Some(path) = line.split_whitespace().find(|part| part.starts_with('/')) {
                libraries.push(PathBuf::from(path));
            }
        }
    }
    libraries.sort();
    libraries.dedup();
    let identity = runtime_bundle::Identity::generate();
    let key_path = root.join("runtime-key.json");
    std::fs::write(&key_path, identity.to_json_bytes()).unwrap();
    let runtime = root.join("signed-runtime");
    runtime_bundle::build_bundle_with(
        &runtime,
        "2026.10.1",
        &runtime_bundle::BuildOptions {
            identity: Some(&key_path),
            extra_bins: &[launcher, bwrap, PathBuf::from("/usr/bin/tar")],
            extra_libs: &libraries,
            ..Default::default()
        },
    )
    .unwrap();
    (runtime, identity.public_key_hex())
}

fn installed_worker(root: &Path, payload: &Path) -> package_store::InstalledPackage {
    let mut seed = [0; 32];
    File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut seed)
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let publisher = key.verifying_key().to_bytes();
    let publisher_hex = hex::encode(publisher);
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{PACKAGE}"
version = "1.0.0"
kind = "service"
runtime = "tilo-runtime/2026.10.1"
[package.isa]
x86_64 = "baseline"
aarch64 = "armv8-a"
[publisher]
identity = "ed25519:{publisher_hex}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/capture-worker"
type = "cli"
"#
    ))
    .unwrap();
    let packed = tilo_app_package::pack(
        payload,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(key),
    )
    .unwrap();
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            &packed.meta,
            &[publisher],
            &[],
            &tilo_app_package::RevocationList::default()
        ),
        tilo_app_package::Verification::Trusted { .. }
    ));
    let installed = root.join("installed-worker");
    tilo_app_package::unpack(&packed.meta, &packed, &installed).unwrap();
    let metadata = root.join("installed-metadata");
    std::fs::create_dir(&metadata).unwrap();
    for (name, bytes) in [
        ("signed-root", packed.meta.signed_root_bytes().to_vec()),
        ("manifest.toml", packed.meta.manifest_bytes.clone()),
        ("collection.qcl", packed.meta.collection_bytes.clone()),
    ] {
        std::fs::write(metadata.join(name), bytes).unwrap();
    }
    let mut row = launcher::managed::agent_package_row(
        packed.id().to_hex(),
        installed,
        metadata.join("manifest.toml"),
    );
    row.id = PACKAGE.into();
    row.version = "1.0.0".into();
    row.publisher_key_hex = publisher_hex;
    row.runtime = "tilo-runtime/2026.10.1".into();
    row
}

fn namespace_metadata(pid: u32) -> io::Result<std::fs::Metadata> {
    std::fs::metadata(format!("/proc/{pid}/ns/pid"))
}

fn assert_peer_custody(
    custody: &launcher::owned_service::ServiceCustody,
    namespace: &std::fs::Metadata,
) {
    assert!(custody.init_pid > 1 && custody.start_ticks > 0);
    assert_eq!(
        (namespace.dev(), namespace.ino()),
        (custody.namespace_dev, custody.namespace_ino)
    );
}

fn inspect_peer(
    endpoint: &launcher::owned_service::ServiceEndpoint,
    cancel: &ServiceCancellation,
    sender: &std::sync::mpsc::Sender<OwnedFd>,
) -> io::Result<()> {
    let custody = endpoint.custody()?;
    assert_peer_custody(&custody, &namespace_metadata(custody.init_pid)?);
    let deadline = tilo_test_env::hang_deadline();
    let mut stream = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
    stream.write_all(b"?")?;
    let mut response = [0; 9];
    stream.read_exact(&mut response)?;
    if &response != b"verified\n" {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = u32::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut peer).cast(),
            &raw mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, peer.pid, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).map_err(io::Error::other)?) };
    sender.send(pidfd).map_err(io::Error::other)?;
    cancel.cancel();
    Ok(())
}

fn assert_worker_reaped(receive: &std::sync::mpsc::Receiver<OwnedFd>) {
    let pidfd = tilo_test_env::recv("the reaped worker's pidfd", receive);
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&raw mut poll, 1, 0) }, 1);
    assert_ne!(
        poll.revents & libc::POLLIN,
        0,
        "authenticated worker must be reaped before capture admission ends"
    );
}

#[test]
fn signed_capture_worker_confines_source_and_output_and_reaps_on_cancel() {
    let payload = super::support::capture_worker_payload();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(launcher::run::DEBUG_STDERR_ENV, "1");
    let root = home.root().to_path_buf();
    let (runtime, publisher) = signed_runtime(&root);
    let installed = installed_worker(&root, payload);
    let source_parent = root.join("source-parent");
    let source = source_parent.join("selected");
    let output_parent = root.join("output-parent");
    let output = output_parent.join("selected");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(source.join("document"), b"selected bytes").unwrap();
    std::fs::write(source_parent.join("source-secret"), b"not selected").unwrap();
    std::fs::write(output_parent.join("output-secret"), b"not selected").unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let admission =
        CaptureAdmission::verify(&runtime, &publisher, File::open(&source).unwrap(), uid)
            .unwrap()
            .with_installation(installed.clone(), &installed.publisher_key_hex)
            .unwrap()
            .with_output(File::open(&output).unwrap())
            .unwrap();
    let secret = File::open(source_parent.join("source-secret")).unwrap();
    let raw = unsafe { libc::fcntl(secret.as_raw_fd(), libc::F_DUPFD, 62) };
    assert!(raw >= 62);
    let ambient = unsafe { OwnedFd::from_raw_fd(raw) };
    let source_custody = InaccessibleAncestor::deny(&source_parent).unwrap();
    assert_ne!(
        uid, 0,
        "private-ancestor capture qualification requires an actual nonroot runner"
    );
    assert_eq!(
        File::open(source.join("document"))
            .expect_err("qualification must observe denied access through the source ancestor")
            .kind(),
        io::ErrorKind::PermissionDenied,
        "qualification must observe the broker-private ancestor denial"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let data = root.join(format!("worker-data-{}", "migration".repeat(12)));
    let socket = launcher::appdir::host_app_dir(&data, launcher::appdir::PERSONAL_ROOM, PACKAGE)
        .join("provider.sock");
    assert!(
        socket.as_os_str().as_encoded_bytes().len() > 108,
        "qualify real long preparation roots"
    );
    std::fs::create_dir(&data).unwrap();
    let launch = ServiceLaunch {
        manifest: installed.manifest_path,
        payload: installed.payload_root,
        options: RunOptions {
            expected_package_hash: Some(installed.package_id_hex),
            app_id: Some(PACKAGE.into()),
            no_services: true,
            identity: IdentitySource::None,
            data_home: Some(data.clone()),
            mode: LaunchMode::Service {
                passthrough_argv: vec![
                    uid.to_string(),
                    source_parent.join("source-secret").display().to_string(),
                    output_parent.join("output-secret").display().to_string(),
                    listener.local_addr().unwrap().to_string(),
                    ambient.as_raw_fd().to_string(),
                ],
            },
            ..RunOptions::default()
        },
    };
    let cancel = ServiceCancellation::default();
    let callback_cancel = cancel.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let key = ServiceLeaseKey::default();
    let exit = launcher::owned_service::run_capture_connected(
        launch,
        admission,
        Arc::new(Authority),
        cancel,
        key.clone(),
        Some(Box::new(move |endpoint| {
            inspect_peer(&endpoint, &callback_cancel, &send)
        })),
    )
    .unwrap_or_else(|error| {
        // The canonical bounded diagnostic sink survives the child exiting before pidfd admission.
        std::thread::sleep(Duration::from_millis(100));
        let diagnostic = std::fs::read_to_string(data.join("launcher-debug-output.log"))
            .unwrap_or_else(|read_error| format!("diagnostic unavailable: {read_error}"));
        panic!("actual capture launch failed: {error:?}\nchild diagnostics:\n{diagnostic}");
    });
    assert!(exit.matches(&key, &data, PACKAGE));
    assert_worker_reaped(&receive);
    drop(source_custody);
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"selected bytes"
    );
    assert_eq!(
        std::fs::read(output.join("exported")).unwrap(),
        b"actual exported bytes"
    );
}

#[test]
fn signed_original_source_reader_drops_source_groups_only_after_namespace_confinement() {
    const CHILD: &str = "TILO_ORIGINAL_READER_QUALIFICATION_CHILD";
    tilo_test_env::require_capability!(
        Chown,
        tilo_test_env::switch_identity(65534),
        "the original reader drops root's groups inside a mount namespace of its own"
    );
    if std::env::var_os(CHILD).is_none() {
        // Own only this disposable child mount namespace.
        let result = std::process::Command::new("/usr/bin/unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "capture_service::signed_original_source_reader_drops_source_groups_only_after_namespace_confinement", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(
            result.success(),
            "actual original-reader confinement is unavailable or violated"
        );
        return;
    }
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "qualification requires disposable root namespace"
    );
    let temporary = tempfile::Builder::new()
        .prefix("tilo-original-reader-")
        .tempdir_in("/run")
        .unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TMPDIR", temporary.path());
    let payload = super::support::capture_worker_payload();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(launcher::run::DEBUG_STDERR_ENV, "1");
    let root = home.root().to_path_buf();
    let (runtime, publisher) = signed_runtime(&root);
    let installed = installed_worker(&root, payload);
    let (source, path) = original_reader_source(&root);
    let admission = CaptureAdmission::verify_original_source(
        &runtime,
        &publisher,
        File::open(&source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_archive_helper()
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap();
    let data = root.join("reader-work");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = ServiceLeaseKey::default();
    assert_original_reader_refuses_unattached_custody(
        &root, &runtime, &publisher, &installed, &source,
    );
    assert_original_reader_refuses_foreign_parent(&root, &runtime, &publisher, &installed, &source);
    let launch = original_reader_launch(&installed, &data, &root.join("source-secret"));
    let result = returning_callback_retains_original_custody(
        launch,
        admission,
        File::open(&source).unwrap(),
        key.clone(),
        false,
    );
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "receiving parent credentials remain unchanged"
    );
    let exit = result.unwrap_or_else(|error| {
        let diagnostic = std::fs::read_to_string(data.join("launcher-debug-output.log")).unwrap();
        panic!("actual original-source launch failed: {error:?}\n{diagnostic}")
    });
    assert!(exit.matches(&key, &data, PACKAGE));
    assert_eq!(
        std::fs::read(source.join("group-only")).unwrap(),
        b"source group bytes"
    );
    assert_eq!(
        std::fs::read(root.join("source-secret")).unwrap(),
        b"receiving root secret"
    );
    assert_eq!(
        std::fs::read(source.join("owner-writable")).unwrap(),
        b"unchanged owner bytes"
    );
    assert!(!source.join("forbidden-new-file").exists());

    assert_retired_original_reader(&root, &runtime, &publisher, &installed, &source);
    assert_eq!(unsafe { libc::umount2(path.as_ptr(), 0) }, 0);
}

fn assert_retired_original_reader(
    root: &Path,
    runtime: &Path,
    publisher: &str,
    installed: &package_store::InstalledPackage,
    source: &Path,
) {
    // A permissive, unrelated outer authority cannot replace the retained
    // original context. Retiring it must refuse endpoint use and tear down the
    // actual signed namespace before its owning callback thread returns.
    let retired_data = root.join("retired-reader-work");
    std::fs::create_dir(&retired_data).unwrap();
    std::fs::set_permissions(&retired_data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retired_admission = CaptureAdmission::verify_original_source(
        runtime,
        publisher,
        File::open(source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_archive_helper()
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap();
    let refused = returning_callback_retains_original_custody(
        original_reader_launch(installed, &retired_data, &root.join("source-secret")),
        retired_admission,
        File::open(source).unwrap(),
        ServiceLeaseKey::default(),
        true,
    );
    assert!(
        refused.is_err(),
        "retired read authority cannot admit a successful checkpoint"
    );
}

struct RetainedReadFixture {
    source: File,
    retired: AtomicBool,
}
impl ServiceAuthority for RetainedReadFixture {
    fn check(&self) -> io::Result<()> {
        if self.retired.load(Ordering::Acquire) || !self.source.metadata()?.is_dir() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}
struct CaptureThread {
    cancellation: ServiceCancellation,
    worker: Option<
        std::thread::JoinHandle<
            Result<launcher::owned_service::QuiescentExit, launcher::run::RunError>,
        >,
    >,
}
impl Drop for CaptureThread {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn returning_callback_retains_original_custody(
    launch: ServiceLaunch,
    admission: CaptureAdmission,
    source: File,
    key: ServiceLeaseKey,
    retire: bool,
) -> Result<launcher::owned_service::QuiescentExit, launcher::run::RunError> {
    let context = Arc::new(RetainedReadFixture {
        source,
        retired: AtomicBool::new(false),
    });
    let weak = Arc::downgrade(&context);
    let admission = admission
        .retain_original_source_custody(context.clone())
        .unwrap();
    let diagnostic = launch
        .options
        .data_home
        .as_ref()
        .unwrap()
        .join("launcher-debug-output.log");
    let cancellation = ServiceCancellation::default();
    let worker_cancel = cancellation.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    // Model the allowed callback return: the caller receives an owned worker
    // handle and drops its outer context while the canonical service is live.
    let mut running = CaptureThread {
        cancellation,
        worker: Some(std::thread::spawn(move || {
            launcher::owned_service::run_capture_connected(
                launch,
                admission,
                Arc::new(Authority),
                worker_cancel,
                key,
                Some(Box::new(move |endpoint| {
                    send.send(endpoint)
                        .map_err(|_| io::Error::other("Original reader endpoint receiver retired"))
                })),
            )
        })),
    };
    let endpoint = match receive.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            return Err(signed_reader_failure(
                &mut running,
                &diagnostic,
                "endpoint",
                &error,
            ));
        }
    };
    let (custody, init) = observe_retained_custody(&endpoint, &weak, context);
    let deadline = tilo_test_env::hang_deadline();
    let stream = loop {
        match endpoint.connect() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(signed_reader_failure(
                    &mut running,
                    &diagnostic,
                    "connect",
                    &error,
                ));
            }
        }
    };
    exercise_retained_endpoint(&endpoint, &weak, &stream, custody, retire);
    drop(endpoint);
    let result = running.worker.take().unwrap().join().unwrap();
    assert_retained_exit(&init, &weak);
    drop(stream);
    result
}

fn observe_retained_custody(
    endpoint: &launcher::owned_service::ServiceEndpoint,
    weak: &std::sync::Weak<RetainedReadFixture>,
    context: Arc<RetainedReadFixture>,
) -> (launcher::owned_service::ServiceCustody, OwnedFd) {
    let custody = endpoint.custody().unwrap();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, custody.init_pid, 0) };
    assert!(raw >= 0, "live authenticated init must be observable");
    let init = unsafe { OwnedFd::from_raw_fd(i32::try_from(raw).unwrap()) };
    drop(context);
    assert!(
        weak.upgrade().is_some(),
        "callback return must retain original context"
    );
    (custody, init)
}

fn exercise_retained_endpoint(
    endpoint: &launcher::owned_service::ServiceEndpoint,
    weak: &std::sync::Weak<RetainedReadFixture>,
    stream: &std::os::unix::net::UnixStream,
    custody: launcher::owned_service::ServiceCustody,
    retire: bool,
) {
    // connect() observes the real provider socket created AFTER the signed
    // shim changed source credentials and the worker verified group reads,
    // readonly source, closed descriptors and GNU tar. Its protocol read is
    // still blocked, keeping the actual live subject available for observation.
    let late_custody = endpoint.custody().unwrap();
    assert_eq!(late_custody, custody);
    assert!(late_custody.start_ticks > 0);
    if retire {
        weak.upgrade()
            .unwrap()
            .retired
            .store(true, Ordering::Release);
        assert_eq!(
            endpoint.custody().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            endpoint.connect().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // Keep its existing accepted stream alive until supervisor retirement;
        // the read worker is blocked awaiting a real protocol command.
    } else {
        let mut stream = stream.try_clone().unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        stream.write_all(b"?").unwrap();
        let mut response = [0; 9];
        stream.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"verified\n");
    }
}

fn signed_reader_failure(
    running: &mut CaptureThread,
    diagnostic: &Path,
    phase: &str,
    error: &dyn std::fmt::Debug,
) -> launcher::run::RunError {
    // Preserve canonical ownership and the actual refusal rather than letting
    // an assertion panic silently discard the owning thread's terminal result.
    running.cancellation.cancel();
    let refusal = match running.worker.take().unwrap().join() {
        Ok(Ok(_)) => "unexpected successful checkpoint".to_owned(),
        Ok(Err(error)) => format!("{error:?}"),
        Err(_) => "owning service thread panicked".to_owned(),
    };
    // Existing canonical stderr sink is bounded and synthetic qualification
    // owns its contents. Allow its reader to finish, then bound diagnostic IO.
    std::thread::sleep(Duration::from_millis(100));
    let mut stderr = String::new();
    let observation =
        File::open(diagnostic).and_then(|file| file.take(64 * 1024).read_to_string(&mut stderr));
    if let Err(error) = observation {
        stderr = format!("diagnostic unavailable: {error}");
    }
    launcher::run::RunError::Io(io::Error::other(format!(
        "actual signed reader {phase}: {error:?}; worker refusal: {refusal}\nchild diagnostics:\n{stderr}"
    )))
}

fn assert_retained_exit(init: &OwnedFd, weak: &std::sync::Weak<RetainedReadFixture>) {
    let mut fd = libc::pollfd {
        fd: init.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&raw mut fd, 1, 0) }, 1);
    assert_ne!(
        fd.revents & libc::POLLIN,
        0,
        "callback worker return must observe actual init quiescence"
    );
    assert!(
        weak.upgrade().is_none(),
        "context can retire only after namespace exit and endpoint release"
    );
}

fn original_reader_source(root: &Path) -> (PathBuf, std::ffi::CString) {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("group-only"), b"source group bytes").unwrap();
    std::fs::write(source.join("owner-writable"), b"unchanged owner bytes").unwrap();
    let owned =
        std::ffi::CString::new(source.join("owner-writable").as_os_str().as_encoded_bytes())
            .unwrap();
    assert_eq!(unsafe { libc::chown(owned.as_ptr(), 1000, 1001) }, 0);
    std::fs::set_permissions(
        source.join("owner-writable"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    std::fs::write(root.join("source-secret"), b"receiving root secret").unwrap();
    std::fs::create_dir(source.join("mounted-directory")).unwrap();
    std::fs::write(source.join("mounted-file"), b"underlying file remains").unwrap();
    std::fs::write(
        source.join(std::ffi::OsStr::from_bytes(b"raw-\xff")),
        b"underlying raw remains",
    )
    .unwrap();
    let path = std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 1000, 1001) }, 0);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document =
        std::ffi::CString::new(source.join("group-only").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(document.as_ptr(), 1003, 1002) }, 0);
    std::fs::set_permissions(
        source.join("group-only"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert_eq!(
        unsafe {
            libc::mount(
                path.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        },
        0
    );
    (source, path)
}

fn original_reader_launch(
    installed: &package_store::InstalledPackage,
    data: &Path,
    secret: &Path,
) -> ServiceLaunch {
    ServiceLaunch {
        manifest: installed.manifest_path.clone(),
        payload: installed.payload_root.clone(),
        options: RunOptions {
            mode: LaunchMode::Service {
                passthrough_argv: vec![
                    "--original-read".into(),
                    secret.to_string_lossy().into_owned(),
                ],
            },
            expected_package_hash: Some(installed.package_id_hex.clone()),
            app_id: Some(PACKAGE.into()),
            data_home: Some(data.to_owned()),
            identity: IdentitySource::None,
            no_services: true,
            ..Default::default()
        },
    }
}

fn assert_original_reader_refuses_foreign_parent(
    root: &Path,
    runtime: &Path,
    publisher: &str,
    installed: &package_store::InstalledPackage,
    source: &Path,
) {
    let parent = root.join("foreign-parent");
    let data = parent.join("root-owned-work");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let parent_c = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(parent_c.as_ptr(), 1000, 1001) }, 0);
    let admission = CaptureAdmission::verify_original_source(
        runtime,
        publisher,
        File::open(source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap()
    .retain_original_source_custody(Arc::new(RetainedReadFixture {
        source: File::open(source).unwrap(),
        retired: AtomicBool::new(false),
    }))
    .unwrap();
    let result = launcher::owned_service::run_capture(
        original_reader_launch(installed, &data, &root.join("source-secret")),
        admission,
        Arc::new(Authority),
        ServiceCancellation::default(),
        ServiceLeaseKey::default(),
    );
    assert!(
        result.is_err(),
        "Foreign-owned work ancestry must refuse before bind/spawn"
    );
    assert!(
        std::fs::read_dir(&data).unwrap().next().is_none(),
        "Refusal must not create worker state"
    );
    assert_eq!(std::fs::metadata(&parent).unwrap().uid(), 1000);
}

fn assert_original_reader_refuses_unattached_custody(
    root: &Path,
    runtime: &Path,
    publisher: &str,
    installed: &package_store::InstalledPackage,
    source: &Path,
) {
    let data = root.join("unattached-reader-work");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let admission = CaptureAdmission::verify_original_source(
        runtime,
        publisher,
        File::open(source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_archive_helper()
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap();
    let error = launcher::owned_service::run_capture(
        original_reader_launch(installed, &data, &root.join("source-secret")),
        admission,
        Arc::new(Authority),
        ServiceCancellation::default(),
        ServiceLeaseKey::default(),
    )
    .err()
    .expect("unguarded original reader must refuse before spawn");
    assert!(
        matches!(error, launcher::run::RunError::Io(_)),
        "unguarded reader refused for an unrelated reason: {error:?}"
    );
    if let launcher::run::RunError::Io(error) = error {
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            "Original-source execution requires retained read custody"
        );
    }
    assert!(
        std::fs::read_dir(data).unwrap().next().is_none(),
        "unguarded reader must not create app/person state or worker evidence"
    );
}

#[path = "capture_service/openpgp_native.rs"]
mod openpgp_native;

#[path = "capture_service/mounted_source.rs"]
mod mounted_source;
