//! Read-only authentication of prepared installation choices. No persona or
//! account custody is imported, activated or registered by this entry point.
use super::{failed, private_parent};
use crate::setup::{migration::PinnedDirectory, prepared_setup::PreparedSetupReview};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    schema: u32,
    sealed_bytes: u64,
    ciphertext_sha256: String,
}
#[derive(Serialize)]
struct Receipt<'a> {
    schema: u32,
    identity_public_key: [u8; 32],
    setup_binding: &'a str,
    install_profile: &'a Option<crate::setup::prepared_setup::PublicInstallProfile>,
    retained_home: &'a Option<crate::setup::prepared_setup::RetainedHomePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_computer_applications_scope:
        Option<crate::setup::prepared_setup::ComputerApplicationsScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    planned_btrfs_resize: &'a Option<crate::setup::prepared_setup::PlannedBtrfsResize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_home: &'a Option<crate::setup::portable_home_media::Expectation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_setup: Option<&'a Expectation>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    completed_applications_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_applications: Option<&'a crate::setup::flatpak_transfer::portable::Expectation>,
}
fn bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let parent = PinnedDirectory::open_absolute(path.parent().ok_or_else(failed)?)?;
    parent.validate_destination()?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .nonblock(true)
        .custom_flags(libc::O_NOFOLLOW);
    let file = cap_primitives::fs::open(
        &parent.file,
        Path::new(path.file_name().ok_or_else(failed)?),
        &options,
    )?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > maximum {
        return Err(failed());
    }
    let mut bytes = Vec::new();
    (&file).take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != before.len() || file.metadata()?.modified()? != before.modified()? {
        return Err(failed());
    }
    parent.validate()?;
    Ok(bytes)
}
fn verify(package: &Path, receipt: &Expectation) -> io::Result<()> {
    if receipt.schema != 1
        || !(48..=33_570_816).contains(&receipt.sealed_bytes)
        || receipt.ciphertext_sha256.len() != 64
    {
        return Err(failed());
    }
    let bytes = bounded(package, receipt.sealed_bytes)?;
    let digest = hex::encode(Sha256::digest(&bytes));
    if bytes.len() as u64 != receipt.sealed_bytes || digest != receipt.ciphertext_sha256 {
        return Err(failed());
    }
    Ok(())
}
/// Authenticate the delivered package using bounded private stdin, returning
/// only authenticated public installation choices. A fresh private quarantine
/// is required; no identity/account import or active identity mutation occurs.
///
/// # Errors
/// Refuses invalid grants, changed ciphertext, invalid expectations or secrets.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    let mut values = BTreeMap::new();
    let mut verify_source = false;
    let mut args = args.into_iter();
    while let Some(name) = args.next() {
        let name = name.to_str().ok_or_else(failed)?;
        if name == "--verify-retained-source" {
            if verify_source {
                return Err(failed());
            }
            verify_source = true;
            continue;
        }
        if !["--package", "--quarantine", "--receipt"].contains(&name) || values.contains_key(name)
        {
            return Err(failed());
        }
        let path = PathBuf::from(args.next().ok_or_else(failed)?);
        if !path.is_absolute() {
            return Err(failed());
        }
        values.insert(name.to_owned(), path);
    }
    if values.len() != 3 {
        return Err(failed());
    }
    let mut password = Zeroizing::new(Vec::new());
    input.take(4097).read_to_end(&mut password)?;
    if password.is_empty() || password.len() > 4096 || std::str::from_utf8(&password).is_err() {
        return Err(failed());
    }
    let package = values.get("--package").ok_or_else(failed)?;
    let quarantine = values.get("--quarantine").ok_or_else(failed)?;
    private_parent(package)?;
    private_parent(quarantine)?;
    let expectation = values.get("--receipt").ok_or_else(failed)?;
    private_parent(expectation)?;
    let receipt: Expectation =
        serde_json::from_slice(&bounded(expectation, 1024)?).map_err(|_| failed())?;
    verify(package, &receipt)?;
    let review =
        PreparedSetupReview::open(package, &password, quarantine, &AtomicBool::new(false))?;
    verify(package, &receipt)?;
    review.require_completed_export()?;
    let manifest = review.manifest();
    if let Some(retained) = &manifest.retained_home {
        retained.as_plan().bytes()?;
    }
    // Effective geometry comes exclusively from authenticated sealed choices.
    // No public handoff or arbitrary post-install amendment can provide it.
    let effective = manifest
        .planned_btrfs_resize
        .as_ref()
        .map(|resize| resize.effective_plan(manifest.retained_home.as_ref().ok_or_else(failed)?))
        .transpose()?;
    let retained_home = if effective.is_some() {
        &effective
    } else {
        &manifest.retained_home
    };
    // Ordinary unlock also needs actual completed geometry proof for a sealed
    // planned resize. The explicit flag remains the legacy retained-source path.
    let verify_source = verify_source || manifest.planned_btrfs_resize.is_some();
    if verify_source && let Some(retained) = retained_home {
        // Fresh kernel read-only grant verifies the original partition, native
        // home inode and UID. Its lease ends before destructive installation.
        let source = crate::setup::migration_handoff::mount_read_only(retained.as_plan())?;
        let verified = if let Some(resize) = &manifest.planned_btrfs_resize {
            source.verify_completed_btrfs_resize(resize.filesystem_bytes())
        } else {
            Ok(())
        };
        // Cleanup is checked even when the independent device proof refuses.
        let retired = source.retire();
        verified?;
        retired?;
    }
    let receipt = Receipt {
        schema: 1,
        identity_public_key: manifest.identity_public_key,
        setup_binding: &manifest.setup_binding,
        install_profile: &manifest.install_profile,
        retained_home,
        pending_computer_applications_scope: manifest.pending_computer_applications_scope()?,
        planned_btrfs_resize: &manifest.planned_btrfs_resize,
        completed_home: &manifest.completed_home,
        completed_setup: (manifest.completed_home.is_some()
            || manifest.completed_applications_only)
            .then_some(&receipt),
        completed_applications_only: manifest.completed_applications_only,
        completed_applications: (manifest.completed_home.is_some()
            || manifest.completed_applications_only)
            .then_some(manifest.portable_flatpak.as_ref())
            .flatten(),
    };
    serde_json::to_writer(&mut *output, &receipt).map_err(|_| failed())?;
    output.write_all(b"\n")?;
    output.flush()
}
