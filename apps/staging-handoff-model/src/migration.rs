//! Secret-free source hints. These types grant neither a mount nor a copy; consumers must
//! re-attest the source before using it.
use super::{Extent, Handoff, HostOs, is_guid, is_operation_id};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SCHEMA: u32 = 1;
pub const MAX_BYTES: usize = 64 * 1024;
pub const DEFERRED_ORIGINAL_UID: u32 = 1000;
pub const MAX_EXCLUSIONS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub schema: u32,
    pub operation_id: String,
    pub categories: Categories,
    #[serde(default, skip_serializing_if = "is_default")]
    pub home_policy: HomePolicy,
    pub home: Home,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_computer_applications_scope"
    )]
    pub pending_computer_applications_scope: Option<ComputerApplicationsScope>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_flatpak_inventory"
    )]
    pub flatpak_inventory: Option<serde_json::Value>,
    /// Consent metadata, not a path grant: the receiving producer revalidates each path under its
    /// own pinned source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub home_exclusions: Vec<NativePath>,
    /// Absent on every earlier plan, so released forms keep parsing with their original meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_handoff: Option<DeferredHandoff>,
}

/// Names an exact operation and journal digest, never a path, profile name, credential or content
/// key, so installation media can carry it without private source data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredHandoff {
    pub schema: u32,
    pub original_uid: u32,
    pub journal_sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Categories {
    pub home: bool,
    pub flatpak: bool,
    pub accounts: bool,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Home {
    pub volume: SourceVolume,
    /// Relative to the top-level filesystem, including any Btrfs subvolume root from mountinfo;
    /// native byte names round-trip without UTF-8 loss.
    relative: NativePath,
    inode: u64,
    original_uid: u32,
    /// Btrfs subvolume UUID/tree ID distinguish reused inode numbers; the native durability service
    /// supplies this binding.
    #[serde(deserialize_with = "required_subvolume")]
    subvolume: Option<Identity>,
}

impl Home {
    #[must_use]
    pub fn new(
        volume: SourceVolume,
        relative: NativePath,
        inode: u64,
        original_uid: u32,
        subvolume: Option<Identity>,
    ) -> Self {
        Self {
            volume,
            relative,
            inode,
            original_uid,
            subvolume,
        }
    }

    #[must_use]
    pub fn relative(&self) -> &NativePath {
        &self.relative
    }

    #[must_use]
    pub fn inode(&self) -> u64 {
        self.inode
    }

    #[must_use]
    pub fn original_uid(&self) -> u32 {
        self.original_uid
    }

    #[must_use]
    pub fn subvolume(&self) -> Option<&Identity> {
        self.subvolume.as_ref()
    }

    pub fn validate_subvolume(&self) -> Result<(), String> {
        let volume = &self.volume;
        match (&self.subvolume, volume.filesystem) {
            (Some(id), Filesystem::Btrfs) => {
                if !id.binds_btrfs(&volume.filesystem_uuid, self.inode) {
                    return Err("migration Btrfs identity differs".into());
                }
            }
            (None, Filesystem::Ext4 | Filesystem::Xfs) => {}
            _ => return Err("migration subvolume identity differs".into()),
        }
        Ok(())
    }

    pub fn validate_identity(&self) -> Result<(), String> {
        let volume = &self.volume;
        let source = Extent {
            first_lba: volume.first_lba,
            last_lba: volume.last_lba,
        };
        if volume.disk_bytes == 0
            || volume.disk_bytes >= super::MAX_LBA
            || self.inode == 0
            || self.inode >= super::MAX_LBA
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
        {
            return Err(
                "migration source or consent differs from retained Linux installation".into(),
            );
        }
        self.validate_subvolume()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
pub enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl NativePath {
    #[cfg(any(unix, windows))]
    pub fn encode(path: &std::path::Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    /// The number of native units the path occupies.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Unix(units) => units.len(),
            Self::Windows(units) => units.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn decode(&self) -> Result<PathBuf, String> {
        if self.len() > 4096 {
            return Err("persisted migration path exceeds bounds".into());
        }
        match self {
            #[cfg(unix)]
            Self::Unix(bytes) if !bytes.contains(&0) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes.clone())))
            }
            #[cfg(windows)]
            Self::Windows(units) if !units.contains(&0) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_wide(units)))
            }
            _ => Err("persisted migration path has a platform mismatch or NUL".into()),
        }
    }

    /// Whether this path is a normalized non-empty relative path (at most 4096 code units,
    /// no NUL, and no empty, `.` or `..` segments).
    #[must_use]
    pub fn is_normalized_relative(&self) -> bool {
        match self {
            Self::Unix(bytes) => valid_unix_relative(bytes),
            Self::Windows(units) => valid_windows_relative(units),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    volume: Vec<u8>,
    inode: u64,
}

impl Identity {
    #[must_use]
    pub fn new(volume: Vec<u8>, inode: u64) -> Self {
        Self { volume, inode }
    }

    #[must_use]
    pub fn binds_filesystem(&self, filesystem_uuid: &str) -> bool {
        let durable = (self.volume.len() == 17 && self.volume.first() == Some(&1))
            || (self.volume.len() == 41 && self.volume.first() == Some(&3));
        durable
            && parse_guid_bytes(filesystem_uuid)
                .is_some_and(|bytes| self.volume.get(1..17) == Some(bytes.as_slice()))
    }

    #[must_use]
    pub fn binds_btrfs(&self, filesystem_uuid: &str, inode: u64) -> bool {
        self.inode == inode
            && self.volume.len() == 41
            && self.volume.first() == Some(&3)
            && parse_guid_bytes(filesystem_uuid)
                .is_some_and(|bytes| self.volume.get(1..17) == Some(bytes.as_slice()))
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
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

struct UniqueJsonSeed;

impl<'de> serde::de::DeserializeSeed<'de> for UniqueJsonSeed {
    type Value = serde_json::Value;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(UniqueJsonValue)
    }
}

struct UniqueJsonValue;

impl<'de> serde::de::Visitor<'de> for UniqueJsonValue {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without repeated object keys")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::from(v))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(UniqueJsonSeed)? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
            }
            let value = map.next_value_seed(UniqueJsonSeed)?;
            object.insert(key, value);
        }
        Ok(serde_json::Value::Object(object))
    }
}

