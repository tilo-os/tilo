//! The policy and state shared between one person's certified Tilo devices.
//! Tilo does not equate proximity, Bluetooth pairing or a shared network with trust.
//! The private same-identity connection authenticates and encrypts peers; this module
//! applies one policy model to media, clipboard, roaming preferences, secret
//! references and cross-screen input.

use serde::{Deserialize, Serialize};

use crate::fabric::carried::CarriedDrag;
use tilo_continuity::notifications::{ContinuityNotificationProjection, NotificationId};
use tilo_fabric::wake::WakeOutcome;
use tilo_fabric::{DeviceId, FabricError, MAX_FABRIC_ACTION_BYTES, valid_fabric_identifier};

/// Locally derived focus identity of a physical gamepad's target. None of
/// these fields are accepted from a remote peer or an untrusted application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalGamepadBinding {
    pub app_id: String,
    pub workspace_id: String,
    pub session_id: String,
}

pub const MAX_INPUT_LEASE_MS: u64 = 15 * 60 * 1000;
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
        notification: ContinuityNotificationProjection,
    },
    NotificationRetire {
        id: NotificationId,
    },
    NotificationDismiss {
        id: NotificationId,
    },
    NotificationInvoke {
        id: NotificationId,
        action_key: String,
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
        tilo_fabric::Envelope {
            request_id: self.request_id,
            source: self.source.clone(),
            target: self.target.clone(),
            issued_unix_ms: self.issued_unix_ms,
        }
        .check_at(transport_source, local_target, now_unix_ms)?;
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

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
