use super::*;

#[test]
fn blocked_quick_settings_read_does_not_turn_waiting_into_animation() {
    let mut host = ShellOverlayHost::new(Box::new(ui::widget::Container::new()));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    host.qs_refresh.kick_fast_with(move || {
        let _ = release_rx.recv();
        crate::shell::quick_settings::refresh::QsReading::Fast(Box::new(
            crate::shell::quick_settings::refresh::FastReading {
                brightness: None,
                displays: Vec::new(),
                volume: None,
                wifi: None,
                bluetooth: None,
                bluetooth_available: false,
                power: None,
                power_profiles: None,
                wired: None,
                battery: crate::shell::quick_settings::actions::BatteryInfo::from_root(
                    std::path::Path::new("/nonexistent"),
                ),
                radios: None,
                privacy: crate::shell::quick_settings::actions::PrivacyStatus::default(),
                vpns: None,
            },
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
            crate::shell::quick_settings::refresh::FastReading {
                brightness: None,
                displays: Vec::new(),
                volume: None,
                wifi: None,
                bluetooth: None,
                bluetooth_available: false,
                power: None,
                power_profiles: None,
                wired: None,
                battery: crate::shell::quick_settings::actions::BatteryInfo::from_root(
                    std::path::Path::new("/nonexistent"),
                ),
                radios: None,
                privacy: crate::shell::quick_settings::actions::PrivacyStatus::default(),
                vpns: None,
            },
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
