//! Synchronous root-controller bootstrap from the exact selected installation medium.
//!
//! This supplies no worker admission. The one trusted carrier callback reads a
//! bounded fixed receipt and copies ciphertext into independent root-private
//! quarantine. It must return no mount pins/descriptors and start no workers;
//! direct descriptor clones are constrained by that call site, not proven absent
//! by Arc counts. Source/worker admission begins only after successful unmount.
//! No formatting, journal replay, filesystem repair, or existing mount adoption.
use super::{Partition, inventory, mount_id, selected, unique_mount, witness};
use crate::imager_model::TargetDrive;
use crate::media_layout::MediaPartitionBinding;
use crate::setup::migration::PinnedDirectory;
use block_devices::udisks::{ReadOnlyFilesystem, Udisks};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

struct BootLease {
    target: TargetDrive,
    disk: File,
    partition: File,
    selected: Partition,
    media: MediaPartitionBinding,
    root: Option<Arc<PinnedDirectory>>,
    id: Option<u64>,
    daemon: Udisks,
}
impl BootLease {
    /// Cancellation never bypasses the independent physical cleanup checks.
    fn check(&self) -> io::Result<()> {
        crate::setup::read_media::attest_boot_descriptor(&self.target, &self.disk)?;
        let stable = self
            .target
            .stable_id
            .as_deref()
            .ok_or_else(|| refused("Selected medium identity missing"))?;
        if crate::media_layout::binding(&self.disk, self.target.size, stable)? != self.media
            || selected(&inventory()?, &self.disk, &self.media)? != self.selected
        {
            return Err(refused("Selected boot medium changed"));
        }
        let actual = std::fs::symlink_metadata(&self.selected.name)?;
        let held = self.partition.metadata()?;
        if !actual.file_type().is_block_device()
            || !held.file_type().is_block_device()
            || actual.dev() != held.dev()
            || actual.ino() != held.ino()
            || actual.rdev() != held.rdev()
            || super::super::number(&self.partition)? != self.selected.number
        {
            return Err(refused("Selected boot partition changed"));
        }
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| refused("Boot mount root is unconfirmed"))?;
        root.validate_destination()?;
        let id = self
            .id
            .ok_or_else(|| refused("Boot mount identity is unconfirmed"))?;
        attest_root(&root.file, &self.partition, &self.selected.number, id)?;
        witness(root, &self.media, &self.selected.uuid)?;
        // Re-read actual GPT after the mounted-root and filesystem witness checks.
        if crate::media_layout::binding(&self.disk, self.target.size, stable)? != self.media {
            return Err(refused("Selected boot medium GPT changed"));
        }
        crate::setup::read_media::attest_boot_descriptor(&self.target, &self.disk)
    }
}

