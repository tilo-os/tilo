//! Secret-free source hints. These types grant neither a mount nor a copy;
//! consumers must re-attest the source before using it.
use super::{Extent, Handoff, HostOs, is_guid};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    schema: u32,
    operation_id: String,
    categories: Categories,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home_policy: Option<Policy>,
    home: Home,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flatpak_inventory: Option<FlatpakInventory>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Categories {
    home: bool,
    flatpak: bool,
    accounts: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Policy {
    Copy,
    AccessReadOnly,
    AccessReadWrite,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Home {
    volume: Volume,
    relative: NativePath,
    inode: u64,
    original_uid: u32,
    subvolume: Option<Identity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
enum NativePath {
    Unix(Vec<u8>),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    volume: Vec<u8>,
    inode: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Volume {
    disk_guid: String,
    disk_bytes: u64,
    sector_bytes: u64,
    partition_guid: String,
    first_lba: u64,
    last_lba: u64,
    filesystem_uuid: String,
    filesystem: Filesystem,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Filesystem {
    Ext4,
    Xfs,
    Btrfs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum FlatpakInventory {
    Inline(ReviewedInventory),
    Protected(ProtectedInventory),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectedInventory {
    kind: String,
    schema: u32,
    operation: String,
    len: u64,
    sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedInventory {
    schema: u32,
    inventory: Refs,
    selected: Vec<InstalledRef>,
    installations: Vec<InstallationSource>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refs {
    refs: Vec<InstalledRef>,
    remotes: Vec<Remote>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Installation {
    User,
    System(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledRef {
    reference: String,
    origin: String,
    installation: Installation,
    name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Remote {
    installation: Installation,
    name: String,
    url: String,
    options: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallationSource {
    installation: Installation,
    source_plan: Vec<u8>,
    repositories: Vec<RepositoryTrust>,
    deployments: Vec<Deployment>,
    config_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryTrust {
    remote: Remote,
    definition_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Deployment {
    reference: String,
    commit: String,
    deploy_sha256: [u8; 32],
}

fn non_nil_guid(value: &str) -> bool {
    is_guid(value) && value.bytes().any(|b| b.is_ascii_hexdigit() && b != b'0')
}

impl Migration {
    pub(super) fn validate(&self, handoff: &Handoff) -> Result<(), String> {
        let volume = &self.home.volume;
        let NativePath::Unix(path) = &self.home.relative;
        let source = Extent {
            first_lba: volume.first_lba,
            last_lba: volume.last_lba,
        };
        if self.schema != 1
            || self.operation_id != handoff.operation_id
            || !(self.categories.home || self.categories.flatpak || self.categories.accounts)
            || (self.home_policy.is_some_and(|p| p != Policy::Copy) && !self.categories.home)
            || handoff.host_os != HostOs::Linux
            || !volume.disk_guid.eq_ignore_ascii_case(&handoff.disk_guid)
            || volume.disk_bytes != handoff.disk_size_bytes
            || ![
                &volume.disk_guid,
                &volume.partition_guid,
                &volume.filesystem_uuid,
            ]
            .into_iter()
            .all(|v| non_nil_guid(v))
            || !matches!(volume.sector_bytes, 512 | 4096)
            || !source.valid()
            || volume
                .last_lba
                .checked_add(1)
                .and_then(|n| n.checked_mul(volume.sector_bytes))
                .is_none_or(|end| end > volume.disk_bytes)
            || self.home.inode == 0
            || path.is_empty()
            || path.len() > 4096
            || path.contains(&0)
            || path.split(|b| *b == b'/').count() > 128
            || path
                .split(|b| *b == b'/')
                .any(|p| p.is_empty() || p == b"." || p == b"..")
        {
            return Err(
                "migration source or consent differs from retained Linux installation".into(),
            );
        }
        match (&self.home.subvolume, volume.filesystem) {
            (Some(id), Filesystem::Btrfs) => {
                let uuid = volume.filesystem_uuid.replace('-', "").to_ascii_lowercase();
                let identity_uuid =
                    id.volume
                        .iter()
                        .skip(1)
                        .take(16)
                        .fold(String::new(), |mut text, byte| {
                            let _ = write!(text, "{byte:02x}");
                            text
                        });
                if id.inode != self.home.inode
                    || id.volume.len() != 41
                    || id.volume.first() != Some(&3)
                    || identity_uuid != uuid
                {
                    return Err("migration Btrfs identity differs".into());
                }
            }
            (None, Filesystem::Ext4 | Filesystem::Xfs) => {}
            _ => return Err("migration subvolume identity differs".into()),
        }
        for staged in [&handoff.staging_esp, &handoff.staging_root] {
            if volume.partition_guid.eq_ignore_ascii_case(&staged.partuuid)
                || source.overlaps(staged.extent())
            {
                return Err("migration source overlaps staged installer".into());
            }
        }
        if source.overlaps(handoff.target_extent) {
            return Err("migration source overlaps installation space".into());
        }
        if let Some(inventory) = &self.flatpak_inventory {
            if !self.categories.flatpak {
                return Err("Flatpak inventory lacks migration consent".into());
            }
            match inventory {
                FlatpakInventory::Protected(binding) => {
                    if binding.kind != "protected-original-flatpak"
                        || binding.schema != 1
                        || binding.operation != self.operation_id
                        || binding.len == 0
                        || binding.len > 8 * 1024 * 1024
                        || binding.sha256 == [0; 32]
                    {
                        return Err("protected original inventory binding differs".into());
                    }
                }
                FlatpakInventory::Inline(inventory) => inventory.validate(handoff)?,
            }
        }
        Ok(())
    }
}
impl ReviewedInventory {
    fn validate(&self, handoff: &Handoff) -> Result<(), String> {
        if !matches!(self.schema, 1 | 2)
            || self.installations.len() > 64
            || self.inventory.refs.len() > 100_000
            || self.selected.len() > 100_000
            || self.inventory.remotes.len() > 4096
        {
            return Err("Flatpak inventory bounds or schema differ".into());
        }
        for item in &self.selected {
            if !self.inventory.refs.contains(item) {
                return Err("Flatpak selection is outside inventory".into());
            }
        }
        for source in &self.installations {
            if source.source_plan.len() > super::MAX_BYTES
                || source.repositories.len() > 4096
                || source.deployments.len() > 100_000
            {
                return Err("Flatpak source exceeds bounds".into());
            }
            let plan: Migration =
                serde_json::from_slice(&source.source_plan).map_err(|e| e.to_string())?;
            if plan.flatpak_inventory.is_some() {
                return Err("recursive Flatpak source inventory".into());
            }
            plan.validate(handoff)?;
            for repo in &source.repositories {
                if repo.remote.installation != source.installation
                    || !self.inventory.remotes.contains(&repo.remote)
                {
                    return Err("Flatpak repository binding differs".into());
                }
            }
            for deployment in &source.deployments {
                if deployment.commit.len() != 64
                    || !deployment.commit.bytes().all(|b| b.is_ascii_hexdigit())
                    || !self.inventory.refs.iter().any(|r| {
                        r.installation == source.installation && r.reference == deployment.reference
                    })
                {
                    return Err("Flatpak deployment binding differs".into());
                }
            }
        }
        Ok(())
    }
}
