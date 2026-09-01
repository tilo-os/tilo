use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The qOS collaborative FUSE mount. Per `fs.service` the daemon mounts
/// at `/var/workspace` (the older `/workspace` was a bug in this app).
pub const WORKSPACE_ROOT: &str = "/var/workspace";

/// The extended-attribute the qOS FUSE driver answers with an entry's sync
/// state. Kept in lock-step with `fs::SYNC_XATTR`.
pub const SYNC_XATTR: &str = "user.qos.sync";

/// The qOS FUSE **sync / hydration state** of an entry, read live from the
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

/// Read the qOS sync state of `path` from its [`SYNC_XATTR`] extended attribute.
/// `None` when the attribute is absent (an ordinary file, off any workspace
/// mount) or unreadable. This is a single `getxattr` — the same call
/// `getfattr -n user.qos.sync <path>` makes.
pub fn read_sync_state(path: &Path) -> Option<SyncState> {
    match xattr::get(path, SYNC_XATTR) {
        Ok(Some(bytes)) => SyncState::from_xattr(&bytes),
        _ => None,
    }
}

/// The extended attribute the qOS FUSE driver answers with a file's **thumbnail**
/// (a small PNG the host generates as metadata, ahead of / independent of the
/// content). Kept in lock-step with `fs::THUMB_XATTR`.
pub const THUMB_XATTR: &str = "user.qos.thumb";

/// Raw 32-byte content/manifest identity exposed by the qOS FUSE driver.
/// Using this instead of `(path, mtime, size)` makes preview cache entries
/// survive renames and deduplicate identical content across folders and peers.
pub const CONTENT_HASH_XATTR: &str = "user.qos.content-hash";

/// Raw 16-byte stable collaborative-record UUID.
pub const RECORD_ID_XATTR: &str = "user.qos.record-id";
pub const METADATA_XATTR: &str = "user.qos.metadata";
pub const MAX_METADATA_XATTR_BYTES: usize = 2_048;
pub const RECORD_INDEX_XATTR: &str = "user.qos.record-index";
pub const MAX_RECORD_INDEX_XATTR_BYTES: usize = 60 * 1024;
pub const MAX_RECORD_INDEX_ENTRIES: usize = 256;
pub const AVAILABILITY_XATTR: &str = "user.qos.availability";
pub const AVAILABILITY_CONTROL_XATTR: &str = "user.qos.availability-control";
pub const PIN_XATTR: &str = "user.qos.pin";
pub const SHARE_XATTR: &str = "user.qos.share";
pub const SHARE_CONTROL_XATTR: &str = "user.qos.share-control";
pub const HISTORY_XATTR: &str = "user.qos.history";
pub const HISTORY_CONTROL_XATTR: &str = "user.qos.history-control";
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
#[allow(clippy::struct_excessive_bools)] // Mirrors the versioned, redundant share projection contract.
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
    if bytes.len() > MAX_RECORD_INDEX_XATTR_BYTES {
        return None;
    }
    let projection: RecordIndexProjection = serde_json::from_slice(&bytes).ok()?;
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

