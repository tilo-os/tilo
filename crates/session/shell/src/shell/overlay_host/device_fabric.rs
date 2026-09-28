//! Presentation contract for devices participating in personal continuity.
//!
//! Runtime-specific snapshots are flattened here into rows the overlay can
//! render without owning transport or device coordination.

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "presence and independently negotiated output/display capabilities are orthogonal facts"
)]
pub struct FabricDeviceView {
    pub id: crate::shell::personal_continuity::DeviceId,
    pub label: String,
    pub kind: crate::shell::device_fabric::DeviceKind,
    pub is_local: bool,
    pub online: bool,
    pub audio_output: bool,
    pub video_output: bool,
    pub display: bool,
    /// This remote device currently holds a lease issued by the local one.
    pub controls_local: bool,
    /// The local device currently holds a lease issued by this remote one.
    pub local_can_control: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricMediaView {
    pub session_id: String,
    pub title: String,
    pub playing: bool,
    pub has_video: bool,
    pub targets: Vec<crate::shell::personal_continuity::DeviceId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMediaOutputView {
    pub node_id: u32,
    pub label: String,
    pub is_default: bool,
    pub bluetooth_codec: Option<String>,
}

/// One microphone or other capture device on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMediaInputView {
    pub node_id: u32,
    pub label: String,
    pub is_default: bool,
    /// The input level (the source's volume) in percent, when known.
    pub level_percent: Option<u8>,
    pub muted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricTransferStatus {
    Sending,
    Sent,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FabricTransferView {
    pub target: crate::shell::personal_continuity::DeviceId,
    pub sent_bytes: u64,
    pub total_bytes: u64,
    pub status: FabricTransferStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothHandoffAuthorizationView {
    pub request_id: [u8; 16],
    pub accessory_name: String,
    pub source_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamepadAuthorizationView {
    pub forwarding_id: [u8; 16],
    pub controller_label: String,
    pub source_label: String,
    pub app_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceFabricView {
    pub bluetooth_authorizations: Vec<BluetoothHandoffAuthorizationView>,
    pub bluetooth_handoffs: Vec<crate::shell::device_fabric_runtime::BluetoothHandoffStatusView>,
    pub gamepad_authorizations: Vec<GamepadAuthorizationView>,
    pub local_gamepads: Vec<crate::shell::physical_gamepad::LocalPhysicalGamepadView>,
    pub devices: Vec<FabricDeviceView>,
    pub local_media: Vec<FabricMediaView>,
    pub media_handoffs: Vec<crate::shell::device_fabric_runtime::MediaHandoffStatusView>,
    pub local_outputs: Vec<LocalMediaOutputView>,
    pub local_inputs: Vec<LocalMediaInputView>,
    pub transfers: Vec<FabricTransferView>,
    pub remote_audio: crate::shell::remote_audio::RemoteAudioView,
    pub speaker_groups: crate::shell::speaker_group_runtime::SpeakerGroupsView,
    pub device_health: Vec<crate::shell::spatial_calibration::DeviceHealthView>,
    pub calibration_proposals: Vec<crate::shell::spatial_calibration::CalibrationProposal>,
    pub revertible_calibrations: Vec<crate::shell::personal_continuity::DeviceId>,
    pub tethering: Vec<crate::shell::personal_tethering_runtime::TetheringStatusView>,
    pub(crate) remote_capture: crate::shell::remote_capture::runtime::RemoteCaptureView,
    pub display_mirror_consents: Vec<crate::shell::device_fabric_runtime::DisplayMirrorConsentView>,
    pub display_mirrors: Vec<crate::shell::device_fabric_runtime::DisplayMirrorStatusView>,
    pub display_mirror_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceFabricUiAction {
    Snapshot,
    /// Bypass presentation caching for semantic action revalidation.
    SnapshotFresh,
    HandoffMedia {
        session_id: String,
        target: crate::shell::personal_continuity::DeviceId,
    },
    CancelMediaHandoff {
        request_id: [u8; 16],
    },
    RetryMediaHandoff {
        request_id: [u8; 16],
    },
    RouteLocalAudio {
        node_id: u32,
    },
    /// Record from this local input from now on.
    SelectLocalInput {
        node_id: u32,
    },
    /// Raise or lower this local input's level by one step.
    StepInputLevel {
        node_id: u32,
        raise: bool,
    },
    HandoffBluetooth {
        address: String,
        name: String,
        target: crate::shell::personal_continuity::DeviceId,
    },
    AuthorizeBluetoothHandoff {
        request_id: [u8; 16],
        approved: bool,
    },
    CancelBluetoothHandoff {
        request_id: [u8; 16],
    },
    RetryBluetoothHandoff {
        request_id: [u8; 16],
    },
    AuthorizeGamepadForwarding {
        forwarding_id: [u8; 16],
        approved: bool,
    },
    StartPhysicalGamepad {
        source_token: u64,
        target: crate::shell::personal_continuity::DeviceId,
    },
    StopPhysicalGamepad {
        session: u64,
    },
    StartRemoteAudio {
        source_token: u64,
        target: crate::shell::personal_continuity::DeviceId,
    },
    AuthorizeRemoteAudio {
        session_id: [u8; 16],
        approved: bool,
    },
    StopRemoteAudio {
        session_id: [u8; 16],
    },
    /// Answer "Join this speaker group?" or "Let it use this microphone?"
    /// on this device.
    ResolveSpeakerGroupConsent {
        group: crate::shell::speaker_group::SpeakerGroupId,
        kind: crate::shell::speaker_group_runtime::SpeakerConsentKind,
        approved: bool,
    },
    /// Withdraw this device from a group led by another device, or stop
    /// letting it listen with this device's microphone.
    LeaveSpeakerGroup {
        group: crate::shell::speaker_group::SpeakerGroupId,
        kind: crate::shell::speaker_group_runtime::SpeakerConsentKind,
    },
    StartSpeakerGroupAudio {
        source_token: u64,
        group: crate::shell::speaker_group::SpeakerGroupId,
    },
    PlayMediaOnSpeakerGroup {
        session_id: String,
        group: crate::shell::speaker_group::SpeakerGroupId,
    },
    StopSpeakerGroupSession {
        session_id: [u8; 16],
    },
    RequestRemoteCapture {
        peer: crate::shell::personal_continuity::DeviceId,
        kind: crate::shell::remote_capture::device::RemoteCaptureKind,
    },
    AuthorizeRemoteCapture {
        request_id: [u8; 16],
        approved: bool,
    },
    ApproveRemoteCaptureTarget {
        session_id: [u8; 16],
    },
    StopRemoteCapture {
        session_id: [u8; 16],
    },
    RequestDisplayMirror {
        target: crate::shell::personal_continuity::DeviceId,
    },
    ResolveDisplayMirror {
        session_id: [u8; 16],
        approved: bool,
    },
    StopDisplayMirror {
        session_id: [u8; 16],
    },
    RequestPhoneTethering {
        phone: crate::shell::personal_continuity::DeviceId,
    },
    AuthorizePhoneTethering {
        request_id: [u8; 16],
        approved: bool,
    },
    DisconnectPhoneTethering {
        request_id: [u8; 16],
    },
    Place {
        device: crate::shell::personal_continuity::DeviceId,
        edge: crate::shell::device_fabric::DisplayEdge,
    },
    ApplyCalibration {
        device: crate::shell::personal_continuity::DeviceId,
    },
    DismissCalibration {
        device: crate::shell::personal_continuity::DeviceId,
    },
    RevertCalibration {
        device: crate::shell::personal_continuity::DeviceId,
    },
    GrantControl {
        controller: crate::shell::personal_continuity::DeviceId,
    },
    RevokeControl {
        controller: crate::shell::personal_continuity::DeviceId,
    },
}

pub type DeviceFabricBridge =
    std::rc::Rc<dyn Fn(DeviceFabricUiAction) -> Result<DeviceFabricView, String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeContinuityUiAction {
    Snapshot,
    Propose {
        session: String,
        target: crate::shell::personal_continuity::DeviceId,
    },
    ResolveConsent {
        session: String,
        approved: bool,
    },
}

pub type RealtimeContinuityBridge = std::rc::Rc<
    dyn Fn(
        RealtimeContinuityUiAction,
    ) -> Result<crate::shell::collab_broker_capnp::RealtimeContinuityUiView, String>,
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeviceFabricPaneRow {
    CalibrationProposal(crate::shell::spatial_calibration::CalibrationProposal),
    CalibrationRevert(crate::shell::personal_continuity::DeviceId),
    RemoteAudioConsent(crate::shell::remote_audio::RemoteAudioConsentView),
    RemoteAudioSource(crate::shell::remote_audio::RemoteAudioSourceView),
    RemoteAudioRoute(crate::shell::remote_audio::RemoteAudioRouteView),
    SpeakerConsent(crate::shell::speaker_group_runtime::SpeakerConsentView),
    SpeakerSession(crate::shell::speaker_group_runtime::SpeakerSessionView),
    SpeakerGroup(crate::shell::speaker_group_runtime::SpeakerGroupView),
    SpeakerMembership(crate::shell::speaker_group_runtime::SpeakerMembershipView),
    /// Opens Settings → Speakers to make a new set.
    SpeakerSetup,
    RemoteCaptureConsent(crate::shell::remote_capture::runtime::RemoteCaptureConsentView),
    RemoteCaptureSource(crate::shell::remote_capture::device::RemoteCaptureKind),
    RemoteCaptureSession(crate::shell::remote_capture::runtime::RemoteCaptureSessionView),
    RemoteCaptureDegraded(crate::shell::remote_capture::runtime::RemoteCaptureDegradedView),
    DisplayMirrorConsent(crate::shell::device_fabric_runtime::DisplayMirrorConsentView),
    DisplayMirrorSource,
    DisplayMirrorStatus(crate::shell::device_fabric_runtime::DisplayMirrorStatusView),
    RealtimeConsent(crate::shell::collab_broker_capnp::RealtimeConsentUiView),
    RealtimeSource(crate::shell::collab_broker_capnp::RealtimeSourceUiView),
    BluetoothAuthorization(BluetoothHandoffAuthorizationView),
    BluetoothHandoff(crate::shell::device_fabric_runtime::BluetoothHandoffStatusView),
    GamepadAuthorization(GamepadAuthorizationView),
    LocalGamepad(crate::shell::physical_gamepad::LocalPhysicalGamepadView),
    MediaHandoff(crate::shell::device_fabric_runtime::MediaHandoffStatusView),
    Media(FabricMediaView),
    LocalOutput(LocalMediaOutputView),
    LocalInput(LocalMediaInputView),
    Tethering(crate::shell::personal_tethering_runtime::TetheringStatusView),
    Device(FabricDeviceView),
}

#[expect(
    clippy::too_many_lines,
    reason = "a declarative row list for every fabric pane section in display order"
)]
pub(super) fn device_fabric_pane_rows(
    view: &DeviceFabricView,
    realtime: &crate::shell::collab_broker_capnp::RealtimeContinuityUiView,
) -> Vec<DeviceFabricPaneRow> {
    view.calibration_proposals
        .iter()
        .cloned()
        .map(DeviceFabricPaneRow::CalibrationProposal)
        .chain(
            view.revertible_calibrations
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::CalibrationRevert),
        )
        .chain(
            view.remote_audio
                .consents
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteAudioConsent),
        )
        .chain(
            view.speaker_groups
                .consents
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::SpeakerConsent),
        )
        .chain(
            view.remote_audio
                .routes
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteAudioRoute),
        )
        .chain(
            view.speaker_groups
                .sessions
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::SpeakerSession),
        )
        .chain(
            view.remote_audio
                .sources
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteAudioSource),
        )
        .chain(
            view.remote_capture
                .consents
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteCaptureConsent),
        )
        .chain(
            [
                (
                    crate::shell::remote_capture::device::RemoteCaptureKind::Camera,
                    view.remote_capture.local_receivers.camera,
                ),
                (
                    crate::shell::remote_capture::device::RemoteCaptureKind::Microphone,
                    view.remote_capture.local_receivers.microphone,
                ),
                (
                    crate::shell::remote_capture::device::RemoteCaptureKind::DocumentScanner,
                    view.remote_capture.local_receivers.scanner,
                ),
            ]
            .into_iter()
            .filter_map(|(kind, available)| available.then_some(kind))
            .map(DeviceFabricPaneRow::RemoteCaptureSource),
        )
        .chain(
            view.remote_capture
                .sessions
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteCaptureSession),
        )
        .chain(
            view.remote_capture
                .degraded
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteCaptureDegraded),
        )
        .chain(
            view.display_mirror_consents
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::DisplayMirrorConsent),
        )
        .chain(
            view.display_mirror_available
                .then_some(DeviceFabricPaneRow::DisplayMirrorSource),
        )
        .chain(
            view.display_mirrors
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::DisplayMirrorStatus),
        )
        .chain(
            realtime
                .consents
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RealtimeConsent),
        )
        .chain(
            realtime
                .sources
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RealtimeSource),
        )
        .chain(
            view.bluetooth_authorizations
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::BluetoothAuthorization),
        )
        .chain(
            view.bluetooth_handoffs
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::BluetoothHandoff),
        )
        .chain(
            view.gamepad_authorizations
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::GamepadAuthorization),
        )
        .chain(
            view.local_gamepads
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::LocalGamepad),
        )
        .chain(
            view.media_handoffs
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::MediaHandoff),
        )
        .chain(
            view.local_media
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::Media),
        )
        .chain(
            view.local_outputs
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::LocalOutput),
        )
        .chain(
            view.local_inputs
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::LocalInput),
        )
        .chain(
            view.tethering
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::Tethering),
        )
        .chain(
            view.speaker_groups
                .groups
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::SpeakerGroup),
        )
        .chain(
            view.speaker_groups
                .memberships
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::SpeakerMembership),
        )
        .chain(
            (view
                .devices
                .iter()
                .filter(|device| device.online && device.audio_output)
                .count()
                >= 2)
                .then_some(DeviceFabricPaneRow::SpeakerSetup),
        )
        .chain(
            view.devices
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::Device),
        )
        .collect()
}

