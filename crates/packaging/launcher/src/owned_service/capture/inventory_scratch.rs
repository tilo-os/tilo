//! Native-only held inventory storage; no ambient temporary directory authority.
use super::contains;
mod coordinate;
mod membership;
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub const INVENTORY_SCRATCH_PATH: &str = "/run/tilo/migration/inventory-scratch";
pub const INVENTORY_SCRATCH_BYTES_ENV: &str = "TILO_INVENTORY_SCRATCH_BYTES";
pub(super) const INVENTORY_SCRATCH_FD: RawFd = 1797;

/// A directory authenticated through its caller's signed runtime/package review.
/// The held identity is checked against the exact native projection path again
/// before every use; a replacement path cannot silently move the exclusion.
pub(super) struct ReadRoot {
    directory: File,
    path: PathBuf,
    identity: (u64, u64, u64),
}
impl ReadRoot {
    pub(super) fn pin(path: &Path) -> io::Result<Self> {
        let directory = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self {
            identity: identity(&directory)?,
            directory,
            path: path.into(),
        })
    }
    pub(super) fn check(&self) -> io::Result<()> {
        let current = Self::pin(&self.path)?;
        if current.identity != self.identity || identity(&self.directory)? != self.identity {
            return Err(refuse("Signed capture directory projection changed"));
        }
        Ok(())
    }
    pub(super) fn refuse_scratch(&self, scratch: &Scratch) -> io::Result<()> {
        self.check()?;
        scratch.refuse_overlap(&self.directory)
    }
}

