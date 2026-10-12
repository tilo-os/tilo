//! Installed copy scratch admission. No medium, receiver or completion proof.
//! The producer must retain actual source, owner and scratch operation custody
//! until the original worker has stopped and target-owner import has finished.
use super::{contains, writable_directory};
use crate::owned_service::ServiceAuthority;
use runtime_portal::process_binding::ExecutingPeer;
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
};

#[derive(Clone, PartialEq, Eq)]
pub struct InstalledCopyOutputBinding {
    pub operation: String,
    pub attempt: String,
    pub read_claim_digest: [u8; 32],
    pub setup_digest: [u8; 32],
    pub person: [u8; 32],
    pub source: (u64, u64),
    pub output: (u64, u64),
    pub private_parent: (u64, u64),
    pub original_uid: u32,
    pub target_uid: u32,
    pub reserve_bytes: u64,
    pub output_mount_id: u64,
    pub namespace: (u64, u64),
    pub partition_rdev: u64,
    pub disk_rdev: u64,
    pub filesystem_uuid: [u8; 16],
}
pub struct InstalledCopyOutputFiles {
    pub source: File,
    pub output: File,
    pub private_parent: File,
    pub namespace: File,
}
/// Implemented by the canonical installed root controller ONLY after protected
/// setup/source review and actual target QPE/person proof. Bare UI paths or an
/// implementation of this trait alone are not authenticated operation authority.
pub trait InstalledCopyOutputCustody: ServiceAuthority {
    fn binding(&self) -> io::Result<InstalledCopyOutputBinding>;
    fn files(&self) -> io::Result<InstalledCopyOutputFiles>;
    fn target_peer(&self) -> io::Result<Arc<ExecutingPeer>>;
    /// Defaults refuse. The actual canonical Store/source scratch owner must
    /// replay ALL protected Home, mounted and Flatpak physical-source geometry
    /// against the held target partition/Btrfs members on every check. Device
    /// numbers or caller-authored binding fields alone confer no disjointness.
    /// Actual durable export phase requires new allocation. Independent
    /// verification reads already allocated ciphertext and requires zero more.
    /// Defaults refuse; caller-authored phase flags cannot reduce a reservation.
    fn additional_output_reservation(&self) -> io::Result<u64> {
        Err(refuse())
    }
    fn check_physical_and_operation_custody(&self) -> io::Result<()> {
        Err(refuse())
    }
}
pub struct InstalledCopyOutputAdmission {
    custody: Arc<dyn InstalledCopyOutputCustody>,
    binding: InstalledCopyOutputBinding,
    files: InstalledCopyOutputFiles,
    peer: Arc<ExecutingPeer>,
}
fn refuse() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn identity(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    if !m.is_dir() {
        return Err(refuse());
    }
    Ok((m.dev(), m.ino()))
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn capacity(file: &File) -> io::Result<u64> {
    let mut out = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: held directory descriptor and writable statvfs result.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), out.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the result.
    let out = unsafe { out.assume_init() };
    if out.f_flag & libc::ST_RDONLY != 0 || out.f_frsize == 0 {
        return Err(refuse());
    }
    out.f_bavail.checked_mul(out.f_frsize).ok_or_else(refuse)
}
fn mount_id(file: &File) -> io::Result<u64> {
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_DONT_SYNC,
            libc::STATX_MNT_ID,
            &raw mut stat,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(refuse());
    }
    Ok(stat.stx_mnt_id)
}

impl InstalledCopyOutputAdmission {
    pub fn admit(custody: Arc<dyn InstalledCopyOutputCustody>) -> io::Result<Self> {
        // SAFETY: observes real/effective/fs uid without changing it.
        if unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || unsafe { libc::setfsuid(u32::MAX) } != 0
        {
            return Err(refuse());
        }
        custody.check()?;
        let binding = custody.binding()?;
        if !uuid(&binding.operation)
            || !uuid(&binding.attempt)
            || binding.read_claim_digest == [0; 32]
            || binding.setup_digest == [0; 32]
            || binding.person == [0; 32]
            || binding.original_uid == 0
            || binding.target_uid == 0
            || binding.reserve_bytes == 0
            || binding.output_mount_id == 0
            || binding.namespace.1 == 0
            || binding.partition_rdev == 0
            || binding.disk_rdev == 0
            || binding.filesystem_uuid == [0; 16]
        {
            return Err(refuse());
        }
        let files = custody.files()?;
        let peer = custody.target_peer()?;
        let result = Self {
            custody,
            binding,
            files,
            peer,
        };
        result.check()?;
        let additional = result.custody.additional_output_reservation()?;
        if additional > result.binding.reserve_bytes || additional > capacity(&result.files.output)?
        {
            return Err(refuse());
        }
        Ok(result)
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        self.custody.check()?;
        self.custody.check_physical_and_operation_custody()?;
        let peer = self.custody.target_peer()?;
        if self.custody.binding()? != self.binding
            || !Arc::ptr_eq(&peer, &self.peer)
            || !self.peer.current()
            || self.peer.uid() != self.binding.target_uid
        {
            return Err(refuse());
        }
        let current_namespace = std::fs::metadata("/proc/self/ns/mnt")?;
        let held_namespace = self.files.namespace.metadata()?;
        if (current_namespace.dev(), current_namespace.ino()) != self.binding.namespace
            || (held_namespace.dev(), held_namespace.ino()) != self.binding.namespace
            || mount_id(&self.files.output)? != self.binding.output_mount_id
            || mount_id(&self.files.private_parent)? != self.binding.output_mount_id
        {
            return Err(refuse());
        }
        let private = tilo_files::native::atomic::StorageDirectory::from_file(
            self.files.private_parent.try_clone()?,
        )?;
        private.validate_namespace_control()?;
        private.validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)?;
        let parent = self.files.private_parent.metadata()?;
        let output = self.files.output.metadata()?;
        if identity(&self.files.source)? != self.binding.source
            || identity(&self.files.output)? != self.binding.output
            || identity(&self.files.private_parent)? != self.binding.private_parent
            || parent.uid() != 0
            || parent.mode() & 0o7777 != 0o700
            || output.uid() != self.binding.original_uid
            || output.mode() & 0o7777 != 0o700
            || output.dev() != parent.dev()
            || !contains(&self.files.private_parent, &self.files.output)?
            || contains(&self.files.source, &self.files.output)?
            || contains(&self.files.output, &self.files.source)?
        {
            return Err(refuse());
        }
        writable_directory(&self.files.output)?;
        self.custody.check_physical_and_operation_custody()?;
        self.custody.check()
    }
    pub(super) fn bind_source(&self, source: &File, uid: u32, digest: [u8; 32]) -> io::Result<()> {
        self.check()?;
        if identity(source)? != self.binding.source
            || uid != self.binding.original_uid
            || digest != self.binding.read_claim_digest
        {
            return Err(refuse());
        }
        Ok(())
    }
    pub(super) fn output(&self) -> io::Result<File> {
        self.check()?;
        self.files.output.try_clone()
    }
}