fn present_flatpak_inventory<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    let value = deserializer.deserialize_any(UniqueJsonValue)?;
    if value.is_null() {
        return Err(serde::de::Error::custom("flatpak_inventory cannot be null"));
    }
    serde_json::from_value::<FlatpakInventory>(value.clone()).map_err(serde::de::Error::custom)?;
    Ok(Some(value))
}

fn required_subvolume<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Identity>, D::Error> {
    Option::<Identity>::deserialize(deserializer)
}

fn present_computer_applications_scope<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ComputerApplicationsScope>, D::Error> {
    ComputerApplicationsScope::deserialize(deserializer).map(Some)
}

fn non_nil_guid(value: &str) -> bool {
    is_guid(value) && value.bytes().any(|b| b.is_ascii_hexdigit() && b != b'0')
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(byte.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

fn parse_guid_bytes(value: &str) -> Option<[u8; 16]> {
    if !is_guid(value) {
        return None;
    }
    let mut bytes = [0u8; 16];
    let mut hex = value.bytes().filter(|b| *b != b'-');
    for slot in &mut bytes {
        let hi = hex_nibble(hex.next()?)?;
        let lo = hex_nibble(hex.next()?)?;
        *slot = (hi << 4) | lo;
    }
    hex.next().is_none().then_some(bytes)
}

fn valid_unix_relative(path: &[u8]) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(&0)
        && path
            .split(|b| *b == b'/')
            .all(|p| !p.is_empty() && p != b"." && p != b"..")
}

fn valid_windows_relative(units: &[u16]) -> bool {
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    const DOT: u16 = b'.' as u16;
    const COLON: u16 = b':' as u16;
    !units.is_empty()
        && units.len() <= 4096
        && !units.contains(&0)
        && units
            .split(|u| *u == SLASH || *u == BACKSLASH)
            .all(|p| !p.is_empty() && p != [DOT] && p != [DOT, DOT] && !p.contains(&COLON))
}

/// Descriptive scope for pending Computer applications composition across reboot.
/// Never authority on its own; each consumer re-verifies against the registered declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerApplicationsScope {
    pub schema: u32,
    pub source_operation: String,
    pub applications_operation: String,
    pub staging_operation: String,
    pub staging_plan_sha256: String,
    pub setup_binding: String,
    pub identity_public_key: [u8; 32],
}

