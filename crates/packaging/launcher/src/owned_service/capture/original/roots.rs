//! Finite root-service source projections, before source credential application.
//! Inputs must come from retained protected source contexts, never public paths.
use super::{
    AsRawFd, Deserialize, Facts, File, FromRawFd, MAX_BYTES, MetadataExt, OpenOptionsExt, RawFd,
    SEALS, Seek, Serialize, Write, io, readonly, refused, root,
};
use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};
mod detached;
mod protocol;
pub(super) use detached::DetachedRoots;
pub use protocol::{
    OriginalMountedBinding, OriginalMountedIdentity, VerifiedOriginalMountedRoots,
    original_mounted_table_sha256, verify_original_mounted_roots,
};
pub(super) use protocol::{observe, publish};
const FIRST_FD: RawFd = 1800;
const LIMIT: usize = 256;
// Linux UAPI mount.h; libc exposes these only on GNU, while the shim also
// supports Linux musl. Empty endpoints name held descriptors, never paths.
const MOVE_MOUNT_EMPTY_ENDPOINTS: libc::c_uint = 0x0000_0004 | 0x0000_0040;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Projection {
    pub(super) placement: Vec<u8>,
    descriptor: RawFd,
    pub(super) identity: (u64, u64, u32, u32, u32),
    mount: u64,
}
/// Actual readonly `O_PATH` source descriptor. Root is required at construction;
/// this type supplies no credentials or public app-selected source authority.
pub struct OriginalMountedInput {
    placement: PathBuf,
    file: File,
}
impl OriginalMountedInput {
    /// The root broker must first verify its protected per-root source record.
    /// This constructor validates only native descriptor shape, not that record.
    /// # Errors
    /// Refuses non-root, nonliteral placement or non-readonly `O_PATH` input.
    pub fn from_reviewed(placement: PathBuf, file: File) -> io::Result<Self> {
        root()?;
        placement_shape(placement.as_os_str().as_bytes())?;
        check_input(&file)?;
        Ok(Self { placement, file })
    }
}
pub(super) struct Inputs {
    files: Vec<File>,
    projections: Vec<Projection>,
}
impl Inputs {
    pub(super) fn admit(inputs: Vec<OriginalMountedInput>, source_device: u64) -> io::Result<Self> {
        Self::admit_mode(inputs, source_device, None)
    }
    pub(super) fn admit_bound(
        inputs: Vec<OriginalMountedInput>,
        source_device: u64,
        expected: [u8; 32],
    ) -> io::Result<Self> {
        Self::admit_mode(inputs, source_device, Some(expected))
    }
    fn admit_mode(
        inputs: Vec<OriginalMountedInput>,
        source_device: u64,
        expected: Option<[u8; 32]>,
    ) -> io::Result<Self> {
        root()?;
        if inputs.is_empty() || inputs.len() > LIMIT {
            return Err(refused("Original mounted source count is outside bound"));
        }
        let mut result = Self {
            files: Vec::with_capacity(inputs.len()),
            projections: Vec::with_capacity(inputs.len()),
        };
        for (index, input) in inputs.into_iter().enumerate() {
            check_input(&input.file)?;
            let identity = identity(&input.file)?;
            // Cross-volume readers require independent participant records.
            // This first bootstrap accepts only the protected SAME-volume set.
            if identity.0 != source_device && expected.is_none() {
                return Err(refused("Mounted source needs independent volume custody"));
            }
            result.projections.push(Projection {
                placement: input.placement.as_os_str().as_bytes().to_vec(),
                descriptor: FIRST_FD
                    .checked_add(i32::try_from(index).map_err(io::Error::other)?)
                    .ok_or(io::ErrorKind::InvalidData)?,
                identity,
                mount: super::super::inventory_scratch::identity(&input.file)?.2,
            });
            result.files.push(input.file);
        }
        validate(&result.projections)?;
        if let Some(expected) = expected
            && (expected == [0; 32]
                || original_mounted_table_sha256(&protocol::identities(&result.projections))?
                    != expected)
        {
            return Err(refused("Protected mounted input table differs"));
        }
        result.check()?;
        Ok(result)
    }
    pub(super) fn projections(&self) -> Vec<Projection> {
        self.projections.clone()
    }
    pub(super) fn pairs(&self) -> Vec<(RawFd, RawFd)> {
        self.files
            .iter()
            .zip(&self.projections)
            .map(|(f, p)| (f.as_raw_fd(), p.descriptor))
            .collect()
    }
    pub(super) fn check_inventory_scratch(
        &self,
        scratch: &super::super::inventory_scratch::Scratch,
    ) -> io::Result<()> {
        self.check()?;
        scratch.check_private_membership()?;
        for file in &self.files {
            scratch.refuse_mounted_overlap(file)?;
        }
        scratch.check_private_membership()?;
        self.check()
    }
    pub(super) fn check(&self) -> io::Result<()> {
        root()?;
        validate(&self.projections)?;
        if self.files.len() != self.projections.len() {
            return Err(refused("Original root descriptors incomplete"));
        }
        for (file, projection) in self.files.iter().zip(&self.projections) {
            check_input(file)?;
            if identity(file)? != projection.identity
                || super::super::inventory_scratch::identity(file)?.2 != projection.mount
            {
                return Err(refused("Original root descriptor changed"));
            }
        }
        Ok(())
    }
}
pub(in crate::owned_service::capture) fn reserved_fd(fd: RawFd) -> bool {
    fd.checked_sub(FIRST_FD)
        .and_then(|offset| usize::try_from(offset).ok())
        .is_some_and(|offset| offset < LIMIT)
        || protocol::reserved(fd)
}
fn identity(file: &File) -> io::Result<(u64, u64, u32, u32, u32)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino(), m.uid(), m.gid(), m.mode()))
}
fn check_input(file: &File) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    let m = file.metadata()?;
    if flags < 0 || flags & libc::O_PATH == 0 || m.nlink() == 0 || (!m.is_dir() && !m.is_file()) {
        return Err(refused(
            "Original root input must be literal O_PATH file or directory",
        ));
    }
    readonly(file)
}
fn placement_shape(bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty()
        || bytes.len() > 4096
        || bytes.contains(&0)
        || bytes.first() == Some(&b'/')
        || bytes
            .split(|c| *c == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return Err(refused(
            "Original mounted placement is not bounded literal relative path",
        ));
    }
    Ok(())
}
pub(super) fn validate(projections: &[Projection]) -> io::Result<()> {
    if projections.len() > LIMIT {
        return Err(refused("Original root count exceeds bound"));
    }
    let mut old: Vec<PathBuf> = Vec::with_capacity(projections.len());
    for (index, p) in projections.iter().enumerate() {
        placement_shape(&p.placement)?;
        if p.descriptor
            != FIRST_FD
                .checked_add(i32::try_from(index).map_err(io::Error::other)?)
                .ok_or(io::ErrorKind::InvalidData)?
            || !matches!(p.identity.4 & libc::S_IFMT, libc::S_IFDIR | libc::S_IFREG)
        {
            return Err(refused("Original root table kind or descriptor changed"));
        }
        let path = Path::new(std::ffi::OsStr::from_bytes(&p.placement));
        if old
            .last()
            .is_some_and(|previous| previous.as_path() >= path)
            || old
                .iter()
                .any(|previous| path.starts_with(previous) || previous.starts_with(path))
        {
            return Err(refused("Original root table overlaps or is not ordered"));
        }
        old.push(path.to_owned());
    }
    Ok(())
}
/// Signed shim only, called after exact namespace and primary mount validation.
/// Mounts happen while root; ambient source FDs close before caller drop.
pub(super) fn project(
    projections: &[Projection],
    access: &super::credentials::ProjectionFilesystemAccess,
) -> io::Result<()> {
    access.validate()?;
    validate(projections)?;
    for p in projections {
        let raw = unsafe { libc::fcntl(p.descriptor, libc::F_DUPFD_CLOEXEC, 128) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let source = unsafe { File::from_raw_fd(raw) };
        check_input(&source)?;
        if identity(&source)? != p.identity
            || super::super::inventory_scratch::identity(&source)?.2 != p.mount
        {
            return Err(refused("Inherited root descriptor changed"));
        }
        let target = open_target(&p.placement, false)?;
        if source.metadata()?.is_dir()
            && (source.metadata()?.dev(), source.metadata()?.ino())
                != (target.metadata()?.dev(), target.metadata()?.ino())
            && super::super::contains(&source, &target)?
        {
            return Err(refused(
                "Original mounted source would create a directory cycle",
            ));
        }
        let target_kind = target.metadata()?.mode() & libc::S_IFMT;
        if target_kind != p.identity.4 & libc::S_IFMT {
            return Err(refused("Original root placement kind differs"));
        }
        // Only the fresh authenticated detached mount can move into this
        // namespace. Both endpoints are held FDs; caller DAC selected target.
        super::phase(
            super::Stage::AttachMountedRoot,
            access.with_root_mount(|| {
                // SAFETY: empty paths select the held source/target descriptors only.
                if unsafe {
                    libc::syscall(
                        libc::SYS_move_mount,
                        source.as_raw_fd(),
                        c"".as_ptr(),
                        target.as_raw_fd(),
                        c"".as_ptr(),
                        MOVE_MOUNT_EMPTY_ENDPOINTS,
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }),
        )?;
        // Reopen through the confined Home path; a descriptor for the covered
        // mountpoint still refers to its underlying inode, not the new bind.
        let mounted = open_target(&p.placement, true)?;
        let mounted_path = CString::new(format!("/proc/self/fd/{}", mounted.as_raw_fd()))
            .map_err(io::Error::other)?;
        super::phase(
            super::Stage::RemountReadonly,
            access.with_root_mount(|| {
                // SAFETY: the NUL-terminated path names the held reopened bind.
                if unsafe {
                    libc::mount(
                        std::ptr::null(),
                        mounted_path.as_ptr(),
                        std::ptr::null(),
                        libc::MS_REMOUNT
                            | libc::MS_BIND
                            | libc::MS_RDONLY
                            | libc::MS_NOSUID
                            | libc::MS_NODEV
                            | libc::MS_NOEXEC,
                        std::ptr::null(),
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }),
        )?;
        if identity(&mounted)? != p.identity
            || super::super::inventory_scratch::identity(&mounted)?.2 != p.mount
        {
            return Err(refused("Projected original root inode differs"));
        }
        readonly(&mounted)?;
    }
    Ok(())
}
fn open_target(placement: &[u8], allow_final_mount: bool) -> io::Result<File> {
    placement_shape(placement)?;
    let mut current = File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(super::super::SOURCE_PATH)?;
    let mount = super::super::inventory_scratch::identity(&current)?.2;
    let mut components = Path::new(std::ffi::OsStr::from_bytes(placement))
        .components()
        .peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(refused("Original target component differs"));
        };
        let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
        let raw = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { File::from_raw_fd(raw) };
        if (components.peek().is_some() || !allow_final_mount)
            && super::super::inventory_scratch::identity(&current)?.2 != mount
        {
            return Err(refused("Original placement crosses an undeclared mount"));
        }
        if current.metadata()?.file_type().is_symlink() {
            return Err(refused("Original target is a symbolic link"));
        }
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_root_table_refuses_nonliteral_overlapping_and_unbounded_slots() {
        let projection = |path: &[u8], slot| Projection {
            placement: path.to_vec(),
            descriptor: slot,
            identity: (1, 2, 1003, 1002, libc::S_IFDIR | 0o750),
            mount: 7,
        };
        for path in [
            b"".as_slice(),
            b"/data",
            b"../data",
            b"data/./x",
            b"data//x",
            b"data/../x",
            b"data\0x",
        ] {
            assert!(validate(&[projection(path, FIRST_FD)]).is_err());
        }
        assert!(
            validate(&[
                projection(b"data", FIRST_FD),
                projection(b"data/child", FIRST_FD + 1)
            ])
            .is_err()
        );
        assert!(validate(&[projection(b"z", FIRST_FD), projection(b"a", FIRST_FD + 1)]).is_err());
        assert!(validate(&[projection(b"data", FIRST_FD + 1)]).is_err());
        let mut wrong = projection(b"data", FIRST_FD);
        wrong.identity.4 = libc::S_IFLNK | 0o777;
        assert!(validate(&[wrong]).is_err());
        let many = (0..=LIMIT)
            .map(|index| {
                projection(
                    format!("root-{index:04}").as_bytes(),
                    FIRST_FD + i32::try_from(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(validate(&many).is_err());
        validate(&[projection(b"raw-\xff", FIRST_FD)]).unwrap();
        let actual = tempfile::NamedTempFile::new().unwrap();
        assert!(
            check_input(actual.as_file()).is_err(),
            "actual ordinary readable/writable descriptor cannot supply O_PATH readonly root authority"
        );
    }
}