/// Only this module constructs the capability; paths and caller DTOs cannot.
pub struct BootMedium {
    lease: Option<BootLease>,
}
impl BootMedium {
    /// Read the attested root synchronously. Pins may remain inside the outer
    /// borrowed boot scope; finish enforces their retirement before unmount.
    /// Never start asynchronous workers or return descriptors beyond that scope.
    pub fn with_root<T>(
        &self,
        callback: impl FnOnce(&Arc<PinnedDirectory>, &MediaPartitionBinding) -> io::Result<T>,
    ) -> io::Result<T> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| refused("Boot medium is closed"))?;
        lease.check()?;
        let root = lease
            .root
            .as_ref()
            .ok_or_else(|| refused("Boot medium root missing"))?;
        let answer = callback(root, &lease.media);
        lease.check()?;
        answer
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(mut lease) = self.lease.take() else {
            return Ok(());
        };
        let result = lease.check().and_then(|()| {
            if lease
                .root
                .as_ref()
                .is_none_or(|root| Arc::strong_count(root) != 1)
            {
                Err(refused("Boot medium still has callback custody"))
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            quarantine(lease);
            return Err(error);
        }
        // The synchronous carrier has returned and relinquished all mount FDs.
        // Close our pinned directory before requesting owned unmount.
        lease.root = None;
        if lease.daemon.unmount(&lease.selected.name) == Ok(()) {
            Ok(())
        } else {
            quarantine(lease);
            Err(refused("Selected boot medium owned unmount failed"))
        }
    }
}
impl Drop for BootMedium {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Uncertain mount substitutions and unmount failures retain their authority;
/// they never trigger a best-effort unmount of somebody else's current mount.
fn quarantine(lease: BootLease) {
    static RETAINED: OnceLock<Mutex<Vec<BootLease>>> = OnceLock::new();
    RETAINED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(lease);
}

fn require_raw(file: &File) -> io::Result<()> {
    if file.metadata()?.file_type().is_block_device() {
        Ok(())
    } else {
        Err(refused("Boot medium requires held selected block custody"))
    }
}

fn attest_root(root: &File, partition: &File, number: &str, expected_id: u64) -> io::Result<()> {
    let metadata = root.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.ino() != 2
        || metadata.dev() != partition.metadata()?.rdev()
        || mount_id(root)? != expected_id
    {
        return Err(refused("Selected boot mounted root differs"));
    }
    unique_mount(number, expected_id)?;
    let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: retained directory FD and correctly sized writable native buffer.
    if unsafe { libc::fstatvfs(root.as_raw_fd(), &raw mut flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let safety = libc::ST_RDONLY | libc::ST_NODEV | libc::ST_NOSUID | libc::ST_NOEXEC;
    if flags.f_flag & safety != safety {
        return Err(refused("Selected boot mount is not strictly readonly"));
    }
    Ok(())
}

/// Root-only, synchronous bootstrap before OfflineReadAdmission exists.
/// `held_raw` and `target` come from the canonical selected-medium controller.
pub fn with_boot_medium<T>(
    target: &TargetDrive,
    held_raw: &File,
    cancel: &AtomicBool,
    callback: impl FnOnce(&BootMedium) -> io::Result<T>,
) -> io::Result<T> {
    require_raw(held_raw)?;
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(refused("Boot medium requires the root controller"));
    }
    check_cancel(cancel)?;
    crate::setup::read_media::attest_boot_descriptor(target, held_raw)?;
    let stable = target
        .stable_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| refused("Selected boot medium stable identity missing"))?;
    let media = crate::media_layout::binding(held_raw, target.size, stable)?;
    let before = selected(&inventory()?, held_raw, &media)?;
    if before.mount.is_some() {
        return Err(refused("Boot bootstrap will not adopt a preexisting mount"));
    }
    let daemon = Udisks::system().map_err(|_| refused("Boot mount service unavailable"))?;
    let partition = daemon
        .open_read_only(&before.name)
        .map_err(|_| refused("Selected boot partition custody refused"))?;
    if super::super::number(&partition)? != before.number {
        return Err(refused("Selected boot partition descriptor differs"));
    }
    crate::setup::read_media::attest_boot_descriptor(target, held_raw)?;
    if crate::media_layout::binding(held_raw, target.size, stable)? != media
        || selected(&inventory()?, held_raw, &media)? != before
    {
        return Err(refused("Selected boot medium changed before mount"));
    }
    check_cancel(cancel)?;
    // Duplicate before mounting, so descriptor allocation failure has no mount effect.
    let disk = held_raw.try_clone()?;
    let path = PathBuf::from(
        daemon
            .mount_read_only(&before.name, ReadOnlyFilesystem::Ext4)
            .map_err(|_| refused("Selected boot readonly mount refused"))?,
    );
    let mut expected = before;
    expected.mount = Some(path.clone());
    // From this point every return, cancellation and panic owns cleanup/quarantine.
    let mut medium = BootMedium {
        lease: Some(BootLease {
            target: target.clone(),
            disk,
            partition,
            selected: expected,
            media,
            root: None,
            id: None,
            daemon,
        }),
    };
    let answer = (|| {
        let lease = medium
            .lease
            .as_mut()
            .ok_or_else(|| refused("Boot mount lease missing"))?;
        lease.root = Some(PinnedDirectory::open_absolute(&path)?);
        lease.id = Some(mount_id(
            &lease
                .root
                .as_ref()
                .ok_or_else(|| refused("Boot root missing"))?
                .file,
        )?);
        lease.check()?;
        check_cancel(cancel)?;
        let answer = callback(&medium)?;
        check_cancel(cancel)?;
        Ok(answer)
    })();
    // Cancellation affects the answer, never the independent physical cleanup.
    medium.finish()?;
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boot_bootstrap_refuses_regular_medium_and_unrelated_root_without_native_mount() {
        let regular = tempfile::tempfile().unwrap();
        assert_eq!(
            require_raw(&regular).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let root = File::open("/").unwrap();
        let actual_id = mount_id(&root).unwrap();
        let meta = root.metadata().unwrap();
        let number = format!("{}:{}", libc::major(meta.dev()), libc::minor(meta.dev()));
        assert!(attest_root(&root, &regular, &number, actual_id).is_err());
        assert!(unique_mount(&number, actual_id.checked_add(1).unwrap()).is_err());
        // No successful test here qualifies UDisks mounting, Ext4 flags or a
        // filesystem witness. Those require the selected disposable medium.
    }
}
