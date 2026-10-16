//! `.tapp` install through the device's package store.
//!
//! The package streams straight into the [`PackageStore`]: its signed
//! metadata is checked here before any payload byte is stored, the payload
//! goes into the content store, and the tree is materialised once from
//! there. A thin package installs when its payload is already stored or a
//! [`BlobFetcher`] can supply the rest.
//!
//! [`InstallExpectation`] is the client's vocabulary too; installing is the
//! daemon's and needs the `service` feature.

#[cfg(feature = "service")]
use package_store::{Admission, BlobFetcher, InstalledPackage, PackageStore, Pinning, StoreError};
#[cfg(feature = "service")]
use thiserror::Error;
use tilo_app_package::PackageId;
#[cfg(feature = "service")]
use tilo_app_package::{PackageMeta, Verification};

#[cfg(feature = "service")]
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("verify: {0}")]
    Verify(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Registry-authenticated identity that downloaded bytes must match.
///
/// This is deliberately supplied by the privileged Store capability, not
/// inferred from the archive: an archive is allowed to describe itself, but
/// it is not allowed to choose the identity the user approved in the
/// catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallExpectation {
    pub package_root: PackageId,
    pub publisher: [u8; 32],
    pub app_id: String,
    pub version: String,
    pub size_bytes: u64,
}

#[cfg(feature = "service")]
fn admission(pinning: Pinning, source: &str) -> Admission {
    Admission {
        pinned: pinning == Pinning::Pinned,
        attested_by: String::new(),
        source: source.to_string(),
        held: false,
    }
}

#[cfg(feature = "service")]
/// Accept a package whose signature is valid, whoever made it. The trust
/// decision belongs to the caller: the Store capability's expectation, or
/// the user who handed over a local file.
fn signed(meta: &PackageMeta) -> Result<(), String> {
    match tilo_app_package::verify(meta, &[]) {
        Verification::Trusted { .. }
        | Verification::AttestedBy { .. }
        | Verification::UntrustedPublisher { .. } => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

#[cfg(feature = "service")]
/// Install a `.tapp` whose signature is valid.
///
/// # Errors
///
/// Returns [`InstallError::Store`] when the package is malformed, its
/// signature is invalid, payload is unavailable, or the store fails.
pub fn install_tapp(
    store: &PackageStore,
    bytes: &[u8],
    pinning: Pinning,
    fetcher: Option<&dyn BlobFetcher>,
) -> Result<InstalledPackage, InstallError> {
    let installed = store.install(
        bytes,
        |meta| {
            signed(meta)?;
            Ok(admission(pinning, "app host"))
        },
        fetcher,
    )?;
    Ok(installed.package)
}

#[cfg(feature = "service")]
/// Install a registry download after checking it against every signed
/// catalogue identity: size, package root, publisher, app id and version.
/// Nothing is stored for a package that does not match.
///
/// # Errors
///
/// Returns [`InstallError::Verify`] when the bytes do not match the expected
/// catalogue identity, or [`InstallError::Store`] when the store refuses or
/// fails.
pub fn install_verified_tapp(
    store: &PackageStore,
    bytes: &[u8],
    pinning: Pinning,
    expected: &InstallExpectation,
    fetcher: Option<&dyn BlobFetcher>,
) -> Result<InstalledPackage, InstallError> {
    let actual_size = u64::try_from(bytes.len())
        .map_err(|_| InstallError::Verify("package length does not fit u64".into()))?;
    if actual_size != expected.size_bytes {
        return Err(InstallError::Verify(format!(
            "package size mismatch: catalogue says {}, download has {actual_size}",
            expected.size_bytes
        )));
    }
    let installed = store.install(
        bytes,
        |meta| {
            if meta.id != expected.package_root {
                return Err(format!(
                    "package id mismatch: download is {}, catalogue promised {}",
                    meta.id, expected.package_root
                ));
            }
            match tilo_app_package::verify(meta, &[expected.publisher]) {
                Verification::Trusted { .. } | Verification::AttestedBy { .. } => {}
                Verification::UntrustedPublisher { identity, .. } => {
                    return Err(format!(
                        "publisher ed25519:{} does not match the catalogue's publisher",
                        hex::encode(identity)
                    ));
                }
                other => return Err(format!("{other:?}")),
            }
            let package = &meta.manifest.package;
            if (package.id.as_str(), package.version.as_str())
                != (expected.app_id.as_str(), expected.version.as_str())
            {
                return Err(format!(
                    "manifest identity mismatch: expected {}@{}, got {}@{}",
                    expected.app_id, expected.version, package.id, package.version
                ));
            }
            Ok(admission(pinning, "app store"))
        },
        fetcher,
    )?;
    Ok(installed.package)
}
