//! Semantic action projection for compositor-owned continuity services.
//!
//! This provider does not own transport or authority. It projects only the
//! exact objects returned by the trusted chrome bridges and re-reads that
//! projection immediately before dispatch, so a stale catalogue cannot turn
//! an offline peer or expired consent into ambient authority.

use std::rc::Rc;

use crate::shell::device_fabric::{DeviceKind, DisplayEdge};
use crate::shell::device_fabric_runtime::MediaHandoffPhase;
use crate::shell::overlay_host::{
    DeviceFabricBridge, DeviceFabricUiAction, DeviceFabricView, RealtimeContinuityBridge,
    RealtimeContinuityUiAction,
};
use crate::shell::personal_tethering_runtime::TetheringPhase;
use crate::shell::remote_capture_device::RemoteCaptureKind;
use crate::shell::remote_capture_runtime::RemoteCaptureSessionPhase;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const DEVICE_PROVIDER_ID: &str = "continuity.devices";
pub const CALL_PROVIDER_ID: &str = "continuity.calls";

#[derive(Clone)]
struct DeviceAction {
    id: String,
    title: String,
    subtitle: String,
    action: DeviceFabricUiAction,
    confirm: bool,
}

#[derive(Clone)]
struct CallAction {
    id: String,
    title: String,
    subtitle: String,
    action: RealtimeContinuityUiAction,
    confirm: bool,
}

fn opaque(bytes: &[u8]) -> String {
    let digest = blake3::derive_key("dev.tilo.continuity-action-locator.v1", bytes);
    let mut encoded = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn matches(needle: &str, title: &str, subtitle: &str, id: &str) -> Option<i32> {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Some(0);
    }
    let title = title.to_ascii_lowercase();
    let subtitle = subtitle.to_ascii_lowercase();
    let id = id.to_ascii_lowercase();
    if title.starts_with(&needle) {
        Some(100)
    } else if id == needle {
        Some(90)
    } else if title.contains(&needle) {
        Some(50)
    } else if subtitle.contains(&needle) || id.contains(&needle) {
        Some(25)
    } else {
        None
    }
}

fn confirmation_params(confirm: bool) -> Vec<ActionParam> {
    confirm
        .then(|| {
            vec![ActionParam::required(
                "confirm",
                ParamType::Boolean,
                "Explicitly confirm this consequential continuity action",
            )]
        })
        .unwrap_or_default()
}

fn contract(confirm: bool) -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        confirmation: if confirm {
            tilo_actions::ConfirmationPolicy::Confirm
        } else {
            tilo_actions::ConfirmationPolicy::None
        },
        ..tilo_actions::ActionContract::default()
    }
}

fn ensure_confirmed(args: &ActionArgs, required: bool) -> Result<(), ActionError> {
    if required && args.boolean("confirm") != Some(true) {
        return Err(ActionError::Refused(
            "explicit confirmation is required".into(),
        ));
    }
    Ok(())
}

fn dispatch_device_action(
    bridge: &DeviceFabricBridge,
    expected: &DeviceFabricUiAction,
    confirmed: bool,
    requires_confirmation: bool,
) -> Result<(), ActionError> {
    if requires_confirmation && !confirmed {
        return Err(ActionError::Refused(
            "explicit confirmation is required".into(),
        ));
    }
    let current = bridge(DeviceFabricUiAction::Snapshot).map_err(ActionError::Refused)?;
    if !device_actions(&current)
        .iter()
        .any(|entry| entry.action == *expected)
    {
        return Err(ActionError::Refused(
            "continuity action is no longer available".into(),
        ));
    }
    bridge(expected.clone()).map_err(ActionError::Refused)?;
    Ok(())
}

