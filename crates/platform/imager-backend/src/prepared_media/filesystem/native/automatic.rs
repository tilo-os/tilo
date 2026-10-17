//! Mount only the verified medium's existing EFI filesystem. Never format it.
use super::*;
use crate::prepared_media::check_cancel;
use std::sync::atomic::AtomicBool;
const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    name: String,
    uuid: String,
    partition_uuid: String,
    number: String,
    mount: Option<PathBuf>,
}
fn selected(devices: &[BlockDevice], target: &TargetDrive) -> io::Result<Selection> {
    if !target.is_media_target() || target.stable_id.as_ref().is_none_or(String::is_empty) {
        return Err(changed(
            "Automatic setup requires stable external media custody",
        ));
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
        return Err(changed("Verified medium is missing or ambiguous"));
    };
    let partitions: Vec<_> = disk
        .children
        .iter()
        .filter(|part| {
            part.kind == "part"
                && part
                    .parttype
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case(ESP))
        })
        .collect();
    let [part] = partitions.as_slice() else {
        return Err(changed("Verified medium has no unique EFI filesystem"));
    };
    if part.read_only || !part.children.is_empty() || part.fstype.as_deref() != Some("vfat") {
        return Err(changed("EFI filesystem is unavailable for prepared setup"));
    }
    let required = |value: &Option<String>| {
        value
            .clone()
            .filter(|text| !text.is_empty())
            .ok_or_else(|| changed("EFI filesystem identity is missing"))
    };
    let points = part.all_mountpoints();
    let mount = match points.as_slice() {
        [] => None,
        [only] if Path::new(only).is_absolute() => Some(PathBuf::from(only)),
        _ => return Err(changed("EFI filesystem has ambiguous mounts")),
    };
    Ok(Selection {
        name: part.name.clone(),
        uuid: required(&part.uuid)?,
        partition_uuid: required(&part.partuuid)?,
        number: required(&part.maj_min)?,
        mount,
    })
}
trait Backend {
    fn revalidate(&self, target: &TargetDrive) -> io::Result<()>;
    fn inventory(&self) -> io::Result<Vec<BlockDevice>>;
    fn rescan(&self, target: &TargetDrive) -> io::Result<()>;
    fn mount(&self, device: &str) -> io::Result<PathBuf>;
    fn unmount(&self, device: &str) -> io::Result<()>;
    fn grant(&self, target: &TargetDrive, root: &Path) -> io::Result<MediaFilesystemGrant> {
        MediaFilesystemGrant::resolve(target, root)
    }
}
#[cfg(feature = "flatpak-media-writer")]
struct Native(block_devices::udisks::Udisks);
#[cfg(feature = "flatpak-media-writer")]
impl Backend for Native {
    fn revalidate(&self, target: &TargetDrive) -> io::Result<()> {
        crate::write::revalidate_target(target)
    }
    fn inventory(&self) -> io::Result<Vec<BlockDevice>> {
        inventory()
    }
    fn rescan(&self, target: &TargetDrive) -> io::Result<()> {
        self.0.rescan(&target.dev_path()).map_err(io::Error::other)
    }
    fn mount(&self, device: &str) -> io::Result<PathBuf> {
        // SAFETY: these calls read the current process credentials.
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        self.0
            .mount_setup(device, uid, gid)
            .map(PathBuf::from)
            .map_err(io::Error::other)
    }
    fn unmount(&self, device: &str) -> io::Result<()> {
        self.0.unmount(device).map_err(io::Error::other)
    }
}
struct Mount<'a, B: Backend> {
    backend: &'a B,
    target: &'a TargetDrive,
    expected: Selection,
    root: PathBuf,
    owned: bool,
}
impl<B: Backend> Mount<'_, B> {
    fn validate(&self) -> io::Result<()> {
        self.backend.revalidate(self.target)?;
        let mut current = selected(&self.backend.inventory()?, self.target)?;
        if current.mount.as_deref() != Some(self.root.as_path()) {
            return Err(changed("Owned media mount changed"));
        }
        current.mount.clone_from(&self.expected.mount);
        if current != self.expected {
            return Err(changed("Owned media partition changed"));
        }
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> {
        if self.owned {
            self.validate()?;
            self.backend.unmount(&self.expected.name)?;
            self.owned = false;
        }
        Ok(())
    }
}
impl<B: Backend> Drop for Mount<'_, B> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
fn with_mount<T>(
    backend: &impl Backend,
    target: &TargetDrive,
    cancel: &AtomicBool,
    deliver: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    check_cancel(cancel)?;
    backend.revalidate(target)?;
    backend.rescan(target)?;
    backend.revalidate(target)?;
    let expected = selected(&backend.inventory()?, target)?;
    let (root, owned) = if let Some(root) = &expected.mount {
        (root.clone(), false)
    } else {
        (backend.mount(&expected.name)?, true)
    };
    let mut mount = Mount {
        backend,
        target,
        expected,
        root,
        owned,
    };
    check_cancel(cancel)?;
    mount.validate()?;
    let grant = backend.grant(target, &mount.root)?;
    let result = deliver(&grant)?;
    mount.validate()?;
    mount.finish()?;
    Ok(result)
}
/// Deliver through the existing EFI filesystem and release only our own mount.
///
/// # Errors
/// Refuses changed/ambiguous media, mount permission failure, failed delivery,
/// cancellation or an unmount failure. No fallback formats a partition.
#[cfg(feature = "flatpak-media-writer")]
pub fn deliver<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    write: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    let backend = Native(block_devices::udisks::Udisks::system().map_err(io::Error::other)?);
    with_mount(&backend, target, cancel, write)
}

#[cfg(test)]
mod tests;
