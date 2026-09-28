//! Publishing builds: upload each package, then ask for one entry per package
//! id that lists them all. Both wait for the registry owner's review.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use registry_types::EntryAction;

use crate::client::RegistryClient;
use crate::entry::{EntryMetadata, Platform, build_entry, check_package};

/// One package [`publish`] uploaded.
#[derive(Clone, Debug)]
pub struct Published {
    pub package_id: String,
    pub version: String,
    pub platform: &'static str,
    pub url: String,
    /// What the registry was asked to do with the entry listing this build;
    /// `None` when the published entry already lists it unchanged.
    pub action: Option<EntryAction>,
}

/// Upload every package in `packages` and ask for the entries that list
/// them, answering one [`Published`] per package in the same order: `add` for
/// a package id the catalogue does not have yet, `update` otherwise. Builds of
/// one package id — its platforms and architectures — go into a single
/// request that extends the one still waiting for review, if any: a newer
/// request for a package replaces the waiting one and must keep its versions.
///
/// # Errors
///
/// When a package would be refused, a published entry belongs to another
/// publisher, or the registry refuses a request. Every package is checked
/// before anything is uploaded.
pub fn publish(
    client: &RegistryClient,
    key: &SigningKey,
    packages: &[Vec<u8>],
    arch: Option<&str>,
    metadata: Option<&EntryMetadata>,
) -> Result<Vec<Published>> {
    let mut by_id: BTreeMap<String, Vec<(usize, Platform, String)>> = BTreeMap::new();
    for (index, bytes) in packages.iter().enumerate() {
        let checked = check_package(bytes, key.verifying_key().to_bytes())?;
        let platform = Platform::of(&checked.manifest).ok_or_else(|| {
            anyhow!("the package's [target] names a platform this tool does not publish")
        })?;
        by_id.entry(checked.manifest.package.id).or_default().push((
            index,
            platform,
            checked.manifest.package.version,
        ));
    }
    let mut published: Vec<Option<Published>> = vec![None; packages.len()];
    for (package_id, builds) in by_id {
        let published_entry = client.entry(&package_id)?;
        if let Some(previous) = &published_entry
            && !previous
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&crate::key::public_hex(key))
        {
            bail!("{package_id} is published by another key");
        }
        let previous = match client.pending(key, &package_id)? {
            Some(pending) => Some(pending),
            None => published_entry.clone(),
        };
        let mut entry = previous.clone();
        let indices: Vec<usize> = builds.iter().map(|(index, ..)| *index).collect();
        for (index, platform, version) in builds {
            let bytes = packages.get(index).map_or(&[][..], Vec::as_slice);
            let url = client.upload(key, bytes)?;
            entry = Some(build_entry(
                bytes,
                &url,
                key,
                platform,
                arch,
                entry.as_ref(),
                metadata,
            )?);
            if let Some(slot) = published.get_mut(index) {
                *slot = Some(Published {
                    package_id: package_id.clone(),
                    version,
                    platform: platform.wire(),
                    url,
                    action: None,
                });
            }
        }
        let Some(entry) = entry.filter(|entry| previous.as_ref() != Some(entry)) else {
            continue;
        };
        let action = if published_entry.is_some() {
            EntryAction::Update
        } else {
            EntryAction::Add
        };
        client.submit(action, entry, key)?;
        for index in indices {
            if let Some(Some(build)) = published.get_mut(index) {
                build.action = Some(action);
            }
        }
    }
    Ok(published.into_iter().flatten().collect())
}
