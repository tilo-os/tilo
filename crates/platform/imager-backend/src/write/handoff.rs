//! Installed-root discovery and safe fallback for staged setup handoff.
use std::{
    io::{self, Read},
    path::Path,
};

/// Install staged setup into the installed root of a regular image file.
///
/// # Errors
/// Returns an error if the target is not a regular file, no installed root is
/// found, or metadata lookup, temporary-directory creation, mounting or `install` fails.
pub fn apply_handoff_stage_to_image_file(
    image: &Path,
    staged: &Path,
    install: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(image)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "customization target must be a regular image file",
        ));
    }
    let mountpoint = tempfile::Builder::new()
        .prefix("tilo-imager-root-")
        .tempdir()?;
    if block_devices::installed_root::with_written_root(image, mountpoint.path(), |mnt| {
        install(mnt, staged)
    })? {
        Ok(())
    } else {
        Err(io::Error::other(
            "the image has no installed Tilo root; customization was not applied",
        ))
    }
}

/// Apply the staged tree through the shared native disk boundary.
///
/// # Errors
/// Returns an error if staging is missing, root discovery or installation fails,
/// the image cannot safely use the fallback, or fallback embedding fails.
pub fn apply_handoff_stage_to_device(
    device: &Path,
    staged: &Path,
    image_len: u64,
    device_len: u64,
    install: impl FnOnce(&Path, &Path) -> io::Result<()>,
    embed: impl FnOnce(&Path, u64, u64, &Path) -> io::Result<()>,
) -> io::Result<()> {
    if !staged.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "setup handoff staging directory is missing",
        ));
    }
    let mountpoint = tempfile::Builder::new()
        .prefix("tilo-imager-root-")
        .tempdir()?;
    if block_devices::installed_root::with_written_root(device, mountpoint.path(), |mnt| {
        install(mnt, staged)
    })? {
        Ok(())
    } else {
        handoff_when_root_was_not_found(device, image_len, device_len, staged, embed)
    }
}

/// No installed root was found, either because `losetup` failed or because
/// the loop device had no Tilo filesystem. A block device in that state is
/// an installer stick: the slack handoff is what the live installer reads.
/// A regular file that is a GUID-partition or qcow2 disk image is not.
/// Hiding Wi-Fi and identity in its slack reports success, and a VM boot of
/// that file never reads it.
///
/// # Errors
/// Returns an error if disk-image detection fails, the target is a regular disk
/// image without an inspected root, or `embed` fails.
pub fn handoff_when_root_was_not_found(
    device: &Path,
    image_len: u64,
    device_len: u64,
    staged: &Path,
    embed: impl FnOnce(&Path, u64, u64, &Path) -> io::Result<()>,
) -> io::Result<()> {
    if regular_file_is_disk_image(device)? {
        return Err(io::Error::other(
            "could not inspect the written image for an installed root, so Wi-Fi and identity were not applied",
        ));
    }
    embed(device, image_len, device_len, staged)
}

/// Detect a regular file with a qcow2 header or a primary GPT signature.
///
/// # Errors
/// Returns metadata, open or read errors from inspecting the target.
pub fn regular_file_is_disk_image(device: &Path) -> io::Result<bool> {
    let meta = std::fs::metadata(device)?;
    if !meta.is_file() {
        return Ok(false);
    }
    let mut file = std::fs::File::open(device)?;
    let mut head = [0_u8; 520];
    let read = file.read(&mut head)?;
    if read >= 4 && &head[..4] == b"QFI\xfb" {
        return Ok(true);
    }
    Ok(read >= 520 && &head[512..520] == b"EFI PART")
}