/// "2 of 2 speakers ready" and the like.
pub(super) fn speaker_group_summary(
    group: &crate::shell::speaker_group_runtime::SpeakerGroupView,
) -> String {
    let total = group.members.len();
    let ready = group.ready_members();
    let waiting = group
        .members
        .iter()
        .filter(|member| {
            member.state == crate::shell::speaker_group::SpeakerMemberState::AwaitingConsent
        })
        .count();
    let counts: [(&str, ui::i18n::ArgValue); 3] = [
        ("ready", ready.into()),
        ("total", total.into()),
        ("waiting", waiting.into()),
    ];
    if waiting > 0 {
        crate::shell::i18n::t_args("speaker-group-summary-waiting", &counts)
    } else {
        crate::shell::i18n::t_args("speaker-group-summary", &counts[..2])
    }
}

/// "Right speaker" from "right speaker".
pub(super) fn sentence_case(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// The catalogue key naming a remote input ("camera"), as the `$input` of a
/// sentence.
pub(super) const fn remote_capture_kind_key(
    kind: crate::shell::remote_capture::device::RemoteCaptureKind,
) -> &'static str {
    match kind {
        crate::shell::remote_capture::device::RemoteCaptureKind::Camera => "remote-input-camera",
        crate::shell::remote_capture::device::RemoteCaptureKind::Microphone => {
            "remote-input-microphone"
        }
        crate::shell::remote_capture::device::RemoteCaptureKind::DocumentScanner => {
            "remote-input-scanner"
        }
    }
}

