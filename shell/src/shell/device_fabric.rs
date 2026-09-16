//! Policy and convergent state for one person's certified qOS devices.
//!
//! qOS does not equate proximity, Bluetooth pairing, or a shared network with
//! trust. The private same-identity lane authenticates and encrypts peers; this
//! module applies one policy model to media, clipboard, roaming preferences,
//! secret references and cross-screen input.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::shell::personal_continuity::DeviceId;

pub const FABRIC_SCHEMA_VERSION: u16 = 1;
pub const MAX_DEVICES: usize = 64;
pub const MAX_MEDIA_SESSIONS: usize = 64;
pub const MAX_CLIPBOARD_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_ITEM_BYTES: usize = 256 * 1024;
pub const MAX_ROAMING_SETTINGS: usize = 2048;
pub const MAX_SETTING_BYTES: usize = 64 * 1024;
pub const MAX_WIFI_CREDENTIALS: usize = 256;
pub const MAX_SEALED_SECRET_BYTES: usize = 16 * 1024;
pub const MAX_INPUT_LEASE_MS: u64 = 15 * 60 * 1000;
/// Aggregate state is metadata and small clipboard payloads, never media data.
pub const MAX_FABRIC_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FABRIC_ACTION_BYTES: usize = 64 * 1024;
pub const MAX_FABRIC_ACTION_AGE_MS: u64 = 2 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FabricStamp {
    pub counter: u64,
    pub device: DeviceId,
}

impl FabricStamp {
    pub fn is_newer_than(&self, other: &Self) -> bool {
        (self.counter, self.device.as_str()) > (other.counter, other.device.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Desktop,
    Laptop,
    Tablet,
    Phone,
    Television,
    Speaker,
    Server,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "wire capabilities are an independently negotiable set, not mutually exclusive state"
)]
pub struct FabricCapabilities {
    pub display: bool,
    pub pointer: bool,
    pub keyboard: bool,
    pub audio_output: bool,
    pub video_output: bool,
    pub clipboard: bool,
    pub roaming_settings: bool,
    pub secret_store: bool,
    pub app_continuation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePresence {
    pub device: DeviceId,
    pub label: String,
    pub kind: DeviceKind,
    pub capabilities: FabricCapabilities,
    pub online: bool,
    pub last_seen_unix_ms: u64,
    pub stamp: FabricStamp,
}

impl DevicePresence {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.device != owner || &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        if self.label.is_empty()
            || self.label.len() > 128
            || self.label.chars().any(char::is_control)
        {
            return Err(FabricError::InvalidLabel);
        }
        Ok(())
    }
}

/// A logical player, never a local `PipeWire` node or file path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSession {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub content_id: String,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub playing: bool,
    pub has_video: bool,
    pub output_device: DeviceId,
    pub stamp: FabricStamp,
}

impl MediaSession {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        if [&self.id, &self.app_id, &self.content_id]
            .into_iter()
            .any(|value| !valid_identifier(value, 256))
        {
            return Err(FabricError::InvalidIdentifier);
        }
        if self.title.len() > 512 || self.title.chars().any(char::is_control) {
            return Err(FabricError::InvalidLabel);
        }
        if self
            .duration_ms
            .is_some_and(|duration| self.position_ms > duration)
        {
            return Err(FabricError::InvalidMediaPosition);
        }
        Ok(())
    }
}

/// Atomic stop-here/start-there instruction for the media runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaHandoff {
    pub session_id: String,
    pub source: DeviceId,
    pub target: DeviceId,
    pub position_ms: u64,
    pub resume_playing: bool,
    pub pause_source: bool,
}

/// A locator usable on another qOS device. Workspace paths hydrate through
/// Files; stream URLs travel only in an ephemeral exact-target action and are
/// never persisted in the fabric snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSourceLocator {
    Workspace { path: String },
    RemoteStream { url: String },
    AppResource { uri: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalFabricAction {
    ResumeMedia {
        session_id: String,
        app_id: String,
        workspace_id: String,
        title: String,
        locator: MediaSourceLocator,
        position_ms: u64,
        resume_playing: bool,
        has_video: bool,
    },
    MediaAccepted {
        request_id: [u8; 16],
        session_id: String,
    },
    /// Ask the receiving device to author this placement in its next
    /// snapshot. The sender never forges another device's durable state.
    SetDisplayPlacement {
        rect: DisplayRect,
    },
    /// A target-issued, short-lived permission delivered to its controller.
    InputLeaseGranted {
        lease: InputLease,
    },
    InputLeaseRevoked {
        nonce: [u8; 16],
    },
    /// One non-durable input event, accepted only while the target still
    /// retains the matching lease it issued.
    Input {
        lease_nonce: [u8; 16],
        event: FabricInputEvent,
    },
}

/// Ephemeral action envelope authenticated by the exact QUIC device route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalFabricActionEnvelope {
    pub request_id: [u8; 16],
    pub source: DeviceId,
    pub target: DeviceId,
    pub issued_unix_ms: u64,
    pub action: PersonalFabricAction,
}

