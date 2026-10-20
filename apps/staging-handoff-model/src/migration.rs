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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_policy"
    )]
    home_policy: Option<Policy>,
    home: Home,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_computer_applications_scope: Option<PendingComputerApplicationsScope>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_inventory"
    )]
    flatpak_inventory: Option<FlatpakInventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deferred_handoff: Option<DeferredHandoff>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeferredHandoff {
    schema: u32,
    original_uid: u32,
    journal_sha256: [u8; 32],
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
    #[serde(deserialize_with = "required_subvolume")]
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

fn present_inventory<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<FlatpakInventory>, D::Error> {
    FlatpakInventory::deserialize(deserializer).map(Some)
}

fn required_subvolume<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Identity>, D::Error> {
    Option::<Identity>::deserialize(deserializer)
}

fn present_policy<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Policy>, D::Error> {
    Policy::deserialize(deserializer).map(Some)
}

fn non_nil_guid(value: &str) -> bool {
    is_guid(value) && value.bytes().any(|b| b.is_ascii_hexdigit() && b != b'0')
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingComputerApplicationsScope {
    schema: u32,
    source_operation: String,
    applications_operation: String,
    staging_operation: String,
    staging_plan_sha256: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
}

impl Migration {
    /// Validate descriptive source metadata without granting source access.
    ///
    /// # Errors
    /// Returns the first malformed source, consent or inventory binding.
    pub fn validate_standalone(&self) -> Result<(), String> {
        self.validate_context(None)
    }

    /// Validate the source against the retained disk and installation extents.
    ///
    /// # Errors
    /// Returns the first malformed or mismatched source binding.
    pub fn validate_in_handoff(&self, handoff: &Handoff) -> Result<(), String> {
        self.validate_context(Some(handoff))
    }

    fn validate_source(&self, handoff: Option<&Handoff>) -> Result<(), String> {
        let volume = &self.home.volume;
        let NativePath::Unix(path) = &self.home.relative;
        let source = Extent {
            first_lba: volume.first_lba,
            last_lba: volume.last_lba,
        };
        if self.schema != 1
            || !is_guid(&self.operation_id)
            || self.deferred_handoff.as_ref().is_some_and(|handoff| {
                handoff.schema != 1
                    || handoff.original_uid != 1000
                    || handoff.original_uid != self.home.original_uid
                    || handoff.journal_sha256 == [0; 32]
            })
            || !(self.categories.home || self.categories.flatpak || self.categories.accounts)
            || (self.home_policy.is_some_and(|p| p != Policy::Copy) && !self.categories.home)
            || handoff.is_some_and(|h| {
                self.operation_id != h.operation_id
                    || h.host_os != HostOs::Linux
                    || !volume.disk_guid.eq_ignore_ascii_case(&h.disk_guid)
                    || volume.disk_bytes != h.disk_size_bytes
            })
            || volume.disk_bytes == 0
            || volume.disk_bytes >= super::MAX_LBA
            || self.home.inode >= super::MAX_LBA
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
        self.validate_subvolume()?;
        if let Some(handoff) = handoff {
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
        }
        Ok(())
    }

    fn validate_subvolume(&self) -> Result<(), String> {
        let volume = &self.home.volume;
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
        Ok(())
    }

    fn validate_context(&self, handoff: Option<&Handoff>) -> Result<(), String> {
        self.validate_source(handoff)?;
        if let Some(scope) = &self.pending_computer_applications_scope {
            let operations = [
                &scope.source_operation,
                &scope.applications_operation,
                &scope.staging_operation,
            ];
            if scope.schema != 1
                || operations
                    .iter()
                    .any(|id| !non_nil_guid(id) || **id != id.to_ascii_lowercase())
                || operations[0] == operations[1]
                || operations[0] == operations[2]
                || operations[1] == operations[2]
                || scope.source_operation != self.operation_id
                || !self.categories.home
                || self.categories.flatpak
                || self.home_policy != Some(Policy::AccessReadWrite)
                || !lower_hex_digest(&scope.staging_plan_sha256)
                || scope.setup_binding.is_empty()
                || scope.setup_binding.len() > 256
                || scope.setup_binding.chars().any(char::is_control)
            {
                return Err("pending Computer applications scope differs".into());
            }
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
                FlatpakInventory::Inline(inventory) => {
                    inventory.validate(&self.operation_id, handoff)?;
                }
            }
        }
        Ok(())
    }
}
pub(super) fn lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Installation {
    fn valid(&self) -> bool {
        match self {
            Self::User => true,
            Self::System(name) => {
                (1..=128).contains(&name.len())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            }
        }
    }
}

fn bounded_text(value: &str) -> bool {
    value.chars().count() <= 4096 && !value.chars().any(|c| (c as u32) < 32)
}

impl Remote {
    fn valid(&self) -> bool {
        let authority = self
            .url
            .strip_prefix("https://")
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        self.installation.valid()
            && [&self.name, &self.url, &self.options]
                .into_iter()
                .all(|s| bounded_text(s))
            && !authority.is_empty()
            && !authority.starts_with(':')
            && !authority.contains('@')
            && !authority.chars().any(char::is_whitespace)
            && !self.url.contains(['?', '#'])
    }
}

impl ReviewedInventory {
    fn validate(&self, operation: &str, handoff: Option<&Handoff>) -> Result<(), String> {
        if !matches!(self.schema, 1 | 2)
            || self.installations.len() > 64
            || self.inventory.refs.len() > 100_000
            || self.selected.len() > 100_000
            || self.inventory.remotes.len() > 4096
        {
            return Err("Flatpak inventory bounds or schema differ".into());
        }
        if self.inventory.refs.iter().chain(&self.selected).any(|r| {
            !r.installation.valid()
                || ![&r.reference, &r.origin, &r.name]
                    .into_iter()
                    .all(|s| bounded_text(s))
        }) || self.inventory.remotes.iter().any(|r| !r.valid())
        {
            return Err("Flatpak metadata text or scope is malformed".into());
        }
        for item in &self.selected {
            if !self.inventory.refs.contains(item) {
                return Err("Flatpak selection is outside inventory".into());
            }
        }
        for source in &self.installations {
            if !source.installation.valid()
                || source.source_plan.len() > super::MAX_BYTES
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
            if plan.operation_id != operation {
                return Err("Flatpak source belongs to another operation".into());
            }
            plan.validate_context(handoff)?;
            for repo in &source.repositories {
                if !repo.remote.valid()
                    || repo.remote.installation != source.installation
                    || !self.inventory.remotes.contains(&repo.remote)
                {
                    return Err("Flatpak repository binding differs".into());
                }
            }
            for deployment in &source.deployments {
                if !lower_hex_digest(&deployment.commit)
                    || !bounded_text(&deployment.reference)
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
