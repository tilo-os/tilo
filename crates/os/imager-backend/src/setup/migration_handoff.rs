//! Metadata-only Linux migration intent across installation and reboot.
//!
//! A plan records consent and stable source identities; it grants neither a mount nor a copy. After
//! reboot a fresh read-only grant must match its GPT partition, filesystem and home inode, and no
//! `/dev` or mount path is serialized as authority.
use super::migration_store::NativePath;
#[cfg(target_os = "linux")]
use super::{migration::PinnedDirectory, staging};
use serde::{Deserialize, Serialize};
use std::io;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::validate_offline_source_disks;
#[cfg(target_os = "linux")]
mod mounted_entry;
#[cfg(target_os = "linux")]
mod source_geometry;
#[cfg(target_os = "linux")]
pub use source_geometry::{SourceGeometry, append_mounted_geometry};
#[cfg(target_os = "linux")]
mod original_inventory;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
mod store;
#[cfg(target_os = "linux")]
pub use crate::migration_entry::EntryKind;
pub use crate::migration_entry::relative;
#[cfg(target_os = "linux")]
pub use linux::{
    AccessMode, InstalledScratchFacts, InstalledScratchIdentity, ResolvedSource, RetainedSource,
    VolumeNamespace, capture, capture_mounted_entry, mount_authenticated_read_only,
    mount_read_only, mounted_record_namespace, original_record_namespace, resolve_flatpak_source,
    retain_access, retain_scoped_source,
};
#[cfg(target_os = "linux")]
pub use mounted_entry::EntryBinding;
#[cfg(target_os = "linux")]
pub use original_inventory::{INVENTORY_BOUND, OriginalInventoryBinding};
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
mod fixture;
#[cfg(test)]
mod pure_tests;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub(in crate::setup) fn fixture_plan() -> Plan {
    fixture::sample()
}
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub use linux::capture_fixture;
#[cfg(test)]
#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
pub use linux::resolve;

