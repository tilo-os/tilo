//! Trusted local files through the existing Store capability and installer.
use std::io::Write;

use serde::{Deserialize, Serialize};
use tilo_app_package::trust::{RevocationList, TrustedAttestors};
use tilo_app_package::{BlobHash, DiscardBlobs, Kind, Manifest, Verification};

pub const MAX_LOCAL_BYTES: usize = 64 * 1024 * 1024;

/// Host-verified identity of immutable handed bytes; contains no trust grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalReview {
    pub root: String,
    pub archive_hash: String,
    pub publisher: String,
    pub attestor: String,
    pub manifest: Manifest,
    pub installed_version: Option<String>,
    pub pinned: bool,
    pub user_managed: bool,
}

/// Verify a local archive against existing host trust, without installing it.
///
/// # Errors
/// Refuses malformed, untrusted, revoked, non-app or oversized archives.
pub fn review(bytes: &[u8]) -> Result<LocalReview, String> {
    let attestors_path = tilo_app_package::trust::default_trusted_attestors_path()
        .ok_or("Local package trust configuration is unavailable")?;
    let revoked_path = tilo_app_package::trust::default_revoked_keys_path()
        .ok_or("Local package revocation configuration is unavailable")?;
    let attestors = TrustedAttestors::load(&attestors_path).map_err(|e| e.to_string())?;
    let revoked = RevocationList::load(&revoked_path).map_err(|e| e.to_string())?;
    let mut checked = review_with_trust(bytes, &attestors, &revoked)?;
    if let Some(installed) = package_store::Catalog::from_env()
        .map_err(|e| e.to_string())?
        .find(&checked.manifest.package.id)
        .map_err(|e| e.to_string())?
    {
        checked.installed_version = Some(installed.version.clone());
        checked.pinned = installed.pinned;
        checked.user_managed = installed.user_managed();
    }
    Ok(checked)
}

fn review_with_trust(
    bytes: &[u8],
    attestors: &TrustedAttestors,
    revoked: &RevocationList,
) -> Result<LocalReview, String> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_BYTES {
        return Err("The local package is empty or exceeds the file size limit".into());
    }
    let tapp = tilo_app_package::tapp::read(bytes, &mut DiscardBlobs).map_err(|e| e.to_string())?;
    if !tapp.carried.is_empty() && !tapp.is_complete() {
        return Err("The local package carries an incomplete payload".into());
    }
    if tapp.meta.manifest.package.kind != Kind::App {
        return Err("Only application packages can be reviewed here".into());
    }
    tilo_app_package::desktop::validate_native_host(&tapp.meta.manifest)
        .map_err(|e| e.to_string())?;
    if tilo_app_package::app_size_limit(&tapp.meta.manifest)
        .is_some_and(|limit| bytes.len() as u64 > limit)
    {
        return Err("The local package exceeds its signed target size limit".into());
    }
    let Verification::AttestedBy { attestor, manifest } =
        tilo_app_package::verify_with_trust(&tapp.meta, &[], attestors.as_slice(), revoked)
    else {
        return Err(
            "The local package needs a valid existing trusted attestation and must not be revoked"
                .into(),
        );
    };
    Ok(LocalReview {
        root: tapp.meta.id.to_hex(),
        archive_hash: BlobHash::of(bytes).to_hex(),
        publisher: hex::encode(tapp.meta.publisher()),
        attestor,
        manifest,
        installed_version: None,
        pinned: false,
        user_managed: false,
    })
}

/// Revalidate review and current host trust, then use the canonical installer.
///
/// # Errors
/// Refuses changed bytes/root, changed trust, missing payload/runtime and normal
/// installer policy errors. Existing local-update publisher and pin policy apply.
pub fn install(bytes: &[u8], root: &str, archive_hash: &str) -> Result<String, String> {
    let checked = review(bytes)?;
    if checked.root != root || checked.archive_hash != archive_hash {
        return Err("The package changed since review; review the file again".into());
    }
    let mut staged = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    staged.write_all(bytes).map_err(|e| e.to_string())?;
    staged.flush().map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(app_cli::install::tilo_install_bin());
    if app_cli::installed_version(&checked.manifest.package.id)?.is_some() {
        command.args(["update", &checked.manifest.package.id, "--from"]);
        command.arg(staged.path());
    } else {
        command.arg("install").arg(staged.path());
    }
    command.args([
        "--require-package-root",
        &checked.root,
        "--require-attestor",
        &checked.attestor,
    ]);
    let output = bounded_command::run_prepared(
        command,
        None,
        1024 * 1024,
        std::time::Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let diagnostic = crate::service::safe_host_diagnostic(&output.stderr);
        return Err(format!(
            "The trusted local package could not be installed: {diagnostic}"
        ));
    }
    let selected = package_store::Catalog::from_env()
        .map_err(|e| e.to_string())?
        .find(&checked.manifest.package.id)
        .map_err(|e| e.to_string())?
        .ok_or("The installed package is unavailable")?;
    if selected.package_id().map(|id| id.to_hex()).as_deref() != Some(checked.root.as_str()) {
        return Err("Installation retained a different selected version; the reviewed root was not activated".into());
    }
    Ok(checked.root)
}

#[cfg(test)]
mod tests;
