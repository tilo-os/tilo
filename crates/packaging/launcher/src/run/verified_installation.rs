//! The installation a launch runs, verified on every launch: its files, its signed record and
//! its publisher, not revoked.

use std::path::Path;

use super::{ManifestView, RunError};

pub(crate) fn verified_launch_installation(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<ManifestView, RunError> {
    verified_launch_publisher(installed, manifest_path, payload_root, expected)
        .map(|(manifest, _)| manifest)
}

/// [`verified_launch_installation`], with the identity of the publisher whose key signed the
/// package.
pub(crate) fn verified_launch_publisher(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<(ManifestView, [u8; 32]), RunError> {
    // Re-read on every launch, as `tilo-install run` does: a publisher
    // revoked after install must not keep launching from the shell. An
    // unreadable list refuses rather than reading as "nothing revoked".
    use tilo_app_package::TrustFile as _;
    let revoked = match tilo_app_package::default_revoked_keys_path() {
        Some(path) => tilo_app_package::RevocationList::load(&path).map_err(|error| {
            RunError::InvalidServiceLaunch(format!(
                "the publisher revocation list is unreadable ({error})"
            ))
        })?,
        None => tilo_app_package::RevocationList::default(),
    };
    verified_launch_publisher_against(installed, manifest_path, payload_root, expected, &revoked)
}

#[cfg(test)]
pub(crate) fn verified_launch_installation_against(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<ManifestView, RunError> {
    verified_launch_publisher_against(installed, manifest_path, payload_root, expected, revoked)
        .map(|(manifest, _)| manifest)
}

fn verified_launch_publisher_against(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
    revoked: &tilo_app_package::RevocationList,
) -> Result<(ManifestView, [u8; 32]), RunError> {
    let refusal =
        || RunError::InvalidServiceLaunch("original signed package launch changed".into());
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
        return Err(RunError::InvalidServiceLaunch(format!(
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
