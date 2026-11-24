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
    #[cfg(unix)]
    _lock: Option<advisory_lock::FileLock>,
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
/// A key kept in the platform's secret store (macOS, Android) is read by the user's own runtime
/// alone, and has no file to lock.
///
/// # Errors
/// Another process took the key ([`PersistError::KeyInUse`]), this process took it already, or
/// it cannot be read as [`super::read_device_key`] reads it.
pub fn take_device_key(paths: &IdentityPaths) -> Result<DeviceKeyLock, PersistError> {
    let path = paths.device_privkey.clone();
    if paths.device_key_ownership != super::DeviceKeyOwnership::File {
        return Ok(DeviceKeyLock {
            key: super::load_device_key(paths)?,
            path,
            #[cfg(unix)]
            _lock: None,
        });
    }
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
    #[cfg_attr(not(unix), expect(unused_variables, reason = "no file lock off Unix"))]
    let (key, lock) = take_file(&path)?;
    taken.push(Taken {
        path: path.clone(),
        file,
        key: key.clone(),
    });
    drop(taken);
    Ok(DeviceKeyLock {
        key,
        path,
        #[cfg(unix)]
        _lock: lock,
    })
}

#[cfg(unix)]
fn take_file(path: &Path) -> Result<(SigningKey, Option<advisory_lock::FileLock>), PersistError> {
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
    Ok((key, Some(lock)))
}

/// Where no file lock reaches another process, the key is read as any reader reads it.
#[cfg(not(unix))]
fn take_file(path: &Path) -> Result<(SigningKey, ()), PersistError> {
    let key = super::signing_key_from_file(path, 0o007, super::SecretOwner::RootOrReader)?;
    Ok((key, ()))
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
    const TEST: &str =
        "persist::one_writer::tests::a_second_process_taking_the_key_is_refused_naming_the_first";

    fn device_key_under(root: &Path) -> Result<IdentityPaths, PersistError> {
        let paths = IdentityPaths::under(root);
        super::super::load_or_create_device_key(&paths)?;
        Ok(paths)
    }

    /// One process writes as the device: a second one that takes its key is refused and told
    /// which process has it, and takes it once that process let go. Inside the first, a read
    /// of the key answers the key it took.
    #[test]
    fn a_second_process_taking_the_key_is_refused_naming_the_first() {
        use std::io::{BufRead as _, BufReader};
        use std::process::{Command, Stdio};
        use tilo_test_env::SpawnUnderEnvLock as _;

        if let Some(root) = std::env::var_os(CHILD) {
            let paths = IdentityPaths::under(PathBuf::from(root));
            let lock = take_device_key(&paths).expect("the first process takes the key");
            assert_eq!(
                super::super::read_device_key(&paths.device_privkey)
                    .unwrap()
                    .to_bytes(),
                lock.key().to_bytes(),
                "a read in the process that took the key answers that key"
            );
            println!("taken");
            let mut rest = String::new();
            let _ = io::Read::read_to_string(&mut io::stdin(), &mut rest);
            drop(lock);
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let paths = device_key_under(root.path()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_locked()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(
            lines.any(|line| line.is_ok_and(|line| line.trim() == "taken")),
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
}
