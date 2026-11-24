//! The one process that writes as this device.
//!
//! A device has one key and one sequence for the changes it signs: every receiver refuses a
//! second change under a number the key already gave one. Two processes signing as the device
//! keep two counts and hand out the same numbers, and two endpoints under one key push each
//! other off the network. So the device's daemon takes its key with [`take_device_key`]: an
//! exclusive lock on the key file, kept until the daemon lets go of the device. A second process
//! that takes the key meanwhile is refused, and told which process has it
//! ([`PersistError::KeyInUse`]).
//!
//! Inside the daemon, every read of the key ([`super::read_device_key`] and what calls it) is
//! answered from the key it took, without opening the file again: its workspace service and the
//! rest of the daemon sign with that one key, never a copy read on the side.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ed25519_dalek::SigningKey;
#[cfg(target_os = "linux")]
use zeroize::Zeroizing;

use super::{IdentityPaths, PersistError};

/// A key this process took, by where it is kept.
struct Taken {
    path: PathBuf,
    /// The file's device and inode, so a read through another name for the same file finds it.
    file: Option<(u64, u64)>,
    key: SigningKey,
}

/// The keys this process took, while it keeps them.
static TAKEN: Mutex<Vec<Taken>> = Mutex::new(Vec::new());

fn taken() -> std::sync::MutexGuard<'static, Vec<Taken>> {
    TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The device and inode of the file at `path`, when it names one.
#[cfg(unix)]
fn file_at(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_at(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The key at `path`, when this process took it.
pub(super) fn taken_here(path: &Path) -> Option<SigningKey> {
    let taken = taken();
    if taken.is_empty() {
        return None;
    }
    let file = file_at(path);
    taken
        .iter()
        .find(|taken| taken.path == path || (file.is_some() && taken.file == file))
        .map(|taken| taken.key.clone())
}

/// Forget the key this process took at `path`: the key there is being replaced.
pub(super) fn forget(path: &Path) {
    taken().retain(|taken| taken.path != path);
}

/// This device's key, taken by the one process that writes as the device: dropping it lets go
/// of the device's key for the next daemon.
#[must_use = "the device's key is let go of when this is dropped"]
pub struct DeviceKeyLock {
    key: SigningKey,
    path: PathBuf,
    /// The exclusive lock that makes this process the device's one writer: on the key file
    /// itself, or, for a key kept in the platform's secret store, on a lock file beside where
    /// the store's key is named ([`platform_store_lock`]).
    _lock: advisory_lock::FileLock,
}

impl DeviceKeyLock {
    /// The device's key.
    #[must_use]
    pub const fn key(&self) -> &SigningKey {
        &self.key
    }
}

impl Drop for DeviceKeyLock {
    fn drop(&mut self) {
        forget(&self.path);
    }
}

impl std::fmt::Debug for DeviceKeyLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeyLock")
            .field("device", &hex::encode(self.key.verifying_key().to_bytes()))
            .finish_non_exhaustive()
    }
}

/// Take this device's key from where `paths` keeps it, as the one process that writes as the
/// device: its daemon. While the returned lock lives, a second process taking the key is refused,
/// and reads of the key in this process answer this one.
///
/// A key file is locked itself: `flock` on Unix, `LockFileEx` on Windows. A key kept in the
/// platform's secret store (the login Keychain on macOS, the Keystore-wrapped store on Android)
/// has no file of its own, so the lock is taken on a lock file beside the place its key is
/// named, which records the process that took it.
///
/// # Errors
/// Another process took the key ([`PersistError::KeyInUse`]), this process took it already, or
/// it cannot be read as [`super::read_device_key`] reads it.
pub fn take_device_key(paths: &IdentityPaths) -> Result<DeviceKeyLock, PersistError> {
    let path = paths.device_privkey.clone();
    // One take at a time: two threads of this process opening the file would refuse each other.
    let mut taken = taken();
    let file = file_at(&path);
    if taken
        .iter()
        .any(|taken| taken.path == path || (file.is_some() && taken.file == file))
    {
        return Err(PersistError::KeyInUse {
            pid: Some(std::process::id()),
            program: program_of(std::process::id()),
        });
    }
    let (key, lock) = if paths.device_key_ownership == super::DeviceKeyOwnership::File {
        take_file(&path)?
    } else {
        let lock = platform_store_lock(&path)?;
        (super::load_device_key(paths)?, lock)
    };
    taken.push(Taken {
        path: path.clone(),
        file,
        key: key.clone(),
    });
    drop(taken);
    Ok(DeviceKeyLock {
        key,
        path,
        _lock: lock,
    })
}

