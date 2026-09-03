//! Safe, transport-neutral continuation state for one person's devices.
//!
//! A collaboration room describes work shared with its members. Continuation
//! is different: it describes what *one person's* devices should offer to
//! reopen. It deliberately has no transport here, and deliberately does not
//! reuse the room document: a colleague changing their active tab must never
//! move another person's focus.
//!
//! The model carries logical app and file-record references only. It never
//! carries Wayland surface ids, local paths, file contents, thumbnails, or
//! credentials. Apps may attach a small, explicitly syncable opaque resume
//! payload; applications remain responsible for encoding a path-free state
//! such as a URL, record id, page number, or playback position.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use thiserror::Error;

use crate::shell::state::{RoomId, TileContent, WorkspaceTree};

/// Wire schema for [`DeviceContinuationSnapshot`].
pub const CONTINUITY_SCHEMA_VERSION: u16 = 1;
/// Resume data is intentionally small: it is metadata, never an app backup.
pub const MAX_RESUME_BYTES: usize = 32 * 1024;
const MAX_DEVICE_ID_BYTES: usize = 128;
const MAX_APP_ID_BYTES: usize = 256;
const MAX_RECORD_ID_BYTES: usize = 256;

/// A stable, human-identity-certified device identifier.
///
/// The network layer will supply this from an authenticated device key. It is
/// kept textual here so the pure model does not depend on a transport crate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContinuityError> {
        let value = value.into();
        if !valid_identifier(&value, MAX_DEVICE_ID_BYTES) {
            return Err(ContinuityError::InvalidDeviceId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DeviceId::new(self.0.clone())
            .map_err(S::Error::custom)?
            .0
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DeviceId::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Stable logical identity for an app instance. A shell tab id is already
/// durable and peer-salted, so it is suitable for scratch-record continuity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppInstanceId(pub u64);

/// A monotonically increasing device-authored revision. Device id is part of
/// the ordering so independently captured states always have a deterministic
/// winner, even when their counters coincide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityStamp {
    pub counter: u64,
    pub device: DeviceId,
}

impl ContinuityStamp {
    fn ordering_key(&self) -> (u64, &str) {
        (self.counter, self.device.as_str())
    }
}

/// Where an app belongs in the restored tiled layout. These are ordinal
/// positions, rather than compositor object ids, so another device can create
/// its own strips and groups without accidentally reusing local handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabPlacement {
    pub strip: u32,
    pub group: u32,
    pub tab: u32,
}

/// A path-free logical launch target.
///
/// A file target names a durable record and optional hashes for its current
/// metadata/preview. There is deliberately no local path, content manifest,
/// or thumbnail payload: Files can show a projected preview and hydrate bytes
/// only when the person opens the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchIntent {
    App {
        app_id: String,
    },
    FileReference {
        record_id: String,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    },
}

impl LaunchIntent {
    pub fn app(app_id: impl Into<String>) -> Result<Self, ContinuityError> {
        let intent = Self::App {
            app_id: app_id.into(),
        };
        intent.validate()?;
        Ok(intent)
    }

    pub fn file_reference(
        record_id: impl Into<String>,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    ) -> Result<Self, ContinuityError> {
        let intent = Self::FileReference {
            record_id: record_id.into(),
            content_hash,
            thumbnail_hash,
        };
        intent.validate()?;
        Ok(intent)
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        match self {
            Self::App { app_id } if valid_app_id(app_id) => Ok(()),
            Self::FileReference { record_id, .. }
                if valid_identifier(record_id, MAX_RECORD_ID_BYTES) =>
            {
                Ok(())
            }
            Self::App { .. } => Err(ContinuityError::InvalidAppId),
            Self::FileReference { .. } => Err(ContinuityError::InvalidRecordReference),
        }
    }
}

#[derive(Serialize, Deserialize)]
// Externally tagged so the same validated representation works with both
// human-readable JSON and non-self-describing Postcard checkpoints.
#[serde(rename_all = "snake_case")]
enum LaunchIntentWire {
    App {
        app_id: String,
    },
    FileReference {
        record_id: String,
        content_hash: Option<[u8; 32]>,
        thumbnail_hash: Option<[u8; 32]>,
    },
}

