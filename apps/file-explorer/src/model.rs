use chrono::{DateTime, Local};
use host_fs::attributes as xattr;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

/// The Tilo collaborative FUSE mount. Per `fs.service` the daemon mounts
/// at `/var/workspace` (the older `/workspace` was a bug in this app).
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// The extended-attribute the Tilo FUSE driver answers with an entry's sync
/// state. Kept in lock-step with `fs::SYNC_XATTR`.
pub const SYNC_XATTR: &str = "user.tilo.sync";

/// The Tilo FUSE **sync / hydration state** of an entry, read live from the
/// driver via the [`SYNC_XATTR`] extended attribute. The collaborative FS
/// projects a peer's file metadata immediately but only hydrates the bytes on
/// first read, so a workspace file is one of:
///
/// * [`Local`](SyncState::Local) — content is on disk; opening it is instant.
/// * [`Projected`](SyncState::Projected) — metadata only; opening it hydrates
///   from a peer.
/// * [`Downloading`](SyncState::Downloading) — a hydration fetch is in flight.
///
/// `Entry::sync_state` is `None` for anything that isn't on a workspace mount
/// (an ordinary local file carries no such attribute).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    Local,
    Projected,
    Downloading,
}

impl SyncState {
    /// Parse the xattr value the driver returns.
    pub fn from_xattr(bytes: &[u8]) -> Option<Self> {
        match std::str::from_utf8(bytes).ok()?.trim() {
            "local" => Some(SyncState::Local),
            "projected" => Some(SyncState::Projected),
            "downloading" => Some(SyncState::Downloading),
            _ => None,
        }
    }
}

/// Read the Tilo sync state of `path` from its [`SYNC_XATTR`] extended attribute.
/// `None` when the attribute is absent (an ordinary file, off any workspace
/// mount) or unreadable. This is a single `getxattr` — the same call
/// `getfattr -n user.tilo.sync <path>` makes.
pub fn read_sync_state(path: &Path) -> Option<SyncState> {
    match xattr::get(path, SYNC_XATTR) {
        Ok(Some(bytes)) => SyncState::from_xattr(&bytes),
        _ => None,
    }
}

/// The extended attribute the Tilo FUSE driver answers with a file's **thumbnail**
/// (a small PNG the host generates as metadata, ahead of / independent of the
/// content). Kept in lock-step with `fs::THUMB_XATTR`.
pub const THUMB_XATTR: &str = "user.tilo.thumb";

/// Raw 32-byte content/manifest identity exposed by the Tilo FUSE driver.
/// Using this instead of `(path, mtime, size)` makes preview cache entries
/// survive renames and deduplicate identical content across folders and peers.
pub const CONTENT_HASH_XATTR: &str = "user.tilo.content-hash";

/// Raw 16-byte stable collaborative-record UUID.
pub const RECORD_ID_XATTR: &str = "user.tilo.record-id";
pub const METADATA_XATTR: &str = "user.tilo.metadata";
pub const MAX_METADATA_XATTR_BYTES: usize = 2_048;
pub const RECORD_INDEX_XATTR: &str = "user.tilo.record-index";
pub const RECORD_INDEX_V2_XATTR: &str = "user.tilo.record-index.v2";
pub const RECORD_INDEX_V3_XATTR: &str = "user.tilo.record-index.v3";
pub const RECORD_INDEX_BY_ID_XATTR_PREFIX: &str = "user.tilo.record-index.by-id.";
pub const MAX_RECORD_INDEX_XATTR_BYTES: usize = 60 * 1024;
pub const MAX_RECORD_INDEX_ENTRIES: usize = 256;
const RECORD_INDEX_V2_VERSION: u16 = 2;
// Must match the mount authority's portable, single-xattr page contract.
const RECORD_INDEX_PAGE_ENTRIES: usize = 32;
const MAX_RECORD_INDEX_PAGE_BYTES: usize = 48 * 1024;
pub const AVAILABILITY_XATTR: &str = "user.tilo.availability";
pub const AVAILABILITY_CONTROL_XATTR: &str = "user.tilo.availability-control";
pub const PIN_XATTR: &str = "user.tilo.pin";
pub const SHARE_XATTR: &str = "user.tilo.share";
pub const SHARE_CONTROL_XATTR: &str = "user.tilo.share-control";
pub const HISTORY_XATTR: &str = "user.tilo.history";
pub const HISTORY_CONTROL_XATTR: &str = "user.tilo.history-control";

/// Fetch a bounded immutable version preview from the collaborative
/// filesystem. Binary/invalid UTF-8 versions intentionally return `None`.
pub fn read_history_text_preview(
    path: &Path,
    version_id: collab_types::FileVersionId,
) -> Option<String> {
    let name = format!(
        "{}{}",
        collab_types::HISTORY_PREVIEW_XATTR_PREFIX,
        version_id.0
    );
    let bytes = xattr::get(path, name).ok()??;
    if bytes.len() > collab_types::MAX_HISTORY_PREVIEW_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.chars().any(|character| character == '\0')).then_some(text)
}

/// Read the daemon's non-committing classification for one open conflict.
/// The daemon loads the exact immutable members and base, so this result is
/// intentionally more authoritative than a filename or timestamp heuristic.
pub fn read_merge_preflight(
    path: &Path,
    conflict_id: uuid::Uuid,
) -> Option<collab_types::MergePreflightProjectionV1> {
    let name = format!(
        "{}{}",
        collab_types::MERGE_PREFLIGHT_XATTR_PREFIX,
        conflict_id
    );
    let bytes = xattr::get(path, name).ok()??;
    if bytes.len() > collab_types::MAX_MERGE_PREFLIGHT_BYTES {
        return None;
    }
    let projection: collab_types::MergePreflightProjectionV1 =
        serde_json::from_slice(&bytes).ok()?;
    (projection.conflict_id == conflict_id && projection.validate().is_ok()).then_some(projection)
}
pub const MAX_AVAILABILITY_XATTR_BYTES: usize = 2_048;
pub const MAX_COLLAB_PROJECTION_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataProjection {
    pub schema_version: u16,
    pub record_id: collab_types::FileRecordId,
    pub properties: collab_types::FilePropertiesV1,
}

impl MetadataProjection {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_METADATA_XATTR_BYTES {
            return None;
        }
        let value: Self = serde_json::from_slice(bytes).ok()?;
        if value.schema_version != 1
            || value.record_id.0.is_nil()
            || value.properties.validate().is_err()
        {
            return None;
        }
        Some(value)
    }
}

