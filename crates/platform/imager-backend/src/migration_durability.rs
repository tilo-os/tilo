//! Native persistence and volume identity for durable copy receipts.
use serde::{Deserialize, Serialize};
use std::{fs::File, io};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    volume: Vec<u8>,
    inode: u64,
}

impl Identity {
    /// Match a live filesystem UUID without accepting boot-local device IDs.
    #[cfg(target_os = "linux")]
    pub fn binds_filesystem(&self, filesystem_uuid: &str) -> bool {
        let durable = (self.volume.len() == 17 && self.volume.first() == Some(&1))
            || (self.volume.len() == 41 && self.volume.first() == Some(&3));
        durable
            && uuid::Uuid::parse_str(filesystem_uuid)
                .is_ok_and(|id| self.volume.get(1..17) == Some(id.as_bytes().as_slice()))
    }

    /// Validate a persisted Btrfs source binding before using it as a hint.
    pub fn binds_btrfs(&self, filesystem_uuid: &str, inode: u64) -> bool {
        self.inode == inode
            && self.volume.len() == 41
            && self.volume.first() == Some(&3)
            && uuid::Uuid::parse_str(filesystem_uuid)
                .is_ok_and(|id| self.volume.get(1..17) == Some(id.as_bytes().as_slice()))
    }
}

/// # Errors
/// Returns an error on platforms without verified directory-entry persistence support.
pub fn supported() -> io::Result<()> {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        Ok(())
    } else {
        Err(io::Error::other(
            "durable journaled copying is unavailable on this platform; directory-entry persistence needs native implementation and verification",
        ))
    }
}

/// # Errors
/// Returns an error if descriptor metadata or a persistent/boot-bound volume identity cannot be read.
pub fn identity(file: &File) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = cap_primitives::fs::Metadata::from_file(file)?;
    Ok(Identity {
        volume: volume(file)?,
        inode: metadata.ino(),
    })
}

/// A literal link uses its containing volume identity and its own inode. The
/// link is inspected without opening or following the referent.
#[cfg(target_os = "linux")]
/// # Errors
/// Rejects non-link entries and propagates directory metadata or volume-identity read failures.
pub fn link_identity(directory: &File, name: &std::ffi::OsStr) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = super::nofollow::metadata_nofollow(directory, std::path::Path::new(name))?;
    if !metadata.is_symlink() {
        return Err(io::Error::other("Expected a literal symbolic link."));
    }
    Ok(Identity {
        volume: volume(&readable_directory(directory)?)?,
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "macos")]
fn volume(file: &File) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut result = [0_u8; 20];
    // SAFETY: live descriptor and correctly sized attrlist and output buffer.
    if unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&raw mut attributes).cast(),
            result.as_mut_ptr().cast(),
            result.len(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if u32::from_ne_bytes([result[0], result[1], result[2], result[3]]) != 20
        || result[4..].iter().all(|b| *b == 0)
    {
        return Err(io::Error::other(
            "volume UUID unavailable; reselect folders",
        ));
    }
    Ok(result[4..].to_vec())
}

#[cfg(target_os = "linux")]
fn volume(file: &File) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: valid fd and writable storage for the complete native structure.
    if unsafe { libc::fstatfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs initialized the structure.
    if u64::try_from(unsafe { status.assume_init() }.f_type).ok() == Some(0x9123_683e) {
        return btrfs_volume(file);
    }
    // Linux UAPI fsuuid2: u8 len; u8 uuid[16]. _IOR(0x15, 0, fsuuid2).
    let mut result = [0_u8; 17];
    // SAFETY: live readable descriptor; kernel writes at most 17 bytes.
    if unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            0x8011_1500 as libc::c_ulong,
            result.as_mut_ptr(),
        )
    } == 0
    {
        let (length, uuid) = result
            .split_first()
            .ok_or_else(|| io::Error::other("Missing volume identity"))?;
        let len = usize::from(*length);
        if let Some(uuid) = uuid
            .get(..len)
            .filter(|uuid| !uuid.is_empty() && uuid.iter().any(|b| *b != 0))
        {
            let mut volume = vec![1];
            volume.extend_from_slice(uuid);
            return Ok(volume);
        }
    }
    // Session-only fallback explicitly binds the device number to this boot.
    // A different boot refuses resume, rather than treating dev_t as durable.
    let boot = std::fs::read("/proc/sys/kernel/random/boot_id")?;
    if boot.len() != 37
        || !boot
            .get(..36)
            .is_some_and(|uuid| uuid.iter().all(|b| b.is_ascii_hexdigit() || *b == b'-'))
    {
        return Err(io::Error::other(
            "boot identity unavailable; reselect folders",
        ));
    }
    let mut volume = vec![2];
    volume.extend(boot);
    volume.extend(file.metadata()?.dev().to_le_bytes());
    Ok(volume)
}

