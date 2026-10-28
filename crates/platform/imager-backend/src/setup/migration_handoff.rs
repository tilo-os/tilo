//! Metadata-only Linux migration intent across installation/reboot.
//!
//! A plan records consent and stable source identities; it grants neither a
//! mount nor a copy. After reboot, a fresh read-only source grant must match its
//! GPT partition, filesystem and home inode. Host-native `/dev` or mount paths
//! are never serialized as target authority. No account secrets or home-file
//! contents belong in this format or on installation media.
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
    AccessMode, InstalledScratchFacts, InstalledScratchIdentity, ParticipantNamespace,
    ResolvedSource, RetainedSource, capture, capture_mounted_entry, mount_authenticated_read_only,
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
#[cfg(all(test, target_os = "linux"))]
mod tests;
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub(in crate::setup) fn fixture_plan() -> Plan {
    fixture::sample()
}
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub use linux::capture_fixture;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
pub use linux::resolve;

pub const SCHEMA: u32 = 1;
pub const MAX_BYTES: usize = 64 * 1024;
/// The normal installed account this same-computer handoff may target.
pub const DEFERRED_ORIGINAL_UID: u32 = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Categories {
    pub home: bool,
    pub flatpak: bool,
    pub accounts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
    Ext4,
    Xfs,
    Btrfs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceVolume {
    pub disk_guid: String,
    pub disk_bytes: u64,
    pub sector_bytes: u64,
    pub partition_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub filesystem_uuid: String,
    pub filesystem: Filesystem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Home {
    pub volume: SourceVolume,
    /// Relative to the top-level filesystem, including any Btrfs subvolume
    /// root from mountinfo. Native byte names round-trip without UTF-8 loss.
    relative: NativePath,
    inode: u64,
    original_uid: u32,
    /// Btrfs subvolume UUID/tree ID distinguish reused inode numbers. The
    /// existing native durability service supplies this binding.
    subvolume: Option<super::migration_durability::Identity>,
}

impl Home {
    fn validate_identity(&self) -> io::Result<()> {
        let volume = &self.volume;
        if !uuid(&volume.disk_guid)
            || !uuid(&volume.partition_guid)
            || !uuid(&volume.filesystem_uuid)
            || volume.disk_bytes == 0
            || !matches!(volume.sector_bytes, 512 | 4096)
            || volume.first_lba == 0
            || volume.last_lba < volume.first_lba
            || volume
                .last_lba
                .checked_add(1)
                .and_then(|last| last.checked_mul(volume.sector_bytes))
                .is_none_or(|end| end > volume.disk_bytes)
            || self.inode == 0
            || (volume.filesystem == Filesystem::Btrfs) != self.subvolume.is_some()
        {
            return Err(refusal(
                "Migration source volume or home identity is incomplete.",
            ));
        }
        if self
            .subvolume
            .as_ref()
            .is_some_and(|identity| !identity.binds_btrfs(&volume.filesystem_uuid, self.inode))
        {
            return Err(refusal(
                "Migration Btrfs identity does not match its filesystem or home.",
            ));
        }
        Ok(())
    }
    pub fn original_uid(&self) -> u32 {
        self.original_uid
    }
}

/// Legacy plans copy data; retained access requires an explicit new policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HomePolicy {
    #[default]
    Copy,
    AccessReadOnly,
    AccessReadWrite,
}
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}
/// Opaque locator of the selected-source journal kept in the retained Home.
/// It names an exact operation and journal digest, never a path, profile name,
/// credential or content key. Installation media can carry this metadata
/// without carrying private source data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredHandoff {
    pub schema: u32,
    pub original_uid: u32,
    pub journal_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: u32,
    pub operation_id: String,
    pub categories: Categories,
    #[serde(default, skip_serializing_if = "is_default")]
    pub home_policy: HomePolicy,
    pub home: Home,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flatpak_inventory: Option<serde_json::Value>,
    /// Nested subtrees the person excluded from Home, relative to the selected
    /// Home. Consent metadata, not a path grant: the receiving producer
    /// revalidates each path under its own pinned source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub home_exclusions: Vec<NativePath>,
    /// Same-computer selections whose private source journal stays in the
    /// retained Home. Absent on every earlier plan, so released forms continue
    /// to parse and retain their original meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_handoff: Option<DeferredHandoff>,
}

