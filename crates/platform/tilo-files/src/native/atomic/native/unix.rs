use crate::native::atomic::identity::VolumeIdentity as PublicVolumeIdentity;
use crate::native::atomic::publication::{
    CommitGuard as PublicCommitGuard, Privacy, PublicationError, PublicationState, PublishMode,
};
#[cfg(feature = "test-support")]
use crate::native::atomic::test_support::{self, Checkpoint, CheckpointEvent, StageObservation};
use rustix::fs::{self as unix_fs, AtFlags, Mode, OFlags, RenameFlags};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "macos")]
pub(crate) mod bounded_acl;
#[cfg(target_os = "linux")]
mod bounded_linux;

#[derive(Clone, Copy)]
pub(crate) enum FilePolicy {
    Ordinary(Privacy),
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    BoundedOwner,
}
impl FilePolicy {
    fn is_private(self) -> bool {
        !matches!(self, Self::Ordinary(Privacy::Inherit))
    }
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
        expect(
            clippy::unused_self,
            reason = "only macOS and Linux have a bounded-owner policy"
        )
    )]
    pub(crate) fn error(self, kind: io::ErrorKind, message: &'static str) -> io::Error {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if matches!(self, Self::BoundedOwner) {
            return kind.into();
        }
        io::Error::new(kind, message)
    }
}

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VolumeIdentity {
    device: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub(crate) public_volume: PublicVolumeIdentity,
    inode: u64,
}

impl Identity {
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        let volume = VolumeIdentity {
            device: metadata.dev(),
        };
        Self {
            public_volume: PublicVolumeIdentity { inner: volume },
            inode: metadata.ino(),
        }
    }
}

pub(crate) struct Directory {
    file: File,
}

