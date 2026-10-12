use super::*;

#[test]
fn blocked_quick_settings_read_does_not_turn_waiting_into_animation() {
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    host.qs_refresh.kick_fast_with(move || {
        let _ = release_rx.recv();
        crate::shell::quick_settings::refresh::QsReading::Fast(Box::new(
            crate::shell::quick_settings::refresh::FastReading::empty(),
        ))
    });
    assert!(host.qs_refresh.is_in_flight());
    assert!(
        !host.needs_animation_frame(),
        "waiting on an OS command is not visual motion and must not repaint the desktop"
    );
    release_tx.send(()).expect("release test worker");
}

#[test]
fn a_reading_with_no_audio_device_leaves_no_volume_slider() {
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    host.qs_refresh.kick_fast_with(|| {
        crate::shell::quick_settings::refresh::QsReading::Fast(Box::new(
            crate::shell::quick_settings::refresh::FastReading::empty(),
        ))
    });
    let deadline = tilo_test_env::hang_deadline();
    while host.qs_refresh.is_in_flight() && std::time::Instant::now() < deadline {
        host.apply_quick_settings_readings();
        std::thread::yield_now();
    }
    host.apply_quick_settings_readings();
    let st = chrome.borrow();
    assert!(
        !st.quick_settings.volume_available,
        "no sink, no volume slider"
    );
    assert!(
        !st.quick_settings.brightness_available,
        "no backlight, no brightness slider"
    );
}

#[test]
fn ordinary_host_update_adopts_completed_wifi_scan_without_a_paint() {
    use os_actions::WifiNetworkInfo;
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let mut scope = ui::WakeScope::new();
    scope
        .bind(move |_| {
            let _ = wake_tx.send(());
        })
        .unwrap();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host = {
        let _entered = scope.enter();
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone())
    };
    chrome.borrow_mut().quick_settings.wifi_scanning = true;
    host.qs_refresh.kick_wifi_networks_with(|| {
        Ok(vec![WifiNetworkInfo {
            ssid: "Fixture network".into(),
            signal: 3,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
        }])
    });
    tilo_test_env::recv("a completed scan must wake its owning host", &wake_rx);
    let changed = host.update(&automerge::AutoCommit::new(), &automerge::ROOT);
    let state = chrome.borrow();
    assert!(
        !host.qs_refresh.is_in_flight(),
        "the normal update must drain the actual completed worker"
    );
    assert!(!state.quick_settings.wifi_scanning);
    assert_eq!(state.quick_settings.wifi_networks.len(), 1);
    assert_eq!(
        state.quick_settings.wifi_networks[0].ssid,
        "Fixture network"
    );
    assert!(
        changed,
        "adopting a network scan must schedule the visible frame"
    );
}

