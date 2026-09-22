//! Native persistence and volume identity for durable copy receipts.
use serde::{Deserialize, Serialize};
use std::{fs::File, io};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
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

#[cfg(not(windows))]
pub(super) fn identity(file: &File) -> io::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = cap_primitives::fs::Metadata::from_file(file)?;
    Ok(Identity {
        volume: volume(file)?,
        inode: metadata.ino(),
    })
}

// Windows supplies a 128-bit file ID. Keep every byte along with the volume
// serial; truncating it to the journal's legacy u64 inode would alias ReFS IDs.
// The journal's `volume` field carries this tagged opaque pair on Windows.
#[cfg(any(test, windows))]
fn windows_identity_from_parts(serial: u64, file_id: [u8; 16]) -> io::Result<Identity> {
    if serial == 0 || file_id.iter().all(|byte| *byte == 0) {
        return Err(io::Error::other(
            "Windows volume or file identity unavailable; reselect folders",
        ));
    }
    let mut volume = Vec::with_capacity(25);
    volume.push(4);
    volume.extend_from_slice(&serial.to_le_bytes());
    volume.extend_from_slice(&file_id);
    Ok(Identity { volume, inode: 0 })
}

#[cfg(windows)]
pub(super) fn identity(file: &File) -> io::Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the handle is live; Windows writes exactly the supplied native
    // structure on success and this function keeps the file open throughout.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful native call initialized the full FILE_ID_INFO.
    let info = unsafe { info.assume_init() };
    windows_identity_from_parts(info.VolumeSerialNumber, info.FileId.Identifier)
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

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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
mod identity_wire_tests {
    use super::*;

    #[test]
    fn windows_identity_binds_the_entire_file_id_and_volume_serial() {
        let mut file_id = [0_u8; 16];
        file_id[0] = 1;
        let original = windows_identity_from_parts(7, file_id).unwrap();
        assert_eq!(original, windows_identity_from_parts(7, file_id).unwrap());
        assert_ne!(original, windows_identity_from_parts(8, file_id).unwrap());
        file_id[15] = 1;
        assert_ne!(original, windows_identity_from_parts(7, file_id).unwrap());
    }

    #[test]
    fn windows_identity_refuses_missing_native_components() {
        assert!(windows_identity_from_parts(0, [1; 16]).is_err());
        assert!(windows_identity_from_parts(7, [0; 16]).is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    fn open_directory(path: &std::path::Path) -> File {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }

    #[test]
    fn native_windows_directory_identity_survives_reopen_and_distinguishes_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let initial = identity(&open_directory(&first)).unwrap();
        assert_eq!(initial, identity(&open_directory(&first)).unwrap());
        assert_ne!(initial, identity(&open_directory(&second)).unwrap());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires disposable native Btrfs subvolumes and snapshot supplied by the test controller"]
    fn distinguishes_btrfs_subvolumes_and_snapshot_file_identities() {
        use std::{os::unix::fs::MetadataExt, path::PathBuf};
        let base =
            PathBuf::from(std::env::var_os("QOS_BTRFS_IDENTITY_FIXTURE").expect(
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
