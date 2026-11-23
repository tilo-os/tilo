//! Reviewing a `.tapp` someone hands over before anything installs it: reading its bytes
//! through the one decoder, and checking them against an identity the reviewer already trusts.
//! An archive can describe itself; it cannot authorize its own publisher or identity. Either an
//! independently verified catalogue offer names it ([`CatalogueArchive::accepts_offer`]), or an
//! attestor the workspace already trusts signed it ([`review_attested`]). Nothing here installs
//! or writes anything.

use crate::trust::{RevocationList, TrustedAttestors};
use crate::{BlobHash, DiscardBlobs, Kind, Manifest, PackageId, PackageMeta, Verification};

/// The largest archive a person can hand over for review.
pub const MAX_REVIEWED_BYTES: usize = 64 * 1024 * 1024;

/// The signed identity an install binds a package's bytes to, as an independently verified
/// catalogue offer (or a review) names it: an archive may describe itself, but it may not choose
/// the identity the person approved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallExpectation {
    pub package_root: PackageId,
    pub publisher: [u8; 32],
    pub app_id: String,
    pub version: String,
    pub size_bytes: u64,
}

/// Structurally checked application metadata, without a trust grant or install.
#[derive(Clone, Debug)]
pub struct CatalogueArchive {
    meta: PackageMeta,
}

/// Why handed bytes cannot be reviewed as an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveReviewError {
    #[error("The application archive is invalid")]
    Invalid,
    #[error("Only application archives can be reviewed here")]
    NotApplication,
}

/// Read handed archive bytes through the canonical package decoder.
///
/// # Errors
/// A malformed, oversized or non-application archive. A structurally checked archive still needs
/// an independently authenticated catalogue offer.
pub fn review_archive(bytes: &[u8]) -> Result<CatalogueArchive, ArchiveReviewError> {
    if bytes.is_empty() || bytes.len() > MAX_REVIEWED_BYTES {
        return Err(ArchiveReviewError::Invalid);
    }
    let tapp =
        crate::tapp::read(bytes, &mut DiscardBlobs).map_err(|_| ArchiveReviewError::Invalid)?;
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

    /// Whether `expected` names exactly this archive, signed by its publisher. Full and thin
    /// forms share a root; the offer's size describes its canonical download, not the handed
    /// representation.
    #[must_use]
    pub fn accepts_offer(&self, expected: &InstallExpectation) -> bool {
        let package = &self.meta.manifest.package;
        expected.size_bytes > 0
            && expected.size_bytes <= crate::MAX_APP_TAPP_BYTES
            && expected.package_root == self.meta.id
            && expected.app_id == package.id
            && expected.version == package.version
            && matches!(
                crate::verify(&self.meta, &[expected.publisher]),
                Verification::Trusted { .. }
            )
    }
}

/// What a review against the workspace's trust found of an archive no catalogue lists. It
/// grants nothing by itself: an install checks the package again.
#[derive(Clone, Debug)]
pub struct AttestedReview {
    /// The signed package root, hex.
    pub root: String,
    /// The BLAKE3 of the handed bytes, hex: the same file is installed as was reviewed.
    pub archive_hash: String,
    /// The publisher key, hex.
    pub publisher: String,
    /// The trusted attestor that signed it.
    pub attestor: String,
    pub manifest: Manifest,
}

impl AttestedReview {
    /// The identity an install binds these reviewed bytes to.
    ///
    /// # Errors
    /// The review names a malformed root or publisher.
    pub fn expectation(&self, size_bytes: u64) -> Result<InstallExpectation, String> {
        let package_root = format!("tapp:{}", self.root)
            .parse()
            .map_err(|_| "The reviewed application root is invalid".to_owned())?;
        let publisher = hex::decode(&self.publisher)
            .ok()
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .ok_or_else(|| "The reviewed publisher is invalid".to_owned())?;
        Ok(InstallExpectation {
            package_root,
            publisher,
            app_id: self.manifest.package.id.clone(),
            version: self.manifest.package.version.clone(),
            size_bytes,
        })
    }
}

/// Review an archive no catalogue lists against the workspace's trust: it must be a complete,
/// native application within its signed size limit, attested by one of `attestors`, and its
/// publisher not revoked.
///
/// # Errors
/// What disqualifies it, in a sentence a person reads.
pub fn review_attested(
    bytes: &[u8],
    attestors: &TrustedAttestors,
    revoked: &RevocationList,
) -> Result<AttestedReview, String> {
    if bytes.is_empty() || bytes.len() > MAX_REVIEWED_BYTES {
        return Err("The local package is empty or exceeds the file size limit".into());
    }
    let tapp = crate::tapp::read(bytes, &mut DiscardBlobs).map_err(|e| e.to_string())?;
    if !tapp.carried.is_empty() && !tapp.is_complete() {
        return Err("The local package carries an incomplete payload".into());
    }
    if tapp.meta.manifest.package.kind != Kind::App {
        return Err("Only application packages can be reviewed here".into());
    }
    crate::desktop::validate_native_host(&tapp.meta.manifest).map_err(|e| e.to_string())?;
    if crate::app_size_limit(&tapp.meta.manifest).is_some_and(|limit| bytes.len() as u64 > limit) {
        return Err("The local package exceeds its signed target size limit".into());
    }
    let Verification::AttestedBy { attestor, manifest } =
        crate::verify_with_trust(&tapp.meta, &[], attestors.as_slice(), revoked)
    else {
        return Err(
            "The local package needs a valid existing trusted attestation and must not be revoked"
                .into(),
        );
    };
    Ok(AttestedReview {
        root: tapp.meta.id.to_hex(),
        archive_hash: BlobHash::of(bytes).to_hex(),
        publisher: hex::encode(tapp.meta.publisher()),
        attestor,
        manifest,
    })
}

#[cfg(test)]
mod tests;