/// Where the lock of a key kept in the platform's secret store is taken: beside the place its
/// key file would be, in the identity tree of the account whose store keeps it.
fn platform_store_lock_path(path: &Path) -> PathBuf {
    path.with_file_name("keypair.lock")
}

/// The exclusive lock on the lock file of a key kept in the platform's secret store, with this
/// process recorded in it so a second process is told which one took the key.
fn platform_store_lock(path: &Path) -> Result<advisory_lock::FileLock, PersistError> {
    let lock_path = platform_store_lock_path(path);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match advisory_lock::FileLock::try_open(&lock_path, advisory_lock::LockMode::Exclusive) {
        Ok(lock) => {
            lock.resource_owner()?;
            Ok(lock)
        }
        Err(advisory_lock::TryLockError::WouldBlock) => {
            let pid = advisory_lock::owner_pid(&lock_path);
            Err(PersistError::KeyInUse {
                pid,
                program: pid.and_then(program_of),
            })
        }
        Err(advisory_lock::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(unix)]
fn take_file(path: &Path) -> Result<(SigningKey, advisory_lock::FileLock), PersistError> {
    super::protect_secret_process()?;
    let file = super::open_regular_without_symlinks(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!("cannot open secret key {}: {error}", path.display()),
        ))
    })?;
    let opened = {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        (metadata.dev(), metadata.ino())
    };
    let lock = match advisory_lock::FileLock::try_exclusive(file) {
        Ok(lock) => lock,
        Err(advisory_lock::TryLockError::WouldBlock) => {
            let pid = lock_owner(opened);
            return Err(PersistError::KeyInUse {
                pid,
                program: pid.and_then(program_of),
            });
        }
        Err(advisory_lock::TryLockError::Error(error)) => return Err(error.into()),
    };
    let key = super::signing_key_from_open_file(
        lock.file(),
        path,
        0o007,
        super::SecretOwner::RootOrReader,
    )?;
    // As a read of the key does: this process is non-dumpable now, and proves to the person's
    // Secret Service that it took the device's key instead.
    #[cfg(target_os = "linux")]
    tilo_secrets::arm_device_proof(Zeroizing::new(key.to_bytes()).as_ref());
    Ok((key, lock))
}

/// On Windows the key file is locked with `LockFileEx` over the whole file: a second process
/// cannot lock it, nor read the bytes the lock covers, while the daemon keeps it. The key is read
/// through the handle that took the lock. Windows does not say which process has a file locked.
#[cfg(not(unix))]
fn take_file(path: &Path) -> Result<(SigningKey, advisory_lock::FileLock), PersistError> {
    let file = fs::File::open(path).map_err(|error| {
        PersistError::Io(io::Error::new(
            error.kind(),
            format!("cannot open secret key {}: {error}", path.display()),
        ))
    })?;
    let lock = match advisory_lock::FileLock::try_exclusive(file) {
        Ok(lock) => lock,
        Err(advisory_lock::TryLockError::WouldBlock) => {
            return Err(PersistError::KeyInUse {
                pid: None,
                program: None,
            });
        }
        Err(advisory_lock::TryLockError::Error(error)) => return Err(error.into()),
    };
    let key = super::signing_key_from_open_file(
        lock.file(),
        path,
        0o007,
        super::SecretOwner::RootOrReader,
    )?;
    Ok((key, lock))
}

