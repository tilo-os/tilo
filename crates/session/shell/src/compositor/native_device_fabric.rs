//! The device fabric's place in the desktop: its pump thread, and the bridge
//! that maps each Quick Settings fabric action onto its runtime call.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// Start the device fabric's pump on its own thread, or say why not.
pub(super) fn start_device_fabric(
    network: &Arc<crate::shell::network::ShellNetwork>,
    notifications: &crate::notifications::NotificationBridge,
) -> Option<
    crate::shell::device_fabric_runtime::worker::FabricWorker<
        crate::shell::device_fabric_runtime::FabricTurn,
    >,
> {
    match crate::shell::device_fabric_runtime::spawn_worker(
        Arc::clone(network),
        notifications.clone(),
    ) {
        Ok(worker) => Some(worker),
        Err(error) => {
            tracing::error!(%error, "device fabric: could not start its thread");
            None
        }
    }
}

pub(super) fn active_physical_gamepad_binding(
    tree: &crate::shell::state::WorkspaceTree,
) -> Option<crate::shell::physical_gamepad::PhysicalGamepadBinding> {
    let (workspace, tab) = active_tab(tree)?;
    let app_id = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
    let workspace_id = workspace.room().to_string();
    Some(crate::shell::physical_gamepad::PhysicalGamepadBinding {
        app_id,
        session_id: format!("{workspace_id}:{}", tab.id),
        workspace_id,
    })
}

/// The tab in front on this desktop, and its workspace.
fn active_tab(
    tree: &crate::shell::state::WorkspaceTree,
) -> Option<(&crate::shell::state::Workspace, &crate::shell::state::Tab)> {
    let workspace = tree.desktop.active_workspace()?;
    let strip_id = tree
        .desktop
        .active_strip
        .get(&workspace.id)
        .copied()
        .or_else(|| workspace.strips.first().map(|strip| strip.id))?;
    let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
    let group_id = tree
        .desktop
        .active_group_for(strip_id)
        .filter(|group_id| {
            strip
                .window_groups
                .iter()
                .any(|group| group.id == *group_id)
        })
        .or_else(|| strip.window_groups.first().map(|group| group.id))?;
    let group = strip
        .window_groups
        .iter()
        .find(|group| group.id == group_id)?;
    let tab = group.tabs.iter().find(|tab| tab.id == group.active_tab)?;
    Some((workspace, tab))
}