impl Serialize for LaunchIntent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        match self {
            Self::App { app_id } => LaunchIntentWire::App {
                app_id: app_id.clone(),
            },
            Self::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            } => LaunchIntentWire::FileReference {
                record_id: record_id.clone(),
                content_hash: *content_hash,
                thumbnail_hash: *thumbnail_hash,
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LaunchIntent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let intent = match LaunchIntentWire::deserialize(deserializer)? {
            LaunchIntentWire::App { app_id } => Self::App { app_id },
            LaunchIntentWire::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            } => Self::FileReference {
                record_id,
                content_hash,
                thumbnail_hash,
            },
        };
        intent.validate().map_err(D::Error::custom)?;
        Ok(intent)
    }
}

/// Whether an app explicitly permits a resume state to leave this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeSensitivity {
    Syncable,
    Sensitive,
}

/// Versioned opaque bytes supplied by an app's explicit safe-resume contract.
///
/// `Sensitive` state is rejected rather than redacted: silently publishing a
/// partial credential/cookie/session blob creates a misleading recovery path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeMetadata {
    schema_version: u16,
    bytes: Vec<u8>,
}

impl ResumeMetadata {
    pub fn new(
        schema_version: u16,
        sensitivity: ResumeSensitivity,
        bytes: Vec<u8>,
    ) -> Result<Self, ContinuityError> {
        if sensitivity != ResumeSensitivity::Syncable {
            return Err(ContinuityError::SensitiveResumeState);
        }
        let value = Self {
            schema_version,
            bytes,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        if self.schema_version == 0 {
            return Err(ContinuityError::InvalidResumeSchema);
        }
        if self.bytes.len() > MAX_RESUME_BYTES {
            return Err(ContinuityError::ResumeTooLarge);
        }
        if has_local_path_marker(&self.bytes) {
            return Err(ContinuityError::LocalPathInResumeState);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct ResumeMetadataWire {
    schema_version: u16,
    bytes: Vec<u8>,
}

impl Serialize for ResumeMetadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        ResumeMetadataWire {
            schema_version: self.schema_version,
            bytes: self.bytes.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResumeMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ResumeMetadataWire::deserialize(deserializer)?;
        let value = Self {
            schema_version: wire.schema_version,
            bytes: wire.bytes,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

/// One app that can be restored on another device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstanceState {
    pub id: AppInstanceId,
    pub placement: TabPlacement,
    pub launch: LaunchIntent,
    pub resume: Option<ResumeMetadata>,
    pub stamp: ContinuityStamp,
}

impl AppInstanceState {
    pub fn new(
        id: AppInstanceId,
        placement: TabPlacement,
        launch: LaunchIntent,
        resume: Option<ResumeMetadata>,
        stamp: ContinuityStamp,
    ) -> Self {
        Self {
            id,
            placement,
            launch,
            resume,
            stamp,
        }
    }

    fn validate_for_device(&self, device: &DeviceId) -> Result<(), ContinuityError> {
        self.launch.validate()?;
        if let Some(resume) = &self.resume {
            resume.validate()?;
        }
        if &self.stamp.device != device {
            return Err(ContinuityError::CrossDeviceAppStamp);
        }
        Ok(())
    }
}

/// The safe app instances in one collaboration room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContinuation {
    pub room: RoomId,
    pub apps: Vec<AppInstanceState>,
}

/// A focus pointer belonging to exactly one device. It is intentionally
/// outside shared workspace layout state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFocus {
    pub room: RoomId,
    pub app: Option<AppInstanceId>,
}

/// One device's latest offered continuation snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceContinuationSnapshot {
    pub schema_version: u16,
    pub device: DeviceId,
    pub stamp: ContinuityStamp,
    pub focus: Option<DeviceFocus>,
    pub workspaces: Vec<WorkspaceContinuation>,
}

impl DeviceContinuationSnapshot {
    pub fn new(
        device: DeviceId,
        counter: u64,
        focus: Option<DeviceFocus>,
        workspaces: Vec<WorkspaceContinuation>,
    ) -> Result<Self, ContinuityError> {
        let value = Self {
            schema_version: CONTINUITY_SCHEMA_VERSION,
            stamp: ContinuityStamp {
                counter,
                device: device.clone(),
            },
            device,
            focus,
            workspaces,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ContinuityError> {
        if self.schema_version != CONTINUITY_SCHEMA_VERSION {
            return Err(ContinuityError::UnsupportedSchema(self.schema_version));
        }
        if self.stamp.device != self.device {
            return Err(ContinuityError::SnapshotStampDeviceMismatch);
        }
        let mut rooms = BTreeSet::new();
        let mut apps = BTreeSet::new();
        for workspace in &self.workspaces {
            if workspace.room.is_nil() || !rooms.insert(workspace.room) {
                return Err(ContinuityError::InvalidOrDuplicateRoom);
            }
            for app in &workspace.apps {
                app.validate_for_device(&self.device)?;
                if !apps.insert(app.id) {
                    return Err(ContinuityError::DuplicateAppInstance);
                }
            }
        }
        if let Some(focus) = &self.focus {
            if focus.room.is_nil() || !rooms.contains(&focus.room) {
                return Err(ContinuityError::InvalidFocus);
            }
            if let Some(app) = focus.app {
                let belongs_to_room = self
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == focus.room)
                    .is_some_and(|workspace| workspace.apps.iter().any(|state| state.id == app));
                if !belongs_to_room {
                    return Err(ContinuityError::InvalidFocus);
                }
            }
        }
        Ok(())
    }

    /// Capture only safe logical state from the shell's persisted tree.
    pub fn capture(tree: &WorkspaceTree, device: DeviceId, counter: u64) -> CaptureReport {
        Self::capture_with_resume(tree, device, counter, |_, _, _| None)
    }

    /// Capture the shell topology and attach app-supplied safe metadata.
    ///
    /// The resolver is intentionally pull-only and gets no mutable tree
    /// access. Receiving a remote checkpoint therefore cannot change local
    /// focus, launch an app, or start media as a side effect of capture.
    pub fn capture_with_resume<F>(
        tree: &WorkspaceTree,
        device: DeviceId,
        counter: u64,
        mut resume_for: F,
    ) -> CaptureReport
    where
        F: FnMut(RoomId, AppInstanceId, &LaunchIntent) -> Option<ResumeMetadata>,
    {
        let stamp = ContinuityStamp {
            counter,
            device: device.clone(),
        };
        let mut redacted_foreign_tabs = 0;
        let mut workspaces = Vec::new();

        for workspace in &tree.workspaces {
            // A local id is not usable on another device. Until a workspace
            // has its durable room id, keeping it local is the safe outcome.
            if workspace.room.is_nil() {
                continue;
            }
            let mut apps = Vec::new();
            for (strip_index, strip) in workspace.strips.iter().enumerate() {
                for (group_index, group) in strip.window_groups.iter().enumerate() {
                    for (tab_index, tab) in group.tabs.iter().enumerate() {
                        let launch = match &tab.tile {
                            TileContent::AppPlaceholder { kind, .. }
                            | TileContent::Stub { kind, .. } => {
                                LaunchIntent::app(kind.app_id()).ok()
                            }
                            // A locally-adopted foreign app is resumable by
                            // its declared app id, but its compositor surface
                            // id stays strictly local and is never serialized.
                            TileContent::Foreign { app_id, .. } => {
                                LaunchIntent::app(app_id.clone()).ok()
                            }
                            // Peer windows are another person's live session,
                            // not our resumable app instance.
                            TileContent::PeerWindow { .. } => {
                                redacted_foreign_tabs += 1;
                                None
                            }
                        };
                        let Some(launch) = launch else { continue };
                        let app_instance_id = AppInstanceId(tab.id);
                        let (Ok(strip), Ok(group), Ok(tab_index)) = (
                            u32::try_from(strip_index),
                            u32::try_from(group_index),
                            u32::try_from(tab_index),
                        ) else {
                            continue;
                        };
                        let resume = resume_for(workspace.room, app_instance_id, &launch);
                        apps.push(AppInstanceState::new(
                            app_instance_id,
                            TabPlacement {
                                strip,
                                group,
                                tab: tab_index,
                            },
                            launch,
                            resume,
                            stamp.clone(),
                        ));
                    }
                }
            }
            workspaces.push(WorkspaceContinuation {
                room: workspace.room,
                apps,
            });
        }

        let focus = focus_from_tree(tree, &workspaces);
        // Capture only constructs values that have already passed its field
        // validators. An impossible internal mismatch becomes an empty safe
        // snapshot rather than publishing a partially unsafe state.
        let snapshot = Self::new(device, counter, focus, workspaces).unwrap_or_else(|error| {
            tracing::warn!(%error, "personal continuity capture rejected local state");
            Self::new(stamp.device, counter, None, Vec::new())
                .expect("an empty continuation snapshot is valid")
        });
        CaptureReport {
            snapshot,
            redacted_foreign_tabs,
        }
    }

    /// Capture using the authenticated app-report registry. The lookup is by
    /// durable shell tab id and launch instance, never by app id alone.
    pub fn capture_with_registry(
        tree: &WorkspaceTree,
        device: DeviceId,
        counter: u64,
        registry: &crate::shell::personal_continuity_registry::ContinuationRegistry,
    ) -> CaptureReport {
        Self::capture_with_resume(tree, device, counter, |room, tab, launch| {
            registry.resume_for_tab(room, tab, launch)
        })
    }
}

#[derive(Serialize, Deserialize)]
struct DeviceContinuationSnapshotWire {
    schema_version: u16,
    device: DeviceId,
    stamp: ContinuityStamp,
    focus: Option<DeviceFocus>,
    workspaces: Vec<WorkspaceContinuation>,
}

impl Serialize for DeviceContinuationSnapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        DeviceContinuationSnapshotWire {
            schema_version: self.schema_version,
            device: self.device.clone(),
            stamp: self.stamp.clone(),
            focus: self.focus.clone(),
            workspaces: self.workspaces.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DeviceContinuationSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = DeviceContinuationSnapshotWire::deserialize(deserializer)?;
        let value = Self {
            schema_version: wire.schema_version,
            device: wire.device,
            stamp: wire.stamp,
            focus: wire.focus,
            workspaces: wire.workspaces,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

/// Diagnostics from a capture. The number records windows whose app identity
/// was not safe and portable enough to synchronize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureReport {
    pub snapshot: DeviceContinuationSnapshot,
    pub redacted_foreign_tabs: u32,
}

/// A convergent collection of each device's own latest snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalContinuity {
    snapshots: BTreeMap<DeviceId, DeviceContinuationSnapshot>,
}

impl PersonalContinuity {
    /// Keep the newest snapshot for its owning device. A same-device,
    /// same-counter but different payload is rejected: accepting based on
    /// arrival order would make replicas diverge.
    pub fn upsert(
        &mut self,
        incoming: DeviceContinuationSnapshot,
    ) -> Result<bool, ContinuityError> {
        incoming.validate()?;
        match self.snapshots.get(&incoming.device) {
            None => {
                self.snapshots.insert(incoming.device.clone(), incoming);
                Ok(true)
            }
            Some(current) if incoming.stamp.counter > current.stamp.counter => {
                self.snapshots.insert(incoming.device.clone(), incoming);
                Ok(true)
            }
            Some(current)
                if incoming.stamp.counter == current.stamp.counter && incoming != *current =>
            {
                Err(ContinuityError::SameCounterConflict)
            }
            Some(_) => Ok(false),
        }
    }

    pub fn snapshot_for(&self, device: &DeviceId) -> Option<&DeviceContinuationSnapshot> {
        self.snapshots.get(device)
    }

    /// Every device's latest snapshot, ordered by stable device id. The
    /// returned map is read-only so callers cannot bypass [`Self::upsert`]'s
    /// convergence checks.
    pub fn snapshots(&self) -> &BTreeMap<DeviceId, DeviceContinuationSnapshot> {
        &self.snapshots
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Focus is a per-device preference, never a global winner.
    pub fn focus_for(&self, device: &DeviceId) -> Option<&DeviceFocus> {
        self.snapshots
            .get(device)
            .and_then(|snapshot| snapshot.focus.as_ref())
    }

    /// Deterministically select the newest safe checkpoint for one logical
    /// app instance across devices. Equal counters break by authenticated
    /// device id, never message arrival order.
    pub fn newest_app_state(&self, app_id: AppInstanceId) -> Option<&AppInstanceState> {
        self.snapshots
            .values()
            .flat_map(|snapshot| snapshot.workspaces.iter())
            .flat_map(|workspace| workspace.apps.iter())
            .filter(|app| app.id == app_id)
            .max_by_key(|app| app.stamp.ordering_key())
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        for (device, snapshot) in &self.snapshots {
            snapshot.validate()?;
            if device != &snapshot.device {
                return Err(ContinuityError::SnapshotMapDeviceMismatch);
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct PersonalContinuityWire {
    // A vec, rather than a map, means deserialization cannot silently drop a
    // duplicated device id. `upsert` either accepts one precise newest value
    // or rejects a same-counter conflict.
    snapshots: Vec<DeviceContinuationSnapshot>,
}

impl Serialize for PersonalContinuity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(S::Error::custom)?;
        PersonalContinuityWire {
            snapshots: self.snapshots.values().cloned().collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PersonalContinuity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PersonalContinuityWire::deserialize(deserializer)?;
        let mut value = Self::default();
        for snapshot in wire.snapshots {
            if value.snapshots.contains_key(&snapshot.device) {
                return Err(D::Error::custom("duplicate device continuation snapshot"));
            }
            value.upsert(snapshot).map_err(D::Error::custom)?;
        }
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

/// Errors are deliberately explicit so a future transport can report a safe
/// redaction rather than silently retaining an unsafe prior checkpoint.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ContinuityError {
    #[error("device id is invalid")]
    InvalidDeviceId,
    #[error("app id is invalid or looks like a local path")]
    InvalidAppId,
    #[error("file record reference is invalid or looks like a local path")]
    InvalidRecordReference,
    #[error("sensitive app state cannot be synced")]
    SensitiveResumeState,
    #[error("resume state has no schema version")]
    InvalidResumeSchema,
    #[error("resume state exceeds {MAX_RESUME_BYTES} bytes")]
    ResumeTooLarge,
    #[error("resume state contains a raw local path marker")]
    LocalPathInResumeState,
    #[error("unsupported continuation schema version {0}")]
    UnsupportedSchema(u16),
    #[error("snapshot stamp belongs to another device")]
    SnapshotStampDeviceMismatch,
    #[error("snapshot map key does not match the embedded device id")]
    SnapshotMapDeviceMismatch,
    #[error("app state stamp belongs to another device")]
    CrossDeviceAppStamp,
    #[error("workspace room is nil or appears more than once")]
    InvalidOrDuplicateRoom,
    #[error("app instance appears more than once")]
    DuplicateAppInstance,
    #[error("focus does not name a captured app in its workspace")]
    InvalidFocus,
    #[error("two different states claim the same device counter")]
    SameCounterConflict,
}

fn focus_from_tree(
    tree: &WorkspaceTree,
    workspaces: &[WorkspaceContinuation],
) -> Option<DeviceFocus> {
    let workspace = tree.active_workspace()?;
    let room = workspace.room;
    let captured = workspaces.iter().find(|entry| entry.room == room)?;
    let strip_id = tree.active_strip.get(&workspace.id)?;
    let group_id = tree.active_group.get(strip_id)?;
    let group = workspace
        .strips
        .iter()
        .find(|strip| &strip.id == strip_id)?
        .window_groups
        .iter()
        .find(|group| &group.id == group_id)?;
    let app = AppInstanceId(group.active_tab);
    captured
        .apps
        .iter()
        .any(|state| state.id == app)
        .then_some(DeviceFocus {
            room,
            app: Some(app),
        })
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !looks_like_local_path(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

fn valid_app_id(value: &str) -> bool {
    valid_identifier(value, MAX_APP_ID_BYTES) && value.contains('.')
}

fn looks_like_local_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("file:")
        || value.starts_with("\\\\")
        || value.as_bytes().get(1) == Some(&b':')
        || value.contains('/')
        || value.contains('\\')
}

fn has_local_path_marker(bytes: &[u8]) -> bool {
    // This guards accidental JSON/text payloads. It cannot prove arbitrary
    // opaque binary data harmless; that is why apps must explicitly declare
    // the payload Syncable before it reaches this model.
    const MARKERS: [&[u8]; 7] = [
        b"/home/", b"/Users/", b"file://", b"C:\\", b"D:\\", b"\\\\", b"~/.",
    ];
    MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|window| window == *marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };
    use std::collections::HashMap;

    fn device(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    fn room(value: &str) -> RoomId {
        RoomId::parse(value).unwrap()
    }

    const ROOM_A: &str = "00000000-0000-4000-8000-000000000001";

    fn safe_app(id: u64, device: &DeviceId, counter: u64) -> AppInstanceState {
        AppInstanceState::new(
            AppInstanceId(id),
            TabPlacement {
                strip: 0,
                group: 0,
                tab: 0,
            },
            LaunchIntent::app("org.example.Reader").unwrap(),
            Some(ResumeMetadata::new(1, ResumeSensitivity::Syncable, b"page=4".to_vec()).unwrap()),
            ContinuityStamp {
                counter,
                device: device.clone(),
            },
        )
    }

    #[test]
    fn two_devices_keep_their_own_focus() {
        let laptop = device("laptop-01");
        let desktop = device("desktop-01");
        let room = room(ROOM_A);
        let laptop_state = DeviceContinuationSnapshot::new(
            laptop.clone(),
            2,
            Some(DeviceFocus {
                room,
                app: Some(AppInstanceId(11)),
            }),
            vec![WorkspaceContinuation {
                room,
                apps: vec![safe_app(11, &laptop, 2)],
            }],
        )
        .unwrap();
        let desktop_state = DeviceContinuationSnapshot::new(
            desktop.clone(),
            7,
            Some(DeviceFocus {
                room,
                app: Some(AppInstanceId(22)),
            }),
            vec![WorkspaceContinuation {
                room,
                apps: vec![safe_app(22, &desktop, 7)],
            }],
        )
        .unwrap();
        let mut continuity = PersonalContinuity::default();
        continuity.upsert(laptop_state).unwrap();
        continuity.upsert(desktop_state).unwrap();

        assert_eq!(
            continuity.focus_for(&laptop).unwrap().app,
            Some(AppInstanceId(11))
        );
        assert_eq!(
            continuity.focus_for(&desktop).unwrap().app,
            Some(AppInstanceId(22))
        );
    }

    #[test]
    fn newest_same_app_checkpoint_has_a_deterministic_winner() {
        let alpha = device("alpha-device");
        let beta = device("beta-device");
        let room = room(ROOM_A);
        let snapshot = |device: DeviceId| {
            DeviceContinuationSnapshot::new(
                device.clone(),
                9,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(77, &device, 9)],
                }],
            )
            .unwrap()
        };
        let mut continuity = PersonalContinuity::default();
        continuity.upsert(snapshot(beta.clone())).unwrap();
        continuity.upsert(snapshot(alpha)).unwrap();

        assert_eq!(
            continuity
                .newest_app_state(AppInstanceId(77))
                .unwrap()
                .stamp
                .device,
            beta,
            "equal counters break by device id, not message order"
        );
    }

    #[test]
    fn personal_continuity_round_trips_as_json_and_postcard() {
        let laptop = device("laptop-01");
        let room = room(ROOM_A);
        let snapshot = DeviceContinuationSnapshot::new(
            laptop.clone(),
            12,
            Some(DeviceFocus {
                room,
                app: Some(AppInstanceId(13)),
            }),
            vec![WorkspaceContinuation {
                room,
                apps: vec![safe_app(13, &laptop, 12)],
            }],
        )
        .unwrap();
        let mut original = PersonalContinuity::default();
        original.upsert(snapshot).unwrap();
        assert_eq!(original.len(), 1);
        assert_eq!(original.snapshots().len(), 1);

        let json = serde_json::to_vec(&original).unwrap();
        let from_json: PersonalContinuity = serde_json::from_slice(&json).unwrap();
        assert_eq!(from_json, original);

        let postcard = postcard::to_allocvec(&original).unwrap();
        let from_postcard: PersonalContinuity = postcard::from_bytes(&postcard).unwrap();
        assert_eq!(from_postcard, original);
    }

    #[test]
    fn rejects_sensitive_invalid_and_pathful_resume_metadata() {
        assert_eq!(
            ResumeMetadata::new(1, ResumeSensitivity::Sensitive, vec![]),
            Err(ContinuityError::SensitiveResumeState)
        );
        assert_eq!(
            ResumeMetadata::new(0, ResumeSensitivity::Syncable, vec![]),
            Err(ContinuityError::InvalidResumeSchema)
        );
        assert_eq!(
            ResumeMetadata::new(
                1,
                ResumeSensitivity::Syncable,
                b"{\"path\":\"/home/ada/secret\"}".to_vec(),
            ),
            Err(ContinuityError::LocalPathInResumeState)
        );
        assert!(LaunchIntent::app("/usr/bin/browser").is_err());
        assert!(LaunchIntent::file_reference("/home/ada/book.epub", None, None).is_err());
        assert!(
            serde_json::to_string(&LaunchIntent::App {
                app_id: "/home/ada/browser".into(),
            })
            .is_err(),
            "even a manually-built bad enum must be rejected at serialization"
        );
    }

    #[test]
    fn file_launch_is_reference_and_thumbnail_hash_only() {
        let intent =
            LaunchIntent::file_reference("record:book-42", Some([3; 32]), Some([4; 32])).unwrap();
        let encoded = serde_json::to_string(&intent).unwrap();
        assert!(encoded.contains("record:book-42"));
        assert!(encoded.contains("thumbnail_hash"));
        assert!(!encoded.contains("/home/"));
        assert!(!encoded.contains("manifest"));
        assert!(!encoded.contains("thumbnail_bytes"));
    }

    #[test]
    fn captures_safe_tabs_and_redacts_foreign_surface_ids() {
        let workspace_id = WorkspaceId(8);
        let strip_id = StripId(9);
        let group_id = 10;
        let room = room(ROOM_A);
        let mut workspace = Workspace::new(workspace_id, "Reading");
        workspace.room = room;
        workspace.strips = vec![Strip {
            id: strip_id,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: group_id,
                tabs: vec![
                    Tab {
                        id: 11,
                        title: "Reader".into(),
                        tile: TileContent::AppPlaceholder {
                            kind: AppKind::Other("org.example.Reader".into()),
                            label: "Reader".into(),
                        },
                    },
                    Tab {
                        id: 12,
                        title: "/home/ada/private.pdf".into(),
                        tile: TileContent::Foreign {
                            app_id: "org.example.Foreign".into(),
                            surface_id: 0xfeed_beef,
                        },
                    },
                ],
                active_tab: 11,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        let mut active_strip = HashMap::new();
        active_strip.insert(workspace_id, strip_id);
        let mut active_group = HashMap::new();
        active_group.insert(strip_id, group_id);
        let tree = WorkspaceTree {
            workspaces: vec![workspace],
            active: workspace_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        };

        let report = DeviceContinuationSnapshot::capture(&tree, device("laptop-01"), 5);
        assert_eq!(report.redacted_foreign_tabs, 0);
        assert_eq!(report.snapshot.workspaces[0].apps.len(), 2);
        assert_eq!(
            report.snapshot.workspaces[0].apps[1].launch,
            LaunchIntent::App {
                app_id: "org.example.Foreign".into()
            }
        );
        assert!(
            !postcard::to_allocvec(&report.snapshot)
                .unwrap()
                .windows(std::mem::size_of::<u64>())
                .any(|bytes| bytes == 0xfeed_beef_u64.to_le_bytes()),
            "the compositor-local surface id must not enter continuation"
        );
        assert_eq!(
            report.snapshot.focus.as_ref().unwrap().app,
            Some(AppInstanceId(11))
        );
        let encoded = serde_json::to_string(&report.snapshot).unwrap();
        assert!(!encoded.contains("feed_beef"));
        assert!(!encoded.contains("/home/ada"));
    }
}