impl Directory {
    pub(crate) fn from_file(file: File) -> io::Result<Self> {
        if !file.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "storage authority is not a directory",
            ));
        }
        #[cfg(target_os = "linux")]
        if unix_fs::fcntl_getfl(&file)?.contains(OFlags::PATH) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path-only handle cannot commit directory publication",
            ));
        }
        Ok(Self { file })
    }

    pub(crate) fn open(path: &std::path::Path) -> io::Result<Self> {
        Self::from_file(File::open(path)?)
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Self::from_file(self.file.try_clone()?)
    }

    pub(crate) fn validate_privacy(&self, privacy: Privacy) -> io::Result<()> {
        if privacy == Privacy::Inherit {
            return Ok(());
        }
        // The retained object supplies all evidence, including filesystem policy.
        let metadata = self.file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "storage authority is not a directory",
            ));
        }
        self.validate_permission_profile()?;
        validate_private_security(&self.file, &metadata)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn validate_bounded(&self) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        #[cfg(target_os = "macos")]
        {
            bounded_acl::validate_private(&self.file, &metadata)
        }
        #[cfg(target_os = "linux")]
        {
            bounded_linux::validate_private(&self.file, &metadata)
        }
    }

    pub(crate) fn validate_namespace_control(&self) -> io::Result<()> {
        self.validate_permission_profile()?;
        let metadata = self.file.metadata()?;
        if !metadata.is_dir()
            || !crate::native::private::owned_by_me(&metadata)
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "directory namespace is not controlled by the effective owner",
            ));
        }
        #[cfg(target_os = "macos")]
        macos_acl::validate(&self.file)?;
        Ok(())
    }

    // Permission-profile admission is shared with held directory validation;
    // it does not impose the private child's owner/mode policy on ancestors.
    #[cfg_attr(
        target_os = "macos",
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "only Linux inspects the held directory's filesystem profile"
        )
    )]
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            clippy::unused_self,
            reason = "only Linux inspects the held directory's filesystem profile"
        )
    )]
    fn validate_permission_profile(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "statfs magic numbers are 32-bit; the width and sign of `f_type` vary by target"
            )]
            let profile = unix_fs::fstatfs(&self.file)?.f_type as u32;
            // Local POSIX DAC/access-ACL profiles only. In particular FUSE may
            // omit DAC checks. OverlayFS also requires trusted host/backing names.
            if !matches!(
                profile,
                0xef53 | 0x9123_683e | 0x5846_5342 | 0x0102_1994 | 0x794c_7630
            ) {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unproven directory permission profile",
                ));
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unproven directory permission profile",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Ok(())
    }

    pub(crate) fn creation_preflight(&self, privacy: Privacy) -> io::Result<()> {
        if privacy == Privacy::OwnerOnly {
            self.validate_permission_profile()?;
            #[cfg(target_os = "macos")]
            macos_acl::validate(&self.file)?;
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn mkdir_fresh_private(&self, name: &OsStr) -> io::Result<()> {
        unix_fs::mkdirat(&self.file, name, Mode::from_raw_mode(0o700))?;
        Ok(())
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) fn normalize_fresh_empty(&self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            bounded_acl::normalize_fresh_empty(&self.file)
        }
        #[cfg(target_os = "linux")]
        {
            bounded_linux::normalize_fresh_empty(&self.file)
        }
    }

    pub(crate) fn open_directory(&self, name: &OsStr) -> io::Result<Self> {
        validate_component(name)?;
        let fd = unix_fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        Self::from_file(File::from(fd))
    }

    pub(crate) fn create_directory(
        &self,
        name: &OsStr,
        privacy: Privacy,
        physical: &std::path::Path,
    ) -> io::Result<()> {
        // Component and inheritance validation happen before the submission
        // boundary in provision.rs. Every error here is a submitted result.
        let operation = || {
            unix_fs::mkdirat(
                &self.file,
                name,
                Mode::from_raw_mode(if privacy == Privacy::OwnerOnly {
                    0o700
                } else {
                    0o777
                }),
            )?;
            Ok(())
        };
        #[cfg(feature = "test-support")]
        {
            test_support::mkdir_result(physical, operation)
        }
        #[cfg(not(feature = "test-support"))]
        {
            let _ = physical;
            operation()
        }
    }

    pub(crate) fn identity(&self) -> io::Result<Identity> {
        Identity::of(&self.file)
    }

    pub(crate) fn open_read(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.open_read_policy(name, FilePolicy::Ordinary(privacy))
    }
    pub(crate) fn open_read_policy(&self, name: &OsStr, privacy: FilePolicy) -> io::Result<File> {
        validate_component(name)?;
        let fd = unix_fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        let file = File::from(fd);
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(privacy.error(
                io::ErrorKind::InvalidData,
                "storage child is not a regular file",
            ));
        }
        validate_privacy(&file, &metadata, privacy)?;
        let mut flags = unix_fs::fcntl_getfl(&file)?;
        if flags.contains(OFlags::NONBLOCK) {
            flags.remove(OFlags::NONBLOCK);
            unix_fs::fcntl_setfl(&file, flags)?;
        }
        Ok(file)
    }

    pub(crate) fn open_writable(
        &self,
        name: &OsStr,
        privacy: Privacy,
        create: bool,
        append: bool,
    ) -> io::Result<File> {
        self.open_writable_policy(name, FilePolicy::Ordinary(privacy), create, append)
    }
    pub(crate) fn open_writable_policy(
        &self,
        name: &OsStr,
        privacy: FilePolicy,
        create: bool,
        append: bool,
    ) -> io::Result<File> {
        validate_component(name)?;
        let mut flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        if create {
            flags |= OFlags::CREATE;
        }
        if append {
            flags |= OFlags::APPEND;
        }
        let file = File::from(unix_fs::openat(
            &self.file,
            name,
            flags,
            Mode::from_raw_mode(0o666),
        )?);
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(privacy.error(
                io::ErrorKind::InvalidData,
                "storage child is not a regular file",
            ));
        }
        validate_privacy(&file, &metadata, privacy)?;
        let flags = unix_fs::fcntl_getfl(&file)? & !OFlags::NONBLOCK;
        unix_fs::fcntl_setfl(&file, flags)?;
        Ok(file)
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        unix_fs::fsync(&self.file)?;
        Ok(())
    }
}

