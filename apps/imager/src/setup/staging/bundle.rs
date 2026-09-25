//! The staging-relevant facts of a validated internal bundle manifest:
//! component sizes and digests, partition minimums and the loader path.
//!
//! Validation stays in `internal_bundle::validate_internal_bundle`; this only
//! reads the extra fields its view does not expose, from bytes that already
//! passed it.

use serde::Deserialize;

use super::plan::Requirements;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentFacts {
    pub byte_size: u64,
    pub sha256: String,
    pub minimum_partition_bytes: u64,
    /// Btrfs fsid (root) or FAT serial (ESP) the exporter bound.
    pub filesystem_uuid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFacts {
    pub release_id: String,
    pub architecture: String,
    pub esp: ComponentFacts,
    pub root: ComponentFacts,
    /// `boot_binding.efi_path`, forward-slash form (`/EFI/BOOT/BOOTX64.EFI`).
    pub efi_path: String,
}

impl BundleFacts {
    pub fn requirements(&self, target_bytes: u64) -> Requirements {
        Requirements {
            staging_esp_bytes: self.esp.minimum_partition_bytes,
            staging_root_bytes: self.root.minimum_partition_bytes,
            target_bytes,
        }
    }
}

#[derive(Deserialize)]
struct Loose {
    release_id: String,
    architecture: String,
    components: Vec<LooseComponent>,
    boot_binding: LooseBinding,
}

#[derive(Deserialize)]
struct LooseComponent {
    role: String,
    byte_size: u64,
    sha256: String,
    minimum_partition_bytes: u64,
    filesystem_uuid: String,
}

#[derive(Deserialize)]
struct LooseBinding {
    efi_path: String,
}

/// Validate `manifest` with the shared validator, then read its staging facts.
pub fn facts(manifest: &[u8]) -> Result<BundleFacts, String> {
    let bundle = crate::setup::internal_bundle::validate_internal_bundle(manifest)?;
    facts_unchecked(manifest).and_then(|facts| {
        if facts.release_id != bundle.release_id() || facts.architecture != bundle.architecture() {
            return Err("manifest views disagree".into());
        }
        Ok(facts)
    })
}

/// Read staging facts without validation (tests and already-validated bytes).
pub(crate) fn facts_unchecked(manifest: &[u8]) -> Result<BundleFacts, String> {
    let loose: Loose = serde_json::from_slice(manifest).map_err(|e| e.to_string())?;
    let find = |role: &str| {
        let mut matches = loose.components.iter().filter(|c| c.role == role);
        match (matches.next(), matches.next()) {
            (Some(c), None) => Ok(ComponentFacts {
                byte_size: c.byte_size,
                sha256: c.sha256.clone(),
                minimum_partition_bytes: c.minimum_partition_bytes,
                filesystem_uuid: c.filesystem_uuid.clone(),
            }),
            _ => Err(format!("manifest needs exactly one {role} component")),
        }
    };
    Ok(BundleFacts {
        release_id: loose.release_id.clone(),
        architecture: loose.architecture.clone(),
        esp: find("staging-esp")?,
        root: find("staging-root")?,
        efi_path: loose.boot_binding.efi_path.clone(),
    })
}