#[test]
fn native_host_wake_delivers_a_failed_scan_to_the_ordinary_update() {
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    host.set_quick_settings_wake(std::sync::Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    chrome.borrow_mut().quick_settings.wifi_scanning = true;
    host.qs_refresh
        .kick_wifi_networks_with(|| Err("fixture scan refused".into()));
    tilo_test_env::recv(
        "native completion must wake the detached owning session",
        &wake_rx,
    );
    assert!(host.update(&automerge::AutoCommit::new(), &automerge::ROOT));
    assert!(!host.qs_refresh.is_in_flight());
    let state = chrome.borrow();
    assert!(!state.quick_settings.wifi_scanning);
    assert_eq!(
        state.quick_settings.wifi_scan_error.as_deref(),
        Some("fixture scan refused")
    );
    assert!(
        !host.needs_animation_frame(),
        "receipt handling must not turn I/O waiting into animation"
    );
}

#[test]
fn wifi_radio_dispatch_does_not_wait_for_the_native_network_operation() {
    struct SlowRadio;
    impl os_actions::Dispatcher for SlowRadio {
        fn prepare_radio_change(
            &self,
            change: os_actions::RadioChange,
        ) -> Option<os_actions::RadioWork> {
            Some(Box::new(move || change.execute(&Self)))
        }
        fn set_brightness(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, _: bool) -> Result<(), String> {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Err("fixture radio refusal".into())
        }
        fn set_bluetooth(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _: &str, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(&self, _: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    chrome.borrow_mut().quick_settings.wifi = false;
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(SlowRadio));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    let started = std::time::Instant::now();
    host.dispatch_quick_settings_toggle_actions(
        crate::shell::quick_settings::root::PendingRootActions {
            wifi: Some(true),
            ..Default::default()
        },
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(200),
        "ordinary Quick Settings input dispatch must not wait for a native radio operation"
    );
    assert!(chrome.borrow().quick_settings.radio_change_pending);
    assert!(!chrome.borrow().quick_settings.wifi);
    assert!(!host.needs_animation_frame());
    wake_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("radio completion wakes native host");
    let doc = automerge::AutoCommit::new();
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(!chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "a refused native write must not turn the switch on"
    );
    assert_eq!(chrome.borrow().notifications().len(), 1);
    host.update(&doc, &automerge::ROOT);
    assert_eq!(
        chrome.borrow().notifications().len(),
        1,
        "the same receipt cannot publish a second failure"
    );
}

#[test]
fn wifi_drill_in_waits_for_its_owned_radio_receipt_and_reenables_controls() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    use crate::shell::quick_settings::wifi::{QuickSettingsWifiPane, WifiPaneOutcome};
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    chrome.borrow_mut().quick_settings.wifi = false;
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(fixture.dispatcher));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    let panel = Rect::new(100.0, 50.0, 500.0, 500.0);
    let toggle = QuickSettingsWifiPane::action_rects(panel)
        .into_iter()
        .find(|(name, _)| *name == "quick_settings_wifi_toggle")
        .unwrap()
        .1;
    let outcome = host.qs_wifi_pane.handle_click(
        toggle.center(),
        panel,
        &mut chrome.borrow_mut().quick_settings,
        &crate::shell::search::providers::actions::ValidatedQsActions::new(
            host.qs_actions.as_ref(),
        ),
    );
    assert_eq!(outcome, WifiPaneOutcome::RequestWifi(true));
    host.apply_qs_wifi_outcome(&chrome, outcome);
    assert_eq!(
        fixture
            .entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        os_actions::RadioChange::Wifi(true)
    );
    assert!(chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "admission is not radio success"
    );
    assert!(!host.needs_animation_frame());
    assert!(
        host.qs_wifi_pane
            .semantic_rects(panel, &chrome.borrow().quick_settings)
            .iter()
            .all(|(name, _)| name != "quick_settings_wifi_toggle")
    );
    let doc = automerge::AutoCommit::new();
    let before_update = std::time::Instant::now();
    host.update(&doc, &automerge::ROOT);
    assert!(
        before_update.elapsed() < std::time::Duration::from_millis(200),
        "unrelated updates cannot wait on radio work"
    );
    fixture.release.send(Ok(())).unwrap();
    wake_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(host.update(&doc, &automerge::ROOT));
    let state = chrome.borrow();
    assert!(!state.quick_settings.radio_change_pending);
    assert!(state.quick_settings.wifi);
    assert!(state.quick_settings.radios.wifi_enabled);
    assert!(
        host.qs_wifi_pane
            .semantic_rects(panel, &state.quick_settings)
            .iter()
            .any(|(name, _)| name == "quick_settings_wifi_toggle")
    );
    assert!(state.notifications().is_empty());
}

#[test]
fn retired_radio_receipt_stays_pending_until_its_actual_worker_finishes() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    chrome.borrow_mut().quick_settings.wifi = false;
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome.clone())
        .with_qs_actions(Box::new(fixture.dispatcher.clone()));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    let job = host
        .qs_action_slot
        .start_radio(os_actions::RadioChange::Wifi(true), &chrome)
        .unwrap()
        .expect("a prepared operation owns a pending receipt");
    fixture
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    host.qs_action_slot.replace(
        std::rc::Rc::new(fixture.dispatcher),
        crate::shell::quick_settings::actions::QsActionAvailability::all(),
    );
    assert!(job.result().unwrap().is_err());
    assert!(!job.finished());
    job.apply(&chrome);
    let doc = automerge::AutoCommit::new();
    host.update(&doc, &automerge::ROOT);
    assert!(
        chrome.borrow().quick_settings.radio_change_pending,
        "caller retirement cannot clear a still-running native operation"
    );
    assert!(
        host.qs_action_slot.take_radio_completion().is_none(),
        "read-back must not happen before the old hardware operation finishes"
    );
    assert!(!chrome.borrow().quick_settings.wifi);
    fixture.release.send(Ok(())).unwrap();
    wake_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(job.finished());
    assert!(
        job.result().unwrap().is_err(),
        "late success cannot overwrite retirement"
    );
    assert!(!chrome.borrow().quick_settings.radio_change_pending);
    assert!(
        !chrome.borrow().quick_settings.wifi,
        "retired work cannot claim the replacement's state"
    );
    assert_eq!(chrome.borrow().notifications().len(), 1);
    assert!(host.qs_action_slot.take_radio_completion().is_none());
}

#[test]
fn successful_radio_reading_refreshes_the_recovered_public_action_availability() {
    use crate::shell::quick_settings::radio::fixture::Fixture;
    use crate::shell::quick_settings::refresh::{FastReading, QsReading};
    use crate::shell::search::{SearchContext, SearchProvider};
    let fixture = Fixture::new();
    let chrome = crate::shell::chrome_state::ChromeState::new(
        crate::shell::state::WorkspaceTree::bootstrap_demo(),
    );
    let mut host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    host.qs_action_slot.replace(
        std::rc::Rc::new(fixture.dispatcher),
        crate::shell::quick_settings::actions::QsActionAvailability::default(),
    );
    let provider =
        crate::shell::search::providers::actions::ActionsProvider::with_shared_qs_actions(
            host.qs_action_slot.clone(),
        );
    let doc = automerge::AutoCommit::new();
    let available = || {
        let rows = provider
            .query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "quick-settings.",
                wanted: &[crate::shell::search::ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
            .into_iter()
            .filter(|row| {
                matches!(
                    row.id.as_str(),
                    "quick-settings.wifi.set-enabled" | "quick-settings.airplane-mode.set-enabled"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            2,
            "both declared radio actions must be observable"
        );
        rows.into_iter().all(|row| {
            matches!(
                row.contract.unwrap().availability,
                tilo_actions::Availability::Available
            )
        })
    };
    assert!(
        !available(),
        "startup has not observed a working radio backend"
    );
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    host.set_quick_settings_wake(Arc::new(move || {
        let _ = wake_tx.send(());
    }));
    host.qs_refresh.kick_fast_with(|| {
        QsReading::Fast(Box::new(FastReading {
            radios: Some(os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            }),
            ..FastReading::empty()
        }))
    });
    wake_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(chrome.borrow().quick_settings.radios.wifi_available);
    assert!(
        available(),
        "actual recovered radio metadata must refresh both public actions"
    );
}
