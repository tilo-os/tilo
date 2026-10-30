//! What the session reads of the person's settings when it starts: their
//! shortcut table, their input devices' preferences (pointer, touchpad,
//! key repeat), and how touchpad gestures read.

use settings_store::hardware::{AccelProfile, RichInputConfig, ShortcutConfig};
use tilo_compositor::InputConfig;
use tilo_compositor::backend::drm::input_config::{TouchpadClickMethod, TouchpadScrollMethod};
use tilo_compositor::gestures::GestureConfig;

/// The person's settings as the session applies them.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionConfig {
    /// The desktop's chords: the one table every host resolves keys by.
    pub shortcuts: ShortcutConfig,
    /// Pointer, touchpad, keyboard repeat and tablets.
    pub input: InputConfig,
    /// How the desktop reads three- and four-finger touchpad gestures;
    /// `None` leaves every gesture to the clients.
    pub gestures: Option<GestureConfig>,
    /// Whether the night light warms the displays.
    pub night_light: bool,
    /// Whether what the person changes in the session (the night light) is
    /// kept in their settings: for settings loaded from them.
    pub kept: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::from_settings(&RichInputConfig::default(), ShortcutConfig::default())
    }
}

impl SessionConfig {
    /// The person's settings as stored.
    #[must_use]
    pub fn load() -> Self {
        Self {
            night_light: settings_store::hardware::NIGHT_LIGHT.get(),
            kept: true,
            ..Self::from_settings(&RichInputConfig::load(), ShortcutConfig::load())
        }
    }

    /// The settings `input` and `shortcuts` hold, as the session applies
    /// them.
    #[must_use]
    pub fn from_settings(input: &RichInputConfig, shortcuts: ShortcutConfig) -> Self {
        let gestures = input.touchpad_gestures.then(|| GestureConfig {
            natural_scroll: input.touchpad_natural_scroll,
            ..GestureConfig::default()
        });
        Self {
            shortcuts,
            input: input_config(input),
            gestures,
            night_light: false,
            kept: false,
        }
    }
}

/// The compositor's input preferences from the stored ones.
fn input_config(input: &RichInputConfig) -> InputConfig {
    use settings_store::hardware::{TouchpadClickMethod as Click, TouchpadScrollMethod as Scroll};
    InputConfig {
        touchpad_natural_scroll: input.touchpad_natural_scroll,
        mouse_natural_scroll: input.mouse_natural_scroll,
        tap_to_click: input.tap_to_click,
        pointer_speed: input.pointer_speed,
        touchpad_pointer_speed: input.touchpad_pointer_speed,
        mouse_pointer_speed: input.mouse_pointer_speed,
        key_repeat: input.key_repeat,
        key_repeat_delay_ms: input.key_repeat_delay_ms,
        key_repeat_rate_hz: input.key_repeat_rate_hz,
        tap_drag: input.tap_drag,
        tap_drag_lock: input.tap_drag_lock,
        disable_while_typing: input.disable_while_typing,
        middle_emulation: input.middle_emulation,
        left_handed: input.left_handed,
        flat_acceleration: input.accel_profile == AccelProfile::Flat,
        touchpad_gestures: input.touchpad_gestures,
        touchpad_click_method: match input.touchpad_click_method {
            Click::Areas => TouchpadClickMethod::Areas,
            Click::Clickfinger => TouchpadClickMethod::Clickfinger,
            Click::Default => TouchpadClickMethod::Default,
        },
        touchpad_scroll_method: match input.touchpad_scroll_method {
            Scroll::Edge => TouchpadScrollMethod::Edge,
            Scroll::TwoFinger => TouchpadScrollMethod::TwoFinger,
        },
        scroll_speed: input.scroll_speed,
        ..InputConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stored_preferences_reach_the_compositor_and_the_gestures() {
        let input = RichInputConfig {
            key_repeat_delay_ms: Some(250),
            key_repeat_rate_hz: Some(40),
            touchpad_natural_scroll: false,
            accel_profile: AccelProfile::Flat,
            ..RichInputConfig::default()
        };
        let config = SessionConfig::from_settings(&input, ShortcutConfig::default());
        assert_eq!(
            config.input.key_repeat_params(),
            (Duration::from_millis(250), 40)
        );
        assert!(config.input.flat_acceleration);
        assert_eq!(
            config.gestures.map(|gestures| gestures.natural_scroll),
            Some(false)
        );
        let off = RichInputConfig {
            touchpad_gestures: false,
            ..RichInputConfig::default()
        };
        assert_eq!(
            SessionConfig::from_settings(&off, ShortcutConfig::default()).gestures,
            None
        );
    }
}
