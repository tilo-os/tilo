//! Held Unix namespaces for enrollment transport; ambient paths never name keys.
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::sync::Arc;

use advisory_lock::FileLock;
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
use zeroize::Zeroizing;

struct Node {
    file: File,
    parent: Option<(Arc<Node>, OsString)>,
    admitted_private: Option<bool>,
    public_read: bool,
    system_namespace: bool,
    observed_owner: Option<u32>,
}

pub(super) struct Directory {
    node: Arc<Node>,
    storage: StorageDirectory,
    private: bool,
    public_read: bool,
    system_namespace: bool,
}

fn component(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
        return Err(io::Error::other("invalid enrollment child component"));
    }
    CString::new(name.as_bytes()).map_err(|_| io::Error::other("invalid enrollment child name"))
}

fn open_child(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = component(name)?;
    // SAFETY: held parent and live terminated name; successful fd is owned below.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh descriptor owned by this operation.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn same(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.is_dir() && right.is_dir() && left.dev() == right.dev() && left.ino() == right.ino())
}

// Admit only the canonical system trust group, never an arbitrary writable gid.
fn system_controlled(metadata: &std::fs::Metadata) -> io::Result<bool> {
    if metadata.uid() != 0 || metadata.mode() & 0o002 != 0 {
        return Ok(false);
    }
    if metadata.mode() & 0o020 == 0 {
        return Ok(true);
    }
    let mut entry = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65_536];
    // SAFETY: fixed terminated name, live bounded writable group/buffer/output.
    let status = unsafe {
        libc::getgrnam_r(
            c"tilo-identity".as_ptr(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if result.is_null() {
        return Ok(false);
    }
    // SAFETY: successful nonnull lookup initialized entry; gid has no borrowed data.
    let gid = unsafe { entry.assume_init() }.gr_gid;
    if metadata.gid() != gid {
        return Ok(false);
    }
    // SAFETY: these calls only read process credential values.
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getegid() } == gid {
        return Ok(true);
    }
    // SAFETY: count-only query has no output buffer.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count > 1024 {
        return Err(io::Error::other(
            "group membership exceeds enrollment limit",
        ));
    }
    let group_count =
        usize::try_from(count).map_err(|_| io::Error::other("invalid enrollment group count"))?;
    let mut groups = vec![0; group_count];
    // SAFETY: vector holds exactly count gid entries; kernel never writes more.
    let found = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    if found < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(groups.contains(&gid))
}

