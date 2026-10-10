//! Mandatory real-process/real-FUSE exact-share release evidence.
//!
//! Unlike the in-process protocol harnesses, this test launches the shipped
//! `fs` binary twice, gives host and recipient distinct certified identity
//! roots and persistent stores, and drives both mounts through the kernel.

// This executable integration-test target uses panic/unwrap as assertion
// boundaries while driving child processes and mandatory system facilities.
#![allow(clippy::panic, clippy::unwrap_used)]

use std::ffi::CString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use fs::availability::{EvictionDecision, EvictionRequirements, Residency};
use fs::eviction::{EvictionQueue, EvictionRequestState};
use fs::{AvailabilityRegistry, ContentStore};
use fs_protocol::{
    ContentHash, ContentRevisionV1, FilePermission, FileRecordId, SHARE_CONTROL_VERSION,
    ShareControlOperationV1, ShareControlRequestV1, ShareControlResultV1,
};
use tempfile::TempDir;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
    persist::{write_device_cert, write_public_key, write_signing_key},
    sign_device_cert,
};

const PAYLOAD: &[u8] = b"separate-process exact-share payload";

struct MountedProcess {
    child: Child,
    output: Receiver<String>,
    logs: Arc<Mutex<Vec<String>>>,
    mount: PathBuf,
    stopped: bool,
}

impl MountedProcess {
    fn wait_for_line(&self, expected: &str) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.output.recv_timeout(remaining).unwrap_or_else(|error| {
                let output = self.logs.lock().unwrap().join("\n");
                panic!("process did not emit {expected:?}: {error}; output was:\n{output}")
            });
            if line.contains(expected) {
                return;
            }
        }
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        let _ = Command::new("fusermount3")
            .args(["-u", "-z"])
            .arg(&self.mount)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = tilo_test_env::hang_deadline();
        while self.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(10));
        }
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
        self.stopped = true;
    }
}

impl Drop for MountedProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn install_identity(
    root: &Path,
    root_seed: [u8; 32],
    device_seed: [u8; 32],
    owner: bool,
    clock_now_ms: u64,
) {
    let paths = IdentityPaths::under(root);
    paths.ensure_directories().unwrap();
    let root_key = SigningKey::from_bytes(&root_seed);
    let device_key = adopt_device_key(&paths, &device_seed).unwrap();
    write_public_key(&paths.user_pubkey, &root_key.verifying_key().to_bytes()).unwrap();
    if owner {
        write_signing_key(&paths.user_privkey, &root_key).unwrap();
    }
    // The processes validate certificates against the clock files, not the
    // wall clock, so issue them relative to that clock.
    let issued_at_ns = clock_now_ms
        .saturating_mul(1_000_000)
        .saturating_sub(1_000_000_000);
    let mut cert = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: if owner { "fuse-host" } else { "fuse-viewer" }.into(),
        capabilities: if owner {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root_key).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();
}

