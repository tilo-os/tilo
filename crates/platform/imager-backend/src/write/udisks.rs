//! Host-authorized exclusive `UDisks` descriptor acquisition.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
use super::{io, revalidate_target};

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn unmount_udisks_filesystems(
    udisks: &block_devices::udisks::Udisks,
    target: &crate::media_model::TargetDrive,
) -> io::Result<()> {
    for name in std::iter::once(target.name.as_str()).chain(
        target
            .partitions
            .iter()
            .map(|partition| partition.name.as_str()),
    ) {
        let usage: String = udisks
            .block(name)
            .and_then(|proxy| proxy.get_property("IdUsage"))
            .map_err(|error| io::Error::other(format!("UDisks2 safety query failed: {error}")))?;
        if usage != "filesystem" {
            continue;
        }
        let mount_points: Vec<Vec<u8>> = udisks
            .filesystem(name)
            .and_then(|proxy| proxy.get_property("MountPoints"))
            .map_err(|error| {
                io::Error::other(format!("UDisks2 mount-state query failed: {error}"))
            })?;
        if mount_points.is_empty() {
            continue;
        }
        udisks.unmount(name).map_err(|error| {
            io::Error::other(format!("UDisks2 could not unmount {name}: {error}"))
        })?;
    }
    Ok(())
}

/// Ask the host's root-owned `UDisks2` daemon for one descriptor to the exact
/// reviewed device. Polkit owns user authentication; `O_EXCL` prevents a
/// mounted or concurrently opened device from being written.
///
/// # Errors
/// Returns an error if the reviewed device changed, the host service cannot be
/// reached, filesystem inspection or unmounting fails, or device access is denied.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
pub fn open_udisks_device(target: &crate::media_model::TargetDrive) -> io::Result<std::fs::File> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedFd, Value};

    revalidate_target(target)?;
    let udisks = block_devices::udisks::Udisks::system()
        .map_err(|error| io::Error::other(format!("could not reach UDisks2: {error}")))?;
    unmount_udisks_filesystems(&udisks, target)?;
    // Revalidate after unmounting: removable media can disappear while an
    // authorization prompt is on screen. UDisks2 then independently resolves
    // and exclusively opens the same object.
    revalidate_target(target)?;
    let proxy = udisks
        .block(&target.name)
        .map_err(|error| io::Error::other(format!("UDisks2 device lookup failed: {error}")))?;
    let flags = libc::O_EXCL | libc::O_SYNC | libc::O_CLOEXEC;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("flags", Value::from(flags));
    let fd: OwnedFd = proxy
        .call("OpenDevice", &("rw", options))
        .map_err(|error| io::Error::other(format!("UDisks2 denied device access: {error}")))?;
    let std_fd: std::os::fd::OwnedFd = fd.into();
    Ok(std::fs::File::from(std_fd))
}
