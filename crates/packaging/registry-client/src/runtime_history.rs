//! Publisher-side reconciliation of never-published native release history.
//!
//! A delayed app can accumulate more pending releases than the SDK catalogue
//! retains. Keep published history intact; retire an unpublished orphan only
//! after the same target has four newer releases with offered exact runtimes.

use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use registry_types::{RegistryEntry, RegistryVersion};

/// Re-sign `entry` after reconciling obsolete, unpublished SDK dependencies.
/// The caller must first verify the catalogue against its existing owner pin.
///
/// # Errors
/// If signatures or publisher ownership differ, or dropping an orphan would
/// remove the current artifact or one of its target's required rollback slots.
pub fn reconcile(
    entry: &mut RegistryEntry,
    catalogue: &[RegistryEntry],
    current_url: &str,
    signer: &SigningKey,
) -> Result<Vec<RegistryVersion>> {
    if !registry_types::verify_entry_signature(entry)
        || entry.publisher_pubkey_hex != hex::encode(signer.verifying_key().to_bytes())
    {
        bail!("runtime reconciliation requires this publisher's valid signed entry");
    }
    if registry_types::is_runtime_package_id(&entry.package_id) {
        return Ok(Vec::new());
    }
    let published = catalogue
        .iter()
        .find(|old| old.package_id == entry.package_id);
    let offered = |version: &RegistryVersion| {
        catalogue.iter().any(|provider| {
            registry_types::is_runtime_package_id(&provider.package_id)
                && provider.versions.iter().any(|provided| {
                    provided.target.platform == version.target.platform
                        && provided.target.architecture == version.target.architecture
                        && provided.target.runtime == version.target.runtime
                })
        })
    };
    // A publisher folds architectures one at a time. The other architecture
    // of this same signed release may still be waiting for its SDK submission.
    let current_release = entry
        .versions
        .iter()
        .find(|version| version.tapp_url == current_url)
        .map(|version| version.version.clone());
    let protected = |version: &RegistryVersion| {
        version.target.native_runtime().is_none()
            || current_release
                .as_ref()
                .is_some_and(|current| *current == version.version)
            || published.is_some_and(|old| old.versions.contains(version))
            || offered(version)
    };
    let remaining: Vec<_> = entry
        .versions
        .iter()
        .filter(|v| protected(v))
        .cloned()
        .collect();
    let installable: Vec<_> = remaining.iter().filter(|v| offered(v)).cloned().collect();
    let dropped: Vec<_> = entry
        .versions
        .iter()
        .filter(|v| !protected(v))
        .cloned()
        .collect();
    for version in &dropped {
        if !registry_types::retention::may_drop(version, &installable) {
            bail!(
                "{}@{} ({}) needs exact runtime {}; fewer than four newer offered releases remain; restore the runtime instead",
                entry.package_id,
                version.version,
                version.target.architecture,
                version.target.runtime.as_deref().unwrap_or_default(),
            );
        }
    }
    if !dropped.is_empty() {
        entry.versions = remaining;
        registry_types::sign_entry(entry, signer);
    }
    Ok(dropped)
}

#[cfg(test)]
#[path = "runtime_history_tests.rs"]
mod tests;
