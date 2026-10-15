//! WIP: shared exact mount syscall core. No source or application grant.
//! Controller must own this before place and establish actual consumer stop
//! before retire; public callers cannot construct or discharge it.
use super::{
    AsRawFd, BASE, CString, DirBuilderExt, File, MOVE_EMPTY, OpenOptionsExt, PathBuf, exact_mode,
    held_inode, identity, io, mount_entry, mount_id, mount_row, mounted_file, namespace,
    protected_directory, readonly, refused, root,
};

#[derive(PartialEq, Eq)]
enum StageDetach {
    Attached,
    Detached,
}

struct Preparation {
    path: PathBuf,
    armed: bool,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        if self.armed {
            // Before placement this unique root-only path contains only
            // our empty directory. Never recursively remove a tree.
            let _ = std::fs::remove_dir(self.path.join("view"));
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

pub(crate) struct MountPlacement {
    namespace: File,
    namespace_identity: (u64, u64),
    parent: File,
    parent_identity: (u64, u64),
    path: PathBuf,
    point: File,
    point_identity: (u64, u64),
    backing: File,
    stage_id: Option<u64>,
    stage_pending: bool,
    stage_detach: StageDetach,
    clone_id: Option<u64>,
    clone_pending: bool,
    view: Option<File>,
    mounted: bool,
}
impl MountPlacement {
    pub(crate) fn allocate(
        view: File,
        clone_id: u64,
        read_only: bool,
        nonce: [u8; 32],
    ) -> io::Result<Self> {
        root()?;
        let ns = namespace()?;
        let nsid = identity(&ns)?;
        protected_directory(std::path::Path::new("/"))?;
        protected_directory(std::path::Path::new("/run"))?;
        match std::fs::DirBuilder::new().mode(0o711).create(BASE) {
            Ok(()) => {
                let file = mounted_file(std::path::Path::new(BASE))?;
                exact_mode(&file, 0o711)?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let base = protected_directory(std::path::Path::new(BASE))?;
        if nonce == [0; 32] {
            return Err(refused("Exposure nonce unavailable"));
        }
        let path = PathBuf::from(BASE).join(hex::encode(nonce));
        std::fs::DirBuilder::new().mode(0o711).create(&path)?;
        let mut preparation = Preparation {
            path: path.clone(),
            armed: true,
        };
        let created = mounted_file(&path)?;
        exact_mode(&created, 0o711)?;
        let parent = protected_directory(&path)?;
        let parent_identity = identity(&parent)?;
        let leaf = path.join("view");
        std::fs::DirBuilder::new().mode(0o700).create(&leaf)?;
        let point = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&leaf)?;
        let point_identity = identity(&point)?;
        if mount_id(&point)? != mount_id(&base)? {
            return Err(refused("Exposure staging contains mount"));
        }
        if mount_id(&view)? != clone_id {
            return Err(refused("Placement clone identity differs"));
        }
        if read_only {
            readonly(&view)?;
        }
        let backing = parent.try_clone()?;
        preparation.armed = false;
        Ok(Self {
            namespace: ns,
            namespace_identity: nsid,
            parent,
            parent_identity,
            path: leaf,
            point,
            point_identity,
            backing,
            stage_id: None,
            stage_pending: false,
            stage_detach: StageDetach::Attached,
            clone_id: Some(clone_id),
            clone_pending: false,
            view: Some(view),
            mounted: false,
        })
    }
    /// Caller must install Self in its persistent owner BEFORE this operation.
    pub(crate) fn place(&mut self) -> io::Result<()> {
        self.check_kernel()?;
        if self.mounted {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if self.stage_id.is_none() {
            // Inserting even an empty carrier under a shared mount propagates.
            // Only the canonical root-managed private consuming namespace is
            // supported; never change a shared host ancestor's propagation.
            if mount_entry(mount_id(&self.backing)?)?.is_none_or(|entry| !entry.0) {
                return Err(refused(
                    "Exposure requires owned private consuming namespace",
                ));
            }
            let stage = CString::new(
                self.path
                    .parent()
                    .ok_or(io::ErrorKind::InvalidData)?
                    .as_os_str()
                    .as_encoded_bytes(),
            )
            .map_err(io::Error::other)?;
            if unsafe {
                libc::mount(
                    c"tilo-retained-stage".as_ptr(),
                    stage.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOEXEC,
                    c"mode=0711,size=4096,nr_inodes=4".as_ptr().cast(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            // Capture placement immediately, before another fallible operation.
            self.stage_pending = true;
            let parent = mounted_file(self.path.parent().ok_or(io::ErrorKind::InvalidData)?)?;
            self.stage_id = Some(mount_id(&parent)?);
            self.parent_identity = identity(&parent)?;
            self.parent = parent;
            if unsafe {
                libc::mount(
                    std::ptr::null(),
                    stage.as_ptr(),
                    std::ptr::null(),
                    libc::MS_PRIVATE,
                    std::ptr::null(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if mount_row(self.stage_id.ok_or(io::ErrorKind::InvalidData)?)? != Some(true) {
                return Err(refused("Exposure destination not exact private stage"));
            }
            std::fs::DirBuilder::new().mode(0o700).create(&self.path)?;
            self.point = mounted_file(&self.path)?;
            self.point_identity = identity(&self.point)?;
        }
        let view = self.view.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        if unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                view.as_raw_fd(),
                c"".as_ptr(),
                self.point.as_raw_fd(),
                c"".as_ptr(),
                MOVE_EMPTY,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.mounted = true;
        let named = mounted_file(&self.path)?;
        if Some(mount_id(&named)?) != self.clone_id || held_inode(&named)? != held_inode(view)? {
            return Err(refused("Placement named clone differs"));
        }
        self.check()
    }
    fn check_kernel(&self) -> io::Result<()> {
        root()?;
        if identity(&namespace()?)? != self.namespace_identity
            || identity(&self.namespace)? != self.namespace_identity
            || identity(&self.parent)? != self.parent_identity
            || identity(&self.point)? != self.point_identity
        {
            return Err(refused("Exposure namespace/staging custody differs"));
        }
        let current = protected_directory(self.path.parent().ok_or(io::ErrorKind::InvalidData)?)?;
        if self.stage_pending && self.stage_detach == StageDetach::Attached {
            if Some(mount_id(&current)?) != self.stage_id {
                return Err(refused("Exposure stage replaced"));
            }
        } else if identity(&current)? != identity(&self.backing)? {
            return Err(refused("Exposure parent replaced"));
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        self.check_kernel()?;
        let id = self.clone_id.ok_or(io::ErrorKind::NotConnected)?;
        let view = self.view.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let named = mounted_file(&self.path)?;
        if !self.mounted
            || mount_id(view)? != id
            || mount_row(id)? != Some(true)
            || mount_id(&named)? != id
            || held_inode(&named)? != held_inode(view)?
        {
            return Err(refused("Placement attached clone differs"));
        }
        Ok(())
    }
    pub(crate) fn file(&self) -> io::Result<&File> {
        self.check()?;
        self.view
            .as_ref()
            .ok_or_else(|| io::ErrorKind::NotConnected.into())
    }
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        // Recover the exact stage identity if initial post-mount fdinfo failed.
        // Namespace and root-owned backing path remain held throughout.
        if self.stage_pending && self.stage_id.is_none() {
            root()?;
            if identity(&namespace()?)? != self.namespace_identity {
                return Err(refused("Exposure retirement namespace differs"));
            }
            let current =
                protected_directory(self.path.parent().ok_or(io::ErrorKind::InvalidData)?)?;
            let id = mount_id(&current)?;
            if id == mount_id(&self.backing)? {
                return Err(refused("Exposure stage mount observation missing"));
            }
            self.stage_id = Some(id);
            self.parent_identity = identity(&current)?;
            self.parent = current;
        }
        self.check_kernel()?;
        if self.mounted {
            let id = self.clone_id.ok_or(io::ErrorKind::NotConnected)?;
            if !self.clone_pending {
                if mount_id(&mounted_file(&self.path)?)? != id {
                    return Err(refused("Exposure retirement placement differs"));
                }
                self.view.take();
                let path = CString::new(self.path.as_os_str().as_encoded_bytes())
                    .map_err(io::Error::other)?;
                if unsafe { libc::umount2(path.as_ptr(), libc::UMOUNT_NOFOLLOW) } != 0 {
                    let error = io::Error::last_os_error();
                    self.view = Some(mounted_file(&self.path)?);
                    return Err(error);
                }
                self.clone_pending = true;
            }
            if mount_row(id)?.is_some() {
                return Err(refused("Exposure unmount not observed"));
            }
            self.mounted = false;
            self.clone_pending = false;
        }
        if self.stage_pending {
            let id = if let Some(id) = self.stage_id {
                id
            } else {
                let parent = mounted_file(self.path.parent().ok_or(io::ErrorKind::InvalidData)?)?;
                let id = mount_id(&parent)?;
                self.stage_id = Some(id);
                id
            };
            // Release every root staging reference before nonlazy unmount.
            self.point = self.backing.try_clone()?;
            self.point_identity = identity(&self.point)?;
            self.parent = self.backing.try_clone()?;
            self.parent_identity = identity(&self.parent)?;
            if self.stage_detach == StageDetach::Attached {
                let path = CString::new(
                    self.path
                        .parent()
                        .ok_or(io::ErrorKind::InvalidData)?
                        .as_os_str()
                        .as_encoded_bytes(),
                )
                .map_err(io::Error::other)?;
                if unsafe { libc::umount2(path.as_ptr(), libc::UMOUNT_NOFOLLOW) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                self.stage_detach = StageDetach::Detached;
            }
            if mount_row(id)?.is_some() {
                return Err(refused("Exposure stage stop not observed"));
            }
            self.stage_pending = false;
            self.stage_detach = StageDetach::Attached;
            self.stage_id = None;
        }
        self.view.take();
        match std::fs::remove_dir(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::fs::remove_dir(self.path.parent().ok_or(io::ErrorKind::InvalidData)?)?;
        Ok(())
    }
}