pub(crate) struct CommitGuard {
    published_file: File,
    parent: Directory,
}

impl CommitGuard {
    pub(crate) fn commit(&self) -> io::Result<()> {
        // Retain both handles through the namespace commit. The data flush has
        // already completed, but keeping the published file prevents identity
        // from being inferred again through a path during recovery.
        let _ = self.published_file.metadata()?;
        checkpoint(CheckpointKind::BeforeCommit)?;
        self.parent.sync()
    }
}

struct Stage {
    file: Option<File>,
    parent: Option<Directory>,
    name: OsString,
    cleanup_armed: bool,
}

impl Stage {
    fn create(
        parent: Directory,
        target: &OsStr,
        privacy: FilePolicy,
        exact_mode: Option<u32>,
    ) -> io::Result<Self> {
        validate_component(target)?;
        let final_mode = exact_mode.or_else(|| (privacy.is_private()).then_some(0o600));
        let create_mode = final_mode.map_or(0o666, |mode| mode & 0o600);
        for _ in 0..32 {
            let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = match privacy {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                FilePolicy::BoundedOwner => bounded_stage_name(target, sequence),
                _ => stage_name(target, sequence),
            };
            match unix_fs::openat(
                &parent.file,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode((create_mode & 0o7777) as _),
            ) {
                Ok(fd) => {
                    let file = File::from(fd);
                    if let Some(final_mode) = final_mode
                        && let Err(error) =
                            unix_fs::fchmod(&file, Mode::from_raw_mode((final_mode & 0o7777) as _))
                    {
                        drop(file);
                        let _ = unix_fs::unlinkat(&parent.file, &name, AtFlags::empty());
                        return Err(error.into());
                    }
                    let stage = Self {
                        file: Some(file),
                        parent: Some(parent),
                        name,
                        cleanup_armed: true,
                    };
                    let metadata = stage.file()?.metadata()?;
                    validate_privacy(stage.file()?, &metadata, privacy)?;
                    return Ok(stage);
                }
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(privacy.error(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique atomic-write staging file",
        ))
    }

    fn file(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("publication stage lost its file handle"))
    }

    fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("publication stage lost its file handle"))
    }

    fn parent(&self) -> io::Result<&Directory> {
        self.parent
            .as_ref()
            .ok_or_else(|| io::Error::other("publication stage lost its parent handle"))
    }

    fn disarm_cleanup(&mut self) {
        self.cleanup_armed = false;
    }

    fn rearm_cleanup(&mut self) {
        self.cleanup_armed = true;
    }

    #[cfg_attr(
        target_os = "macos",
        expect(
            clippy::cast_sign_loss,
            reason = "mirrors std's `MetadataExt::dev` sign extension of the macOS `dev_t`"
        )
    )]
    fn is_still_named(&self) -> io::Result<bool> {
        let named = unix_fs::statat(&self.parent()?.file, &self.name, AtFlags::SYMLINK_NOFOLLOW)?;
        let held = self.file()?.metadata()?;
        Ok(named.st_dev as u64 == held.dev() && named.st_ino as u64 == held.ino())
    }

    fn into_commit_guard(mut self) -> io::Result<CommitGuard> {
        self.cleanup_armed = false;
        Ok(CommitGuard {
            published_file: self
                .file
                .take()
                .ok_or_else(|| io::Error::other("publication stage lost its file handle"))?,
            parent: self
                .parent
                .take()
                .ok_or_else(|| io::Error::other("publication stage lost its parent handle"))?,
        })
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.cleanup_armed
            && let Some(parent) = self.parent.as_ref()
        {
            // This is safe only under the publication contract's private or
            // cooperatively excluded namespace: no identity-check-then-unlink
            // sequence can make a hostile shared directory race-free.
            let _ = unix_fs::unlinkat(&parent.file, &self.name, AtFlags::empty());
        }
    }
}

