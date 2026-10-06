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
///
/// # Errors
///
/// The device key is missing, unreadable, or too widely readable to trust.
pub fn device_identity() -> anyhow::Result<SigningIdentity> {
    let paths = std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|v| !v.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under);
    let key = tilo_user_identity::persist::load_device_key(&paths)?;
    Ok(SigningIdentity::from_signing_key(key))
}

/// A publisher identity file, `{"secret_key_hex": "…"}` — the format
/// `tilo-app-package keygen` writes and CI keeps in a secret.
///
/// # Errors
///
/// Reading or parsing the file, or a key that isn't 32 bytes of hex.
pub fn identity_file(path: &Path) -> anyhow::Result<SigningIdentity> {
    #[derive(serde::Deserialize)]
    struct File {
        secret_key_hex: String,
    }
    let file: File = serde_json::from_slice(&std::fs::read(path)?)?;
    let seed: [u8; 32] = hex::decode(file.secret_key_hex.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("{}: the secret key is not 32 bytes", path.display()))?;
    Ok(SigningIdentity::from_seed(&seed))
}
