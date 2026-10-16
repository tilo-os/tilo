//! Policy and convergent state for one person's certified Tilo devices.
//!
//! Tilo does not equate proximity, Bluetooth pairing, or a shared network with
//! trust. The private same-identity lane authenticates and encrypts peers; this
//! module applies one policy model to media, clipboard, roaming preferences,
//! secret references and cross-screen input.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::shell::personal_continuity::DeviceId;

pub const FABRIC_SCHEMA_VERSION: u16 = 6;
pub const MAX_DEVICES: usize = 64;
/// More network interfaces than any one device wakes through.
pub const MAX_WAKE_ROUTES: usize = 8;
pub const MAX_MEDIA_SESSIONS: usize = 64;
pub const MAX_CLIPBOARD_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_ITEM_BYTES: usize = 256 * 1024;
pub const MAX_ROAMING_SETTINGS: usize = 2048;
pub const MAX_SETTING_BYTES: usize = settings_store::continuity::MAX_PERSONAL_VALUE_BYTES;
pub const MAX_WIFI_CREDENTIALS: usize = 256;
pub const MAX_SEALED_SECRET_BYTES: usize = 16 * 1024;
pub const MAX_INPUT_LEASE_MS: u64 = 15 * 60 * 1000;
/// Aggregate state is metadata and small clipboard payloads, never media data.
pub const MAX_FABRIC_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FABRIC_ACTION_BYTES: usize = 64 * 1024;
pub const MAX_FABRIC_ACTION_AGE_MS: u64 = 2 * 60 * 1000;
pub const MAX_DISPLAY_MOVE_AGE_MS: u64 = 10_000;
/// Inline fast path only; larger nearby drops belong on the object stream.
pub const MAX_NEARBY_DROP_BYTES: usize = 48 * 1024;
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
    /// The device can play as a member of a speaker group.
    pub audio_group: bool,
}

/// The kind of network interface a device can be woken through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeInterfaceKind {
    Ethernet,
    WiFi,
}

/// An IPv4 network in canonical form: no host bits set beyond the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Ipv4Subnet {
    pub network: [u8; 4],
    pub prefix: u8,
}

impl Ipv4Subnet {
    /// The subnet `address/prefix` lies in, or `None` for a prefix that names
    /// no broadcast domain (wider than /8 or a single host).
    #[must_use]
    pub fn containing(address: std::net::Ipv4Addr, prefix: u8) -> Option<Self> {
        if !(8..=30).contains(&prefix) {
            return None;
        }
        let mask = u32::MAX.checked_shl(32_u32.checked_sub(u32::from(prefix))?)?;
        Some(Self {
            network: (u32::from(address) & mask).to_be_bytes(),
            prefix,
        })
    }

    #[must_use]
    pub fn is_canonical(self) -> bool {
        Self::containing(std::net::Ipv4Addr::from(self.network), self.prefix) == Some(self)
    }

    /// The subnet's directed broadcast address.
    #[must_use]
    pub fn broadcast(self) -> std::net::Ipv4Addr {
        let host = u32::MAX.checked_shr(u32::from(self.prefix)).unwrap_or(0);
        std::net::Ipv4Addr::from(u32::from_be_bytes(self.network) | host)
    }
}

impl std::fmt::Display for Ipv4Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}",
            std::net::Ipv4Addr::from(self.network),
            self.prefix
        )
    }
}

/// One network interface of a device, as the network sees it: what a sibling
/// on the same LAN needs to wake it with a magic packet. Only the person's
/// own devices receive it, inside their certified presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRoute {
    /// The MAC the network sees, which for Wi-Fi is the per-network one.
    pub mac: [u8; 6],
    pub kind: WakeInterfaceKind,
    /// Whether the interface wakes the device on a magic packet.
    pub magic_packet: bool,
    /// The IPv4 subnet the interface was last seen on.
    pub subnet: Option<Ipv4Subnet>,
}