fn device_actions(view: &DeviceFabricView) -> Vec<DeviceAction> {
    let mut actions = Vec::new();
    let label_for = |id: &crate::shell::personal_continuity::DeviceId| {
        view.devices
            .iter()
            .find(|device| &device.id == id)
            .map_or_else(
                || "certified device".to_string(),
                |device| device.label.clone(),
            )
    };
    let eligible = view
        .devices
        .iter()
        .filter(|device| !device.is_local && device.online)
        .collect::<Vec<_>>();

    for media in &view.local_media {
        for target in &media.targets {
            let label = label_for(target);
            actions.push(DeviceAction {
                id: format!(
                    "media.handoff.{}.{}",
                    opaque(media.session_id.as_bytes()),
                    opaque(target.as_str().as_bytes())
                ),
                title: format!("Play {} on {label}", media.title),
                subtitle: "Move this live media session to an exact certified device".into(),
                action: DeviceFabricUiAction::HandoffMedia {
                    session_id: media.session_id.clone(),
                    target: target.clone(),
                },
                confirm: true,
            });
        }
    }
    for status in &view.media_handoffs {
        let (verb, action) = match status.phase {
            MediaHandoffPhase::Pending => (
                "Cancel",
                DeviceFabricUiAction::CancelMediaHandoff {
                    request_id: status.request_id,
                },
            ),
            MediaHandoffPhase::Failed(_) | MediaHandoffPhase::Cancelled => (
                "Retry",
                DeviceFabricUiAction::RetryMediaHandoff {
                    request_id: status.request_id,
                },
            ),
            MediaHandoffPhase::Completed => continue,
        };
        actions.push(DeviceAction {
            id: format!(
                "media.{}.{}",
                verb.to_ascii_lowercase(),
                opaque(&status.request_id)
            ),
            title: format!("{verb} moving {}", status.title),
            subtitle: format!("Media handoff to {}", status.target_label),
            action,
            confirm: false,
        });
    }
    for output in view
        .local_outputs
        .iter()
        .filter(|output| !output.is_default)
    {
        actions.push(DeviceAction {
            id: format!("audio.route.{}", opaque(&output.node_id.to_le_bytes())),
            title: format!("Route audio to {}", output.label),
            subtitle: "Choose an exact currently enumerated local audio output".into(),
            action: DeviceFabricUiAction::RouteLocalAudio {
                node_id: output.node_id,
            },
            confirm: false,
        });
    }
    for source in &view.remote_audio.sources {
        for target in eligible
            .iter()
            .copied()
            .filter(|device| device.audio_output)
        {
            actions.push(DeviceAction {
                id: format!(
                    "audio.forward.{}.{}",
                    opaque(&source.token.to_le_bytes()),
                    opaque(target.id.as_str().as_bytes())
                ),
                title: format!("Play {} on {}", source.label, target.label),
                subtitle: "Request a consented live-audio route to this certified device".into(),
                action: DeviceFabricUiAction::StartRemoteAudio {
                    source_token: source.token,
                    target: target.id.clone(),
                },
                confirm: true,
            });
        }
    }
    for consent in &view.remote_audio.consents {
        for (approved, verb) in [(true, "Allow"), (false, "Decline")] {
            actions.push(DeviceAction {
                id: format!(
                    "audio.consent.{}.{}",
                    if approved { "allow" } else { "decline" },
                    opaque(&consent.session_id)
                ),
                title: format!("{verb} live audio from {}", consent.label),
                subtitle: "Resolve this exact remote-audio consent request".into(),
                action: DeviceFabricUiAction::AuthorizeRemoteAudio {
                    session_id: consent.session_id,
                    approved,
                },
                confirm: approved,
            });
        }
    }
    for route in &view.remote_audio.routes {
        actions.push(DeviceAction {
            id: format!("audio.stop.{}", opaque(&route.session_id)),
            title: format!("Stop routing {}", route.label),
            subtitle: "Close and neutralize this exact live-audio route".into(),
            action: DeviceFabricUiAction::StopRemoteAudio {
                session_id: route.session_id,
            },
            confirm: false,
        });
    }

    for device in eligible.iter().copied().filter(|device| device.display) {
        for (edge, edge_name) in [
            (DisplayEdge::Left, "left"),
            (DisplayEdge::Right, "right"),
            (DisplayEdge::Top, "above"),
            (DisplayEdge::Bottom, "below"),
        ] {
            actions.push(DeviceAction {
                id: format!(
                    "spatial.place.{edge_name}.{}",
                    opaque(device.id.as_str().as_bytes())
                ),
                title: format!("Place {} {edge_name}", device.label),
                subtitle: "Arrange an exact certified display around this device".into(),
                action: DeviceFabricUiAction::Place {
                    device: device.id.clone(),
                    edge,
                },
                confirm: true,
            });
        }
        let (verb, action) = if device.controls_local {
            (
                "Revoke control from",
                DeviceFabricUiAction::RevokeControl {
                    controller: device.id.clone(),
                },
            )
        } else {
            (
                "Grant control to",
                DeviceFabricUiAction::GrantControl {
                    controller: device.id.clone(),
                },
            )
        };
        actions.push(DeviceAction {
            id: format!(
                "spatial.control.{}.{}",
                if device.controls_local {
                    "revoke"
                } else {
                    "grant"
                },
                opaque(device.id.as_str().as_bytes())
            ),
            title: format!("{verb} {}", device.label),
            subtitle: "Change the short-lived cross-device input lease".into(),
            action,
            confirm: !device.controls_local,
        });
    }
    for proposal in &view.calibration_proposals {
        for (verb, action) in [
            (
                "apply",
                DeviceFabricUiAction::ApplyCalibration {
                    device: proposal.peer.clone(),
                },
            ),
            (
                "dismiss",
                DeviceFabricUiAction::DismissCalibration {
                    device: proposal.peer.clone(),
                },
            ),
        ] {
            actions.push(DeviceAction {
                id: format!(
                    "spatial.calibration.{verb}.{}",
                    opaque(proposal.peer.as_str().as_bytes())
                ),
                title: format!(
                    "{} placement suggestion for {}",
                    capitalize(verb),
                    label_for(&proposal.peer)
                ),
                subtitle: "Act on the current proximity-derived placement proposal".into(),
                action,
                confirm: verb == "apply",
            });
        }
    }
    for peer in &view.revertible_calibrations {
        actions.push(DeviceAction {
            id: format!(
                "spatial.calibration.revert.{}",
                opaque(peer.as_str().as_bytes())
            ),
            title: format!("Revert placement for {}", label_for(peer)),
            subtitle: "Restore the previous certified-display placement".into(),
            action: DeviceFabricUiAction::RevertCalibration {
                device: peer.clone(),
            },
            confirm: false,
        });
    }

    let tethering_peers = view
        .tethering
        .iter()
        .map(|status| &status.peer)
        .collect::<Vec<_>>();
    for phone in eligible
        .iter()
        .copied()
        .filter(|device| device.kind == DeviceKind::Phone && !tethering_peers.contains(&&device.id))
    {
        actions.push(DeviceAction {
            id: format!("tether.request.{}", opaque(phone.id.as_str().as_bytes())),
            title: format!("Ask {} for tethering", phone.label),
            subtitle: "Request a short-lived, phone-approved connection".into(),
            action: DeviceFabricUiAction::RequestPhoneTethering {
                phone: phone.id.clone(),
            },
            confirm: true,
        });
    }
    for status in &view.tethering {
        if status.phase == TetheringPhase::ConsentRequired {
            for (approved, verb) in [(true, "Allow"), (false, "Decline")] {
                actions.push(DeviceAction {
                    id: format!(
                        "tether.consent.{}.{}",
                        if approved { "allow" } else { "decline" },
                        opaque(&status.request_id)
                    ),
                    title: format!("{verb} tethering for {}", status.peer_label),
                    subtitle: "Resolve this exact phone-side consent request".into(),
                    action: DeviceFabricUiAction::AuthorizePhoneTethering {
                        request_id: status.request_id,
                        approved,
                    },
                    confirm: approved,
                });
            }
        }
        if status.can_disconnect() {
            actions.push(DeviceAction {
                id: format!("tether.disconnect.{}", opaque(&status.request_id)),
                title: format!("Disconnect tethering from {}", status.peer_label),
                subtitle: "Revoke this exact tethering request and its network route".into(),
                action: DeviceFabricUiAction::DisconnectPhoneTethering {
                    request_id: status.request_id,
                },
                confirm: false,
            });
        }
    }

    let receiver_kinds = [
        (
            RemoteCaptureKind::Camera,
            view.remote_capture.local_receivers.camera,
            "camera",
        ),
        (
            RemoteCaptureKind::Microphone,
            view.remote_capture.local_receivers.microphone,
            "microphone",
        ),
        (
            RemoteCaptureKind::DocumentScanner,
            view.remote_capture.local_receivers.scanner,
            "scanner",
        ),
    ];
    for peer in &eligible {
        for (kind, available, name) in receiver_kinds {
            if available {
                actions.push(DeviceAction {
                    id: format!(
                        "capture.request.{name}.{}",
                        opaque(peer.id.as_str().as_bytes())
                    ),
                    title: format!("Use {} from {}", name, peer.label),
                    subtitle: "Request a one-session remote input for the focused app".into(),
                    action: DeviceFabricUiAction::RequestRemoteCapture {
                        peer: peer.id.clone(),
                        kind,
                    },
                    confirm: true,
                });
            }
        }
    }
    for consent in &view.remote_capture.consents {
        for (approved, verb) in [(true, "Allow"), (false, "Decline")] {
            actions.push(DeviceAction {
                id: format!(
                    "capture.consent.{}.{}",
                    if approved { "allow" } else { "decline" },
                    opaque(&consent.request_id)
                ),
                title: format!("{verb} remote input for {}", consent.app_label),
                subtitle: "Resolve this exact source-side capture request".into(),
                action: DeviceFabricUiAction::AuthorizeRemoteCapture {
                    request_id: consent.request_id,
                    approved,
                },
                confirm: approved,
            });
        }
    }
    for session in &view.remote_capture.sessions {
        let Some(session_id) = session.session_id else {
            continue;
        };
        let (verb, action, confirm) = if session.receiving
            && session.phase == RemoteCaptureSessionPhase::AppApprovalRequired
        {
            (
                "Allow focused app",
                DeviceFabricUiAction::ApproveRemoteCaptureTarget { session_id },
                true,
            )
        } else {
            (
                "Stop remote input",
                DeviceFabricUiAction::StopRemoteCapture { session_id },
                false,
            )
        };
        actions.push(DeviceAction {
            id: format!(
                "capture.session.{}.{}",
                if confirm { "approve" } else { "stop" },
                opaque(&session_id)
            ),
            title: format!("{verb} from {}", label_for(&session.peer)),
            subtitle: "Act on this exact remote capture session".into(),
            action,
            confirm,
        });
    }

    for gamepad in &view.local_gamepads {
        if let Some(forwarding) = &gamepad.forwarding {
            actions.push(DeviceAction {
                id: format!(
                    "controller.stop.{}",
                    opaque(&forwarding.session.to_le_bytes())
                ),
                title: format!("Stop forwarding {}", gamepad.name),
                subtitle: "Neutralize and close this controller session".into(),
                action: DeviceFabricUiAction::StopPhysicalGamepad {
                    session: forwarding.session,
                },
                confirm: false,
            });
        } else {
            for target in &eligible {
                actions.push(DeviceAction {
                    id: format!(
                        "controller.start.{}.{}",
                        opaque(&gamepad.token.to_le_bytes()),
                        opaque(target.id.as_str().as_bytes())
                    ),
                    title: format!("Use {} on {}", gamepad.name, target.label),
                    subtitle: "Forward normalized controls to the currently focused app".into(),
                    action: DeviceFabricUiAction::StartPhysicalGamepad {
                        source_token: gamepad.token,
                        target: target.id.clone(),
                    },
                    confirm: true,
                });
            }
        }
    }
    for authorization in &view.gamepad_authorizations {
        for (approved, verb) in [(true, "Allow"), (false, "Decline")] {
            actions.push(DeviceAction {
                id: format!(
                    "controller.consent.{}.{}",
                    if approved { "allow" } else { "decline" },
                    opaque(&authorization.forwarding_id)
                ),
                title: format!(
                    "{verb} {} from {}",
                    authorization.controller_label, authorization.source_label
                ),
                subtitle: "Controller access for the locally verified focused app".into(),
                action: DeviceFabricUiAction::AuthorizeGamepadForwarding {
                    forwarding_id: authorization.forwarding_id,
                    approved,
                },
                confirm: approved,
            });
        }
    }
    actions
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

pub struct DeviceContinuityActionsProvider {
    bridge: DeviceFabricBridge,
}

impl DeviceContinuityActionsProvider {
    pub fn new(bridge: DeviceFabricBridge) -> Self {
        Self { bridge }
    }
}

impl SearchProvider for DeviceContinuityActionsProvider {
    fn id(&self) -> &str {
        DEVICE_PROVIDER_ID
    }
    fn name(&self) -> &str {
        "Continuity"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let Ok(view) = (self.bridge)(DeviceFabricUiAction::Snapshot) else {
            return Vec::new();
        };
        device_actions(&view)
            .into_iter()
            .filter_map(|entry| {
                let score = matches(ctx.needle, &entry.title, &entry.subtitle, &entry.id)?;
                let bridge = Rc::clone(&self.bridge);
                let expected = entry.action.clone();
                let confirm = entry.confirm;
                Some(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: DEVICE_PROVIDER_ID.into(),
                    id: entry.id,
                    title: entry.title,
                    subtitle: Some(entry.subtitle),
                    score,
                    params: confirmation_params(confirm),
                    contract: Some(contract(confirm)),
                    on_open: Rc::new(move |_doc, _root, _chrome, args| {
                        ensure_confirmed(args, confirm)?;
                        dispatch_device_action(&bridge, &expected, true, confirm)
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                })
            })
            .collect()
    }
}

fn call_actions(
    view: &crate::shell::collab_broker_capnp::RealtimeContinuityUiView,
    devices: &DeviceFabricView,
) -> Vec<CallAction> {
    let mut actions = Vec::new();
    for source in &view.sources {
        for target in devices
            .devices
            .iter()
            .filter(|device| !device.is_local && device.online)
        {
            actions.push(CallAction {
                id: format!(
                    "call.propose.{}.{}",
                    opaque(source.session.as_bytes()),
                    opaque(target.id.as_str().as_bytes())
                ),
                title: format!("Move {} call to {}", source.room_label, target.label),
                subtitle: "Propose handoff to an exact certified device".into(),
                action: RealtimeContinuityUiAction::Propose {
                    session: source.session.clone(),
                    target: target.id.clone(),
                },
                confirm: true,
            });
        }
    }
    for consent in &view.consents {
        for (approved, verb) in [(true, "Allow"), (false, "Decline")] {
            actions.push(CallAction {
                id: format!(
                    "call.consent.{}.{}",
                    if approved { "allow" } else { "decline" },
                    opaque(consent.session.as_bytes())
                ),
                title: format!("{verb} handoff for {}", consent.room_label),
                subtitle: "Resolve this exact call handoff consent".into(),
                action: RealtimeContinuityUiAction::ResolveConsent {
                    session: consent.session.clone(),
                    approved,
                },
                confirm: approved,
            });
        }
    }
    actions
}

pub struct CallContinuityActionsProvider {
    realtime: RealtimeContinuityBridge,
    devices: DeviceFabricBridge,
}

impl CallContinuityActionsProvider {
    pub fn new(realtime: RealtimeContinuityBridge, devices: DeviceFabricBridge) -> Self {
        Self { realtime, devices }
    }
}

impl SearchProvider for CallContinuityActionsProvider {
    fn id(&self) -> &str {
        CALL_PROVIDER_ID
    }
    fn name(&self) -> &str {
        "Call continuity"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let Ok(view) = (self.realtime)(RealtimeContinuityUiAction::Snapshot) else {
            return Vec::new();
        };
        let Ok(devices) = (self.devices)(DeviceFabricUiAction::Snapshot) else {
            return Vec::new();
        };
        call_actions(&view, &devices)
            .into_iter()
            .filter_map(|entry| {
                let score = matches(ctx.needle, &entry.title, &entry.subtitle, &entry.id)?;
                let realtime = Rc::clone(&self.realtime);
                let device_bridge = Rc::clone(&self.devices);
                let expected = entry.action.clone();
                let confirm = entry.confirm;
                Some(SearchResult {
                    kind: ResourceKind::Action,
                    provider_id: CALL_PROVIDER_ID.into(),
                    id: entry.id,
                    title: entry.title,
                    subtitle: Some(entry.subtitle),
                    score,
                    params: confirmation_params(confirm),
                    contract: Some(contract(confirm)),
                    on_open: Rc::new(move |_doc, _root, _chrome, args| {
                        ensure_confirmed(args, confirm)?;
                        let current = realtime(RealtimeContinuityUiAction::Snapshot)
                            .map_err(ActionError::Refused)?;
                        let devices = device_bridge(DeviceFabricUiAction::Snapshot)
                            .map_err(ActionError::Refused)?;
                        if !call_actions(&current, &devices)
                            .iter()
                            .any(|entry| entry.action == expected)
                        {
                            return Err(ActionError::Refused(
                                "call handoff action is no longer available".into(),
                            ));
                        }
                        realtime(expected.clone()).map_err(ActionError::Refused)?;
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::shell::overlay_host::{FabricDeviceView, FabricMediaView};
    use crate::shell::personal_continuity::DeviceId;

    fn device(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn fixture() -> DeviceFabricView {
        let peer = device("certified-peer-secret-looking-id");
        DeviceFabricView {
            devices: vec![FabricDeviceView {
                id: peer.clone(),
                label: "Travel phone".into(),
                kind: DeviceKind::Phone,
                is_local: false,
                online: true,
                audio_output: true,
                video_output: true,
                display: true,
                controls_local: false,
                local_can_control: false,
            }],
            local_media: vec![FabricMediaView {
                session_id: "private-session-token".into(),
                title: "Now playing".into(),
                playing: true,
                has_video: false,
                targets: vec![peer],
            }],
            remote_capture: crate::shell::remote_capture_runtime::RemoteCaptureView {
                local_receivers: crate::shell::remote_capture_adapter::RemoteCaptureAvailability {
                    camera: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            tethering: vec![
                crate::shell::personal_tethering_runtime::TetheringStatusView {
                    request_id: [0x43; 16],
                    peer: device("tether-requester"),
                    peer_label: "Laptop".into(),
                    direction:
                        crate::shell::personal_tethering_runtime::TetheringDirection::Provider,
                    phase: TetheringPhase::ConsentRequired,
                    expires_unix_ms: 10_000,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn action_locators_do_not_disclose_identity_session_or_device_paths() {
        let actions = device_actions(&fixture());
        assert!(!actions.is_empty());
        for action in actions {
            assert!(!action.id.contains("certified-peer-secret-looking-id"));
            assert!(!action.id.contains("private-session-token"));
            assert!(!action.id.contains("2f6465762f"));
            assert!(!action.id.contains("43434343"));
        }
    }

    #[test]
    fn dispatch_requires_confirmation_and_fresh_full_action_match() {
        let view = Rc::new(RefCell::new(fixture()));
        let dispatched = Rc::new(RefCell::new(Vec::new()));
        let bridge_view = Rc::clone(&view);
        let bridge_dispatched = Rc::clone(&dispatched);
        let bridge: DeviceFabricBridge = Rc::new(move |action| {
            if action != DeviceFabricUiAction::Snapshot {
                bridge_dispatched.borrow_mut().push(action);
            }
            Ok(bridge_view.borrow().clone())
        });
        let expected = device_actions(&view.borrow())
            .into_iter()
            .find(|entry| matches!(entry.action, DeviceFabricUiAction::HandoffMedia { .. }))
            .unwrap()
            .action;

        assert!(dispatch_device_action(&bridge, &expected, false, true).is_err());
        assert!(dispatched.borrow().is_empty());

        view.borrow_mut().local_media.clear();
        assert!(dispatch_device_action(&bridge, &expected, true, true).is_err());
        assert!(dispatched.borrow().is_empty());

        *view.borrow_mut() = fixture();
        dispatch_device_action(&bridge, &expected, true, true).unwrap();
        assert_eq!(&*dispatched.borrow(), &[expected]);
    }
}
