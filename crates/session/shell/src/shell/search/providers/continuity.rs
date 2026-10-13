//! Semantic action projection for compositor-owned continuity services.
//!
//! This provider does not own transport or authority. It projects only the
//! exact objects returned by the trusted chrome bridges and re-reads that
//! projection immediately before dispatch, so a stale catalogue cannot turn
//! an offline peer or expired consent into ambient authority.

use std::rc::Rc;

use ui::i18n::ArgValue;
use ui::search::{FieldWeight, ListIndex};

use super::actions::ACTION_ID_WEIGHT;

use crate::shell::device_fabric::{DeviceKind, DisplayEdge};
use crate::shell::device_fabric_runtime::{BluetoothHandoffPhaseView, MediaHandoffPhase};
use crate::shell::i18n::{t, t_args};
use crate::shell::overlay_host::{
    DeviceFabricBridge, DeviceFabricUiAction, DeviceFabricView, RealtimeContinuityBridge,
    RealtimeContinuityUiAction,
};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::personal_tethering_runtime::TetheringPhase;
use crate::shell::remote_capture::device::RemoteCaptureKind;
use crate::shell::remote_capture::runtime::RemoteCaptureSessionPhase;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use crate::shell::speaker_group_runtime::SpeakerConsentKind;

pub const DEVICE_PROVIDER_ID: &str = "continuity.devices";
pub const CALL_PROVIDER_ID: &str = "continuity.calls";

/// One continuity action row, re-derived from the trusted view before it runs.
#[derive(Clone)]
struct ContinuityAction<A> {
    id: String,
    title: String,
    subtitle: String,
    action: A,
    confirm: bool,
}

type DeviceAction = ContinuityAction<DeviceFabricUiAction>;
type CallAction = ContinuityAction<RealtimeContinuityUiAction>;

impl<A: std::fmt::Debug> ContinuityAction<A> {
    fn revision_row(&self) -> (&str, &str, &str, bool, String) {
        (
            &self.id,
            &self.title,
            &self.subtitle,
            self.confirm,
            format!("{:?}", self.action),
        )
    }

    fn fields(&self) -> Vec<(String, FieldWeight)> {
        action_fields(&self.title, &self.subtitle, &self.id)
    }
}

impl<A: 'static> ContinuityAction<A> {
    /// The search row for this action. Opening it demands confirmation when
    /// the action asks for it, then hands the expected action to `dispatch`.
    fn into_result(
        self,
        provider_id: &'static str,
        dispatch: impl Fn(&A) -> Result<(), ActionError> + 'static,
    ) -> SearchResult {
        let Self {
            id,
            title,
            subtitle,
            action,
            confirm,
        } = self;
        SearchResult {
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::Effect,
            provider_id: provider_id.into(),
            id,
            title,
            subtitle: Some(subtitle),
            badge: None,
            score: 0,
            params: confirmation_params(confirm),
            contract: Some(contract(confirm)),
            on_open: Rc::new(move |_doc, _root, _chrome, args| {
                ensure_confirmed(args, confirm)?;
                dispatch(&action)
            }),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }
}

fn opaque(domain: &'static str, bytes: &[u8]) -> String {
    let digest = blake3::derive_key(domain, bytes);
    hex::encode(digest.get(..16).unwrap_or(&digest))
}

/// The semantic id of "keep playing this local media session here and send
/// its sound to speaker group `group`". Like [`media_handoff_action_id`], it
/// names the destination without disclosing the session or group identity.
pub(crate) fn speaker_group_media_action_id(
    session_id: &str,
    group: crate::shell::speaker_group::SpeakerGroupId,
) -> String {
    format!(
        "speakers.media.{}.{}",
        opaque("so.tilo.continuity.media-session.v1", session_id.as_bytes()),
        opaque("so.tilo.continuity.speaker-group.v1", &group.0)
    )
}

/// The semantic id of "play this local media session on `target`". Other
/// chrome names a handoff destination by this id, never by the device or
/// session identity behind it.
pub(crate) fn media_handoff_action_id(session_id: &str, target: &DeviceId) -> String {
    format!(
        "media.handoff.{}.{}",
        opaque("so.tilo.continuity.media-session.v1", session_id.as_bytes()),
        device_token(target)
    )
}

/// The semantic id of "route local playback to this output". The `PipeWire`
/// node id stays behind the digest.
pub(crate) fn audio_route_action_id(node_id: u32) -> String {
    format!(
        "audio.route.{}",
        opaque("so.tilo.continuity.audio-node.v1", &node_id.to_le_bytes())
    )
}

/// An action row's searchable title, identity and description.
fn action_fields(title: &str, subtitle: &str, id: &str) -> Vec<(String, FieldWeight)> {
    vec![
        (title.to_owned(), FieldWeight::TITLE),
        (id.to_owned(), ACTION_ID_WEIGHT),
        (subtitle.to_owned(), FieldWeight::SUBTITLE),
    ]
}

