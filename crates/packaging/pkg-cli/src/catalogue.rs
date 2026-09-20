//! Compose one publisher-signed mutation per app from a root-verified catalogue.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use registry_types::{EntryAction, RegistryEntry, SignedRegistrySnapshot};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    file: PathBuf,
    url: String,
}

pub(super) fn prepare(
    snapshot: &Path,
    owner: &str,
    artifacts: &Path,
    identity: &Path,
) -> Result<()> {
    let snapshot: SignedRegistrySnapshot = serde_json::from_slice(&std::fs::read(snapshot)?)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    // Same verifier used by app-catalogue: root/delegation, expiry, authorized
    // publishers and every nested publisher signature are all checked.
    registry_types::verify_snapshot(&snapshot, owner, now)
        .context("authenticate catalogue baseline")?;
    let signer = super::load_signing_key(identity)?;
    let publisher = hex::encode(signer.verifying_key().to_bytes());
    let authorization = snapshot
        .publishers
        .iter()
        .find(|row| row.pubkey_hex.eq_ignore_ascii_case(&publisher))
        .context("publisher is absent from authenticated catalogue policy")?;
    let artifacts: Vec<Artifact> = serde_json::from_slice(&std::fs::read(artifacts)?)?;
    if artifacts.is_empty() {
        bail!("catalogue submission requires at least one artifact");
    }
    let baseline: BTreeMap<_, _> = snapshot
        .entries
        .iter()
        .map(|entry| (entry.package_id.clone(), entry))
        .collect();
    let mut updated: BTreeMap<String, RegistryEntry> = BTreeMap::new();
    for artifact in artifacts {
        if !artifact.url.starts_with("https://") {
            bail!("catalogue artifacts require public HTTPS URLs");
        }
        let bytes = std::fs::read(&artifact.file)
            .with_context(|| format!("read {}", artifact.file.display()))?;
        let package_id = app_registry::qpkg::load(&bytes)?.manifest.package.id;
        let previous = updated
            .get(&package_id)
            .or_else(|| baseline.get(&package_id).copied());
        if previous.is_none() && !authorization.can_add_new {
            bail!("publisher is not authorized to add {package_id}");
        }
        let entry = super::registry_entry_for(&bytes, &artifact.url, &signer, previous)?;
        updated.insert(package_id, entry);
    }
    let requests: Vec<_> = updated
        .into_iter()
        .filter_map(|(id, entry)| {
            let old = baseline.get(&id).copied();
            if old == Some(&entry) {
                return None;
            }
            let action = if old.is_some() {
                EntryAction::Update
            } else {
                EntryAction::Add
            };
            Some(registry_types::sign_change_request(action, entry, &signer))
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&requests)?);
    Ok(())
}