/// The process whose lock on the file `(device, inode)` refuses this one, as the kernel lists
/// it: `/proc/locks` names each `flock` lock's process and file.
#[cfg(target_os = "linux")]
fn lock_owner((device, inode): (u64, u64)) -> Option<u32> {
    // The kernel's `MAJOR:MINOR:INODE`, its numbers in hex, the inode's in decimal.
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & !0xfff);
    let minor = (device & 0xff) | ((device >> 12) & !0xff);
    let file = format!("{major:02x}:{minor:02x}:{inode}");
    let locks = fs::read_to_string("/proc/locks").ok()?;
    locks.lines().find_map(|line| {
        let mut fields = line.split_whitespace().skip(1);
        // A waiting lock is listed as `->` before its kind; only a granted one refuses.
        if fields.next()? != "FLOCK" {
            return None;
        }
        let _advisory = fields.next()?;
        if fields.next()? != "WRITE" {
            return None;
        }
        let pid = fields.next()?.parse::<u32>().ok()?;
        (fields.next()? == file && pid != 0).then_some(pid)
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn lock_owner(_file: (u64, u64)) -> Option<u32> {
    None
}

/// The name of the program process `pid` runs, when this system says.
fn program_of(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// How [`PersistError::KeyInUse`] names the process that took the key.
pub(super) fn in_use_by(pid: Option<u32>, program: Option<&str>) -> String {
    match (pid, program) {
        (Some(pid), Some(program)) => format!("{pid} ({program})"),
        (Some(pid), None) => pid.to_string(),
        (None, _) => "another process".to_owned(),
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// The child this test starts takes the key under the root this names, says so, and keeps
    /// it until its standard input ends.
    const CHILD: &str = "TILO_TEST_TAKE_DEVICE_KEY_UNDER";
    /// Set for a child that takes a key kept in the platform's secret store.
    const CHILD_STORE: &str = "TILO_TEST_TAKE_DEVICE_KEY_FROM_STORE";
    /// What the child says once it took the key.
    const SAID_TAKEN: &str = "the device key is taken";
    const FILE_TEST: &str =
        "persist::one_writer::tests::a_second_process_taking_the_key_is_refused_naming_the_first";
    const STORE_TEST: &str = "persist::one_writer::tests::a_second_process_taking_a_key_the_platform_store_keeps_is_refused_naming_the_first";

    /// A key under `root`, in a file or in the platform's secret store, which this thread's
    /// store keeps for the test.
    fn device_key_under(root: &Path, store: bool) -> IdentityPaths {
        let paths = IdentityPaths::under(root);
        let paths = if store {
            paths.with_device_key_ownership(super::super::DeviceKeyOwnership::PlatformStore)
        } else {
            paths
        };
        super::super::load_or_create_device_key(&paths).unwrap();
        paths
    }

    /// The child's side: take the key, say so, keep it until standard input ends.
    fn take_in_child(root: std::ffi::OsString) {
        let store = std::env::var_os(CHILD_STORE).is_some();
        let _memory = store.then(|| tilo_secrets::scope_thread(tilo_secrets::memory_backend()));
        let paths = device_key_under(Path::new(&root), store);
        let lock = take_device_key(&paths).expect("the first process takes the key");
        assert_eq!(
            super::super::load_device_key(&paths).unwrap().to_bytes(),
            lock.key().to_bytes(),
            "a read in the process that took the key answers that key"
        );
        println!("{SAID_TAKEN}");
        let mut rest = String::new();
        let _ = io::Read::read_to_string(&mut io::stdin(), &mut rest);
        drop(lock);
    }

    /// One process writes as the device: a second one that takes its key, kept where `store`
    /// says, is refused and told which process has it, and takes it once that process let go.
    fn second_process_is_refused(test: &str, store: bool) {
        use std::io::{BufRead as _, BufReader};
        use std::process::{Command, Stdio};
        use tilo_test_env::SpawnUnderEnvLock as _;

        let _memory = store.then(|| tilo_secrets::scope_thread(tilo_secrets::memory_backend()));
        let root = tempfile::tempdir().unwrap();
        let paths = device_key_under(root.path(), store);
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(CHILD, root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if store {
            child.env(CHILD_STORE, "1");
        }
        let mut child = child.spawn_locked().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(
            // The test harness names the test on the line the child's word ends.
            lines.any(|line| line.is_ok_and(|line| line.trim_end().ends_with(SAID_TAKEN))),
            "the first process took the key"
        );

        let refused = take_device_key(&paths).expect_err("a second process is refused");
        assert!(
            matches!(refused, PersistError::KeyInUse { pid: Some(pid), .. } if pid == child.id()),
            "{refused:?}"
        );
        let said = refused.to_string();
        assert!(
            said.starts_with(&format!(
                "this device's key is already in use by {}",
                child.id()
            )),
            "{said}"
        );

        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        let lock = take_device_key(&paths).expect("the key is taken once the first let go");
        assert!(
            matches!(take_device_key(&paths), Err(PersistError::KeyInUse { pid: Some(pid), .. }) if pid == std::process::id()),
            "this process takes the key once"
        );
        drop(lock);
        drop(take_device_key(&paths).expect("a key let go of is taken again"));
    }

    #[test]
    fn a_second_process_taking_the_key_is_refused_naming_the_first() {
        if let Some(root) = std::env::var_os(CHILD) {
            take_in_child(root);
            return;
        }
        second_process_is_refused(FILE_TEST, false);
    }

    /// The login Keychain and the Android Keystore have no file to lock: the lock beside the
    /// key's place refuses a second process all the same.
    #[test]
    fn a_second_process_taking_a_key_the_platform_store_keeps_is_refused_naming_the_first() {
        if let Some(root) = std::env::var_os(CHILD) {
            take_in_child(root);
            return;
        }
        second_process_is_refused(STORE_TEST, true);
    }
}
