//! The policy and state shared between one person's certified Tilo devices.
//! Tilo does not equate proximity, Bluetooth pairing or a shared network with trust.
//! The private same-identity connection authenticates and encrypts peers; this module
//! applies one policy model to media, clipboard, roaming preferences and secret
//! references.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fabric::MAX_FABRIC_ACTION_BYTES;
use crate::fabric::{DeviceId, FabricStamp};

/// Locally derived focus identity of a physical gamepad's target. None of
/// these fields are accepted from a remote peer or an untrusted application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadBinding {
    pub app_id: String,
    pub workspace_id: String,
    pub session_id: String,
}

pub const MAX_DEVICES: usize = 64;
pub const MAX_CLIPBOARD_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_ITEM_BYTES: usize = 256 * 1024;
pub const MAX_FABRIC_ACTION_AGE_MS: u64 = 2 * 60 * 1000;
/// Versioned remote-audio wire contract. Unknown versions fail closed instead
/// of being interpreted as raw PCM with potentially incompatible framing.
pub const REMOTE_AUDIO_PROTOCOL_VERSION: u16 = 2;
pub const REMOTE_AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;
pub const REMOTE_AUDIO_CHANNELS: u8 = 2;
pub const REMOTE_AUDIO_FRAME_SAMPLES: u16 = 960;
/// Bytes in one interleaved S16LE sample frame across all channels.
pub const REMOTE_AUDIO_SAMPLE_FRAME_BYTES: usize = REMOTE_AUDIO_CHANNELS as usize * 2;
pub const MAX_REMOTE_AUDIO_FRAME_BYTES: usize =
    REMOTE_AUDIO_FRAME_SAMPLES as usize * REMOTE_AUDIO_CHANNELS as usize * 2;
pub const MAX_REMOTE_AUDIO_SESSIONS: usize = 4;
/// Delay from a sample's capture on the source to its playout on every
/// member: covers capture, the network and its jitter, and the members'
/// device latency.
pub const REMOTE_AUDIO_GROUP_LATENCY_NS: u64 = 120_000_000;
/// Longest group latency a frame may ask for.
pub const MAX_REMOTE_AUDIO_GROUP_LATENCY_NS: u64 = 1_000_000_000;

/// `media_ts` (frames at [`REMOTE_AUDIO_SAMPLE_RATE_HZ`]) as nanoseconds on the
/// source's clock, or `None` past the representable range.
#[must_use]
pub fn remote_audio_media_ns(media_ts: u64) -> Option<u64> {
    let ns = u128::from(media_ts)
        .checked_mul(1_000_000_000)?
        .checked_div(u128::from(REMOTE_AUDIO_SAMPLE_RATE_HZ))?;
    u64::try_from(ns).ok()
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
    /// The device can play as a member of a speaker group.
    pub audio_group: bool,
}

/// A locator usable on another Tilo device. Workspace paths hydrate through
/// Files; stream URLs travel only in an ephemeral exact-target action and are
/// never kept anywhere.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSourceLocator {
    Workspace {
        path: String,
    },
    RemoteStream {
        url: String,
    },
    AppResource {
        uri: String,
    },
    /// Retired: a media handoff names a workspace file or a stream now, and
    /// Nearby Send is the daemon's. Kept in its place so a frame an earlier
    /// build wrote still reads as itself; it resolves nothing.
    RetiredPersonalTransfer {
        transfer_id: [u8; 16],
        file_name: String,
    },
}

