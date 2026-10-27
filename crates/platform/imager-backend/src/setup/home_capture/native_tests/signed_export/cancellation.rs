//! Cancel the actual signed exporter after it has written encrypted archive bytes.
use super::super::super::{
    State,
    host_runner::Namespace,
    process::{Custody, Incarnation},
    store::Record,
};
use super::*;
use std::{
    io,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

struct CancelOnDrop<'a>(&'a AtomicBool);
impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

// Signal only pidfds pinned while the actual recorded private namespace is live.
// Dropping on any failed assertion resumes surviving processes so cleanup can run.
struct Paused(Vec<(File, Incarnation)>);
impl Drop for Paused {
    fn drop(&mut self) {
        for (process, _) in &self.0 {
            let _ = signal(process, libc::SIGCONT);
        }
    }
}
fn await_paused(incarnation: Incarnation, uid: u32) -> io::Result<()> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if Incarnation::capture(incarnation.pid, uid)? != incarnation {
            return Err(io::Error::other("paused exporter incarnation changed"));
        }
        let status = std::fs::read_to_string(format!("/proc/{}/status", incarnation.pid))?;
        if status.lines().any(|line| line.starts_with("State:\tT")) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned exporter did not stop",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn signal(process: &File, signal: i32) -> io::Result<()> {
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            process.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn in_namespace(pid: u32, namespace: Namespace, uid: u32) -> bool {
    let root = format!("/proc/{pid}");
    let Ok(owner) = std::fs::metadata(&root) else {
        return false;
    };
    let Ok(actual) = std::fs::metadata(format!("{root}/ns/pid")) else {
        return false;
    };
    owner.uid() == uid
        && (actual.dev(), actual.ino()) == (namespace.namespace_dev, namespace.namespace_ino)
}
fn pause_exporter(custody: &Custody, uid: u32) -> io::Result<Paused> {
    if !custody.active_namespace()? {
        return Err(io::Error::other(
            "export namespace stopped before ciphertext milestone",
        ));
    }
    let namespace = custody
        .namespace
        .ok_or_else(|| io::Error::other("missing admitted namespace"))?;
    let own = std::fs::metadata("/proc/self/ns/pid")?;
    if (own.dev(), own.ino()) == (namespace.namespace_dev, namespace.namespace_ino) {
        return Err(io::Error::other(
            "exporter must have its own private namespace",
        ));
    }
    let mut paused = Paused(Vec::new());
    let mut exporter = false;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if !in_namespace(pid, namespace, uid) {
            continue;
        }
        let before = Incarnation::capture(pid, uid)?;
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let process =
            unsafe { File::from_raw_fd(i32::try_from(descriptor).map_err(io::Error::other)?) };
        if before != Incarnation::capture(pid, uid)? || !in_namespace(pid, namespace, uid) {
            return Err(io::Error::other(
                "export process incarnation changed before pause",
            ));
        }
        signal(&process, libc::SIGSTOP)?;
        paused.0.push((process, before));
        await_paused(before, uid)?;
        exporter |= pid != namespace.init_pid;
    }
    if !exporter || !custody.active_namespace()? {
        return Err(io::Error::other(
            "actual exporter not observed in owned namespace",
        ));
    }
    Ok(paused)
}
fn protected_record(path: &Path) -> io::Result<Record> {
    let facts = std::fs::symlink_metadata(path)?;
    if !facts.is_file() || facts.uid() != 0 || facts.mode() & 0o077 != 0 || facts.len() > 64 * 1024
    {
        return Err(io::Error::other(
            "unsafe native qualification custody record",
        ));
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)
}
fn partial_bytes(media: &Path, uid: u32) -> io::Result<bool> {
    for entry in std::fs::read_dir(media)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tilo-home-") && name.ends_with(".partial") {
            let facts = std::fs::symlink_metadata(entry.path())?;
            if !facts.is_file() || facts.uid() != uid || facts.mode() & 0o077 != 0 {
                return Err(io::Error::other("ciphertext partial has unsafe custody"));
            }
            if facts.len() > 64 * 1024 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn archive_pending(media: &Path) -> io::Result<bool> {
    for entry in std::fs::read_dir(media)? {
        if entry?.file_name().to_string_lossy().ends_with(".sealed") {
            return Ok(false);
        }
    }
    Ok(true)
}
fn milestone(
    record: &Path,
    media: &Path,
    uid: u32,
    finished: impl Fn() -> bool,
) -> io::Result<(Custody, Paused)> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if finished() {
            return Err(io::Error::other(
                "export ended before actual encrypted partial could be cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "actual export ciphertext milestone",
            ));
        }
        if partial_bytes(media, uid)? {
            let record = protected_record(record)?;
            if let Some(custody) = record.worker
                && custody.active_namespace()?
            {
                let paused = pause_exporter(&custody, uid)?;
                if !archive_pending(media)? {
                    return Err(io::Error::other("archive completed before exporter pause"));
                }
                return Ok((custody, paused));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn actual_export_cancellation_reaps_namespace_and_releases_epoch() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "canonical privileged native-tier1 qualifier required"
    );
    let executable = artifact();
    let (uid, gid) = native_account();
    let native_store = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("canonical disposable Btrfs volume required"),
    );
    let temporary = tempfile::Builder::new()
        .prefix("cancel-signed-home-")
        .permissions(std::fs::Permissions::from_mode(0o711))
        .tempdir_in(native_store)
        .unwrap();
    let live_path = temporary.path().join("live");
    fs_native::btrfs::create_subvolume(&live_path).unwrap();
    let live = Live(live_path);
    std::fs::set_permissions(&live.0, std::fs::Permissions::from_mode(0o711)).unwrap();
    let home = live.0.join("alice");
    private(&home);
    let database = dataset(&home);
    private(&live.0.join("bob"));
    std::fs::write(live.0.join("bob/private"), b"unapproved sibling").unwrap();
    // Real data, rather than fabricated progress or a pre-created media receipt.
    let large_path = home.join("Documents/large-file");
    let mut large = File::create(&large_path).unwrap();
    let block = [0x5a; 64 * 1024];
    for _ in 0..2048 {
        large.write_all(&block).unwrap();
    }
    large.sync_all().unwrap();
    drop(large);
    let original_digest = executable_digest(&large_path);
    let mut writer = writer(&database);
    ownership(&home, uid, gid);
    let before: u64 = query(&database, "SELECT n FROM value").parse().unwrap();
    let media = temporary.path().join("media");
    private(&media);
    ownership(&media, uid, gid);
    let operation = uuid::Uuid::new_v4().to_string();
    let review_executable = stage_reviewer(temporary.path());
    let (grant, output, reviewer) =
        reviewer(&review_executable, &home, &media, &operation, uid, gid);
    let broker_directory = temporary.path().join("broker");
    private(&broker_directory);
    let store = Store::open(
        PinnedDirectory::open_absolute(&broker_directory).unwrap(),
        &operation,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut broker = Broker::capture_in(grant, uid, &cancel, store).unwrap();
    reviewer.finish();
    assert!(
        matches!(broker.run_command(&executable, Command::Review, None, &cancel).unwrap(), Response::Review { files, .. } if files > 0)
    );
    let record_path = broker_directory
        .join("records")
        .join(format!("{operation}.json"));
    let (result, observed) = std::thread::scope(|scope| {
        let stop_on_error = CancelOnDrop(&cancel);
        let exporting = scope.spawn(|| {
            broker.run_command(
                &executable,
                Command::Export {
                    export_operation: uuid::Uuid::new_v4().to_string(),
                    setup_binding: BINDING.into(),
                    password: Secret::new(Zeroizing::new(PASSWORD.to_vec())),
                },
                Some(output),
                &cancel,
            )
        });
        let observed = milestone(&record_path, &media, uid, || exporting.is_finished());
        cancel.store(true, Ordering::Release);
        let result = exporting.join();
        drop(stop_on_error);
        (result, observed)
    });
    // Production failure teardown runs even when observation fails; it must prove
    // the real namespace stopped before deleting the captured source epoch.
    let stopped = broker.finish_failed_command();
    let released = broker.release();
    stopped.unwrap();
    released.unwrap();
    let (custody, paused) = observed.unwrap();
    assert!(
        result.unwrap().is_err(),
        "actual cancelled export must not report success"
    );
    custody.require_stopped().unwrap();
    drop(paused);
    let record = protected_record(&record_path).unwrap();
    assert_eq!(record.state, State::Released);
    assert!(record.worker.is_none());
    assert!(
        !broker_directory
            .join("snapshots")
            .join(format!("home-{operation}"))
            .exists()
    );
    assert!(
        archive_pending(&media).unwrap(),
        "cancelled export cannot publish a complete sealed archive"
    );
    assert!(writer.0.try_wait().unwrap().is_none());
    assert!(
        query(&database, "SELECT n FROM value")
            .parse::<u64>()
            .unwrap()
            > before
    );
    assert_eq!(query(&database, "PRAGMA integrity_check"), "ok");
    assert_eq!(
        std::fs::metadata(&large_path).unwrap().len(),
        128 * 1024 * 1024
    );
    assert_eq!(executable_digest(&large_path), original_digest);
    assert_eq!(
        std::fs::read(home.join(".var/app/so.test/data/document")).unwrap(),
        b"hidden application document"
    );
    assert_eq!(
        std::fs::read(live.0.join("bob/private")).unwrap(),
        b"unapproved sibling"
    );
}