impl PersonalFabricActionEnvelope {
    /// Validate transport-bound authorship, target, freshness, and payload.
    ///
    /// # Errors
    ///
    /// Returns an error for a replay-shaped, misaddressed, oversized, or
    /// semantically invalid action.
    pub fn validate_at(
        &self,
        transport_source: &DeviceId,
        local_target: &DeviceId,
        now_unix_ms: u64,
    ) -> Result<(), FabricError> {
        if self.request_id == [0; 16]
            || &self.source != transport_source
            || &self.target != local_target
            || self.source == self.target
            || self.issued_unix_ms > now_unix_ms.saturating_add(30_000)
            || now_unix_ms.saturating_sub(self.issued_unix_ms) > MAX_FABRIC_ACTION_AGE_MS
        {
            return Err(FabricError::InvalidAction);
        }
        match &self.action {
            PersonalFabricAction::ResumeMedia {
                session_id,
                app_id,
                workspace_id,
                title,
                locator,
                ..
            } => {
                if !valid_identifier(session_id, 256)
                    || !valid_identifier(app_id, 256)
                    || !valid_identifier(workspace_id, 64)
                    || title.len() > 512
                    || title.chars().any(char::is_control)
                    || !valid_media_locator(locator)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::MediaAccepted {
                request_id,
                session_id,
            } => {
                if request_id == &[0; 16] || !valid_identifier(session_id, 256) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SetDisplayPlacement { rect } => {
                if !valid_display_rect(rect) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::InputLeaseGranted { lease } => {
                if &lease.target != transport_source
                    || &lease.controller != local_target
                    || lease.nonce == [0; 16]
                    || lease.validate_at(now_unix_ms).is_err()
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::InputLeaseRevoked { nonce } => {
                if nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::Input { lease_nonce, event } => {
                if lease_nonce == &[0; 16] || !event.validate() {
                    return Err(FabricError::InvalidAction);
                }
            }
        }
        let bytes = postcard::to_allocvec(self).map_err(|_| FabricError::InvalidAction)?;
        if bytes.len() > MAX_FABRIC_ACTION_BYTES {
            return Err(FabricError::ActionTooLarge);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardPayload {
    Text(String),
    Binary { mime: String, bytes: Vec<u8> },
}

impl ClipboardPayload {
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text(value) => value.len(),
            Self::Binary { bytes, .. } => bytes.len(),
        }
    }

    fn validate(&self) -> Result<(), FabricError> {
        if self.byte_len() > MAX_CLIPBOARD_ITEM_BYTES {
            return Err(FabricError::ClipboardItemTooLarge);
        }
        if let Self::Binary { mime, .. } = self
            && (!valid_mime(mime) || mime == "application/x-qos-secret")
        {
            return Err(FabricError::InvalidClipboardMime);
        }
        Ok(())
    }
}

/// Content-addressed clipboard item. Secret fields and primary-selection churn
/// do not construct this type; a caller must explicitly classify a copy as
/// shareable before it reaches the person's private device lane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: [u8; 32],
    pub payload: ClipboardPayload,
    pub origin: DeviceId,
    pub created_unix_ms: u64,
    pub pinned: bool,
    pub stamp: FabricStamp,
}

impl ClipboardEntry {
    /// Construct a content-addressed item after the caller has classified it
    /// as safe to leave the originating device.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized/invalid payload or foreign stamp.
    pub fn shareable(
        payload: ClipboardPayload,
        origin: DeviceId,
        created_unix_ms: u64,
        stamp: FabricStamp,
    ) -> Result<Self, FabricError> {
        payload.validate()?;
        if stamp.device != origin {
            return Err(FabricError::ForeignAuthorship);
        }
        let mut material = Vec::with_capacity(payload.byte_len().saturating_add(128));
        material.extend_from_slice(origin.as_str().as_bytes());
        match &payload {
            ClipboardPayload::Text(value) => {
                material.extend_from_slice(b"text\0");
                material.extend_from_slice(value.as_bytes());
            }
            ClipboardPayload::Binary { mime, bytes } => {
                material.extend_from_slice(mime.as_bytes());
                material.push(0);
                material.extend_from_slice(bytes);
            }
        }
        Ok(Self {
            id: *blake3::hash(&material).as_bytes(),
            payload,
            origin,
            created_unix_ms,
            pinned: false,
            stamp,
        })
    }

    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.origin != owner || &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        let rebuilt = Self::shareable(
            self.payload.clone(),
            self.origin.clone(),
            self.created_unix_ms,
            self.stamp.clone(),
        )?;
        if rebuilt.id != self.id {
            return Err(FabricError::ClipboardHashMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipboardEntry>,
}

impl ClipboardHistory {
    pub fn entries(&self) -> impl Iterator<Item = &ClipboardEntry> {
        self.entries.iter()
    }

    /// Add or promote an item while enforcing the bounded history policy.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid item or when every history slot is pinned.
    pub fn push(&mut self, entry: ClipboardEntry) -> Result<bool, FabricError> {
        entry.payload.validate()?;
        if let Some(index) = self.entries.iter().position(|item| item.id == entry.id) {
            let Some(mut prior) = self.entries.remove(index) else {
                return Ok(false);
            };
            if entry.stamp.is_newer_than(&prior.stamp) {
                prior.stamp = entry.stamp;
                prior.pinned = entry.pinned;
            }
            self.entries.push_front(prior);
            return Ok(false);
        }
        while self.entries.len() >= MAX_CLIPBOARD_ITEMS {
            let Some(index) = self.entries.iter().rposition(|item| !item.pinned) else {
                return Err(FabricError::ClipboardHistoryFull);
            };
            self.entries.remove(index);
        }
        self.entries.push_front(entry);
        Ok(true)
    }

    pub fn set_pinned(&mut self, id: [u8; 32], pinned: bool) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.pinned = pinned;
        true
    }

    pub fn remove(&mut self, id: [u8; 32]) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    pub fn clear_unpinned(&mut self) {
        self.entries.retain(|entry| entry.pinned);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoamingSetting {
    pub key: String,
    pub value: Value,
    pub stamp: FabricStamp,
}

impl RoamingSetting {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner || !valid_setting_key(&self.key) {
            return Err(FabricError::InvalidRoamingSetting);
        }
        if serde_json::to_vec(&self.value)
            .map_err(|_| FabricError::InvalidRoamingSetting)?
            .len()
            > MAX_SETTING_BYTES
        {
            return Err(FabricError::SettingTooLarge);
        }
        Ok(())
    }
}

/// Versioned per-recipient ciphertext envelopes. Plaintext credentials are
/// intentionally impossible to represent in fabric state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiCredential {
    pub network_id: [u8; 32],
    pub ssid: String,
    pub security: String,
    pub sealed_secret: Vec<u8>,
    pub nonce: [u8; 24],
    pub stamp: FabricStamp,
}

impl WifiCredential {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        if self.ssid.is_empty()
            || self.ssid.len() > 128
            || !matches!(
                self.security.as_str(),
                "wpa2" | "wpa3" | "owe" | "enterprise"
            )
            || self.sealed_secret.is_empty()
            || self.sealed_secret.len() > MAX_SEALED_SECRET_BYTES
        {
            return Err(FabricError::InvalidWifiCredential);
        }
        crate::shell::device_fabric_secrets::validate_wifi_envelopes(&self.sealed_secret)
            .map_err(|_| FabricError::InvalidWifiCredential)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl DisplayRect {
    fn contains_y(&self, y: i32) -> bool {
        y >= self.y && y < self.y.saturating_add_unsigned(self.height)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPlacement {
    pub device: DeviceId,
    pub rect: DisplayRect,
    pub scale_milli: u16,
    pub stamp: FabricStamp,
}

impl DisplayPlacement {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.device != owner
            || &self.stamp.device != owner
            || self.rect.width == 0
            || self.rect.height == 0
            || !(500..=4000).contains(&self.scale_milli)
        {
            return Err(FabricError::InvalidDisplayPlacement);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeTransition {
    pub from: DeviceId,
    pub to: DeviceId,
    pub target_x: i32,
    pub target_y: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputLease {
    pub controller: DeviceId,
    pub target: DeviceId,
    pub issued_unix_ms: u64,
    pub expires_unix_ms: u64,
    pub nonce: [u8; 16],
    pub allow_keyboard: bool,
    pub allow_pointer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FabricInputEvent {
    PointerMotion {
        x: i32,
        y: i32,
    },
    PointerButton {
        button: u32,
        pressed: bool,
    },
    Scroll {
        horizontal: i16,
        vertical: i16,
    },
    Key {
        keycode: u32,
        pressed: bool,
        modifiers: u32,
    },
}

impl FabricInputEvent {
    fn validate(self) -> bool {
        match self {
            Self::PointerMotion { x, y } => {
                x.unsigned_abs() <= 1_000_000 && y.unsigned_abs() <= 1_000_000
            }
            Self::PointerButton { button, .. } => (1..=0x3ff).contains(&button),
            Self::Scroll {
                horizontal,
                vertical,
            } => horizontal != 0 || vertical != 0,
            Self::Key {
                keycode, modifiers, ..
            } => keycode <= 0x2ff && modifiers <= 0xffff,
        }
    }
}

impl InputLease {
    /// Whether this permission binds exactly this authenticated controller to
    /// this target device.
    pub fn binds(&self, controller: &DeviceId, target: &DeviceId) -> bool {
        &self.controller == controller && &self.target == target
    }

    /// Check the target-issued lease at the moment an input event is routed.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease shape is invalid or it has expired.
    pub fn validate_at(&self, now_unix_ms: u64) -> Result<(), FabricError> {
        if self.controller == self.target
            || self.expires_unix_ms <= self.issued_unix_ms
            || self.expires_unix_ms.saturating_sub(self.issued_unix_ms) > MAX_INPUT_LEASE_MS
            || !(self.allow_keyboard || self.allow_pointer)
        {
            return Err(FabricError::InvalidInputLease);
        }
        if now_unix_ms > self.expires_unix_ms {
            return Err(FabricError::ExpiredInputLease);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFabricSnapshot {
    pub schema_version: u16,
    pub owner: DeviceId,
    pub presence: DevicePresence,
    pub media: Vec<MediaSession>,
    pub clipboard: Vec<ClipboardEntry>,
    pub settings: Vec<RoamingSetting>,
    pub wifi: Vec<WifiCredential>,
    pub display: Option<DisplayPlacement>,
}

impl DeviceFabricSnapshot {
    /// Validate authorship, size, uniqueness, and feature-specific invariants.
    ///
    /// # Errors
    ///
    /// Returns the first violated fabric invariant.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.schema_version != FABRIC_SCHEMA_VERSION {
            return Err(FabricError::UnsupportedSchema(self.schema_version));
        }
        if self.media.len() > MAX_MEDIA_SESSIONS
            || self.clipboard.len() > MAX_CLIPBOARD_ITEMS
            || self.settings.len() > MAX_ROAMING_SETTINGS
            || self.wifi.len() > MAX_WIFI_CREDENTIALS
        {
            return Err(FabricError::SnapshotTooLarge);
        }
        self.presence.validate(&self.owner)?;
        for item in &self.media {
            item.validate(&self.owner)?;
        }
        for item in &self.clipboard {
            item.validate(&self.owner)?;
        }
        for item in &self.settings {
            item.validate(&self.owner)?;
        }
        for item in &self.wifi {
            item.validate(&self.owner)?;
        }
        if let Some(display) = &self.display {
            display.validate(&self.owner)?;
        }
        unique(self.media.iter().map(|item| item.id.as_str()))?;
        unique(self.clipboard.iter().map(|item| item.id))?;
        unique(self.settings.iter().map(|item| item.key.as_str()))?;
        unique(self.wifi.iter().map(|item| item.network_id))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFabric {
    snapshots: BTreeMap<DeviceId, DeviceFabricSnapshot>,
}

impl DeviceFabric {
    pub fn snapshots(&self) -> &BTreeMap<DeviceId, DeviceFabricSnapshot> {
        &self.snapshots
    }

    /// Highest logical counter observed in any authenticated record.
    pub fn max_counter(&self) -> u64 {
        self.snapshots
            .values()
            .flat_map(|snapshot| {
                std::iter::once(snapshot.presence.stamp.counter)
                    .chain(snapshot.media.iter().map(|item| item.stamp.counter))
                    .chain(snapshot.clipboard.iter().map(|item| item.stamp.counter))
                    .chain(snapshot.settings.iter().map(|item| item.stamp.counter))
                    .chain(snapshot.wifi.iter().map(|item| item.stamp.counter))
                    .chain(snapshot.display.iter().map(|item| item.stamp.counter))
            })
            .max()
            .unwrap_or(0)
    }

    /// Encode a validated, bounded aggregate for durable local storage.
    ///
    /// # Errors
    ///
    /// Returns an error when retained state is invalid or exceeds its wire bound.
    pub fn encode_checkpoint(&self) -> Result<Vec<u8>, FabricError> {
        if self.snapshots.len() > MAX_DEVICES {
            return Err(FabricError::TooManyDevices);
        }
        for (owner, snapshot) in &self.snapshots {
            if owner != &snapshot.owner {
                return Err(FabricError::ForeignAuthorship);
            }
            snapshot.validate()?;
        }
        let bytes = postcard::to_allocvec(self).map_err(|_| FabricError::InvalidCheckpoint)?;
        if bytes.len() > MAX_FABRIC_CHECKPOINT_BYTES {
            return Err(FabricError::CheckpointTooLarge);
        }
        Ok(bytes)
    }

    /// Decode and validate an aggregate before it reaches live fabric state.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, oversized, or invariant-violating input.
    pub fn decode_checkpoint(bytes: &[u8]) -> Result<Self, FabricError> {
        if bytes.len() > MAX_FABRIC_CHECKPOINT_BYTES {
            return Err(FabricError::CheckpointTooLarge);
        }
        let (decoded, remaining): (Self, &[u8]) =
            postcard::take_from_bytes(bytes).map_err(|_| FabricError::InvalidCheckpoint)?;
        if !remaining.is_empty() {
            return Err(FabricError::InvalidCheckpoint);
        }
        // Re-encoding applies every per-device validation and also rejects a
        // malicious aggregate that expands beyond the in-memory device bound.
        let _ = decoded.encode_checkpoint()?;
        Ok(decoded)
    }

    /// Merge one device's newest self-authored snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid state, capacity exhaustion, or an equal
    /// revision carrying conflicting content.
    pub fn upsert(&mut self, snapshot: DeviceFabricSnapshot) -> Result<bool, FabricError> {
        snapshot.validate()?;
        if !self.snapshots.contains_key(&snapshot.owner) && self.snapshots.len() >= MAX_DEVICES {
            return Err(FabricError::TooManyDevices);
        }
        match self.snapshots.get(&snapshot.owner) {
            Some(current)
                if !snapshot
                    .presence
                    .stamp
                    .is_newer_than(&current.presence.stamp) =>
            {
                if snapshot == *current {
                    Ok(false)
                } else if snapshot.presence.stamp == current.presence.stamp {
                    Err(FabricError::ConflictingRevision)
                } else {
                    Ok(false)
                }
            }
            _ => {
                self.snapshots.insert(snapshot.owner.clone(), snapshot);
                Ok(true)
            }
        }
    }

    pub fn online_devices(&self) -> impl Iterator<Item = &DevicePresence> {
        self.snapshots
            .values()
            .map(|snapshot| &snapshot.presence)
            .filter(|device| device.online)
    }

    /// Prepare an atomic output transfer for a logical media session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session/device is unknown or the online
    /// target lacks the required media capabilities.
    pub fn handoff_media(
        &self,
        session_id: &str,
        target: &DeviceId,
    ) -> Result<MediaHandoff, FabricError> {
        let (source, session) = self
            .snapshots
            .iter()
            .flat_map(|(device, snapshot)| snapshot.media.iter().map(move |item| (device, item)))
            .filter(|(_, item)| item.id == session_id)
            .max_by(|(_, a), (_, b)| a.stamp.cmp(&b.stamp))
            .ok_or(FabricError::UnknownMediaSession)?;
        let destination = self
            .snapshots
            .get(target)
            .ok_or(FabricError::UnknownDevice)?;
        let caps = destination.presence.capabilities;
        if !destination.presence.online
            || !caps.audio_output
            || (session.has_video && !caps.video_output)
        {
            return Err(FabricError::IncompatibleMediaTarget);
        }
        Ok(MediaHandoff {
            session_id: session.id.clone(),
            source: source.clone(),
            target: target.clone(),
            position_ms: session.position_ms,
            resume_playing: session.playing,
            pause_source: source != target,
        })
    }

    pub fn cross_right_edge(&self, from: &DeviceId, local_y: i32) -> Option<EdgeTransition> {
        let source = self.snapshots.get(from)?.display.as_ref()?;
        let max_local_y = i32::try_from(source.rect.height)
            .unwrap_or(i32::MAX)
            .saturating_sub(1);
        let global_y = source.rect.y.saturating_add(local_y.clamp(0, max_local_y));
        let source_right = source.rect.x.saturating_add_unsigned(source.rect.width);
        let target = self
            .snapshots
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .filter(|snapshot| {
                let caps = snapshot.presence.capabilities;
                caps.display && caps.pointer && snapshot.owner != *from
            })
            .filter_map(|snapshot| snapshot.display.as_ref())
            .filter(|display| display.rect.x == source_right && display.rect.contains_y(global_y))
            .min_by_key(|display| display.rect.y)?;
        Some(EdgeTransition {
            from: from.clone(),
            to: target.device.clone(),
            target_x: 0,
            target_y: global_y.saturating_sub(target.rect.y),
        })
    }

    /// Resolve each roaming key to its newest authenticated authored record.
    /// Keeping the stamp and author lets runtimes distinguish a remote apply
    /// from a fresh local edit instead of echoing every received preference.
    pub fn merged_setting_records(&self) -> BTreeMap<String, RoamingSetting> {
        let mut winners: BTreeMap<&str, &RoamingSetting> = BTreeMap::new();
        for item in self
            .snapshots
            .values()
            .flat_map(|snapshot| &snapshot.settings)
        {
            if winners
                .get(item.key.as_str())
                .is_none_or(|current| item.stamp.is_newer_than(&current.stamp))
            {
                winners.insert(item.key.as_str(), item);
            }
        }
        winners
            .into_iter()
            .map(|(key, item)| (key.to_string(), item.clone()))
            .collect()
    }

    pub fn merged_settings(&self) -> BTreeMap<String, Value> {
        self.merged_setting_records()
            .into_iter()
            .map(|(key, item)| (key, item.value))
            .collect()
    }

    pub fn merged_wifi(&self) -> BTreeMap<[u8; 32], WifiCredential> {
        let mut winners = BTreeMap::<[u8; 32], WifiCredential>::new();
        for item in self.snapshots.values().flat_map(|snapshot| &snapshot.wifi) {
            if winners
                .get(&item.network_id)
                .is_none_or(|current| item.stamp.is_newer_than(&current.stamp))
            {
                winners.insert(item.network_id, item.clone());
            }
        }
        winners
    }

    pub fn merged_clipboard(&self) -> ClipboardHistory {
        let mut items: Vec<_> = self
            .snapshots
            .values()
            .flat_map(|snapshot| snapshot.clipboard.iter().cloned())
            .collect();
        items.sort_by(|a, b| (b.created_unix_ms, &b.stamp).cmp(&(a.created_unix_ms, &a.stamp)));
        let mut history = ClipboardHistory::default();
        for item in items {
            if history.push(item).is_err() {
                break;
            }
        }
        history
    }
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
}

fn valid_mime(value: &str) -> bool {
    value.len() <= 128
        && value.contains('/')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '+' | '_'))
}

fn valid_display_rect(rect: &DisplayRect) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect.width <= 100_000
        && rect.height <= 100_000
        && rect.x.unsigned_abs() <= 1_000_000
        && rect.y.unsigned_abs() <= 1_000_000
}

fn valid_media_locator(locator: &MediaSourceLocator) -> bool {
    match locator {
        MediaSourceLocator::Workspace { path } => {
            !path.is_empty()
                && path.len() <= 4096
                && std::path::Path::new(path).is_relative()
                && std::path::Path::new(path).components().all(|component| {
                    matches!(
                        component,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                })
        }
        MediaSourceLocator::RemoteStream { url } => {
            let Some(rest) = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
            else {
                return false;
            };
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            !authority.is_empty()
                && !authority.contains('@')
                && url.len() <= 8192
                && !url
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte == b'\\')
        }
        MediaSourceLocator::AppResource { uri } => {
            let Some((scheme, rest)) = uri.split_once(':') else {
                return false;
            };
            !scheme.is_empty()
                && scheme.len() <= 64
                && scheme.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_alphabetic()
                        || (index > 0 && matches!(byte, b'0'..=b'9' | b'+' | b'-' | b'.'))
                })
                && !rest.is_empty()
                && uri.len() <= 8192
                && !uri.chars().any(char::is_control)
        }
    }
}

fn valid_setting_key(key: &str) -> bool {
    valid_identifier(key, 256)
        && key.starts_with("qos.")
        && ![
            "qos.device.",
            "qos.display.hardware.",
            "qos.network.interface.",
            "qos.storage.mount.",
            "qos.security.local_credential.",
        ]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

fn unique<T: Ord>(items: impl IntoIterator<Item = T>) -> Result<(), FabricError> {
    let mut seen = BTreeSet::new();
    if items.into_iter().all(|item| seen.insert(item)) {
        Ok(())
    } else {
        Err(FabricError::DuplicateItem)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FabricError {
    #[error("unsupported device-fabric schema {0}")]
    UnsupportedSchema(u16),
    #[error("the snapshot exceeds a device-fabric bound")]
    SnapshotTooLarge,
    #[error("the device-fabric checkpoint exceeds its transport bound")]
    CheckpointTooLarge,
    #[error("invalid device-fabric checkpoint")]
    InvalidCheckpoint,
    #[error("a value was authored by a different device")]
    ForeignAuthorship,
    #[error("invalid device label")]
    InvalidLabel,
    #[error("invalid identifier")]
    InvalidIdentifier,
    #[error("invalid media position")]
    InvalidMediaPosition,
    #[error("clipboard item exceeds the sync limit")]
    ClipboardItemTooLarge,
    #[error("clipboard MIME type is invalid or secret-only")]
    InvalidClipboardMime,
    #[error("clipboard content hash does not match its payload")]
    ClipboardHashMismatch,
    #[error("clipboard history is full of pinned entries")]
    ClipboardHistoryFull,
    #[error("invalid roaming setting")]
    InvalidRoamingSetting,
    #[error("roaming setting exceeds the sync limit")]
    SettingTooLarge,
    #[error("invalid encrypted Wi-Fi credential")]
    InvalidWifiCredential,
    #[error("invalid display placement")]
    InvalidDisplayPlacement,
    #[error("invalid input lease")]
    InvalidInputLease,
    #[error("input lease has expired")]
    ExpiredInputLease,
    #[error("duplicate item in a device snapshot")]
    DuplicateItem,
    #[error("too many devices are enrolled")]
    TooManyDevices,
    #[error("same device revision carried conflicting state")]
    ConflictingRevision,
    #[error("unknown media session")]
    UnknownMediaSession,
    #[error("unknown target device")]
    UnknownDevice,
    #[error("target device cannot play this media")]
    IncompatibleMediaTarget,
    #[error("invalid personal-fabric action")]
    InvalidAction,
    #[error("personal-fabric action exceeds its wire bound")]
    ActionTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }
    fn stamp(device: &str, counter: u64) -> FabricStamp {
        FabricStamp {
            counter,
            device: id(device),
        }
    }
    fn snapshot(
        device: &str,
        counter: u64,
        x: i32,
        caps: FabricCapabilities,
    ) -> DeviceFabricSnapshot {
        let owner = id(device);
        DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: device.into(),
                kind: DeviceKind::Laptop,
                capabilities: caps,
                online: true,
                last_seen_unix_ms: 10,
                stamp: stamp(device, counter),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: Some(DisplayPlacement {
                device: owner,
                rect: DisplayRect {
                    x,
                    y: 0,
                    width: 1000,
                    height: 800,
                },
                scale_milli: 1000,
                stamp: stamp(device, counter),
            }),
        }
    }

    #[test]
    fn media_handoff_preserves_playhead_and_pauses_source() {
        let caps = FabricCapabilities {
            display: true,
            pointer: true,
            audio_output: true,
            video_output: true,
            ..FabricCapabilities::default()
        };
        let mut laptop = snapshot("laptop", 1, 0, caps);
        laptop.media.push(MediaSession {
            id: "movie-1".into(),
            app_id: "dev.qos.video".into(),
            title: "Film".into(),
            content_id: "record:film".into(),
            position_ms: 42_000,
            duration_ms: Some(90_000),
            playing: true,
            has_video: true,
            output_device: id("laptop"),
            stamp: stamp("laptop", 1),
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(laptop).unwrap();
        fabric.upsert(snapshot("tablet", 1, 1000, caps)).unwrap();
        let transfer = fabric.handoff_media("movie-1", &id("tablet")).unwrap();
        assert_eq!(transfer.position_ms, 42_000);
        assert!(transfer.resume_playing && transfer.pause_source);
    }

    #[test]
    fn video_cannot_be_sent_to_an_audio_only_device() {
        let mut source = snapshot(
            "laptop",
            1,
            0,
            FabricCapabilities {
                audio_output: true,
                video_output: true,
                ..FabricCapabilities::default()
            },
        );
        source.media.push(MediaSession {
            id: "movie".into(),
            app_id: "video".into(),
            title: String::new(),
            content_id: "record:1".into(),
            position_ms: 0,
            duration_ms: None,
            playing: false,
            has_video: true,
            output_device: id("laptop"),
            stamp: stamp("laptop", 1),
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(source).unwrap();
        fabric
            .upsert(snapshot(
                "speaker",
                1,
                1000,
                FabricCapabilities {
                    audio_output: true,
                    ..FabricCapabilities::default()
                },
            ))
            .unwrap();
        assert_eq!(
            fabric.handoff_media("movie", &id("speaker")),
            Err(FabricError::IncompatibleMediaTarget)
        );
    }

    #[test]
    fn clipboard_is_deduplicated_and_bounded() {
        let mut history = ClipboardHistory::default();
        for n in 0..=MAX_CLIPBOARD_ITEMS {
            history
                .push(
                    ClipboardEntry::shareable(
                        ClipboardPayload::Text(format!("item-{n}")),
                        id("laptop"),
                        n as u64,
                        stamp("laptop", n as u64),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(history.entries().count(), MAX_CLIPBOARD_ITEMS);
    }

    #[test]
    fn clipboard_hash_tampering_is_rejected() {
        let mut entry = ClipboardEntry::shareable(
            ClipboardPayload::Text("safe".into()),
            id("phone"),
            1,
            stamp("phone", 1),
        )
        .unwrap();
        entry.payload = ClipboardPayload::Text("changed".into());
        let mut source = snapshot("phone", 1, 0, FabricCapabilities::default());
        source.clipboard.push(entry);
        assert_eq!(source.validate(), Err(FabricError::ClipboardHashMismatch));
    }

    #[test]
    fn local_hardware_settings_do_not_roam() {
        for key in [
            "qos.device.name",
            "qos.display.hardware.edid",
            "qos.network.interface.mac",
            "qos.storage.mount.usb",
        ] {
            let mut source = snapshot("desktop", 1, 0, FabricCapabilities::default());
            source.settings.push(RoamingSetting {
                key: key.into(),
                value: Value::Bool(true),
                stamp: stamp("desktop", 1),
            });
            assert_eq!(source.validate(), Err(FabricError::InvalidRoamingSetting));
        }
    }

    #[test]
    fn newest_roaming_setting_wins_deterministically() {
        let mut old = snapshot("desktop", 1, 0, FabricCapabilities::default());
        old.settings.push(RoamingSetting {
            key: "qos.appearance.dark".into(),
            value: Value::Bool(false),
            stamp: stamp("desktop", 1),
        });
        let mut new = snapshot("laptop", 2, 1000, FabricCapabilities::default());
        new.settings.push(RoamingSetting {
            key: "qos.appearance.dark".into(),
            value: Value::Bool(true),
            stamp: stamp("laptop", 2),
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(old).unwrap();
        fabric.upsert(new).unwrap();
        assert_eq!(
            fabric.merged_settings()["qos.appearance.dark"],
            Value::Bool(true)
        );
    }

    #[test]
    fn wifi_secrets_must_be_sealed() {
        let mut source = snapshot("phone", 1, 0, FabricCapabilities::default());
        source.wifi.push(WifiCredential {
            network_id: [1; 32],
            ssid: "Home".into(),
            security: "wpa3".into(),
            sealed_secret: Vec::new(),
            nonce: [2; 24],
            stamp: stamp("phone", 1),
        });
        assert_eq!(source.validate(), Err(FabricError::InvalidWifiCredential));
    }

    #[test]
    fn adjacent_displays_cross_without_a_vertical_jump() {
        let caps = FabricCapabilities {
            display: true,
            pointer: true,
            ..FabricCapabilities::default()
        };
        let mut fabric = DeviceFabric::default();
        fabric.upsert(snapshot("laptop", 1, 0, caps)).unwrap();
        fabric.upsert(snapshot("tablet", 1, 1000, caps)).unwrap();
        let transition = fabric.cross_right_edge(&id("laptop"), 417).unwrap();
        assert_eq!(transition.to, id("tablet"));
        assert_eq!(transition.target_y, 417);
    }

    #[test]
    fn remote_input_requires_a_short_lived_explicit_lease() {
        let lease = InputLease {
            controller: id("laptop"),
            target: id("tablet"),
            issued_unix_ms: 100,
            expires_unix_ms: 200,
            nonce: [7; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        assert_eq!(lease.validate_at(150), Ok(()));
        assert_eq!(lease.validate_at(201), Err(FabricError::ExpiredInputLease));
    }

    #[test]
    fn equal_revision_conflicts_fail_instead_of_using_arrival_order() {
        let mut fabric = DeviceFabric::default();
        let first = snapshot("laptop", 1, 0, FabricCapabilities::default());
        let mut conflicting = first.clone();
        conflicting.presence.label = "Different".into();
        fabric.upsert(first).unwrap();
        assert_eq!(
            fabric.upsert(conflicting),
            Err(FabricError::ConflictingRevision)
        );
    }

    #[test]
    fn persisted_checkpoint_round_trips_only_validated_state() {
        let mut fabric = DeviceFabric::default();
        fabric
            .upsert(snapshot("laptop", 4, 0, FabricCapabilities::default()))
            .unwrap();
        let encoded = fabric.encode_checkpoint().unwrap();
        assert_eq!(DeviceFabric::decode_checkpoint(&encoded).unwrap(), fabric);

        let mut trailing = encoded;
        trailing.push(0xff);
        assert_eq!(
            DeviceFabric::decode_checkpoint(&trailing),
            Err(FabricError::InvalidCheckpoint)
        );
    }

    #[test]
    fn ephemeral_actions_are_transport_bound_and_fresh() {
        let source = id("source");
        let target = id("target");
        let envelope = PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::ResumeMedia {
                session_id: "player:one".into(),
                app_id: "dev.qos.music".into(),
                workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                title: "Track".into(),
                locator: MediaSourceLocator::Workspace {
                    path: "Music/track.flac".into(),
                },
                position_ms: 42,
                resume_playing: true,
                has_video: false,
            },
        };
        assert_eq!(envelope.validate_at(&source, &target, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("intruder"), &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &id("other-target"), 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &target, 10_000 + MAX_FABRIC_ACTION_AGE_MS + 1),
            Err(FabricError::InvalidAction)
        );
    }

    #[test]
    fn input_lease_must_be_issued_by_the_target_to_its_controller() {
        let controller = id("controller");
        let target = id("target");
        let lease = InputLease {
            controller: controller.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            expires_unix_ms: 20_000,
            nonce: [8; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        let envelope = PersonalFabricActionEnvelope {
            request_id: [9; 16],
            source: target.clone(),
            target: controller.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::InputLeaseGranted { lease },
        };
        assert_eq!(envelope.validate_at(&target, &controller, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("forged-target"), &controller, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    #[test]
    fn input_actions_reject_unbound_or_malformed_events() {
        let source = id("controller");
        let target = id("target");
        for (nonce, event) in [
            ([0; 16], FabricInputEvent::PointerMotion { x: 10, y: 20 }),
            (
                [1; 16],
                FabricInputEvent::PointerButton {
                    button: 0,
                    pressed: true,
                },
            ),
            (
                [1; 16],
                FabricInputEvent::Key {
                    keycode: 0x300,
                    pressed: true,
                    modifiers: 0,
                },
            ),
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [2; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::Input {
                    lease_nonce: nonce,
                    event,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }
    }

    #[test]
    fn media_action_locators_reject_host_paths_and_url_credentials() {
        let source = id("source");
        let target = id("target");
        for locator in [
            MediaSourceLocator::Workspace {
                path: "../private/song.flac".into(),
            },
            MediaSourceLocator::RemoteStream {
                url: "https://secret@example.com/song".into(),
            },
            MediaSourceLocator::AppResource {
                uri: "1music://track/foreign".into(),
            },
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::ResumeMedia {
                    session_id: "player:one".into(),
                    app_id: "dev.qos.music".into(),
                    workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                    title: "Track".into(),
                    locator,
                    position_ms: 0,
                    resume_playing: true,
                    has_video: false,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }
    }
}
