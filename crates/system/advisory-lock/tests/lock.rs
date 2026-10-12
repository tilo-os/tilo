//! Lock semantics observed through real descriptors and a real child process.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers assert on real filesystem and process behavior"
)]

use advisory_lock::{FileLock, LockMode, TryLockError, open_lock_file, owner_pid};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn busy(result: Result<FileLock, TryLockError>) -> bool {
    matches!(result.err(), Some(TryLockError::WouldBlock))
}

/// Spawns `cat` with `file` inherited as descriptor 3. `cat` waits on its
/// piped stdin, so the child keeps the inherited descriptor open until the
/// test closes that pipe. `spawn` returns only after the child has executed,
/// so the child holds the descriptor from then on.
fn child_holding(file: &File) -> Child {
    let fd = file.as_raw_fd();
    let mut command = Command::new("cat");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the hook only calls the async-signal-safe `dup2` and `fcntl`.
    // When the lock file already is descriptor 3 (a test process with only
    // the standard streams open), `dup2` is a no-op that leaves the guard's
    // close-on-exec flag in place, so the flag is cleared explicitly for the
    // descriptor to survive the exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}

fn assert_child_holds(child: &Child, path: &Path) {
    #[cfg(target_os = "linux")]
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/fd/3", child.id())).unwrap(),
        path.canonicalize().unwrap(),
        "the child must hold the inherited lock descriptor"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = (child, path);
}

fn finish(mut child: Child) {
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

/// A fresh lock path per phase, so no phase observes another's lock.
fn fresh_lock_path() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("lock");
    (temp, path)
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "demonstrates the raw flock behavior the guard exists to prevent"
)]
fn the_guard_releases_its_lock_even_while_a_child_inherits_the_descriptor() {
    // Phase: closing without unlock leaves the lock with an inheriting child.
    // The failure the guard exists to prevent: the child's inherited copy of
    // the description keeps a merely closed lock alive.
    {
        let (_temp, path) = fresh_lock_path();
        let file = open_lock_file(&path).unwrap();
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let child = child_holding(&file);
        assert_child_holds(&child, &path);
        drop(file);
        assert!(busy(FileLock::try_open(&path, LockMode::Exclusive)));
        finish(child);
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
    }

    // Phase: dropping the guard releases the lock while a child holds the
    // inherited fd.
    {
        let (_temp, path) = fresh_lock_path();
        let guard = FileLock::open(&path, LockMode::Exclusive).unwrap();
        let mut child = child_holding(guard.file());
        assert_child_holds(&child, &path);
        drop(guard);
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child is still alive"
        );
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
        finish(child);
    }

    // Phase: explicit unlock also releases an inherited lock.
    {
        let (_temp, path) = fresh_lock_path();
        let guard = FileLock::open(&path, LockMode::Shared).unwrap();
        let child = child_holding(guard.file());
        assert_child_holds(&child, &path);
        guard.unlock().unwrap();
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
        finish(child);
    }

    // Phase: the lock descriptor is close-on-exec.
    {
        let (_temp, path) = fresh_lock_path();
        let file = File::from(
            rustix::fs::open(
                &path,
                rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CREATE,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap(),
        );
        assert!(
            !rustix::io::fcntl_getfd(&file)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        let guard = FileLock::exclusive(file).unwrap();
        assert!(
            rustix::io::fcntl_getfd(guard.file())
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
    }
}

#[test]
fn peers_are_admitted_by_mode_and_by_the_current_holders() {
    // Phase: try variants fail fast under contention.
    {
        let (_temp, path) = fresh_lock_path();
        let _held = FileLock::open(&path, LockMode::Exclusive).unwrap();
        let started = Instant::now();
        assert!(busy(FileLock::try_open(&path, LockMode::Exclusive)));
        assert!(busy(FileLock::try_shared(open_lock_file(&path).unwrap())));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a try variant must not wait for the holder"
        );
    }

    // Phase: shared holders coexist and exclude an exclusive one.
    {
        let (_temp, path) = fresh_lock_path();
        let first = FileLock::open(&path, LockMode::Shared).unwrap();
        let second = FileLock::try_open(&path, LockMode::Shared).unwrap();
        assert_eq!(second.mode(), LockMode::Shared);
        assert!(busy(FileLock::try_open(&path, LockMode::Exclusive)));
        drop(first);
        assert!(busy(FileLock::try_open(&path, LockMode::Exclusive)));
        drop(second);
        let exclusive = FileLock::try_open(&path, LockMode::Exclusive).unwrap();
        assert!(busy(FileLock::try_open(&path, LockMode::Shared)));
        drop(exclusive);
        drop(FileLock::try_open(&path, LockMode::Shared).unwrap());
    }

    // Phase: a blocking lock is granted once the holder drops.
    {
        let (_temp, path) = fresh_lock_path();
        let held = FileLock::open(&path, LockMode::Shared).unwrap();
        let (ready, waiting) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let file = open_lock_file(&path).unwrap();
            ready.send(()).unwrap();
            FileLock::exclusive(file).unwrap().mode()
        });
        waiting.recv().unwrap();
        drop(held);
        assert_eq!(waiter.join().unwrap(), LockMode::Exclusive);
    }

    // Phase: conversion changes what peers may hold.
    {
        let (_temp, path) = fresh_lock_path();
        let guard = FileLock::open(&path, LockMode::Shared).unwrap();
        let guard = guard.try_convert(LockMode::Exclusive).unwrap();
        assert!(busy(FileLock::try_open(&path, LockMode::Shared)));
        let guard = guard.try_convert(LockMode::Shared).unwrap();
        let peer = FileLock::try_open(&path, LockMode::Shared).unwrap();
        // A failed upgrade drops the guard, leaving only the peer's lock.
        assert!(matches!(
            guard.try_convert(LockMode::Exclusive),
            Err(TryLockError::WouldBlock)
        ));
        drop(peer);
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
    }
}

