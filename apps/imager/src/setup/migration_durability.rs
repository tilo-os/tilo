//! Native persistence and volume identity for durable copy receipts.
use serde::{Deserialize, Serialize};
use std::{fs::File, io};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    volume: Vec<u8>,
    inode: u64,
}

pub(super) fn supported() -> io::Result<()> {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        Ok(())
    } else {
        Err(io::Error::other(
            "durable journaled copying is unavailable on this platform; directory-entry persistence needs native implementation and verification",
        ))
    }
}

pub(super) fn identity(file: &File) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = cap_primitives::fs::Metadata::from_file(file)?;
    Ok(Identity {
        volume: volume(file)?,
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
    if unsafe { status.assume_init() }.f_type as u64 == 0x9123_683e {
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
        let len = usize::from(result[0]);
        if (1..=16).contains(&len) && result[1..=len].iter().any(|b| *b != 0) {
            let mut volume = vec![1];
            volume.extend_from_slice(&result[1..=len]);
            return Ok(volume);
        }
    }
    // Session-only fallback explicitly binds the device number to this boot.
    // A different boot refuses resume, rather than treating dev_t as durable.
    let boot = std::fs::read("/proc/sys/kernel/random/boot_id")?;
    if boot.len() != 37
        || !boot[..36]
            .iter()
            .all(|b| b.is_ascii_hexdigit() || *b == b'-')
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
    let tree = u64::from_ne_bytes(subvolume[..8].try_into().unwrap());
    if filesystem[16..32].iter().all(|b| *b == 0)
        || tree == 0
        || (tree != 5 && subvolume[296..312].iter().all(|b| *b == 0))
    {
        return Err(io::Error::other(
            "Btrfs filesystem/subvolume identity unavailable; reselect folders",
        ));
    }
    // Tree5 is the top-level filesystem tree and can lack a subvolume UUID.
    // Tree ID plus UUID separates reused inode numbers and snapshot aliases.
    let mut identity = vec![3];
    identity.extend_from_slice(&filesystem[16..32]);
    identity.extend_from_slice(&tree.to_le_bytes());
    identity.extend_from_slice(&subvolume[296..312]);
    Ok(identity)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn volume(_file: &File) -> io::Result<Vec<u8>> {
    supported()?;
    unreachable!()
}

pub(super) fn sync_file(file: &File) -> io::Result<()> {
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

pub(super) fn sync_directory(file: &File) -> io::Result<()> {
    // Linux no-follow traversal handles can be O_PATH. Reopen '.' relative to
    // the pinned directory, not through its pathname, for fsync and ioctls.
    let reopened = readable_directory(file)?;
    sync_file(&reopened)
}

pub(super) fn readable_directory(file: &File) -> io::Result<File> {
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