#[expect(
    clippy::too_many_lines,
    reason = "one bridge closure maps every Quick Settings fabric action onto its runtime call beside the shared snapshot cache"
)]
pub(super) fn install_device_fabric_bridge(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    fabric_net: Arc<crate::shell::network::ShellNetwork>,
    tree_handle: crate::shell::state::WorkspaceTreeHandle,
) {
    let fabric_cache = std::rc::Rc::new(std::cell::RefCell::new(
        None::<(Instant, u64, crate::shell::overlay_host::DeviceFabricView)>,
    ));
    // `pw-dump` and `wpctl` run on the graph's own thread; the view reads
    // the graph as last read and never waits on `PipeWire`.
    let local_audio =
        crate::shell::local_audio_graph::LocalAudioGraph::system(fabric_net.loop_waker());
    host.set_device_fabric_bridge({
        let fabric_cache = std::rc::Rc::clone(&fabric_cache);
        std::rc::Rc::new(move |action| {
            use crate::shell::overlay_host::DeviceFabricUiAction;
            if matches!(action, DeviceFabricUiAction::Snapshot)
                && let Some((updated, audio_revision, view)) = fabric_cache.borrow().as_ref()
                && updated.elapsed() < Duration::from_millis(750)
                && *audio_revision == local_audio.revision()
            {
                return Ok(view.clone());
            }
            match action {
                DeviceFabricUiAction::MoveDisplay(request) => {
                    crate::shell::device_fabric_runtime::move_device_display(
                        &fabric_net,
                        &request,
                    )?;
                }
                DeviceFabricUiAction::Snapshot | DeviceFabricUiAction::SnapshotFresh => {}
                DeviceFabricUiAction::WakeDevice { device } => {
                    crate::shell::device_wake::request_wake(&fabric_net, &device)?;
                }
                DeviceFabricUiAction::ShowWindowOn { target } => {
                    let tab = active_tab(&tree_handle.read())
                        .map(|(_, tab)| tab.id)
                        .ok_or_else(|| "no window is in front here".to_owned())?;
                    crate::shell::device_fabric_runtime::show_tab_on(&fabric_net, &target, tab)?;
                }
                DeviceFabricUiAction::HandoffMedia { session_id, target } => {
                    let _ = crate::shell::device_fabric_runtime::request_media_handoff(
                        &fabric_net,
                        &session_id,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::CancelMediaHandoff { request_id } => {
                    crate::shell::device_fabric_runtime::cancel_media_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RetryMediaHandoff { request_id } => {
                    let _ = crate::shell::device_fabric_runtime::retry_media_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RouteLocalAudio { node_id } => {
                    local_audio.submit(
                        crate::shell::local_audio_graph::LocalAudioEdit::RouteOutput(node_id),
                    )?;
                }
                DeviceFabricUiAction::SelectLocalInput { node_id } => {
                    local_audio.submit(
                        crate::shell::local_audio_graph::LocalAudioEdit::SelectInput(node_id),
                    )?;
                }
                DeviceFabricUiAction::StepInputLevel { node_id, raise } => {
                    local_audio.submit(
                        crate::shell::local_audio_graph::LocalAudioEdit::StepInput {
                            node_id,
                            raise,
                        },
                    )?;
                }
                DeviceFabricUiAction::HandoffBluetooth {
                    address,
                    name,
                    target,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_bluetooth_handoff(
                        &fabric_net,
                        &address,
                        &name,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                    request_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::CancelBluetoothHandoff { request_id } => {
                    crate::shell::device_fabric_runtime::cancel_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::RetryBluetoothHandoff { request_id } => {
                    let _ = crate::shell::device_fabric_runtime::retry_bluetooth_handoff(
                        &fabric_net,
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeGamepadForwarding {
                    forwarding_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_typed_gamepad_forwarding(
                        forwarding_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::StartPhysicalGamepad {
                    source_token,
                    target,
                } => {
                    let binding = active_physical_gamepad_binding(&tree_handle.read())
                        .ok_or_else(|| crate::shell::i18n::t("fabric-focus-app-for-controller"))?;
                    let _ = crate::shell::physical_gamepad::start_physical_gamepad_forwarding(
                        &fabric_net,
                        source_token,
                        &target,
                        binding,
                    )?;
                }
                DeviceFabricUiAction::StopPhysicalGamepad { session } => {
                    crate::shell::physical_gamepad::stop_physical_gamepad_forwarding(
                        &fabric_net,
                        session,
                    )?;
                }
                DeviceFabricUiAction::StartRemoteAudio {
                    source_token,
                    target,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_remote_audio(
                        &fabric_net,
                        source_token,
                        &target,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeRemoteAudio {
                    session_id,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::authorize_remote_audio(
                        &fabric_net,
                        session_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::StopRemoteAudio { session_id } => {
                    crate::shell::device_fabric_runtime::stop_remote_audio(
                        &fabric_net,
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::ResolveSpeakerGroupConsent {
                    group,
                    kind,
                    approved,
                } => {
                    crate::shell::device_fabric_runtime::require_speaker_groups()?;
                    crate::shell::speaker_group_runtime::resolve_consent(
                        &fabric_net,
                        group,
                        kind,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::LeaveSpeakerGroup { group, kind } => {
                    crate::shell::speaker_group_runtime::leave_group(&fabric_net, group, kind)?;
                }
                DeviceFabricUiAction::StartSpeakerGroupAudio {
                    source_token,
                    group,
                } => {
                    let _ = crate::shell::device_fabric_runtime::request_speaker_group_audio(
                        &fabric_net,
                        source_token,
                        group,
                    )?;
                }
                DeviceFabricUiAction::PlayMediaOnSpeakerGroup { session_id, group } => {
                    let _ = crate::shell::device_fabric_runtime::play_media_on_speaker_group(
                        &fabric_net,
                        &session_id,
                        group,
                    )?;
                }
                DeviceFabricUiAction::StopSpeakerGroupSession { session_id } => {
                    crate::shell::speaker_group_runtime::stop_session(&fabric_net, session_id)?;
                }
                DeviceFabricUiAction::RequestRemoteCapture { peer, kind } => {
                    let binding =
                        active_physical_gamepad_binding(&tree_handle.read()).ok_or_else(|| {
                            crate::shell::i18n::t("fabric-focus-app-for-remote-input")
                        })?;
                    let app =
                        crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
                            binding.app_id,
                            binding.workspace_id,
                        )
                        .map_err(|error| error.to_string())?;
                    crate::shell::remote_capture::runtime::request_remote_capture(
                        &fabric_net,
                        app,
                        peer,
                        kind,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeRemoteCapture {
                    request_id,
                    approved,
                } => {
                    crate::shell::remote_capture::runtime::decide_remote_capture(
                        &fabric_net,
                        request_id,
                        approved,
                    )?;
                }
                DeviceFabricUiAction::ApproveRemoteCaptureTarget { session_id } => {
                    crate::shell::remote_capture::runtime::approve_remote_capture_target(
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::StopRemoteCapture { session_id } => {
                    crate::shell::remote_capture::runtime::stop_remote_capture(
                        &fabric_net,
                        session_id,
                    )?;
                }
                DeviceFabricUiAction::RequestPhoneTethering { phone } => {
                    let _ = crate::shell::personal_tethering_runtime::request_phone_tethering(
                        &fabric_net,
                        &phone,
                    )?;
                }
                DeviceFabricUiAction::AuthorizePhoneTethering {
                    request_id,
                    approved,
                } => {
                    crate::shell::personal_tethering_runtime::authorize_phone_tethering(
                        request_id, approved,
                    )?;
                }
                DeviceFabricUiAction::DisconnectPhoneTethering { request_id } => {
                    crate::shell::personal_tethering_runtime::disconnect_phone_tethering(
                        request_id,
                    )?;
                }
                DeviceFabricUiAction::AuthorizeDeviceUnlock {
                    request_digest,
                    approved,
                } => {
                    crate::shell::device_unlock_runtime::decide_device_unlock(
                        request_digest,
                        approved,
                    )?;
                }
                #[cfg(target_os = "linux")]
                DeviceFabricUiAction::RouteThrough { device } => {
                    crate::shell::device_vpn::choose(&device, true)?;
                }
                #[cfg(target_os = "linux")]
                DeviceFabricUiAction::StopRouteThrough { device } => {
                    crate::shell::device_vpn::choose(&device, false)?;
                }
                #[cfg(target_os = "linux")]
                DeviceFabricUiAction::StopExitUser { device } => {
                    crate::shell::device_vpn::stop_device(&device);
                }
                #[cfg(target_os = "linux")]
                DeviceFabricUiAction::ShareConnection { on } => {
                    crate::shell::device_vpn::set_exit(on)?;
                }
                #[cfg(not(target_os = "linux"))]
                DeviceFabricUiAction::RouteThrough { .. }
                | DeviceFabricUiAction::StopRouteThrough { .. }
                | DeviceFabricUiAction::StopExitUser { .. }
                | DeviceFabricUiAction::ShareConnection { .. } => {
                    return Err("routing through your devices needs Linux".into());
                }
                DeviceFabricUiAction::RemoveDevice { device } => {
                    let mut key = [0_u8; 32];
                    hex::decode_to_slice(device.as_str(), &mut key)
                        .map_err(|error| error.to_string())?;
                    crate::shell::device_unlock_runtime::remove_device(key)?;
                }
                DeviceFabricUiAction::Place { device, edge } => {
                    crate::shell::device_fabric_runtime::place_device(&fabric_net, &device, edge)?;
                }
                DeviceFabricUiAction::ApplyCalibration { device } => {
                    crate::shell::device_fabric_runtime::apply_spatial_calibration(
                        &fabric_net,
                        &device,
                    )?;
                }
                DeviceFabricUiAction::DismissCalibration { device } => {
                    crate::shell::device_fabric_runtime::dismiss_spatial_calibration(&device)?;
                }
                DeviceFabricUiAction::RevertCalibration { device } => {
                    crate::shell::device_fabric_runtime::revert_spatial_calibration(
                        &fabric_net,
                        &device,
                    )?;
                }
                DeviceFabricUiAction::GrantControl { controller } => {
                    let _ = crate::shell::device_fabric_runtime::grant_input_control(
                        &fabric_net,
                        &controller,
                    )?;
                }
                DeviceFabricUiAction::RevokeControl { controller } => {
                    crate::shell::device_fabric_runtime::revoke_input_control(
                        &fabric_net,
                        &controller,
                    )?;
                }
            }
            let audio_revision = local_audio.revision();
            let mut view = crate::shell::device_fabric_runtime::device_fabric_view(
                &fabric_net,
                local_audio.graph(),
            );
            view.local_gamepads =
                crate::shell::physical_gamepad::local_physical_gamepads().unwrap_or_default();
            let focus = active_physical_gamepad_binding(&tree_handle.read());
            view.remote_audio =
                crate::shell::device_fabric_runtime::remote_audio_view(focus.as_ref());
            *fabric_cache.borrow_mut() = Some((Instant::now(), audio_revision, view.clone()));
            Ok(view)
        })
    });
}

/// End a spatial control session: the device it controlled gets the
/// release of every key and button still held there.
pub(super) fn end_spatial_control(
    network: &crate::shell::network::ShellNetwork,
    active: &mut Option<super::SpatialControlSession>,
) {
    let Some(session) = active.take() else {
        return;
    };
    let events = session
        .pressed_keys
        .into_iter()
        .map(
            |keycode| crate::shell::device_fabric::FabricInputEvent::Key {
                keycode,
                pressed: false,
                modifiers: 0,
            },
        )
        .chain(session.pressed_buttons.into_iter().map(|button| {
            crate::shell::device_fabric::FabricInputEvent::PointerButton {
                button,
                pressed: false,
            }
        }))
        .collect();
    let _ = crate::shell::device_fabric_runtime::queue_spatial_input_cleanup_bound(
        network,
        &session.target,
        session.lease_nonce,
        events,
    );
}

/// [`end_spatial_control`] in a session with a network; one without has no
/// device to tell, and only forgets the session.
pub(super) fn end_spatial_control_or_forget(
    network: Option<&crate::shell::network::ShellNetwork>,
    active: &mut Option<super::SpatialControlSession>,
) {
    match network {
        Some(network) => end_spatial_control(network, active),
        None => *active = None,
    }
}
