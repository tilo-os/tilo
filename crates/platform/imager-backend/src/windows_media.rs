//! Private Windows recovery-to-media handoff. No identity or replace authority.
use account_custody::hints::media::Prepared;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsMedia {
    pub prepared: Prepared,
    /// Receipt belongs to independent recovery storage, not the USB being erased.
    receipt: PathBuf,
}
impl WindowsMedia {
    /// Bind prepared recovery data to its independently stored receipt.
    ///
    /// # Errors
    /// Returns an error if the receipt path is not absolute, is not a regular
    /// file, exceeds 1 MiB, cannot be read, or fails receipt verification.
    pub fn new(prepared: Prepared, receipt: PathBuf) -> io::Result<Self> {
        let value = Self { prepared, receipt };
        value.check_receipt()?;
        Ok(value)
    }
    fn check_receipt(&self) -> io::Result<()> {
        if !self.receipt.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let metadata = std::fs::symlink_metadata(&self.receipt)?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let file = std::fs::File::open(&self.receipt)?;
        if !file.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.prepared.verify_receipt(&bytes)
    }
    /// Recheck the receipt and attest that it is not stored on the target disk.
    ///
    /// # Errors
    /// Returns an error if receipt verification fails, native storage inspection
    /// fails, or the receipt's independence from the target cannot be established.
    /// Unsupported platforms and unknown or stacked storage mappings fail closed.
    pub fn validate(&self, target: impl Into<crate::media_model::TargetDrive>) -> io::Result<()> {
        self.check_receipt()?;
        independent_storage(&self.receipt, &target.into())
    }
}

#[cfg(windows)]
fn independent_storage(path: &Path, target: &crate::media_model::TargetDrive) -> io::Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW,
            GetVolumePathNameW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
        },
        System::IO::DeviceIoControl,
    };
    let number: u32 = target
        .name
        .strip_prefix("PhysicalDrive")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("invalid Windows media disk"))?;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut mount = [0u16; 32768];
    let mut volume = [0u16; 1024];
    // SAFETY: all strings are terminated and buffers carry their exact lengths.
    unsafe {
        if GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0
            || GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let end = volume
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(|| io::Error::other("invalid volume name"))?;
        if end == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if volume[end - 1] == b'\\' as u16 {
            volume[end - 1] = 0;
        }
        let handle = CreateFileW(
            volume.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // VOLUME_DISK_EXTENTS has a DWORD count, alignment padding, then 24-byte
        // DISK_EXTENT entries. Refuse spanning/oversized/unknown layouts whole.
        let mut extents = [0u64; 128];
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            ptr::null(),
            0,
            extents.as_mut_ptr().cast(),
            std::mem::size_of_val(&extents) as u32,
            &mut returned,
            ptr::null_mut(),
        );
        let error = io::Error::last_os_error();
        CloseHandle(handle);
        if ok == 0 {
            return Err(error);
        }
        let count = extents[0] as u32 as usize;
        if count == 0 || count > 42 || (returned as usize) < 8 + count * 24 {
            return Err(io::Error::other("cannot attest recovery storage"));
        }
        for index in 0..count {
            if extents[1 + index * 3] as u32 == number {
                return Err(io::Error::other(
                    "Recovery copies must not be on the USB being flashed",
                ));
            }
        }
    }
    Ok(())
}
#[cfg(unix)]
fn independent_storage(path: &Path, target: &crate::media_model::TargetDrive) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let recovery = std::fs::metadata(path)?;
    let disk = std::fs::metadata(target.dev_path())?;
    // Linux partition dev_t differs from its parent disk: compare sysfs parent
    // identity, not just dev_t equality. Unknown topology must fail closed.
    if disk.file_type().is_block_device() {
        #[cfg(target_os = "linux")]
        {
            let major = libc::major(recovery.dev());
            let minor = libc::minor(recovery.dev());
            let source = plain_disk(std::fs::canonicalize(format!(
                "/sys/dev/block/{major}:{minor}"
            ))?)?;
            let destination = plain_disk(std::fs::canonicalize(format!(
                "/sys/class/block/{}",
                target.name
            ))?)?;
            if source != destination {
                return Ok(());
            }
        }
    }
    Err(io::Error::other(
        "Recovery storage is on the target disk or cannot be attested",
    ))
}
// A virtual block device's sysfs location says nothing about its backing
// storage. Refuse stacked/unknown mappings instead of calling them independent.
#[cfg(target_os = "linux")]
fn plain_disk(mut sys: PathBuf) -> io::Result<PathBuf> {
    if sys.join("partition").try_exists()? {
        sys.pop();
    }
    if !sys.join("device").try_exists()? || std::fs::read_dir(sys.join("slaves"))?.next().is_some()
    {
        return Err(io::Error::other(
            "Recovery storage physical mapping is unverified",
        ));
    }
    Ok(sys)
}

#[cfg(not(any(unix, windows)))]
fn independent_storage(_: &Path, _: &crate::media_model::TargetDrive) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn stacked_recovery_storage_cannot_claim_independence_from_its_backing_disk() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("disk");
        let partition = disk.join("partition1");
        std::fs::create_dir_all(disk.join("device")).unwrap();
        std::fs::create_dir_all(disk.join("slaves")).unwrap();
        std::fs::create_dir(&partition).unwrap();
        std::fs::write(partition.join("partition"), "1").unwrap();
        assert_eq!(plain_disk(partition).unwrap(), disk);

        let mapped = directory.path().join("dm-0");
        std::fs::create_dir_all(mapped.join("slaves")).unwrap();
        std::os::unix::fs::symlink(&disk, mapped.join("slaves/backing")).unwrap();
        assert!(plain_disk(mapped.clone()).is_err());
        // Even a device link does not make a stacked mapping a plain disk.
        std::fs::create_dir(mapped.join("device")).unwrap();
        assert!(plain_disk(mapped).is_err());
    }
}