fn validate_node(
    node: &Node,
    storage: &StorageDirectory,
    private: bool,
    public_read: bool,
) -> io::Result<()> {
    if let Some(owner) = node.observed_owner {
        #[cfg(target_os = "linux")]
        {
            require_root_observer()?;
            storage.validate_observed_owner(
                owner,
                if private {
                    Privacy::OwnerOnly
                } else {
                    Privacy::Inherit
                },
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = owner;
            Err(io::ErrorKind::Unsupported.into())
        }
    } else if public_read || node.system_namespace {
        let metadata = node.file.metadata()?;
        if !metadata.is_dir() || !system_controlled(&metadata)? {
            return Err(io::Error::other(
                "public enrollment namespace is not root-controlled",
            ));
        }
        Ok(())
    } else if private {
        storage.validate_privacy(Privacy::OwnerOnly)
    } else {
        storage.validate_namespace_control()
    }
}

#[cfg(target_os = "linux")]
fn require_root_observer() -> io::Result<()> {
    // SAFETY: these calls only read process credentials.
    if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
impl Directory {
    /// Read-only explicit owner grant, supplied by the authenticated owner channel.
    /// No ambient pathname or ownership override participates in ordinary APIs.
    #[cfg(target_os = "linux")]
    pub(super) fn observe_owner(file: File, owner: u32) -> io::Result<Self> {
        require_root_observer()?;
        // SAFETY: F_GETFL only observes the supplied live descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_PATH != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // SAFETY: F_SETFD applies close-on-exec to this owned received descriptor.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Self::admit(
            &Node {
                file,
                parent: None,
                admitted_private: None,
                public_read: false,
                system_namespace: false,
                observed_owner: Some(owner),
            },
            true,
            false,
            false,
        )
    }

    pub(super) fn open(path: &Path) -> io::Result<Self> {
        Self::open_mode(path, true, false, false)
    }

    pub(super) fn open_namespace(path: &Path) -> io::Result<Self> {
        Self::open_mode(path, false, false, false)
    }

    pub(super) fn open_public_root(path: &Path) -> io::Result<Self> {
        Self::open_mode(path, false, true, false)
    }

    pub(super) fn open_system_namespace(path: &Path) -> io::Result<Self> {
        // SAFETY: geteuid only reads current process credentials.
        if unsafe { libc::geteuid() } != 0 {
            return Err(io::Error::other(
                "system enrollment publication requires root",
            ));
        }
        Self::open_mode(path, false, false, true)
    }

    fn open_mode(
        path: &Path,
        private: bool,
        public_read: bool,
        system_namespace: bool,
    ) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::other("enrollment grant must be absolute"));
        }
        let mut node = Arc::new(Node {
            file: File::open("/")?,
            parent: None,
            admitted_private: None,
            public_read: false,
            system_namespace: false,
            observed_owner: None,
        });
        for edge in path.components() {
            match edge {
                Component::RootDir => {}
                Component::Normal(name) => {
                    node = Arc::new(Node {
                        file: open_child(&node.file, name)?,
                        parent: Some((node, name.to_owned())),
                        admitted_private: None,
                        public_read: false,
                        system_namespace: false,
                        observed_owner: None,
                    });
                }
                _ => return Err(io::Error::other("enrollment grant contains an alias")),
            }
        }
        Self::admit(&node, private, public_read, system_namespace)
    }

    fn admit(
        node: &Node,
        private: bool,
        public_read: bool,
        system_namespace: bool,
    ) -> io::Result<Self> {
        let node = Arc::new(Node {
            file: node.file.try_clone()?,
            parent: node.parent.clone(),
            admitted_private: Some(private),
            public_read,
            system_namespace,
            observed_owner: node.observed_owner,
        });
        let storage = StorageDirectory::from_file(node.file.try_clone()?)?;
        validate_node(&node, &storage, private, public_read)?;
        let result = Self {
            node,
            storage,
            private,
            public_read,
            system_namespace,
        };
        result.validate()?;
        Ok(result)
    }

    pub(super) fn child(&self, name: &str, create: bool) -> io::Result<Self> {
        self.validate()?;
        if create && (self.public_read || self.node.observed_owner.is_some()) {
            return Err(io::Error::other("public enrollment grant is read-only"));
        }
        let name = OsStr::new(name);
        let file = match open_child(&self.node.file, name) {
            Ok(file) => file,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                let native = component(name)?;
                // SAFETY: held directory, live terminated child, ordinary mode bits.
                if unsafe { libc::mkdirat(self.node.file.as_raw_fd(), native.as_ptr(), 0o700) } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let file = open_child(&self.node.file, name)?;
                StorageDirectory::from_file(file.try_clone()?)?
                    .validate_privacy(Privacy::OwnerOnly)?;
                self.node.file.sync_all()?;
                file
            }
            Err(error) => return Err(error),
        };
        Self::admit(
            &Node {
                file,
                parent: Some((Arc::clone(&self.node), name.to_owned())),
                admitted_private: None,
                public_read: self.public_read,
                system_namespace: self.system_namespace,
                observed_owner: self.node.observed_owner,
            },
            false,
            self.public_read,
            self.system_namespace,
        )
    }

    pub(super) fn file(
        &self,
        name: &str,
        private: bool,
        max: u64,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        self.validate()?;
        if private && self.public_read {
            return Err(io::Error::other(
                "public enrollment grant cannot read private keys",
            ));
        }
        let privacy = if private && self.node.observed_owner.is_none() {
            Privacy::OwnerOnly
        } else {
            Privacy::Inherit
        };
        let file = self.storage.open_read(OsStr::new(name), privacy)?;
        let before = file.metadata()?;
        let uid = self.node.file.metadata()?.uid();
        if !before.is_file()
            || before.uid() != uid
            || before.len() > max
            || (private && self.node.observed_owner.is_some() && before.mode() & 0o077 != 0)
            || (self.public_read && (before.mode() & 0o044 == 0 || !system_controlled(&before)?))
        {
            return Err(io::Error::other("enrollment file grant refused"));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        (&file)
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if bytes.len() as u64 > max
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(io::Error::other("enrollment file changed"));
        }
        let named = self
            .storage
            .open_read(OsStr::new(name), privacy)?
            .metadata()?;
        if named.dev() != after.dev()
            || named.ino() != after.ino()
            || named.uid() != uid
            || named.mode() != after.mode()
            || before.mode() != after.mode()
            || (private && self.node.observed_owner.is_some() && after.mode() & 0o077 != 0)
        {
            return Err(io::Error::other("enrollment file binding changed"));
        }
        self.validate()?;
        Ok(bytes)
    }

    /// Derive only the public half through the canonical held-inode key reader.
    /// Read-only root namespace authority never permits seed publication.
    pub(super) fn device_public_key(&self) -> io::Result<[u8; 32]> {
        self.validate()?;
        let file = self
            .storage
            .open_read(OsStr::new("keypair.bin"), Privacy::Inherit)?;
        let before = file.metadata()?;
        let key = tilo_user_identity::persist::read_device_key_from_file(&file).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Current device key is unavailable",
            )
        })?;
        let after = file.metadata()?;
        let named = self
            .storage
            .open_read(OsStr::new("keypair.bin"), Privacy::Inherit)?
            .metadata()?;
        let stamp = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.uid(),
                m.gid(),
                m.mode(),
                m.nlink(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if stamp(&before) != stamp(&after) || stamp(&after) != stamp(&named) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Current device key custody changed",
            ));
        }
        self.validate()?;
        Ok(key.verifying_key().to_bytes())
    }

    pub(super) fn optional(
        &self,
        name: &str,
        private: bool,
        max: u64,
    ) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        match self.file(name, private, max) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn publish(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.validate()?;
        if self.public_read || self.node.observed_owner.is_some() {
            return Err(io::Error::other("observed enrollment grant is read-only"));
        }
        self.storage
            .publish(
                OsStr::new(name),
                bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        self.validate()
    }

    pub(super) fn lock(&self, name: &str) -> io::Result<FileLock> {
        match self.publish(name, &[]) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        self.validate()?;
        let file = self
            .storage
            .open_append_existing(OsStr::new(name), Privacy::OwnerOnly)?;
        FileLock::try_exclusive(file).map_err(io::Error::other)
    }

    fn require_system_publication(&self) -> io::Result<()> {
        // SAFETY: geteuid only reads the current process credential.
        if !self.system_namespace || self.public_read || unsafe { libc::geteuid() } != 0 {
            return Err(io::Error::other(
                "public projection normalization requires root publication authority",
            ));
        }
        self.validate()
    }

    /// Called only after the canonical importer verifies the exact public value.
    pub(super) fn normalize_public_file(&self, name: &str) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        self.require_system_publication()?;
        let file = self.storage.open_read(OsStr::new(name), Privacy::Inherit)?;
        let admitted = file.metadata()?;
        if !admitted.is_file() || admitted.uid() != 0 {
            return Err(io::Error::other(
                "public projection is not a root-owned regular file",
            ));
        }
        if admitted.mode() & 0o7777 != 0o644 {
            file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
        file.sync_all()?;
        let named = self
            .storage
            .open_read(OsStr::new(name), Privacy::Inherit)?
            .metadata()?;
        if named.dev() != admitted.dev()
            || named.ino() != admitted.ino()
            || named.uid() != 0
            || named.mode() & 0o7777 != 0o644
        {
            return Err(io::Error::other(
                "public projection changed during normalization",
            ));
        }
        self.node.file.sync_all()?;
        self.validate()
    }

    pub(super) fn normalize_public_directory(&self) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        self.require_system_publication()?;
        let admitted = self.node.file.metadata()?;
        if !admitted.is_dir() || admitted.uid() != 0 {
            return Err(io::Error::other(
                "public projection directory is not root-owned",
            ));
        }
        if admitted.mode() & 0o7777 != 0o755 {
            self.node
                .file
                .set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        self.node.file.sync_all()?;
        if let Some((parent, _)) = &self.node.parent {
            parent.file.sync_all()?;
        }
        self.validate()
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let mut node = &self.node;
        loop {
            if let Some(private) = node.admitted_private {
                let held = StorageDirectory::from_file(node.file.try_clone()?)?;
                validate_node(node, &held, private, node.public_read)?;
            }
            let Some((parent, name)) = &node.parent else {
                break;
            };
            if !same(&node.file, &open_child(&parent.file, name)?)? {
                return Err(io::Error::other("enrollment directory grant changed"));
            }
            node = parent;
        }
        validate_node(&self.node, &self.storage, self.private, self.public_read)
    }

    pub(super) fn overlaps(&self, other: &Self) -> bool {
        fn contains(mut leaf: &Arc<Node>, needle: &File) -> bool {
            loop {
                if same(&leaf.file, needle).unwrap_or(true) {
                    return true;
                }
                let Some((parent, _)) = &leaf.parent else {
                    return false;
                };
                leaf = parent;
            }
        }
        contains(&self.node, &other.node.file) || contains(&other.node, &self.node.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_owner_observation_is_read_only_and_preserves_private_admission() {
        let root = private_root();
        let path = root.path().canonicalize().unwrap();
        let ordinary = Directory::open(&path).unwrap();
        ordinary.publish("record", b"retained-original").unwrap();
        let uid = unsafe { libc::geteuid() };
        if unsafe { libc::getuid() } != 0 || uid != 0 {
            assert!(Directory::observe_owner(File::open(&path).unwrap(), uid).is_err());
            return;
        }
        let shared = StorageDirectory::from_file(File::open(&path).unwrap()).unwrap();
        assert!(
            shared
                .validate_observed_owner(1, Privacy::OwnerOnly)
                .is_err()
        );
        assert!(Directory::observe_owner(File::open(&path).unwrap(), 1).is_err());
        let observed = Directory::observe_owner(File::open(&path).unwrap(), uid).unwrap();
        assert_eq!(
            observed.file("record", true, 64).unwrap().as_slice(),
            b"retained-original"
        );
        assert!(observed.publish("replacement", b"refused").is_err());
        assert!(observed.lock("lock").is_err());
        assert!(observed.child("new-child", true).is_err());
        std::fs::set_permissions(path.join("record"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(observed.file("record", true, 64).is_err());
        assert!(ordinary.file("record", true, 64).is_err());
        assert_eq!(
            std::fs::read(path.join("record")).unwrap(),
            b"retained-original"
        );
        assert!(!path.join("replacement").exists());
        assert!(!path.join("lock").exists());
        assert!(!path.join("new-child").exists());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(observed.validate().is_err());
        assert!(
            shared
                .validate_observed_owner(uid, Privacy::OwnerOnly)
                .is_err()
        );
    }

    #[test]
    fn held_enrollment_storage_bounds_reads_preserves_existing_bytes_and_excludes_aliases() {
        let root = private_root();
        let directory = Directory::open(&root.path().canonicalize().unwrap()).unwrap();
        directory.publish("seed", b"original-key").unwrap();
        assert_eq!(
            directory.file("seed", true, 32).unwrap().as_slice(),
            b"original-key"
        );
        assert!(directory.file("seed", true, 2).is_err());
        assert!(directory.publish("seed", b"replacement").is_err());
        assert_eq!(
            std::fs::read(root.path().join("seed")).unwrap(),
            b"original-key"
        );
        assert!(directory.optional("absent", true, 32).unwrap().is_none());
        assert!(directory.file("../seed", true, 32).is_err());
        symlink("seed", root.path().join("alias")).unwrap();
        assert!(directory.file("alias", true, 32).is_err());
        let child = directory.child("device", true).unwrap();
        assert!(directory.overlaps(&child));
        assert!(child.overlaps(&directory));
        let unrelated = private_root();
        let unrelated = Directory::open(&unrelated.path().canonicalize().unwrap()).unwrap();
        assert!(!directory.overlaps(&unrelated));
        let lease = directory.lock("lease").unwrap();
        assert!(directory.lock("lease").is_err());
        drop(lease);
        assert!(directory.lock("lease").is_ok());
    }

    #[test]
    fn public_root_authority_never_becomes_private_key_or_write_authority() {
        let root = private_root();
        let path = root.path().canonicalize().unwrap();
        let namespace = Directory::open_namespace(&path).unwrap();
        namespace
            .publish("certificate", b"public-certificate")
            .unwrap();
        namespace.publish("device.key", b"private-key").unwrap();
        std::fs::set_permissions(
            path.join("certificate"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(namespace.normalize_public_file("device.key").is_err());
        assert!(namespace.normalize_public_directory().is_err());
        let metadata = std::fs::metadata(&path).unwrap();
        if metadata.uid() != 0 {
            assert!(Directory::open_public_root(&path).is_err());
            assert!(Directory::open_system_namespace(&path).is_err());
        } else {
            let publisher = Directory::open_system_namespace(&path).unwrap();
            publisher
                .publish("new-publication", b"root-publication")
                .unwrap();
            assert_eq!(
                std::fs::read(path.join("new-publication")).unwrap(),
                b"root-publication"
            );
            publisher.normalize_public_file("new-publication").unwrap();
            publisher.normalize_public_directory().unwrap();
            assert_eq!(
                std::fs::metadata(path.join("new-publication"))
                    .unwrap()
                    .mode()
                    & 0o7777,
                0o644
            );
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o755);
            assert_eq!(
                std::fs::metadata(path.join("device.key")).unwrap().mode() & 0o7777,
                0o600
            );
            let public = Directory::open_public_root(&path).unwrap();
            assert_eq!(
                public.file("certificate", false, 64).unwrap().as_slice(),
                b"public-certificate"
            );
            assert!(public.file("device.key", true, 64).is_err());
            assert!(public.file("device.key", false, 64).is_err());
            assert!(public.publish("new", b"never-written").is_err());
            assert!(public.normalize_public_file("device.key").is_err());
            assert!(public.normalize_public_directory().is_err());
            assert!(public.lock("lock").is_err());
            assert!(public.child("new", true).is_err());
            assert!(!path.join("new").exists());
            assert!(!path.join("lock").exists());
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Directory::open_public_root(&path).is_err());
        assert!(namespace.validate().is_err());
    }

    #[test]
    fn public_child_namespace_cannot_escape_or_outlive_changed_private_ancestry() {
        let root = private_root();
        let path = root.path().canonicalize().unwrap();
        std::fs::create_dir(path.join("public")).unwrap();
        std::fs::set_permissions(path.join("public"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let directory = Directory::open(&path).unwrap();
        let child = directory.child("public", false).unwrap();
        child.publish("certificate", b"certificate").unwrap();
        std::fs::set_permissions(
            path.join("public/certificate"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(
            child.file("certificate", false, 64).unwrap().as_slice(),
            b"certificate"
        );
        assert!(child.file("certificate", true, 64).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(child.validate().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(path.join("public"), path.join("original-public")).unwrap();
        std::fs::create_dir(path.join("public")).unwrap();
        assert!(child.validate().is_err());
        symlink(path.join("original-public"), path.join("alias")).unwrap();
        assert!(directory.child("alias", false).is_err());
        assert!(Directory::open(&path.join("alias")).is_err());
        assert_eq!(
            std::fs::read(path.join("original-public/certificate")).unwrap(),
            b"certificate"
        );
    }
}
