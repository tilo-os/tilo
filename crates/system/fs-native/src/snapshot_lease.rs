//! Broker-owned raw-tree snapshot leases. No replication filters are applied.
//!
//! The broker must durably record the operation intent before creation and the
//! returned receipt before publishing readiness. This module is not an app
//! capability or a sandbox: the broker must confine a consumer to the selected
//! subtree before passing a handle. Ancestor snapshot roots remain private.

use crate::btrfs::{self, SubvolumeIdentity};
use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

const MAX_ENTRIES: usize = 1_000_000;
const MAX_DEPTH: usize = 128;
const MAX_LIFETIME: Duration = Duration::from_secs(3600);

fn refusal(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "capture cancelled",
        ));
    }
    Ok(())
}

fn child(parent: &File, name: &OsStr) -> io::Result<File> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(refusal("capture path must contain only normal entries"));
    }
    let name = CString::new(bytes).map_err(|_| refusal("invalid capture entry"))?;
    // SAFETY: parent is held, name is NUL-terminated, and flags never create a file.
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
    // SAFETY: successful openat returned a fresh descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn subtree(root: &File, relative: &Path) -> io::Result<File> {
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(refusal("selected subtree must be a relative normal path"));
        };
        directory = child(&directory, name)?;
    }
    Ok(directory)
}

/// Kernel mount identity of an already held file or directory descriptor.
/// Missing kernel evidence is an error, never permission to cross mounts.
/// This identifies mounts, not a portable filesystem or authorization grant.
pub fn mount_id(directory: &File) -> io::Result<u64> {
    // SAFETY: zero is valid output storage and an empty string plus AT_EMPTY_PATH
    // identifies the held descriptor, not an ambient pathname.
    let mut value: libc::statx = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::statx(
            directory.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &raw mut value,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if value.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(refusal("kernel cannot attest capture mount boundaries"));
    }
    Ok(value.stx_mnt_id)
}

fn same_directory(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.is_dir()
        && right.is_dir()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid())
}

fn private_parent(parent: &File) -> io::Result<()> {
    let metadata = parent.metadata()?;
    // SAFETY: geteuid has no arguments and reads process credentials.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(refusal("capture store must be private to its broker"));
    }
    Ok(())
}

fn capacity(parent: &File, reserve: u64) -> io::Result<()> {
    // SAFETY: fstatvfs fills the output for a held descriptor.
    let mut value: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(parent.as_raw_fd(), &raw mut value) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if value.f_bavail.saturating_mul(value.f_frsize) < reserve {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "capture CoW reserve exhausted",
        ));
    }
    Ok(())
}

/// Rejects unsupported component captures instead of omitting their data.
fn single_component(root: &File, cancel: &AtomicBool) -> io::Result<()> {
    let identity = btrfs::subvolume_identity(root.as_fd())?;
    let mount = mount_id(root)?;
    let mut pending = vec![(root.try_clone()?, 0usize)];
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        cancelled(cancel)?;
        if depth > MAX_DEPTH {
            return Err(refusal("capture depth exceeds supported bound"));
        }
        // /proc/self/fd refers to this held directory, never an untrusted path.
        for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
            cancelled(cancel)?;
            entries = entries.saturating_add(1);
            if entries > MAX_ENTRIES {
                return Err(refusal("capture inventory exceeds supported bound"));
            }
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let next = child(&directory, &entry.file_name())?;
            if next.metadata()?.ino() == 2
                || mount_id(&next)? != mount
                || btrfs::subvolume_identity(next.as_fd())?.id != identity.id
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "home contains a nested subvolume or mounted folder requiring another capture provider",
                ));
            }
            pending.push((next, depth.saturating_add(1)));
            if pending.len() > 4096 {
                return Err(refusal("capture directory fanout exceeds handle budget"));
            }
        }
    }
    Ok(())
}

/// Non-secret durable custody receipt; the broker persists this under its own
/// authenticated operation record, never accepts an untrusted claim as authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub name: String,
    pub source: SubvolumeIdentity,
    pub snapshot: SubvolumeIdentity,
    pub selected_relative: PathBuf,
    pub selected_inode: u64,
    pub selected_owner: u32,
    pub expires: SystemTime,
    pub reserve_bytes: u64,
}

/// Bounds for one caller-authorized capture, independent of transfer limits.
pub struct Request {
    pub name: String,
    pub selected_relative: PathBuf,
    pub lifetime: Duration,
    pub reserve_bytes: u64,
}

/// Holds only broker-owned capture custody. Drop attempts exact-ID cleanup;
/// failed cleanup must also be recovered from the broker's durable receipt.
pub struct SnapshotLease {
    parent: File,
    root: File,
    selected: File,
    receipt: Receipt,
    retained: bool,
}

