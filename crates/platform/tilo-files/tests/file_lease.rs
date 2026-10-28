#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "macos")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, mpsc};
use tilo_files::native::atomic::lease::{AcquireError, FileLease, LeaseMode};
use tilo_lock::MutexRecover as _;

const CHILD_MARKER: &str = "TILO_ATOMIC_FILE_LEASE_CHILD";
const CHILD_PATH: &str = "TILO_ATOMIC_FILE_LEASE_PATH";
const CHILD_READY: &str = "TILO_ATOMIC_FILE_LEASE_READY";
const CHILD_TEST: &str = "child_holds_exclusive_lease";
static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn serialize_native_lock_tests() -> MutexGuard<'static, ()> {
    // A concurrently spawned child can briefly inherit another test's open
    // lock across fork. Keep process creation and native leases disjoint.
    TEST_MUTEX.lock_recover()
}

fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}

#[test]
fn exclusive_lease_excludes_a_second_open_until_drop() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");

    let first = FileLease::try_acquire(open(&path), LeaseMode::Exclusive).unwrap();
    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Exclusive),
        Err(AcquireError::Busy)
    ));
    drop(first);

    let _second = FileLease::try_acquire(open(&path), LeaseMode::Exclusive).unwrap();
}

#[test]
fn two_shared_leases_can_coexist() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");

    let _first = FileLease::try_acquire(open(&path), LeaseMode::Shared).unwrap();
    let _second = FileLease::try_acquire(open(&path), LeaseMode::Shared).unwrap();
}

#[test]
fn a_shared_lease_rejects_an_exclusive_lease() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");

    let _shared = FileLease::try_acquire(open(&path), LeaseMode::Shared).unwrap();
    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Exclusive),
        Err(AcquireError::Busy)
    ));
}

#[test]
fn an_exclusive_lease_rejects_a_shared_lease() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");

    let _exclusive = FileLease::try_acquire(open(&path), LeaseMode::Exclusive).unwrap();
    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Shared),
        Err(AcquireError::Busy)
    ));
}

#[test]
fn metadata_comes_from_the_held_file() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    fs::write(&path, b"guard").unwrap();

    let lease = FileLease::try_acquire(open(&path), LeaseMode::Exclusive).unwrap();

    assert_eq!(lease.metadata().unwrap().len(), 5);

    #[cfg(unix)]
    {
        let replacement = directory.path().join("replacement");
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_eq!(lease.metadata().unwrap().len(), 5);
        assert_eq!(fs::metadata(path).unwrap().len(), 11);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn assert_native_io_failure(result: Result<FileLease, AcquireError>) {
    match result.unwrap_err() {
        AcquireError::Busy => panic!("an unsupported handle is not lock contention"),
        AcquireError::Io(error) => {
            assert!(error.raw_os_error().is_some());
            let wrapped = AcquireError::Io(error);
            assert!(std::error::Error::source(&wrapped).is_some());
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_io_failure_is_distinguishable_from_contention() {
    let _serial = serialize_native_lock_tests();
    let (stream, _peer) = UnixStream::pair().unwrap();
    let file = File::from(OwnedFd::from(stream));

    assert_native_io_failure(FileLease::try_acquire(file, LeaseMode::Exclusive));
}

#[cfg(target_os = "linux")]
#[test]
fn native_io_failure_is_distinguishable_from_contention() {
    // Stable Linux UAPI value on the supported x86_64 and aarch64 targets.
    const O_PATH: i32 = 0o10_000_000;

    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    fs::write(&path, b"sentinel").unwrap();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_PATH)
        .open(path)
        .unwrap();

    assert_native_io_failure(FileLease::try_acquire(file, LeaseMode::Exclusive));
}

#[cfg(target_os = "windows")]
#[test]
fn native_io_failure_is_distinguishable_from_contention() {
    let _serial = serialize_native_lock_tests();
    let file = File::open("NUL").unwrap();

    assert_native_io_failure(FileLease::try_acquire(file, LeaseMode::Exclusive));
}

#[test]
fn normal_child_release_releases_the_lease() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let mut child = LeaseChild::spawn(&path);

    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Exclusive),
        Err(AcquireError::Busy)
    ));
    child.release_and_wait();

    let acquired = FileLease::try_acquire(open(&path), LeaseMode::Exclusive);
    assert!(
        acquired.is_ok(),
        "lock remained after normal child exit: {acquired:?}"
    );
}