/// Read bounded, ACL-filtered record catalogs from the workspace mounts.
/// Recipient shares are individual mounts one level below `Shared`, so that
/// ordinary parent is expanded by one bounded directory read only when it has
/// no catalog of its own. No file contents are opened.
pub fn read_workspace_record_index(roots: &[PathBuf]) -> WorkspaceRecordIndex {
    let mut catalog_roots = Vec::new();
    let mut truncated = false;
    for root in roots {
        if read_record_index_projection(root).is_some() {
            catalog_roots.push(root.clone());
            continue;
        }
        let Ok(children) = std::fs::read_dir(root) else {
            continue;
        };
        for child in children.flatten().take(MAX_RECORD_INDEX_ENTRIES + 1) {
            if catalog_roots.len() >= MAX_RECORD_INDEX_ENTRIES {
                truncated = true;
                break;
            }
            if child.file_type().is_ok_and(|kind| kind.is_dir())
                && read_record_index_projection(&child.path()).is_some()
            {
                catalog_roots.push(child.path());
            }
        }
    }

    let mut records = Vec::new();
    for root in catalog_roots {
        let Some(projection) = read_record_index_projection(&root) else {
            continue;
        };
        truncated |= projection.truncated;
        for entry in projection.records {
            if records.len() >= MAX_RECORD_INDEX_ENTRIES {
                truncated = true;
                break;
            }
            let Some(path) = safe_projected_path(&root, &entry.path) else {
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
    }
    records.sort_by_key(|record| record.record_id);
    records.dedup_by_key(|record| record.record_id);
    WorkspaceRecordIndex { records, truncated }
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

/// The qOS workspace roots the FUSE driver mounts (first-class sidebar
/// locations, and the dirs whose entries carry sync state). Normally just
/// [`WORKSPACE_ROOT`]; `QOS_WORKSPACE_ROOTS` (colon-separated) overrides it for
/// tests and alternate mounts. Only roots that currently exist are returned.
pub fn workspace_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("QOS_WORKSPACE_ROOTS") {
        return parse_workspace_roots(&v);
    }
    let p = PathBuf::from(WORKSPACE_ROOT);
    let mut roots = if p.exists() { vec![p] } else { Vec::new() };
    // The per-user recipient service deliberately mounts personal shares
    // outside the ordinary workspace replication graph. Surface that root as
    // another collaborative System place once it exists.
    if let Some(received) = user_home().map(|home| home.join("Shared"))
        && received.exists()
        && !roots.contains(&received)
    {
        roots.push(received);
    }
    roots
}

/// Parse a `QOS_WORKSPACE_ROOTS` value (colon-separated) into the roots that
/// currently exist. Split out so the parsing is unit-testable without touching
/// the process environment.
fn parse_workspace_roots(val: &str) -> Vec<PathBuf> {
    val.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// Whether `path` sits inside a qOS workspace root (so its entries carry sync
/// state and should be queried for the [`SYNC_XATTR`]).
pub fn is_workspace_path(path: &Path) -> bool {
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
/// over an explicit root list for the same reason [`path_under_any`] is.
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
    /// The qOS FUSE sync state, when this entry is on a workspace mount.
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

/// The listing body, with the workspace-gating decision passed in (so tests can
/// exercise the sync-state population path without mutating the global
/// `QOS_WORKSPACE_ROOTS` env). Production always routes through
/// [`read_dir_sync`], which derives `in_workspace` from the real environment.
fn read_dir_inner(path: &Path, in_workspace: bool) -> Result<Vec<Entry>, String> {
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
    // `/workspace` predates `QOS_WORKSPACE_ROOTS`; retain it only when no
    // override was supplied, so old installations keep their start location.
    if std::env::var_os("QOS_WORKSPACE_ROOTS").is_none() {
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
/// `QOS_USER_HOME` is present only when a grant actually bound the real
/// home, so preferring it can never point at a directory this process
/// cannot read.
///
/// Every surface that needs to know where home is resolves it here, so
/// the directory the window opens on and the home the breadcrumb
/// abbreviates against are the same answer. When they were two answers
/// the trail rendered as an absolute path from the filesystem root: the
/// window was on the real home and `format_path` was measuring against
/// `/run/qos/app`, which nothing is under.
pub fn user_home() -> Option<PathBuf> {
    ui::sysdirs::user_home_override().or_else(dirs::home_dir)
}

#[cfg(test)]
mod sync_state_tests {
    use super::*;

    #[test]
    fn metadata_projection_is_bounded_and_validated() {
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

    #[test]
    fn from_xattr_parses_the_three_states_and_rejects_junk() {
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

    #[test]
    fn read_sync_state_round_trips_a_real_xattr() {
        // A real getxattr against a real `user.qos.sync` attribute — the exact
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

    #[test]
    fn read_thumb_xattr_round_trips_a_real_host_preview() {
        // The FS-metadata channel: a host writes a preview into `user.qos.thumb`
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

    #[test]
    fn read_content_hash_requires_exactly_the_raw_blake3_width() {
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

    #[test]
    fn read_record_id_requires_exactly_the_raw_uuid_width() {
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

    #[test]
    fn availability_parser_keeps_unknown_distinct_from_known_zero_and_is_bounded() {
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
            AvailabilityProjection::parse(&vec![b' '; MAX_AVAILABILITY_XATTR_BYTES + 1]).is_none()
        );
    }

    #[test]
    fn share_and_history_projections_are_bounded_and_actionable() {
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
        assert!(HistoryProjection::parse(&vec![b' '; MAX_COLLAB_PROJECTION_BYTES + 1]).is_none());
    }

    #[test]
    fn read_dir_populates_sync_state_only_inside_a_workspace_root() {
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
        let projected_availability = availability_json(&serde_json::json!({"state": "unknown"}));
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

    #[test]
    fn workspace_root_parsing_and_containment() {
        // Colon-separated roots parse, non-existent entries drop out, and
        // containment matches the root itself and any nested path — the exact
        // logic `read_dir_sync` uses to gate the sync-xattr query. Exercised
        // through the pure helpers so no global env is touched.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let spec = format!(
            "{}:/does/not/exist:{}",
            a.path().display(),
            b.path().display()
        );
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

    #[test]
    fn share_control_locator_is_read_only_from_the_workspace_root() {
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

    #[test]
    fn workspace_record_index_projects_nested_paths_by_stable_identity() {
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

    #[test]
    fn workspace_record_index_rejects_noncanonical_or_escaping_projections() {
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
        xattr::set(
            root.path(),
            RECORD_INDEX_XATTR,
            &serde_json::to_vec(&projection).unwrap(),
        )
        .unwrap();
        assert!(
            read_workspace_record_index(&[root.path().to_path_buf()])
                .records
                .is_empty(),
            "an oversized catalog is rejected wholesale"
        );
    }
}