impl SnapshotLease {
    /// The supplied source is a subvolume root; `approved` is the original held
    /// HOME grant. Ancestor capture never expands the exported subtree authority.
    pub fn create(
        source: &File,
        approved: &File,
        parent: File,
        request: Request,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        cancelled(cancel)?;
        private_parent(&parent)?;
        if request.lifetime.is_zero()
            || request.lifetime > MAX_LIFETIME
            || request.reserve_bytes == 0
            || source.metadata()?.ino() != 256
        {
            return Err(refusal("invalid capture bounds or source root"));
        }
        let selected = subtree(source, &request.selected_relative)?;
        if !same_directory(&selected, approved)? {
            return Err(refusal("capture subtree is not the approved home"));
        }
        single_component(&selected, cancel)?;
        capacity(&parent, request.reserve_bytes)?;
        let identity = btrfs::subvolume_identity(source.as_fd())?;
        if identity.uuid == [0; 16] {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source lacks a durable subvolume UUID",
            ));
        }
        let expires = SystemTime::now()
            .checked_add(request.lifetime)
            .ok_or_else(|| refusal("capture expiry overflow"))?;
        btrfs::snapshot_readonly_at(source.as_fd(), parent.as_fd(), OsStr::new(&request.name))?;
        let root = child(&parent, OsStr::new(&request.name))?;
        let snapshot = btrfs::subvolume_identity(root.as_fd())?;
        let selected_metadata = approved.metadata()?;
        let receipt = Receipt {
            name: request.name,
            source: identity,
            snapshot,
            selected_relative: request.selected_relative,
            selected_inode: selected_metadata.ino(),
            selected_owner: selected_metadata.uid(),
            expires,
            reserve_bytes: request.reserve_bytes,
        };
        let mut lease = Self {
            parent,
            selected: root.try_clone()?,
            root,
            receipt,
            retained: false,
        };
        lease.selected = subtree(&lease.root, &lease.receipt.selected_relative)?;
        lease.validate(cancel)?;
        // Check the frozen tree too: nested subvolumes created during preflight
        // are stubs in a parent snapshot, and must never become silent omissions.
        single_component(&lease.selected, cancel)?;
        single_component(&selected, cancel)?;
        if !same_directory(
            &subtree(source, &lease.receipt.selected_relative)?,
            approved,
        )? {
            return Err(refusal("approved home changed during capture"));
        }
        Ok(lease)
    }

    /// Restore only a receipt held in the broker's durable operation record.
    pub fn restore(parent: File, receipt: Receipt, cancel: &AtomicBool) -> io::Result<Self> {
        private_parent(&parent)?;
        let root = child(&parent, OsStr::new(&receipt.name))?;
        // Do not attach cleanup custody to an unverified replacement entry.
        if btrfs::subvolume_identity(root.as_fd())? != receipt.snapshot {
            return Err(refusal("retained capture identity changed"));
        }
        let selected = subtree(&root, &receipt.selected_relative)?;
        let lease = Self {
            parent,
            root,
            selected,
            receipt,
            retained: false,
        };
        lease.validate(cancel)?;
        Ok(lease)
    }

    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// Propose an extension of this exact epoch; the broker durably publishes
    /// the receipt before committing it. This never revives an expired lease.
    /// # Errors
    /// Refuses cancellation, changed provenance, exhausted reserve or an
    /// extension outside the bounded native lease lifetime.
    pub fn prepare_renewal(&self, lifetime: Duration, cancel: &AtomicBool) -> io::Result<Receipt> {
        self.validate(cancel)?;
        if lifetime.is_zero() || lifetime > MAX_LIFETIME {
            return Err(refusal("capture renewal lifetime exceeds bound"));
        }
        let expires = SystemTime::now()
            .checked_add(lifetime)
            .ok_or_else(|| refusal("capture renewal deadline overflows"))?;
        if expires <= self.receipt.expires {
            return Err(refusal("capture renewal must extend its deadline"));
        }
        let mut receipt = self.receipt.clone();
        receipt.expires = expires;
        self.validate(cancel)?;
        Ok(receipt)
    }

    /// Adopt a previously durably published renewal of the same capture.
    /// # Errors
    /// Refuses changed identity, source, reserve, selection, cancellation,
    /// expired custody or a deadline beyond the bounded renewal horizon.
    pub fn commit_renewal(&mut self, renewal: &Receipt, cancel: &AtomicBool) -> io::Result<()> {
        self.validate(cancel)?;
        let mut expected = self.receipt.clone();
        expected.expires = renewal.expires;
        let now = SystemTime::now();
        if expected != *renewal
            || renewal.expires <= self.receipt.expires
            || renewal.expires.duration_since(now).is_err()
            || renewal.expires
                > now
                    .checked_add(MAX_LIFETIME)
                    .ok_or_else(|| refusal("capture renewal horizon overflows"))?
        {
            return Err(refusal(
                "capture renewal changed authenticated provenance or bounds",
            ));
        }
        self.receipt = renewal.clone();
        self.validate(cancel)
    }

    /// Broker-only. Confine consumers through the canonical launcher before
    /// passing this handle; a directory descriptor alone is not a sandbox.
    pub fn selected(&self) -> &File {
        &self.selected
    }

    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        cancelled(cancel)?;
        private_parent(&self.parent)?;
        capacity(&self.parent, self.receipt.reserve_bytes)?;
        let current = btrfs::subvolume_identity(self.root.as_fd())?;
        let named = child(&self.parent, OsStr::new(&self.receipt.name))?;
        let metadata = self.selected.metadata()?;
        if SystemTime::now() >= self.receipt.expires
            || !current.readonly
            || current != self.receipt.snapshot
            || current.parent_uuid != self.receipt.source.uuid
            || btrfs::subvolume_identity(named.as_fd())? != current
            || metadata.ino() != self.receipt.selected_inode
            || metadata.uid() != self.receipt.selected_owner
        {
            return Err(refusal("capture expired or provenance changed"));
        }
        Ok(())
    }

    /// Keeps the snapshot for crash/retry recovery after its receipt is durable.
    pub fn retain(mut self) -> Receipt {
        self.retained = true;
        self.receipt.clone()
    }

    /// Explicit completion/cancellation cleanup; failures remain actionable.
    pub fn release(mut self) -> io::Result<()> {
        btrfs::destroy_snapshot_id(
            self.parent.as_fd(),
            self.root.as_fd(),
            &self.receipt.snapshot,
        )?;
        self.retained = true;
        Ok(())
    }
}

impl Drop for SnapshotLease {
    fn drop(&mut self) {
        if !self.retained {
            let _ = btrfs::destroy_snapshot_id(
                self.parent.as_fd(),
                self.root.as_fd(),
                &self.receipt.snapshot,
            );
        }
    }
}
