//! Private derivative of the exact signed retained worker installation.
use super::{ServiceAuthority, ServiceCancellation, ServiceEndpoint, ServiceOwnership, exited};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn snapshot(m: &Metadata) -> [u64; 10] {
    [
        m.dev(),
        m.ino(),
        m.len(),
        u64::from(m.mode()),
        u64::from(m.uid()),
        u64::from(m.gid()),
        m.mtime().cast_unsigned(),
        m.mtime_nsec().cast_unsigned(),
        m.ctime().cast_unsigned(),
        m.ctime_nsec().cast_unsigned(),
    ]
}
fn protected(m: &Metadata) -> io::Result<()> {
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(refused());
    }
    Ok(())
}
fn open_entry(root: &File, relative: &Path) -> io::Result<File> {
    let components: Vec<_> = relative.components().collect();
    if components.is_empty()
        || components.len() > 64
        || components
            .iter()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(refused());
    }
    let mut parent = root.try_clone()?;
    protected(&parent.metadata()?)?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(refused());
        };
        let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
        let last = components.len().checked_sub(1) == Some(index);
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        parent = unsafe { File::from_raw_fd(fd) };
        let facts = parent.metadata()?;
        protected(&facts)?;
        if (last && (!facts.is_file() || facts.mode() & 0o111 == 0)) || (!last && !facts.is_dir()) {
            return Err(refused());
        }
    }
    Ok(parent)
}
pub(super) struct RetainedWorkerProgram {
    file: File,
    root: File,
    root_path: PathBuf,
    relative: PathBuf,
    facts: [u64; 10],
}
impl RetainedWorkerProgram {
    pub(super) fn admit(installed: &package_store::InstalledPackage) -> io::Result<Self> {
        if installed.id != super::RETAINED_IO_PACKAGE_ID {
            return Err(refused());
        }
        let verify = || {
            crate::run::verified_launch_installation(
                installed,
                &installed.manifest_path,
                &installed.payload_root,
                &installed.package_id_hex,
            )
            .map_err(io::Error::other)
        };
        let manifest = verify()?;
        let relative = PathBuf::from(
            manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin),
        );
        let root = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&installed.payload_root)?;
        let mut file = open_entry(&root, &relative)?;
        let facts = snapshot(&file.metadata()?);
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        if magic != *b"\x7fELF" {
            return Err(refused());
        }
        let after = verify()?;
        if Path::new(after.entrypoint.cli_bin().unwrap_or(&after.entrypoint.bin))
            != relative.as_path()
            || snapshot(&file.metadata()?) != facts
        {
            return Err(refused());
        }
        let program = Self {
            file,
            root,
            root_path: installed.payload_root.clone(),
            relative,
            facts,
        };
        program.check()?;
        Ok(program)
    }
    pub(super) fn check(&self) -> io::Result<()> {
        let named_root = std::fs::symlink_metadata(&self.root_path)?;
        let held_root = self.root.metadata()?;
        if !named_root.is_dir()
            || (named_root.dev(), named_root.ino()) != (held_root.dev(), held_root.ino())
            || snapshot(&self.file.metadata()?) != self.facts
            || snapshot(&open_entry(&self.root, &self.relative)?.metadata()?) != self.facts
        {
            return Err(refused());
        }
        Ok(())
    }
}
/// Opaque ownership derived only from a retained endpoint's signed installed ELF, exact init pidfd and
/// current socket peer.
pub struct RetainedWorkerProof {
    init: OwnedFd,
    namespace: File,
    program: Arc<RetainedWorkerProgram>,
    authority: Arc<dyn ServiceAuthority>,
    cancellation: ServiceCancellation,
    ownership: ServiceOwnership,
    uid: u32,
    gid: u32,
}
impl RetainedWorkerProof {
    /// Verify the live worker's authority, program and namespace ownership.
    pub fn check(&self) -> io::Result<()> {
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(refused());
        }
        self.program.check()?;
        let namespace = self.namespace.metadata()?;
        let start = tilo_process::start_time(self.ownership.init_pid).ok_or_else(refused)?;
        if start != self.ownership.start_ticks
            || namespace.dev() != self.ownership.namespace_dev
            || namespace.ino() != self.ownership.namespace_ino
        {
            return Err(refused());
        }
        self.authority.check()?;
        if self.cancellation.is_cancelled() || exited(&self.init)? {
            return Err(refused());
        }
        Ok(())
    }
    pub fn pid(&self) -> u32 {
        self.ownership.init_pid
    }
    pub fn start_time(&self) -> u64 {
        self.ownership.start_ticks
    }
    pub fn uid(&self) -> u32 {
        self.uid
    }
    pub fn gid(&self) -> u32 {
        self.gid
    }
}
pub(super) fn same_worker_peer(
    stream: &UnixStream,
    ownership: &ServiceOwnership,
    uid: u32,
    gid: u32,
) -> io::Result<libc::ucred> {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let expected_len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    let mut len = expected_len;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut peer).cast(),
            &raw mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if len != expected_len
        || peer.pid <= 1
        || peer.pid.cast_unsigned() != ownership.init_pid
        || peer.uid != uid
        || peer.gid != gid
    {
        return Err(refused());
    }
    Ok(peer)
}
impl ServiceEndpoint {
    /// A NEW unread stream to the retained worker only.
    pub fn connect_pending_worker(&self) -> io::Result<(UnixStream, RetainedWorkerProof)> {
        let before = self.ownership()?;
        let program = self.retained_program.as_ref().ok_or_else(refused)?.clone();
        program.check()?;
        let gid = self.retained_gid.ok_or_else(refused)?;
        let stream = self.connect()?;
        let peer = same_worker_peer(&stream, &before, self.expected_uid, gid)?;
        if self.ownership()? != before {
            return Err(refused());
        }
        let proof = RetainedWorkerProof {
            init: self.init.try_clone()?,
            namespace: self.namespace.try_clone()?,
            program,
            authority: self.authority.clone(),
            cancellation: self.cancellation.clone(),
            ownership: before,
            uid: peer.uid,
            gid: peer.gid,
        };
        proof.check()?;
        Ok((stream, proof))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn held_entry_walk_refuses_symlink_and_writable_parent() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::switch_identity(65534),
            "the protected worker fixture is a root-owned tree under /run"
        );
        let fixture = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/run")
            .unwrap();
        let directory = fixture.path().join("bin");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let entry = directory.join("worker");
        std::fs::write(&entry, b"\x7fELF fixture observation only").unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o500)).unwrap();
        let root = File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(fixture.path())
            .unwrap();
        let held = open_entry(&root, Path::new("bin/worker")).unwrap();
        let identity = snapshot(&held.metadata().unwrap());
        std::fs::rename(&entry, directory.join("held")).unwrap();
        std::os::unix::fs::symlink("held", &entry).unwrap();
        assert!(open_entry(&root, Path::new("bin/worker")).is_err());
        let held_now = held.metadata().unwrap();
        assert_eq!(
            [held_now.dev(), held_now.ino()],
            [identity[0], identity[1]],
            "actual held inode survives a substituted name"
        );
        std::fs::remove_file(&entry).unwrap();
        std::fs::rename(directory.join("held"), &entry).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(open_entry(&root, Path::new("bin/worker")).is_err());
        // This walks real descriptors, and never constructs signed admission.
    }
}