pub(crate) fn publish(
    directory: &Directory,
    target: &OsStr,
    bytes: &[u8],
    mode: PublishMode,
    privacy: Privacy,
    exact_mode: Option<u32>,
) -> Result<(), PublicationError> {
    publish_policy(
        directory,
        target,
        bytes,
        mode,
        FilePolicy::Ordinary(privacy),
        exact_mode,
    )
}
pub(crate) fn publish_policy(
    directory: &Directory,
    target: &OsStr,
    bytes: &[u8],
    mode: PublishMode,
    privacy: FilePolicy,
    exact_mode: Option<u32>,
) -> Result<(), PublicationError> {
    validate_component(target).map_err(not_published)?;
    let parent = directory.try_clone().map_err(not_published)?;
    let mut stage = Stage::create(parent, target, privacy, exact_mode).map_err(not_published)?;

    checkpoint_with_stage(CheckpointKind::StageCreated, &stage).map_err(not_published)?;
    stage
        .file_mut()
        .map_err(not_published)?
        .write_all(bytes)
        .map_err(not_published)?;
    checkpoint_with_stage(CheckpointKind::BeforeDataFlush, &stage).map_err(not_published)?;
    stage
        .file()
        .map_err(not_published)?
        .sync_all()
        .map_err(not_published)?;
    checkpoint_with_stage(CheckpointKind::DataFlushed, &stage).map_err(not_published)?;
    checkpoint_with_stage(CheckpointKind::BeforePublish, &stage).map_err(not_published)?;

    // From this point a simulated or native publication result may be
    // ambiguous, so Drop must never unlink the object automatically.
    stage.disarm_cleanup();
    if let Err(source) = checkpoint_with_stage(CheckpointKind::PublishSubmitted, &stage) {
        return Err(PublicationError::new(
            PublicationState::PublicationUnknown,
            source,
        ));
    }

    let parent = stage
        .parent()
        .map_err(|source| PublicationError::new(PublicationState::PublicationUnknown, source))?;
    let rename = match mode {
        PublishMode::Replace => unix_fs::renameat(&parent.file, &stage.name, &parent.file, target),
        PublishMode::IfAbsent => unix_fs::renameat_with(
            &parent.file,
            &stage.name,
            &parent.file,
            target,
            RenameFlags::NOREPLACE,
        ),
    };
    if let Err(error) = rename {
        let source = io::Error::from(error);
        let collision =
            mode == PublishMode::IfAbsent && source.kind() == io::ErrorKind::AlreadyExists;
        if collision || stage.is_still_named().unwrap_or(false) {
            // The unique source name and retained handle prove no publication
            // under the required private/cooperatively excluded namespace.
            // Re-arm cleanup only after that proof.
            stage.rearm_cleanup();
            return Err(not_published(source));
        }
        return Err(PublicationError::new(
            PublicationState::PublicationUnknown,
            source,
        ));
    }

    if let Err(source) = checkpoint(CheckpointKind::AfterPublish) {
        return Err(PublicationError::new(
            PublicationState::PublicationUnknown,
            source,
        ));
    }

    let guard =
        PublicCommitGuard::new(stage.into_commit_guard().map_err(|source| {
            PublicationError::new(PublicationState::PublicationUnknown, source)
        })?);
    if let Err(source) = guard.commit() {
        return Err(PublicationError::pending(source, guard));
    }
    checkpoint(CheckpointKind::Committed).map_err(|source| {
        PublicationError::new(PublicationState::PublishedDurabilityUnknown, source)
    })?;
    Ok(())
}

pub(crate) fn validate_component(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage child name must be one non-special component",
        ));
    }
    Ok(())
}

