//! The `accessibility` section as the record Settings edits by path. Its
//! keys, defaults and ranges are the shell's, from one definition.
use settings_store::dictation::DictationLanguage;
use settings_store::magnifier::{MAX_ZOOM, MagnifierView, OFF};
use tilo_accessibility_settings::{
    BOUNCE_KEYS_DELAY_RANGE, CURSOR_SIZE_RANGE, ColorFilter, OnScreenKeyboard, SECTION,
    SWITCH_SCAN_RANGE_MS, ScreenReaderVoice, SwitchKeyPair,
};
use ui::tilo_resource;

tilo_accessibility_settings::accessibility_settings!(
    #[derive(tilo_resource::Model)]
    #[model(
        defaults,
        field(cursor_size, range = *CURSOR_SIZE_RANGE.start()..=*CURSOR_SIZE_RANGE.end()),
        field(
            bounce_keys_delay_ms,
            range = *BOUNCE_KEYS_DELAY_RANGE.start()..=*BOUNCE_KEYS_DELAY_RANGE.end()
        ),
        field(magnifier_zoom, range = OFF..=MAX_ZOOM),
        field(switch_scan_ms, range = 0..=*SWITCH_SCAN_RANGE_MS.end()),
        field(
            screen_reader_rate,
            range = *settings_store::accessibility::SCREEN_READER_RATE_RANGE.start()
                ..=*settings_store::accessibility::SCREEN_READER_RATE_RANGE.end()
        ),
    )]
    ;
    #[model(readonly)]
);

/// The person's assists, shared with the compositor and the shell.
impl ui::storage::SettingsSchema for AccessibilitySettings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(SECTION);
    ui::settings_codec!();
}

/// The section as a record the person edits by path.
#[tilo_resource::resource(kind = "settings.accessibility", label = "Accessibility")]
impl AccessibilitySettings {}
