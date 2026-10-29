//! The `accessibility` section as the record Settings edits by path. Its
//! keys, defaults and ranges are the shell's, from one definition.
use settings_store::dictation::DictationLanguage;
use settings_store::magnifier::{MAX_ZOOM, MagnifierStep, MagnifierView, OFF};
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
#[tilo_resource::resource(
    kind = "settings.accessibility",
    label = "Accessibility",
    version = 2,
    migrate = crate::leaves_moved_out
)]
impl AccessibilitySettings {
    /// Turn the magnifier on at the zoom last in use, or off; or magnify a
    /// level more or less. The desktop's magnifier shortcuts run this.
    #[action(id = "step-magnifier", keywords = "zoom, magnify, magnifier")]
    pub fn step(
        &mut self,
        #[param("`toggle`, `zoom-in` or `zoom-out`")] step: String,
    ) -> Result<(), tilo_resource::Refusal> {
        let step = MagnifierStep::from_id(&step).ok_or_else(|| {
            tilo_resource::Refusal::new(format!(
                "`{step}` is no magnifier step: `toggle`, `zoom-in` or `zoom-out`"
            ))
        })?;
        self.step_magnifier(step);
        Ok(())
    }

    /// Turn the screen reader on or off. The field is changed only here:
    /// the palette and agents find it by the words people use for it.
    #[action(
        id = "set-screen-reader",
        keywords = "narrator, voiceover, accessibility, read aloud"
    )]
    pub fn set_screen_reader(&mut self, on: bool) -> Result<(), tilo_resource::Refusal> {
        self.screen_reader = on;
        Ok(())
    }
}

/// This device's accessibility choices.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::Model,
)]
#[model(defaults)]
pub struct AccessibilityDevicePrefs {
    /// The screen reader, text size, contrast, language and keyboard layout
    /// the person turns on apply to this device's login screens too.
    #[serde(default = "apply_to_sign_in")]
    pub apply_to_sign_in: bool,
}

fn apply_to_sign_in() -> bool {
    settings_store::sign_in_comforts::APPLY_TO_SIGN_IN.default_value()
}

impl Default for AccessibilityDevicePrefs {
    fn default() -> Self {
        Self {
            apply_to_sign_in: apply_to_sign_in(),
        }
    }
}

impl ui::storage::SettingsSchema for AccessibilityDevicePrefs {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::accessibility::DEVICE_SECTION);
}

#[tilo_resource::resource(
    kind = "settings.accessibility-device",
    label = "Accessibility on this device"
)]
impl AccessibilityDevicePrefs {}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// Stepping turns the magnifier on at the zoom last in use, and the
    /// zoom it remembers is part of the section, so undoing a step puts both
    /// back.
    #[test]
    fn the_magnifier_steps_back_to_the_zoom_last_in_use() {
        let mut access = Replica::<AccessibilitySettings>::new();
        let mut step = |step: &str| {
            access.apply(AccessibilitySettings::STEP.with(step.to_owned()))?;
            Ok::<_, ui::tilo_actions::DoorError>(access.state().unwrap().magnifier_zoom)
        };
        assert_eq!(step("toggle").unwrap(), 2);
        assert_eq!(step("zoom-in").unwrap(), 3);
        assert_eq!(step("toggle").unwrap(), OFF);
        assert_eq!(step("toggle").unwrap(), 3);
        assert!(step("zoom").is_err());
    }
}
