//! The key a tool image is signed with.
//!
//! A device signs the tools it builds with its own device key: the image is
//! a local artefact vouched for by the machine that made it. CI signs the
//! catalog's images with the publisher key it signs everything else with.

use std::path::Path;

use tilo_app_package::SigningIdentity;
use tilo_user_identity::IdentityPaths;

/// This device's signing identity, from the system identity root
/// (`TILO_IDENTITY_ROOT` overrides it, as for every other identity reader).
pub fn device_identity() -> anyhow::Result<SigningIdentity> {
    let paths = std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|v| !v.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under);
    let key = tilo_user_identity::persist::load_device_key(&paths)?;
    Ok(SigningIdentity::from_signing_key(key))
}

/// A publisher key file ([`registry_types::key_file`]), as
/// `tilo-runtime-bundle key generate` writes it and CI keeps it in a secret.
///
/// # Errors
///
/// Reading the file, or a file that is no key file.
pub fn identity_file(path: &Path) -> anyhow::Result<SigningIdentity> {
    Ok(SigningIdentity::from_signing_key(
        registry_types::key_file::load(path)?,
    ))
}
