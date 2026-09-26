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