fn capture_lines(child: &mut Child) -> (Receiver<String>, Arc<Mutex<Vec<String>>>) {
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let logs = Arc::new(Mutex::new(Vec::new()));
    for reader in [
        BufReader::new(Box::new(stdout) as Box<dyn std::io::Read + Send>),
        BufReader::new(Box::new(stderr) as Box<dyn std::io::Read + Send>),
    ] {
        let tx = tx.clone();
        let logs = Arc::clone(&logs);
        std::thread::spawn(move || {
            for line in reader.lines() {
                let line = line.unwrap_or_else(|error| error.to_string());
                logs.lock().unwrap().push(line.clone());
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    (rx, logs)
}

fn spawn_host(
    binary: &Path,
    identity: &Path,
    authority_sockets: &Path,
    mount: &Path,
    cache: &Path,
    seed: &Path,
    clock: &Path,
) -> MountedProcess {
    let mut child = Command::new(binary)
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .env("TILO_AUTHORITY_SOCKET_DIR", authority_sockets)
        .env("TILO_FS_FUSE_OWNER_ONLY", "1")
        .arg("--test-clock-file")
        .arg(clock)
        .args(["serve", "--mount-point"])
        .arg(mount)
        .arg("--cache-dir")
        .arg(cache)
        .arg("--seed")
        .arg(seed)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (output, logs) = capture_lines(&mut child);
    MountedProcess {
        child,
        output,
        logs,
        mount: mount.to_path_buf(),
        stopped: false,
    }
}

fn spawn_recipient(
    binary: &Path,
    identity: &Path,
    mount: &Path,
    cache: &Path,
    uri: &str,
    clock: &Path,
) -> MountedProcess {
    let mut child = Command::new(binary)
        .env(fs::identity::IDENTITY_ROOT_ENV, identity)
        .env("TILO_FS_FUSE_OWNER_ONLY", "1")
        .arg("--test-clock-file")
        .arg(clock)
        .args(["mount-share", "--mount-point"])
        .arg(mount)
        .arg("--link-stdin")
        .arg("--cache-dir")
        .arg(cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{uri}").unwrap();
    let (output, logs) = capture_lines(&mut child);
    MountedProcess {
        child,
        output,
        logs,
        mount: mount.to_path_buf(),
        stopped: false,
    }
}

fn get_xattr(path: &Path, name: &str) -> std::io::Result<Vec<u8>> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    let size = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut bytes = vec![0; size.cast_unsigned()];
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    bytes.truncate(read.cast_unsigned());
    Ok(bytes)
}

fn set_xattr(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn wait_for_process<T>(
    process: &mut MountedProcess,
    what: &str,
    mut inspect: impl FnMut() -> Option<T>,
) -> T {
    tilo_test_env::wait_for(what, || {
        if let Some(status) = process.child.try_wait().unwrap() {
            let output = process.logs.lock().unwrap().join("\n");
            panic!("mounted process exited with {status}; output was:\n{output}");
        }
        inspect()
    })
}

fn one_visible_file(mount: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(mount)
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (entries.len() == 1)
        .then(|| entries.first().map(std::fs::DirEntry::path))
        .flatten()
}

async fn create_view_link_at(
    socket: &Path,
    record_id: FileRecordId,
    expires_at_ms: u64,
) -> ShareControlResultV1 {
    let deadline = tilo_test_env::hang_deadline();
    let mut stream = loop {
        match tokio::net::UnixStream::connect(socket).await {
            Ok(stream) => break stream,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "share-control socket did not become ready: {error}"
                );
                tokio::task::yield_now().await;
            }
        }
    };
    fs::write_share_control_request(
        &mut stream,
        &ShareControlRequestV1 {
            protocol_version: SHARE_CONTROL_VERSION,
            operation: ShareControlOperationV1::CreateBearerV2 {
                record_id,
                permission: FilePermission::View,
                expires_at_ms,
                max_uses: None,
            },
        },
    )
    .await
    .unwrap();
    fs::read_share_control_response(&mut stream)
        .await
        .unwrap()
        .result
}

async fn create_view_link(socket: &Path, record_id: FileRecordId, now_ms: u64) -> String {
    let expires_at_ms = now_ms.saturating_add(10 * 60 * 1000);
    match create_view_link_at(socket, record_id, expires_at_ms).await {
        ShareControlResultV1::BearerCreatedV2 { uri } => uri,
        result => panic!("share creation failed: {result:?}"),
    }
}

fn write_clock(path: &Path, now_ms: u64) {
    std::fs::write(path, now_ms.to_string()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

// This mandatory release scenario keeps process lifecycle, eviction evidence,
// restart, expiry, and rehydration barriers in one linear auditable trace.
#[allow(
    clippy::too_many_lines,
    reason = "keeps process lifecycle and restart recovery barriers in one auditable trace"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn separate_process_exact_share_evicts_restarts_and_rehydrates() {
    if std::env::var("TILO_FS_FUSE_REQUIRED").as_deref() != Ok("1") {
        eprintln!("skipping real FUSE lifecycle outside the dedicated mandatory FUSE job");
        return;
    }
    let fuse = std::fs::metadata("/dev/fuse").expect("mandatory test requires /dev/fuse");
    assert!(std::os::unix::fs::FileTypeExt::is_char_device(
        &fuse.file_type()
    ));
    assert!(
        Command::new("fusermount3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("mandatory test requires fusermount3")
            .success()
    );

    let temp = TempDir::new().unwrap();
    let host_identity = temp.path().join("host-identity");
    let viewer_identity = temp.path().join("viewer-identity");
    let loss_identity = temp.path().join("loss-identity");
    let host_mount = temp.path().join("host-mount");
    let viewer_mount = temp.path().join("viewer-mount");
    let loss_mount = temp.path().join("loss-mount");
    let host_cache = temp.path().join("host-cache");
    let authority_sockets = temp.path().join("authority-sockets");
    let viewer_cache = temp.path().join("viewer-cache");
    let loss_cache = temp.path().join("loss-cache");
    let seed = temp.path().join("seed");
    let host_clock = temp.path().join("host-clock-ms");
    let viewer_clock = temp.path().join("viewer-clock-ms");
    let loss_clock = temp.path().join("loss-clock-ms");
    let clock_now = tilo_clock::unix_now_ms();
    write_clock(&host_clock, clock_now);
    write_clock(&viewer_clock, clock_now);
    write_clock(&loss_clock, clock_now);
    for path in [
        &host_mount,
        &viewer_mount,
        &loss_mount,
        &seed,
        &authority_sockets,
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(seed.join("shared.bin"), PAYLOAD).unwrap();
    // The real processes advertise their Iroh identities beyond this guest.
    // Keep concurrent CI runs distinct while binding assertions to these keys.
    let host_seed: [u8; 32] = rand::random();
    install_identity(&host_identity, rand::random(), host_seed, true, clock_now);
    install_identity(
        &viewer_identity,
        rand::random(),
        rand::random(),
        false,
        clock_now,
    );
    install_identity(
        &loss_identity,
        rand::random(),
        rand::random(),
        false,
        clock_now,
    );
    let host_device = SigningKey::from_bytes(&host_seed)
        .verifying_key()
        .to_bytes();

    let binary = Path::new(env!("CARGO_BIN_EXE_fs"));
    let mut host = spawn_host(
        binary,
        &host_identity,
        &authority_sockets,
        &host_mount,
        &host_cache,
        &seed,
        &host_clock,
    );
    host.wait_for_line("Mounting QuickFS");
    let host_file = host_mount.join("shared.bin");
    let record_bytes = wait_for_process(&mut host, "the host to stamp the record id", || {
        get_xattr(&host_file, fs::syncstate::RECORD_ID_XATTR).ok()
    });
    let record_id = FileRecordId(uuid::Uuid::from_bytes(record_bytes.try_into().unwrap()));
    let share_socket = host_cache.join("share-control.sock");
    let uri = create_view_link(&share_socket, record_id, clock_now).await;

    // Move the already-running shipped daemon backward and forward without a
    // wall sleep. The exact deadline is denied (`expiry <= now`), while the
    // same absolute deadline is valid when the injected clock is behind it.
    write_clock(&host_clock, clock_now.saturating_sub(1));
    assert!(matches!(
        create_view_link_at(&share_socket, record_id, clock_now).await,
        ShareControlResultV1::BearerCreatedV2 { .. }
    ));
    write_clock(&host_clock, clock_now);
    assert!(matches!(
        create_view_link_at(&share_socket, record_id, clock_now).await,
        ShareControlResultV1::Error { .. }
    ));
    std::fs::write(&host_clock, b"malformed").unwrap();
    assert!(matches!(
        create_view_link_at(&share_socket, record_id, clock_now + 1).await,
        ShareControlResultV1::Error { .. }
    ));
    write_clock(&host_clock, clock_now);

    let mut viewer = spawn_recipient(
        binary,
        &viewer_identity,
        &viewer_mount,
        &viewer_cache,
        &uri,
        &viewer_clock,
    );
    viewer.wait_for_line(fs::recipient_mount::RECIPIENT_MOUNT_READY_LINE);
    let viewer_file = wait_for_process(&mut viewer, "the viewer to list the shared file", || {
        one_visible_file(&viewer_mount)
    });
    let write_error = std::fs::write(&viewer_file, b"forbidden").unwrap_err();
    assert_eq!(write_error.raw_os_error(), Some(libc::EROFS));

    // Drive the public Retry control so successful hydration is durably
    // reflected in availability before asking the same public boundary to
    // remove the cached bytes.
    set_xattr(
        &viewer_file,
        fs::syncstate::AVAILABILITY_CONTROL_XATTR,
        b"retry",
    )
    .unwrap();
    assert_eq!(std::fs::read(&viewer_file).unwrap(), PAYLOAD);

    let availability_path = viewer_cache.join("availability.json");
    let observation = wait_for_process(
        &mut viewer,
        "the viewer to record its replica observation",
        || {
            let registry = AvailabilityRegistry::open(&availability_path).ok()?;
            let record = registry.get(record_id)?;
            (record.replica_observations.len() == 1
                && record.residency
                    == Residency::Resident {
                        bytes: PAYLOAD.len() as u64,
                    })
            .then(|| record.replica_observations[0].clone())
        },
    );
    assert_eq!(observation.device.as_bytes(), &host_device);
    assert_eq!(observation.record_id, record_id);
    assert_eq!(observation.issued_at_ms, clock_now);
    assert_eq!(observation.observed_at_ms, clock_now);
    let registry = AvailabilityRegistry::open(&availability_path).unwrap();
    assert_eq!(
        registry.eviction_decision(
            record_id,
            observation.observed_at_ms,
            EvictionRequirements::default(),
            false,
        ),
        EvictionDecision::Allowed
    );

    let content_hash: [u8; 32] = get_xattr(&viewer_file, fs::syncstate::CONTENT_HASH_XATTR)
        .unwrap()
        .try_into()
        .unwrap();
    set_xattr(
        &viewer_file,
        fs::syncstate::AVAILABILITY_CONTROL_XATTR,
        b"evict",
    )
    .unwrap();
    wait_for_process(&mut viewer, "the viewer to evict the file", || {
        let registry = AvailabilityRegistry::open(&availability_path).ok()?;
        (registry.get(record_id)?.residency == Residency::NotResident).then_some(())
    });
    wait_for_process(
        &mut viewer,
        "the viewer to project the evicted file",
        || {
            (get_xattr(&viewer_file, fs::syncstate::SYNC_XATTR)
                .ok()?
                .as_slice()
                == b"projected")
                .then_some(())
        },
    );

    viewer.stop();
    let completed = EvictionQueue::open(viewer_cache.join("evictions.json")).unwrap();
    let request = completed.requests().next().unwrap();
    assert!(matches!(
        request.state,
        EvictionRequestState::Completed { .. }
    ));
    assert!(
        request
            .requested_hashes
            .contains(&iroh_blobs::Hash::from(content_hash))
    );
    let restarted = AvailabilityRegistry::open(&availability_path).unwrap();
    assert_eq!(
        restarted.get(record_id).unwrap().replica_observations.len(),
        1
    );
    drop(restarted);

    let mut restarted_viewer = spawn_recipient(
        binary,
        &viewer_identity,
        &viewer_mount,
        &viewer_cache,
        &uri,
        &viewer_clock,
    );
    restarted_viewer.wait_for_line(fs::recipient_mount::RECIPIENT_MOUNT_READY_LINE);
    let restarted_file = wait_for_process(
        &mut restarted_viewer,
        "the restarted viewer to list the shared file",
        || one_visible_file(&viewer_mount),
    );
    assert_eq!(std::fs::read(&restarted_file).unwrap(), PAYLOAD);
    wait_for_process(
        &mut restarted_viewer,
        "the restarted viewer to report the file local",
        || {
            (get_xattr(&restarted_file, fs::syncstate::SYNC_XATTR)
                .ok()?
                .as_slice()
                == b"local")
                .then_some(())
        },
    );
    restarted_viewer.stop();

    let final_store = ContentStore::open(viewer_cache.join("blobs"))
        .await
        .unwrap();
    let revision = ContentRevisionV1 {
        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
        record_id,
        version_id: observation.version_id,
        content_hash: Some(ContentHash(content_hash)),
        byte_len: observation.byte_len,
        metadata_hash: observation.metadata_hash,
    };
    final_store
        .verify_durable_revision(&revision)
        .await
        .unwrap();
    final_store.store().shutdown().await.unwrap();
    drop(final_store);

    // A distinct recipient proves source loss without disturbing the recovery
    // lifecycle above. Hydrate and evict through public controls, wait for the
    // durable projected/NotResident boundary, then stop the only serving
    // process before issuing the kernel read.
    let loss_availability = loss_cache.join("availability.json");
    let loss_uri = create_view_link(&share_socket, record_id, clock_now).await;
    let mut loss_viewer = spawn_recipient(
        binary,
        &loss_identity,
        &loss_mount,
        &loss_cache,
        &loss_uri,
        &loss_clock,
    );
    loss_viewer.wait_for_line(fs::recipient_mount::RECIPIENT_MOUNT_READY_LINE);
    let loss_file = wait_for_process(
        &mut loss_viewer,
        "the loss viewer to list the shared file",
        || one_visible_file(&loss_mount),
    );
    set_xattr(
        &loss_file,
        fs::syncstate::AVAILABILITY_CONTROL_XATTR,
        b"retry",
    )
    .unwrap();
    assert_eq!(std::fs::read(&loss_file).unwrap(), PAYLOAD);
    wait_for_process(
        &mut loss_viewer,
        "the loss viewer to hydrate the file",
        || {
            let registry = AvailabilityRegistry::open(&loss_availability).ok()?;
            let record = registry.get(record_id)?;
            (record.residency
                == Residency::Resident {
                    bytes: PAYLOAD.len() as u64,
                }
                && !record.replica_observations.is_empty())
            .then_some(())
        },
    );
    set_xattr(
        &loss_file,
        fs::syncstate::AVAILABILITY_CONTROL_XATTR,
        b"evict",
    )
    .unwrap();
    wait_for_process(
        &mut loss_viewer,
        "the loss viewer to evict the file",
        || {
            let registry = AvailabilityRegistry::open(&loss_availability).ok()?;
            (registry.get(record_id)?.residency == Residency::NotResident).then_some(())
        },
    );
    wait_for_process(
        &mut loss_viewer,
        "the loss viewer to project the evicted file",
        || {
            (get_xattr(&loss_file, fs::syncstate::SYNC_XATTR)
                .ok()?
                .as_slice()
                == b"projected")
                .then_some(())
        },
    );
    host.stop();
    let unavailable = std::fs::read(&loss_file).unwrap_err();
    assert_eq!(unavailable.raw_os_error(), Some(libc::EIO));
    assert_eq!(
        get_xattr(&loss_file, fs::syncstate::SYNC_XATTR)
            .unwrap()
            .as_slice(),
        b"projected"
    );
    assert_eq!(
        AvailabilityRegistry::open(&loss_availability)
            .unwrap()
            .get(record_id)
            .unwrap()
            .residency,
        Residency::NotResident
    );
    loss_viewer.stop();
}

/// Voluntary wakeups per second an idle `fs serve` may make across all of
/// its threads (TIL-750). An idle thread blocks once and stays blocked, so
/// each voluntary switch is a timer or a poll that fired. Preemptions are
/// printed but not budgeted: on a loaded shared runner they measure the
/// neighbours, not this process (TIL-749).
const IDLE_SERVE_WAKEUP_BUDGET: f64 = 5.0;

/// The shipped daemon serving a mounted workspace that nobody touches
/// sleeps: no persist poll, no session-supervisor poll, no repair tick.
/// Prints the per-thread table the budget is judged on.
#[cfg(target_os = "linux")]
#[test]
fn idle_serve_mount_stays_asleep() {
    if std::env::var("TILO_FS_FUSE_REQUIRED").as_deref() != Ok("1") {
        eprintln!("skipping the idle serve mount outside the dedicated mandatory FUSE job");
        return;
    }
    let temp = TempDir::new().unwrap();
    let identity = temp.path().join("identity");
    let mount = temp.path().join("mount");
    let cache = temp.path().join("cache");
    let seed = temp.path().join("seed");
    let clock = temp.path().join("clock-ms");
    let sockets = temp.path().join("authority-sockets");
    for path in [&mount, &seed, &sockets] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(seed.join("idle.txt"), PAYLOAD).unwrap();
    let clock_now = tilo_clock::unix_now_ms();
    write_clock(&clock, clock_now);
    install_identity(&identity, rand::random(), rand::random(), true, clock_now);
    let binary = Path::new(env!("CARGO_BIN_EXE_fs"));
    let mut host = spawn_host(binary, &identity, &sockets, &mount, &cache, &seed, &clock);
    host.wait_for_line("Mounting QuickFS");
    let file = mount.join("idle.txt");
    wait_for_process(&mut host, "the mount to serve its seed", || {
        get_xattr(&file, fs::syncstate::RECORD_ID_XATTR).ok()
    });

    // Startup work (seeding, the first persist, thumbnails, the first sync
    // pass) settles well inside the first 10 s; the window sees only idle.
    let wakeups = tilo_test_env::idle::idle_wakeups(
        host.child.id(),
        Duration::from_secs(10),
        Duration::from_secs(20),
    )
    .expect("the idle daemon's threads must be observable");
    eprintln!("{wakeups}");
    let blocked: u64 = wakeups.rows.iter().map(|row| row.voluntary).sum();
    let per_second = wakeups.rate(blocked);
    eprintln!("TIL-750 idle fs serve: {per_second:.2} voluntary wakeups/s");
    host.stop();
    assert!(
        per_second <= IDLE_SERVE_WAKEUP_BUDGET,
        "an idle fs serve woke {per_second:.2} times a second (budget \
         {IDLE_SERVE_WAKEUP_BUDGET}):\n{wakeups}"
    );
}
