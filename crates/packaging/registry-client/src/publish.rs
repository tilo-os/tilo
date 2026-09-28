//! Publishing one build: upload the package, then ask for the entry that
//! lists it. Both wait for the registry owner's review.

use anyhow::{Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use registry_types::EntryAction;

use crate::client::RegistryClient;
use crate::entry::{EntryMetadata, Platform, build_entry, check_package};

/// What [`publish`] asked the registry for.
#[derive(Clone, Debug)]
pub struct Published {
    pub package_id: String,
    pub version: String,
    pub platform: &'static str,
    pub url: String,
    /// `None` when the published entry already lists this build unchanged.
    pub action: Option<EntryAction>,
}

/// Upload `tapp_bytes` and ask for the entry that lists them: `add` for a
/// package the catalogue does not have yet, `update` otherwise.
///
/// # Errors
///
/// When the package would be refused, the published entry belongs to
/// another publisher, or the registry refuses a request.
pub fn publish(
    client: &RegistryClient,
    key: &SigningKey,
    tapp_bytes: &[u8],
    arch: Option<&str>,
    metadata: Option<&EntryMetadata>,
) -> Result<Published> {
    let checked = check_package(tapp_bytes, key.verifying_key().to_bytes())?;
    let platform = Platform::of(&checked.manifest).ok_or_else(|| {
        anyhow!("the package's [target] names a platform this tool does not publish")
    })?;
    let package_id = checked.manifest.package.id.clone();
    let previous = client.entry(&package_id)?;
    if let Some(previous) = &previous
        && !previous
            .publisher_pubkey_hex
            .eq_ignore_ascii_case(&crate::key::public_hex(key))
    {
        bail!("{package_id} is published by another key");
    }
    let url = client.upload(key, tapp_bytes)?;
    let entry = build_entry(
        tapp_bytes,
        &url,
        key,
        platform,
        arch,
        previous.as_ref(),
        metadata,
    )?;
    let action = if previous.as_ref() == Some(&entry) {
        None
    } else if previous.is_some() {
        Some(EntryAction::Update)
    } else {
        Some(EntryAction::Add)
    };
    if let Some(action) = action {
        client.submit(action, entry, key)?;
    }
    Ok(Published {
        package_id,
        version: checked.manifest.package.version,
        platform: platform.wire(),
        url,
        action,
    })
}
