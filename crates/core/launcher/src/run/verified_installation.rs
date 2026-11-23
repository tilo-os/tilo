//! The installation a launch runs, verified on every launch: its files, its signed
//! record and its publisher, not revoked.

use std::path::Path;

use super::{LaunchError, ManifestView};

pub(crate) fn verified_launch_installation(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<ManifestView, LaunchError> {
    verified_launch_publisher(installed, manifest_path, payload_root, expected)
        .map(|(manifest, _)| manifest)
}

/// [`verified_launch_installation`], with the identity of the publisher whose key signed the package.
pub(crate) fn verified_launch_publisher(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<(ManifestView, [u8; 32]), LaunchError> {
    // Read on every launch, as `tilo-install run` does: a publisher the workspace
    // revoked after install must not keep launching from the shell.
    let revoked = workspace_revocations();
    verified_launch_publisher_against(installed, manifest_path, payload_root, expected, &revoked)
}

fn verified_launch_publisher_against(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<(ManifestView, [u8; 32]), LaunchError> {
    let refusal =
        || LaunchError::InvalidServiceLaunch("original signed package launch changed".into());
    if installed.package_id_hex != expected
        || installed.manifest_path != manifest_path
        || installed.payload_root != payload_root
    {
        return Err(refusal());
    }
    let report =
        package_store::integrity::verify(installed, payload_root).map_err(|_| refusal())?;
    if !report.is_intact() {
        return Err(refusal());
    }
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(installed),
        &installed.id,
        expected,
    )
    .map_err(|_| refusal())?;
    if let Some(entry) = revoked.find(&record.root.publisher_identity) {
        return Err(LaunchError::InvalidServiceLaunch(format!(
            "the package's publisher key is revoked ({})",
            entry.reason
        )));
    }
    let manifest: ManifestView =
        toml::from_str(std::str::from_utf8(&record.manifest_bytes).map_err(|_| refusal())?)
            .map_err(|_| refusal())?;
    if manifest.package_id != installed.id {
        return Err(refusal());
    }
    Ok((manifest, record.root.publisher_identity))
}

/// The publisher keys the workspace refuses, as its trust reading answers
/// them: none when no desktop session answers, which refuses nothing beyond
/// the package's own signature.
fn workspace_revocations() -> tilo_app_package::RevocationList {
    tilo_app_package::RevocationList {
        revoked: tilo_control::packages::trust()
            .1
            .into_iter()
            .map(|key| tilo_app_package::RevokedKey {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect(),
    }
}
