//! Developer/release-gate helper for putting an Imager profile into a regular
//! media image. This calls the production encoder and read-back verifier; it
//! never accepts a block device.

use std::io;
use std::path::PathBuf;

use crate::model::{Filesystem, InstallProfile};

fn value(args: &[String], name: &str) -> io::Result<String> {
    let position = args
        .iter()
        .position(|arg| arg == name)
        .ok_or_else(|| io::Error::other(format!("missing {name}")))?;
    position
        .checked_add(1)
        .and_then(|value_position| args.get(value_position))
        .cloned()
        .ok_or_else(|| io::Error::other(format!("missing value after {name}")))
}

/// Embed a secret-free profile in a regular media image and verify its bytes.
///
/// # Errors
/// Returns an error for missing arguments, invalid profile values, a non-regular
/// target, insufficient space, or failed write/read-back verification.
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let target = PathBuf::from(value(args, "--target")?);

    let filesystem = match value(args, "--filesystem")?.as_str() {
        "btrfs" => Filesystem::Btrfs,
        "ext4" => Filesystem::Ext4,
        "xfs" => Filesystem::Xfs,
        other => return Err(format!("unsupported filesystem: {other}").into()),
    };
    let profile = InstallProfile {
        mode: crate::model::InstallMode::DiskInstall,
        hostname: value(args, "--hostname")?,
        user: crate::model::UserAccount {
            full_name: value(args, "--full-name")?,
            username: value(args, "--username")?,
            password: String::new(),
            is_admin: true,
            autologin: false,
        },
        locale: value(args, "--locale")?,
        keyboard: value(args, "--keyboard")?,
        timezone: value(args, "--timezone")?,
        filesystem,
        encrypt: false,
        encryption_passphrase: String::new(),
        persist_mb: 0,
        wifi_ssid: String::new(),
        device_label: String::new(),
        enroll_local_identity: false,
    };

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself, so the handle metadata rejects links.
        options.custom_flags(0x0020_0000);
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    return Err("regular-file profile embedding is unsupported on this platform".into());
    let mut file = options.open(&target)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("profile target must be a non-symlink regular file".into());
    }
    crate::profile::write_profile_block_to_file(&mut file, metadata.len(), &profile, &|| false)?;
    println!("embedded and verified profile in {}", target.display());
    Ok(())
}
