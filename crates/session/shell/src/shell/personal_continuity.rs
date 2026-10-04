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
pub const CONTINUITY_SCHEMA_VERSION: u16 = 2;
/// Resume data is intentionally small: it is metadata, never an app backup.
pub const MAX_RESUME_BYTES: usize = 32 * 1024;
const MAX_DEVICE_ID_BYTES: usize = 128;
const MAX_APP_ID_BYTES: usize = 256;
const MAX_RECORD_ID_BYTES: usize = 256;
const MAX_WORKSPACES_PER_DEVICE: usize = 256;
const MAX_APPS_PER_DEVICE: usize = 4096;
const MAX_TOMBSTONES_PER_DEVICE: usize = 32 * 1024;
const MAX_DEVICES: usize = 256;
/// Leave framing/headroom below the transport's 4 MiB ceiling.
const MAX_TOTAL_RESUME_BYTES: usize = 3 * 1024 * 1024;
const MAX_ENCODED_CHECKPOINT_BYTES: usize = 3_900_000;

/// A stable, human-identity-certified device identifier.
///
/// The network layer will supply this from an authenticated device key. It is
/// kept textual here so the pure model does not depend on a transport crate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    /// # Errors
    ///
    /// Returns an error when the device identifier is empty, oversized, or
    /// contains unsupported characters.
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

    /// Advance a local Lamport clock after observing this stamp. Device ids
    /// only break a true concurrent tie; they must never compensate for a
    /// freshly-started device having a smaller private counter.
    pub fn advance_clock(&self, local_counter: &mut u64) {
        *local_counter = (*local_counter).max(self.counter).saturating_add(1);
    }

    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.ordering_key() > other.ordering_key()
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
    /// # Errors
    ///
    /// Returns an error when `app_id` is not a valid application launch
    /// target.
    pub fn app(app_id: impl Into<String>) -> Result<Self, ContinuityError> {
        let intent = Self::App {
            app_id: app_id.into(),
        };
        intent.validate()?;
        Ok(intent)
    }

    /// # Errors
    ///
    /// Returns an error when the record identifier or supplied hashes are
    /// invalid for a file reference.
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
    /// # Errors
    ///
    /// Returns an error for sensitive, oversized, or otherwise invalid resume
    /// metadata.
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
    /// The app's collaboration record identity.  This is distinct from the
    /// durable shell tab id while a normal launch is waiting for toplevel
    /// adoption; carrying it lets a restored process join the source CRDT
    /// record instead of creating a lookalike empty document.
    pub record_instance: Option<AppInstanceId>,
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
            record_instance: None,
            stamp,
        }
    }

    fn validate_for_device(&self, device: &DeviceId) -> Result<(), ContinuityError> {
        self.launch.validate()?;
        if let Some(resume) = &self.resume {
            resume.validate()?;
            let LaunchIntent::App { app_id } = &self.launch else {
                return Err(ContinuityError::InvalidResumePayload);
            };
            if !crate::shell::personal_continuity_registry::validate_app_metadata(
                app_id,
                resume.schema_version(),
                resume.bytes(),
            ) {
                return Err(ContinuityError::InvalidResumePayload);
            }
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

/// Explicit removal of one logical app instance. Snapshot absence only says a
/// device does not currently offer an app; it cannot defeat an older offer
/// authored by another device after ownership moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppTombstone {
    pub room: RoomId,
    pub id: AppInstanceId,
    pub stamp: ContinuityStamp,
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
    pub tombstones: Vec<AppTombstone>,
}

impl DeviceContinuationSnapshot {
    /// # Errors
    ///
    /// Returns an error when the snapshot violates size, identity, or focus
    /// invariants.
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
            tombstones: Vec::new(),
        };
        value.validate()?;
        Ok(value)
    }

    /// # Errors
    ///
    /// Returns an error when schema, device, size, workspace, or focus
    /// invariants are violated.
    pub fn validate(&self) -> Result<(), ContinuityError> {
        if self.schema_version != CONTINUITY_SCHEMA_VERSION {
            return Err(ContinuityError::UnsupportedSchema(self.schema_version));
        }
        if self.stamp.device != self.device {
            return Err(ContinuityError::SnapshotStampDeviceMismatch);
        }
        if self.workspaces.len() > MAX_WORKSPACES_PER_DEVICE
            || self.tombstones.len() > MAX_TOMBSTONES_PER_DEVICE
        {
            return Err(ContinuityError::SnapshotTooLarge);
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
        if apps.len() > MAX_APPS_PER_DEVICE {
            return Err(ContinuityError::SnapshotTooLarge);
        }
        let mut tombstones = BTreeSet::new();
        for tombstone in &self.tombstones {
            if tombstone.room.is_nil()
                || tombstone.stamp.device != self.device
                || !tombstones.insert((tombstone.room, tombstone.id))
                || apps.contains(&tombstone.id)
            {
                return Err(ContinuityError::InvalidOrDuplicateTombstone);
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

    /// Preserve prior removals and add tombstones for apps this device owned
    /// in its preceding snapshot but has now closed. Callers must advance the
    /// Lamport counter past every received checkpoint before constructing
    /// `self`, so the removal causally defeats the offer it observed.
    ///
    /// # Errors
    ///
    /// Returns an error when `previous` belongs to another device or the
    /// resulting snapshot violates continuity invariants.
    pub fn record_closures_from(&mut self, previous: &Self) -> Result<(), ContinuityError> {
        if previous.device != self.device {
            return Err(ContinuityError::SnapshotStampDeviceMismatch);
        }
        let live: BTreeSet<_> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .apps
                    .iter()
                    .map(move |app| (workspace.room, app.id))
            })
            .collect();
        let mut tombstones: BTreeMap<_, _> = previous
            .tombstones
            .iter()
            .cloned()
            .map(|item| ((item.room, item.id), item))
            .collect();
        tombstones.extend(
            self.tombstones
                .iter()
                .cloned()
                .map(|item| ((item.room, item.id), item)),
        );
        for workspace in &previous.workspaces {
            for app in &workspace.apps {
                let key = (workspace.room, app.id);
                if !live.contains(&key) {
                    tombstones.insert(
                        key,
                        AppTombstone {
                            room: workspace.room,
                            id: app.id,
                            stamp: self.stamp.clone(),
                        },
                    );
                }
            }
        }
        // A later local reopen supersedes this device's old tombstone.
        tombstones.retain(|key, _| !live.contains(key));
        self.tombstones = tombstones.into_values().collect();
        self.validate()
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
    #[allow(
        clippy::too_many_lines,
        reason = "one walk over the tree decides every tile kind's launch intent in place"
    )]
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
        let mut redacted_foreign_tabs: u32 = 0;
        let mut workspaces = Vec::new();

        for workspace in &tree.desktop.workspaces {
            // A local id is not usable on another device. Until a workspace
            // has its durable room id, keeping it local is the safe outcome.
            if workspace.room.is_nil() {
                continue;
            }
            let mut apps = Vec::new();
            for (strip_index, strip) in workspace.strips.iter().enumerate() {
                for (group_index, group) in strip.window_groups.iter().enumerate() {
                    for (tab_index, tab) in group.tabs.iter().enumerate() {
                        // Materialized continuation tabs are remote offers,
                        // not locally-open state. Republishing them with this
                        // device's stamp creates an echo that can outlive the
                        // source device's later close (especially file cards,
                        // which use a Stub tile rather than PeerWindow).
                        if tree.continuation_tabs.contains(&tab.id) {
                            continue;
                        }
                        let launch = tree
                            .continuation_file_references
                            .get(&tab.id)
                            .cloned()
                            .or_else(|| match &tab.tile {
                                TileContent::WorkspaceActivity
                                | TileContent::HostedWindow { .. } => None,
                                TileContent::AppPlaceholder { kind, .. }
                                | TileContent::Stub { kind, .. } => {
                                    LaunchIntent::app(kind.app_id()).ok()
                                }
                                // A locally-adopted foreign app is resumable by
                                // its declared app id, but its compositor surface
                                // id stays strictly local and is never serialized.
                                TileContent::Foreign {
                                    app_id, surface_id, ..
                                } => {
                                    crate::shell::doc_open::continuation_file_reference_for_surface(
                                        *surface_id,
                                    )
                                    .or_else(|| LaunchIntent::app(app_id.clone()).ok())
                                }
                                TileContent::LocalApp { app_id, .. } => {
                                    LaunchIntent::app(app_id.clone()).ok()
                                }
                                // Peer windows are another person's live session,
                                // not our resumable app instance.
                                TileContent::PeerWindow { .. } => {
                                    redacted_foreign_tabs = redacted_foreign_tabs.saturating_add(1);
                                    None
                                }
                            });
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
        let mut snapshot = Self::new(device, counter, focus, workspaces).unwrap_or_else(|error| {
            tracing::warn!(%error, "personal continuity capture rejected local state");
            Self {
                schema_version: CONTINUITY_SCHEMA_VERSION,
                device: stamp.device.clone(),
                stamp: ContinuityStamp {
                    counter,
                    device: stamp.device.clone(),
                },
                focus: None,
                workspaces: Vec::new(),
                tombstones: Vec::new(),
            }
        });
        snapshot.tombstones = tree
            .continuation_closed_tabs
            .iter()
            .map(|(id, room)| AppTombstone {
                room: *room,
                id: AppInstanceId(*id),
                stamp: stamp.clone(),
            })
            .collect();
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
        let mut report = Self::capture_with_resume(tree, device, counter, |room, tab, launch| {
            registry.resume_for_tab(room, tab, launch)
        });
        for workspace in &mut report.snapshot.workspaces {
            for app in &mut workspace.apps {
                app.record_instance =
                    registry.record_instance_for_tab(workspace.room, app.id, &app.launch);
            }
        }
        report
    }
}

#[derive(Serialize, Deserialize)]
struct DeviceContinuationSnapshotWire {
    schema_version: u16,
    device: DeviceId,
    stamp: ContinuityStamp,
    focus: Option<DeviceFocus>,
    workspaces: Vec<WorkspaceContinuation>,
    tombstones: Vec<AppTombstone>,
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
            tombstones: self.tombstones.clone(),
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
            tombstones: wire.tombstones,
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
    ///
    /// # Errors
    ///
    /// Returns an error for invalid snapshots, conflicting equal counters, or
    /// a collection that would exceed its bounded resume payload.
    pub fn upsert(
        &mut self,
        incoming: DeviceContinuationSnapshot,
    ) -> Result<bool, ContinuityError> {
        incoming.validate()?;
        if !self.snapshots.contains_key(&incoming.device) && self.snapshots.len() >= MAX_DEVICES {
            return Err(ContinuityError::SnapshotTooLarge);
        }
        let replaced_bytes = self
            .snapshots
            .get(&incoming.device)
            .map_or(0, snapshot_resume_bytes);
        let projected_bytes = self
            .snapshots
            .values()
            .map(snapshot_resume_bytes)
            .sum::<usize>()
            .saturating_sub(replaced_bytes)
            .saturating_add(snapshot_resume_bytes(&incoming));
        if projected_bytes > MAX_TOTAL_RESUME_BYTES {
            return Err(ContinuityError::SnapshotTooLarge);
        }
        match self.snapshots.get(&incoming.device) {
            Some(current)
                if incoming.stamp.counter == current.stamp.counter && incoming != *current =>
            {
                return Err(ContinuityError::SameCounterConflict);
            }
            Some(current) if incoming.stamp.counter <= current.stamp.counter => return Ok(false),
            _ => {}
        }
        let mut projected = self.snapshots.clone();
        projected.insert(incoming.device.clone(), incoming.clone());
        let encoded = postcard::to_allocvec(&PersonalContinuityWire {
            snapshots: projected.values().cloned().collect(),
        })
        .map_err(|_| ContinuityError::InvalidCheckpoint)?;
        if encoded.len() > MAX_ENCODED_CHECKPOINT_BYTES {
            return Err(ContinuityError::SnapshotTooLarge);
        }
        self.snapshots.insert(incoming.device.clone(), incoming);
        Ok(true)
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
        let room = self
            .snapshots
            .values()
            .flat_map(|snapshot| &snapshot.workspaces)
            .find(|workspace| workspace.apps.iter().any(|app| app.id == app_id))?
            .room;
        self.newest_app_state_in_room(room, app_id)
    }

    pub fn newest_app_state_in_room(
        &self,
        room: RoomId,
        app_id: AppInstanceId,
    ) -> Option<&AppInstanceState> {
        let state = self
            .snapshots
            .values()
            .flat_map(|snapshot| snapshot.workspaces.iter())
            .filter(|workspace| workspace.room == room)
            .flat_map(|workspace| workspace.apps.iter())
            .filter(|app| app.id == app_id)
            .max_by_key(|app| app.stamp.ordering_key())?;
        let removed = self
            .snapshots
            .values()
            .flat_map(|snapshot| &snapshot.tombstones)
            .filter(|tombstone| tombstone.room == room && tombstone.id == app_id)
            .max_by_key(|tombstone| tombstone.stamp.ordering_key());
        removed
            .is_none_or(|tombstone| state.stamp.is_newer_than(&tombstone.stamp))
            .then_some(state)
    }

    /// Highest Lamport value observed anywhere in the merged checkpoint.
    pub fn observed_counter(&self) -> u64 {
        self.snapshots
            .values()
            .flat_map(|snapshot| {
                std::iter::once(snapshot.stamp.counter)
                    .chain(snapshot.tombstones.iter().map(|item| item.stamp.counter))
            })
            .max()
            .unwrap_or(0)
    }

    /// Recover a durable local clock from both the scalar checkpoint and the
    /// signed snapshots it contains. A torn/partially migrated scalar must
    /// never move a device backwards and make peers reject its next update.
    pub fn recover_counter(&self, persisted_counter: u64) -> u64 {
        persisted_counter.max(self.observed_counter())
    }

    fn validate(&self) -> Result<(), ContinuityError> {
        if self.snapshots.len() > MAX_DEVICES
            || self
                .snapshots
                .values()
                .map(snapshot_resume_bytes)
                .sum::<usize>()
                > MAX_TOTAL_RESUME_BYTES
        {
            return Err(ContinuityError::SnapshotTooLarge);
        }
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

/// Decode a Postcard checkpoint.
///
/// # Errors
///
/// Returns [`ContinuityError::InvalidCheckpoint`] when the bytes are not a
/// valid checkpoint.
pub fn decode_checkpoint(bytes: &[u8]) -> Result<PersonalContinuity, ContinuityError> {
    postcard::from_bytes::<PersonalContinuity>(bytes)
        .map_err(|_| ContinuityError::InvalidCheckpoint)
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
    #[error("resume state does not match a host-owned portable schema")]
    InvalidResumePayload,
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
    #[error("app tombstone is invalid, duplicated, or conflicts with live state")]
    InvalidOrDuplicateTombstone,
    #[error("focus does not name a captured app in its workspace")]
    InvalidFocus,
    #[error("two different states claim the same device counter")]
    SameCounterConflict,
    #[error("continuation snapshot exceeds bounded retention limits")]
    SnapshotTooLarge,
    #[error("continuation checkpoint is malformed")]
    InvalidCheckpoint,
}

fn snapshot_resume_bytes(snapshot: &DeviceContinuationSnapshot) -> usize {
    snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.apps)
        .filter_map(|app| app.resume.as_ref())
        .map(|resume| resume.bytes().len())
        .sum()
}

fn focus_from_tree(
    tree: &WorkspaceTree,
    workspaces: &[WorkspaceContinuation],
) -> Option<DeviceFocus> {
    let workspace = tree.desktop.active_workspace()?;
    let room = workspace.room;
    let captured = workspaces.iter().find(|entry| entry.room == room)?;
    let strip_id = tree.desktop.active_strip.get(&workspace.id)?;
    let group_id = tree.desktop.active_group.get(strip_id)?;
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
    use std::collections::{BTreeSet, HashMap};

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
            LaunchIntent::app("so.tilo.browser").unwrap(),
            Some(
                ResumeMetadata::new(
                    1,
                    ResumeSensitivity::Syncable,
                    br#"{"url":"https://example.test/page/4"}"#.to_vec(),
                )
                .unwrap(),
            ),
            ContinuityStamp {
                counter,
                device: device.clone(),
            },
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_snapshots_keep_per_device_focus_and_deterministic_winners() {
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

        two_devices_keep_their_own_focus();
        newest_same_app_checkpoint_has_a_deterministic_winner();
        personal_continuity_round_trips_as_json_and_postcard();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_capture_redacts_paths_ids_and_sensitive_resume_metadata() {
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

        fn file_launch_is_reference_and_thumbnail_hash_only() {
            let intent =
                LaunchIntent::file_reference("record:book-42", Some([3; 32]), Some([4; 32]))
                    .unwrap();
            let encoded = serde_json::to_string(&intent).unwrap();
            assert!(encoded.contains("record:book-42"));
            assert!(encoded.contains("thumbnail_hash"));
            assert!(!encoded.contains("/home/"));
            assert!(!encoded.contains("manifest"));
            assert!(!encoded.contains("thumbnail_bytes"));
        }

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
                desktop: crate::shell::state::DesktopTree {
                    workspaces: vec![workspace],
                    active: workspace_id,
                    active_strip,
                    active_group,
                    ..Default::default()
                },
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

        fn does_not_echo_a_remote_file_placeholder_after_its_source_closes() {
            let workspace_id = WorkspaceId(8);
            let strip_id = StripId(9);
            let room = room(ROOM_A);
            let mut workspace = Workspace::new(workspace_id, "From another device");
            workspace.room = room;
            workspace.strips = vec![Strip {
                id: strip_id,
                name: "Available elsewhere".into(),
                window_groups: vec![WindowGroup {
                    id: 10,
                    tabs: vec![Tab {
                        id: 81,
                        title: "File".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::FileExplorer,
                            label: "Available on another device".into(),
                        },
                    }],
                    active_tab: 81,
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            }];
            let mut continuation_tabs = BTreeSet::new();
            continuation_tabs.insert(81);
            let mut continuation_file_references = HashMap::new();
            continuation_file_references.insert(
                81,
                LaunchIntent::file_reference("record:remote-file", Some([7; 32]), None).unwrap(),
            );
            let tree = WorkspaceTree {
                desktop: crate::shell::state::DesktopTree {
                    workspaces: vec![workspace],
                    active: workspace_id,
                    ..Default::default()
                },
                continuation_tabs,
                continuation_file_references,
                ..WorkspaceTree::default()
            };

            let recaptured = DeviceContinuationSnapshot::capture(&tree, device("desktop-01"), 2);
            assert!(recaptured.snapshot.workspaces[0].apps.is_empty());
        }

        rejects_sensitive_invalid_and_pathful_resume_metadata();
        file_launch_is_reference_and_thumbnail_hash_only();
        captures_safe_tabs_and_redacts_foreign_surface_ids();
        does_not_echo_a_remote_file_placeholder_after_its_source_closes();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_tombstones_and_size_limits_defeat_stale_offers_without_restamping() {
        fn takeover_advances_observed_clock_and_tombstone_defeats_old_source_offer() {
            let room = room(ROOM_A);
            let laptop = device("laptop");
            let desktop = device("desktop");
            let source = DeviceContinuationSnapshot::new(
                laptop.clone(),
                100,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(77, &laptop, 100)],
                }],
            )
            .unwrap();

            let mut desktop_clock = 2;
            source.stamp.advance_clock(&mut desktop_clock);
            assert_eq!(desktop_clock, 101);
            let claimed = DeviceContinuationSnapshot::new(
                desktop.clone(),
                desktop_clock,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(77, &desktop, desktop_clock)],
                }],
            )
            .unwrap();
            let mut closed = DeviceContinuationSnapshot::new(
                desktop,
                102,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: Vec::new(),
                }],
            )
            .unwrap();
            closed.record_closures_from(&claimed).unwrap();

            let mut merged = PersonalContinuity::default();
            merged.upsert(source).unwrap();
            merged.upsert(closed).unwrap();
            assert!(merged.newest_app_state(AppInstanceId(77)).is_none());
            assert_eq!(merged.observed_counter(), 102);
            assert_eq!(merged.recover_counter(1), 102);
        }

        fn rejects_oversized_cumulative_resume_state_before_mutating_collection() {
            let device = device("oversized-device");
            let mut apps = Vec::new();
            for id in 0..100 {
                let mut app = safe_app(id, &device, 1);
                let url = format!(
                    "https://example.test/?q={}",
                    "x".repeat(MAX_RESUME_BYTES - 64)
                );
                app.resume = Some(
                    ResumeMetadata::new(
                        1,
                        ResumeSensitivity::Syncable,
                        serde_json::to_vec(&serde_json::json!({ "url": url })).unwrap(),
                    )
                    .unwrap(),
                );
                apps.push(app);
            }
            let snapshot = DeviceContinuationSnapshot::new(
                device,
                1,
                None,
                vec![WorkspaceContinuation {
                    room: room(ROOM_A),
                    apps,
                }],
            )
            .unwrap();
            let mut continuity = PersonalContinuity::default();
            assert_eq!(
                continuity.upsert(snapshot),
                Err(ContinuityError::SnapshotTooLarge)
            );
            assert!(continuity.is_empty());
        }

        fn near_limit_live_snapshot_can_be_replaced_by_closure_tombstones() {
            let device = device("near-limit-device");
            let mut apps = Vec::new();
            for id in 0..90 {
                let mut app = safe_app(id, &device, 1);
                let url = format!(
                    "https://example.test/?q={}",
                    "x".repeat(MAX_RESUME_BYTES - 64)
                );
                app.resume = Some(
                    ResumeMetadata::new(
                        1,
                        ResumeSensitivity::Syncable,
                        serde_json::to_vec(&serde_json::json!({ "url": url })).unwrap(),
                    )
                    .unwrap(),
                );
                apps.push(app);
            }
            let live = DeviceContinuationSnapshot::new(
                device.clone(),
                1,
                None,
                vec![WorkspaceContinuation {
                    room: room(ROOM_A),
                    apps,
                }],
            )
            .unwrap();
            let mut continuity = PersonalContinuity::default();
            continuity.upsert(live.clone()).unwrap();

            let mut closed = DeviceContinuationSnapshot::new(
                device,
                2,
                None,
                vec![WorkspaceContinuation {
                    room: room(ROOM_A),
                    apps: Vec::new(),
                }],
            )
            .unwrap();
            closed.record_closures_from(&live).unwrap();
            continuity.upsert(closed).unwrap();
            assert!(continuity.newest_app_state(AppInstanceId(0)).is_none());
            assert!(
                postcard::to_allocvec(&continuity).unwrap().len() < MAX_ENCODED_CHECKPOINT_BYTES
            );
        }

        fn unrelated_local_change_does_not_restamp_a_tombstone_past_remote_reopen() {
            let room = room(ROOM_A);
            let desktop = device("desktop");
            let laptop = device("laptop");
            let claimed = DeviceContinuationSnapshot::new(
                desktop.clone(),
                10,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(77, &desktop, 10)],
                }],
            )
            .unwrap();
            let mut closed = DeviceContinuationSnapshot::new(
                desktop.clone(),
                11,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: Vec::new(),
                }],
            )
            .unwrap();
            closed.record_closures_from(&claimed).unwrap();
            let reopened = DeviceContinuationSnapshot::new(
                laptop.clone(),
                12,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(77, &laptop, 12)],
                }],
            )
            .unwrap();
            let mut unrelated = DeviceContinuationSnapshot::new(
                desktop,
                13,
                None,
                vec![WorkspaceContinuation {
                    room,
                    apps: vec![safe_app(88, &device("desktop"), 13)],
                }],
            )
            .unwrap();
            unrelated.record_closures_from(&closed).unwrap();
            assert_eq!(unrelated.tombstones[0].stamp.counter, 11);

            let mut continuity = PersonalContinuity::default();
            continuity.upsert(reopened).unwrap();
            continuity.upsert(unrelated).unwrap();
            assert_eq!(
                continuity
                    .newest_app_state_in_room(room, AppInstanceId(77))
                    .map(|app| app.stamp.counter),
                Some(12)
            );
        }

        takeover_advances_observed_clock_and_tombstone_defeats_old_source_offer();
        rejects_oversized_cumulative_resume_state_before_mutating_collection();
        near_limit_live_snapshot_can_be_replaced_by_closure_tombstones();
        unrelated_local_change_does_not_restamp_a_tombstone_past_remote_reopen();
    }
}