fn validate_privacy(file: &File, metadata: &Metadata, privacy: FilePolicy) -> io::Result<()> {
    if privacy.is_private() {
        match privacy {
            FilePolicy::Ordinary(_) => validate_private_security(file, metadata)?,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            FilePolicy::BoundedOwner => {
                #[cfg(target_os = "macos")]
                bounded_acl::validate_private(file, metadata)?;
                #[cfg(target_os = "linux")]
                bounded_linux::validate_private(file, metadata)?;
            }
        }
        // Files must have exactly one name. Directories have a separate policy.
        if metadata.nlink() != 1 {
            return Err(privacy.error(
                io::ErrorKind::PermissionDenied,
                "owner-only file has additional hard links",
            ));
        }
    }
    Ok(())
}

fn validate_private_security(file: &File, metadata: &Metadata) -> io::Result<()> {
    validate_owner_mode(metadata)?;
    #[cfg(target_os = "macos")]
    macos_acl::validate(file)?;
    #[cfg(not(target_os = "macos"))]
    let _ = file;
    Ok(())
}
fn validate_owner_mode(metadata: &Metadata) -> io::Result<()> {
    if !crate::native::private::owned_by_me(metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "owner-only object is not owned by the effective user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "owner-only object grants group or other permissions",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos_acl {
    use std::ffi::{c_int, c_void};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    // SDK sys/acl.h and membership.h. macOS uses 0 for successful
    // acl_get_entry, and EINVAL to indicate exhaustion of a valid ACL.
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_EXTENDED_ALLOW: c_int = 1;
    const ACL_EXTENDED_DENY: c_int = 2;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_free(object: *mut c_void) -> c_int;
        fn acl_valid(acl: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, index: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_get_qualifier(entry: *mut c_void) -> *mut c_void;
        fn mbr_uuid_to_id(uuid: *const u8, id: *mut u32, kind: *mut c_int) -> c_int;
    }

    struct Allocation(*mut c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            // SAFETY: owns exactly one acl_get_fd_np/acl_get_qualifier allocation.
            unsafe {
                acl_free(self.0);
            }
        }
    }

    pub(super) fn validate(file: &File) -> io::Result<()> {
        // SAFETY: live held descriptor, public SDK ACL type.
        let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            };
        }
        let acl = Allocation(raw);
        // SAFETY: the allocation remains live throughout traversal.
        if unsafe { acl_valid(acl.0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut selector = 0; // ACL_FIRST_ENTRY
        loop {
            let mut entry = std::ptr::null_mut();
            // SAFETY: valid ACL, documented first/next selector, writable output.
            if unsafe { acl_get_entry(acl.0, selector, &raw mut entry) } != 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(22) {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            selector = -1; // ACL_NEXT_ENTRY
            let mut tag = 0;
            // SAFETY: successful enumeration returned an entry in our live ACL.
            if unsafe { acl_get_tag_type(entry, &raw mut tag) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if tag == ACL_EXTENDED_DENY {
                continue; // A deny cannot widen access, inherited or otherwise.
            }
            if tag != ACL_EXTENDED_ALLOW {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unknown private-file ACL entry type",
                ));
            }
            // SAFETY: allow entry qualifiers are allocated uuid_t values.
            let qualifier = unsafe { acl_get_qualifier(entry) };
            if qualifier.is_null() {
                return Err(io::Error::last_os_error());
            }
            let qualifier = Allocation(qualifier);
            let mut id = 0;
            let mut kind = -1;
            // SAFETY: native UUID qualifier and correctly sized native outputs.
            let status = unsafe { mbr_uuid_to_id(qualifier.0.cast(), &raw mut id, &raw mut kind) };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status));
            }
            // Check all allows, including ONLY_INHERIT entries, conservatively:
            // flags never turn another principal into the trusted effective owner.
            // INHERITED records provenance and does not make an allow harmless.
            if kind != 0 || id != rustix::process::geteuid().as_raw() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private-file ACL grants another principal access",
                ));
            }
        }
    }
}

fn stage_name(target: &OsStr, sequence: u64) -> OsString {
    let mut bytes = Vec::with_capacity(target.as_bytes().len().saturating_add(48));
    bytes.push(b'.');
    bytes.extend_from_slice(target.as_bytes());
    bytes.extend_from_slice(format!(".{}.{sequence}.tmp", std::process::id()).as_bytes());
    OsString::from_vec(bytes)
}

