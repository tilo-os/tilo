//! Developer/release-gate helper for putting an Imager profile into a regular
//! media image. This calls the production encoder and read-back verifier; it
//! never accepts a block device.

use std::io;
use std::path::PathBuf;

use imager::{DiskLayout, InstallProfile};

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let target = PathBuf::from(value(&args, "--target")?);
    let metadata = std::fs::symlink_metadata(&target)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("profile target must be a non-symlink regular file".into());
    }

    let layout = match value(&args, "--filesystem")?.as_str() {
        "btrfs" => DiskLayout::BtrfsSubvols,
        "ext4" => DiskLayout::WholeDisk,
        other => return Err(format!("unsupported filesystem: {other}").into()),
    };
    let profile = InstallProfile {
        mode: imager::InstallMode::DiskInstall,
        hostname: value(&args, "--hostname")?,
        user: imager::model::UserAccount {
            full_name: value(&args, "--full-name")?,
            username: value(&args, "--username")?,
            password: String::new(),
            is_admin: true,
            autologin: false,
        },
        locale: value(&args, "--locale")?,
        keyboard: value(&args, "--keyboard")?,
        timezone: value(&args, "--timezone")?,
        layout,
        encrypt: false,
        encryption_passphrase: String::new(),
        persist_mb: 0,
    };

    imager::write::write_profile_block(&target, metadata.len(), &profile)?;
    println!("embedded and verified profile in {}", target.display());
    Ok(())
}