pub fn read_metadata(path: &Path) -> Option<MetadataProjection> {
    MetadataProjection::parse(&xattr::get(path, METADATA_XATTR).ok()??)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilePermission {
    View,
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ShareSubject {
    Person { person: [u8; 32] },
    BearerLink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareGrantStatus {
    Active,
    Expired,
    Revoked,
    Exhausted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the versioned, redundant share projection contract"
)]
pub struct ShareGrantProjection {
    pub grant_id: String,
    pub subject: ShareSubject,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
    pub expired: bool,
    pub used: bool,
    pub exhausted: bool,
    pub status: ShareGrantStatus,
    pub redemptions: u32,
    pub max_redemptions: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareProjection {
    pub schema_version: u16,
    pub record_id: String,
    pub observed_at_ms: u64,
    pub grants: Vec<ShareGrantProjection>,
}

impl ShareProjection {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_COLLAB_PROJECTION_BYTES {
            return None;
        }
        let value: Self = serde_json::from_slice(bytes).ok()?;
        if value.schema_version != 1 || value.grants.len() > 32 {
            return None;
        }
        Some(value)
    }

    pub fn active_grants(&self) -> usize {
        self.grants
            .iter()
            .filter(|grant| grant.status == ShareGrantStatus::Active)
            .count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryActor {
    pub person: [u8; 32],
    pub device: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryVersion {
    pub id: String,
    pub record_id: String,
    pub parents: Vec<String>,
    pub actor: HistoryActor,
    pub timestamp_ms: u64,
    pub content_hash: Option<[u8; 32]>,
    pub metadata_hash: [u8; 32],
    pub provenance: serde_json::Value,
}

impl HistoryVersion {
    /// Fluent key for the user-facing provenance label. Keeping the wire value
    /// out of presentation lets every history surface share one localized
    /// vocabulary without changing signed metadata.
    pub fn provenance_key(&self) -> &'static str {
        match &self.provenance {
            serde_json::Value::String(value) => match value.as_str() {
                "Create" => "files-history-action-created",
                "Edit" => "files-history-action-edited",
                "Rename" => "files-history-action-renamed",
                "Move" => "files-history-action-moved",
                "Delete" => "files-history-action-deleted",
                "ConflictResolution" => "files-history-action-conflict-resolved",
                _ => "files-history-action-changed",
            },
            serde_json::Value::Object(value) if value.contains_key("Restore") => {
                "files-history-action-restored"
            }
            _ => "files-history-action-changed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryProjection {
    pub schema_version: u16,
    pub record_id: String,
    pub total_versions: u64,
    pub versions_truncated: bool,
    pub heads: Vec<String>,
    pub versions: Vec<HistoryVersion>,
    pub conflicts_truncated: bool,
    pub conflicts: Vec<collab_types::FileConflictV1>,
}

impl HistoryProjection {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_COLLAB_PROJECTION_BYTES {
            return None;
        }
        let value: Self = serde_json::from_slice(bytes).ok()?;
        let record_id = uuid::Uuid::parse_str(&value.record_id).ok()?;
        if value.schema_version != 1
            || value.versions.len() > 12
            || value.conflicts.len() > 8
            || value.heads.len() > collab_types::files::MAX_CONFLICT_VERSIONS
            || value.total_versions < value.versions.len() as u64
            || value.versions.iter().any(|version| {
                version.record_id != value.record_id
                    || version.parents.len() > collab_types::files::MAX_VERSION_PARENTS
                    || version.parents.contains(&version.id)
            })
            || value
                .conflicts
                .iter()
                .any(|conflict| conflict.record_id.0 != record_id || conflict.validate().is_err())
        {
            return None;
        }
        Some(value)
    }

    pub fn open_conflicts(&self) -> impl Iterator<Item = &collab_types::FileConflictV1> {
        self.conflicts
            .iter()
            .filter(|conflict| conflict.status == collab_types::ConflictStatus::Open)
    }
}

pub fn read_share(path: &Path) -> Option<ShareProjection> {
    let bytes = xattr::get(path, SHARE_XATTR).ok()??;
    ShareProjection::parse(&bytes)
}

pub fn read_history(path: &Path) -> Option<HistoryProjection> {
    let bytes = xattr::get(path, HISTORY_XATTR).ok()??;
    HistoryProjection::parse(&bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Residency {
    NotResident,
    Partial { resident_bytes: u64 },
    Resident { bytes: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPhase {
    Idle,
    Queued,
    Downloading,
    Uploading,
    Verifying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub completed_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedTransfer {
    pub phase: TransferPhase,
    pub progress: Option<TransferProgress>,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<u64>,
    pub error_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityPolicy {
    Automatic,
    Pinned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ReplicaEvidence {
    Unknown,
    Known { reachable: u32, durable: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityFreshness {
    pub metadata_observed_at_ms: Option<u64>,
    pub local_verified_at_ms: Option<u64>,
    pub replicas_observed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityProjection {
    pub schema_version: u16,
    pub record_id: String,
    pub current_version: Option<String>,
    pub residency: Residency,
    pub transfer: ProjectedTransfer,
    pub policy: AvailabilityPolicy,
    #[serde(default)]
    pub policy_inherited: bool,
    pub replicas: ReplicaEvidence,
    pub freshness: AvailabilityFreshness,
}

impl AvailabilityProjection {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_AVAILABILITY_XATTR_BYTES {
            return None;
        }
        let value: Self = serde_json::from_slice(bytes).ok()?;
        if value.schema_version != 1
            || value.transfer.last_error.is_some() != value.transfer.last_error_at_ms.is_some()
            || value
                .transfer
                .progress
                .is_some_and(|p| p.total_bytes.is_some_and(|total| p.completed_bytes > total))
        {
            return None;
        }
        Some(value)
    }

    pub fn safely_replicated(&self) -> bool {
        matches!(self.replicas, ReplicaEvidence::Known { durable, .. } if durable > 0)
    }
}

pub fn read_availability(path: &Path) -> Option<AvailabilityProjection> {
    let bytes = xattr::get(path, AVAILABILITY_XATTR).ok()??;
    AvailabilityProjection::parse(&bytes)
}

pub fn read_content_hash(path: &Path) -> Option<[u8; 32]> {
    let bytes = xattr::get(path, CONTENT_HASH_XATTR).ok()??;
    bytes.as_slice().try_into().ok()
}

pub fn read_record_id(path: &Path) -> Option<[u8; 16]> {
    let bytes = xattr::get(path, RECORD_ID_XATTR).ok()??;
    bytes.as_slice().try_into().ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRecordIndex {
    pub records: Vec<WorkspaceRecord>,
    pub truncated: bool,
}

/// One bounded unit of work from a mounted collaborative record catalogue.
/// `reset` means the authority generation changed while this pager was alive;
/// consumers must discard previously indexed rows before accepting `records`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRecordIndexPage {
    pub records: Vec<WorkspaceRecord>,
    pub reset: bool,
    pub complete: bool,
    pub truncated: bool,
}

/// Retained continuation state for the mount's record-index protocol. A call
/// to [`Self::next_page`] reads at most one v2 page (or the bounded v1
/// compatibility projection), so opening or typing in global search never
/// synchronously materialises the complete workspace authority.
#[derive(Debug)]
pub struct WorkspaceRecordIndexPager {
    source_roots: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    root_index: usize,
    cursor: Option<RecordIndexCursor>,
    opaque_cursor: Option<(String, String, Option<collab_types::FileRecordId>)>,
    discovered: bool,
    reset_pending: bool,
    observed_generation: Option<String>,
    root_generations: std::collections::HashMap<PathBuf, String>,
    generation_root_cursor: usize,
    discovery_sources: std::collections::VecDeque<PathBuf>,
    /// A generated authority source keeps the multi-page paging contract
    /// covered without writing thousands of xattrs during the unit test.
    #[cfg(test)]
    test_v2: Option<TestV2Source>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct TestV2Source {
    root: PathBuf,
    revision: String,
    records: usize,
}

#[derive(Clone, Debug)]
struct RecordIndexCursor {
    revision: String,
    offset: usize,
    last_record_id: Option<collab_types::FileRecordId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub record_id: uuid::Uuid,
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexProjection {
    schema_version: u16,
    truncated: bool,
    records: Vec<RecordIndexEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexEntry {
    record_id: collab_types::FileRecordId,
    path: String,
    is_dir: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexManifestV2 {
    schema_version: u16,
    revision: String,
    records: usize,
    page_entries: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexPageV2 {
    schema_version: u16,
    revision: String,
    offset: usize,
    next_offset: Option<usize>,
    records: Vec<RecordIndexEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexManifestV3 {
    schema_version: u16,
    revision: String,
    initial_cursor: String,
    page_entries: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexPageV3 {
    schema_version: u16,
    revision: String,
    next_cursor: Option<String>,
    records: Vec<RecordIndexEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIndexExactV2 {
    schema_version: u16,
    revision: String,
    record: RecordIndexEntry,
}

fn safe_projected_path(root: &Path, projected: &str) -> Option<PathBuf> {
    use std::path::Component;
    let relative = projected.strip_prefix('/')?;
    let path = Path::new(relative);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(path))
}

fn read_record_index_projection(root: &Path) -> Option<RecordIndexProjection> {
    let bytes = xattr::get(root, RECORD_INDEX_XATTR).ok()??;
    parse_record_index_projection(root, &bytes)
}

fn valid_record_index_revision(revision: &str) -> bool {
    revision.len() == 64 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_record_index_manifest_v2(root: &Path) -> Option<RecordIndexManifestV2> {
    let bytes = xattr::get(root, RECORD_INDEX_V2_XATTR).ok()??;
    if bytes.len() > MAX_RECORD_INDEX_PAGE_BYTES {
        return None;
    }
    let manifest: RecordIndexManifestV2 = serde_json::from_slice(&bytes).ok()?;
    if manifest.schema_version != RECORD_INDEX_V2_VERSION
        || !valid_record_index_revision(&manifest.revision)
        || manifest.page_entries != RECORD_INDEX_PAGE_ENTRIES
    {
        return None;
    }
    Some(manifest)
}

fn read_record_index_page_v2(
    root: &Path,
    manifest: &RecordIndexManifestV2,
    offset: usize,
) -> Option<RecordIndexPageV2> {
    let name = format!("{RECORD_INDEX_V2_XATTR}.{}.{}", manifest.revision, offset);
    let bytes = xattr::get(root, &name).ok()??;
    if bytes.len() > MAX_RECORD_INDEX_PAGE_BYTES {
        return None;
    }
    let page: RecordIndexPageV2 = serde_json::from_slice(&bytes).ok()?;
    if page.schema_version != RECORD_INDEX_V2_VERSION
        || page.revision != manifest.revision
        || page.offset != offset
        || page.records.len() > manifest.page_entries
        || page
            .records
            .windows(2)
            .any(|pair| matches!(pair, [first, second] if first.record_id >= second.record_id))
    {
        return None;
    }
    Some(page)
}

fn read_record_index_manifest_v3(root: &Path) -> Option<RecordIndexManifestV3> {
    let bytes = xattr::get(root, RECORD_INDEX_V3_XATTR).ok()??;
    if bytes.len() > MAX_RECORD_INDEX_PAGE_BYTES {
        return None;
    }
    let value: RecordIndexManifestV3 = serde_json::from_slice(&bytes).ok()?;
    (value.schema_version == 3
        && valid_record_index_revision(&value.revision)
        && uuid::Uuid::parse_str(&value.initial_cursor).is_ok()
        && value.page_entries == RECORD_INDEX_PAGE_ENTRIES)
        .then_some(value)
}

fn read_record_index_page_v3(
    root: &Path,
    manifest: &RecordIndexManifestV3,
    cursor: &str,
) -> Option<RecordIndexPageV3> {
    let name = format!("{RECORD_INDEX_V3_XATTR}.{}.{}", manifest.revision, cursor);
    let bytes = xattr::get(root, name).ok()??;
    if bytes.len() > MAX_RECORD_INDEX_PAGE_BYTES {
        return None;
    }
    let value: RecordIndexPageV3 = serde_json::from_slice(&bytes).ok()?;
    (value.schema_version == 3
        && value.revision == manifest.revision
        && value.records.len() <= manifest.page_entries
        && value
            .next_cursor
            .as_ref()
            .is_none_or(|value| uuid::Uuid::parse_str(value).is_ok())
        && value
            .records
            .windows(2)
            .all(|pair| matches!(pair, [first, second] if first.record_id < second.record_id)))
    .then_some(value)
}

/// Resolve one known stable record through the mount authority. This is a
/// single bounded xattr request, including for a record beyond page 100,000.
pub fn resolve_workspace_record(
    roots: &[PathBuf],
    record_id: uuid::Uuid,
) -> Option<WorkspaceRecord> {
    for root in roots {
        let name = format!("{RECORD_INDEX_BY_ID_XATTR_PREFIX}{record_id}");
        let Some(bytes) = xattr::get(root, name).ok().flatten() else {
            continue;
        };
        if bytes.len() > MAX_RECORD_INDEX_PAGE_BYTES {
            continue;
        }
        let Ok(projection) = serde_json::from_slice::<RecordIndexExactV2>(&bytes) else {
            continue;
        };
        if projection.schema_version != RECORD_INDEX_V2_VERSION
            || !valid_record_index_revision(&projection.revision)
            || projection.record.record_id.0 != record_id
        {
            continue;
        }
        let (mut records, malformed) = project_record_entries(root, vec![projection.record]);
        if !malformed && let Some(record) = records.pop() {
            return Some(record);
        }
    }
    None
}

fn has_record_index(root: &Path) -> bool {
    if read_record_index_manifest_v3(root).is_some() {
        return true;
    }
    match xattr::get(root, RECORD_INDEX_V2_XATTR).ok().flatten() {
        Some(bytes) if bytes.len() <= MAX_RECORD_INDEX_PAGE_BYTES => {
            read_record_index_manifest_v2(root).is_some()
        }
        Some(_) => false,
        None => read_record_index_projection(root).is_some(),
    }
}

fn root_generation(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.files.record-index-root-generation.v2\\0");
    let root_bytes = root.as_os_str().as_encoded_bytes();
    hasher.update((root_bytes.len() as u64).to_be_bytes());
    hasher.update(root_bytes);
    // ACL mounts intentionally expose a stable opaque manifest. Do not hash
    // directory mtime there: it would turn hidden record edits into a visible
    // global-search reset signal.
    if let Some(bytes) = xattr::get(root, RECORD_INDEX_V3_XATTR).ok().flatten() {
        hasher.update(b"opaque-v3\\0");
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
        return hex::encode(hasher.finalize());
    }
    match std::fs::metadata(root) {
        Ok(metadata) => {
            hasher.update(b"metadata\\0");
            hasher.update(metadata.len().to_be_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(duration) = modified.duration_since(ui::time::UNIX_EPOCH)
            {
                hasher.update(duration.as_secs().to_be_bytes());
                hasher.update(duration.subsec_nanos().to_be_bytes());
            }
        }
        Err(_) => hasher.update(b"missing\\0"),
    }
    for name in [RECORD_INDEX_V2_XATTR, RECORD_INDEX_XATTR] {
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        if let Some(bytes) = xattr::get(root, name).ok().flatten() {
            hasher.update((bytes.len() as u64).to_be_bytes());
            hasher.update(&bytes);
        }
    }
    hex::encode(hasher.finalize())
}

fn parse_record_index_projection(root: &Path, bytes: &[u8]) -> Option<RecordIndexProjection> {
    if bytes.len() > MAX_RECORD_INDEX_XATTR_BYTES {
        return None;
    }
    let projection: RecordIndexProjection = serde_json::from_slice(bytes).ok()?;
    if projection.schema_version != 1 || projection.records.len() > MAX_RECORD_INDEX_ENTRIES {
        return None;
    }
    let mut previous = None;
    for entry in &projection.records {
        if previous.is_some_and(|value| value >= entry.record_id)
            || safe_projected_path(root, &entry.path).is_none()
        {
            return None;
        }
        previous = Some(entry.record_id);
    }
    Some(projection)
}

fn project_record_entries(
    root: &Path,
    entries: Vec<RecordIndexEntry>,
) -> (Vec<WorkspaceRecord>, bool) {
    let mut truncated = false;
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(path) = safe_projected_path(root, &entry.path) else {
            truncated = true;
            continue;
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into(),
        );
        records.push(WorkspaceRecord {
            record_id: entry.record_id.0,
            name,
            path,
            is_dir: entry.is_dir,
        });
    }
    records.sort_by_key(|record| record.record_id);
    records.dedup_by_key(|record| record.record_id);
    (records, truncated)
}

impl WorkspaceRecordIndexPager {
    pub fn new(roots: &[PathBuf]) -> Self {
        Self {
            source_roots: roots.to_vec(),
            roots: Vec::new(),
            root_index: 0,
            cursor: None,
            opaque_cursor: None,
            discovered: false,
            reset_pending: false,
            observed_generation: None,
            root_generations: std::collections::HashMap::new(),
            generation_root_cursor: 0,
            discovery_sources: roots.iter().cloned().collect(),
            #[cfg(test)]
            test_v2: None,
        }
    }

    #[cfg(test)]
    fn from_generated_v2(root: PathBuf, records: usize) -> Self {
        let revision = "a".repeat(64);
        Self {
            source_roots: vec![root.clone()],
            roots: vec![root.clone()],
            root_index: 0,
            cursor: None,
            opaque_cursor: None,
            discovered: true,
            reset_pending: false,
            observed_generation: Some(revision.clone()),
            root_generations: std::collections::HashMap::new(),
            generation_root_cursor: 0,
            discovery_sources: std::collections::VecDeque::new(),
            test_v2: Some(TestV2Source {
                root,
                revision,
                records,
            }),
        }
    }

    #[cfg_attr(
        not(test),
        allow(
            clippy::unused_self,
            reason = "the receiver is used by the test-only synthetic authority seam"
        )
    )]
    fn v2_manifest(&self, root: &Path) -> Option<RecordIndexManifestV2> {
        #[cfg(test)]
        if let Some(source) = &self.test_v2
            && source.root == root
        {
            return Some(RecordIndexManifestV2 {
                schema_version: RECORD_INDEX_V2_VERSION,
                revision: source.revision.clone(),
                records: source.records,
                page_entries: RECORD_INDEX_PAGE_ENTRIES,
            });
        }
        read_record_index_manifest_v2(root)
    }

    #[cfg_attr(
        not(test),
        allow(
            clippy::unused_self,
            reason = "the receiver is used by the test-only synthetic authority seam"
        )
    )]
    fn v2_page(
        &self,
        root: &Path,
        manifest: &RecordIndexManifestV2,
        offset: usize,
    ) -> Option<RecordIndexPageV2> {
        #[cfg(test)]
        if let Some(source) = &self.test_v2
            && source.root == root
            && source.revision == manifest.revision
        {
            let end = offset
                .checked_add(RECORD_INDEX_PAGE_ENTRIES)?
                .min(source.records);
            let records = ((offset + 1)..=end)
                .map(|ordinal| RecordIndexEntry {
                    record_id: collab_types::FileRecordId(uuid::Uuid::from_u128(ordinal as u128)),
                    path: if ordinal == source.records {
                        "/late-target.txt".into()
                    } else {
                        "/indexed.txt".into()
                    },
                    is_dir: false,
                })
                .collect();
            return Some(RecordIndexPageV2 {
                schema_version: RECORD_INDEX_V2_VERSION,
                revision: source.revision.clone(),
                offset,
                next_offset: (end < source.records).then_some(end),
                records,
            });
        }
        read_record_index_page_v2(root, manifest, offset)
    }

    fn discover_step(&mut self) -> bool {
        // Each configured workspace root is an authority mount. We do not
        // infer child mounts by directory enumeration: doing so makes an exact
        // record UUID impossible to reopen without a hidden tail scan. Shared
        // mounts must therefore be listed directly in `TILO_WORKSPACE_ROOTS`.
        while let Some(root) = self.discovery_sources.pop_front() {
            if has_record_index(&root) {
                self.roots.push(root);
            }
        }
        self.discovered = true;
        true
    }

    /// Whether idle work still has an authority continuation to consume.
    pub fn has_more(&self) -> bool {
        !self.discovered || self.root_index < self.roots.len()
    }

    /// Cheap, periodic manifest/root-set poll. It never reads record pages;
    /// a changed or vanished authority withdraws the retained index before a
    /// fresh bounded page is admitted on the next idle turn.
    pub fn poll_generation(&mut self) -> bool {
        const ROOT_WATCH_BUDGET: usize = 8;
        let mut roots = self.source_roots.clone();
        roots.extend(self.roots.iter().cloned());
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            return false;
        }
        let mut changed = false;
        for _ in 0..ROOT_WATCH_BUDGET.min(roots.len()) {
            let index = self.generation_root_cursor % roots.len();
            self.generation_root_cursor = self.generation_root_cursor.wrapping_add(1);
            // `index` is modulo `roots.len()` above; preserve that invariant
            // without an unchecked slice access in this refresh path.
            let Some(root) = roots.get(index) else {
                return false;
            };
            let generation = root_generation(root);
            let root_changed = self
                .root_generations
                .insert(root.clone(), generation.clone())
                .is_some_and(|previous| previous != generation);
            changed |= root_changed;
        }
        self.observed_generation = Some(String::from("watching"));
        if changed {
            self.roots.clear();
            self.root_index = 0;
            self.cursor = None;
            self.opaque_cursor = None;
            self.discovered = false;
            self.discovery_sources = self.source_roots.iter().cloned().collect();
            self.generation_root_cursor = 0;
            self.reset_pending = true;
        }
        changed
    }

    /// Fetch one authority page. A changed manifest generation restarts from
    /// its first page and marks `reset`, preventing mixed old/new paths.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded authority-page state machine keeps generation, cursor, and fail-closed validation together"
    )]
    pub fn next_page(&mut self) -> WorkspaceRecordIndexPage {
        // Capture the current authority before admitting the first page. This
        // prevents a mutation between startup and the first periodic poll from
        // becoming a stale-row baseline.
        if self.observed_generation.is_none() {
            let _ = self.poll_generation();
        }
        let mut reset = std::mem::take(&mut self.reset_pending);
        let mut truncated = false;
        if !self.discovered && !self.discover_step() {
            return WorkspaceRecordIndexPage {
                records: Vec::new(),
                reset,
                complete: false,
                truncated,
            };
        }
        loop {
            let Some(root) = self.roots.get(self.root_index).cloned() else {
                return WorkspaceRecordIndexPage {
                    records: Vec::new(),
                    reset,
                    complete: true,
                    truncated,
                };
            };
            if let Some(manifest) = read_record_index_manifest_v3(&root) {
                let cursor = match &self.opaque_cursor {
                    Some((revision, cursor, _)) if revision == &manifest.revision => cursor.clone(),
                    Some(_) => {
                        self.root_index = 0;
                        self.cursor = None;
                        self.opaque_cursor = None;
                        reset = true;
                        continue;
                    }
                    None => manifest.initial_cursor.clone(),
                };
                let Some(page) = read_record_index_page_v3(&root, &manifest, &cursor) else {
                    self.root_index += 1;
                    self.opaque_cursor = None;
                    return WorkspaceRecordIndexPage {
                        records: Vec::new(),
                        reset,
                        complete: false,
                        truncated: true,
                    };
                };
                let prior_record_id =
                    self.opaque_cursor
                        .as_ref()
                        .and_then(|(revision, _, previous)| {
                            (revision == &manifest.revision)
                                .then_some(*previous)
                                .flatten()
                        });
                if prior_record_id.is_some_and(|previous| {
                    page.records
                        .first()
                        .is_some_and(|record| record.record_id <= previous)
                }) {
                    self.root_index += 1;
                    self.opaque_cursor = None;
                    return WorkspaceRecordIndexPage {
                        records: Vec::new(),
                        reset,
                        complete: false,
                        truncated: true,
                    };
                }
                let last_record_id = page.records.last().map(|record| record.record_id);
                let (records, malformed) = project_record_entries(&root, page.records);
                truncated |= malformed;
                if let Some(next) = page.next_cursor {
                    self.opaque_cursor = Some((manifest.revision, next, last_record_id));
                } else {
                    self.root_index += 1;
                    self.opaque_cursor = None;
                }
                return WorkspaceRecordIndexPage {
                    records,
                    reset,
                    complete: self.root_index == self.roots.len(),
                    truncated,
                };
            }
            let manifest = self.v2_manifest(&root);
            let v2_present = manifest.is_some()
                || xattr::get(&root, RECORD_INDEX_V2_XATTR)
                    .ok()
                    .flatten()
                    .is_some();
            match manifest {
                Some(manifest) => {
                    let (offset, prior_record_id) = match &self.cursor {
                        Some(cursor) if cursor.revision == manifest.revision => {
                            (cursor.offset, cursor.last_record_id)
                        }
                        Some(_) => {
                            // The mount invalidated the cursor. Do not keep old
                            // rows under the new authority generation.
                            self.root_index = 0;
                            self.cursor = None;
                            reset = true;
                            continue;
                        }
                        None => {
                            self.cursor = Some(RecordIndexCursor {
                                revision: manifest.revision.clone(),
                                offset: 0,
                                last_record_id: None,
                            });
                            (0, None)
                        }
                    };
                    if manifest.records == 0 {
                        self.root_index += 1;
                        self.cursor = None;
                        continue;
                    }
                    let Some(page) = self.v2_page(&root, &manifest, offset) else {
                        self.root_index += 1;
                        self.cursor = None;
                        return WorkspaceRecordIndexPage {
                            records: Vec::new(),
                            reset,
                            complete: false,
                            truncated: true,
                        };
                    };
                    let next = page.next_offset;
                    if prior_record_id.is_some_and(|prior| {
                        page.records
                            .first()
                            .is_some_and(|record| record.record_id <= prior)
                    }) {
                        self.root_index += 1;
                        self.cursor = None;
                        return WorkspaceRecordIndexPage {
                            records: Vec::new(),
                            reset,
                            complete: false,
                            truncated: true,
                        };
                    }
                    let last_record_id = page.records.last().map(|record| record.record_id);
                    // A page's continuation advances over the namespace-owned
                    // source window, not merely returned rows: an ACL can
                    // deny every row in a bounded window. Empty pages are
                    // therefore valid and never trigger an unbounded hunt for
                    // the next visible record.
                    let terminal = next.is_none();
                    let valid_next = terminal && offset < manifest.records
                        || matches!(next, Some(next) if next > offset && next <= manifest.records && next % manifest.page_entries == 0);
                    if !valid_next {
                        self.root_index += 1;
                        self.cursor = None;
                        return WorkspaceRecordIndexPage {
                            records: Vec::new(),
                            reset,
                            complete: false,
                            truncated: true,
                        };
                    }
                    let (records, malformed) = project_record_entries(&root, page.records);
                    truncated |= malformed;
                    if let Some(next) = next {
                        self.cursor = Some(RecordIndexCursor {
                            revision: manifest.revision,
                            offset: next,
                            last_record_id,
                        });
                    } else {
                        self.root_index += 1;
                        self.cursor = None;
                    }
                    return WorkspaceRecordIndexPage {
                        records,
                        reset,
                        complete: self.root_index == self.roots.len(),
                        truncated,
                    };
                }
                None if v2_present => {
                    self.root_index += 1;
                    self.cursor = None;
                    return WorkspaceRecordIndexPage {
                        records: Vec::new(),
                        reset,
                        complete: false,
                        truncated: true,
                    };
                }
                None => {
                    let Some(projection) = read_record_index_projection(&root) else {
                        self.root_index += 1;
                        return WorkspaceRecordIndexPage {
                            records: Vec::new(),
                            reset,
                            complete: false,
                            truncated: true,
                        };
                    };
                    self.root_index += 1;
                    let (records, malformed) = project_record_entries(&root, projection.records);
                    return WorkspaceRecordIndexPage {
                        records,
                        reset,
                        complete: self.root_index == self.roots.len(),
                        truncated: truncated || projection.truncated || malformed,
                    };
                }
            }
        }
    }
}

/// Compatibility helper for callers that only need the first bounded unit.
/// New retained search code owns a [`WorkspaceRecordIndexPager`] and keeps
/// advancing it as the user refreshes or opens global search.
pub fn read_workspace_record_index(roots: &[PathBuf]) -> WorkspaceRecordIndex {
    let mut pager = WorkspaceRecordIndexPager::new(roots);
    let page = pager.next_page();
    WorkspaceRecordIndex {
        records: page.records,
        truncated: page.truncated || !page.complete,
    }
}

/// Read a host-provided thumbnail (raw PNG bytes) for `path` from the
/// [`THUMB_XATTR`] extended attribute. `None` when the driver has no preview for
/// it (a plain local file off any workspace mount, a non-previewable type, or
/// content not yet local on the host). A single `getxattr`, exactly the metadata
/// channel the FUSE driver exposes — the file manager never re-decodes a file
/// the host already previewed.
pub fn read_thumb_xattr(path: &Path) -> Option<Vec<u8>> {
    match xattr::get(path, THUMB_XATTR) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// The Tilo workspace roots the FUSE driver mounts (first-class sidebar
/// locations, and the dirs whose entries carry sync state). Normally just
/// [`WORKSPACE_ROOT`]; `TILO_WORKSPACE_ROOTS` (a native path list) overrides it for
/// tests and alternate mounts. This navigable list includes the user's Shared
/// place even when received shares are separate authority mounts.
pub fn workspace_roots() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("TILO_WORKSPACE_ROOTS") {
        return parse_workspace_roots(&v);
    }
    let p = PathBuf::from(WORKSPACE_ROOT);
    let mut roots = if p.exists() { vec![p] } else { Vec::new() };
    if let Some(received) = user_home().map(|home| home.join("Shared"))
        && received.exists()
        && !roots.contains(&received)
    {
        roots.push(received);
    }
    roots
}

/// Direct mounts permitted to publish record-index resources and satisfy exact
/// record opens. Received-share discovery requires a dedicated subscription
/// index and is intentionally not inferred by walking `~/Shared`.
pub fn workspace_record_authority_roots() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os("TILO_WORKSPACE_ROOTS") {
        return parse_workspace_roots(&value);
    }
    let root = PathBuf::from(WORKSPACE_ROOT);
    root.exists().then_some(root).into_iter().collect()
}

/// Parse a `TILO_WORKSPACE_ROOTS` value (colon-separated on Unix, semicolon-
/// separated on Windows) into the roots that
/// currently exist. Split out so the parsing is unit-testable without touching
/// the process environment.
fn parse_workspace_roots(val: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(val)
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|p| p.exists())
        .collect()
}

/// Whether `path` sits inside a Tilo workspace root (so its entries carry sync
/// state and should be queried for the [`SYNC_XATTR`]).
pub fn is_workspace_path(path: &Path) -> bool {
    #[cfg(test)]
    fixture_io_guard::ambient_io("model::is_workspace_path");
    path_under_any(path, &workspace_roots())
}

/// Whether `path` is one of `roots` or nested under one. Pure over an explicit
/// root list so the containment logic is testable without the environment.
fn path_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| path == r || path.starts_with(r))
}

/// Which of `roots` contains `path`, if any - the longest match, so a
/// workspace mounted inside another resolves to the inner one.
///
/// [`is_workspace_path`] answers whether a path is in a workspace at all,
/// which is what a sync-state query needs. Naming the workspace needs the
/// root itself, and that is a different question: `/var/workspace` *is*
/// the workspace, while `/var/workspace/notes` is a folder in it. Pure
/// over an explicit root list for the same reason `path_under_any` is.
pub fn workspace_root_for<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .filter(|r| path == r.as_path() || path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
        .map(std::path::PathBuf::as_path)
}

/// Discover the owner-only response-bearing share service from the workspace
/// root. The xattr carries a socket locator, never a link secret.
pub fn share_control_socket(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let root = workspace_root_for(path, roots)?;
    let bytes = xattr::get(root, SHARE_CONTROL_XATTR).ok()??;
    if bytes.is_empty() || bytes.len() > 4096 {
        return None;
    }
    let value = std::str::from_utf8(&bytes).ok()?;
    let socket = PathBuf::from(value);
    socket.is_absolute().then_some(socket)
}

/// What to call a workspace root on screen.
///
/// The mount path is an implementation detail of where the driver put it,
/// and a misleading one: the demo mounts under `/var/tmp`, and a trail
/// that spells it out tells a reader they are looking at scratch space on
/// somebody's laptop - the opposite of a folder that belongs to the room.
/// The mount's own final component is the name the workspace was given.
///
/// One function because the places sidebar and the breadcrumb name the
/// same location, and somebody who clicks the one to reach the other must
/// not find it has been renamed on the way.
pub fn workspace_label(root: &Path) -> String {
    root.file_name().map_or_else(
        || crate::i18n::t("files-place-workspace"),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub mime: String,
    /// The Tilo FUSE sync state, when this entry is on a workspace mount.
    pub sync_state: Option<SyncState>,
    /// Stable content identity supplied by the collaborative mount. Ordinary
    /// local files have `None` and retain the path/mtime/size fallback key.
    pub content_hash: Option<[u8; 32]>,
    /// Stable collaborative record identity; survives rename and move.
    pub record_id: Option<[u8; 16]>,
    /// Rich, honest availability state. `None` falls back to legacy sync state.
    pub availability: Option<AvailabilityProjection>,
    /// Redacted authority-backed recipients and bearer-link state.
    pub share: Option<ShareProjection>,
    /// Recent immutable versions and first-class conflicts.
    pub history: Option<HistoryProjection>,
    /// Useful media/document facts from signed metadata; no payload read.
    pub rich_metadata: Option<MetadataProjection>,
}

impl Entry {
    /// First-class conflict state projected by stable record identity. A
    /// filename never creates or clears this badge.
    pub fn is_conflict(&self) -> bool {
        self.history
            .as_ref()
            .is_some_and(|history| history.open_conflicts().next().is_some())
    }

    /// Whether this entry's **content** is already on this machine, so
    /// reading it costs a read and nothing else.
    ///
    /// [`SyncState::Projected`] and [`SyncState::Downloading`] say the bytes
    /// are not here: opening such a file hydrates it from a peer, which is
    /// work nobody asked for. Everything else is local — a file the driver
    /// reports as [`SyncState::Local`], and equally a file carrying no sync
    /// attribute at all, which is what an ordinary path (or a workspace root
    /// that is a plain directory rather than a live FUSE mount) looks like.
    pub fn content_is_local(&self) -> bool {
        if let Some(availability) = &self.availability {
            return matches!(availability.residency, Residency::Resident { .. });
        }
        !matches!(
            self.sync_state,
            Some(SyncState::Projected | SyncState::Downloading)
        )
    }
}

/// Human-readable byte size (e.g. `4.0 kB`). Directories show "—".
pub fn human_size(entry: &Entry) -> String {
    if entry.is_dir {
        "—".to_string()
    } else {
        crate::i18n::format_bytes(entry.size)
    }
}

/// Locale-sensitive short modified timestamp, or "—" when unknown.
pub fn format_modified(modified: Option<DateTime<Local>>) -> String {
    match modified {
        Some(t) => crate::i18n::format_datetime(t),
        None => "—".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

pub fn sort_entries(sort_by: SortBy, sort_order: SortOrder, entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let cmp = match sort_by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Type => a.mime.cmp(&b.mime),
        };
        let final_cmp = if sort_order == SortOrder::Descending {
            cmp.reverse()
        } else {
            cmp
        };
        // Directories always first, regardless of order direction.
        b.is_dir.cmp(&a.is_dir).then(final_cmp)
    });
}

/// # Errors
///
/// Returns an error if `path` cannot be read as a directory (it does not
/// exist, is not a directory, or is not readable).
pub fn read_dir_sync(path: &Path) -> Result<Vec<Entry>, String> {
    // Only query the sync xattr when listing a workspace mount — an ordinary
    // directory has no such attribute, so skipping the per-entry `getxattr`
    // there keeps a plain home-folder listing at one syscall per file.
    read_dir_inner(path, is_workspace_path(path))
}

/// List real entries with classification derived solely from supplied roots.
pub(crate) fn read_dir_with_workspace_roots(
    path: &Path,
    roots: &[PathBuf],
) -> Result<Vec<Entry>, String> {
    read_dir_inner(path, path_under_any(path, roots))
}

/// The listing body, with the workspace-gating decision passed in (so tests can
/// exercise the sync-state population path without mutating the global
/// `TILO_WORKSPACE_ROOTS` env). Standalone callers use [`read_dir_sync`];
/// injected views supply workspace roots explicitly.
fn read_dir_inner(path: &Path, in_workspace: bool) -> Result<Vec<Entry>, String> {
    #[cfg(test)]
    fixture_io_guard::observe_owned_listing(path, in_workspace);
    let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let meta = entry.metadata().ok();
        let path = entry.path();
        let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        // Content hydration state only applies to files — a directory's structure
        // is always fully projected — so we don't query (or badge) folders.
        let sync_state = if in_workspace && !is_dir {
            read_sync_state(&path)
        } else {
            None
        };
        let content_hash = if in_workspace && !is_dir {
            read_content_hash(&path)
        } else {
            None
        };
        let record_id = if in_workspace {
            read_record_id(&path)
        } else {
            None
        };
        let availability = if in_workspace {
            read_availability(&path)
        } else {
            None
        };
        let share = if in_workspace {
            read_share(&path)
        } else {
            None
        };
        let history = if in_workspace {
            read_history(&path)
        } else {
            None
        };
        let rich_metadata = if in_workspace && !is_dir {
            read_metadata(&path)
        } else {
            None
        };
        out.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir,
            size: meta.as_ref().map_or(0, std::fs::Metadata::len),
            modified: meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .map(DateTime::from),
            mime,
            sync_state,
            content_hash,
            record_id,
            availability,
            share,
            history,
            rich_metadata,
        });
    }
    Ok(out)
}

pub fn format_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rel) = path.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub fn initial_path() -> PathBuf {
    // Prefer a configured collaborative workspace mount. An explicit empty
    // override means there is no workspace: test/alternate environments often
    // have an unrelated `/workspace` (for example, the CI checkout mount), and
    // must be able to keep Files from mistaking it for user content.
    if let Some(root) = workspace_roots().into_iter().next() {
        return root;
    }
    // `/workspace` predates `TILO_WORKSPACE_ROOTS`; retain it only when no
    // override was supplied, so old installations keep their start location.
    if std::env::var_os("TILO_WORKSPACE_ROOTS").is_none() {
        let legacy = PathBuf::from("/workspace");
        if legacy.exists() {
            return legacy;
        }
    }
    if let Some(home) = user_home() {
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// The user's home, which under a sandbox is *not* this process's `HOME`
/// — that is the app's own per-package directory, and opening a file
/// manager on it shows a folder with none of the user's files in it.
/// `TILO_USER_HOME` is present only when a grant actually bound the real
/// home, so preferring it can never point at a directory this process
/// cannot read.
///
/// Every surface that needs to know where home is resolves it here, so
/// the directory the window opens on and the home the breadcrumb
/// abbreviates against are the same answer. When they were two answers
/// the trail rendered as an absolute path from the filesystem root: the
/// window was on the real home and `format_path` was measuring against
/// `/run/tilo/app`, which nothing is under.
pub fn user_home() -> Option<PathBuf> {
    ui::sysdirs::user_home_override().or_else(tilo_paths::home)
}

#[cfg(test)]
mod sync_state_tests {
    use super::*;

    /// Metadata, xattr state, availability and share/history projections are bounded, validated and keep unknown distinct.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn xattr_and_availability_projections_are_bounded() {
        // --- phase: metadata_projection_is_bounded_and_validated ---
        {
            let projection = MetadataProjection {
                schema_version: 1,
                record_id: collab_types::FileRecordId(uuid::Uuid::from_u128(7)),
                properties: collab_types::FilePropertiesV1 {
                    schema_version: collab_types::files::FILE_PROPERTIES_VERSION,
                    image_width: Some(800),
                    image_height: Some(600),
                    duration_ms: None,
                    page_count: None,
                    codec: Some("png".into()),
                    container: Some("png".into()),
                },
            };
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert_eq!(MetadataProjection::parse(&bytes), Some(projection));
            assert!(MetadataProjection::parse(&vec![b' '; MAX_METADATA_XATTR_BYTES + 1]).is_none());
        }

        // --- phase: from_xattr_parses_the_three_states_and_rejects_junk ---
        {
            assert_eq!(SyncState::from_xattr(b"local"), Some(SyncState::Local));
            assert_eq!(
                SyncState::from_xattr(b"projected"),
                Some(SyncState::Projected)
            );
            assert_eq!(
                SyncState::from_xattr(b"downloading"),
                Some(SyncState::Downloading)
            );
            // Trailing whitespace/newline (some xattr tools append one) is tolerated.
            assert_eq!(SyncState::from_xattr(b"local\n"), Some(SyncState::Local));
            assert_eq!(SyncState::from_xattr(b"bogus"), None);
            assert_eq!(SyncState::from_xattr(&[0xff, 0xfe]), None);
        }

        // --- phase: availability_parser_keeps_unknown_distinct_from_known_zero_and_is_bounded ---
        {
            let unknown = AvailabilityProjection::parse(&availability_json(
                &serde_json::json!({"state": "unknown"}),
            ))
            .unwrap();
            assert_eq!(unknown.replicas, ReplicaEvidence::Unknown);
            assert!(!unknown.safely_replicated());

            let zero = AvailabilityProjection::parse(&availability_json(
                &serde_json::json!({"state": "known", "reachable": 0, "durable": 0}),
            ))
            .unwrap();
            assert_eq!(
                zero.replicas,
                ReplicaEvidence::Known {
                    reachable: 0,
                    durable: 0
                }
            );
            assert!(!zero.safely_replicated());
            assert!(
                AvailabilityProjection::parse(&vec![b' '; MAX_AVAILABILITY_XATTR_BYTES + 1])
                    .is_none()
            );
        }

        // --- phase: share_and_history_projections_are_bounded_and_actionable ---
        {
            let share = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": [{
                    "grant_id": "c0d4e23f-c7cb-4ded-a427-b32cf9da9343",
                    "subject": {"kind": "person", "person": vec![1; 32]},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": null
                }]
            }))
            .unwrap();
            let share = ShareProjection::parse(&share).unwrap();
            assert_eq!(share.active_grants(), 1);

            let history = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 1,
                "versions_truncated": false,
                "heads": ["a0d4e23f-c7cb-4ded-a427-b32cf9da9342"],
                "versions": [{
                    "id": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": null,
                    "metadata_hash": vec![3; 32],
                    "provenance": "Create"
                }],
                "conflicts_truncated": false,
                "conflicts": []
            }))
            .unwrap();
            let history = HistoryProjection::parse(&history).unwrap();
            assert_eq!(
                crate::i18n::t(history.versions[0].provenance_key()),
                "Created"
            );
            assert!(ShareProjection::parse(&vec![b' '; MAX_COLLAB_PROJECTION_BYTES + 1]).is_none());
            assert!(
                HistoryProjection::parse(&vec![b' '; MAX_COLLAB_PROJECTION_BYTES + 1]).is_none()
            );
        }
    }

    /// Real sync-state and thumbnail xattrs round-trip and hashes and record ids require their exact raw widths.
    #[test]
    #[cfg(unix)]
    fn real_xattrs_round_trip_with_exact_widths() {
        // --- phase: read_sync_state_round_trips_a_real_xattr ---
        {
            // A real getxattr against a real `user.tilo.sync` attribute — the exact
            // syscall the app makes against the FUSE mount, exercised here on a
            // tmpfile so it doesn't need a live driver.
            let d = tempfile::tempdir().unwrap();
            let f = d.path().join("projected.txt");
            std::fs::write(&f, b"x").unwrap();
            // No attribute yet → None.
            assert_eq!(read_sync_state(&f), None, "absent attribute reads as None");
            // Set it the way the driver would and read it back.
            xattr::set(&f, SYNC_XATTR, b"projected").unwrap();
            assert_eq!(read_sync_state(&f), Some(SyncState::Projected));
            xattr::set(&f, SYNC_XATTR, b"local").unwrap();
            assert_eq!(read_sync_state(&f), Some(SyncState::Local));
        }

        // --- phase: read_thumb_xattr_round_trips_a_real_host_preview ---
        {
            // The FS-metadata channel: a host writes a preview into `user.tilo.thumb`
            // and the file manager reads the exact bytes back — the same `getxattr`
            // the driver answers. Exercised on a tmpfile so it needs no live driver.
            let d = tempfile::tempdir().unwrap();
            let f = d.path().join("photo.png");
            std::fs::write(&f, b"content").unwrap();
            assert_eq!(read_thumb_xattr(&f), None, "absent preview reads as None");
            let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
            xattr::set(&f, THUMB_XATTR, &png).unwrap();
            assert_eq!(
                read_thumb_xattr(&f),
                Some(png),
                "the host preview bytes come back verbatim"
            );
        }

        // --- phase: read_content_hash_requires_exactly_the_raw_blake3_width ---
        {
            let d = tempfile::tempdir().unwrap();
            let f = d.path().join("photo.png");
            std::fs::write(&f, b"content").unwrap();
            assert_eq!(read_content_hash(&f), None);

            let hash = [0x5au8; 32];
            xattr::set(&f, CONTENT_HASH_XATTR, &hash).unwrap();
            assert_eq!(read_content_hash(&f), Some(hash));

            xattr::set(&f, CONTENT_HASH_XATTR, &[1u8; 31]).unwrap();
            assert_eq!(
                read_content_hash(&f),
                None,
                "a malformed metadata value cannot become a cache identity"
            );
        }

        // --- phase: read_record_id_requires_exactly_the_raw_uuid_width ---
        {
            let d = tempfile::tempdir().unwrap();
            let f = d.path().join("shared.txt");
            std::fs::write(&f, b"content").unwrap();
            assert_eq!(read_record_id(&f), None);
            let id = [0x7bu8; 16];
            xattr::set(&f, RECORD_ID_XATTR, &id).unwrap();
            assert_eq!(read_record_id(&f), Some(id));
            xattr::set(&f, RECORD_ID_XATTR, &[0u8; 15]).unwrap();
            assert_eq!(read_record_id(&f), None);
        }
    }

    fn availability_json(replicas: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
            "residency": {"state": "resident", "bytes": 42},
            "transfer": {
                "phase": "idle", "progress": null, "last_error": null,
                "last_error_at_ms": null, "error_truncated": false
            },
            "policy": "automatic",
            "replicas": replicas,
            "freshness": {
                "metadata_observed_at_ms": 1,
                "local_verified_at_ms": 2,
                "replicas_observed_at_ms": 3
            }
        }))
        .unwrap()
    }

    /// Sync state appears only inside workspace roots, the share locator is read-only, and the record index projects stable identities and rejects escapes.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    #[cfg(unix)]
    fn workspace_records_project_by_stable_identity() {
        // --- phase: read_dir_populates_sync_state_only_inside_a_workspace_root ---
        {
            // Tag two files with real sync xattrs, then confirm the real listing
            // path surfaces them on `Entry` when (and only when) the directory is a
            // workspace mount — driven through `read_dir_inner`, the exact function
            // `read_dir_sync` delegates to.
            let ws = tempfile::tempdir().unwrap();
            let downloading = ws.path().join("a-downloading.bin");
            let projected = ws.path().join("b-projected.bin");
            let untagged = ws.path().join("c-plain.bin");
            std::fs::write(&downloading, b"x").unwrap();
            std::fs::write(&projected, b"x").unwrap();
            std::fs::write(&untagged, b"x").unwrap();
            xattr::set(&downloading, SYNC_XATTR, b"downloading").unwrap();
            xattr::set(&projected, SYNC_XATTR, b"projected").unwrap();
            let projected_hash = [0x33u8; 32];
            xattr::set(&projected, CONTENT_HASH_XATTR, &projected_hash).unwrap();
            let projected_record = [0x44u8; 16];
            xattr::set(&projected, RECORD_ID_XATTR, &projected_record).unwrap();
            let projected_availability =
                availability_json(&serde_json::json!({"state": "unknown"}));
            xattr::set(&projected, AVAILABILITY_XATTR, &projected_availability).unwrap();
            let projected_share = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "observed_at_ms": 10,
                "grants": []
            }))
            .unwrap();
            xattr::set(&projected, SHARE_XATTR, &projected_share).unwrap();

            // Not a workspace: sync_state is never queried → all None.
            let outside = read_dir_inner(ws.path(), false).unwrap();
            assert!(
                outside.iter().all(|e| e.sync_state.is_none()),
                "outside a workspace root, no entry carries sync state",
            );

            // Declared a workspace: real xattrs are surfaced on the entries.
            let entries = read_dir_inner(ws.path(), true).unwrap();
            let by = |name: &str| entries.iter().find(|e| e.name == name).unwrap().sync_state;
            assert_eq!(by("a-downloading.bin"), Some(SyncState::Downloading));
            assert_eq!(by("b-projected.bin"), Some(SyncState::Projected));
            assert_eq!(
                entries
                    .iter()
                    .find(|e| e.name == "b-projected.bin")
                    .unwrap()
                    .content_hash,
                Some(projected_hash),
                "content identity projects independently of the bytes"
            );
            assert_eq!(
                entries
                    .iter()
                    .find(|e| e.name == "b-projected.bin")
                    .unwrap()
                    .record_id,
                Some(projected_record),
                "record identity projects independently of path and bytes"
            );
            assert_eq!(
                entries
                    .iter()
                    .find(|e| e.name == "b-projected.bin")
                    .unwrap()
                    .availability
                    .as_ref()
                    .map(|a| a.replicas),
                Some(ReplicaEvidence::Unknown)
            );
            assert_eq!(
                entries
                    .iter()
                    .find(|e| e.name == "b-projected.bin")
                    .unwrap()
                    .share
                    .as_ref()
                    .map(ShareProjection::active_grants),
                Some(0)
            );
            assert_eq!(
                by("c-plain.bin"),
                None,
                "an untagged workspace file has no state"
            );
        }

        // --- phase: share_control_locator_is_read_only_from_the_workspace_root ---
        {
            let root = tempfile::tempdir().unwrap();
            let nested = root.path().join("folder");
            std::fs::create_dir(&nested).unwrap();
            let socket = root.path().join("share.sock");
            xattr::set(
                root.path(),
                SHARE_CONTROL_XATTR,
                socket.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            assert_eq!(
                share_control_socket(&nested, &[root.path().to_path_buf()]),
                Some(socket)
            );
            assert_eq!(share_control_socket(&nested, &[]), None);
        }

        // --- phase: workspace_record_index_projects_nested_paths_by_stable_identity ---
        {
            let root = tempfile::tempdir().unwrap();
            let record_id = uuid::Uuid::from_u128(7);
            let projection = serde_json::json!({
                "schema_version": 1,
                "truncated": false,
                "records": [{
                    "record_id": record_id,
                    "path": "/deep/report.pdf",
                    "is_dir": false
                }]
            });
            xattr::set(
                root.path(),
                RECORD_INDEX_XATTR,
                &serde_json::to_vec(&projection).unwrap(),
            )
            .unwrap();

            let index = read_workspace_record_index(&[root.path().to_path_buf()]);
            assert!(!index.truncated);
            assert_eq!(index.records.len(), 1);
            assert_eq!(index.records[0].record_id, record_id);
            assert_eq!(index.records[0].path, root.path().join("deep/report.pdf"));
        }

        // --- phase: workspace_record_index_rejects_noncanonical_or_escaping_projections ---
        {
            let root = tempfile::tempdir().unwrap();
            for records in [
                serde_json::json!([{
                    "record_id": uuid::Uuid::from_u128(1),
                    "path": "/../outside",
                    "is_dir": false
                }]),
                serde_json::json!([
                    {
                        "record_id": uuid::Uuid::from_u128(2),
                        "path": "/b",
                        "is_dir": false
                    },
                    {
                        "record_id": uuid::Uuid::from_u128(1),
                        "path": "/a",
                        "is_dir": false
                    }
                ]),
            ] {
                let projection = serde_json::json!({
                    "schema_version": 1,
                    "truncated": false,
                    "records": records
                });
                xattr::set(
                    root.path(),
                    RECORD_INDEX_XATTR,
                    &serde_json::to_vec(&projection).unwrap(),
                )
                .unwrap();
                assert!(
                    read_workspace_record_index(&[root.path().to_path_buf()])
                        .records
                        .is_empty()
                );
            }

            let records = (1..=(MAX_RECORD_INDEX_ENTRIES + 1))
                .map(|value| {
                    serde_json::json!({
                        "record_id": uuid::Uuid::from_u128(value as u128),
                        "path": format!("/record-{value}"),
                        "is_dir": false
                    })
                })
                .collect::<Vec<_>>();
            let projection = serde_json::json!({
                "schema_version": 1,
                "truncated": true,
                "records": records
            });
            let bytes = serde_json::to_vec(&projection).unwrap();
            assert!(
                parse_record_index_projection(root.path(), &bytes).is_none(),
                "an oversized catalog is rejected wholesale"
            );
        }
    }

    /// Workspace roots parse and contain paths, and the revisioned pager is one-page bounded and indexes later rows incrementally.
    #[test]
    fn workspace_roots_and_pager_are_bounded() {
        // --- phase: workspace_root_parsing_and_containment ---
        {
            // Colon-separated roots parse, non-existent entries drop out, and
            // containment matches the root itself and any nested path — the exact
            // logic `read_dir_sync` uses to gate the sync-xattr query. Exercised
            // through the pure helpers so no global env is touched.
            let a = tempfile::tempdir().unwrap();
            let b = tempfile::tempdir().unwrap();
            let missing = a.path().join("missing-root");
            let spec = std::env::join_paths([a.path(), missing.as_path(), b.path()]).unwrap();
            let roots = parse_workspace_roots(&spec);
            assert!(
                roots.contains(&a.path().to_path_buf()),
                "existing root A kept: {roots:?}"
            );
            assert!(
                roots.contains(&b.path().to_path_buf()),
                "existing root B kept"
            );
            assert_eq!(roots.len(), 2, "the non-existent root is filtered out");

            assert!(
                path_under_any(a.path(), &roots),
                "the root itself is a workspace path"
            );
            assert!(
                path_under_any(&a.path().join("deep/file.txt"), &roots),
                "nested path matches"
            );
            assert!(
                !path_under_any(Path::new("/etc/passwd"), &roots),
                "unrelated path does not match"
            );
        }

        // --- phase: revisioned_workspace_pager_is_one_page_bounded_and_indexes_later_rows_incrementally ---
        {
            use ui::search::SearchProvider as _;

            let root = tempfile::tempdir().unwrap();
            // An injected FUSE page source models dozens of pages without trying
            // to store them as host xattrs (which many filesystems reject long
            // before the protocol's per-page limit). The late target sits alone
            // on a final partial page.
            let records = RECORD_INDEX_PAGE_ENTRIES * 64 + 1;
            let mut pager =
                WorkspaceRecordIndexPager::from_generated_v2(root.path().to_path_buf(), records);
            let first = pager.next_page();
            assert_eq!(
                first.records.len(),
                RECORD_INDEX_PAGE_ENTRIES,
                "one step cannot materialise the whole index"
            );
            assert!(!first.complete);
            let provider =
                crate::search_provider::CollaborativeRecordSearchProvider::new(&first.records);
            assert!(provider.search("late-target").is_empty());

            let mut indexed = first.records.len();
            while pager.has_more() {
                let page = pager.next_page();
                assert!(
                    page.records.len() <= RECORD_INDEX_PAGE_ENTRIES,
                    "an idle tick must not monopolise the frame"
                );
                indexed += page.records.len();
                provider.append_page(0, &page.records);
            }
            assert!(
                !pager.has_more(),
                "idle work stops once the current authority is indexed"
            );
            assert_eq!(indexed, records, "every page is indexed exactly once");
            assert!(
                provider
                    .search("late-target")
                    .iter()
                    .any(|hit| hit.title == "late-target.txt"),
                "later pages enter the retained search catalogue without a full refresh"
            );
        }
    }

    /// The pager withdraws rows after the mount disappears, does not claim nested shares and refuses duplicates across pages.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    #[cfg(unix)]
    fn workspace_pager_withdraws_and_refuses_ambiguous_rows() {
        // --- phase: completed_pager_withdraws_rows_after_mount_disappears ---
        {
            let root = tempfile::tempdir().unwrap();
            let projection = serde_json::json!({
                "schema_version": 1,
                "truncated": false,
                "records": [{
                    "record_id": uuid::Uuid::from_u128(9),
                    "path": "/visible.txt",
                    "is_dir": false,
                }],
            });
            xattr::set(
                root.path(),
                RECORD_INDEX_XATTR,
                &serde_json::to_vec(&projection).unwrap(),
            )
            .unwrap();
            let mut pager = WorkspaceRecordIndexPager::new(&[root.path().to_path_buf()]);
            assert_eq!(pager.next_page().records.len(), 1);
            xattr::remove(root.path(), RECORD_INDEX_XATTR).unwrap();
            assert!(
                pager.poll_generation(),
                "lost mount authority withdraws stale rows"
            );
            let withdrawn = pager.next_page();
            assert!(withdrawn.reset);
            assert!(withdrawn.records.is_empty());
            assert!(withdrawn.complete);
        }

        // --- phase: pager_does_not_advertise_nested_share_as_an_exact_record_authority ---
        {
            let parent = tempfile::tempdir().unwrap();
            let share = parent.path().join("received-share");
            std::fs::create_dir(&share).unwrap();
            let projection = serde_json::json!({
                "schema_version": 1,
                "truncated": false,
                "records": [{
                    "record_id": uuid::Uuid::from_u128(1),
                    "path": "/not-an-authority-record",
                    "is_dir": false,
                }],
            });
            xattr::set(
                &share,
                RECORD_INDEX_XATTR,
                &serde_json::to_vec(&projection).unwrap(),
            )
            .unwrap();
            let mut pager = WorkspaceRecordIndexPager::new(&[parent.path().to_path_buf()]);
            let page = pager.next_page();
            assert!(page.records.is_empty());
            assert!(page.complete);
            assert!(
                resolve_workspace_record(&[parent.path().to_path_buf()], uuid::Uuid::from_u128(1))
                    .is_none(),
                "walking child mounts would make search publish a record that exact opens cannot route"
            );
        }

        // --- phase: pager_refuses_duplicate_identity_across_revisioned_page_boundary ---
        {
            let root = tempfile::tempdir().unwrap();
            let revision = "b".repeat(64);
            xattr::set(
                root.path(),
                RECORD_INDEX_V2_XATTR,
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": RECORD_INDEX_V2_VERSION,
                    "revision": revision,
                    "records": 129,
                    "page_entries": RECORD_INDEX_PAGE_ENTRIES,
                }))
                .unwrap(),
            )
            .unwrap();
            let first = (1..=RECORD_INDEX_PAGE_ENTRIES)
                .map(|id| {
                    serde_json::json!({
                        "record_id": uuid::Uuid::from_u128(id as u128),
                        "path": format!("/row-{id}"),
                        "is_dir": false,
                    })
                })
                .collect::<Vec<_>>();
            for (offset, next, records) in [
                (0, Some(RECORD_INDEX_PAGE_ENTRIES), first),
                (
                    RECORD_INDEX_PAGE_ENTRIES,
                    None,
                    vec![serde_json::json!({
                        "record_id": uuid::Uuid::from_u128(RECORD_INDEX_PAGE_ENTRIES as u128),
                        "path": "/aliased",
                        "is_dir": false,
                    })],
                ),
            ] {
                xattr::set(
                    root.path(),
                    format!("{RECORD_INDEX_V2_XATTR}.{revision}.{offset}"),
                    &serde_json::to_vec(&serde_json::json!({
                        "schema_version": RECORD_INDEX_V2_VERSION,
                        "revision": revision,
                        "offset": offset,
                        "next_offset": next,
                        "records": records,
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            let mut pager = WorkspaceRecordIndexPager::new(&[root.path().to_path_buf()]);
            assert_eq!(pager.next_page().records.len(), RECORD_INDEX_PAGE_ENTRIES);
            let rejected = pager.next_page();
            assert!(rejected.records.is_empty());
            assert!(
                rejected.truncated,
                "an id cannot acquire a second path in one revision"
            );
        }
    }
}

/// Fail before ambient boundaries in the isolated library-test process.
#[cfg(test)]
pub(crate) mod fixture_io_guard {
    use std::cell::{Cell, RefCell};
    use std::marker::PhantomData;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    struct ListingState {
        root: PathBuf,
        entries: Vec<(PathBuf, bool)>,
    }

    thread_local! {
        static DENY: Cell<bool> = const { Cell::new(false) };
        static ATTEMPTS: Cell<usize> = const { Cell::new(0) };
        static LISTINGS: RefCell<Option<ListingState>> = const { RefCell::new(None) };
    }

    /// Per-test-thread ambient-I/O guard.
    ///
    /// Rust runs independent unit tests concurrently in one process. Keeping
    /// this state thread-local prevents one negative-boundary test from
    /// denying legitimate fixture I/O in an unrelated test.
    pub(crate) struct DenyAmbientIo {
        owner: std::thread::ThreadId,
        not_send: PhantomData<Rc<()>>,
    }
    impl DenyAmbientIo {
        pub(crate) fn new() -> Self {
            DENY.with(|deny| assert!(!deny.replace(true), "ambient guard already armed"));
            ATTEMPTS.set(0);
            Self {
                owner: std::thread::current().id(),
                not_send: PhantomData,
            }
        }
        pub(crate) fn attempts(&self) -> usize {
            assert_eq!(self.owner, std::thread::current().id());
            ATTEMPTS.get()
        }
    }
    impl Drop for DenyAmbientIo {
        fn drop(&mut self) {
            DENY.set(false);
        }
    }
    pub(crate) fn ambient_io(boundary: &'static str) {
        ATTEMPTS.set(ATTEMPTS.get().saturating_add(1));
        DENY.with(|deny| assert!(!deny.get(), "ambient I/O denied: {boundary}"));
    }

    pub(crate) struct OwnedListings {
        owner: std::thread::ThreadId,
        not_send: PhantomData<Rc<()>>,
    }
    impl OwnedListings {
        pub(crate) fn new(root: &Path) -> Self {
            LISTINGS.with(|listings| {
                let mut state = listings.borrow_mut();
                assert!(state.is_none(), "listing observer already armed");
                *state = Some(ListingState {
                    root: root.to_path_buf(),
                    entries: Vec::new(),
                });
            });
            Self {
                owner: std::thread::current().id(),
                not_send: PhantomData,
            }
        }
        pub(crate) fn observations(&self) -> Vec<(PathBuf, bool)> {
            assert_eq!(self.owner, std::thread::current().id());
            LISTINGS.with(|listings| {
                listings
                    .borrow()
                    .as_ref()
                    .expect("listing observer is armed")
                    .entries
                    .clone()
            })
        }
    }
    impl Drop for OwnedListings {
        fn drop(&mut self) {
            LISTINGS.with(|listings| *listings.borrow_mut() = None);
        }
    }
    pub(crate) fn observe_owned_listing(path: &Path, in_workspace: bool) {
        LISTINGS.with(|listings| {
            let mut state = listings.borrow_mut();
            if let Some(state) = state.as_mut() {
                assert!(
                    path.starts_with(&state.root),
                    "listing outside owned fixture: {}",
                    path.display()
                );
                state.entries.push((path.to_path_buf(), in_workspace));
            }
        });
    }
}