impl ComputerApplicationsScope {
    /// Validate the scope's own fields, canonical operation IDs, distinctness, digest and binding.
    pub fn validate_fields(&self, expected_source_operation: &str) -> Result<(), String> {
        let operations = [
            &self.source_operation,
            &self.applications_operation,
            &self.staging_operation,
        ];
        if self.schema != 1
            || operations.iter().any(|id| !is_operation_id(id))
            || self.source_operation == self.applications_operation
            || self.source_operation == self.staging_operation
            || self.applications_operation == self.staging_operation
            || self.source_operation != expected_source_operation
            || !lower_hex_digest(&self.staging_plan_sha256)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
        {
            return Err("pending Computer applications scope differs".into());
        }
        Ok(())
    }

    /// Validate the scope against its carrying migration plan's operation and consent.
    pub fn validate_for_migration(&self, migration: &Migration) -> Result<(), String> {
        self.validate_fields(&migration.operation_id)?;
        if !migration.categories.home
            || migration.categories.flatpak
            || migration.home_policy != HomePolicy::AccessReadWrite
        {
            return Err("pending Computer applications scope differs".into());
        }
        Ok(())
    }
}

impl Migration {
    /// Validate descriptive source metadata without granting source access.
    pub fn validate_standalone(&self) -> Result<(), String> {
        self.validate_context(None)
    }

    /// Validate the source against the retained disk and installation extents.
    pub fn validate_in_handoff(&self, handoff: &Handoff) -> Result<(), String> {
        self.validate_context(Some(handoff))
    }

    /// Validate reviewed Home exclusions: normalized relative Unix paths within bounds,
    /// only permitted when Home data is selected.
    pub fn validate_home_exclusions(&self) -> Result<(), String> {
        if self.home_exclusions.is_empty() {
            return Ok(());
        }
        if !self.categories.home {
            return Err("Home exclusions require selected Home data".into());
        }
        if self.home_exclusions.len() > MAX_EXCLUSIONS {
            return Err("too many excluded source subtrees".into());
        }
        for exclusion in &self.home_exclusions {
            if !matches!(exclusion, NativePath::Unix(_)) {
                return Err("excluded source subtree is not a Unix native path".into());
            }
            if !exclusion.is_normalized_relative() {
                return Err("excluded source subtree is not a normalized relative path".into());
            }
        }
        Ok(())
    }

    fn validate_source(&self, handoff: Option<&Handoff>) -> Result<(), String> {
        let volume = &self.home.volume;
        let NativePath::Unix(path) = &self.home.relative else {
            return Err(
                "migration source or consent differs from retained Linux installation".into(),
            );
        };
        let source = Extent {
            first_lba: volume.first_lba,
            last_lba: volume.last_lba,
        };
        if self.schema != SCHEMA
            || !is_guid(&self.operation_id)
            || self.deferred_handoff.as_ref().is_some_and(|handoff| {
                handoff.schema != SCHEMA
                    || handoff.original_uid != DEFERRED_ORIGINAL_UID
                    || handoff.original_uid != self.home.original_uid
                    || handoff.journal_sha256 == [0; 32]
            })
            || !(self.categories.home || self.categories.flatpak || self.categories.accounts)
            || (self.home_policy != HomePolicy::Copy && !self.categories.home)
            || handoff.is_some_and(|h| {
                self.operation_id != h.operation_id
                    || h.host_os != HostOs::Linux
                    || !volume.disk_guid.eq_ignore_ascii_case(&h.disk_guid)
                    || volume.disk_bytes != h.disk_size_bytes
            })
            || !valid_unix_relative(path)
            || path.split(|b| *b == b'/').count() > 128
        {
            return Err(
                "migration source or consent differs from retained Linux installation".into(),
            );
        }
        self.home.validate_identity()?;
        self.validate_home_exclusions()?;
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

    fn validate_context(&self, handoff: Option<&Handoff>) -> Result<(), String> {
        self.validate_source(handoff)?;
        if let Some(scope) = &self.pending_computer_applications_scope {
            scope.validate_for_migration(self)?;
        }
        if let Some(raw) = &self.flatpak_inventory {
            if !self.categories.flatpak {
                return Err("Flatpak inventory lacks migration consent".into());
            }
            let inventory: FlatpakInventory =
                serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
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
                || source.source_plan.len() > MAX_BYTES
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