pub(in crate::owned_service::capture) struct Scratch {
    directory: File,
    identity: (u64, u64, u64),
    allocation_bytes: u64,
}
impl Scratch {
    pub(super) fn admit(
        directory: File,
        allocation_bytes: u64,
        uid: u32,
        source: &File,
    ) -> io::Result<Self> {
        if allocation_bytes == 0 {
            return Err(refuse("Inventory scratch allocation is empty"));
        }
        let value = Self {
            identity: identity(&directory)?,
            directory,
            allocation_bytes,
        };
        value.check(uid, source)?;
        let filesystem = filesystem(&value.directory)?;
        let available = filesystem
            .f_bavail
            .checked_mul(
                u64::try_from(filesystem.f_bsize)
                    .map_err(|_| refuse("Inventory scratch block size overflows"))?,
            )
            .ok_or_else(|| refuse("Inventory scratch capacity overflows"))?;
        if available < allocation_bytes {
            return Err(refuse("Inventory scratch capacity is insufficient"));
        }
        Ok(value)
    }
    pub(super) fn check(&self, uid: u32, source: &File) -> io::Result<()> {
        let facts = self.directory.metadata()?;
        if !facts.is_dir()
            || facts.uid() != uid
            || facts.mode() & 0o7777 != 0o700
            || facts.nlink() == 0
            || identity(&self.directory)? != self.identity
        {
            return Err(refuse(
                "Inventory scratch directory changed or is not private",
            ));
        }
        self.refuse_overlap(source)?;
        let filesystem = filesystem(&self.directory)?;
        // Accept only supported local filesystems, rejecting direct RAM,
        // overlay, network and unknown mounts. This is not physical backing
        // proof: the broker must separately exclude RAM-backed loop devices.
        let mut flags = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.directory.as_raw_fd(), flags.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = unsafe { flags.assume_init() };
        if !disk_filesystem(filesystem.f_type as u64) || flags.f_flag & libc::ST_RDONLY != 0 {
            return Err(refuse(
                "Inventory scratch is not a supported writable local filesystem",
            ));
        }
        self.check_private_membership()
    }
    pub(in crate::owned_service::capture) fn check_private_membership(&self) -> io::Result<()> {
        membership::check(&self.directory)
    }
    pub(super) fn refuse_overlap(&self, source: &File) -> io::Result<()> {
        if contains(source, &self.directory)? || contains(&self.directory, source)? {
            return Err(refuse("Inventory scratch overlaps its migration source"));
        }
        Ok(())
    }
    /// Compare actual kernel filesystem coordinates, including bind aliases.
    pub(in crate::owned_service::capture) fn refuse_mounted_overlap(
        &self,
        source: &File,
    ) -> io::Result<()> {
        if identity(&self.directory)? != self.identity {
            return Err(refuse("Inventory scratch identity changed"));
        }
        let scratch = coordinate::of(&self.directory)?;
        let source = coordinate::of(source)?;
        if scratch.overlaps(&source) {
            return Err(refuse(
                "Inventory scratch overlaps its mounted migration source",
            ));
        }
        if identity(&self.directory)? != self.identity {
            return Err(refuse("Inventory scratch identity changed"));
        }
        Ok(())
    }
    pub(super) fn descriptor_pair(&self) -> (RawFd, RawFd) {
        (self.directory.as_raw_fd(), INVENTORY_SCRATCH_FD)
    }
    pub(super) fn splice(&self, argv: &mut Vec<String>) {
        argv.splice(
            0..0,
            [
                "--dir".into(),
                "/run/tilo/migration".into(),
                "--bind-fd".into(),
                INVENTORY_SCRATCH_FD.to_string(),
                INVENTORY_SCRATCH_PATH.into(),
                "--setenv".into(),
                INVENTORY_SCRATCH_BYTES_ENV.into(),
                self.allocation_bytes.to_string(),
            ],
        );
    }
}
fn refuse(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn disk_filesystem(kind: u64) -> bool {
    matches!(kind, 0xef53 | 0x58465342 | 0x9123683e)
}
fn filesystem(directory: &File) -> io::Result<libc::statfs> {
    let mut value = std::mem::MaybeUninit::uninit();
    if unsafe { libc::fstatfs(directory.as_raw_fd(), value.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { value.assume_init() })
}
pub(super) fn identity(directory: &File) -> io::Result<(u64, u64, u64)> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::uninit();
    if unsafe {
        libc::statx(
            directory.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_INO | libc::STATX_MNT_ID,
            value.as_mut_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    if value.stx_mask & (libc::STATX_INO | libc::STATX_MNT_ID)
        != (libc::STATX_INO | libc::STATX_MNT_ID)
    {
        return Err(refuse("Inventory scratch mount identity is unavailable"));
    }
    let facts = directory.metadata()?;
    Ok((facts.dev(), value.stx_ino, value.stx_mnt_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn actual_held_directory_refuses_overlap_wrong_worker_and_changed_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("scratch");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let held = File::open(&path).unwrap();
        let uid = held.metadata().unwrap().uid();
        assert!(
            Scratch::admit(
                held.try_clone().unwrap(),
                1,
                uid.wrapping_add(1),
                &File::open(root.path()).unwrap()
            )
            .is_err()
        );
        assert_eq!(
            Scratch::admit(held.try_clone().unwrap(), 1, uid, &held)
                .err()
                .unwrap()
                .to_string(),
            "Inventory scratch overlaps its migration source"
        );
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let source = File::open(source).unwrap();
        let scratch = Scratch {
            identity: identity(&held).unwrap(),
            directory: held,
            allocation_bytes: 1,
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(scratch.check(uid, &source).is_err());
        assert!(Scratch::admit(File::open(&path).unwrap(), 0, uid, &source).is_err());
    }

    #[test]
    fn mounted_scratch_checks_actual_directory_and_file_coordinates_without_refusing_disjoint_siblings()
     {
        let root = tempfile::tempdir().unwrap();
        let scratch_path = root.path().join("scratch");
        std::fs::create_dir(&scratch_path).unwrap();
        let directory = File::open(&scratch_path).unwrap();
        let scratch = Scratch {
            identity: identity(&directory).unwrap(),
            directory,
            allocation_bytes: 1,
        };
        let sibling = root.path().join("source");
        std::fs::create_dir(&sibling).unwrap();
        let document = sibling.join("document");
        std::fs::write(&document, b"actual selected source bytes").unwrap();
        for source in [
            File::open(&sibling).unwrap(),
            File::open(&document).unwrap(),
        ] {
            scratch.refuse_mounted_overlap(&source).unwrap();
        }
        for source in [
            File::open(root.path()).unwrap(),
            File::open(&scratch_path).unwrap(),
        ] {
            assert!(scratch.refuse_mounted_overlap(&source).is_err());
        }
        let inside = scratch_path.join("document");
        std::fs::write(&inside, b"inside scratch").unwrap();
        assert!(
            scratch
                .refuse_mounted_overlap(&File::open(inside).unwrap())
                .is_err()
        );
        assert_eq!(
            std::fs::read(document).unwrap(),
            b"actual selected source bytes"
        );
    }

    #[test]
    fn held_signed_root_refuses_scratch_ancestors_descendants_and_replaced_projection() {
        let root = tempfile::tempdir().unwrap();
        let signed = root.path().join("signed-runtime");
        std::fs::create_dir(&signed).unwrap();
        let pin = ReadRoot::pin(&signed).unwrap();
        let below = signed.join("scratch");
        std::fs::create_dir(&below).unwrap();
        for path in [root.path(), signed.as_path(), below.as_path()] {
            let directory = File::open(path).unwrap();
            let scratch = Scratch {
                identity: identity(&directory).unwrap(),
                directory,
                allocation_bytes: 1,
            };
            assert!(pin.refuse_scratch(&scratch).is_err());
        }
        let separate = root.path().join("separate");
        std::fs::create_dir(&separate).unwrap();
        let directory = File::open(separate).unwrap();
        let scratch = Scratch {
            identity: identity(&directory).unwrap(),
            directory,
            allocation_bytes: 1,
        };
        pin.refuse_scratch(&scratch).unwrap();
        std::fs::rename(&signed, root.path().join("moved-runtime")).unwrap();
        std::fs::create_dir(&signed).unwrap();
        assert!(pin.refuse_scratch(&scratch).is_err());
        let alias = root.path().join("signed-alias");
        std::os::unix::fs::symlink(&signed, &alias).unwrap();
        assert!(ReadRoot::pin(&alias).is_err());
    }

    #[test]
    fn ram_network_overlay_and_unknown_filesystems_are_refused() {
        for kind in [0x01021994, 0x858458f6, 0x794c7630, 0x6969, 0, u64::MAX] {
            assert!(!disk_filesystem(kind));
        }
        for kind in [0xef53, 0x58465342, 0x9123683e] {
            assert!(disk_filesystem(kind));
        }
        assert!(super::super::archive_reserved_fd(INVENTORY_SCRATCH_FD));
        assert!(!super::super::archive_reserved_fd(INVENTORY_SCRATCH_FD + 1));
    }
}

// Shared retained source exclusion.
// It does not mint source read/write permission or disclose either ancestor.
pub(in crate::owned_service) fn refuse_retained_overlap(
    source: &File,
    input: &File,
) -> io::Result<()> {
    let source_coordinate = coordinate::of(source)?;
    let input_coordinate = coordinate::of(input)?;
    let source_meta = source.metadata()?;
    let input_meta = input.metadata()?;
    if source_coordinate.overlaps(&input_coordinate)
        || (source_meta.dev(), source_meta.ino()) == (input_meta.dev(), input_meta.ino())
    {
        return Err(refuse("Retained source overlaps another launch projection"));
    }
    Ok(())
}
