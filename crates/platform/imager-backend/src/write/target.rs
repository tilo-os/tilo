//! Native target identity, exclusive custody and platform preparation.
/// Bind an already-open block descriptor to the freshly reviewed device node.
///
/// # Errors
/// Rejects stale inventory, non-block descriptors/nodes, changed inode/device identity, or native metadata errors.
#[cfg(target_os = "linux")]
pub fn attest_target_descriptor(target: &TargetDrive, file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    revalidate_target(target)?;
    let held = file.metadata()?;
    let current = std::fs::symlink_metadata(target.dev_path())?;
    if !held.file_type().is_block_device()
        || !current.file_type().is_block_device()
        || held.rdev() != current.rdev()
        || held.ino() != current.ino()
        || held.dev() != current.dev()
    {
        return Err(io::Error::other(
            "opened descriptor does not identify the reviewed block device",
        ));
    }
    revalidate_target(target)
}

use super::JobResult;
use crate::media_model::TargetDrive;
use sha2::{Digest, Sha256};
#[cfg(any(
    test,
    feature = "test-support",
    target_os = "macos",
    target_os = "windows"
))]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::{fs::OpenOptions, path::Path};
use std::{io, time::Duration};

/// # Errors
/// Rejects failed inventory probes, missing targets, or changed identity, size, model, removability or system-disk status.
pub fn revalidate_target(selected: &TargetDrive) -> io::Result<()> {
    let current = crate::probe::probe()
        .and_then(|drives| drives.into_iter().find(|drive| drive.name == selected.name))
        .ok_or_else(|| io::Error::other("the selected device is no longer present"))?;
    if !target_identity_matches(selected, &current) {
        return Err(io::Error::other(
            "device identity, capacity, or system-disk status changed",
        ));
    }
    Ok(())
}

pub fn target_identity_matches(selected: &TargetDrive, current: &TargetDrive) -> bool {
    let stable_id_changed = match (&selected.stable_id, &current.stable_id) {
        (Some(expected), Some(actual)) => expected != actual,
        (Some(_), None) => true,
        (None, _) => false,
    };
    !stable_id_changed
        && current.name == selected.name
        && current.size == selected.size
        && current.model == selected.model
        && current.removable == selected.removable
        && (selected.is_system || !current.is_system)
}

/// # Errors
/// Returns an error if mount-state enumeration or unmounting fails, times out, or exceeds bounded output.
#[cfg(target_os = "linux")]
pub fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    const PREPARE_OUTPUT_LIMIT: usize = 1024 * 1024;
    const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
    let device_path = target.dev_path();
    let out = bounded_command::run(
        "lsblk",
        &["-lnpo", "NAME,MOUNTPOINT", &device_path],
        None,
        PREPARE_OUTPUT_LIMIT,
        PREPARE_TIMEOUT,
    )
    .map_err(|error| io::Error::other(format!("lsblk probe failed: {error}")))?;
    if !out.status.success() {
        return Err(io::Error::other("lsblk could not inspect mount state"));
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            let out = bounded_command::run(
                "umount",
                &[device],
                None,
                PREPARE_OUTPUT_LIMIT,
                PREPARE_TIMEOUT,
            )
            .map_err(|error| io::Error::other(format!("unmount failed: {error}")))?;
            if !out.status.success() {
                return Err(io::Error::other(format!("failed to unmount {device}")));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let mut command = Command::new("diskutil");
    command.args(["unmountDisk", &format!("/dev/{}", target.name)]);
    let status = run_finite_status(command, Duration::from_secs(10))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("diskutil unmountDisk failed"))
}

