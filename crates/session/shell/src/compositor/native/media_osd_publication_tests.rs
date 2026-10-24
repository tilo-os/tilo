use super::sync_quick_settings_from_osd;
use crate::shell::chrome_state::QuickSettingsState;
use crate::shell::osd::Osd;

#[test]
fn successful_level_osds_update_the_open_quick_settings_sliders() {
    let mut quick_settings = QuickSettingsState::default();

    sync_quick_settings_from_osd(&mut quick_settings, Osd::volume(0.73, false));
    assert!((quick_settings.volume_level() - 0.73).abs() < f32::EPSILON);

    sync_quick_settings_from_osd(&mut quick_settings, Osd::brightness(0.28));
    assert!((quick_settings.brightness - 0.28).abs() < f32::EPSILON);
}

#[test]
fn the_wireless_key_keeps_the_quick_settings_toggle_in_step() {
    let mut quick_settings = QuickSettingsState::default();
    sync_quick_settings_from_osd(&mut quick_settings, Osd::airplane_mode(true));
    assert!(quick_settings.radios.airplane_mode);
    sync_quick_settings_from_osd(&mut quick_settings, Osd::airplane_mode(false));
    assert!(!quick_settings.radios.airplane_mode);
}
