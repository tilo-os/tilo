//! The `accessibility` section as the record Settings edits by path. Its
//! keys, defaults and ranges are the shell's, from one definition.
use std::ops::RangeInclusive;

use settings_store::dictation::DictationLanguage;
use settings_store::magnifier::{MAX_ZOOM, MagnifierView, OFF};
use tilo_accessibility_settings::{
    BOUNCE_KEYS_DELAY_RANGE, CURSOR_SIZE_RANGE, ColorFilter, OnScreenKeyboard, SECTION,
    ScreenReaderVoice, SwitchKeyPair,
};
use ui::tilo_resource::{self, Refusal};

tilo_accessibility_settings::accessibility_settings!(
    #[derive(tilo_resource::Model)]
    #[model(check = AccessibilitySettings::valid)]
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

impl AccessibilitySettings {
    /// Refuse an edit that leaves a value outside the range reading would
    /// clamp it to, so what was set is what applies.
    fn valid(&self) -> Result<(), Refusal> {
        fn within<T: PartialOrd + Copy + std::fmt::Display>(
            value: T,
            range: &RangeInclusive<T>,
            what: &str,
        ) -> Result<(), Refusal> {
            if range.contains(&value) {
                Ok(())
            } else {
                Err(Refusal::new(format!(
                    "{what} is between {} and {}",
                    range.start(),
                    range.end()
                )))
            }
        }
        within(self.cursor_size, &CURSOR_SIZE_RANGE, "the cursor size")?;
        within(
            self.bounce_keys_delay_ms,
            &BOUNCE_KEYS_DELAY_RANGE,
            "the bounce keys delay",
        )?;
        within(self.magnifier_zoom, &(OFF..=MAX_ZOOM), "the magnifier zoom")?;
        within(
            self.screen_reader_rate,
            &settings_store::accessibility::SCREEN_READER_RATE_RANGE,
            "the speech rate",
        )
    }
}
