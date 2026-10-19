use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::collab_broker_capnp::{
    RealtimeConsentUiView, RealtimeContinuityUiView, RealtimeSourceUiView,
};
use crate::shell::device_fabric::DeviceKind;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::state::WorkspaceTree;
use std::cell::RefCell;
use std::rc::Rc;
use ui::widget::Container;

fn fixture() -> (DeviceFabricView, DeviceId) {
    let remote = DeviceId::new("certified-laptop").unwrap();
    (
        DeviceFabricView {
            displays: Vec::new(),
            bluetooth_authorizations: Vec::new(),
            bluetooth_handoffs: Vec::new(),
            gamepad_authorizations: Vec::new(),
            local_gamepads: Vec::new(),
            devices: vec![FabricDeviceView {
                id: remote.clone(),
                label: "Travel laptop".into(),
                kind: DeviceKind::Laptop,
                is_local: false,
                online: true,
                audio_output: true,
                video_output: true,
                display: true,
                controls_local: false,
                local_can_control: false,
            }],
            local_media: vec![FabricMediaView {
                session_id: "so.tilo.video:7".into(),
                title: "Shared film".into(),
                playing: true,
                has_video: true,
                targets: vec![remote.clone()],
            }],
            media_handoffs: Vec::new(),
            local_outputs: vec![LocalMediaOutputView {
                node_id: 41,
                label: "Bluetooth headphones".into(),
                is_default: false,
                bluetooth_codec: Some("AAC".into()),
            }],
            local_inputs: Vec::new(),
            transfers: Vec::new(),
            remote_audio: crate::shell::remote_audio::RemoteAudioView::default(),
            speaker_groups: crate::shell::speaker_group_runtime::SpeakerGroupsView::default(),
            device_health: Vec::new(),
            calibration_proposals: Vec::new(),
            revertible_calibrations: Vec::new(),
            tethering: Vec::new(),
            remote_capture: crate::shell::remote_capture::runtime::RemoteCaptureView::default(),
            device_unlocks: Vec::new(),
        },
        remote,
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn continuity_catalog_registers_providers_and_reaches_every_device_and_session() {
    fn continuity_providers_register_in_either_order_and_replace_stale_bridges() {
        use crate::shell::search::action_api::AllVisible;
        use crate::shell::search::providers::continuity::{CALL_PROVIDER_ID, DEVICE_PROVIDER_ID};

        let realtime: RealtimeContinuityBridge =
            Rc::new(|_| Ok(RealtimeContinuityUiView::default()));
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.set_realtime_continuity_bridge(Rc::clone(&realtime));
        assert_eq!(
            host.search_registry
                .providers_iter()
                .filter(|provider| provider.id() == CALL_PROVIDER_ID)
                .count(),
            0
        );

        let (old_view, _) = fixture();
        host.set_device_fabric_bridge(Rc::new(move |_| Ok(old_view.clone())));
        for id in [DEVICE_PROVIDER_ID, CALL_PROVIDER_ID] {
            assert_eq!(
                host.search_registry
                    .providers_iter()
                    .filter(|provider| provider.id() == id)
                    .count(),
                1,
                "{id} must be registered exactly once"
            );
        }

        let (mut current_view, _) = fixture();
        current_view.local_media[0].title = "Replacement film".into();
        host.set_device_fabric_bridge(Rc::new(move |_| Ok(current_view.clone())));
        for id in [DEVICE_PROVIDER_ID, CALL_PROVIDER_ID] {
            assert_eq!(
                host.search_registry
                    .providers_iter()
                    .filter(|provider| provider.id() == id)
                    .count(),
                1,
                "rebinding must replace {id}, not append it"
            );
        }

        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        let catalog = host.action_catalog(
            "Replacement film",
            &state,
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(
            catalog
                .enumerate()
                .iter()
                .any(|action| action.title.contains("Replacement film"))
        );

        let (view, _) = fixture();
        let mut reverse = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        reverse.set_device_fabric_bridge(Rc::new(move |_| Ok(view.clone())));
        reverse.set_realtime_continuity_bridge(realtime);
        assert_eq!(
            reverse
                .search_registry
                .providers_iter()
                .filter(|provider| provider.id() == CALL_PROVIDER_ID)
                .count(),
            1
        );
    }

    fn headless_continuity_catalog_lists_and_resolves_beyond_palette_page() {
        use crate::shell::search::action_api::{AllVisible, ResourceRef};
        use crate::shell::search::providers::continuity::DEVICE_PROVIDER_ID;

        let (mut view, _) = fixture();
        view.devices = (0..60)
            .map(|index| FabricDeviceView {
                id: DeviceId::new(format!("certified-screen-{index}")).unwrap(),
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
        view.local_media[0].title = "Paging proof".into();
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.set_device_fabric_bridge(Rc::new(move |_| Ok(view.clone())));
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let state = chrome.borrow();
        let doc = AutoCommit::new();

        let catalog =
            host.action_catalog("Paging proof", &state, &doc, &automerge::ROOT, &AllVisible);
        let actions = catalog
            .enumerate()
            .into_iter()
            .filter(|action| action.resource.provider == DEVICE_PROVIDER_ID)
            .collect::<Vec<_>>();
        assert_eq!(actions.len(), 60);
        let last = actions.last().unwrap().resource.clone();
        let exact = host.exact_action_catalog(
            &last.provider,
            &last.id,
            &state,
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(exact.contains(&ResourceRef::new(&last.provider, &last.id)));
    }

    fn exact_session_action_ignores_temporary_active_app_search_scope() {
        use crate::shell::search::action_api::{AllVisible, ResourceRef};

        let host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::ActiveApp;
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        let exact = host.exact_action_catalog(
            crate::shell::search::providers::session_actions::PROVIDER_ID,
            crate::shell::search::providers::session_actions::LOGOUT_ID,
            &state,
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );

        assert!(exact.contains(&ResourceRef::new("session", "logout")));
    }

    fn every_enrolled_device_remains_reachable_beyond_the_compact_first_page() {
        let (mut view, _) = fixture();
        view.devices = (0..12)
            .map(|index| FabricDeviceView {
                id: DeviceId::new(format!("device-{index}")).unwrap(),
                label: format!("Device {index}"),
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
        let total = device_fabric_pane_rows(&view, &RealtimeContinuityUiView::default()).len();
        assert_eq!(
            total,
            // One extra row offers speaker setup once two devices can play.
            view.local_media.len() + view.local_outputs.len() + view.devices.len() + 1,
            "the pane model must not truncate media, outputs, or devices"
        );
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |_| Ok(view.clone())));
        host.open_devices_for_tests();
        // In place, the section lists every row; the page scrolls to them.
        let rows = chrome.borrow().quick_settings.device_rows.len();
        assert_eq!(rows, total, "every destination is a row of the section");
        let last = host.qs_devices_row_rect(rows - 1);
        assert!(last.height() > 0.0, "and each has its place on the page");
    }

    continuity_providers_register_in_either_order_and_replace_stale_bridges();
    headless_continuity_catalog_lists_and_resolves_beyond_palette_page();
    exact_session_action_ignores_temporary_active_app_search_scope();
    every_enrolled_device_remains_reachable_beyond_the_compact_first_page();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn device_pane_media_rows_route_exact_requests_and_retry_or_refuse() {
    fn device_pane_routes_local_audio_and_exact_remote_media() {
        let (view, remote) = fixture();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let bridge_view = view;
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(bridge_view.clone())
        }));
        chrome
            .borrow_mut()
            .open_quick_settings_section(crate::shell::quick_settings::QsSection::Devices);
        assert_eq!(
            chrome.borrow().quick_settings.section,
            Some(crate::shell::quick_settings::QsSection::Devices)
        );
        host.open_devices_for_tests();

        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        let device_row = host.qs_devices_row_rect(2);
        host.handle_qs_device_fabric_click(Point::new(device_row.x0 + 44.0, device_row.center().y));
        let (placements, control) =
            ShellOverlayHost::qs_device_row_actions(host.qs_devices_row_rect(2));
        host.handle_qs_device_fabric_click(placements[1].center());
        host.handle_qs_device_fabric_click(control.center());
        let actions = actions.borrow();
        assert!(actions.contains(&DeviceFabricUiAction::RouteLocalAudio { node_id: 41 }));
        assert!(actions.contains(&DeviceFabricUiAction::HandoffMedia {
            session_id: "so.tilo.video:7".into(),
            target: remote,
        }));
        assert!(actions.contains(&DeviceFabricUiAction::Place {
            device: DeviceId::new("certified-laptop").unwrap(),
            edge: crate::shell::device_fabric::DisplayEdge::Right,
        }));
        assert!(actions.contains(&DeviceFabricUiAction::GrantControl {
            controller: DeviceId::new("certified-laptop").unwrap(),
        }));
    }

    fn failed_media_handoff_row_retries_the_exact_request() {
        let (mut view, remote) = fixture();
        view.media_handoffs = vec![
            crate::shell::device_fabric_runtime::MediaHandoffStatusView {
                request_id: [7; 16],
                session_id: "so.tilo.video:7".into(),
                title: "Shared film".into(),
                target: remote,
                target_label: "Travel laptop".into(),
                phase: crate::shell::device_fabric_runtime::MediaHandoffPhase::Failed(
                    crate::shell::device_fabric::MediaHandoffFailure::OpenFailed,
                ),
            },
        ];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        let (allow, _) = ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(allow.center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::RetryMediaHandoff {
                    request_id: [7; 16],
                })
        );
    }

    fn device_pane_routes_opaque_audio_source_and_requires_target_choice() {
        let (mut view, remote) = fixture();
        view.local_media.clear();
        view.local_outputs.clear();
        view.remote_audio = crate::shell::remote_audio::RemoteAudioView {
            sources: vec![crate::shell::remote_audio::RemoteAudioSourceView {
                token: 77,
                label: "All system audio".into(),
                application: false,
            }],
            routes: Vec::new(),
            consents: vec![crate::shell::remote_audio::RemoteAudioConsentView {
                session_id: [8; 16],
                source: remote.clone(),
                label: "All system audio".into(),
            }],
        };
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(2).center());

        let actions = actions.borrow();
        assert!(
            actions.contains(&DeviceFabricUiAction::AuthorizeRemoteAudio {
                session_id: [8; 16],
                approved: false,
            })
        );
        assert!(actions.contains(&DeviceFabricUiAction::StartRemoteAudio {
            source_token: 77,
            target: remote,
        }));
    }

    fn incompatible_device_row_does_not_emit_a_handoff() {
        let (mut view, _) = fixture();
        view.local_media[0].targets.clear();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let host_view = view.clone();
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(host_view.clone())
        }));
        host.open_devices_for_tests();
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(2).center());
        assert!(
            !actions
                .borrow()
                .iter()
                .any(|action| matches!(action, DeviceFabricUiAction::HandoffMedia { .. }))
        );
    }

    device_pane_routes_local_audio_and_exact_remote_media();
    failed_media_handoff_row_retries_the_exact_request();
    device_pane_routes_opaque_audio_source_and_requires_target_choice();
    incompatible_device_row_does_not_emit_a_handoff();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn device_pane_bluetooth_pairing_and_handoffs_need_local_choice_and_exact_targets() {
    fn device_pane_can_decline_new_bluetooth_pairing() {
        let (mut view, _) = fixture();
        view.bluetooth_authorizations = vec![BluetoothHandoffAuthorizationView {
            request_id: [10; 16],
            accessory_name: "Keyboard".into(),
            source_label: "Laptop".into(),
        }];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                    request_id: [10; 16],
                    approved: false,
                })
        );
    }

    fn bluetooth_handoff_rows_cancel_active_and_retry_failed_requests() {
        let (mut view, _) = fixture();
        view.bluetooth_handoffs = vec![
            crate::shell::device_fabric_runtime::BluetoothHandoffStatusView {
                request_id: [11; 16],
                accessory_name: "Headphones".into(),
                peer_label: "Laptop".into(),
                outbound: true,
                phase:
                    crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::ConnectingTarget,
            },
            crate::shell::device_fabric_runtime::BluetoothHandoffStatusView {
                request_id: [12; 16],
                accessory_name: "Headphones".into(),
                peer_label: "Laptop".into(),
                outbound: true,
                phase: crate::shell::device_fabric_runtime::BluetoothHandoffPhaseView::Failed(
                    crate::shell::device_fabric_runtime::BluetoothHandoffFailure::ConnectTarget,
                ),
            },
        ];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(0).center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        let actions = actions.borrow();
        assert!(
            actions.contains(&DeviceFabricUiAction::CancelBluetoothHandoff {
                request_id: [11; 16],
            })
        );
        assert!(
            actions.contains(&DeviceFabricUiAction::RetryBluetoothHandoff {
                request_id: [12; 16],
            })
        );
    }

    fn device_pane_requires_a_local_click_for_new_bluetooth_pairing() {
        let (mut view, _) = fixture();
        view.bluetooth_authorizations = vec![BluetoothHandoffAuthorizationView {
            request_id: [9; 16],
            accessory_name: "Headphones".into(),
            source_label: "Laptop".into(),
        }];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));

        assert!(actions.borrow().is_empty());
        host.open_devices_for_tests();
        let (allow, _) = ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(allow.center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::AuthorizeBluetoothHandoff {
                    request_id: [9; 16],
                    approved: true,
                })
        );
    }

    fn bluetooth_details_routes_exact_accessory_to_exact_certified_device() {
        let (view, remote) = fixture();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.bt_devices = vec![crate::shell::quick_settings::BtDevice {
                name: "Headphones".into(),
                address: "AA:BB:CC:DD:EE:FF".into(),
                battery: 80,
                connected: true,
            }];
            state.quick_settings.push_level(
                crate::shell::quick_settings::QsLevel::BluetoothDetails("AA:BB:CC:DD:EE:FF".into()),
            );
        }
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()))
            .with_chrome(chrome);
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));

        host.handle_qs_bluetooth_details_click(
            host.qs_device_row_rect(1).center(),
            "AA:BB:CC:DD:EE:FF",
        );
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::HandoffBluetooth {
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    name: "Headphones".into(),
                    target: remote,
                })
        );
    }

    device_pane_can_decline_new_bluetooth_pairing();
    bluetooth_handoff_rows_cancel_active_and_retry_failed_requests();
    device_pane_requires_a_local_click_for_new_bluetooth_pairing();
    bluetooth_details_routes_exact_accessory_to_exact_certified_device();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn device_pane_settings_controllers_and_calls_require_explicit_choices() {
    fn device_pane_requires_explicit_apply_and_keeps_revert_reachable() {
        let (mut view, remote) = fixture();
        view.calibration_proposals = vec![crate::shell::spatial_calibration::CalibrationProposal {
            peer: remote.clone(),
            edge: crate::shell::device_fabric::DisplayEdge::Right,
            previous: crate::shell::device_fabric::DisplayRect {
                x: 800,
                y: 0,
                width: 100,
                height: 80,
            },
            proposed: crate::shell::device_fabric::DisplayRect {
                x: 100,
                y: 0,
                width: 100,
                height: 80,
            },
            evidence_kind: crate::shell::spatial_calibration::SpatialEvidenceKind::UltraWideband,
            confidence_milli: 900,
            expires_unix_ms: 20_000,
        }];
        view.revertible_calibrations = vec![remote.clone()];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));

        assert!(
            actions.borrow().is_empty(),
            "proposals must never auto-apply"
        );
        host.open_devices_for_tests();
        let (apply, _) = ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(apply.center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        let actions = actions.borrow();
        assert!(actions.contains(&DeviceFabricUiAction::ApplyCalibration {
            device: remote.clone(),
        }));
        assert!(actions.contains(&DeviceFabricUiAction::RevertCalibration { device: remote }));
    }

    fn device_pane_requires_an_explicit_gamepad_consent_choice() {
        let (mut view, _) = fixture();
        view.gamepad_authorizations = vec![GamepadAuthorizationView {
            forwarding_id: [7; 16],
            controller_label: "Arcade controller".into(),
            source_label: "Travel laptop".into(),
            app_id: "so.tilo.game".into(),
        }];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::AuthorizeGamepadForwarding {
                    forwarding_id: [7; 16],
                    approved: false,
                })
        );
    }

    fn device_pane_starts_a_selected_local_controller_on_exact_device() {
        let (mut view, remote) = fixture();
        view.local_media.clear();
        view.local_outputs.clear();
        view.local_gamepads = vec![crate::shell::physical_gamepad::LocalPhysicalGamepadView {
            token: 42,
            name: "Arcade controller".into(),
            forwarding: None,
        }];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(0).center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());

        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::StartPhysicalGamepad {
                    source_token: 42,
                    target: remote,
                })
        );
    }

    fn device_pane_proposes_calls_and_requires_explicit_consent_choice() {
        let (view, remote) = fixture();
        let source = RealtimeSourceUiView {
            session: "source-session".into(),
            room_label: "Design huddle".into(),
        };
        let consent = RealtimeConsentUiView {
            session: "target-session".into(),
            room_label: "Support call".into(),
            source: remote.clone(),
        };
        let realtime_view = RealtimeContinuityUiView {
            sources: vec![source],
            consents: vec![consent],
        };
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |_| Ok(view.clone())));
        host.set_realtime_continuity_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(realtime_view.clone())
        }));
        host.open_devices_for_tests();

        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        // Choosing a device after the call's source proposes it there.
        host.handle_qs_device_fabric_click(Point::new(
            host.qs_devices_row_rect(3).x0 + 44.0,
            host.qs_devices_row_rect(3).center().y,
        ));
        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        // Enter on the request allows it.
        host.press_qs_device_row(0, None);

        let actions = actions.borrow();
        assert!(actions.contains(&RealtimeContinuityUiAction::Propose {
            session: "source-session".into(),
            target: remote,
        }));
        assert!(
            actions.contains(&RealtimeContinuityUiAction::ResolveConsent {
                session: "target-session".into(),
                approved: false,
            })
        );
        assert!(
            actions.contains(&RealtimeContinuityUiAction::ResolveConsent {
                session: "target-session".into(),
                approved: true,
            })
        );
    }

    device_pane_requires_explicit_apply_and_keeps_revert_reachable();
    device_pane_requires_an_explicit_gamepad_consent_choice();
    device_pane_starts_a_selected_local_controller_on_exact_device();
    device_pane_proposes_calls_and_requires_explicit_consent_choice();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn device_pane_tethering_and_capture_require_explicit_consent() {
    fn device_pane_requests_tethering_from_the_exact_certified_phone() {
        let (mut view, remote) = fixture();
        view.local_media.clear();
        view.local_outputs.clear();
        view.devices[0].kind = DeviceKind::Phone;
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(0).center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::RequestPhoneTethering { phone: remote })
        );
    }

    fn device_pane_requires_explicit_tethering_consent_and_exposes_disconnect() {
        let (mut view, remote) = fixture();
        view.local_media.clear();
        view.local_outputs.clear();
        view.devices.clear();
        view.tethering = vec![
            crate::shell::personal_tethering_runtime::TetheringStatusView {
                request_id: [14; 16],
                peer: remote,
                peer_label: "Travel laptop".into(),
                direction: crate::shell::personal_tethering_runtime::TetheringDirection::Provider,
                phase: crate::shell::personal_tethering_runtime::TetheringPhase::ConsentRequired,
                expires_unix_ms: 30_000,
            },
        ];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let shared = Rc::new(RefCell::new(view));
        let bridge_view = Rc::clone(&shared);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(bridge_view.borrow().clone())
        }));
        host.open_devices_for_tests();

        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::AuthorizePhoneTethering {
                    request_id: [14; 16],
                    approved: false,
                })
        );

        shared.borrow_mut().tethering[0].phase =
            crate::shell::personal_tethering_runtime::TetheringPhase::Active;
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(0).center());
        assert!(
            actions
                .borrow()
                .contains(&DeviceFabricUiAction::DisconnectPhoneTethering {
                    request_id: [14; 16],
                })
        );
    }

    fn device_pane_routes_remote_capture_and_requires_explicit_source_consent() {
        let (mut view, remote) = fixture();
        view.local_media.clear();
        view.local_outputs.clear();
        view.remote_capture.local_receivers.camera = true;
        view.remote_capture.consents = vec![
            crate::shell::remote_capture::runtime::RemoteCaptureConsentView {
                request_id: [12; 16],
                peer: remote.clone(),
                app_label: "Camera".into(),
                profile: crate::shell::remote_capture::device::RemoteCaptureProfile::Camera {
                    format: crate::shell::remote_capture::device::RemoteVideoFormat::Nv12,
                    width: 1280,
                    height: 720,
                    frames_per_second: 30,
                },
            },
        ];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&actions);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        host.set_device_fabric_bridge(Rc::new(move |action| {
            recorded.borrow_mut().push(action);
            Ok(view.clone())
        }));
        host.open_devices_for_tests();

        let (_, decline) =
            ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
        host.handle_qs_device_fabric_click(decline.center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
        host.handle_qs_device_fabric_click(host.qs_devices_row_rect(2).center());

        let actions = actions.borrow();
        assert!(
            actions.contains(&DeviceFabricUiAction::AuthorizeRemoteCapture {
                request_id: [12; 16],
                approved: false,
            })
        );
        assert!(
            actions.contains(&DeviceFabricUiAction::RequestRemoteCapture {
                peer: remote,
                kind: crate::shell::remote_capture::device::RemoteCaptureKind::Camera,
            })
        );
    }

    device_pane_requests_tethering_from_the_exact_certified_phone();
    device_pane_requires_explicit_tethering_consent_and_exposes_disconnect();
    device_pane_routes_remote_capture_and_requires_explicit_source_consent();
}