fn not_published(source: io::Error) -> PublicationError {
    PublicationError::new(PublicationState::NotPublished, source)
}

#[derive(Clone, Copy)]
enum CheckpointKind {
    StageCreated,
    BeforeDataFlush,
    DataFlushed,
    BeforePublish,
    PublishSubmitted,
    AfterPublish,
    BeforeCommit,
    Committed,
}

#[cfg(feature = "test-support")]
impl From<CheckpointKind> for Checkpoint {
    fn from(checkpoint: CheckpointKind) -> Self {
        match checkpoint {
            CheckpointKind::StageCreated => Self::StageCreated,
            CheckpointKind::BeforeDataFlush => Self::BeforeDataFlush,
            CheckpointKind::DataFlushed => Self::DataFlushed,
            CheckpointKind::BeforePublish => Self::BeforePublish,
            CheckpointKind::PublishSubmitted => Self::PublishSubmitted,
            CheckpointKind::AfterPublish => Self::AfterPublish,
            CheckpointKind::BeforeCommit => Self::BeforeCommit,
            CheckpointKind::Committed => Self::Committed,
        }
    }
}

#[cfg(feature = "test-support")]
fn checkpoint(kind: CheckpointKind) -> io::Result<()> {
    test_support::checkpoint(&CheckpointEvent {
        checkpoint: kind.into(),
        stage: None,
    })
}

#[cfg(not(feature = "test-support"))]
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the test-support build injects failures through the same signature"
    )
)]
fn checkpoint(_kind: CheckpointKind) -> io::Result<()> {
    Ok(())
}

#[cfg(feature = "test-support")]
fn checkpoint_with_stage(kind: CheckpointKind, stage: &Stage) -> io::Result<()> {
    let file = stage.file()?;
    let metadata = file.metadata()?;
    let identity = crate::native::atomic::FileIdentity::of(file)?;
    test_support::checkpoint(&CheckpointEvent {
        checkpoint: kind.into(),
        stage: Some(StageObservation { metadata, identity }),
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bounded_stage_name(target: &OsStr, sequence: u64) -> OsString {
    fn decimal(mut n: u64, target: &mut Vec<u8>) {
        // u64::MAX has 20 digits, so the loop always ends by `n == 0`.
        let mut digits = [0u8; 20];
        let mut first = digits.len();
        for (index, slot) in digits.iter_mut().enumerate().rev() {
            *slot = b'0'.wrapping_add((n % 10) as u8);
            n /= 10;
            first = index;
            if n == 0 {
                break;
            }
        }
        target.extend_from_slice(digits.get(first..).unwrap_or_default());
    }
    // '.', target, '.', pid<=10, '.', sequence<=20, '.tmp': target+37.
    let mut bytes = Vec::with_capacity(target.as_bytes().len().saturating_add(37));
    bytes.push(b'.');
    bytes.extend_from_slice(target.as_bytes());
    bytes.push(b'.');
    decimal(u64::from(std::process::id()), &mut bytes);
    bytes.push(b'.');
    decimal(sequence, &mut bytes);
    bytes.extend_from_slice(b".tmp");
    OsString::from_vec(bytes)
}

#[cfg(not(feature = "test-support"))]
#[cfg_attr(
    not(feature = "test-support"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the test-support build injects failures through the same signature"
    )
)]
fn checkpoint_with_stage(_kind: CheckpointKind, _stage: &Stage) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_staging_file_is_a_hidden_sibling_component() {
        let target = OsStr::new("state.automerge");
        let first = stage_name(target, 1);
        assert!(first.as_bytes().starts_with(b".state.automerge."));
        assert!(first.as_bytes().ends_with(b".tmp"));
        validate_component(&first).unwrap();
    }

    #[test]
    fn two_writes_do_not_share_a_staging_name() {
        let target = OsStr::new("state.automerge");
        assert_ne!(stage_name(target, 1), stage_name(target, 2));
    }
}
