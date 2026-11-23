//! Trusted local files through the existing Store capability and installer. The Store reads an
//! archive ([`review_archive`]) and holds a service review; reviewing against process trust and
//! installing ([`review`], [`install()`]) run in the daemon and need the `service` feature.
#[cfg(feature = "service")]
use std::io::Write;

#[cfg(feature = "service")]
use tilo_app_package::BlobHash;
#[cfg(feature = "service")]
use tilo_app_package::trust::{RevocationList, TrustFile, TrustedAttestors};
use tilo_app_package::{DiscardBlobs, Kind, Verification};

pub const MAX_LOCAL_BYTES: usize = 64 * 1024 * 1024;

pub use package_service_client::LocalReview;

/// Structurally checked application metadata, without a trust grant or install.
#[derive(Clone, Debug)]
pub struct CatalogueArchive {
    meta: tilo_app_package::PackageMeta,
}

/// Why immutable handed bytes cannot be reviewed as an application.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum ArchiveReviewError {
    #[error("The application archive is invalid")]
    Invalid,
    #[error("Only application archives can be reviewed here")]
    NotApplication,
}

/// Read immutable archive bytes through the canonical package decoder.
///
/// # Errors
/// A malformed, oversized or non-application archive. A structurally checked archive still requires
/// an independently authenticated catalogue offer.
pub fn review_archive(bytes: &[u8]) -> Result<CatalogueArchive, ArchiveReviewError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_BYTES {
        return Err(ArchiveReviewError::Invalid);
    }
    let tapp = tilo_app_package::tapp::read(bytes, &mut DiscardBlobs)
        .map_err(|_| ArchiveReviewError::Invalid)?;
    if tapp.meta.manifest.package.kind != Kind::App {
        return Err(ArchiveReviewError::NotApplication);
    }
    Ok(CatalogueArchive { meta: tapp.meta })
}

impl CatalogueArchive {
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.meta.manifest.package.id
    }

    /// Verify every supplied identity and the publisher signature. Full and thin forms share a
    /// root; the offer's size describes its canonical download, not the handed representation.
    #[must_use]
    pub fn accepts_offer(&self, expected: &crate::install::InstallExpectation) -> bool {
        let package = &self.meta.manifest.package;
        expected.size_bytes > 0
            && expected.size_bytes <= tilo_app_package::MAX_APP_TAPP_BYTES
            && expected.package_root == self.meta.id
            && expected.app_id == package.id
            && expected.version == package.version
            && matches!(
                tilo_app_package::verify(&self.meta, &[expected.publisher]),
                Verification::Trusted { .. }
            )
    }
}

/// Retain the service-reviewed identity for the ordinary install operation.
///
/// # Errors
/// A malformed process review identity. It grants no publisher trust.
pub fn local_expectation(
    review: &LocalReview,
    size_bytes: u64,
) -> Result<crate::install::InstallExpectation, String> {
    let package_root = format!("tapp:{}", review.root)
        .parse()
        .map_err(|_| "The reviewed application root is invalid")?;
    let publisher = hex::decode(&review.publisher)
        .map_err(|_| "The reviewed publisher is invalid")?
        .try_into()
        .map_err(|_| "The reviewed publisher is invalid")?;
    Ok(crate::install::InstallExpectation {
        package_root,
        publisher,
        app_id: review.manifest.package.id.clone(),
        version: review.manifest.package.version.clone(),
        size_bytes,
    })
}

#[cfg(feature = "service")]
/// Verify a local archive against existing process trust, without installing it.
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

#[cfg(feature = "service")]
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

#[cfg(feature = "service")]
/// Revalidate review and current process trust, then install through the one installer, which
/// verifies the package again: an install, or an update of the installed version.
pub fn install(
    store: &package_store::PackageStore,
    catalog: &package_store::Catalog,
    bytes: &[u8],
    root: &str,
    archive_hash: &str,
) -> Result<package_store::installer::Selected, String> {
    use package_store::installer;
    let checked = review(bytes)?;
    if checked.root != root || checked.archive_hash != archive_hash {
        return Err("The package changed since review; review the file again".into());
    }
    let mut staged = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    staged.write_all(bytes).map_err(|e| e.to_string())?;
    staged.flush().map_err(|e| e.to_string())?;
    let trust = installer::Trust {
        require_package_root: Some(checked.root.clone()),
        require_attestor: Some(checked.attestor.clone()),
        ..installer::Trust::default()
    };
    let report = installer::install_or_update(
        store,
        catalog,
        &installer::Candidate::staged(staged.path(), "local file"),
        &trust,
        None,
    )
    .map_err(|error| format!("The trusted local package could not be installed: {error}"))?;
    match report.selected {
        Some(selected) if selected.root == checked.root => Ok(selected),
        _ => Err("Installation retained a different selected version; the reviewed root was not activated".into()),
    }
}

#[cfg(test)]
#[cfg(feature = "service")]
mod tests;