#[test]
fn abrupt_child_death_releases_the_lease() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let mut child = LeaseChild::spawn(&path);

    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Exclusive),
        Err(AcquireError::Busy)
    ));
    child.kill_and_wait();

    let acquired = FileLease::try_acquire(open(&path), LeaseMode::Exclusive);
    assert!(
        acquired.is_ok(),
        "lock remained after abrupt child exit: {acquired:?}"
    );
}

#[test]
fn blocking_acquisition_completes_after_another_process_releases() {
    let _serial = serialize_native_lock_tests();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let mut child = LeaseChild::spawn(&path);

    assert!(matches!(
        FileLease::try_acquire(open(&path), LeaseMode::Exclusive),
        Err(AcquireError::Busy)
    ));

    let (attempting_tx, attempting_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let waiter_path = path;
    let waiter = std::thread::spawn(move || {
        let waiter_file = open(&waiter_path);
        // The kernel's id for this thread, where the platform has one.
        let tid = tilo_test_env::idle::current_thread_id().ok();
        attempting_tx.send(tid).unwrap();
        acquired_tx
            .send(FileLease::acquire(waiter_file, LeaseMode::Exclusive))
            .unwrap();
    });
    let waiter_tid = tilo_test_env::recv("the waiter to start acquiring", &attempting_rx);
    // Blocked in the kernel on the child's lease, not granted.
    if let Some(tid) = waiter_tid.map(|tid| tid.to_string()) {
        tilo_test_env::wait_until("the waiter to block on the held lease", || {
            tilo_test_env::idle::blocked(&tid)
        });
    }
    assert!(matches!(
        acquired_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    child.release_and_wait();
    let acquired = tilo_test_env::recv("the blocked acquisition", &acquired_rx);
    assert!(
        acquired.is_ok(),
        "blocking acquisition failed: {acquired:?}"
    );
    drop(acquired);
    waiter.join().unwrap();
}

#[test]
fn child_holds_exclusive_lease() {
    if std::env::var_os(CHILD_MARKER).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }
    let _serial = serialize_native_lock_tests();

    let path = PathBuf::from(std::env::var_os(CHILD_PATH).unwrap());
    let ready = PathBuf::from(std::env::var_os(CHILD_READY).unwrap());
    let _lease = FileLease::acquire(open(&path), LeaseMode::Exclusive).unwrap();
    fs::write(ready, b"ready").unwrap();

    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
}

struct LeaseChild {
    child: Option<Child>,
    ready: PathBuf,
}

impl LeaseChild {
    fn spawn(path: &Path) -> Self {
        let ready = path.with_extension("ready");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_MARKER, "1")
            .env(CHILD_PATH, path)
            .env(CHILD_READY, &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut child = Self {
            child: Some(child),
            ready,
        };
        child.wait_until_ready();
        child
    }

    fn wait_until_ready(&mut self) {
        tilo_test_env::wait_until("the lease child to become ready", || {
            if let Some(status) = self.child_mut().try_wait().unwrap() {
                panic!("lease child exited before signaling readiness: {status}");
            }
            self.ready.is_file()
        });
    }

    fn release_and_wait(&mut self) {
        self.child_mut()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        let status = self.wait_until_exit();
        assert!(status.success(), "lease child failed: {status}");
    }

    fn kill_and_wait(&mut self) {
        self.child_mut().kill().unwrap();
        let status = self.wait_until_exit();
        assert!(
            !status.success(),
            "killed lease child unexpectedly succeeded"
        );
    }

    fn wait_until_exit(&mut self) -> ExitStatus {
        tilo_test_env::wait_for("the lease child to exit", || {
            self.child_mut().try_wait().unwrap()
        })
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().unwrap()
    }
}

impl Drop for LeaseChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.ready);
    }
}