impl WakeRoute {
    fn valid(&self) -> bool {
        // A unicast, non-zero MAC; a multicast one cannot address a NIC.
        self.mac != [0; 6]
            && self.mac.first().is_some_and(|octet| octet & 1 == 0)
            && self.subnet.is_none_or(Ipv4Subnet::is_canonical)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePresence {
    pub device: DeviceId,
    pub label: String,
    pub kind: DeviceKind,
    pub capabilities: FabricCapabilities,
    pub online: bool,
    pub last_seen_unix_ms: u64,
    /// How siblings on the same LAN can wake this device.
    pub wake_routes: Vec<WakeRoute>,
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
        if self.wake_routes.len() > MAX_WAKE_ROUTES {
            return Err(FabricError::SnapshotTooLarge);
        }
        if !self.wake_routes.iter().all(WakeRoute::valid) {
            return Err(FabricError::InvalidWakeRoute);
        }
        unique(self.wake_routes.iter().map(|route| route.mac))
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

/// A locator usable on another Tilo device. Workspace paths hydrate through
/// Files; stream URLs travel only in an ephemeral exact-target action and are
/// never persisted in the fabric snapshot.
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
    /// Random transfer identity resolved only inside the receiving shell's
    /// verified cache. No host path or bearer capability crosses the wire.
    PersonalTransfer {
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
    /// Phase one of accessory handoff. The address is ephemeral routing
    /// metadata; bond keys and the device inventory never enter a snapshot.
    BluetoothHandoffPrepare {
        address: String,
        name: String,
    },
    /// The target confirms whether this same address is already paired there.
    BluetoothHandoffPrepared {
        request_id: [u8; 16],
        address: String,
        ready: bool,
    },
    /// Sent only after the source has disconnected without forgetting its bond.
    BluetoothHandoffCommit {
        request_id: [u8; 16],
        address: String,
    },
    /// Final target outcome. A failure tells the source to reconnect locally.
    BluetoothHandoffFinished {
        request_id: [u8; 16],
        address: String,
        connected: bool,
    },
    /// Source confirms it received a successful finish. Until this arrives,
    /// the target retains rollback state and disconnects on timeout.
    BluetoothHandoffAccepted {
        request_id: [u8; 16],
        address: String,
    },
    /// The target discovered the exact address locally and its compositor
    /// issued this one-shot grant after an explicit local approval.
    BluetoothHandoffPairingOffered {
        request_id: [u8; 16],
        address: String,
        authorization: [u8; 16],
    },
    /// Echo the target-issued grant only after the source released the
    /// accessory. The target consumes it before asking `BlueZ` to pair.
    BluetoothHandoffPairingCommit {
        request_id: [u8; 16],
        address: String,
        authorization: [u8; 16],
    },
    /// Inline fast path of the one Nearby transfer protocol: a file up to
    /// [`MAX_NEARBY_DROP_BYTES`] rides in this action instead of an offer and
    /// a byte stream, saving a round trip. It shares the transfer's consent
    /// queue and integrity check; it is not a second protocol.
    NearbyDrop {
        file_name: String,
        content_hash: [u8; 32],
        bytes: Vec<u8>,
    },
    NearbyDropResult {
        request_id: [u8; 16],
        file_name: String,
        accepted: bool,
    },
    /// Durable object offer. The source path never leaves its device; the ID
    /// addresses an immutable private spool object. A `MediaHandoff` offer is
    /// admitted by the `ResumeMedia` that named its ID, never by Files.
    NearbyTransferOffer {
        transfer_id: [u8; 16],
        name: String,
        kind: crate::shell::nearby_transfer::TransferKind,
        total_bytes: u64,
        content_hash: [u8; 32],
    },
    /// Receiver checkpoint for an exact offer request. The source verifies
    /// the prefix before seeking, otherwise it restarts from byte zero.
    NearbyTransferReady {
        request_id: [u8; 16],
        transfer_id: [u8; 16],
        committed_offset: u64,
        prefix_hash: [u8; 32],
    },
    NearbyTransferCancel {
        request_id: [u8; 16],
        transfer_id: [u8; 16],
    },
    NearbyTransferRestart {
        request_id: [u8; 16],
        transfer_id: [u8; 16],
    },
    NearbyTransferResult {
        request_id: [u8; 16],
        transfer_id: [u8; 16],
        accepted: bool,
    },
    /// Ephemeral, policy-redacted notification state. Authorship comes only
    /// from the authenticated envelope source; the payload names no device.
    NotificationProject {
        notification: crate::notifications::ContinuityNotificationProjection,
    },
    NotificationRetire {
        id: tilo_desktop::notifications::ContinuityNotificationId,
    },
    NotificationDismiss {
        id: tilo_desktop::notifications::ContinuityNotificationId,
    },
    NotificationInvoke {
        id: tilo_desktop::notifications::ContinuityNotificationId,
        action_key: String,
    },
    /// Metadata-only realtime handoff offer. Capture grants, media handles,
    /// and join URLs are deliberately not representable on this wire.
    RealtimeOffer {
        offer: crate::shell::realtime_session::RealtimeFabricOffer,
    },
    RealtimeAccept {
        session_id: crate::shell::realtime_session::RealtimeFabricSessionId,
    },
    RealtimeMediaReady {
        session_id: crate::shell::realtime_session::RealtimeFabricSessionId,
    },
    RealtimeStopSource {
        session_id: crate::shell::realtime_session::RealtimeFabricSessionId,
    },
    RealtimeCancel {
        session_id: crate::shell::realtime_session::RealtimeFabricSessionId,
    },
    /// Request one fixed-profile virtual gamepad on the exact target. The
    /// payload contains semantic controls only: descriptors, HID reports,
    /// serials, and arbitrary evdev codes are not representable.
    GamepadForwardRequest {
        forwarding_id: [u8; 16],
        app_id: String,
        workspace_id: String,
        session_id: String,
        label: String,
    },
    /// Target acknowledgement. A nonzero lease nonce is minted only after
    /// the constrained virtual device exists on that certified target.
    GamepadForwardGrant {
        forwarding_id: [u8; 16],
        accepted: bool,
        lease_nonce: [u8; 16],
    },
    /// One logical event for the exact target-issued lease. Replay and rate
    /// checks are enforced again by the target's privileged broker.
    GamepadForwardEvent {
        forwarding_id: [u8; 16],
        lease_nonce: [u8; 16],
        sequence: u64,
        event: os_actions::ForwardedGamepadEvent,
    },
    GamepadForwardStop {
        forwarding_id: [u8; 16],
        lease_nonce: [u8; 16],
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
    /// Bounded semantic negotiation for a camera, microphone, or document
    /// scanner. PipeWire/V4L2 identities and local app approvals never appear
    /// in this payload.
    RemoteCaptureSignal {
        signal: crate::shell::remote_capture::device::RemoteCaptureSignal,
    },
    /// One bounded media or scanner chunk. The runtime validates it again
    /// against the exact authenticated peer/session/grant/profile binding.
    RemoteCaptureChunk {
        chunk: crate::shell::remote_capture::device::RemoteCaptureChunk,
    },
    /// Shell-owned liveness and RTT probe. The random nonce is bound to the
    /// exact authenticated peer and is never exposed through an app contract.
    DeviceHealthProbe {
        nonce: [u8; 16],
    },
    /// Power is diagnostic routing context, never authorization. Latency is
    /// deliberately absent because the probing compositor measures it.
    DeviceHealthReport {
        report: crate::shell::spatial_calibration::DeviceHealthReport,
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
        report: crate::shell::fabric_clock::ClockReport,
    },
    /// Consent-bound request, grant, sealed credential, or teardown for an
    /// exact certified phone. Plaintext Wi-Fi secrets are not representable.
    Tethering {
        signal: crate::shell::personal_tethering::TetheringSignal,
    },
    /// Abort one exact authenticated Bluetooth transaction. Both peers roll
    /// back only the accessory bound to this request; no inventory or bond
    /// material is disclosed.
    BluetoothHandoffCancel {
        request_id: [u8; 16],
        address: String,
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
        playout: Option<crate::shell::fabric_clock::ClockSyncState>,
    },
    /// Ask one exact member device to join a group. The member's own
    /// compositor asks its user; nothing plays until it approves.
    SpeakerGroupInvite {
        group: crate::shell::speaker_group::SpeakerGroupId,
        name: String,
        output: crate::shell::speaker_group::EndpointSelector,
        role: crate::shell::speaker_group::SpeakerRole,
    },
    /// The member's answer. The durable consent itself is the membership the
    /// member authors in its own snapshot; this only reports promptly.
    SpeakerGroupDecision {
        group: crate::shell::speaker_group::SpeakerGroupId,
        accepted: bool,
    },
    /// The leader removed this member or deleted the group, or the member
    /// left. Either side tears down every session of the group.
    SpeakerGroupLeave {
        group: crate::shell::speaker_group::SpeakerGroupId,
    },
    /// Start one playback session on a consented member. Any of the person's
    /// devices may play to a group; the member admits it only while its own
    /// membership still covers the group as published by the leader.
    SpeakerGroupSessionOffer {
        session_id: [u8; 16],
        group: crate::shell::speaker_group::SpeakerGroupId,
    },
    /// Admission result. Approval carries a fresh random capability bound to
    /// the exact source and session.
    SpeakerGroupSessionDecision {
        session_id: [u8; 16],
        group: crate::shell::speaker_group::SpeakerGroupId,
        authorization: Option<[u8; 16]>,
    },
    SpeakerGroupSessionStop {
        session_id: [u8; 16],
        authorization: Option<[u8; 16]>,
    },
    /// Play a short sound on one of the target's own outputs so the person
    /// setting up speakers can tell which one it is. Rate-limited there.
    SpeakerIdentify {
        output: crate::shell::speaker_group::EndpointSelector,
    },
    /// The group's leader, calibrating, asks a member to play the
    /// measurement sweep so its first sample leaves the speaker at `start_ns`
    /// on the member's own `CLOCK_MONOTONIC`. The leader converts its
    /// schedule through its estimate of the member's clock, and refuses to
    /// calibrate without one.
    SpeakerSweep {
        group: crate::shell::speaker_group::SpeakerGroupId,
        start_ns: i64,
    },
    /// The group's leader asks one of the person's devices to let it listen
    /// with `microphone` while calibrating the group. That device's own
    /// compositor asks its user; nothing is recorded until it approves.
    SpeakerListenInvite {
        group: crate::shell::speaker_group::SpeakerGroupId,
        name: String,
        microphone: crate::shell::speaker_group::EndpointSelector,
    },
    /// The answer. The durable consent itself is the listener record the
    /// device authors in its own snapshot; this only reports promptly.
    SpeakerListenDecision {
        group: crate::shell::speaker_group::SpeakerGroupId,
        accepted: bool,
    },
    /// The calibrating leader asks a consenting device to record around one
    /// member's sweep, whose first sample leaves the speaker at `start_ns` on
    /// the listening device's own `CLOCK_MONOTONIC`. The leader converts its
    /// schedule through the fabric clock estimate it holds of that device, so
    /// the listener needs no estimate of its own.
    SpeakerListen {
        group: crate::shell::speaker_group::SpeakerGroupId,
        sweep: [u8; 16],
        microphone: crate::shell::speaker_group::EndpointSelector,
        start_ns: i64,
    },
    /// What that microphone heard of the sweep, measured on the device. The
    /// recording itself never crosses the fabric.
    SpeakerHeard {
        group: crate::shell::speaker_group::SpeakerGroupId,
        sweep: [u8; 16],
        hearing: crate::shell::speaker_calibration::SpeakerHearing,
    },
    /// The legacy app windows this device shows right now, each at the size
    /// it shows it. The device running a listed window streams that one
    /// window here, and stops once a refresh no longer lists it or the
    /// refreshes stop. An empty list ends every stream to this device.
    /// `decode` is what this device's hardware decoders take, so the host
    /// encodes nothing they would hand back to the CPU.
    WindowViews {
        views: Vec<WindowView>,
        decode: Vec<HardwareDecode>,
    },
    /// Wake `target` with a magic packet on a LAN this device is on, then
    /// watch for it to rejoin. The receiver reads the target's wake routes
    /// from its own copy of the person's fabric, never from the request.
    WakeDevice {
        target: DeviceId,
    },
    /// The waker's account of one [`Self::WakeDevice`] request.
    WakeReport {
        target: DeviceId,
        outcome: WakeOutcome,
    },
    /// The waker asking a device it woke whether it is back.
    WakeProbe {
        nonce: [u8; 16],
    },
    /// The woken device answering a [`Self::WakeProbe`].
    WakeProbeReply {
        nonce: [u8; 16],
    },
    /// Compare-and-set placement; old action tags and checkpoint bytes stay stable.
    SetDisplayPlacementChecked {
        expected: FabricStamp,
        rect: DisplayRect,
        expires_unix_ms: u64,
    },
    /// Probe/acknowledge sequencing only for an existing exact-route lease.
    SpatialInputProtocol {
        lease_nonce: [u8; 16],
        acknowledgement: bool,
    },
    /// Appended optional action; the existing Input tag and bytes do not move.
    SpatialInputSequenced {
        lease_nonce: [u8; 16],
        sequence: u64,
        event: FabricInputEvent,
    },
}

/// What became of one request to wake a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeOutcome {
    /// The magic packet went out on this subnet; the waker now waits.
    Sent { subnet: Ipv4Subnet },
    /// The target is back on the person's device network.
    Rejoined,
    /// The target did not come back within the wait.
    NotRejoined,
    /// The waker shares no LAN with any wakeable interface of the target.
    NotOnTargetLan,
    /// The packet could not be sent.
    SendFailed,
}

/// Upper bound on the windows one [`PersonalFabricAction::WindowViews`] lists:
/// more than a person can see at once on any one screen.
pub const MAX_WINDOW_VIEWS: usize = 32;
/// Largest window side, in logical pixels, a viewer may ask for.
pub const MAX_WINDOW_VIEW_SIDE: u32 = 8192;

/// One legacy app window a device shows: the workspace tab holding it and the
/// tile's size in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowView {
    pub tab: u64,
    pub width: u32,
    pub height: u32,
}

/// One codec a device decodes in hardware, with the largest picture its
/// decoder takes when it has a limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareDecode {
    /// The codec's stream label (`h264`, `vp9`, `av1`, `hevc-main10`).
    pub codec: String,
    pub max: Option<(u32, u32)>,
}

impl HardwareDecode {
    fn valid(&self) -> bool {
        media::StreamCodec::from_wire_label(&self.codec).is_some()
            && self.max.is_none_or(|(width, height)| {
                (2..=MAX_WINDOW_VIEW_SIDE).contains(&width)
                    && (2..=MAX_WINDOW_VIEW_SIDE).contains(&height)
            })
    }
}

impl WindowView {
    fn valid(&self) -> bool {
        self.tab != 0
            && (1..=MAX_WINDOW_VIEW_SIDE).contains(&self.width)
            && (1..=MAX_WINDOW_VIEW_SIDE).contains(&self.height)
    }
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
                if request_id == &[0; 16] || !valid_identifier(session_id, 256) {
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
            PersonalFabricAction::SpeakerGroupInvite {
                group,
                name,
                output,
                role,
            } => {
                if !group.valid()
                    || !crate::shell::speaker_group::valid_group_name(name)
                    || !output.valid()
                    || !role.valid()
                {
                    return Err(FabricError::InvalidAction);
                }
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
                    || !crate::shell::speaker_group::valid_group_name(name)
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
            PersonalFabricAction::WakeDevice { target }
            | PersonalFabricAction::WakeReport { target, .. } => {
                // Neither end of the exchange is the device being woken.
                if target == &self.source || target == &self.target {
                    return Err(FabricError::InvalidAction);
                }
                if let PersonalFabricAction::WakeReport {
                    outcome: WakeOutcome::Sent { subnet },
                    ..
                } = &self.action
                    && !subnet.is_canonical()
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::WindowViews { views, decode } => {
                let mut tabs = BTreeSet::new();
                let mut codecs = BTreeSet::new();
                if views.len() > MAX_WINDOW_VIEWS
                    || !views
                        .iter()
                        .all(|view| view.valid() && tabs.insert(view.tab))
                    || decode.len() > media::StreamCodec::ALL.len()
                    || !decode
                        .iter()
                        .all(|codec| codec.valid() && codecs.insert(codec.codec.as_str()))
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerGroupDecision { group, .. }
            | PersonalFabricAction::SpeakerListenDecision { group, .. }
            | PersonalFabricAction::SpeakerGroupLeave { group } => {
                if !group.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerGroupSessionOffer { session_id, group } => {
                if session_id == &[0; 16] || !group.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id,
                group,
                authorization,
            } => {
                if session_id == &[0; 16]
                    || !group.valid()
                    || authorization.is_some_and(|value| value == [0; 16])
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id,
                authorization,
            } => {
                if session_id == &[0; 16] || authorization.is_some_and(|value| value == [0; 16]) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteAudioCancel { session_id } => {
                if session_id == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteCaptureSignal { signal } => {
                if !signal.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RemoteCaptureChunk { chunk } => {
                use crate::shell::remote_capture::device::RemoteCaptureKind;
                if !chunk.valid_for(RemoteCaptureKind::Camera)
                    && !chunk.valid_for(RemoteCaptureKind::Microphone)
                    && !chunk.valid_for(RemoteCaptureKind::DocumentScanner)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::DeviceHealthProbe { nonce }
            | PersonalFabricAction::WakeProbe { nonce }
            | PersonalFabricAction::WakeProbeReply { nonce }
            | PersonalFabricAction::FabricClockProbe { nonce, .. }
            | PersonalFabricAction::InputLeaseRevoked { nonce } => {
                if nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::DeviceHealthReport { report } => {
                if !report.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::FabricClockReport { report } => {
                if !report.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::Tethering { signal } => {
                signal
                    .validate_transport(transport_source, local_target, now_unix_ms)
                    .map_err(|_| FabricError::InvalidAction)?;
            }
            PersonalFabricAction::BluetoothHandoffPrepare { address, name } => {
                if !valid_bluetooth_address(address)
                    || name.is_empty()
                    || name.len() > 128
                    || name.chars().any(char::is_control)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::BluetoothHandoffPrepared {
                request_id,
                address,
                ..
            }
            | PersonalFabricAction::BluetoothHandoffCommit {
                request_id,
                address,
            }
            | PersonalFabricAction::BluetoothHandoffFinished {
                request_id,
                address,
                ..
            }
            | PersonalFabricAction::BluetoothHandoffAccepted {
                request_id,
                address,
            }
            | PersonalFabricAction::BluetoothHandoffCancel {
                request_id,
                address,
            } => {
                if request_id == &[0; 16] || !valid_bluetooth_address(address) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::BluetoothHandoffPairingOffered {
                request_id,
                address,
                authorization,
            }
            | PersonalFabricAction::BluetoothHandoffPairingCommit {
                request_id,
                address,
                authorization,
            } => {
                if !valid_bluetooth_pairing_action(request_id, address, authorization) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NearbyDrop {
                file_name,
                content_hash,
                bytes,
            } => {
                if !valid_nearby_file_name(file_name)
                    || bytes.is_empty()
                    || bytes.len() > MAX_NEARBY_DROP_BYTES
                    || blake3::hash(bytes).as_bytes() != content_hash
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NearbyDropResult {
                request_id,
                file_name,
                ..
            } => {
                if request_id == &[0; 16] || !valid_nearby_file_name(file_name) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NearbyTransferOffer {
                transfer_id,
                name,
                total_bytes,
                ..
            } => {
                if transfer_id == &[0; 16]
                    || !valid_nearby_file_name(name)
                    || *total_bytes == 0
                    || *total_bytes > crate::shell::nearby_transfer::MAX_TRANSFER_BYTES
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NearbyTransferReady {
                request_id,
                transfer_id,
                committed_offset,
                ..
            } => {
                if request_id == &[0; 16]
                    || transfer_id == &[0; 16]
                    || *committed_offset > crate::shell::nearby_transfer::MAX_TRANSFER_BYTES
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NearbyTransferCancel {
                request_id,
                transfer_id,
            }
            | PersonalFabricAction::NearbyTransferRestart {
                request_id,
                transfer_id,
            }
            | PersonalFabricAction::NearbyTransferResult {
                request_id,
                transfer_id,
                ..
            } => {
                if request_id == &[0; 16] || transfer_id == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NotificationProject { notification } => {
                if !notification.validate_at(now_unix_ms) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NotificationRetire { id }
            | PersonalFabricAction::NotificationDismiss { id } => {
                if id.epoch == [0; 16] || id.token == [0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::NotificationInvoke { id, action_key } => {
                if id.epoch == [0; 16]
                    || id.token == [0; 16]
                    || action_key.is_empty()
                    || action_key.len() > 128
                    || action_key.chars().any(char::is_control)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RealtimeOffer { offer } => {
                if !offer.valid_at(now_unix_ms) {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::RealtimeAccept { session_id }
            | PersonalFabricAction::RealtimeMediaReady { session_id }
            | PersonalFabricAction::RealtimeStopSource { session_id }
            | PersonalFabricAction::RealtimeCancel { session_id } => {
                if !session_id.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::GamepadForwardRequest {
                forwarding_id,
                app_id,
                workspace_id,
                session_id,
                label,
            } => {
                if forwarding_id == &[0; 16]
                    || !valid_identifier(app_id, 256)
                    || !valid_identifier(workspace_id, 128)
                    || !valid_identifier(session_id, 256)
                    || label.is_empty()
                    || label.len() > 128
                    || label.chars().any(char::is_control)
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id,
                accepted,
                lease_nonce,
            } => {
                if forwarding_id == &[0; 16]
                    || (*accepted && lease_nonce == &[0; 16])
                    || (!*accepted && lease_nonce != &[0; 16])
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::GamepadForwardEvent {
                forwarding_id,
                lease_nonce,
                sequence,
                event,
            } => {
                if forwarding_id == &[0; 16]
                    || lease_nonce == &[0; 16]
                    || *sequence == 0
                    || !event.is_valid()
                {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::GamepadForwardStop {
                forwarding_id,
                lease_nonce,
            } => {
                if forwarding_id == &[0; 16] || lease_nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SetDisplayPlacementChecked {
                expected,
                rect,
                expires_unix_ms,
            } => {
                if &expected.device != local_target
                    || expected.counter == 0
                    || *expires_unix_ms < self.issued_unix_ms
                    || *expires_unix_ms
                        > self.issued_unix_ms.saturating_add(MAX_DISPLAY_MOVE_AGE_MS)
                    || now_unix_ms > *expires_unix_ms
                    || !valid_display_rect(rect)
                {
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
            PersonalFabricAction::SpatialInputProtocol { lease_nonce, .. } => {
                if lease_nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::SpatialInputSequenced {
                lease_nonce,
                sequence,
                event,
            } => {
                if lease_nonce == &[0; 16] || *sequence == 0 || !event.validate() {
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
            && (!valid_mime(mime) || mime == "application/x-tilo-secret")
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
        if self.key == settings_store::person_displays::ARRANGEMENT_KEY
            && settings_store::person_displays::Arrangement::from_value(&self.value).is_none()
        {
            return Err(FabricError::InvalidRoamingSetting);
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
    pub security: os_actions::WifiSecurity,
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
            || matches!(
                self.security,
                os_actions::WifiSecurity::Open | os_actions::WifiSecurity::Unknown
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
    /// Logical geometry shared with the Displays arrangement control.
    /// Malformed or overflowing screen bounds are never crossing targets.
    pub fn logical_placement(self) -> Option<settings_store::display_placement::Placement> {
        let width = i32::try_from(self.width).ok()?;
        let height = i32::try_from(self.height).ok()?;
        if width <= 0 || height <= 0 {
            return None;
        }
        self.x.checked_add(width)?;
        self.y.checked_add(height)?;
        Some(settings_store::display_placement::Placement::new(
            self.x, self.y, width, height,
        ))
    }

    fn contains_x(&self, x: i32) -> bool {
        x >= self.x && x < self.x.saturating_add_unsigned(self.width)
    }

    fn contains_y(&self, y: i32) -> bool {
        y >= self.y && y < self.y.saturating_add_unsigned(self.height)
    }

    pub fn intersects(&self, other: Self) -> bool {
        self.x < other.x.saturating_add_unsigned(other.width)
            && self.x.saturating_add_unsigned(self.width) > other.x
            && self.y < other.y.saturating_add_unsigned(other.height)
            && self.y.saturating_add_unsigned(self.height) > other.y
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl DisplayEdge {
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// A physical connector in its owner's logical seat coordinate space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayConnectorPlacement {
    pub connector: String,
    pub label: String,
    pub rect: DisplayRect,
    pub scale_milli: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPlacement {
    pub device: DeviceId,
    /// Ephemeral extension received separately; preserve the schema-6 checkpoint.
    #[serde(skip)]
    pub outputs: Vec<DisplayConnectorPlacement>,
    pub rect: DisplayRect,
    pub scale_milli: u16,
    pub stamp: FabricStamp,
}

impl DisplayPlacement {
    /// Only the owner may accept a request over its exact published revision.
    pub fn accepts_move(&self, expected: &FabricStamp, rect: DisplayRect) -> bool {
        expected == &self.stamp
            && !self.outputs.is_empty()
            && rect.width == self.rect.width
            && rect.height == self.rect.height
            && valid_display_rect(&rect)
            && self.outputs.iter().all(|output| {
                rect.x
                    .checked_add(output.rect.x)
                    .zip(rect.y.checked_add(output.rect.y))
                    .is_some_and(|(x, y)| {
                        valid_display_rect(&DisplayRect {
                            x,
                            y,
                            ..output.rect
                        })
                    })
            })
    }

    /// Shared plane rectangle paired with the owner's seat rectangle.
    fn screens(&self) -> Vec<(DisplayRect, DisplayRect)> {
        if self.outputs.is_empty() {
            return vec![(
                self.rect,
                DisplayRect {
                    x: 0,
                    y: 0,
                    ..self.rect
                },
            )];
        }
        self.outputs
            .iter()
            .filter_map(|output| {
                let global = DisplayRect {
                    x: self.rect.x.checked_add(output.rect.x)?,
                    y: self.rect.y.checked_add(output.rect.y)?,
                    ..output.rect
                };
                global.logical_placement()?;
                Some((global, output.rect))
            })
            .collect()
    }

    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.device != owner
            || &self.stamp.device != owner
            || self.rect.width == 0
            || self.rect.height == 0
            || !(500..=4000).contains(&self.scale_milli)
        {
            return Err(FabricError::InvalidDisplayPlacement);
        }
        if self.outputs.len() > 32 {
            return Err(FabricError::InvalidDisplayPlacement);
        }
        unique(self.outputs.iter().map(|output| output.connector.as_str()))?;
        for output in &self.outputs {
            if output.connector.is_empty()
                || output.connector.len() > 256
                || output.connector.chars().any(char::is_control)
                || output.label.len() > 256
                || output.label.chars().any(char::is_control)
                || output.rect.logical_placement().is_none()
                || !(500..=4000).contains(&output.scale_milli)
            {
                return Err(FabricError::InvalidDisplayPlacement);
            }
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
    pub(super) fn validate(self) -> bool {
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

/// The existing target-issued spatial-input gate, shared by the runtime and
/// native transport fixtures. Same-user identity is never a substitute for a
/// currently retained lease with the exact controller, target and nonce.
pub fn permits_spatial_input(
    lease: Option<&InputLease>,
    controller: &DeviceId,
    target: &DeviceId,
    input: ([u8; 16], FabricInputEvent),
    now_unix_ms: u64,
) -> bool {
    let (nonce, event) = input;
    lease.is_some_and(|lease| {
        lease.nonce == nonce
            && lease.binds(controller, target)
            && lease.validate_at(now_unix_ms).is_ok()
            && match event {
                FabricInputEvent::PointerMotion { .. }
                | FabricInputEvent::PointerButton { .. }
                | FabricInputEvent::Scroll { .. } => lease.allow_pointer,
                FabricInputEvent::Key { .. } => lease.allow_keyboard,
            }
    })
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
    pub speakers: crate::shell::speaker_group::SpeakerFabricState,
    /// Disk-unlock enrolments of this device and the owner's unlock
    /// settings published from it.
    pub unlock: crate::shell::device_unlock::UnlockFabricState,
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
        self.speakers.validate(&self.owner)?;
        self.unlock.validate(&self.owner)?;
        unique(self.media.iter().map(|item| item.id.as_str()))?;
        unique(self.clipboard.iter().map(|item| item.id))?;
        unique(self.settings.iter().map(|item| item.key.as_str()))?;
        unique(self.wifi.iter().map(|item| item.network_id))?;
        Ok(())
    }
}

/// Rich hardware facts accompanying the connector extension. They describe
/// an output but never grant control or change its live geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOutputObservation {
    pub connector: String,
    pub output: settings_store::person_displays::DeviceOutput,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFabric {
    snapshots: BTreeMap<DeviceId, DeviceFabricSnapshot>,
    /// Revision-bound observations stay outside the schema-6 checkpoint.
    #[serde(skip)]
    display_outputs: BTreeMap<DeviceId, (FabricStamp, Vec<DisplayOutputObservation>)>,
}

impl DeviceFabric {
    /// Use actual native screen interiors, including legacy aggregate fallbacks.
    pub fn display_destination_clear(&self, target: &DeviceId, rect: DisplayRect) -> bool {
        let Some(display) = self
            .snapshots
            .get(target)
            .and_then(|snapshot| snapshot.display.as_ref())
        else {
            return false;
        };
        if !display.accepts_move(&display.stamp, rect) {
            return false;
        }
        let moved = DisplayPlacement {
            rect,
            ..display.clone()
        };
        let screens = moved.screens();
        let others = self
            .snapshots
            .iter()
            .filter(|(device, snapshot)| *device != target && snapshot.presence.online)
            .filter_map(|(_, snapshot)| snapshot.display.as_ref())
            .flat_map(DisplayPlacement::screens)
            .map(|(screen, _)| screen)
            .collect::<Vec<_>>();
        !others
            .iter()
            .any(|other| screens.iter().any(|(screen, _)| screen.intersects(*other)))
            && others.iter().any(|other| {
                screens.iter().any(|(screen, _)| {
                    screen
                        .logical_placement()
                        .zip(other.logical_placement())
                        .is_some_and(|(screen, other)| screen.touches(other))
                })
            })
    }

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
                    .chain(
                        snapshot
                            .speakers
                            .groups
                            .iter()
                            .map(|item| item.stamp.counter),
                    )
                    .chain(
                        snapshot
                            .speakers
                            .memberships
                            .iter()
                            .map(|item| item.stamp.counter),
                    )
                    .chain(
                        snapshot
                            .unlock
                            .enrolments
                            .iter()
                            .map(|item| item.stamp.counter),
                    )
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
        let mut snapshot = snapshot;
        if let Some(current) = self.snapshots.get(&snapshot.owner)
            && let (Some(incoming), Some(previous)) = (&mut snapshot.display, &current.display)
            && incoming.stamp == previous.stamp
            && incoming.outputs.is_empty()
        {
            incoming.outputs.clone_from(&previous.outputs);
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

    /// Apply owner-authored connector observations for the exact display revision.
    /// They are an optional transport extension and confer no input authority.
    /// # Errors
    /// Rejects unknown owners, stale revisions, invalid bounds or conflicting observations.
    pub fn observe_display_connectors(
        &mut self,
        owner: &DeviceId,
        stamp: &FabricStamp,
        outputs: Vec<DisplayConnectorPlacement>,
    ) -> Result<bool, FabricError> {
        let current = self
            .snapshots
            .get_mut(owner)
            .and_then(|snapshot| snapshot.display.as_mut())
            .ok_or(FabricError::InvalidDisplayPlacement)?;
        if &stamp.device != owner || stamp != &current.stamp {
            return Err(FabricError::ForeignAuthorship);
        }
        let mut candidate = current.clone();
        candidate.outputs = outputs;
        candidate.validate(owner)?;
        if candidate == *current {
            return Ok(false);
        }
        if !current.outputs.is_empty() {
            return Err(FabricError::ConflictingRevision);
        }
        *current = candidate;
        Ok(true)
    }

    /// Admit bounded hardware facts for the owner's exact live display revision.
    /// # Errors
    /// Rejects unknown owners, stale revisions, conflicting facts and metadata
    /// that disagrees with the independently admitted connector geometry.
    pub fn observe_display_outputs(
        &mut self,
        owner: &DeviceId,
        stamp: &FabricStamp,
        outputs: Vec<DisplayOutputObservation>,
    ) -> Result<bool, FabricError> {
        let display = self
            .snapshots
            .get(owner)
            .and_then(|snapshot| snapshot.display.as_ref())
            .ok_or(FabricError::InvalidDisplayPlacement)?;
        if &stamp.device != owner || stamp != &display.stamp {
            return Err(FabricError::ForeignAuthorship);
        }
        if outputs.len() > settings_store::person_displays::MAX_DEVICE_OUTPUTS {
            return Err(FabricError::InvalidDisplayPlacement);
        }
        unique(outputs.iter().map(|item| item.connector.as_str()))?;
        unique(outputs.iter().map(|item| item.output.id.as_str()))?;
        for item in &outputs {
            if item.connector.is_empty()
                || item.connector.len() > 256
                || item.connector.chars().any(char::is_control)
                || !item.output.valid()
            {
                return Err(FabricError::InvalidDisplayPlacement);
            }
            if item.output.active() {
                let Some(connector) = display
                    .outputs
                    .iter()
                    .find(|connector| connector.connector == item.connector)
                else {
                    return Err(FabricError::InvalidDisplayPlacement);
                };
                let output = &item.output;
                if connector.rect
                    != (DisplayRect {
                        x: output.x,
                        y: output.y,
                        width: output.width,
                        height: output.height,
                    })
                    || connector.scale_milli != output.scale_milli
                {
                    return Err(FabricError::InvalidDisplayPlacement);
                }
            } else if display
                .outputs
                .iter()
                .any(|connector| connector.connector == item.connector)
            {
                return Err(FabricError::InvalidDisplayPlacement);
            }
        }
        if display.outputs.iter().any(|connector| {
            !outputs
                .iter()
                .any(|item| item.connector == connector.connector && item.output.active())
        }) {
            return Err(FabricError::InvalidDisplayPlacement);
        }
        if let Some((previous_stamp, previous)) = self.display_outputs.get(owner)
            && previous_stamp == stamp
        {
            return if previous == &outputs {
                Ok(false)
            } else {
                Err(FabricError::ConflictingRevision)
            };
        }
        self.display_outputs
            .insert(owner.clone(), (stamp.clone(), outputs));
        Ok(true)
    }

    /// The current revision's observed facts; stale metadata is never projected.
    pub fn display_outputs(&self, owner: &DeviceId) -> Option<&[DisplayOutputObservation]> {
        let display = self.snapshots.get(owner)?.display.as_ref()?;
        let (stamp, outputs) = self.display_outputs.get(owner)?;
        (stamp == &display.stamp).then_some(outputs.as_slice())
    }

    /// Certified device observations consumed by the shared layout model.
    pub fn person_devices(
        &self,
        local: &DeviceId,
    ) -> Vec<settings_store::person_displays::PersonDevice> {
        self.snapshots
            .values()
            .map(|snapshot| settings_store::person_displays::PersonDevice {
                device: snapshot.owner.as_str().to_owned(),
                label: snapshot.presence.label.clone(),
                local: &snapshot.owner == local,
                online: snapshot.presence.online,
                outputs: self
                    .display_outputs(&snapshot.owner)
                    .unwrap_or_default()
                    .iter()
                    .map(|item| item.output.clone())
                    .collect(),
            })
            .collect()
    }

    /// Forget everything a device published, e.g. once it left the fleet.
    /// Returns whether it had published anything.
    pub fn remove_device(&mut self, device: &DeviceId) -> bool {
        self.display_outputs.remove(device);
        self.snapshots.remove(device).is_some()
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

    pub fn cross_edge(
        &self,
        from: &DeviceId,
        edge: DisplayEdge,
        local_x: i32,
        local_y: i32,
    ) -> Option<EdgeTransition> {
        let snapshot = self.snapshots.get(from)?;
        if !snapshot.presence.online {
            return None;
        }
        let display = snapshot.display.as_ref()?;
        display.rect.logical_placement()?;
        let screens = display.screens();
        let (left, top, right, bottom) = seat_bounds(&screens)?;
        let x = local_x.clamp(left, right.checked_sub(1)?);
        let y = local_y.clamp(top, bottom.checked_sub(1)?);
        let (source, seat) = screens
            .into_iter()
            .find(|(_, seat)| seat.contains_x(x) && seat.contains_y(y))?;
        let source_placement = source.logical_placement()?;
        let global_x = source.x.checked_add(x.checked_sub(seat.x)?)?;
        let global_y = source.y.checked_add(y.checked_sub(seat.y)?)?;
        self.snapshots
            .values()
            .filter(|peer| {
                peer.owner != *from
                    && peer.presence.online
                    && peer.presence.capabilities.display
                    && peer.presence.capabilities.pointer
            })
            .filter_map(|peer| Some((&peer.owner, peer.display.as_ref()?)))
            .flat_map(|(device, display)| {
                display
                    .screens()
                    .into_iter()
                    .map(move |(rect, seat)| (device, rect, seat))
            })
            .filter(|(_, target, _)| {
                target
                    .logical_placement()
                    .is_some_and(|rect| rect.touches(source_placement))
            })
            .filter(|(_, target, _)| match edge {
                DisplayEdge::Left => {
                    target.x.saturating_add_unsigned(target.width) == source.x
                        && target.contains_y(global_y)
                }
                DisplayEdge::Right => {
                    target.x == source.x.saturating_add_unsigned(source.width)
                        && target.contains_y(global_y)
                }
                DisplayEdge::Top => {
                    target.y.saturating_add_unsigned(target.height) == source.y
                        && target.contains_x(global_x)
                }
                DisplayEdge::Bottom => {
                    target.y == source.y.saturating_add_unsigned(source.height)
                        && target.contains_x(global_x)
                }
            })
            .min_by_key(|(device, target, _)| ((*device).clone(), target.x, target.y))
            .map(|(device, target, seat)| EdgeTransition {
                from: from.clone(),
                to: device.clone(),
                target_x: seat.x.saturating_add(match edge {
                    DisplayEdge::Left => i32::try_from(target.width)
                        .unwrap_or(i32::MAX)
                        .saturating_sub(1),
                    DisplayEdge::Right => 0,
                    DisplayEdge::Top | DisplayEdge::Bottom => global_x.saturating_sub(target.x),
                }),
                target_y: seat.y.saturating_add(match edge {
                    DisplayEdge::Top => i32::try_from(target.height)
                        .unwrap_or(i32::MAX)
                        .saturating_sub(1),
                    DisplayEdge::Bottom => 0,
                    DisplayEdge::Left | DisplayEdge::Right => global_y.saturating_sub(target.y),
                }),
            })
    }

    pub fn cross_right_edge(&self, from: &DeviceId, local_y: i32) -> Option<EdgeTransition> {
        let right = self
            .snapshots
            .get(from)?
            .display
            .as_ref()?
            .screens()
            .iter()
            .filter_map(|(_, seat)| seat.x.checked_add(i32::try_from(seat.width).ok()?))
            .max()?;
        self.cross_edge(from, DisplayEdge::Right, right.checked_sub(1)?, local_y)
    }

    /// Resolve roaming keys from authenticated authored records. Display
    /// arrangements merge per display; other keys keep their newest record.
    /// Keeping the stamp and author lets runtimes distinguish a remote apply
    /// from a fresh local edit instead of echoing every received preference.
    pub fn merged_setting_records(&self) -> BTreeMap<String, RoamingSetting> {
        use settings_store::person_displays::{ARRANGEMENT_KEY, Arrangement};

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
        let mut merged: BTreeMap<_, _> = winners
            .into_iter()
            .map(|(key, item)| (key.to_string(), item.clone()))
            .collect();
        if let Some(record) = merged.get_mut(ARRANGEMENT_KEY) {
            let arrangements = self
                .snapshots
                .values()
                .flat_map(|snapshot| &snapshot.settings)
                .filter(|setting| setting.key == ARRANGEMENT_KEY)
                .filter_map(|setting| {
                    Arrangement::from_value(&setting.value)
                        .map(|arrangement| (setting.stamp.device.as_str(), arrangement))
                });
            // Keep the newest authored record for provenance and echo suppression,
            // but resolve each display independently so concurrent edits survive.
            record.value = Arrangement::merge(arrangements).to_value();
        }
        merged
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

impl DeviceFabric {
    /// Every group any certified device leads, newest record per id.
    pub fn speaker_groups(&self) -> Vec<&crate::shell::speaker_group::SpeakerGroup> {
        let mut winners = BTreeMap::<
            crate::shell::speaker_group::SpeakerGroupId,
            &crate::shell::speaker_group::SpeakerGroup,
        >::new();
        for group in self
            .snapshots
            .values()
            .flat_map(|snapshot| &snapshot.speakers.groups)
        {
            if winners
                .get(&group.id)
                .is_none_or(|current| group.stamp.is_newer_than(&current.stamp))
            {
                winners.insert(group.id, group);
            }
        }
        let mut groups = winners.into_values().collect::<Vec<_>>();
        groups.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        groups
    }

    pub fn speaker_group(
        &self,
        id: crate::shell::speaker_group::SpeakerGroupId,
    ) -> Option<&crate::shell::speaker_group::SpeakerGroup> {
        self.speaker_groups()
            .into_iter()
            .find(|group| group.id == id)
    }

    /// Where each member of `group` stands, in member order.
    pub fn speaker_member_states(
        &self,
        group: &crate::shell::speaker_group::SpeakerGroup,
    ) -> Vec<crate::shell::speaker_group::SpeakerMemberStatus> {
        use crate::shell::speaker_group::SpeakerMemberStatus;
        group
            .members
            .iter()
            .map(|member| {
                let Some(snapshot) = self.snapshots.get(&member.device) else {
                    return SpeakerMemberStatus::NotCertified;
                };
                if member.device != group.leader && !snapshot.presence.capabilities.audio_group {
                    return SpeakerMemberStatus::Unavailable;
                }
                if snapshot.speakers.output(member.output).is_none() {
                    return SpeakerMemberStatus::OutputMissing;
                }
                let consented = member.device == group.leader
                    || snapshot
                        .speakers
                        .memberships
                        .iter()
                        .any(|membership| membership.covers(group, member));
                if !consented {
                    SpeakerMemberStatus::AwaitingConsent
                } else if snapshot.presence.online {
                    SpeakerMemberStatus::Ready
                } else {
                    SpeakerMemberStatus::Offline
                }
            })
            .collect()
    }

    /// Admission rules for a group its leader is about to publish: every
    /// member is one of this person's certified devices, can join speaker
    /// groups (the leader's own outputs always can), and offers the named
    /// output.
    ///
    /// # Errors
    ///
    /// [`FabricError::InvalidSpeakerGroup`] for a structural problem,
    /// [`FabricError::UnknownDevice`] for a device outside the fabric, and
    /// [`FabricError::IneligibleSpeakerMember`] for a device that cannot
    /// join or does not offer that output.
    pub fn admit_speaker_group(
        &self,
        group: &crate::shell::speaker_group::SpeakerGroup,
    ) -> Result<(), FabricError> {
        group.validate(&group.leader)?;
        for member in &group.members {
            let snapshot = self
                .snapshots
                .get(&member.device)
                .ok_or(FabricError::UnknownDevice)?;
            if (member.device != group.leader && !snapshot.presence.capabilities.audio_group)
                || snapshot.speakers.output(member.output).is_none()
            {
                return Err(FabricError::IneligibleSpeakerMember);
            }
        }
        Ok(())
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

pub(crate) fn valid_nearby_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
}

fn seat_bounds(screens: &[(DisplayRect, DisplayRect)]) -> Option<(i32, i32, i32, i32)> {
    let left = screens.iter().map(|(_, seat)| seat.x).min()?;
    let top = screens.iter().map(|(_, seat)| seat.y).min()?;
    let right = screens
        .iter()
        .filter_map(|(_, seat)| seat.x.checked_add(i32::try_from(seat.width).ok()?))
        .max()?;
    let bottom = screens
        .iter()
        .filter_map(|(_, seat)| seat.y.checked_add(i32::try_from(seat.height).ok()?))
        .max()?;
    Some((left, top, right, bottom))
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
        MediaSourceLocator::PersonalTransfer {
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

pub(super) fn valid_bluetooth_address(address: &str) -> bool {
    let bytes = address.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn valid_bluetooth_pairing_action(
    request_id: &[u8; 16],
    address: &str,
    authorization: &[u8; 16],
) -> bool {
    request_id != &[0; 16] && authorization != &[0; 16] && valid_bluetooth_address(address)
}

/// Whether a settings-store leaf is deliberately part of a person's fabric.
///
/// This is an allowlist rather than a denylist: a newly-added setting stays
/// local until its semantics have been reviewed. In particular, connector
/// names, selected audio devices, display geometry/scale, radios, mounts,
/// power policy, update state and credentials must never become convergent
/// state merely because their key happens to begin with `tilo.`.
pub(super) fn valid_setting_key(key: &str) -> bool {
    valid_identifier(key, 256) && settings_store::continuity::is_personal(key)
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
    #[error("invalid speaker group")]
    InvalidSpeakerGroup,
    #[error("that device cannot play in this speaker group")]
    IneligibleSpeakerMember,
    #[error("invalid disk-unlock enrolment or setting")]
    InvalidUnlockEnrolment,
    #[error("invalid wake route")]
    InvalidWakeRoute,
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
                wake_routes: Vec::new(),
                stamp: stamp(device, counter),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: Some(DisplayPlacement {
                outputs: Vec::new(),
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
            unlock: crate::shell::device_unlock::UnlockFabricState::default(),
            speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
        }
    }

    fn arranged_setting(device: &str, counter: u64, displays: Value) -> RoamingSetting {
        let mut value = serde_json::Map::new();
        value.insert("displays".into(), displays);
        RoamingSetting {
            key: settings_store::person_displays::ARRANGEMENT_KEY.into(),
            value: Value::Object(value),
            stamp: stamp(device, counter),
        }
    }

    #[test]
    fn display_arrangements_merge_concurrent_edits_in_both_delivery_orders() {
        let entry = |output: &str, x: i32, rev: u64| {
            serde_json::json!({
                "device": "desktop", "output": output, "enabled": true,
                "order": 0, "placed": true, "x": x, "y": 0, "rev": rev
            })
        };
        let mut desktop = snapshot("desktop", 9, 0, FabricCapabilities::default());
        desktop.settings.push(arranged_setting(
            "desktop",
            9,
            serde_json::json!([entry("DP-1", 100, 3), entry("DP-2", 0, 1)]),
        ));
        let mut laptop = snapshot("laptop", 10, 1000, FabricCapabilities::default());
        laptop.settings.push(arranged_setting(
            "laptop",
            10,
            serde_json::json!([entry("DP-1", 0, 1), entry("DP-2", 200, 3)]),
        ));
        let mut first = DeviceFabric::default();
        first.upsert(desktop.clone()).unwrap();
        first.upsert(laptop.clone()).unwrap();
        let mut second = DeviceFabric::default();
        second.upsert(laptop).unwrap();
        second.upsert(desktop).unwrap();
        let records = first.merged_setting_records();
        assert_eq!(records, second.merged_setting_records());
        let record = &records[settings_store::person_displays::ARRANGEMENT_KEY];
        assert_eq!(record.stamp, stamp("laptop", 10));
        let arrangement =
            settings_store::person_displays::Arrangement::from_value(&record.value).unwrap();
        assert_eq!(arrangement.displays.len(), 2);
        assert_eq!(arrangement.displays[0].x, 100);
        assert_eq!(arrangement.displays[1].x, 200);
        // A later fabric publication must not beat a newer per-display edit.
        let mut stale = snapshot("tablet", 100, 2000, FabricCapabilities::default());
        stale.settings.push(arranged_setting(
            "tablet",
            100,
            serde_json::json!([entry("DP-1", -300, 2)]),
        ));
        first.upsert(stale).unwrap();
        assert_eq!(
            first.merged_settings()[settings_store::person_displays::ARRANGEMENT_KEY],
            record.value
        );
    }

    #[test]
    fn display_arrangements_reject_malformed_fabric_records() {
        for displays in [
            serde_json::json!(false),
            serde_json::json!([{
                "device": "desktop", "output": "DP-1", "enabled": true,
                "order": 0, "placed": true, "x": i32::MIN, "y": 0, "rev": 1
            }]),
        ] {
            let mut source = snapshot("desktop", 1, 0, FabricCapabilities::default());
            source
                .settings
                .push(arranged_setting("desktop", 1, displays));
            assert_eq!(
                DeviceFabric::default().upsert(source),
                Err(FabricError::InvalidRoamingSetting)
            );
        }
    }

    #[test]
    fn rich_display_extension_keeps_existing_payload_bytes() {
        let payload = crate::network::actor::PersonalFabricPayload::DisplayConnectors {
            stamp: stamp("peer", 1),
            outputs: Vec::new(),
        };
        let previous = hex::decode("0801047065657200").unwrap();
        assert_eq!(postcard::to_allocvec(&payload).unwrap(), previous);
        let decoded: crate::network::actor::PersonalFabricPayload =
            postcard::from_bytes(&previous).unwrap();
        assert!(matches!(decoded,
            crate::network::actor::PersonalFabricPayload::DisplayConnectors { outputs, .. }
                if outputs.is_empty()));
    }

    fn rich_display_fixture() -> (DeviceFabricSnapshot, DisplayOutputObservation) {
        let mut source = snapshot("peer", 2, 960, FabricCapabilities::default());
        let facts = DisplayOutputObservation {
            connector: "DP-1".into(),
            output: settings_store::person_displays::DeviceOutput {
                id: "monitor:panel".into(),
                name: "Panel".into(),
                builtin: false,
                x: 0,
                y: 0,
                width: 1000,
                height: 800,
                mode_width: 2000,
                mode_height: 1600,
                refresh_mhz: 59_940,
                scale_milli: 2000,
                connected: true,
                enabled: true,
            },
        };
        source
            .display
            .as_mut()
            .unwrap()
            .outputs
            .push(DisplayConnectorPlacement {
                connector: facts.connector.clone(),
                label: "DP-1".into(),
                rect: DisplayRect {
                    x: 0,
                    y: 0,
                    width: 1000,
                    height: 800,
                },
                scale_milli: 2000,
            });
        (source, facts)
    }

    #[test]
    fn rich_display_observations_bind_owner_revision_geometry_and_checkpoint() {
        let (mut source, facts) = rich_display_fixture();
        let owner = source.owner.clone();
        let revision = source.display.as_ref().unwrap().stamp.clone();
        let mut fabric = DeviceFabric::default();
        fabric.upsert(source.clone()).unwrap();
        let checkpoint = fabric.encode_checkpoint().unwrap();
        for stamp in [stamp("stranger", 2), stamp("peer", 1)] {
            assert_eq!(
                fabric.observe_display_outputs(&owner, &stamp, vec![facts.clone()]),
                Err(FabricError::ForeignAuthorship)
            );
        }
        let mut wrong = facts.clone();
        wrong.output.width += 1;
        assert_eq!(
            fabric.observe_display_outputs(&owner, &revision, vec![wrong]),
            Err(FabricError::InvalidDisplayPlacement)
        );
        assert_eq!(
            fabric.observe_display_outputs(&owner, &revision, Vec::new()),
            Err(FabricError::InvalidDisplayPlacement)
        );
        assert!(
            fabric
                .observe_display_outputs(&owner, &revision, vec![facts.clone()])
                .unwrap()
        );
        assert!(
            !fabric
                .observe_display_outputs(&owner, &revision, vec![facts.clone()])
                .unwrap()
        );
        assert_eq!(fabric.encode_checkpoint().unwrap(), checkpoint);
        assert!(
            DeviceFabric::decode_checkpoint(&checkpoint)
                .unwrap()
                .display_outputs(&owner)
                .is_none()
        );
        let mut conflicting = facts.clone();
        conflicting.output.refresh_mhz = 60_000;
        assert_eq!(
            fabric.observe_display_outputs(&owner, &revision, vec![conflicting]),
            Err(FabricError::ConflictingRevision)
        );
        source.presence.online = false;
        source.presence.stamp.counter = 3;
        fabric.upsert(source.clone()).unwrap();
        let devices = fabric.person_devices(&owner);
        assert_eq!(devices[0].outputs, vec![facts.output]);
        assert!(!devices[0].online);
        source.presence.stamp.counter = 4;
        source.display.as_mut().unwrap().stamp.counter = 4;
        fabric.upsert(source).unwrap();
        assert!(fabric.display_outputs(&owner).is_none());
        assert!(fabric.person_devices(&owner)[0].outputs.is_empty());
        assert!(fabric.remove_device(&owner));
        assert!(fabric.person_devices(&owner).is_empty());
    }

    #[test]
    fn rich_display_observations_reject_unbounded_and_duplicate_metadata() {
        let (source, facts) = rich_display_fixture();
        let owner = source.owner.clone();
        let revision = source.display.as_ref().unwrap().stamp.clone();
        let mut fabric = DeviceFabric::default();
        fabric.upsert(source).unwrap();
        for invalid in [
            {
                let mut item = facts.clone();
                item.output.x = i32::MIN;
                vec![item]
            },
            vec![facts.clone(); settings_store::person_displays::MAX_DEVICE_OUTPUTS + 1],
        ] {
            assert_eq!(
                fabric.observe_display_outputs(&owner, &revision, invalid),
                Err(FabricError::InvalidDisplayPlacement)
            );
        }
        assert_eq!(
            fabric.observe_display_outputs(&owner, &revision, vec![facts.clone(), facts]),
            Err(FabricError::DuplicateItem)
        );
    }

    #[test]
    fn connector_extension_keeps_schema_six_display_bytes() {
        let peer = DeviceId::new("peer").unwrap();
        let display = DisplayPlacement {
            device: peer.clone(),
            rect: DisplayRect {
                x: 960,
                y: 0,
                width: 1280,
                height: 720,
            },
            scale_milli: 1000,
            stamp: FabricStamp {
                counter: 1,
                device: peer,
            },
            outputs: vec![DisplayConnectorPlacement {
                connector: "DP-1".into(),
                label: "Panel".into(),
                rect: DisplayRect {
                    x: 0,
                    y: 0,
                    width: 1280,
                    height: 720,
                },
                scale_milli: 1250,
            }],
        };
        // Fixed schema-6 Postcard bytes: owner, aggregate rect, scale, stamp.
        let previous = hex::decode("0470656572800f00800ad005e807010470656572").unwrap();
        assert_eq!(FABRIC_SCHEMA_VERSION, 6);
        assert_eq!(postcard::to_allocvec(&display).unwrap(), previous);
        let decoded: DisplayPlacement = postcard::from_bytes(&previous).unwrap();
        assert!(
            decoded.outputs.is_empty(),
            "legacy peers retain their aggregate-screen behavior"
        );
        assert_eq!(decoded.rect, display.rect);
    }

    #[test]
    fn connector_observations_require_the_exact_owner_display_revision() {
        let caps = FabricCapabilities {
            display: true,
            pointer: true,
            ..Default::default()
        };
        let mut local = snapshot("peer", 2, 960, caps);
        let display = local.display.as_mut().unwrap();
        let owner = display.device.clone();
        let stamp = display.stamp.clone();
        let outputs = vec![DisplayConnectorPlacement {
            connector: "DP-1".into(),
            label: "Panel".into(),
            rect: DisplayRect {
                x: 0,
                y: 0,
                width: 960,
                height: 540,
            },
            scale_milli: 1250,
        }];
        let mut fabric = DeviceFabric::default();
        fabric.upsert(local.clone()).unwrap();
        let mut stale = stamp.clone();
        stale.counter -= 1;
        assert!(
            fabric
                .observe_display_connectors(&owner, &stale, outputs.clone())
                .is_err()
        );
        assert!(
            fabric
                .observe_display_connectors(
                    &DeviceId::new("other").unwrap(),
                    &stamp,
                    outputs.clone()
                )
                .is_err()
        );
        assert!(
            fabric
                .observe_display_connectors(&owner, &stamp, outputs.clone())
                .unwrap()
        );
        assert!(
            !fabric
                .observe_display_connectors(&owner, &stamp, outputs.clone())
                .unwrap()
        );
        let mut conflict = outputs;
        conflict[0].rect.x = 50;
        assert!(
            fabric
                .observe_display_connectors(&owner, &stamp, conflict)
                .is_err()
        );
        assert!(
            !fabric.upsert(local.clone()).unwrap(),
            "duplicate schema-6 checkpoints retain the optional observation"
        );
        local.presence.stamp.counter += 1;
        fabric.upsert(local).unwrap();
        assert_eq!(
            fabric
                .snapshots()
                .get(&owner)
                .unwrap()
                .display
                .as_ref()
                .unwrap()
                .outputs
                .len(),
            1
        );
        fabric.remove_device(&owner);
        assert!(
            fabric
                .observe_display_connectors(&owner, &stamp, Vec::new())
                .is_err(),
            "revoked or forgotten peers cannot retain observations"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_handoff_and_media_actions_are_capability_and_locator_checked() {
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
                app_id: "so.tilo.video".into(),
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
                MediaSourceLocator::PersonalTransfer {
                    transfer_id: [0; 16],
                    file_name: "../foreign.mp4".into(),
                },
            ] {
                let envelope = PersonalFabricActionEnvelope {
                    request_id: [4; 16],
                    source: source.clone(),
                    target: target.clone(),
                    issued_unix_ms: 10_000,
                    action: PersonalFabricAction::ResumeMedia {
                        session_id: "player:one".into(),
                        app_id: "so.tilo.music".into(),
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

        fn media_terminal_actions_require_exact_nonzero_request_and_session() {
            let source = id("source");
            let target = id("target");
            for action in [
                PersonalFabricAction::MediaRejected {
                    request_id: [9; 16],
                    session_id: "player:one".into(),
                    failure: MediaHandoffFailure::OpenFailed,
                },
                PersonalFabricAction::MediaCancelled {
                    request_id: [9; 16],
                    session_id: "player:one".into(),
                },
            ] {
                let envelope = PersonalFabricActionEnvelope {
                    request_id: [4; 16],
                    source: source.clone(),
                    target: target.clone(),
                    issued_unix_ms: 10_000,
                    action,
                };
                assert_eq!(envelope.validate_at(&source, &target, 10_000), Ok(()));
            }

            let invalid = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::MediaRejected {
                    request_id: [0; 16],
                    session_id: "player:one".into(),
                    failure: MediaHandoffFailure::OpenFailed,
                },
            };
            assert_eq!(
                invalid.validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }

        media_handoff_preserves_playhead_and_pauses_source();
        video_cannot_be_sent_to_an_audio_only_device();
        media_action_locators_reject_host_paths_and_url_credentials();
        media_terminal_actions_require_exact_nonzero_request_and_session();
    }

    #[test]
    fn window_views_are_bounded_distinct_and_sized() {
        let source = id("viewer");
        let target = id("host");
        let envelope =
            |views: Vec<WindowView>, decode: Vec<HardwareDecode>| PersonalFabricActionEnvelope {
                request_id: [5; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::WindowViews { views, decode },
            };
        let view = |tab, width, height| WindowView { tab, width, height };
        let ok = |views| {
            envelope(views, Vec::new())
                .validate_at(&source, &target, 10_000)
                .is_ok()
        };
        let decodes = |decode| {
            envelope(vec![view(7, 1280, 800)], decode)
                .validate_at(&source, &target, 10_000)
                .is_ok()
        };
        let hw = |codec: &str, max| HardwareDecode {
            codec: codec.into(),
            max,
        };

        assert!(ok(Vec::new()), "an empty list ends every stream");
        assert!(ok(vec![
            view(7, 1280, 800),
            view(8, 1, MAX_WINDOW_VIEW_SIDE)
        ]));
        assert!(!ok(vec![view(0, 1280, 800)]), "a tab id is never zero");
        assert!(!ok(vec![view(7, 0, 800)]));
        assert!(!ok(vec![view(7, 1280, MAX_WINDOW_VIEW_SIDE + 1)]));
        assert!(
            !ok(vec![view(7, 1280, 800), view(7, 640, 480)]),
            "one size per tab"
        );
        assert!(!ok((1..=u64::try_from(MAX_WINDOW_VIEWS).unwrap() + 1)
            .map(|tab| view(tab, 100, 100))
            .collect()));

        // A board's decoder limit travels with the list.
        assert!(decodes(vec![
            hw("h264", Some((1920, 1088))),
            hw("av1", None)
        ]));
        assert!(!decodes(vec![hw("mjpeg", None)]), "a codec streams use");
        assert!(!decodes(vec![hw("h264", Some((0, 1088)))]));
        assert!(
            !decodes(vec![hw("h264", None), hw("h264", Some((1280, 720)))]),
            "one limit per codec"
        );
    }

    #[test]
    fn display_placement_wire_encoding_is_stable() {
        let action = PersonalFabricAction::SetDisplayPlacement {
            rect: DisplayRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        };
        // The original wire tag and postcard field encoding, independent of
        // the current enum serializer. Encoding and decoding must both agree.
        let original = [2, 0, 0, 1, 1];
        assert_eq!(postcard::to_allocvec(&action).unwrap(), original);
        assert_eq!(
            postcard::from_bytes::<PersonalFabricAction>(&original).unwrap(),
            action
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn clipboard_and_nearby_payloads_are_bounded_and_tamper_checked() {
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

        fn nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests() {
            let source = id("source");
            let target = id("target");
            let envelope = |action| PersonalFabricActionEnvelope {
                request_id: [3; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert_eq!(
                envelope(PersonalFabricAction::NearbyTransferOffer {
                    transfer_id: [4; 16],
                    name: "Photos".into(),
                    kind: crate::shell::nearby_transfer::TransferKind::FolderArchive,
                    total_bytes: 100,
                    content_hash: [5; 32],
                })
                .validate_at(&source, &target, 10_100),
                Ok(())
            );
            assert_eq!(
                envelope(PersonalFabricAction::NearbyTransferReady {
                    request_id: [0; 16],
                    transfer_id: [4; 16],
                    committed_offset: 0,
                    prefix_hash: *blake3::hash(&[]).as_bytes(),
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::NearbyTransferOffer {
                    transfer_id: [4; 16],
                    name: "../escape".into(),
                    kind: crate::shell::nearby_transfer::TransferKind::File,
                    total_bytes: 100,
                    content_hash: [5; 32],
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }

        fn nearby_drop_rejects_paths_tampering_and_oversize_payloads() {
            let source = id("source");
            let target = id("target");
            for (name, bytes, hash) in [
                ("../secret", b"ok".to_vec(), *blake3::hash(b"ok").as_bytes()),
                ("note.txt", b"changed".to_vec(), [0; 32]),
                ("note.txt", vec![1; MAX_NEARBY_DROP_BYTES + 1], [0; 32]),
            ] {
                let envelope = PersonalFabricActionEnvelope {
                    request_id: [1; 16],
                    source: source.clone(),
                    target: target.clone(),
                    issued_unix_ms: 10,
                    action: PersonalFabricAction::NearbyDrop {
                        file_name: name.into(),
                        content_hash: hash,
                        bytes,
                    },
                };
                assert_eq!(
                    envelope.validate_at(&source, &target, 10),
                    Err(FabricError::InvalidAction)
                );
            }
        }

        clipboard_is_deduplicated_and_bounded();
        clipboard_hash_tampering_is_rejected();
        nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests();
        nearby_drop_rejects_paths_tampering_and_oversize_payloads();
    }

    #[test]
    fn roaming_settings_accept_only_audited_leaves_and_resolve_deterministically() {
        fn local_hardware_settings_do_not_roam() {
            for key in [
                "tilo.device.name",
                "tilo.display.hardware.edid",
                "tilo.display.scale",
                "tilo.network.interface.mac",
                "tilo.storage.mount.usb",
                "tilo.input.tablets",
                "tilo.audio.output_device",
                "tilo.agent_providers.api_key",
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

        fn only_audited_personal_setting_leaves_are_accepted() {
            for key in [
                "tilo.accessibility.screen_reader",
                "tilo.appearance.studio",
                "tilo.locale.measurement",
                "tilo.notifications.per_app",
                "tilo.input.keyboard_layouts",
                "tilo.audio.mono_output",
                "tilo.display.night_light_temperature",
                "tilo.shortcuts.bindings",
                "tilo.desktop_controls.vim_mode",
                "tilo.quick_settings.controls",
                "tilo.text_assistance.auto_correct",
            ] {
                assert!(valid_setting_key(key), "expected {key} to roam");
            }
            for key in [
                "tilo.appearance.wallpaper_path",
                "tilo.input.future_device_serial",
                "tilo.audio.input_device",
                "tilo.display.auto_brightness",
                "tilo.power.auto_suspend",
                "tilo.updates.auto",
                "tilo.security.local_credential.pin",
            ] {
                assert!(!valid_setting_key(key), "expected {key} to stay local");
            }
        }

        fn newest_roaming_setting_wins_deterministically() {
            let mut old = snapshot("desktop", 1, 0, FabricCapabilities::default());
            old.settings.push(RoamingSetting {
                key: "tilo.appearance.mode".into(),
                value: Value::Bool(false),
                stamp: stamp("desktop", 1),
            });
            let mut new = snapshot("laptop", 2, 1000, FabricCapabilities::default());
            new.settings.push(RoamingSetting {
                key: "tilo.appearance.mode".into(),
                value: Value::Bool(true),
                stamp: stamp("laptop", 2),
            });
            let mut fabric = DeviceFabric::default();
            fabric.upsert(old).unwrap();
            fabric.upsert(new).unwrap();
            assert_eq!(
                fabric.merged_settings()["tilo.appearance.mode"],
                Value::Bool(true)
            );
        }

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

        local_hardware_settings_do_not_roam();
        only_audited_personal_setting_leaves_are_accepted();
        newest_roaming_setting_wins_deterministically();
        equal_revision_conflicts_fail_instead_of_using_arrival_order();
        persisted_checkpoint_round_trips_only_validated_state();
    }

    #[test]
    fn wifi_records_are_sealed_typed_and_legacy_safe() {
        fn wifi_secrets_must_be_sealed() {
            let mut source = snapshot("phone", 1, 0, FabricCapabilities::default());
            source.wifi.push(WifiCredential {
                network_id: [1; 32],
                ssid: "Home".into(),
                security: os_actions::WifiSecurity::Wpa3,
                sealed_secret: Vec::new(),
                nonce: [2; 24],
                stamp: stamp("phone", 1),
            });
            assert_eq!(source.validate(), Err(FabricError::InvalidWifiCredential));
        }

        fn typed_wifi_security_decodes_the_legacy_string_postcard_shape() {
            #[derive(Serialize)]
            struct LegacyWifiCredential {
                network_id: [u8; 32],
                ssid: String,
                security: String,
                sealed_secret: Vec<u8>,
                nonce: [u8; 24],
                stamp: FabricStamp,
            }

            let legacy = LegacyWifiCredential {
                network_id: [1; 32],
                ssid: "Home".into(),
                security: "wpa3".into(),
                sealed_secret: vec![1, 2, 3],
                nonce: [2; 24],
                stamp: stamp("phone", 1),
            };
            let bytes = postcard::to_allocvec(&legacy).unwrap();
            let decoded: WifiCredential = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.security, os_actions::WifiSecurity::Wpa3);
            assert_eq!(postcard::to_allocvec(&decoded).unwrap(), bytes);
        }

        fn legacy_enterprise_record_is_retained_but_cannot_supply_a_password() {
            let key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let owner_text = hex::encode(key.verifying_key().to_bytes());
            let owner = id(&owner_text);
            let mut credential = crate::shell::device_fabric_secrets::seal_wifi_credential(
                "Corp",
                os_actions::WifiSecurity::Wpa2,
                "legacy-secret",
                &owner,
                [owner.clone()],
                FabricStamp {
                    counter: 1,
                    device: owner.clone(),
                },
            )
            .unwrap();
            // Version 1 allowed this string even though its secret lacked the EAP
            // identity and CA policy needed to safely recreate the profile.
            credential.security = os_actions::WifiSecurity::Enterprise;

            assert_eq!(
                crate::shell::device_fabric_secrets::open_wifi_credential(&credential, &key),
                Err(crate::shell::device_fabric_secrets::WifiSecretError::InvalidCredential)
            );

            let mut source = snapshot(&owner_text, 1, 0, FabricCapabilities::default());
            source.wifi.push(credential);
            source.settings.push(RoamingSetting {
                key: "tilo.appearance.mode".into(),
                value: Value::String("Nord".into()),
                stamp: FabricStamp {
                    counter: 1,
                    device: owner,
                },
            });
            let mut fabric = DeviceFabric::default();
            fabric.upsert(source).unwrap();

            assert_eq!(
                fabric.merged_settings()["tilo.appearance.mode"],
                Value::String("Nord".into())
            );
        }

        wifi_secrets_must_be_sealed();
        typed_wifi_security_decodes_the_legacy_string_postcard_shape();
        legacy_enterprise_record_is_retained_but_cannot_supply_a_password();
    }

    #[test]
    fn display_edges_map_into_the_adjacent_screen() {
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

        fn every_touching_display_edge_maps_into_the_adjacent_screen() {
            let caps = FabricCapabilities {
                display: true,
                pointer: true,
                ..FabricCapabilities::default()
            };
            let mut center = snapshot("center", 1, 0, caps);
            center.display.as_mut().unwrap().rect = DisplayRect {
                x: 0,
                y: 0,
                width: 1000,
                height: 800,
            };
            let mut left = snapshot("left", 1, -600, caps);
            left.display.as_mut().unwrap().rect = DisplayRect {
                x: -600,
                y: 200,
                width: 600,
                height: 600,
            };
            let mut right = snapshot("right", 1, 1000, caps);
            right.display.as_mut().unwrap().rect = DisplayRect {
                x: 1000,
                y: 100,
                width: 700,
                height: 700,
            };
            let mut top = snapshot("top", 1, 100, caps);
            top.display.as_mut().unwrap().rect = DisplayRect {
                x: 100,
                y: -500,
                width: 700,
                height: 500,
            };
            let mut bottom = snapshot("bottom", 1, 200, caps);
            bottom.display.as_mut().unwrap().rect = DisplayRect {
                x: 200,
                y: 800,
                width: 800,
                height: 400,
            };
            let mut fabric = DeviceFabric::default();
            for display in [center, left, right, top, bottom] {
                fabric.upsert(display).unwrap();
            }
            let left = fabric
                .cross_edge(&id("center"), DisplayEdge::Left, 0, 417)
                .unwrap();
            assert_eq!(
                (left.to, left.target_x, left.target_y),
                (id("left"), 599, 217)
            );
            let right = fabric
                .cross_edge(&id("center"), DisplayEdge::Right, 999, 417)
                .unwrap();
            assert_eq!(
                (right.to, right.target_x, right.target_y),
                (id("right"), 0, 317)
            );
            let top = fabric
                .cross_edge(&id("center"), DisplayEdge::Top, 417, 0)
                .unwrap();
            assert_eq!((top.to, top.target_x, top.target_y), (id("top"), 317, 499));
            let bottom = fabric
                .cross_edge(&id("center"), DisplayEdge::Bottom, 417, 799)
                .unwrap();
            assert_eq!(
                (bottom.to, bottom.target_x, bottom.target_y),
                (id("bottom"), 217, 0)
            );
        }

        adjacent_displays_cross_without_a_vertical_jump();
        every_touching_display_edge_maps_into_the_adjacent_screen();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_input_and_gamepad_actions_require_exact_bound_leases() {
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

        fn gamepad_actions_are_typed_exact_bound_and_nonzero() {
            let source = id("source");
            let target = id("target");
            let envelope = |action| PersonalFabricActionEnvelope {
                request_id: [8; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert!(
                envelope(PersonalFabricAction::GamepadForwardEvent {
                    forwarding_id: [1; 16],
                    lease_nonce: [2; 16],
                    sequence: 1,
                    event: os_actions::ForwardedGamepadEvent::Button {
                        button: os_actions::GamepadButton::South,
                        pressed: true,
                    },
                })
                .validate_at(&source, &target, 10_001)
                .is_ok()
            );
            for action in [
                PersonalFabricAction::GamepadForwardGrant {
                    forwarding_id: [1; 16],
                    accepted: true,
                    lease_nonce: [0; 16],
                },
                PersonalFabricAction::GamepadForwardGrant {
                    forwarding_id: [1; 16],
                    accepted: false,
                    lease_nonce: [2; 16],
                },
                PersonalFabricAction::GamepadForwardEvent {
                    forwarding_id: [1; 16],
                    lease_nonce: [2; 16],
                    sequence: 2,
                    event: os_actions::ForwardedGamepadEvent::Hat { x: 2, y: 0 },
                },
            ] {
                assert_eq!(
                    envelope(action).validate_at(&source, &target, 10_001),
                    Err(FabricError::InvalidAction)
                );
            }
        }

        remote_input_requires_a_short_lived_explicit_lease();
        input_lease_must_be_issued_by_the_target_to_its_controller();
        input_actions_reject_unbound_or_malformed_events();
        gamepad_actions_are_typed_exact_bound_and_nonzero();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wire_actions_are_fresh_source_bound_and_never_renumbered() {
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
                    app_id: "so.tilo.music".into(),
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

        fn new_action_variants_do_not_renumber_existing_wire_actions() {
            let placement = postcard::to_allocvec(&PersonalFabricAction::SetDisplayPlacement {
                rect: DisplayRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            })
            .unwrap();
            let input = postcard::to_allocvec(&PersonalFabricAction::Input {
                lease_nonce: [1; 16],
                event: FabricInputEvent::PointerMotion { x: 0, y: 0 },
            })
            .unwrap();
            let bluetooth = postcard::to_allocvec(&PersonalFabricAction::BluetoothHandoffPrepare {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Headphones".into(),
            })
            .unwrap();
            assert_eq!(placement.first(), Some(&2));
            assert_eq!(input.first(), Some(&5));
            assert_eq!(bluetooth.first(), Some(&6));
            let pairing =
                postcard::to_allocvec(&PersonalFabricAction::BluetoothHandoffPairingOffered {
                    request_id: [2; 16],
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    authorization: [4; 16],
                })
                .unwrap();
            assert_eq!(pairing.first(), Some(&11));
            let nearby = postcard::to_allocvec(&PersonalFabricAction::NearbyDrop {
                file_name: "note.txt".into(),
                content_hash: *blake3::hash(b"hello").as_bytes(),
                bytes: b"hello".to_vec(),
            })
            .unwrap();
            assert_eq!(nearby.first(), Some(&13));
            let notification = postcard::to_allocvec(&PersonalFabricAction::NotificationRetire {
                id: tilo_desktop::notifications::ContinuityNotificationId {
                    epoch: [1; 16],
                    token: [2; 16],
                },
            })
            .unwrap();
            assert_eq!(notification.first(), Some(&21));
            let realtime_id = crate::shell::realtime_session::RealtimeFabricSessionId {
                broker: [1; 16],
                nonce: [2; 16],
            };
            for (action, index) in [
                (
                    PersonalFabricAction::RealtimeOffer {
                        offer: crate::shell::realtime_session::RealtimeFabricOffer {
                            session_id: realtime_id,
                            app_id: "so.tilo.chat".into(),
                            workspace_id: "room".into(),
                            provider: "chat".into(),
                            room_id: "call".into(),
                            semantic_position: None,
                            media_bits: 1,
                            controls: crate::shell::realtime_session::RealtimeControlState::default(
                            ),
                            expires_unix_ms: 20_000,
                        },
                    },
                    24,
                ),
                (
                    PersonalFabricAction::RealtimeAccept {
                        session_id: realtime_id,
                    },
                    25,
                ),
                (
                    PersonalFabricAction::RealtimeMediaReady {
                        session_id: realtime_id,
                    },
                    26,
                ),
                (
                    PersonalFabricAction::RealtimeStopSource {
                        session_id: realtime_id,
                    },
                    27,
                ),
                (
                    PersonalFabricAction::RealtimeCancel {
                        session_id: realtime_id,
                    },
                    28,
                ),
            ] {
                assert_eq!(
                    postcard::to_allocvec(&action).unwrap().first(),
                    Some(&index)
                );
            }
            for (action, index) in [
                (
                    PersonalFabricAction::GamepadForwardRequest {
                        forwarding_id: [3; 16],
                        app_id: "so.tilo.game".into(),
                        workspace_id: "workspace".into(),
                        session_id: "session".into(),
                        label: "Controller".into(),
                    },
                    29,
                ),
                (
                    PersonalFabricAction::GamepadForwardGrant {
                        forwarding_id: [3; 16],
                        accepted: true,
                        lease_nonce: [4; 16],
                    },
                    30,
                ),
                (
                    PersonalFabricAction::GamepadForwardEvent {
                        forwarding_id: [3; 16],
                        lease_nonce: [4; 16],
                        sequence: 1,
                        event: os_actions::ForwardedGamepadEvent::Hat { x: 0, y: -1 },
                    },
                    31,
                ),
                (
                    PersonalFabricAction::GamepadForwardStop {
                        forwarding_id: [3; 16],
                        lease_nonce: [4; 16],
                    },
                    32,
                ),
            ] {
                assert_eq!(
                    postcard::to_allocvec(&action).unwrap().first(),
                    Some(&index)
                );
            }
        }

        fn notification_actions_are_source_bound_bounded_and_fresh() {
            let source = id("source");
            let target = id("target");
            let notification = crate::notifications::ContinuityNotificationProjection {
                id: tilo_desktop::notifications::ContinuityNotificationId {
                    epoch: [1; 16],
                    token: [2; 16],
                },
                app_id: "so.tilo.mail".into(),
                app_name: "Mail".into(),
                title: "New message".into(),
                body: "Preview".into(),
                body_redacted: false,
                actions: vec![("reply".into(), "Reply".into())],
                urgency: 1,
                expires_unix_ms: 20_000,
            };
            let envelope = PersonalFabricActionEnvelope {
                request_id: [3; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::NotificationProject { notification },
            };
            assert!(envelope.validate_at(&source, &target, 10_000).is_ok());
            assert_eq!(
                envelope.validate_at(&id("spoof"), &target, 10_000),
                Err(FabricError::InvalidAction)
            );

            let invalid = PersonalFabricActionEnvelope {
                action: PersonalFabricAction::NotificationInvoke {
                    id: tilo_desktop::notifications::ContinuityNotificationId {
                        epoch: [1; 16],
                        token: [2; 16],
                    },
                    action_key: "bad\naction".into(),
                },
                ..envelope
            };
            assert_eq!(
                invalid.validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }

        fn realtime_offer_rejects_spoofing_urls_and_stale_metadata() {
            let source = id("source");
            let target = id("target");
            let mut offer = crate::shell::realtime_session::RealtimeFabricOffer {
                session_id: crate::shell::realtime_session::RealtimeFabricSessionId {
                    broker: [1; 16],
                    nonce: [2; 16],
                },
                app_id: "so.tilo.chat".into(),
                workspace_id: "room".into(),
                provider: "chat".into(),
                room_id: "call-7".into(),
                semantic_position: Some("stage".into()),
                media_bits: 1,
                controls: crate::shell::realtime_session::RealtimeControlState::default(),
                expires_unix_ms: 20_000,
            };
            let envelope = PersonalFabricActionEnvelope {
                request_id: [9; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::RealtimeOffer {
                    offer: offer.clone(),
                },
            };
            assert!(envelope.validate_at(&source, &target, 10_000).is_ok());
            assert_eq!(
                envelope.validate_at(&id("spoof"), &target, 10_000),
                Err(FabricError::InvalidAction)
            );
            offer.room_id = "https://secret.example/join?token=x".into();
            assert_eq!(
                PersonalFabricActionEnvelope {
                    action: PersonalFabricAction::RealtimeOffer {
                        offer: offer.clone(),
                    },
                    ..envelope.clone()
                }
                .validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
            offer.room_id = "call-7".into();
            offer.expires_unix_ms = 9_999;
            assert_eq!(
                PersonalFabricActionEnvelope {
                    action: PersonalFabricAction::RealtimeOffer { offer },
                    ..envelope
                }
                .validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }

        ephemeral_actions_are_transport_bound_and_fresh();
        new_action_variants_do_not_renumber_existing_wire_actions();
        notification_actions_are_source_bound_bounded_and_fresh();
        realtime_offer_rejects_spoofing_urls_and_stale_metadata();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn audio_and_bluetooth_wire_is_versioned_bounded_and_canonical() {
        fn remote_audio_wire_is_versioned_bounded_and_capability_bearing() {
            let source = id("source");
            let target = id("target");
            let envelope = |action| PersonalFabricActionEnvelope {
                request_id: [3; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            let offer = || PersonalFabricAction::RemoteAudioOffer {
                session_id: [4; 16],
                protocol_version: REMOTE_AUDIO_PROTOCOL_VERSION,
                label: "All system audio".into(),
                sample_rate_hz: REMOTE_AUDIO_SAMPLE_RATE_HZ,
                channels: REMOTE_AUDIO_CHANNELS,
                frame_samples: REMOTE_AUDIO_FRAME_SAMPLES,
            };
            assert_eq!(
                envelope(offer()).validate_at(&source, &target, 10_100),
                Ok(())
            );
            let mut unsupported = offer();
            if let PersonalFabricAction::RemoteAudioOffer {
                protocol_version, ..
            } = &mut unsupported
            {
                *protocol_version = REMOTE_AUDIO_PROTOCOL_VERSION + 1;
            }
            assert_eq!(
                envelope(unsupported).validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioDecision {
                    session_id: [4; 16],
                    accepted: true,
                    authorization: None,
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioFrame {
                    session_id: [4; 16],
                    authorization: [5; 16],
                    sequence: 1,
                    media_ts: 48_000,
                    presentation_ns: 1_000_000_000 + REMOTE_AUDIO_GROUP_LATENCY_NS,
                    pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
                })
                .validate_at(&source, &target, 10_100),
                Ok(())
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioFrame {
                    session_id: [4; 16],
                    authorization: [5; 16],
                    sequence: 2,
                    media_ts: 0,
                    presentation_ns: REMOTE_AUDIO_GROUP_LATENCY_NS,
                    pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES + 4],
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioCancel {
                    session_id: [4; 16],
                })
                .validate_at(&source, &target, 10_100),
                Ok(())
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioCancel {
                    session_id: [0; 16],
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioKeepalive {
                    session_id: [4; 16],
                    authorization: [5; 16],
                    playout: None,
                })
                .validate_at(&source, &target, 10_100),
                Ok(())
            );
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioKeepalive {
                    session_id: [4; 16],
                    authorization: [0; 16],
                    playout: None,
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }

        fn bluetooth_handoff_actions_accept_only_canonical_bounded_ephemeral_identity() {
            let source = id("source");
            let target = id("target");
            let envelope = |action| PersonalFabricActionEnvelope {
                request_id: [3; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert_eq!(
                envelope(PersonalFabricAction::BluetoothHandoffPrepare {
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    name: "Headphones".into(),
                })
                .validate_at(&source, &target, 10_100),
                Ok(())
            );
            for address in [
                "AA:BB:CC:DD:EE",
                "AA-BB-CC-DD-EE-FF",
                "../../org/bluez/hci0",
                "GG:BB:CC:DD:EE:FF",
            ] {
                assert_eq!(
                    envelope(PersonalFabricAction::BluetoothHandoffPrepare {
                        address: address.into(),
                        name: "Headphones".into(),
                    })
                    .validate_at(&source, &target, 10_100),
                    Err(FabricError::InvalidAction)
                );
            }
            assert_eq!(
                envelope(PersonalFabricAction::BluetoothHandoffCommit {
                    request_id: [0; 16],
                    address: "AA:BB:CC:DD:EE:FF".into(),
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
            assert_eq!(
                envelope(PersonalFabricAction::BluetoothHandoffPairingOffered {
                    request_id: [5; 16],
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    authorization: [0; 16],
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }

        remote_audio_wire_is_versioned_bounded_and_capability_bearing();
        bluetooth_handoff_actions_accept_only_canonical_bounded_ephemeral_identity();
    }

    #[test]
    fn remote_audio_frames_play_after_capture_within_the_latency_bound() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        for presentation_ns in [999_999_999, 2_000_000_001] {
            assert_eq!(
                envelope(PersonalFabricAction::RemoteAudioFrame {
                    session_id: [4; 16],
                    authorization: [5; 16],
                    sequence: 1,
                    media_ts: 48_000,
                    presentation_ns,
                    pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction),
                "a frame played before its capture or beyond the latency bound"
            );
        }
    }

    #[test]
    fn speaker_group_wire_rejects_zero_identities_and_bad_names() {
        use crate::shell::speaker_group::{
            EndpointSelector, RoomPoint, SpeakerGroupId, SpeakerRole, StereoSide,
        };
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        let check = |action| envelope(action).validate_at(&source, &target, 10_100);
        let invite = |group, name: &str, output, role| PersonalFabricAction::SpeakerGroupInvite {
            group,
            name: name.into(),
            output,
            role,
        };
        let group = SpeakerGroupId([1; 16]);
        let output = EndpointSelector([2; 16]);
        let right = SpeakerRole::Stereo(StereoSide::Right);
        assert_eq!(check(invite(group, "Living room", output, right)), Ok(()));
        for bad in [
            invite(SpeakerGroupId([0; 16]), "Living room", output, right),
            invite(group, "", output, right),
            invite(group, "line\nbreak", output, right),
            invite(group, "Living room", EndpointSelector([0; 16]), right),
            invite(
                group,
                "Living room",
                output,
                SpeakerRole::RoomPoint(RoomPoint {
                    x_mm: i32::MAX,
                    y_mm: 0,
                    z_mm: 0,
                }),
            ),
            PersonalFabricAction::SpeakerGroupSessionOffer {
                session_id: [0; 16],
                group,
            },
            PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id: [4; 16],
                group,
                authorization: Some([0; 16]),
            },
            PersonalFabricAction::SpeakerGroupSessionStop {
                session_id: [4; 16],
                authorization: Some([0; 16]),
            },
            PersonalFabricAction::SpeakerGroupLeave {
                group: SpeakerGroupId([0; 16]),
            },
        ] {
            assert_eq!(
                check(bad.clone()),
                Err(FabricError::InvalidAction),
                "{bad:?}"
            );
        }
        assert_eq!(
            check(PersonalFabricAction::SpeakerGroupSessionDecision {
                session_id: [4; 16],
                group,
                authorization: None,
            }),
            Ok(())
        );
    }

    #[test]
    fn a_snapshot_cannot_carry_a_group_led_by_another_device() {
        use crate::shell::speaker_group::{
            EndpointSelector, SpeakerGroup, SpeakerGroupId, SpeakerGroupMember, SpeakerRole,
            SpeakerTrim, StereoSide,
        };
        let mut laptop = snapshot("laptop", 1, 0, FabricCapabilities::default());
        let member = |device: &str, side| SpeakerGroupMember {
            device: id(device),
            output: EndpointSelector([9; 16]),
            role: SpeakerRole::Stereo(side),
            trim: SpeakerTrim::default(),
        };
        laptop.speakers.groups.push(SpeakerGroup {
            id: SpeakerGroupId([5; 16]),
            name: "Desk".into(),
            leader: id("tablet"),
            members: vec![
                member("laptop", StereoSide::Left),
                member("tablet", StereoSide::Right),
            ],
            stamp: stamp("laptop", 1),
        });
        assert_eq!(laptop.validate(), Err(FabricError::ForeignAuthorship));
        laptop.speakers.groups[0].leader = id("laptop");
        assert_eq!(laptop.validate(), Ok(()));
        let mut fabric = DeviceFabric::default();
        fabric.upsert(laptop).unwrap();
        let group = fabric.speaker_group(SpeakerGroupId([5; 16])).unwrap();
        assert_eq!(
            fabric.admit_speaker_group(group),
            Err(FabricError::IneligibleSpeakerMember),
            "the laptop does not offer that output"
        );
    }
}