#[test]
fn an_exclusive_holder_records_its_pid_for_diagnostics() {
    let (_temp, path) = fresh_lock_path();
    assert_eq!(owner_pid(&path), None);
    let shared = FileLock::open(&path, LockMode::Shared).unwrap();
    assert!(shared.record_owner().is_err());
    drop(shared);
    std::fs::write(&path, "a much longer stale record").unwrap();
    let guard = FileLock::open(&path, LockMode::Exclusive).unwrap();
    guard.record_owner().unwrap();
    assert_eq!(owner_pid(&path), Some(std::process::id()));
}

#[test]
fn inherited_file_transfer_preserves_parent_ownership_without_leaking_descriptor() {
    let (_directory, path) = fresh_lock_path();
    let parent = FileLock::exclusive(open_lock_file(&path).unwrap()).unwrap();
    let inherited = FileLock::try_exclusive(parent.file().try_clone().unwrap()).unwrap();
    let file = inherited.into_file_preserving_lock();
    assert!(busy(FileLock::try_exclusive(
        open_lock_file(&path).unwrap()
    )));
    drop(file);
    assert!(busy(FileLock::try_exclusive(
        open_lock_file(&path).unwrap()
    )));
    drop(parent);
    assert!(FileLock::try_exclusive(open_lock_file(&path).unwrap()).is_ok());
}

#[test]
fn open_current_follows_a_replaced_name() {
    let (_directory, path) = fresh_lock_path();
    // An exclusive holder retires the lock file while a peer waits on it.
    let retiring = FileLock::open_current(&path, LockMode::Exclusive).unwrap();
    let waiter = {
        let path = path.clone();
        std::thread::spawn(move || FileLock::open_current(&path, LockMode::Shared).unwrap())
    };
    // Long enough for the waiter to have opened the old file and blocked.
    std::thread::sleep(Duration::from_millis(100));
    std::fs::remove_file(&path).unwrap();
    drop(retiring);
    let shared = waiter.join().unwrap();
    // The waiter holds the file now at the name, not the removed one, so a
    // later exclusive taker contends with it.
    assert!(path.exists());
    assert!(busy(FileLock::try_open_current(&path, LockMode::Exclusive)));
    drop(shared);
    assert!(FileLock::try_open_current(&path, LockMode::Exclusive).is_ok());
}