#[test]
fn device_pane_lists_microphones_and_switches_and_levels_them() {
    let (mut view, _) = fixture();
    view.local_media.clear();
    view.local_outputs.clear();
    view.devices.clear();
    view.local_inputs = vec![
        LocalMediaInputView {
            node_id: 70,
            label: "Built-in microphone".into(),
            is_default: true,
            level_percent: Some(62),
            muted: false,
        },
        LocalMediaInputView {
            node_id: 71,
            label: "USB headset".into(),
            is_default: false,
            level_percent: Some(40),
            muted: false,
        },
    ];
    let rows = device_fabric_pane_rows(
        &view,
        &crate::shell::collab_broker_capnp::RealtimeContinuityUiView::default(),
    );
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                DeviceFabricPaneRow::LocalInput(input) => Some(input.label.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["Built-in microphone", "USB headset"],
        "the picker lists every source, current first"
    );
    assert_eq!(
        ShellOverlayHost::local_input_detail(&view.local_inputs[0]),
        "Current microphone · input level 62%"
    );

    let actions = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&actions);
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
    host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    host.set_device_fabric_bridge(Rc::new(move |action| {
        recorded.borrow_mut().push(action);
        Ok(view.clone())
    }));
    host.open_devices_for_tests();
    let (lower, raise) = ShellOverlayHost::qs_realtime_consent_actions(host.qs_devices_row_rect(0));
    host.handle_qs_device_fabric_click(raise.center());
    host.handle_qs_device_fabric_click(lower.center());
    host.handle_qs_device_fabric_click(host.qs_devices_row_rect(1).center());
    let actions = actions.borrow();
    assert!(actions.contains(&DeviceFabricUiAction::StepInputLevel {
        node_id: 70,
        raise: true
    }));
    assert!(actions.contains(&DeviceFabricUiAction::StepInputLevel {
        node_id: 70,
        raise: false
    }));
    assert!(actions.contains(&DeviceFabricUiAction::SelectLocalInput { node_id: 71 }));
    assert!(
        !actions.contains(&DeviceFabricUiAction::SelectLocalInput { node_id: 70 }),
        "the current microphone is not re-selected"
    );
}

