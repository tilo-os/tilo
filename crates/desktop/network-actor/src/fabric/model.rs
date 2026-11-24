//! The policy and state shared between one person's certified Tilo devices.
//! Tilo does not equate proximity, Bluetooth pairing or a shared network with trust.
//! The private same-identity connection authenticates and encrypts peers; this module
//! applies one policy model to media, clipboard, roaming preferences, secret
//! references and cross-screen input.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fabric::MAX_FABRIC_ACTION_BYTES;
use crate::fabric::carried::CarriedDrag;
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
/// More network interfaces than any one device wakes through.
pub const MAX_WAKE_ROUTES: usize = 8;
pub const MAX_CLIPBOARD_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_ITEM_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_LEASE_MS: u64 = 15 * 60 * 1000;
pub const MAX_FABRIC_ACTION_AGE_MS: u64 = 2 * 60 * 1000;
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
    pub(super) fn valid(&self) -> bool {
        // A unicast, non-zero MAC; a multicast one cannot address a NIC.
        self.mac != [0; 6]
            && self.mac.first().is_some_and(|octet| octet & 1 == 0)
            && self.subnet.is_none_or(Ipv4Subnet::is_canonical)
    }
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
    /// addresses an immutable private spool object. A played-on transfer is
    /// admitted by the `ResumeMedia` that named its ID, never by Files.
    NearbyTransferOffer {
        transfer_id: [u8; 16],
        name: String,
        kind: tilo_nearby_service::TransferKind,
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
        notification: crate::fabric::notification::ContinuityNotificationProjection,
    },
    NotificationRetire {
        id: crate::fabric::notification::ContinuityNotificationId,
    },
    NotificationDismiss {
        id: crate::fabric::notification::ContinuityNotificationId,
    },
    NotificationInvoke {
        id: crate::fabric::notification::ContinuityNotificationId,
        action_key: String,
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
        playout: Option<crate::fabric::clock::ClockSyncState>,
    },
    /// Ask one exact member device to join a group. The member's own
    /// compositor asks its user; nothing plays until it approves.
    SpeakerGroupInvite {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        name: String,
        output: crate::fabric::speaker_group::EndpointSelector,
        role: crate::fabric::speaker_group::SpeakerRole,
    },
    /// The member's answer. The durable consent itself is the membership the
    /// member authors in its own snapshot; this only reports promptly.
    SpeakerGroupDecision {
        group: crate::fabric::speaker_group::SpeakerGroupId,
        accepted: bool,
    },
    /// The leader removed this member or deleted the group, or the member
    /// left. Either side tears down every session of the group.
    SpeakerGroupLeave {
        group: crate::fabric::speaker_group::SpeakerGroupId,
    },
    /// Start one playback session on a consented member. Any of the person's
    /// devices may play to a group; the member admits it only while its own
    /// membership still covers the group as published by the leader.
    SpeakerGroupSessionOffer {
        session_id: [u8; 16],
        group: crate::fabric::speaker_group::SpeakerGroupId,
    },
    /// Admission result. Approval carries a fresh random capability bound to
    /// the exact source and session.
    SpeakerGroupSessionDecision {
        session_id: [u8; 16],
        group: crate::fabric::speaker_group::SpeakerGroupId,
        authorization: Option<[u8; 16]>,
    },
    SpeakerGroupSessionStop {
        session_id: [u8; 16],
        authorization: Option<[u8; 16]>,
    },
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
    /// The legacy app windows this device shows right now, each at the size it shows it.
    /// The device running a listed window streams that one window here and stops once a
    /// refresh no longer lists it or the refreshes stop. `decode` is what this device's
    /// hardware decoders take, so the host encodes nothing they would hand back to the
    /// CPU.
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
    /// Certify a device the sender paired: the devices that can approve it
    /// ask their person (the shell's device certification). The ask,
    /// postcard-encoded.
    CertifyAsk {
        ask: Vec<u8>,
    },
    /// A device's postcard-encoded answer to a [`Self::CertifyAsk`].
    CertifyAnswer {
        answer: Vec<u8>,
    },
    /// The sender's pointer pushes against the edge that leads to the
    /// receiving device: grant it an [`InputLease`] if this device takes
    /// the person's input from their other devices.
    InputLeaseAsk,
    /// What the pointer under `lease_nonce` carried across to the
    /// receiving device in the middle of a drag. The drag goes on there.
    DragCarried {
        lease_nonce: [u8; 16],
        drag: CarriedDrag,
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
            PersonalFabricAction::SpeakerGroupInvite {
                group,
                name,
                output,
                role,
            } => {
                if !group.valid()
                    || !crate::fabric::speaker_group::valid_group_name(name)
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
            PersonalFabricAction::InputLeaseAsk => {}
            PersonalFabricAction::DragCarried { lease_nonce, drag } => {
                if lease_nonce == &[0; 16] || !drag.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::CertifyAsk { ask: message }
            | PersonalFabricAction::CertifyAnswer { answer: message } => {
                if message.is_empty() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::WakeProbe { nonce }
            | PersonalFabricAction::WakeProbeReply { nonce }
            | PersonalFabricAction::FabricClockProbe { nonce, .. }
            | PersonalFabricAction::InputLeaseRevoked { nonce } => {
                if nonce == &[0; 16] {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::FabricClockReport { report } => {
                if !report.valid() {
                    return Err(FabricError::InvalidAction);
                }
            }
            PersonalFabricAction::BluetoothHandoffPrepare { address, name } => {
                if !os_actions::bluez::valid_address(address)
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
                if request_id == &[0; 16] || !os_actions::bluez::valid_address(address) {
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
                if !tilo_nearby_service::valid_nearby_file_name(file_name)
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
                if request_id == &[0; 16] || !tilo_nearby_service::valid_nearby_file_name(file_name)
                {
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
                    || !tilo_nearby_service::valid_nearby_file_name(name)
                    || *total_bytes == 0
                    || *total_bytes > tilo_nearby_service::MAX_TRANSFER_BYTES
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
                    || *committed_offset > tilo_nearby_service::MAX_TRANSFER_BYTES
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
            PersonalFabricAction::GamepadForwardRequest {
                forwarding_id,
                app_id,
                workspace_id,
                session_id,
                label,
            } => {
                if forwarding_id == &[0; 16]
                    || !valid_fabric_identifier(app_id, 256)
                    || !valid_fabric_identifier(workspace_id, 128)
                    || !valid_fabric_identifier(session_id, 256)
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
            PersonalFabricAction::InputLeaseGranted { lease } => {
                if &lease.target != transport_source
                    || &lease.controller != local_target
                    || lease.nonce == [0; 16]
                    || lease.validate_at(now_unix_ms).is_err()
                {
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
    /// Amounts in [`ScrollUnit::STEPS`] of `unit`, so a high-resolution
    /// wheel's part notches and a touchpad's sub-pixel motion add up as
    /// they do on the device they came from.
    Scroll {
        horizontal: i32,
        vertical: i32,
        unit: ScrollUnit,
    },
    Key {
        keycode: u32,
        pressed: bool,
        modifiers: u32,
    },
}

/// What a scroll moves by: wheel lines, or the pixels of finger and
/// continuous scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollUnit {
    Lines,
    Pixels,
}

impl ScrollUnit {
    /// Steps a scroll amount counts per line or pixel.
    pub const STEPS: f64 = 120.0;
}

impl FabricInputEvent {
    pub fn validate(self) -> bool {
        match self {
            Self::PointerMotion { x, y } => {
                x.unsigned_abs() <= 1_000_000 && y.unsigned_abs() <= 1_000_000
            }
            Self::PointerButton { button, .. } => (1..=0x3ff).contains(&button),
            // A finger scroll ends with an empty frame (kinetic scrolling
            // starts from it); an empty wheel step means nothing.
            Self::Scroll {
                horizontal,
                vertical,
                unit,
            } => {
                horizontal.unsigned_abs() <= 1_000_000
                    && vertical.unsigned_abs() <= 1_000_000
                    && (unit == ScrollUnit::Pixels || horizontal != 0 || vertical != 0)
            }
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
    /// # Errors
    /// An invalid lease shape, or one that has expired.
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

fn valid_bluetooth_pairing_action(
    request_id: &[u8; 16],
    address: &str,
    authorization: &[u8; 16],
) -> bool {
    request_id != &[0; 16] && authorization != &[0; 16] && os_actions::bluez::valid_address(address)
}

pub(super) fn unique<T: Ord>(items: impl IntoIterator<Item = T>) -> Result<(), FabricError> {
    let mut seen = BTreeSet::new();
    if items.into_iter().all(|item| seen.insert(item)) {
        Ok(())
    } else {
        Err(FabricError::DuplicateItem)
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
    #[error("invalid display placement")]
    InvalidDisplayPlacement,
    #[error("invalid input lease")]
    InvalidInputLease,
    #[error("input lease has expired")]
    ExpiredInputLease,
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
    #[error("invalid wake route")]
    InvalidWakeRoute,
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