#[cfg(target_os = "linux")]
fn btrfs_volume(file: &File) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    // Linux UAPI BTRFS_IOC_FS_INFO: 1024 bytes, filesystem UUID at offset16.
    // GET_SUBVOL_INFO: 504 bytes, treeid at0, subvolume UUID at296. The kernel
    // derives the subvolume from the fd's inode, including ordinary file fds.
    // Native Fedora's headers verify both layouts; no pathname ioctl is used.
    let mut filesystem = [0_u8; 1024];
    let mut subvolume = [0_u8; 504];
    // SAFETY: live readable fd and correctly sized, writable UAPI buffers.
    if unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            0x8400_941f as libc::c_ulong,
            filesystem.as_mut_ptr(),
        )
    } != 0
        || unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                0x81f8_943c as libc::c_ulong,
                subvolume.as_mut_ptr(),
            )
        } != 0
    {
        return Err(io::Error::other(format!(
            "Btrfs persistent subvolume identity unavailable; reselect folders: {}",
            io::Error::last_os_error()
        )));
    }
    let tree_bytes = subvolume
        .get(..8)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| io::Error::other("Missing Btrfs tree identity"))?;
    let tree = u64::from_ne_bytes(tree_bytes);
    let filesystem_uuid = filesystem
        .get(16..32)
        .ok_or_else(|| io::Error::other("Missing Btrfs filesystem identity"))?;
    let subvolume_uuid = subvolume
        .get(296..312)
        .ok_or_else(|| io::Error::other("Missing Btrfs subvolume identity"))?;
    if filesystem_uuid.iter().all(|b| *b == 0)
        || tree == 0
        || (tree != 5 && subvolume_uuid.iter().all(|b| *b == 0))
    {
        return Err(io::Error::other(
            "Btrfs filesystem/subvolume identity unavailable; reselect folders",
        ));
    }
    // Tree5 is the top-level filesystem tree and can lack a subvolume UUID.
    // Tree ID plus UUID separates reused inode numbers and snapshot aliases.
    let mut identity = vec![3];
    identity.extend_from_slice(filesystem_uuid);
    identity.extend_from_slice(&tree.to_le_bytes());
    identity.extend_from_slice(subvolume_uuid);
    Ok(identity)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn volume(_file: &File) -> io::Result<Vec<u8>> {
    supported()?;
    unreachable!()
}

/// # Errors
/// Propagates native sync failures, including full filesystem-cache flush failures on macOS.
pub fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all()?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: F_FULLFSYNC takes no third argument and uses a live fd.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// # Errors
/// Returns an error if the held directory cannot be reopened for reading or synchronized.
pub fn sync_directory(file: &File) -> io::Result<()> {
    // Linux no-follow traversal handles can be O_PATH. Reopen '.' relative to
    // the pinned directory, not through its pathname, for fsync and ioctls.
    let reopened = readable_directory(file)?;
    sync_file(&reopened)
}

/// # Errors
/// Returns the native open error if `.` cannot be reopened relative to the held directory.
pub fn readable_directory(file: &File) -> io::Result<File> {
    use cap_fs_ext::OpenOptionsMaybeDirExt;
    let mut options = cap_primitives::fs::OpenOptions::new();
    options.read(true).maybe_dir(true);
    cap_primitives::fs::open(file, std::path::Path::new("."), &options)
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[test]
    fn malformed_volume_bindings_fail_closed_without_indexing() {
        let uuid = uuid::Uuid::from_u128(17);
        for length in 0..=42 {
            let value = Identity {
                volume: vec![0; length],
                inode: 256,
            };
            assert!(!value.binds_filesystem(&uuid.to_string()));
            assert!(!value.binds_btrfs(&uuid.to_string(), 256));
        }
        let mut volume = vec![1];
        volume.extend_from_slice(uuid.as_bytes());
        let value = Identity { volume, inode: 256 };
        assert!(value.binds_filesystem(&uuid.to_string()));
        assert!(!value.binds_btrfs(&uuid.to_string(), 256));
        assert!(!value.binds_filesystem("not-a-uuid"));
    }

    #[test]
    #[ignore = "requires disposable native Btrfs subvolumes and snapshot supplied by the test controller"]
    fn distinguishes_btrfs_subvolumes_and_snapshot_file_identities() {
        use std::{os::unix::fs::MetadataExt, path::PathBuf};
        let base =
            PathBuf::from(std::env::var_os("TILO_BTRFS_IDENTITY_FIXTURE").expect(
                "set fixture with one, two and snapshot subvolumes; one/file before snapshot",
            ));
        let one = File::open(base.join("one")).unwrap();
        let two = File::open(base.join("two")).unwrap();
        assert_eq!(one.metadata().unwrap().ino(), 256);
        assert_eq!(two.metadata().unwrap().ino(), 256);
        let first_identity = identity(&one).unwrap();
        assert_ne!(
            first_identity.volume.first(),
            Some(&2),
            "Btrfs must have persistent identity, not boot/device fallback"
        );
        assert_ne!(first_identity, identity(&two).unwrap());
        let original = File::open(base.join("one/file")).unwrap();
        let snapshot = File::open(base.join("snapshot/file")).unwrap();
        assert_eq!(
            original.metadata().unwrap().ino(),
            snapshot.metadata().unwrap().ino()
        );
        assert_ne!(identity(&original).unwrap(), identity(&snapshot).unwrap());
        assert_eq!(
            identity(&original).unwrap(),
            identity(&File::open(base.join("one/file")).unwrap()).unwrap()
        );
    }
}