fn refusal(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil())
}
fn operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Plan {
    /// Narrow an explicitly selected application transfer while retaining the
    /// original operation, source identity and reviewed signed inventory.
    #[cfg(target_os = "linux")]
    pub fn applications_copy_plan(&self) -> io::Result<Self> {
        self.validate()?;
        if !self.categories.flatpak {
            return Err(refusal("Application transfer was not selected"));
        }
        let inventory = self
            .flatpak_inventory
            .as_ref()
            .ok_or_else(|| refusal("Reviewed application inventory is missing"))?;
        let inventory = super::flatpak_transfer::HandoffInventory::parse(
            &serde_json::to_vec(inventory).map_err(|error| refusal(error.to_string()))?,
        )
        .map_err(refusal)?;
        inventory
            .validate_operation(&self.operation_id)
            .map_err(refusal)?;
        let mut narrowed = self.clone();
        narrowed.categories = Categories {
            home: false,
            flatpak: true,
            accounts: false,
        };
        narrowed.home_policy = HomePolicy::Copy;
        narrowed.validate()?;
        Ok(narrowed)
    }

    /// Decode the reviewed Home exclusions as normalized relative paths.
    ///
    /// # Errors
    /// Refuses unrepresentable bytes, non-relative components and exclusions
    /// without selected Home data.
    pub fn home_exclusion_paths(&self) -> io::Result<Vec<std::path::PathBuf>> {
        if !self.categories.home && !self.home_exclusions.is_empty() {
            return Err(refusal("Home exclusions require selected Home data"));
        }
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
        self.home.validate_identity()?;
        self.home_exclusion_paths()?;
        if let Some(handoff) = &self.deferred_handoff
            && (handoff.schema != 1
                || handoff.original_uid != self.home.original_uid
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
        let path = self.home.relative.decode().map_err(refusal)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            if bytes.contains(&0)
                || bytes
                    .split(|byte| *byte == b'/')
                    .any(|part| part.is_empty() || part == b"." || part == b"..")
            {
                return Err(refusal(
                    "Migration home locator is not a normalized native path.",
                ));
            }
        }
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

    /// Digest of the plan without the journal locator. The private journal
    /// stores this value, while the plan stores the journal digest, so the two
    /// records bind each other without a circular digest.
    ///
    /// # Errors
    /// Refuses an invalid base plan or one that cannot be serialized.
    pub fn base_digest(&self) -> io::Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut base = self.clone();
        base.deferred_handoff = None;
        base.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(&base)?).into())
    }

    /// Attach an opaque journal locator only after the journal itself exists.
    ///
    /// # Errors
    /// Refuses a locator for another UID, a zero digest or an invalid plan.
    pub fn attach_deferred_handoff(&mut self, handoff: DeferredHandoff) -> io::Result<()> {
        self.deferred_handoff = Some(handoff);
        self.validate()
    }

    /// Synthetic fixtures run as the build account, not as the reviewed source
    /// account. Production plans always take this value from the source Home.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_fixture_original_uid(&mut self, uid: u32) {
        self.home.original_uid = uid;
    }

    #[cfg(target_os = "linux")]
    pub fn write_private(&self, directory: &Path) -> io::Result<PathBuf> {
        store::write(self, directory)
    }
    #[cfg(target_os = "linux")]
    pub fn read_private(directory: &Path, operation: &str) -> io::Result<Self> {
        store::read(directory, operation)
    }

    /// Direct Linux installation must retain the exact source partition. The
    /// ordinary staging planner chooses free space and never resizes Linux.
    /// Produce prospective retained geometry from the reviewed installation.
    /// This is metadata only: the elevated helper must independently prove
    /// recorded and actual resize completion before publishing the handoff.
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
        self.home.inode
    }
    pub fn home_uid(&self) -> u32 {
        self.home.original_uid
    }
}