pub use staging_handoff_model::migration::{
    Categories, ComputerApplicationsScope, DEFERRED_ORIGINAL_UID, DeferredHandoff, Filesystem,
    Home, HomePolicy, MAX_BYTES, Migration, SCHEMA, SourceVolume,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Plan(pub(crate) Migration);

impl std::ops::Deref for Plan {
    type Target = Migration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Plan {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TryFrom<Migration> for Plan {
    type Error = io::Error;

    fn try_from(migration: Migration) -> io::Result<Self> {
        let plan = Self(migration);
        plan.validate()?;
        Ok(plan)
    }
}

impl From<Plan> for Migration {
    fn from(plan: Plan) -> Self {
        plan.0
    }
}

#[cfg(test)]
pub(super) fn set_home_relative(home: &mut Home, relative: NativePath) {
    *home = Home::new(
        home.volume.clone(),
        relative,
        home.inode(),
        home.original_uid(),
        home.subvolume().cloned(),
    );
}

fn refusal(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Plan {
    /// Decodes the reviewed Home exclusions as normalized relative paths. Refuses unrepresentable
    /// bytes, non-relative components and exclusions without selected Home data.
    pub fn home_exclusion_paths(&self) -> io::Result<Vec<std::path::PathBuf>> {
        self.0.validate_home_exclusions().map_err(refusal)?;
        crate::setup::source_tree::exclusion_paths(&self.home_exclusions)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.home_policy != HomePolicy::Copy && !self.categories.home {
            return Err(refusal(
                "Retained original-files policy requires selected home data",
            ));
        }
        if self.schema != SCHEMA
            || !operation_id(&self.operation_id)
            || !(self.categories.home || self.categories.flatpak || self.categories.accounts)
        {
            return Err(refusal("Invalid Linux migration plan or empty selection."));
        }
        self.home.validate_identity().map_err(refusal)?;
        self.home_exclusion_paths()?;
        if let Some(scope) = &self.pending_computer_applications_scope {
            scope.validate_for_migration(&self.0).map_err(refusal)?;
        }
        if let Some(handoff) = &self.deferred_handoff
            && (handoff.schema != SCHEMA
                || handoff.original_uid != self.home.original_uid()
                || handoff.original_uid != DEFERRED_ORIGINAL_UID
                || handoff.journal_sha256 == [0; 32])
        {
            return Err(refusal(
                "Deferred selected-source handoff differs from its retained Home.",
            ));
        }
        #[cfg(not(target_os = "linux"))]
        if self.flatpak_inventory.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Native application inventory validation requires Linux",
            ));
        }
        #[cfg(target_os = "linux")]
        if let Some(raw) = &self.flatpak_inventory {
            if !self.categories.flatpak {
                return Err(refusal("Flatpak inventory lacks migration consent"));
            }
            if OriginalInventoryBinding::parse(self)?.is_none() {
                let bytes = serde_json::to_vec(raw)?;
                super::flatpak_transfer::HandoffInventory::parse(&bytes)
                    .map_err(refusal)?
                    .validate_operation(&self.operation_id)
                    .map_err(refusal)?;
            }
        }
        let NativePath::Unix(bytes) = self.home.relative() else {
            return Err(refusal("Migration home locator is not a Unix native path."));
        };
        if bytes.contains(&0)
            || bytes
                .split(|byte| *byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b"..")
        {
            return Err(refusal(
                "Migration home locator is not a normalized native path.",
            ));
        }
        let path = self.home.relative().decode().map_err(refusal)?;
        relative(&path)
    }

    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(refusal("Migration plan exceeds its size bound."));
        }
        let plan: Self = serde_json::from_slice(bytes)?;
        plan.validate()?;
        Ok(plan)
    }
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_BYTES {
            return Err(refusal("Migration plan exceeds its size bound."));
        }
        Ok(bytes)
    }
    pub fn digest(&self) -> io::Result<String> {
        use sha2::{Digest, Sha256};
        Ok(hex::encode(Sha256::digest(self.bytes()?)))
    }

    /// The private journal stores this value while the plan stores the journal digest, so the two
    /// bind each other without a circular digest.
    pub fn base_digest(&self) -> io::Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut base = self.clone();
        base.deferred_handoff = None;
        base.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(&base)?).into())
    }

    pub fn attach_deferred_handoff(&mut self, handoff: DeferredHandoff) -> io::Result<()> {
        self.deferred_handoff = Some(handoff);
        self.validate()
    }

    /// Fixtures run as the build account; production plans take this value from the source Home.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_fixture_original_uid(&mut self, uid: u32) {
        self.0.home = Home::new(
            self.home.volume.clone(),
            self.home.relative().clone(),
            self.home.inode(),
            uid,
            self.home.subvolume().cloned(),
        );
    }

    #[cfg(target_os = "linux")]
    pub fn write_private(&self, directory: &Path) -> io::Result<PathBuf> {
        store::write(self, directory)
    }
    #[cfg(target_os = "linux")]
    pub fn read_private(directory: &Path, operation: &str) -> io::Result<Self> {
        store::read(directory, operation)
    }

    /// Direct Linux installation must retain the exact source partition; the ordinary planner picks
    /// free space and never resizes Linux. Metadata only: the helper must prove recorded and actual
    /// resize completion before publishing the handoff.
    #[cfg(target_os = "linux")]
    pub fn for_completed_staging(&self, installation: &staging::plan::Plan) -> io::Result<Self> {
        self.validate_for_install(installation)?;
        let mut result = self.clone();
        if let Some(shrink) = &installation.shrink
            && result
                .home
                .volume
                .partition_guid
                .eq_ignore_ascii_case(&shrink.partition_guid)
        {
            result.home.volume.last_lba = result
                .home
                .volume
                .first_lba
                .checked_add(shrink.to_bytes / result.home.volume.sector_bytes)
                .and_then(|end| end.checked_sub(1))
                .ok_or_else(|| refusal("Retained source resize overflows."))?;
        }
        if let Some(raw) = &self.flatpak_inventory {
            let inventory =
                super::flatpak_transfer::HandoffInventory::parse(&serde_json::to_vec(raw)?)
                    .map_err(refusal)?;
            result.flatpak_inventory = Some(serde_json::from_slice(
                &inventory
                    .for_completed_staging(installation)
                    .map_err(refusal)?
                    .bytes()
                    .map_err(refusal)?,
            )?);
        }
        result.validate_for_install(installation)?;
        Ok(result)
    }

    #[cfg(target_os = "linux")]
    pub fn validate_for_install(&self, plan: &staging::plan::Plan) -> io::Result<()> {
        self.validate()?;
        let source = &self.home.volume;
        if plan.basis.host_os() != staging_handoff_model::HostOs::Linux
            || !plan.basis.disk.guid.eq_ignore_ascii_case(&source.disk_guid)
            || plan.basis.disk.size_bytes != source.disk_bytes
            || plan.basis.disk.sector_bytes != source.sector_bytes
        {
            return Err(refusal(
                "Migration must remain on the reviewed Linux disk and retain its source volume.",
            ));
        }
        let matches: Vec<_> = plan
            .basis
            .disk
            .partitions
            .iter()
            .filter(|partition| partition.guid.eq_ignore_ascii_case(&source.partition_guid))
            .collect();
        let [partition] = matches.as_slice() else {
            return Err(refusal(
                "Migration source partition is absent or ambiguous.",
            ));
        };
        let effective_last = if let Some(shrink) = &plan.shrink {
            let volume = plan.basis.linux_btrfs_volume.as_ref().ok_or_else(|| {
                refusal("Linux source resize has no authenticated Btrfs binding.")
            })?;
            if source
                .partition_guid
                .eq_ignore_ascii_case(&shrink.partition_guid)
            {
                if source.filesystem != Filesystem::Btrfs
                    || !source
                        .filesystem_uuid
                        .eq_ignore_ascii_case(&volume.filesystem_uuid)
                    || volume.partition_guid != shrink.partition_guid
                    || volume.first_lba != source.first_lba
                    || shrink.from_bytes
                        != (partition.last_lba - partition.first_lba + 1) * source.sector_bytes
                    || shrink.to_bytes >= shrink.from_bytes
                    || shrink.to_bytes % source.sector_bytes != 0
                {
                    return Err(refusal(
                        "Retained source resize differs from the reviewed Btrfs volume.",
                    ));
                }
                source
                    .first_lba
                    .checked_add(shrink.to_bytes / source.sector_bytes)
                    .and_then(|end| end.checked_sub(1))
                    .ok_or_else(|| refusal("Retained source resize overflows."))?
            } else {
                source.last_lba
            }
        } else {
            source.last_lba
        };
        if partition.first_lba != source.first_lba
            || (partition.last_lba != source.last_lba && effective_last != source.last_lba)
        {
            return Err(refusal("Migration source partition geometry changed."));
        }
        for (first, last) in [
            (plan.target_extent.first_lba, plan.target_extent.last_lba),
            (plan.staging_esp.first_lba, plan.staging_esp.last_lba),
            (plan.staging_root.first_lba, plan.staging_root.last_lba),
        ] {
            if first <= effective_last && last >= source.first_lba {
                return Err(refusal("Installation would overlap the migration source."));
            }
        }
        if let Some(raw) = &self.flatpak_inventory {
            let bytes = serde_json::to_vec(raw)?;
            let inventory =
                super::flatpak_transfer::HandoffInventory::parse(&bytes).map_err(refusal)?;
            for (_, root) in inventory.sources() {
                Self::parse(root)?.validate_for_install(plan)?;
            }
        }
        Ok(())
    }
}

impl Plan {
    pub fn home_inode(&self) -> u64 {
        self.home.inode()
    }
    pub fn home_uid(&self) -> u32 {
        self.home.original_uid()
    }
}
