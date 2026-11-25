//! The key a tool image is signed with.
//!
//! A device signs the tools it builds with its own device key: the image is
//! a local artefact vouched for by the machine that made it. The key stays with
//! this computer's daemon, so the image is signed by asking it. CI signs the
//! catalog's images with the publisher key it signs everything else with.

use std::path::Path;

use tilo_app_package::{ExternalSigner, SigningIdentity};
use tilo_device_identity::IdentityPaths;

pub fn device_identity() -> anyhow::Result<SigningIdentity> {
    let paths = std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|v| !v.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under);
    let actions = tilo_device_identity::login::daemon::DeviceActions::here(paths)
        .map_err(anyhow::Error::msg)?;
    let key = actions.device_key()?;
    Ok(SigningIdentity::from_external(ExternalSigner::new(
        key,
        move |message| actions.sign(message).map_err(|refused| refused.message),
    )))
}

pub fn identity_file(path: &Path) -> anyhow::Result<SigningIdentity> {
    Ok(SigningIdentity::from_signing_key(
        registry_types::key_file::load(path)?,
    ))
}
