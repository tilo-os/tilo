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
    pub transfers: Vec<FabricTransferView>,
    pub remote_audio: crate::shell::remote_audio::RemoteAudioView,
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
    Tethering(crate::shell::personal_tethering_runtime::TetheringStatusView),
    Device(FabricDeviceView),
}

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
            view.remote_audio
                .routes
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::RemoteAudioRoute),
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
            view.tethering
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::Tethering),
        )
        .chain(
            view.devices
                .iter()
                .cloned()
                .map(DeviceFabricPaneRow::Device),
        )
        .collect()
}

pub(super) fn remote_capture_kind_label(
    kind: crate::shell::remote_capture::device::RemoteCaptureKind,
) -> &'static str {
    match kind {
        crate::shell::remote_capture::device::RemoteCaptureKind::Camera => "camera",
        crate::shell::remote_capture::device::RemoteCaptureKind::Microphone => "microphone",
        crate::shell::remote_capture::device::RemoteCaptureKind::DocumentScanner => "scanner",
    }
}

pub(super) fn display_mirror_failure_label(
    failure: crate::shell::device_fabric_runtime::DisplayMirrorFailure,
) -> &'static str {
    use crate::shell::device_fabric_runtime::DisplayMirrorFailure;
    match failure {
        DisplayMirrorFailure::Denied => "declined on the target",
        DisplayMirrorFailure::PeerOffline => "certified peer went offline",
        DisplayMirrorFailure::TimedOut => "session timed out",
        DisplayMirrorFailure::Transport => "private video route failed",
        DisplayMirrorFailure::Policy => "display continuity was disabled",
    }
}

pub(super) fn media_handoff_failure_label(
    failure: crate::shell::device_fabric::MediaHandoffFailure,
) -> &'static str {
    use crate::shell::device_fabric::MediaHandoffFailure;
    match failure {
        MediaHandoffFailure::DestinationUnavailable => "destination unavailable",
        MediaHandoffFailure::UnsupportedOutput => "destination cannot play this media",
        MediaHandoffFailure::InvalidWorkspace => "workspace is unavailable there",
        MediaHandoffFailure::OpenFailed => "destination could not open it",
        MediaHandoffFailure::TimedOut => "destination did not respond",
        MediaHandoffFailure::Cancelled => "cancelled",
    }
}

pub(super) fn bluetooth_handoff_failure_label(
    failure: crate::shell::device_fabric_runtime::BluetoothHandoffFailure,
) -> &'static str {
    use crate::shell::device_fabric_runtime::BluetoothHandoffFailure;
    match failure {
        BluetoothHandoffFailure::InspectTarget => "target inspection failed",
        BluetoothHandoffFailure::TargetUnavailable => "target unavailable or declined",
        BluetoothHandoffFailure::DisconnectSource => "source could not release accessory",
        BluetoothHandoffFailure::PairTarget => "target pairing failed",
        BluetoothHandoffFailure::ConnectTarget => "target connection failed",
        BluetoothHandoffFailure::Send => "certified route was interrupted",
        BluetoothHandoffFailure::TimedOut => "transaction timed out",
        BluetoothHandoffFailure::Rollback => "source rollback failed",
    }
}