#[cfg(target_os = "windows")]
pub fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let number = target
        .name
        .strip_prefix("PhysicalDrive")
        .ok_or_else(|| io::Error::other("invalid Windows physical-drive name"))?;
    let script = format!("Set-Disk -Number {number} -IsOffline $true -ErrorAction Stop");
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let status = run_finite_status(command, Duration::from_secs(10))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("Windows could not take the selected disk offline"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn prepare_target(_target: &TargetDrive) -> io::Result<()> {
    Err(io::Error::other(
        "raw media writing is unsupported on this platform",
    ))
}

#[cfg(target_os = "macos")]
pub fn finish_target(target: &TargetDrive, result: &JobResult) {
    // Keep a failed target attached so the user can retry without physically
    // reconnecting it. It remains unmounted from `prepare_target`, which is
    // also the safe state for another raw-write attempt. Successful media is
    // ejected so it can be removed immediately.
    if macos_should_eject(result) {
        let mut command = Command::new("diskutil");
        command.args(["eject", &format!("/dev/{}", target.name)]);
        let _ = run_finite_status(command, Duration::from_secs(10));
    }
}

#[cfg(any(test, feature = "test-support", target_os = "macos"))]
pub fn macos_should_eject(result: &JobResult) -> bool {
    result.is_success()
}

#[cfg(target_os = "windows")]
pub fn finish_target(target: &TargetDrive, _result: &JobResult) {
    // Set-Disk took the device offline before opening its raw handle. Always
    // restore it, including after a failed attempt, so it remains usable.
    if let Some(number) = target.name.strip_prefix("PhysicalDrive") {
        let script =
            format!("Set-Disk -Number {number} -IsOffline $false -ErrorAction SilentlyContinue");
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        let _ = run_finite_status(command, Duration::from_secs(10));
    }
}

#[cfg(any(
    test,
    feature = "test-support",
    target_os = "macos",
    target_os = "windows"
))]
/// # Errors
/// Returns spawn/wait failures or a timeout error after terminating and reaping the command.
pub fn run_finite_status(
    mut command: Command,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    bounded_command::wait_or_kill(&mut child, timeout)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "platform media command timed out"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn finish_target(_target: &TargetDrive, _result: &JobResult) {}

/// A stable hardware identity is the rendezvous key shared by separately
/// elevated helpers, falling back to the canonical device path when probing
/// cannot provide one. The helper owns the lock, so the kernel releases it if
/// either the UI or helper crashes.
pub fn target_lock_name(target: &TargetDrive) -> String {
    let identity = target
        .stable_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!("path:{}", target.dev_path()),
            |id| format!("id:{id}"),
        );
    hex::encode(Sha256::digest(identity.as_bytes()))
}

#[cfg(unix)]
pub type TargetWriteLock = advisory_lock::FileLock;

#[cfg(unix)]
/// # Errors
/// Returns native lock-file open errors, lock failures, or `WouldBlock` when another writer holds the lock.
pub fn try_lock_file(path: &Path) -> io::Result<TargetWriteLock> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    advisory_lock::FileLock::try_exclusive(file).map_err(|error| match error {
        advisory_lock::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "another Tilo Imager helper is already using this drive",
        ),
        advisory_lock::TryLockError::Error(error) => error,
    })
}

/// Keep the existing privileged writer lock through reviewed mounted output.
/// This grants no raw descriptor or filesystem authority to its callback.
///
/// # Errors
/// Rejects non-root callers, unsafe/contended writer locks and stale targets, and propagates callback errors.
#[cfg(target_os = "linux")]
pub fn with_locked_media_root<T>(
    target: &TargetDrive,
    callback: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    // SAFETY: credential getters observe only this process.
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let _lock = acquire_target_write_lock(target)?;
    revalidate_target(target)?;
    callback()
}

/// # Errors
/// Rejects unsafe lock-directory ownership/permissions and propagates native directory, file-open and exclusive-lock failures.
#[cfg(unix)]
pub fn acquire_target_write_lock(target: &TargetDrive) -> io::Result<TargetWriteLock> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[cfg(target_os = "linux")]
    let directory = Path::new("/run/lock/tilo-imager");
    #[cfg(not(target_os = "linux"))]
    let directory = Path::new("/var/run/tilo-imager");

    if !directory.exists() {
        std::fs::create_dir(directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = directory.symlink_metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the privileged Tilo Imager lock directory is not root-owned and private",
        ));
    }
    try_lock_file(&directory.join(target_lock_name(target)))
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct TargetWriteLock(*mut std::ffi::c_void);

#[cfg(target_os = "windows")]
impl Drop for TargetWriteLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        mutex_attributes: *const std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
    fn ReleaseMutex(handle: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "windows")]
pub fn acquire_target_write_lock(target: &TargetDrive) -> io::Result<TargetWriteLock> {
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name = format!("Global\\Tilo-Imager-target-{}", target_lock_name(target));
    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another Tilo Imager helper is already using this drive",
        ));
    }
    Ok(TargetWriteLock(handle))
}

#[cfg(not(any(unix, target_os = "windows")))]
pub struct TargetWriteLock;

#[cfg(not(any(unix, target_os = "windows")))]
pub fn acquire_target_write_lock(_target: &TargetDrive) -> io::Result<TargetWriteLock> {
    Err(io::Error::other(
        "exclusive media writes are unsupported on this platform",
    ))
}
