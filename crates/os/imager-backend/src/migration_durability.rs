//! Native persistence and volume identity for durable copy receipts.
use std::{fs::File, io};

pub use staging_handoff_model::migration::Identity;

pub fn supported() -> io::Result<()> {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        Ok(())
    } else {
        Err(io::Error::other(
            "durable journaled copying is unavailable on this platform; directory-entry persistence needs native implementation and verification",
        ))
    }
}

/// Returns an error if descriptor metadata or a persistent or boot-bound volume identity cannot be
/// read.
pub fn identity(file: &File) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = cap_primitives::fs::Metadata::from_file(file)?;
    Ok(Identity::new(volume(file)?, metadata.ino()))
}

/// A literal link uses its containing volume identity and its own inode, inspected without opening
/// or following the referent.
#[cfg(target_os = "linux")]
/// Rejects non-link entries and propagates directory metadata or volume-identity read failures.
pub fn link_identity(directory: &File, name: &std::ffi::OsStr) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = super::nofollow::metadata_nofollow(directory, std::path::Path::new(name))?;
    if !metadata.is_symlink() {
        return Err(io::Error::other("Expected a literal symbolic link."));
    }
    Ok(Identity::new(
        volume(&readable_directory(directory)?)?,
        metadata.ino(),
    ))
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
    // Linux UAPI BTRFS_IOC_FS_INFO: 1024 bytes, filesystem UUID at offset 16. GET_SUBVOL_INFO: 504
    // bytes, treeid at 0, subvolume UUID at 296. The kernel derives the subvolume from the fd's
    // inode, including ordinary file fds; no pathname ioctl is used.
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

pub fn sync_directory(file: &File) -> io::Result<()> {
    let reopened = readable_directory(file)?;
    sync_file(&reopened)
}

pub use tilo_migrate::pinned::readable_directory;

/// The persistent volume identity of a pinned directory itself.
pub fn directory_identity(
    directory: &tilo_migrate::pinned::PinnedDirectory,
) -> io::Result<Identity> {
    identity(&readable_directory(&directory.file)?)
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[test]
    fn malformed_volume_bindings_fail_closed_without_indexing() {
        let uuid = uuid::Uuid::from_u128(17);
        for length in 0..=42 {
            let value = Identity::new(vec![0; length], 256);
            assert!(!value.binds_filesystem(&uuid.to_string()));
            assert!(!value.binds_btrfs(&uuid.to_string(), 256));
        }
        let mut volume = vec![1];
        volume.extend_from_slice(uuid.as_bytes());
        let value = Identity::new(volume, 256);
        assert!(value.binds_filesystem(&uuid.to_string()));
        assert!(!value.binds_btrfs(&uuid.to_string(), 256));
        assert!(!value.binds_filesystem("not-a-uuid"));
    }
}