#[test]
fn display_move_requires_settings_owner_and_an_unlocked_session() {
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()));
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    host.chrome = Some(chrome.clone());
    let calls = Rc::new(RefCell::new(Vec::new()));
    let received = calls.clone();
    host.set_device_fabric_bridge(Rc::new(move |action| {
        received.borrow_mut().push(action);
        Ok(DeviceFabricView::default())
    }));
    let request = settings_store::device_displays::DeviceDisplayMove {
        screen: settings_store::device_displays::DeviceDisplay {
            device: "peer".into(),
            device_label: "Peer".into(),
            connector: "eDP-1".into(),
            output_id: None,
            label: "Panel".into(),
            online: true,
            local: false,
            x: 1920,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 7,
        },
        x: 0,
        y: 1080,
    };
    chrome.borrow_mut().test_only_owner_override = Some(false);
    assert!(
        host.request_device_display_move("so.tilo.settings", request.clone())
            .is_err()
    );
    chrome.borrow_mut().test_only_owner_override = Some(true);
    assert!(
        host.request_device_display_move("so.tilo.terminal", request.clone())
            .is_err()
    );
    chrome.borrow_mut().set_session_locked(true);
    assert!(
        host.request_device_display_move("so.tilo.settings", request.clone())
            .is_err()
    );
    assert!(calls.borrow().is_empty());
    chrome.borrow_mut().set_session_locked(false);
    host.request_device_display_move("so.tilo.settings", request.clone())
        .unwrap();
    assert_eq!(
        calls.borrow().as_slice(),
        &[DeviceFabricUiAction::MoveDisplay(request)]
    );
}
