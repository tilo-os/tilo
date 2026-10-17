//! Format only the exact newly provisioned GPT partition, then grant its mount.
use super::*;
use crate::migration_durability as durability;
use crate::prepared_media::{DIRECTORY, check_cancel};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::sync::atomic::AtomicBool;
#[cfg(feature = "flatpak-media-writer")]
struct Native(block_devices::udisks::Udisks);
use crate::media_layout::{DATA_PARTITION_TYPE, MediaPartitionBinding};
use std::fs::File;
#[cfg(not(test))]
use std::os::unix::fs::MetadataExt as _;
mod witness;
#[cfg(feature = "flatpak-media-writer")]
const COLUMNS_HOME: &str =
    "NAME,TYPE,SIZE,START,RO,FSTYPE,UUID,PARTUUID,PARTTYPE,MAJ:MIN,MOUNTPOINTS";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Partition {
    name: String,
    filesystem: Option<String>,
    uuid: Option<String>,
    number: String,
    mount: Option<PathBuf>,
}
fn selection(
    devices: &[BlockDevice],
    target: &TargetDrive,
    binding: &MediaPartitionBinding,
) -> io::Result<Partition> {
    if !target.is_media_target()
        || target.stable_id.as_deref() != Some(binding.target_stable_id.as_str())
    {
        return Err(changed("Migration data medium custody changed"));
    }
    let disks: Vec<_> = devices
        .iter()
        .filter(|disk| {
            disk.name == target.dev_path()
                && disk.kind == "disk"
                && disk.size == target.size
                && !disk.read_only
        })
        .collect();
    let [disk] = disks.as_slice() else {
        return Err(changed("Migration data medium is missing or ambiguous"));
    };
    let matches: Vec<_> = disk
        .children
        .iter()
        .filter(|part| {
            part.partuuid
                .as_deref()
                .is_some_and(|guid| guid.eq_ignore_ascii_case(&binding.partition_guid))
        })
        .collect();
    let [part] = matches.as_slice() else {
        return Err(changed(
            "Declared migration partition is missing or ambiguous",
        ));
    };
    let size = binding
        .last_lba
        .checked_sub(binding.first_lba)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(512))
        .ok_or_else(|| changed("Migration partition geometry overflow"))?;
    if part.kind != "part"
        || part.read_only
        || !part.children.is_empty()
        || part.start != Some(binding.first_lba)
        || part.size != size
        || !part
            .parttype
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(DATA_PARTITION_TYPE))
    {
        return Err(changed("Migration partition geometry or type changed"));
    }
    if part.fstype.as_deref().is_some_and(|value| value != "ext4") {
        return Err(changed(
            "Declared migration partition already contains an incompatible filesystem",
        ));
    }
    let points = part.all_mountpoints();
    let mount = match points.as_slice() {
        [] => None,
        [only] if Path::new(only).is_absolute() => Some(PathBuf::from(only)),
        _ => return Err(changed("Migration data filesystem has ambiguous mounts")),
    };
    if part.fstype.is_none() && mount.is_some() {
        return Err(changed("Unformatted migration partition is mounted"));
    }
    Ok(Partition {
        name: part.name.clone(),
        filesystem: part.fstype.clone(),
        uuid: part.uuid.clone(),
        number: part
            .maj_min
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| changed("Migration partition device identity missing"))?,
        mount,
    })
}
trait DataBackend {
    fn revalidate(&self, target: &TargetDrive) -> io::Result<()>;
    fn raw(&self, target: &TargetDrive) -> io::Result<File>;
    fn inventory(&self) -> io::Result<Vec<BlockDevice>>;
    fn rescan(&self, target: &TargetDrive) -> io::Result<()>;
    fn format(&self, device: &str) -> io::Result<()>;
    fn mount(&self, device: &str) -> io::Result<PathBuf>;
    fn unmount(&self, device: &str) -> io::Result<()>;
    fn grant(&self, target: &TargetDrive, root: &Path) -> io::Result<MediaFilesystemGrant> {
        MediaFilesystemGrant::resolve(target, root)
    }
}
#[cfg(feature = "flatpak-media-writer")]
impl DataBackend for Native {
    fn revalidate(&self, target: &TargetDrive) -> io::Result<()> {
        crate::write::revalidate_target(target)
    }
    fn raw(&self, target: &TargetDrive) -> io::Result<File> {
        let raw = self
            .0
            .open_read_only(&target.dev_path())
            .map_err(io::Error::other)?;
        crate::write::attest_target_descriptor(target, &raw)?;
        Ok(raw)
    }
    fn inventory(&self) -> io::Result<Vec<BlockDevice>> {
        Lsblk::new(COLUMNS_HOME)
            .full_paths(true)
            .list()
            .ok_or_else(|| changed("Cannot attest migration data partition"))
    }
    fn rescan(&self, target: &TargetDrive) -> io::Result<()> {
        self.0.rescan(&target.dev_path()).map_err(io::Error::other)
    }
    fn format(&self, device: &str) -> io::Result<()> {
        self.0.format_migration(device).map_err(io::Error::other)
    }
    fn mount(&self, device: &str) -> io::Result<PathBuf> {
        self.0
            .mount_migration(device)
            .map(PathBuf::from)
            .map_err(io::Error::other)
    }
    fn unmount(&self, device: &str) -> io::Result<()> {
        self.0.unmount(device).map_err(io::Error::other)
    }
    fn grant(&self, target: &TargetDrive, root: &Path) -> io::Result<MediaFilesystemGrant> {
        use std::os::fd::AsRawFd;
        let grant = MediaFilesystemGrant::resolve(target, root)?;
        let mut flags = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: live held root descriptor and writable statvfs buffer.
        if unsafe { libc::fstatvfs(grant.pin.file.as_raw_fd(), flags.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatvfs initialized the structure.
        let flags = unsafe { flags.assume_init() }.f_flag;
        let required = libc::ST_NODEV | libc::ST_NOSUID | libc::ST_NOEXEC;
        if flags & required != required || flags & libc::ST_RDONLY != 0 {
            return Err(changed(
                "Migration media must be writable with nodev,nosuid,noexec",
            ));
        }
        Ok(grant)
    }
}
fn attest(
    backend: &impl DataBackend,
    target: &TargetDrive,
    raw: &File,
    expected: &MediaPartitionBinding,
) -> io::Result<Partition> {
    backend.revalidate(target)?;
    if crate::media_layout::binding(raw, target.size, &expected.target_stable_id)? != *expected {
        return Err(changed("Held migration GPT changed"));
    }
    let devices = backend.inventory()?;
    #[cfg(not(test))]
    {
        let actual = raw.metadata()?.rdev();
        let number = format!("{}:{}", libc::major(actual), libc::minor(actual));
        if !devices.iter().any(|disk| {
            disk.name == target.dev_path() && disk.maj_min.as_deref() == Some(number.as_str())
        }) {
            return Err(changed(
                "Held descriptor no longer identifies selected media",
            ));
        }
    }
    selection(&devices, target, expected)
}
struct DataMount<'a, B: DataBackend> {
    backend: &'a B,
    target: &'a TargetDrive,
    raw: &'a File,
    binding: &'a MediaPartitionBinding,
    partition: Partition,
    root: PathBuf,
    owned: bool,
}
impl<B: DataBackend> DataMount<'_, B> {
    fn validate(&self) -> io::Result<()> {
        let mut current = attest(self.backend, self.target, self.raw, self.binding)?;
        if current.mount.as_deref() != Some(self.root.as_path()) {
            return Err(changed("Migration data mount changed"));
        }
        current.mount.clone_from(&self.partition.mount);
        if current != self.partition {
            return Err(changed("Migration data filesystem identity changed"));
        }
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> {
        if self.owned {
            self.validate()?;
            self.backend.unmount(&self.partition.name)?;
            self.owned = false;
        }
        Ok(())
    }
}
impl<B: DataBackend> Drop for DataMount<'_, B> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
fn format_partition(
    backend: &impl DataBackend,
    target: &TargetDrive,
    raw: &File,
    binding: &MediaPartitionBinding,
    previous: &Partition,
    cancel: &AtomicBool,
) -> io::Result<Partition> {
    check_cancel(cancel)?;
    let before = attest(backend, target, raw, binding)?;
    let mut expected = previous.clone();
    expected.mount = None;
    if before != expected || before.mount.is_some() {
        return Err(changed("Migration partition changed before formatting"));
    }
    backend.format(&before.name)?;
    backend.rescan(target)?;
    let after = attest(backend, target, raw, binding)?;
    if after.name != before.name
        || after.number != before.number
        || after.filesystem.as_deref() != Some("ext4")
        || after.uuid.as_ref().is_none_or(String::is_empty)
        || after.mount.is_some()
    {
        return Err(changed(
            "Formatted migration partition failed independent attestation",
        ));
    }
    Ok(after)
}
fn with_data<T>(
    backend: &impl DataBackend,
    target: &TargetDrive,
    cancel: &AtomicBool,
    write: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    check_cancel(cancel)?;
    backend.revalidate(target)?;
    let raw = backend.raw(target)?;
    let stable = target
        .stable_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| changed("Migration medium has no stable identity"))?;
    let binding = crate::media_layout::binding(&raw, target.size, stable)?;
    backend.rescan(target)?;
    let mut partition = attest(backend, target, &raw, &binding)?;
    let mut fresh = partition.filesystem.is_none();
    if fresh {
        partition = format_partition(backend, target, &raw, &binding, &partition, cancel)?;
    }
    if partition.uuid.as_ref().is_none_or(String::is_empty) {
        return Err(changed("Migration filesystem UUID missing"));
    }
    check_cancel(cancel)?;
    let (root, owned) = if let Some(root) = &partition.mount {
        (root.clone(), false)
    } else {
        (backend.mount(&partition.name)?, true)
    };
    let mut mount = DataMount {
        backend,
        target,
        raw: &raw,
        binding: &binding,
        partition,
        root,
        owned,
    };
    check_cancel(cancel)?;
    mount.validate()?;
    if !fresh && !witness::matches(&mount.root, &binding, &mount.partition)? {
        // A new GPT identity cannot inherit ciphertext from an old filesystem.
        // The already authorized whole-drive rewrite permits replacing it.
        check_cancel(cancel)?;
        mount.validate()?;
        backend.unmount(&mount.partition.name)?;
        mount.owned = false;
        let previous = mount.partition.clone();
        drop(mount);
        let partition = format_partition(backend, target, &raw, &binding, &previous, cancel)?;
        let root = backend.mount(&partition.name)?;
        mount = DataMount {
            backend,
            target,
            raw: &raw,
            binding: &binding,
            partition,
            root,
            owned: true,
        };
        mount.validate()?;
        fresh = true;
    }
    check_cancel(cancel)?;
    let grant = backend.grant(target, &mount.root)?;
    if fresh {
        witness::publish(&grant, &binding, &mount.partition)?;
    }
    if !witness::matches(&mount.root, &binding, &mount.partition)? {
        return Err(changed("Migration filesystem ownership witness missing"));
    }
    let result = write(&grant)?;
    mount.validate()?;
    if !witness::matches(&mount.root, &binding, &mount.partition)? {
        return Err(changed(
            "Migration filesystem witness changed during delivery",
        ));
    }
    drop(grant);
    mount.finish()?;
    Ok(result)
}
/// Deliver whole-home ciphertext through the verified new data partition.
///
/// # Errors
/// Refuses missing GPT proof, substituted media, incompatible existing data,
/// denied formatting/mounting, cancellation, or failed delivery/read-back.
#[cfg(feature = "flatpak-media-writer")]
pub fn deliver_home<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    write: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    let backend = Native(block_devices::udisks::Udisks::system().map_err(io::Error::other)?);
    with_data(&backend, target, cancel, write)
}
/// Authenticated root helper retrieval: never format, repair or adopt a mount.
///
/// # Errors
/// Returns an error unless running as root with the expected, unmounted ext4
/// partition. Also returns errors for cancellation, failed device/GPT or grant
/// validation, unavailable system services, mount/unmount failures, missing or
/// changed ownership witnesses, or a failed read callback.
#[cfg(feature = "flatpak-media-writer")]
pub fn with_existing_home<T>(
    target: &TargetDrive,
    expected: &MediaPartitionBinding,
    cancel: &AtomicBool,
    read: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(changed("Protected pending retrieval requires root helper"));
    }
    check_cancel(cancel)?;
    let backend = Native(block_devices::udisks::Udisks::system().map_err(io::Error::other)?);
    DataBackend::revalidate(&backend, target)?;
    let raw = backend.raw(target)?;
    let partition = attest(&backend, target, &raw, expected)?;
    if partition.filesystem.as_deref() != Some("ext4") || partition.mount.is_some() {
        return Err(changed(
            "Protected pending retrieval requires an unmounted existing ext4 partition",
        ));
    }
    let root = DataBackend::mount(&backend, &partition.name)?;
    let mut mount = DataMount {
        backend: &backend,
        target,
        raw: &raw,
        binding: expected,
        partition,
        root,
        owned: true,
    };
    mount.validate()?;
    if !witness::matches(&mount.root, expected, &mount.partition)? {
        return Err(changed("Protected pending filesystem witness differs"));
    }
    let grant = DataBackend::grant(&backend, target, &mount.root)?;
    grant.validate_offline_media(&raw, expected)?;
    let result = read(&grant);
    mount.validate()?;
    if !witness::matches(&mount.root, expected, &mount.partition)? {
        return Err(changed("Protected pending filesystem witness changed"));
    }
    grant.validate_offline_media(&raw, expected)?;
    drop(grant);
    mount.finish()?;
    result
}
#[cfg(test)]
mod tests;