/// Bounded destination-side outcome for a requested media handoff. The source
/// remains active until it receives [`PersonalFabricAction::MediaAccepted`],
/// so these failures are safe to surface and retry without reconstructing
/// backend error strings from another device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaHandoffFailure {
    DestinationUnavailable,
    UnsupportedOutput,
    InvalidWorkspace,
    OpenFailed,
    TimedOut,
    Cancelled,
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
    /// Request a shell-owned, live audio route. Local `PipeWire` node identities
    /// never cross the certified-device boundary. Appending wire variants is
    /// intentional: postcard encodes enum discriminants by declaration order.
    RemoteAudioOffer {
        session_id: [u8; 16],
        protocol_version: u16,
        label: String,
        sample_rate_hz: u32,
        channels: u8,
        frame_samples: u16,
    },
    /// Target-local consent result. Approval carries the random capability
    /// required by every subsequent frame and stop message.
    RemoteAudioDecision {
        session_id: [u8; 16],
        accepted: bool,
        authorization: Option<[u8; 16]>,
    },
    /// One bounded S16LE frame for an approved exact-peer session.
    RemoteAudioFrame {
        session_id: [u8; 16],
        authorization: [u8; 16],
        sequence: u64,
        /// Index of the frame's first sample on the source's
        /// `CLOCK_MONOTONIC`, in frames at the stream rate. Consecutive frames
        /// continue exactly; a gap is audio the source never captured.
        media_ts: u64,
        /// Source `CLOCK_MONOTONIC` nanoseconds at which the frame's first
        /// sample plays on every member of the group.
        presentation_ns: u64,
        pcm_s16le: Vec<u8>,
    },
    RemoteAudioStop {
        session_id: [u8; 16],
        authorization: [u8; 16],
    },
    /// Source-side cancellation before target consent has minted an
    /// authorization. Exact authenticated source/session binding is required.
    RemoteAudioCancel {
        session_id: [u8; 16],
    },
    /// Fabric clock probe, sent only while something on the prober holds
    /// this peer's clock in sync. `sent_us` is the prober's
    /// `CLOCK_MONOTONIC` send time in microseconds, echoed back so the
    /// report closes a four-timestamp exchange. Carries nothing else.
    FabricClockProbe {
        nonce: [u8; 16],
        sent_us: u64,
    },
    /// The reporter's receive and reply times for one clock probe, from
    /// which the prober estimates clock offset and skew.
    FabricClockReport {
        report: crate::fabric::clock::ClockReport,
    },
    // Keep additions at the end: postcard uses declaration-order tags, and
    // inserting media outcomes beside MediaAccepted renumbers older actions.
    /// The exact destination could not open the requested session. Reasons
    /// are typed so remote process details never cross the personal fabric.
    MediaRejected {
        request_id: [u8; 16],
        session_id: String,
        failure: MediaHandoffFailure,
    },
    /// Stop waiting for one exact request without affecting playback on the
    /// destination or granting broader control of it.
    MediaCancelled {
        request_id: [u8; 16],
        session_id: String,
    },
    /// Liveness for one approved remote-audio session, sent by both peers
    /// while it is active. A paused source produces no frames, so silence on
    /// the media path alone cannot tell an idle route from a dead link.
    RemoteAudioKeepalive {
        session_id: [u8; 16],
        authorization: [u8; 16],
        /// Sent by the playing device only: whether it has the source's
        /// clock, so the source can show a member still syncing, or why it
        /// cannot play in time.
        playout: Option<crate::fabric::clock::ClockSyncState>,
    },
    // Speaker groups, their consents and their playbacks are carried in the record of the
    // person's devices now: no device sends these, and validation refuses one from an older
    // device. Each holds its tag so the actions after it keep theirs.
    RetiredSpeakerGroupInvite,
    RetiredSpeakerGroupDecision,
    RetiredSpeakerGroupLeave,
    RetiredSpeakerGroupSessionOffer,
    RetiredSpeakerGroupSessionDecision,
    RetiredSpeakerGroupSessionStop,
    /// Play a short sound on one of the target's own outputs so the person
    /// setting up speakers can tell which one it is. Rate-limited there.
    SpeakerIdentify {
        output: crate::fabric::speaker_group::EndpointSelector,
    },
    /// The group's leader, calibrating, asks a member to play the measurement sweep so
    /// its first sample leaves the speaker at `start_ns` on the member's own
    /// `CLOCK_MONOTONIC`. The leader converts its schedule through its estimate of the
    /// member's clock, and refuses to calibrate without one.
    SpeakerSweep {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        start_ns: i64,
    },
    /// The group's leader asks one of the person's devices to let it listen
    /// with `microphone` while calibrating the group. That device's own
    /// compositor asks its user; nothing is recorded until it approves.
    SpeakerListenInvite {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        name: String,
        microphone: crate::fabric::speaker_group::EndpointSelector,
    },
    /// The answer. The durable consent itself is the listener record the
    /// device authors in its own snapshot; this only reports promptly.
    SpeakerListenDecision {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        accepted: bool,
    },
    /// The calibrating leader asks a consenting device to record around one member's
    /// sweep, whose first sample leaves the speaker at `start_ns` on the listening
    /// device's own clock. The listener needs no estimate of its own.
    SpeakerListen {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        sweep: [u8; 16],
        microphone: crate::fabric::speaker_group::EndpointSelector,
        start_ns: i64,
    },
    /// What that microphone heard of the sweep, measured on the device. The
    /// recording itself never crosses the fabric.
    SpeakerHeard {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        sweep: [u8; 16],
        hearing: crate::fabric::speaker_group::SpeakerHearing,
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
    /// Validate transport-bound authorship, target, freshness and payload.
    /// # Errors
    /// A replay-shaped, misaddressed, oversized or semantically invalid action.
    #[allow(
        clippy::too_many_lines,
        reason = "all authenticated action payload rules stay in one exhaustive validator so a new wire variant cannot bypass review"
    )]
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
                if !valid_fabric_identifier(session_id, 256)
                    || !valid_fabric_identifier(app_id, 256)
                    || !valid_fabric_identifier(workspace_id, 64)
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
            }
            | PersonalFabricAction::MediaRejected {
                request_id,
                session_id,
                ..
            }
            | PersonalFabricAction::MediaCancelled {
                request_id,
                session_id,
            } => {
                if request_id == &[0; 16] || !valid_fabric_identifier(session_id, 256) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioOffer {
                session_id,
                protocol_version,
                label,
                sample_rate_hz,
                channels,
                frame_samples,
            } => {
                if session_id == &[0; 16]
                    || *protocol_version != REMOTE_AUDIO_PROTOCOL_VERSION
                    || label.is_empty()
                    || label.len() > 160
                    || label.chars().any(char::is_control)
                    || *sample_rate_hz != REMOTE_AUDIO_SAMPLE_RATE_HZ
                    || *channels != REMOTE_AUDIO_CHANNELS
                    || *frame_samples != REMOTE_AUDIO_FRAME_SAMPLES
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioDecision {
                session_id,
                accepted,
                authorization,
            } => {
                if session_id == &[0; 16]
                    || (*accepted && authorization.is_none_or(|value| value == [0; 16]))
                    || (!*accepted && authorization.is_some())
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioFrame {
                session_id,
                authorization,
                sequence,
                media_ts,
                presentation_ns,
                pcm_s16le,
            } => {
                let latency = remote_audio_media_ns(*media_ts)
                    .and_then(|media_ns| presentation_ns.checked_sub(media_ns));
                if session_id == &[0; 16]
                    || authorization == &[0; 16]
                    || *sequence == 0
                    || latency.is_none_or(|latency| latency > MAX_REMOTE_AUDIO_GROUP_LATENCY_NS)
                    || pcm_s16le.is_empty()
                    || pcm_s16le.len() > MAX_REMOTE_AUDIO_FRAME_BYTES
                    || !pcm_s16le
                        .len()
                        .is_multiple_of(REMOTE_AUDIO_SAMPLE_FRAME_BYTES)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioStop {
                session_id,
                authorization,
            }
            | PersonalFabricAction::RemoteAudioKeepalive {
                session_id,
                authorization,
                ..
            } => {
                if session_id == &[0; 16] || authorization == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RetiredSpeakerGroupInvite
            | PersonalFabricAction::RetiredSpeakerGroupDecision
            | PersonalFabricAction::RetiredSpeakerGroupLeave
            | PersonalFabricAction::RetiredSpeakerGroupSessionOffer
            | PersonalFabricAction::RetiredSpeakerGroupSessionDecision
            | PersonalFabricAction::RetiredSpeakerGroupSessionStop => {
                return Err(FabricError::InvalidAction);
            }
            PersonalFabricAction::SpeakerIdentify { output } => {
                if !output.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerSweep { group, start_ns } => {
                if !group.valid() || *start_ns <= 0 {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerListenInvite {
                group,
                name,
                microphone,
            } => {
                if !group.valid()
                    || !crate::fabric::speaker_group::valid_group_name(name)
                    || !microphone.valid()
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerListen {
                group,
                sweep,
                microphone,
                start_ns,
            } => {
                if !group.valid() || sweep == &[0; 16] || !microphone.valid() || *start_ns <= 0 {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerHeard {
                group,
                sweep,
                hearing,
            } => {
                if !group.valid() || sweep == &[0; 16] || !hearing.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerListenDecision { group, .. } => {
                if !group.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioCancel { session_id } => {
                if session_id == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::FabricClockProbe { nonce, .. } => {
                if nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::FabricClockReport { report } => {
                if !report.valid() {
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
            && (!valid_mime(mime) || mime == ui::clipboard::SECRET_MIME)
        {
            return Err(FabricError::InvalidClipboardMime);
        }
        Ok(())
    }
}

/// Content-addressed clipboard item. Secret fields and primary-selection churn
/// do not construct this type; a caller must explicitly classify a copy as
/// shareable before it reaches the person's private device connection.
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
    /// Construct a content-addressed item the caller has classified as safe to leave the
    /// originating device.
    /// # Errors
    /// An oversized or invalid payload, or a foreign stamp.
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

    /// Whether this is a copy `owner` made, as it made it.
    /// # Errors
    /// A copy another device made, or one whose content is not what its id names.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
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

    /// Add or promote an item, enforcing the bounded history policy.
    /// # Errors
    /// An invalid item, or every history slot pinned.
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

    pub fn remove(&mut self, id: [u8; 32]) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.entries.remove(index);
        true
    }
}

pub(crate) fn valid_fabric_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
}

pub(crate) fn valid_mime(value: &str) -> bool {
    value.len() <= 128
        && value.contains('/')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '+' | '_'))
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
        MediaSourceLocator::RetiredPersonalTransfer {
            transfer_id,
            file_name,
        } => {
            transfer_id != &[0; 16]
                && !file_name.is_empty()
                && file_name.len() <= 1024
                && file_name != "."
                && file_name != ".."
                && !file_name
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FabricError {
    #[error("what a device says of itself exceeds its bound")]
    TooLarge,
    #[error("a value was authored by a different device")]
    ForeignAuthorship,
    #[error("invalid device label")]
    InvalidLabel,
    #[error("invalid identifier")]
    InvalidIdentifier,
    #[error("clipboard item exceeds the sync limit")]
    ClipboardItemTooLarge,
    #[error("clipboard MIME type is invalid or secret-only")]
    InvalidClipboardMime,
    #[error("clipboard content hash does not match its payload")]
    ClipboardHashMismatch,
    #[error("clipboard history is full of pinned entries")]
    ClipboardHistoryFull,
    #[error("a device says one thing twice")]
    DuplicateItem,
    #[error("unknown target device")]
    UnknownDevice,
    #[error("invalid personal-fabric action")]
    InvalidAction,
    #[error("personal-fabric action exceeds its wire bound")]
    ActionTooLarge,
    #[error("invalid speaker group")]
    InvalidSpeakerGroup,
    #[error("that device cannot play in this speaker group")]
    IneligibleSpeakerMember,
    #[error("invalid disk-unlock enrolment or setting")]
    InvalidUnlockEnrolment,
}

impl From<tilo_unlock_protocol::published::PublishedError> for FabricError {
    fn from(error: tilo_unlock_protocol::published::PublishedError) -> Self {
        use tilo_unlock_protocol::published::PublishedError;
        match error {
            PublishedError::TooLarge => Self::TooLarge,
            PublishedError::ForeignAuthorship => Self::ForeignAuthorship,
            PublishedError::DuplicateItem => Self::DuplicateItem,
            PublishedError::InvalidUnlockEnrolment => Self::InvalidUnlockEnrolment,
        }
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
