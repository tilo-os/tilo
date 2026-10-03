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