fn confirmation_params(confirm: bool) -> Vec<ActionParam> {
    if confirm {
        vec![ActionParam::required(
            "confirm",
            ParamType::Boolean,
            "Explicitly confirm this consequential continuity action",
        )]
    } else {
        Vec::new()
    }
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
    let current = bridge(DeviceFabricUiAction::SnapshotFresh).map_err(ActionError::Refused)?;
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

/// A device's identity as it appears in action ids: a digest, never the id.
fn device_token(id: &DeviceId) -> String {
    opaque("so.tilo.continuity.device.v1", id.as_str().as_bytes())
}

/// The allow and decline rows answering one consent request, with ids
/// `{topic}.consent.{allow|decline}.{token}`. Only allowing asks for
/// confirmation.
fn consent_pair<A>(
    topic: &str,
    token: &str,
    [allow_key, decline_key]: [&str; 2],
    title_args: &[(&str, ArgValue)],
    subtitle_key: &str,
    action: impl Fn(bool) -> A,
) -> [ContinuityAction<A>; 2] {
    [(true, "allow", allow_key), (false, "decline", decline_key)].map(
        |(approved, verb, title_key)| ContinuityAction {
            id: format!("{topic}.consent.{verb}.{token}"),
            title: t_args(title_key, title_args),
            subtitle: t(subtitle_key),
            action: action(approved),
            confirm: approved,
        },
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive projection keeps every continuity state and its authorized action visibly paired"
)]
fn device_actions(view: &DeviceFabricView) -> Vec<DeviceAction> {
    let mut actions = Vec::new();
    let label_for = |id: &DeviceId| {
        view.devices
            .iter()
            .find(|device| &device.id == id)
            .map_or_else(
                || t("search-continuity-certified-device"),
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
            actions.push(DeviceAction {
                id: media_handoff_action_id(&media.session_id, target),
                title: t_args(
                    "search-continuity-play-on",
                    &[
                        ("source", media.title.as_str().into()),
                        ("target", label_for(target).into()),
                    ],
                ),
                subtitle: t("search-continuity-media-handoff-subtitle"),
                action: DeviceFabricUiAction::HandoffMedia {
                    session_id: media.session_id.clone(),
                    target: target.clone(),
                },
                confirm: true,
            });
        }
    }
    for status in &view.media_handoffs {
        let (verb, title_key, action) = match status.phase {
            MediaHandoffPhase::Pending => (
                "cancel",
                "search-continuity-media-cancel",
                DeviceFabricUiAction::CancelMediaHandoff {
                    request_id: status.request_id,
                },
            ),
            MediaHandoffPhase::Failed(_) | MediaHandoffPhase::Cancelled => (
                "retry",
                "search-continuity-media-retry",
                DeviceFabricUiAction::RetryMediaHandoff {
                    request_id: status.request_id,
                },
            ),
            MediaHandoffPhase::Completed => continue,
        };
        actions.push(DeviceAction {
            id: format!(
                "media.{verb}.{}",
                opaque("so.tilo.continuity.media-request.v1", &status.request_id)
            ),
            title: t_args(title_key, &[("media", status.title.as_str().into())]),
            subtitle: t_args(
                "search-continuity-media-status-subtitle",
                &[("device", status.target_label.as_str().into())],
            ),
            action,
            confirm: false,
        });
    }
    for authorization in &view.bluetooth_authorizations {
        actions.extend(consent_pair(
            "bluetooth",
            &opaque(
                "so.tilo.continuity.bluetooth-request.v1",
                &authorization.request_id,
            ),
            [
                "search-continuity-bluetooth-allow",
                "search-continuity-bluetooth-decline",
            ],
            &[
                ("accessory", authorization.accessory_name.as_str().into()),
                ("device", authorization.source_label.as_str().into()),
            ],
            "search-continuity-bluetooth-consent-subtitle",
            |approved| DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                request_id: authorization.request_id,
                approved,
            },
        ));
    }
    for status in &view.bluetooth_handoffs {
        let (verb, title_key, action) = match status.phase {
            BluetoothHandoffPhaseView::Completed => continue,
            BluetoothHandoffPhaseView::Failed(_) | BluetoothHandoffPhaseView::Cancelled
                if status.outbound =>
            {
                (
                    "retry",
                    "search-continuity-bluetooth-retry",
                    DeviceFabricUiAction::RetryBluetoothHandoff {
                        request_id: status.request_id,
                    },
                )
            }
            BluetoothHandoffPhaseView::Failed(_) | BluetoothHandoffPhaseView::Cancelled => {
                continue;
            }
            _ => (
                "cancel",
                "search-continuity-bluetooth-cancel",
                DeviceFabricUiAction::CancelBluetoothHandoff {
                    request_id: status.request_id,
                },
            ),
        };
        actions.push(DeviceAction {
            id: format!(
                "bluetooth.{verb}.{}",
                opaque(
                    "so.tilo.continuity.bluetooth-request.v1",
                    &status.request_id
                )
            ),
            title: t_args(
                title_key,
                &[("accessory", status.accessory_name.as_str().into())],
            ),
            subtitle: t_args(
                "search-continuity-bluetooth-status-subtitle",
                &[("device", status.peer_label.as_str().into())],
            ),
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
            id: audio_route_action_id(output.node_id),
            title: t_args(
                "search-continuity-route-audio",
                &[("output", output.label.as_str().into())],
            ),
            subtitle: t("search-continuity-route-audio-subtitle"),
            action: DeviceFabricUiAction::RouteLocalAudio {
                node_id: output.node_id,
            },
            confirm: false,
        });
    }
    for input in &view.local_inputs {
        let token = opaque(
            "so.tilo.continuity.audio-input.v1",
            &input.node_id.to_le_bytes(),
        );
        let title_args = [("input", input.label.as_str().into())];
        if input.is_default {
            for (raise, verb, title_key) in [
                (true, "raise", "search-continuity-input-raise"),
                (false, "lower", "search-continuity-input-lower"),
            ] {
                actions.push(DeviceAction {
                    id: format!("audio.input-level.{verb}.{token}"),
                    title: t_args(title_key, &title_args),
                    subtitle: t("search-continuity-input-level-subtitle"),
                    action: DeviceFabricUiAction::StepInputLevel {
                        node_id: input.node_id,
                        raise,
                    },
                    confirm: false,
                });
            }
        } else {
            actions.push(DeviceAction {
                id: format!("audio.input.{token}"),
                title: t_args("search-continuity-input-select", &title_args),
                subtitle: t("search-continuity-input-select-subtitle"),
                action: DeviceFabricUiAction::SelectLocalInput {
                    node_id: input.node_id,
                },
                confirm: false,
            });
        }
    }
    let source_token =
        |token: u64| opaque("so.tilo.continuity.audio-source.v1", &token.to_le_bytes());
    for source in &view.remote_audio.sources {
        for target in eligible
            .iter()
            .copied()
            .filter(|device| device.audio_output)
        {
            actions.push(DeviceAction {
                id: format!(
                    "audio.forward.{}.{}",
                    source_token(source.token),
                    device_token(&target.id)
                ),
                title: t_args(
                    "search-continuity-play-on",
                    &[
                        ("source", source.label.as_str().into()),
                        ("target", target.label.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-audio-forward-subtitle"),
                action: DeviceFabricUiAction::StartRemoteAudio {
                    source_token: source.token,
                    target: target.id.clone(),
                },
                confirm: true,
            });
        }
    }
    for consent in &view.remote_audio.consents {
        actions.extend(consent_pair(
            "audio",
            &opaque("so.tilo.continuity.audio-session.v1", &consent.session_id),
            [
                "search-continuity-audio-allow",
                "search-continuity-audio-decline",
            ],
            &[("device", consent.label.as_str().into())],
            "search-continuity-audio-consent-subtitle",
            |approved| DeviceFabricUiAction::AuthorizeRemoteAudio {
                session_id: consent.session_id,
                approved,
            },
        ));
    }
    for route in &view.remote_audio.routes {
        actions.push(DeviceAction {
            id: format!(
                "audio.stop.{}",
                opaque("so.tilo.continuity.audio-session.v1", &route.session_id)
            ),
            title: t_args(
                "search-continuity-audio-stop",
                &[("source", route.label.as_str().into())],
            ),
            subtitle: t("search-continuity-audio-stop-subtitle"),
            action: DeviceFabricUiAction::StopRemoteAudio {
                session_id: route.session_id,
            },
            confirm: false,
        });
    }

    let speaker_id = |group: crate::shell::speaker_group::SpeakerGroupId| {
        opaque("so.tilo.continuity.speaker-group.v1", &group.0)
    };
    for consent in &view.speaker_groups.consents {
        let kind = consent.scope.kind();
        let topic = match kind {
            SpeakerConsentKind::Play => "consent",
            SpeakerConsentKind::Listen => "listen",
        };
        for (approved, verb) in [(true, "allow"), (false, "decline")] {
            actions.push(DeviceAction {
                id: format!("speakers.{topic}.{verb}.{}", speaker_id(consent.group)),
                title: consent.action_title(approved),
                subtitle: consent.action_subtitle(),
                action: DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                    group: consent.group,
                    kind,
                    approved,
                },
                confirm: approved,
            });
        }
    }
    for membership in &view.speaker_groups.memberships {
        let kind = membership.scope.kind();
        let verb = match kind {
            SpeakerConsentKind::Play => "leave",
            SpeakerConsentKind::Listen => "listen.stop",
        };
        actions.push(DeviceAction {
            id: format!("speakers.{verb}.{}", speaker_id(membership.group)),
            title: membership.leave_title(),
            subtitle: membership.leave_subtitle(),
            action: DeviceFabricUiAction::LeaveSpeakerGroup {
                group: membership.group,
                kind,
            },
            confirm: false,
        });
    }
    for group in view
        .speaker_groups
        .groups
        .iter()
        .filter(|group| group.ready_members() > 0)
    {
        for source in &view.remote_audio.sources {
            actions.push(DeviceAction {
                id: format!(
                    "speakers.play.{}.{}",
                    source_token(source.token),
                    speaker_id(group.id)
                ),
                title: t_args(
                    "search-continuity-play-on",
                    &[
                        ("source", source.label.as_str().into()),
                        ("target", group.name.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-speaker-play-subtitle"),
                action: DeviceFabricUiAction::StartSpeakerGroupAudio {
                    source_token: source.token,
                    group: group.id,
                },
                confirm: true,
            });
        }
        for media in &view.local_media {
            actions.push(DeviceAction {
                id: speaker_group_media_action_id(&media.session_id, group.id),
                title: t_args(
                    "search-continuity-play-on",
                    &[
                        ("source", media.title.as_str().into()),
                        ("target", group.name.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-speaker-media-subtitle"),
                action: DeviceFabricUiAction::PlayMediaOnSpeakerGroup {
                    session_id: media.session_id.clone(),
                    group: group.id,
                },
                confirm: true,
            });
        }
    }
    for session in &view.speaker_groups.sessions {
        actions.push(DeviceAction {
            id: format!(
                "speakers.stop.{}",
                opaque("so.tilo.continuity.speaker-session.v1", &session.session_id)
            ),
            title: t_args(
                "search-continuity-speaker-stop",
                &[("group", session.name.as_str().into())],
            ),
            subtitle: t("search-continuity-speaker-stop-subtitle"),
            action: DeviceFabricUiAction::StopSpeakerGroupSession {
                session_id: session.session_id,
            },
            confirm: false,
        });
    }

    for device in eligible.iter().copied().filter(|device| device.display) {
        let token = device_token(&device.id);
        let title_args = [("device", device.label.as_str().into())];
        for (edge, edge_name, title_key) in [
            (DisplayEdge::Left, "left", "search-continuity-place-left"),
            (DisplayEdge::Right, "right", "search-continuity-place-right"),
            (DisplayEdge::Top, "above", "search-continuity-place-above"),
            (
                DisplayEdge::Bottom,
                "below",
                "search-continuity-place-below",
            ),
        ] {
            actions.push(DeviceAction {
                id: format!("spatial.place.{edge_name}.{token}"),
                title: t_args(title_key, &title_args),
                subtitle: t("search-continuity-place-subtitle"),
                action: DeviceFabricUiAction::Place {
                    device: device.id.clone(),
                    edge,
                },
                confirm: true,
            });
        }
        let controller = device.id.clone();
        let (verb, title_key, action) = if device.controls_local {
            (
                "revoke",
                "search-continuity-control-revoke",
                DeviceFabricUiAction::RevokeControl { controller },
            )
        } else {
            (
                "grant",
                "search-continuity-control-grant",
                DeviceFabricUiAction::GrantControl { controller },
            )
        };
        actions.push(DeviceAction {
            id: format!("spatial.control.{verb}.{token}"),
            title: t_args(title_key, &title_args),
            subtitle: t("search-continuity-control-subtitle"),
            action,
            confirm: !device.controls_local,
        });
    }
    for proposal in &view.calibration_proposals {
        let token = device_token(&proposal.peer);
        let title_args = [("device", label_for(&proposal.peer).into())];
        let device = || proposal.peer.clone();
        for (verb, title_key, action, confirm) in [
            (
                "apply",
                "search-continuity-calibration-apply",
                DeviceFabricUiAction::ApplyCalibration { device: device() },
                true,
            ),
            (
                "dismiss",
                "search-continuity-calibration-dismiss",
                DeviceFabricUiAction::DismissCalibration { device: device() },
                false,
            ),
        ] {
            actions.push(DeviceAction {
                id: format!("spatial.calibration.{verb}.{token}"),
                title: t_args(title_key, &title_args),
                subtitle: t("search-continuity-calibration-subtitle"),
                action,
                confirm,
            });
        }
    }
    for peer in &view.revertible_calibrations {
        actions.push(DeviceAction {
            id: format!("spatial.calibration.revert.{}", device_token(peer)),
            title: t_args(
                "search-continuity-calibration-revert",
                &[("device", label_for(peer).into())],
            ),
            subtitle: t("search-continuity-calibration-revert-subtitle"),
            action: DeviceFabricUiAction::RevertCalibration {
                device: peer.clone(),
            },
            confirm: false,
        });
    }

    for phone in eligible.iter().copied().filter(|device| {
        device.kind == DeviceKind::Phone
            && !view.tethering.iter().any(|status| status.peer == device.id)
    }) {
        actions.push(DeviceAction {
            id: format!("tether.request.{}", device_token(&phone.id)),
            title: t_args(
                "search-continuity-tether-request",
                &[("device", phone.label.as_str().into())],
            ),
            subtitle: t("search-continuity-tether-request-subtitle"),
            action: DeviceFabricUiAction::RequestPhoneTethering {
                phone: phone.id.clone(),
            },
            confirm: true,
        });
    }
    for status in &view.tethering {
        let token = opaque("so.tilo.continuity.tether-request.v1", &status.request_id);
        let title_args = [("device", status.peer_label.as_str().into())];
        if status.phase == TetheringPhase::ConsentRequired {
            actions.extend(consent_pair(
                "tether",
                &token,
                [
                    "search-continuity-tether-allow",
                    "search-continuity-tether-decline",
                ],
                &title_args,
                "search-continuity-tether-consent-subtitle",
                |approved| DeviceFabricUiAction::AuthorizePhoneTethering {
                    request_id: status.request_id,
                    approved,
                },
            ));
        }
        if status.can_disconnect() {
            actions.push(DeviceAction {
                id: format!("tether.disconnect.{token}"),
                title: t_args("search-continuity-tether-disconnect", &title_args),
                subtitle: t("search-continuity-tether-disconnect-subtitle"),
                action: DeviceFabricUiAction::DisconnectPhoneTethering {
                    request_id: status.request_id,
                },
                confirm: false,
            });
        }
    }

    for unlock in &view.device_unlocks {
        let device = if unlock.device_label.is_empty() {
            t("search-continuity-certified-device")
        } else {
            unlock.device_label.clone()
        };
        for (approved, verb, title_key) in [
            (true, "approve", "search-continuity-unlock-approve"),
            (false, "deny", "search-continuity-unlock-deny"),
        ] {
            actions.push(DeviceAction {
                // The code is the one the waiting boot shows.
                id: format!(
                    "{}.{verb}.{}",
                    crate::shell::device_unlock::DEVICE_UNLOCK_ACTION,
                    unlock.approval_code
                ),
                title: t_args(
                    title_key,
                    &[
                        ("device", device.as_str().into()),
                        ("code", unlock.approval_code.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-unlock-subtitle"),
                action: DeviceFabricUiAction::AuthorizeDeviceUnlock {
                    request_digest: unlock.request_digest,
                    approved,
                },
                confirm: approved,
            });
        }
    }
    for device in view.devices.iter().filter(|device| !device.is_local) {
        actions.push(DeviceAction {
            id: format!("device.remove.{}", device_token(&device.id)),
            title: t_args(
                "search-continuity-device-remove",
                &[("device", device.label.as_str().into())],
            ),
            subtitle: t("search-continuity-device-remove-subtitle"),
            action: DeviceFabricUiAction::RemoveDevice {
                device: device.id.clone(),
            },
            confirm: true,
        });
    }

    let receivers = &view.remote_capture.local_receivers;
    let receiver_kinds = [
        (
            RemoteCaptureKind::Camera,
            receivers.camera,
            "camera",
            "search-continuity-capture-camera",
        ),
        (
            RemoteCaptureKind::Microphone,
            receivers.microphone,
            "microphone",
            "search-continuity-capture-microphone",
        ),
        (
            RemoteCaptureKind::DocumentScanner,
            receivers.scanner,
            "scanner",
            "search-continuity-capture-scanner",
        ),
    ];
    for peer in &eligible {
        for (kind, _, name, title_key) in receiver_kinds
            .iter()
            .copied()
            .filter(|(_, available, ..)| *available)
        {
            actions.push(DeviceAction {
                id: format!("capture.request.{name}.{}", device_token(&peer.id)),
                title: t_args(title_key, &[("device", peer.label.as_str().into())]),
                subtitle: t("search-continuity-capture-request-subtitle"),
                action: DeviceFabricUiAction::RequestRemoteCapture {
                    peer: peer.id.clone(),
                    kind,
                },
                confirm: true,
            });
        }
    }
    for consent in &view.remote_capture.consents {
        actions.extend(consent_pair(
            "capture",
            &opaque("so.tilo.continuity.capture-request.v1", &consent.request_id),
            [
                "search-continuity-capture-allow",
                "search-continuity-capture-decline",
            ],
            &[("app", consent.app_label.as_str().into())],
            "search-continuity-capture-consent-subtitle",
            |approved| DeviceFabricUiAction::AuthorizeRemoteCapture {
                request_id: consent.request_id,
                approved,
            },
        ));
    }
    for session in &view.remote_capture.sessions {
        let Some(session_id) = session.session_id else {
            continue;
        };
        let (verb, title_key, action, confirm) = if session.receiving
            && session.phase == RemoteCaptureSessionPhase::AppApprovalRequired
        {
            (
                "approve",
                "search-continuity-capture-approve-app",
                DeviceFabricUiAction::ApproveRemoteCaptureTarget { session_id },
                true,
            )
        } else {
            (
                "stop",
                "search-continuity-capture-stop",
                DeviceFabricUiAction::StopRemoteCapture { session_id },
                false,
            )
        };
        actions.push(DeviceAction {
            id: format!(
                "capture.session.{verb}.{}",
                opaque("so.tilo.continuity.capture-session.v1", &session_id)
            ),
            title: t_args(title_key, &[("device", label_for(&session.peer).into())]),
            subtitle: t("search-continuity-capture-session-subtitle"),
            action,
            confirm,
        });
    }

    for gamepad in &view.local_gamepads {
        if let Some(forwarding) = &gamepad.forwarding {
            actions.push(DeviceAction {
                id: format!(
                    "controller.stop.{}",
                    opaque(
                        "so.tilo.continuity.controller-session.v1",
                        &forwarding.session.to_le_bytes()
                    )
                ),
                title: t_args(
                    "search-continuity-controller-stop",
                    &[("controller", gamepad.name.as_str().into())],
                ),
                subtitle: t("search-continuity-controller-stop-subtitle"),
                action: DeviceFabricUiAction::StopPhysicalGamepad {
                    session: forwarding.session,
                },
                confirm: false,
            });
            continue;
        }
        let source = opaque(
            "so.tilo.continuity.controller-source.v1",
            &gamepad.token.to_le_bytes(),
        );
        for target in &eligible {
            actions.push(DeviceAction {
                id: format!("controller.start.{source}.{}", device_token(&target.id)),
                title: t_args(
                    "search-continuity-controller-start",
                    &[
                        ("controller", gamepad.name.as_str().into()),
                        ("device", target.label.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-controller-start-subtitle"),
                action: DeviceFabricUiAction::StartPhysicalGamepad {
                    source_token: gamepad.token,
                    target: target.id.clone(),
                },
                confirm: true,
            });
        }
    }
    for authorization in &view.gamepad_authorizations {
        actions.extend(consent_pair(
            "controller",
            &opaque(
                "so.tilo.continuity.controller-forwarding.v1",
                &authorization.forwarding_id,
            ),
            [
                "search-continuity-controller-allow",
                "search-continuity-controller-decline",
            ],
            &[
                ("controller", authorization.controller_label.as_str().into()),
                ("device", authorization.source_label.as_str().into()),
            ],
            "search-continuity-controller-consent-subtitle",
            |approved| DeviceFabricUiAction::AuthorizeGamepadForwarding {
                forwarding_id: authorization.forwarding_id,
                approved,
            },
        ));
    }
    actions
}

fn projection_revision<'a>(
    domain: &'static str,
    needle: &str,
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str, bool, String)>,
) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(domain);
    hasher.update(needle.as_bytes());
    for (id, title, subtitle, confirm, action) in rows {
        for value in [id, title, subtitle, action.as_str()] {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        hasher.update(&[u8::from(confirm)]);
    }
    hasher.finalize().to_hex().to_string()
}

fn result_bytes(result: &SearchResult) -> usize {
    result
        .id
        .len()
        .saturating_add(result.title.len())
        .saturating_add(result.subtitle.as_ref().map_or(0, String::len))
        .saturating_add(result.provider_id.len())
}

fn page_results(
    revision: String,
    results: Vec<SearchResult>,
    cursor: Option<&SearchCursor>,
    budget: SearchBudget,
) -> SearchPage {
    if cursor.is_some_and(|cursor| cursor.revision != revision) {
        return SearchPage::stale(revision);
    }
    let start = match cursor {
        None => 0,
        Some(cursor) => match cursor.token.parse::<usize>() {
            Ok(offset) if offset <= results.len() => offset,
            _ => return SearchPage::stale(revision),
        },
    };
    let mut end = start;
    let mut bytes = 0usize;
    let result_limit = if budget.max_results == 0 {
        usize::MAX
    } else {
        budget.max_results
    };
    let work_limit = if budget.max_work == 0 {
        usize::MAX
    } else {
        budget.max_work
    };
    let byte_limit = if budget.max_bytes == 0 {
        usize::MAX
    } else {
        budget.max_bytes
    };
    while end < results.len()
        && end.saturating_sub(start) < result_limit
        && end.saturating_sub(start) < work_limit
    {
        #[expect(
            clippy::indexing_slicing,
            reason = "the loop condition keeps end below results.len()"
        )]
        let next_bytes = result_bytes(&results[end]);
        if bytes.saturating_add(next_bytes) > byte_limit {
            break;
        }
        bytes = bytes.saturating_add(next_bytes);
        // end < results.len(), so this cannot saturate.
        end = end.saturating_add(1);
    }
    let exhausted = end == results.len();
    // end starts at start and only grows, so this cannot saturate.
    let results = results
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    SearchPage::continuing(revision, results, (!exhausted).then(|| end.to_string()))
}

fn device_projection(
    bridge: &DeviceFabricBridge,
    ranker: &ListIndex,
    ctx: SearchContext<'_>,
    view: &DeviceFabricView,
) -> (String, Vec<SearchResult>) {
    let actions = device_actions(view);
    let wake = wake_row(view);
    let revision = projection_revision(
        "so.tilo.continuity.device-action-revision.v1",
        ctx.needle,
        actions
            .iter()
            .map(ContinuityAction::revision_row)
            .chain(wake.as_ref().map(|(title, subtitle)| {
                (
                    WAKE_ACTION_ID,
                    title.as_str(),
                    subtitle.as_str(),
                    false,
                    String::new(),
                )
            })),
    );
    let mut fields: Vec<_> = actions.iter().map(ContinuityAction::fields).collect();
    let wake_result = wake.map(|(title, subtitle)| {
        fields.push(action_fields(&title, &subtitle, WAKE_ACTION_ID));
        wake_search_result(bridge, title, subtitle)
    });
    let results = actions
        .into_iter()
        .map(|entry| {
            let bridge = Rc::clone(bridge);
            // `into_result` has already demanded any required confirmation.
            entry.into_result(DEVICE_PROVIDER_ID, move |expected| {
                dispatch_device_action(&bridge, expected, true, true)
            })
        })
        .chain(wake_result)
        .collect();
    (
        revision,
        crate::shell::search::rank_rows(ranker, ctx.needle, results, &fields),
    )
}

/// Wake one of the person's sleeping devices, named by device id or label.
pub const WAKE_ACTION_ID: &str = "device.wake";

/// The wake row's title and subtitle, offered while the person has another
/// device to wake.
fn wake_row(view: &DeviceFabricView) -> Option<(String, String)> {
    view.devices.iter().any(|device| !device.is_local).then(|| {
        (
            t("search-continuity-wake-device"),
            t("search-continuity-wake-device-subtitle"),
        )
    })
}

fn wake_search_result(
    bridge: &DeviceFabricBridge,
    title: String,
    subtitle: String,
) -> SearchResult {
    let bridge = Rc::clone(bridge);
    SearchResult {
        shortcut: None,
        kind: ResourceKind::Action,
        operation_kind: tilo_actions::OperationKind::Effect,
        provider_id: DEVICE_PROVIDER_ID.into(),
        id: WAKE_ACTION_ID.into(),
        title,
        subtitle: Some(subtitle),
        badge: None,
        score: 0,
        params: vec![ActionParam::required(
            "device",
            ParamType::Text,
            "The device to wake: its name or its device id",
        )],
        contract: Some(contract(false)),
        on_open: Rc::new(move |_doc, _root, _chrome, args| {
            let device = args
                .text("device")
                .filter(|device| !device.trim().is_empty())
                .ok_or_else(|| ActionError::Refused("name the device to wake".into()))?;
            bridge(DeviceFabricUiAction::WakeDevice {
                device: device.to_owned(),
            })
            .map_err(ActionError::Refused)?;
            Ok(())
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
}

pub struct DeviceContinuityActionsProvider {
    bridge: DeviceFabricBridge,
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ListIndex,
}

impl DeviceContinuityActionsProvider {
    pub fn new(bridge: DeviceFabricBridge) -> Self {
        Self {
            bridge,
            ranker: ListIndex::default(),
        }
    }
}

impl SearchProvider for DeviceContinuityActionsProvider {
    fn id(&self) -> &str {
        DEVICE_PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Continuity"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        (self.bridge)(DeviceFabricUiAction::Snapshot).map_or_else(
            |_| "continuity-device-unavailable".into(),
            |view| device_projection(&self.bridge, &self.ranker, *ctx, &view).0,
        )
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let Ok(view) = (self.bridge)(DeviceFabricUiAction::Snapshot) else {
            return Vec::new();
        };
        device_projection(&self.bridge, &self.ranker, ctx, &view).1
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return SearchPage::exhausted("continuity-device-not-wanted", Vec::new());
        }
        let Ok(view) = (self.bridge)(DeviceFabricUiAction::Snapshot) else {
            return SearchPage::exhausted("continuity-device-unavailable", Vec::new());
        };
        let (revision, results) = device_projection(&self.bridge, &self.ranker, ctx, &view);
        page_results(revision, results, cursor, budget)
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
                    opaque(
                        "so.tilo.continuity.call-session.v1",
                        source.session.as_bytes()
                    ),
                    device_token(&target.id)
                ),
                title: t_args(
                    "search-continuity-call-move",
                    &[
                        ("room", source.room_label.as_str().into()),
                        ("device", target.label.as_str().into()),
                    ],
                ),
                subtitle: t("search-continuity-call-move-subtitle"),
                action: RealtimeContinuityUiAction::Propose {
                    session: source.session.clone(),
                    target: target.id.clone(),
                },
                confirm: true,
            });
        }
    }
    for consent in &view.consents {
        actions.extend(consent_pair(
            "call",
            &opaque(
                "so.tilo.continuity.call-session.v1",
                consent.session.as_bytes(),
            ),
            [
                "search-continuity-call-allow",
                "search-continuity-call-decline",
            ],
            &[("room", consent.room_label.as_str().into())],
            "search-continuity-call-consent-subtitle",
            |approved| RealtimeContinuityUiAction::ResolveConsent {
                session: consent.session.clone(),
                approved,
            },
        ));
    }
    actions
}

fn call_projection(
    realtime: &RealtimeContinuityBridge,
    device_bridge: &DeviceFabricBridge,
    ranker: &ListIndex,
    ctx: SearchContext<'_>,
    view: &crate::shell::collab_broker_capnp::RealtimeContinuityUiView,
    devices: &DeviceFabricView,
) -> (String, Vec<SearchResult>) {
    let actions = call_actions(view, devices);
    let revision = projection_revision(
        "so.tilo.continuity.call-action-revision.v1",
        ctx.needle,
        actions.iter().map(ContinuityAction::revision_row),
    );
    let fields: Vec<_> = actions.iter().map(ContinuityAction::fields).collect();
    let results = actions
        .into_iter()
        .map(|entry| {
            let realtime = Rc::clone(realtime);
            let device_bridge = Rc::clone(device_bridge);
            entry.into_result(CALL_PROVIDER_ID, move |expected| {
                let current =
                    realtime(RealtimeContinuityUiAction::Snapshot).map_err(ActionError::Refused)?;
                let devices = device_bridge(DeviceFabricUiAction::SnapshotFresh)
                    .map_err(ActionError::Refused)?;
                if !call_actions(&current, &devices)
                    .iter()
                    .any(|entry| entry.action == *expected)
                {
                    return Err(ActionError::Refused(
                        "call handoff action is no longer available".into(),
                    ));
                }
                realtime(expected.clone()).map_err(ActionError::Refused)?;
                Ok(())
            })
        })
        .collect();
    (
        revision,
        crate::shell::search::rank_rows(ranker, ctx.needle, results, &fields),
    )
}

pub struct CallContinuityActionsProvider {
    realtime: RealtimeContinuityBridge,
    devices: DeviceFabricBridge,
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ListIndex,
}

impl CallContinuityActionsProvider {
    pub fn new(realtime: RealtimeContinuityBridge, devices: DeviceFabricBridge) -> Self {
        Self {
            realtime,
            devices,
            ranker: ListIndex::default(),
        }
    }
}

impl SearchProvider for CallContinuityActionsProvider {
    fn id(&self) -> &str {
        CALL_PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Call continuity"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        let Ok(view) = (self.realtime)(RealtimeContinuityUiAction::Snapshot) else {
            return "continuity-call-unavailable".into();
        };
        let Ok(devices) = (self.devices)(DeviceFabricUiAction::Snapshot) else {
            return "continuity-call-devices-unavailable".into();
        };
        call_projection(
            &self.realtime,
            &self.devices,
            &self.ranker,
            *ctx,
            &view,
            &devices,
        )
        .0
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
        call_projection(
            &self.realtime,
            &self.devices,
            &self.ranker,
            ctx,
            &view,
            &devices,
        )
        .1
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return SearchPage::exhausted("continuity-call-not-wanted", Vec::new());
        }
        let Ok(view) = (self.realtime)(RealtimeContinuityUiAction::Snapshot) else {
            return SearchPage::exhausted("continuity-call-unavailable", Vec::new());
        };
        let Ok(devices) = (self.devices)(DeviceFabricUiAction::Snapshot) else {
            return SearchPage::exhausted("continuity-call-devices-unavailable", Vec::new());
        };
        let (revision, results) = call_projection(
            &self.realtime,
            &self.devices,
            &self.ranker,
            ctx,
            &view,
            &devices,
        );
        page_results(revision, results, cursor, budget)
    }
}

pub const CONTINUE_PROVIDER_ID: &str = "continuity.continue";
const CONTINUE_ID: &str = "continuity.continue";
const CONTINUE_DISMISS_ID: &str = "continuity.continue.dismiss";

/// The palette's door to the top bar's "Continue <app> from <device>"
/// offer: accept it (focus that tab here) or dismiss it.
#[derive(Default)]
pub struct ContinueFromDeviceProvider {
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ListIndex,
}

impl ContinueFromDeviceProvider {
    fn row(
        id: &'static str,
        title: String,
        subtitle: String,
        score: i32,
        expected: &crate::shell::continuity_suggestion::ContinuitySuggestion,
    ) -> SearchResult {
        let expected = expected.clone();
        SearchResult {
            shortcut: None,
            kind: ResourceKind::Action,
            operation_kind: tilo_actions::OperationKind::View,
            provider_id: CONTINUE_PROVIDER_ID.into(),
            id: id.into(),
            title,
            subtitle: Some(subtitle),
            badge: Some(expected.device_label.clone()),
            score,
            params: Vec::new(),
            contract: Some(tilo_actions::ActionContract::view()),
            on_open: Rc::new(
                move |_doc,
                      _root,
                      chrome: &crate::shell::chrome_state::Chrome,
                      _args: &ActionArgs| {
                    let mut state = chrome.borrow_mut();
                    let now = std::time::Instant::now();
                    // Re-read at dispatch: a stale row must not focus a tab
                    // the person has since moved away from elsewhere.
                    if state.continuity_suggestion(now).as_ref() != Some(&expected) {
                        return Err(ActionError::Refused(
                            "that device has moved on; nothing to continue".into(),
                        ));
                    }
                    let done = if id == CONTINUE_ID {
                        state.accept_continuity_suggestion(now)
                    } else {
                        state.dismiss_continuity_suggestion(now)
                    };
                    drop(state);
                    done.map(|_| ()).ok_or_else(|| {
                        ActionError::Refused("nothing to continue from another device".into())
                    })
                },
            ),
            on_invoke: None,
            on_begin_invoke: None,
        }
    }
}

impl SearchProvider for ContinueFromDeviceProvider {
    fn id(&self) -> &str {
        CONTINUE_PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Continue from another device"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        ctx.chrome
            .continuity_suggestion(std::time::Instant::now())
            .map_or_else(
                || "continuity-continue:none".into(),
                |offer| {
                    format!(
                        "continuity-continue:{}:{}:{}",
                        offer.device.as_str(),
                        offer.revision,
                        offer.tab
                    )
                },
            )
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let Some(offer) = ctx.chrome.continuity_suggestion(std::time::Instant::now()) else {
            return Vec::new();
        };
        let device = offer.device_label.as_str();
        let app = offer.app_label.as_str();
        let rows = [
            (
                CONTINUE_ID,
                crate::shell::continuity_suggestion::continue_label(&offer),
                crate::shell::i18n::t_args(
                    "continuity-continue-subtitle",
                    &[("device", device.into())],
                ),
            ),
            (
                CONTINUE_DISMISS_ID,
                crate::shell::i18n::t_args(
                    "continuity-dismiss-title",
                    &[("app", app.into()), ("device", device.into())],
                ),
                crate::shell::i18n::t_args(
                    "continuity-dismiss-subtitle",
                    &[("device", device.into())],
                ),
            ),
        ];
        let fields: Vec<_> = rows
            .iter()
            .map(|(id, title, subtitle)| action_fields(title, subtitle, id))
            .collect();
        let rows = rows
            .into_iter()
            .map(|(id, title, subtitle)| Self::row(id, title, subtitle, 0, &offer))
            .collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle, rows, &fields)
    }
}

#[cfg(test)]
mod tests {
    use crate::shell::state::FreshId as _;
    use std::cell::RefCell;

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::overlay_host::{FabricDeviceView, FabricMediaView};
    use crate::shell::personal_continuity::DeviceId;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;

    fn device(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn fixture() -> DeviceFabricView {
        let peer = device("certified-peer-secret-looking-id");
        DeviceFabricView {
            bluetooth_authorizations: vec![
                crate::shell::overlay_host::BluetoothHandoffAuthorizationView {
                    request_id: [0x44; 16],
                    accessory_name: "Headphones".into(),
                    source_label: "Laptop".into(),
                },
            ],
            bluetooth_handoffs: vec![
                crate::shell::device_fabric_runtime::BluetoothHandoffStatusView {
                    request_id: [0x45; 16],
                    accessory_name: "Headphones".into(),
                    peer_label: "Laptop".into(),
                    outbound: true,
                    phase: BluetoothHandoffPhaseView::Failed(
                        crate::shell::device_fabric_runtime::BluetoothHandoffFailure::Send,
                    ),
                },
            ],
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
            remote_capture: crate::shell::remote_capture::runtime::RemoteCaptureView {
                local_receivers: crate::shell::remote_capture::adapter::RemoteCaptureAvailability {
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

    fn chrome() -> crate::shell::chrome_state::Chrome {
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        tree.desktop
            .workspaces
            .push(Workspace::new(id, "Continuity"));
        tree.desktop.active = id;
        let handle: WorkspaceTreeHandle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        ChromeState::new_with_handle(handle)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn continuity_locators_hide_identity_and_page_across_revisions() {
        // Phase `action_locators_do_not_disclose_identity_session_or_device_paths`.
        {
            let actions = device_actions(&fixture());
            assert!(!actions.is_empty());
            for action in &actions {
                let descriptor = format!("{} {} {}", action.id, action.title, action.subtitle);
                for secret in [
                    "certified-peer-secret-looking-id",
                    "private-session-token",
                    "2f6465762f",
                    "43434343",
                    "44444444",
                    "45454545",
                ] {
                    assert!(
                        !descriptor.contains(secret),
                        "leaked {secret} in {descriptor}"
                    );
                }
            }
            assert!(actions.iter().any(|action| {
                action.action
                    == DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                        request_id: [0x44; 16],
                        approved: true,
                    }
                    && action.confirm
            }));
            assert_ne!(
                opaque("so.tilo.continuity.device.v1", &[9; 16]),
                opaque("so.tilo.continuity.call-session.v1", &[9; 16])
            );
            assert!(actions.iter().any(|action| {
                action.action
                    == DeviceFabricUiAction::RetryBluetoothHandoff {
                        request_id: [0x45; 16],
                    }
            }));
        }

        // Phase `speaker_groups_project_consent_play_and_leave_without_disclosing_ids`.
        {
            use crate::shell::speaker_group::{
                EndpointSelector, SpeakerGroupId, SpeakerMemberStatus, SpeakerRole, SpeakerTrim,
                StereoSide,
            };
            use crate::shell::speaker_group_runtime::{
                SpeakerConsentKind, SpeakerConsentScope, SpeakerConsentView, SpeakerGroupView,
                SpeakerMemberView, SpeakerMembershipView,
            };
            let mut view = fixture();
            let group = SpeakerGroupId([0x51; 16]);
            let right = SpeakerRole::Stereo(StereoSide::Right);
            view.remote_audio
                .sources
                .push(crate::shell::remote_audio::RemoteAudioSourceView {
                    token: 77,
                    label: "All system audio".into(),
                    application: false,
                });
            view.speaker_groups.consents.push(SpeakerConsentView {
                group,
                name: "Living room".into(),
                leader: device("desk"),
                leader_label: "Desk".into(),
                scope: SpeakerConsentScope::Play(right),
                endpoint_label: Some("Speakers".into()),
            });
            view.speaker_groups.consents.push(SpeakerConsentView {
                group,
                name: "Living room".into(),
                leader: device("desk"),
                leader_label: "Desk".into(),
                scope: SpeakerConsentScope::Listen,
                endpoint_label: Some("Microphone".into()),
            });
            view.speaker_groups.memberships.push(SpeakerMembershipView {
                group,
                name: "Living room".into(),
                leader_label: "Desk".into(),
                scope: SpeakerConsentScope::Play(right),
            });
            view.speaker_groups.memberships.push(SpeakerMembershipView {
                group,
                name: "Living room".into(),
                leader_label: "Desk".into(),
                scope: SpeakerConsentScope::Listen,
            });
            view.speaker_groups.groups.push(SpeakerGroupView {
                id: group,
                name: "Living room".into(),
                leader: device("desk"),
                leader_label: "Desk".into(),
                led_locally: false,
                members: vec![SpeakerMemberView {
                    device: device("desk"),
                    device_label: "Desk".into(),
                    output: EndpointSelector([0x52; 16]),
                    output_label: None,
                    role: SpeakerRole::Stereo(StereoSide::Left),
                    trim: SpeakerTrim::default(),
                    state: SpeakerMemberStatus::Ready,
                }],
            });
            let actions = device_actions(&view);
            let join = actions
                .iter()
                .find(|action| {
                    action.action
                        == DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                            group,
                            kind: SpeakerConsentKind::Play,
                            approved: true,
                        }
                })
                .unwrap();
            assert!(join.confirm, "joining needs explicit confirmation");
            assert_eq!(join.title, "Join Living room as the right speaker");
            assert!(actions.iter().any(|action| {
                action.action
                    == DeviceFabricUiAction::LeaveSpeakerGroup {
                        group,
                        kind: SpeakerConsentKind::Play,
                    }
                    && !action.confirm
            }));
            let listen = actions
                .iter()
                .find(|action| {
                    action.action
                        == DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                            group,
                            kind: SpeakerConsentKind::Listen,
                            approved: true,
                        }
                })
                .unwrap();
            assert!(listen.confirm, "a microphone needs explicit confirmation");
            assert_eq!(
                listen.title,
                "Let Living room calibrate with this microphone"
            );
            assert!(actions.iter().any(|action| {
                action.action
                    == DeviceFabricUiAction::LeaveSpeakerGroup {
                        group,
                        kind: SpeakerConsentKind::Listen,
                    }
            }));
            assert!(actions.iter().any(|action| {
                action.action
                    == DeviceFabricUiAction::StartSpeakerGroupAudio {
                        source_token: 77,
                        group,
                    }
                    && action.confirm
            }));
            assert!(actions.iter().any(|action| {
                matches!(
                    &action.action,
                    DeviceFabricUiAction::PlayMediaOnSpeakerGroup { group: target, .. }
                        if *target == group
                )
            }));
            for action in &actions {
                assert!(!action.id.contains("51515151"), "{}", action.id);
            }
        }

        // Phase `dynamic_revision_changes_and_more_than_one_page_is_reachable`.
        {
            let mut view = fixture();
            view.devices = (0..60)
                .map(|index| FabricDeviceView {
                    id: device(&format!("certified-device-{index}")),
                    label: format!("Screen {index}"),
                    kind: DeviceKind::Laptop,
                    is_local: false,
                    online: true,
                    audio_output: true,
                    video_output: true,
                    display: true,
                    controls_local: false,
                    local_can_control: false,
                })
                .collect();
            view.local_media[0].targets = view
                .devices
                .iter()
                .map(|device| device.id.clone())
                .collect();
            let live = Rc::new(RefCell::new(view));
            let bridge_live = Rc::clone(&live);
            let bridge: DeviceFabricBridge = Rc::new(move |_| Ok(bridge_live.borrow().clone()));
            let provider = DeviceContinuityActionsProvider::new(bridge);
            let chrome_handle = chrome();
            let chrome = chrome_handle.borrow();
            let doc = AutoCommit::new();
            let wanted = [ResourceKind::Action];
            let context = |needle| SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &wanted,
                chrome: &chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };

            let before = provider.search_revision(&context("Now playing"));
            let first = provider.query_page(context("Now playing"), None, SearchBudget::PALETTE);
            assert_eq!(first.results.len(), 48);
            assert!(!first.exhausted);
            let second = provider.query_page(
                context("Now playing"),
                first.next_cursor.as_ref(),
                SearchBudget::PALETTE,
            );
            assert_eq!(second.results.len(), 12);
            assert!(second.exhausted);

            live.borrow_mut().local_media.clear();
            assert_ne!(before, provider.search_revision(&context("Now playing")));
            assert!(provider.query(context("Now playing")).is_empty());
        }
    }

    #[test]
    fn continuity_dispatch_needs_confirmation_and_a_fresh_exact_action() {
        // Phase `dispatch_requires_confirmation_and_fresh_full_action_match`.
        {
            let view = Rc::new(RefCell::new(fixture()));
            let dispatched = Rc::new(RefCell::new(Vec::new()));
            let bridge_view = Rc::clone(&view);
            let bridge_dispatched = Rc::clone(&dispatched);
            let bridge: DeviceFabricBridge = Rc::new(move |action| {
                if !matches!(
                    action,
                    DeviceFabricUiAction::Snapshot | DeviceFabricUiAction::SnapshotFresh
                ) {
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

        // Phase `dispatch_bypasses_cached_snapshot_and_refuses_stale_action`.
        {
            let cached = fixture();
            let current = Rc::new(RefCell::new(cached.clone()));
            let dispatched = Rc::new(RefCell::new(Vec::new()));
            let current_bridge = Rc::clone(&current);
            let dispatched_bridge = Rc::clone(&dispatched);
            let bridge: DeviceFabricBridge = Rc::new(move |action| match action {
                DeviceFabricUiAction::Snapshot => Ok(cached.clone()),
                DeviceFabricUiAction::SnapshotFresh => Ok(current_bridge.borrow().clone()),
                action => {
                    dispatched_bridge.borrow_mut().push(action);
                    Ok(current_bridge.borrow().clone())
                }
            });
            let expected = device_actions(&current.borrow())
                .into_iter()
                .find(|entry| matches!(entry.action, DeviceFabricUiAction::HandoffMedia { .. }))
                .unwrap()
                .action;
            current.borrow_mut().local_media.clear();

            assert!(dispatch_device_action(&bridge, &expected, true, true).is_err());
            assert!(dispatched.borrow().is_empty());
        }
    }

    fn continuity_test_group(
        id: u64,
        tab_id: u64,
        title: &str,
    ) -> crate::shell::state::WindowGroup {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
        WindowGroup {
            id,
            tabs: vec![Tab {
                id: tab_id,
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: title.into(),
                    note: String::new(),
                },
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    #[test]
    fn palette_continue_focuses_the_offered_tab_and_then_retires() {
        use crate::shell::continuity_suggestion::{ContinuityFeed, FocusObservation};
        use crate::shell::personal_continuity::{AppInstanceId, ContinuityStamp};
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use crate::shell::search::{SearchRegistry, SearchScope};
        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        let workspace = tree.desktop.workspaces[0].id;
        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.workspaces[0].strips[0].window_groups = vec![
            continuity_test_group(1, 11, "Terminal"),
            continuity_test_group(2, 22, "Notes"),
        ];
        tree.desktop.active = workspace;
        tree.desktop.active_strip.insert(workspace, strip);
        tree.desktop.set_active_group(strip, 1);
        let chrome = ChromeState::new(tree);
        let laptop = device("laptop");
        chrome.borrow_mut().set_continuity_feed(ContinuityFeed {
            local_device: None,
            observations: vec![FocusObservation {
                device: laptop.clone(),
                room,
                app: AppInstanceId(22),
                stamp: ContinuityStamp {
                    counter: 4,
                    device: laptop.clone(),
                },
                observed_at: Some(std::time::Instant::now()),
                revision: 1,
                withdrawn: false,
            }],
            devices: std::collections::BTreeMap::from([(laptop, "Laptop".to_string())]),
        });
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(ContinueFromDeviceProvider::default()));
        let mut doc = AutoCommit::new();
        let catalog = {
            let state = chrome.borrow();
            ActionCatalog::build(
                &registry,
                "",
                SearchScope::All,
                None,
                &state,
                &doc,
                &automerge::ROOT,
                &AllVisible,
            )
        };
        let titles = catalog
            .enumerate()
            .into_iter()
            .map(|action| action.title)
            .collect::<Vec<_>>();
        assert!(
            titles.contains(&"Continue Notes from Laptop".to_string()),
            "{titles:?}"
        );
        catalog
            .select(&ResourceRef::new(CONTINUE_PROVIDER_ID, CONTINUE_ID))
            .expect("continue row")
            .invoke(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                [],
            )
            .expect("accepted");
        let focused_group = chrome
            .borrow()
            .tree()
            .desktop
            .active_group
            .get(&strip)
            .copied();
        assert_eq!(focused_group, Some(2));
        assert!(
            chrome
                .borrow()
                .continuity_suggestion(std::time::Instant::now())
                .is_none()
        );
    }
}