/// The catalogue key of the reason, as the `$reason` of a sentence.
pub(super) const fn display_mirror_failure_key(
    failure: crate::shell::device_fabric_runtime::DisplayMirrorFailure,
) -> &'static str {
    use crate::shell::device_fabric_runtime::DisplayMirrorFailure;
    match failure {
        DisplayMirrorFailure::Denied => "mirror-failure-denied",
        DisplayMirrorFailure::PeerOffline => "mirror-failure-peer-offline",
        DisplayMirrorFailure::TimedOut => "mirror-failure-timed-out",
        DisplayMirrorFailure::Transport => "mirror-failure-transport",
        DisplayMirrorFailure::Policy => "mirror-failure-policy",
    }
}

/// The catalogue key of the reason, as the `$reason` of a sentence.
pub(super) const fn media_handoff_failure_key(
    failure: crate::shell::device_fabric::MediaHandoffFailure,
) -> &'static str {
    use crate::shell::device_fabric::MediaHandoffFailure;
    match failure {
        MediaHandoffFailure::DestinationUnavailable => {
            "media-handoff-failure-destination-unavailable"
        }
        MediaHandoffFailure::UnsupportedOutput => "media-handoff-failure-unsupported-output",
        MediaHandoffFailure::InvalidWorkspace => "media-handoff-failure-invalid-workspace",
        MediaHandoffFailure::OpenFailed => "media-handoff-failure-open-failed",
        MediaHandoffFailure::TimedOut => "media-handoff-failure-timed-out",
        MediaHandoffFailure::Cancelled => "media-handoff-failure-cancelled",
    }
}

/// The catalogue key of the reason, as the `$reason` of a sentence.
pub(super) const fn bluetooth_handoff_failure_key(
    failure: crate::shell::device_fabric_runtime::BluetoothHandoffFailure,
) -> &'static str {
    use crate::shell::device_fabric_runtime::BluetoothHandoffFailure;
    match failure {
        BluetoothHandoffFailure::InspectTarget => "bluetooth-handoff-failure-inspect-target",
        BluetoothHandoffFailure::TargetUnavailable => {
            "bluetooth-handoff-failure-target-unavailable"
        }
        BluetoothHandoffFailure::DisconnectSource => "bluetooth-handoff-failure-disconnect-source",
        BluetoothHandoffFailure::PairTarget => "bluetooth-handoff-failure-pair-target",
        BluetoothHandoffFailure::ConnectTarget => "bluetooth-handoff-failure-connect-target",
        BluetoothHandoffFailure::Send => "bluetooth-handoff-failure-send",
        BluetoothHandoffFailure::TimedOut => "bluetooth-handoff-failure-timed-out",
        BluetoothHandoffFailure::Rollback => "bluetooth-handoff-failure-rollback",
    }
}
