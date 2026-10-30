//! What the session reads of the person's settings when it starts: their
//! shortcut table, their input devices' preferences (pointer, touchpad,
//! key repeat), how touchpad gestures read, the layout they type with, the
//! accessibility assists the compositor carries out, when the screen blanks,
//! locks and the machine sleeps, what the lid, the power key and a failing
//! battery do, the pointer's size, and the comforts the login screens start
//! with.

use std::time::Duration;

use settings_store::hardware::{
    AccelProfile, KeyboardInputSource, RichInputConfig, ShortcutConfig,
};
use settings_store::sign_in_comforts::SignInComforts;
use tilo_accessibility_settings::AccessibilitySettings;
use tilo_compositor::InputConfig;
use tilo_compositor::a11y::A11y;
use tilo_compositor::backend::drm::input_config::{TouchpadClickMethod, TouchpadScrollMethod};
use tilo_compositor::gestures::GestureConfig;
use tilo_compositor::{IdleConfig, KeyboardLayout};

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
    /// The layouts the person types with, the one they type in first, and
    /// the options they chose for them.
    pub keyboard: KeyboardLayout,
    /// The input sources of [`Self::keyboard`] in the person's order: XKB
    /// layout ids (`de`, `us:intl`) and input-method engines (`ime/rime`).
    pub keyboard_layouts: Vec<String>,
    /// The source of [`Self::keyboard_layouts`] that types now.
    pub keyboard_active: String,
    /// When the displays dim, blank, and the session locks and the machine
    /// sleeps with no input: [`Self::power`]'s timeouts, as the compositor
    /// runs them.
    pub idle: IdleConfig,
    /// The machine's power policy, resolved for this machine: what the lid,
    /// the power key and a failing battery do, and whether a sleep locks.
    pub power: tilo_power_mgmt::PowerPolicy,
    /// The person's accessibility settings: the assists, the pointer's size
    /// and shape, the colour filter, the magnifier and visual alerts.
    pub a11y: A11y,
    /// The comforts the login screens start with: the device's copy, which
    /// the session keeps equal to these (`docs/security/sign-in.md` §3.4).
    pub comforts: SignInComforts,
    /// Whether the person's changes to those comforts apply to the login
    /// screens too, or only to their own session.
    pub apply_comforts_to_sign_in: bool,
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
        let input = RichInputConfig::load();
        let accessibility = tilo_accessibility_settings::AccessibilitySettings::load();
        let power = loaded_power();
        Self {
            night_light: settings_store::hardware::NIGHT_LIGHT.get(),
            kept: true,
            idle: idle_config(&power),
            power,
            a11y: crate::a11y_settings::a11y(&accessibility),
            comforts: sign_in_comforts(&accessibility, &input, true),
            apply_comforts_to_sign_in: settings_store::sign_in_comforts::APPLY_TO_SIGN_IN.get(),
            ..Self::from_settings(&input, ShortcutConfig::load())
        }
    }

    /// Load the person's settings now and again whenever they change,
    /// handing each reading to `apply`: the loader's one watch, so whoever
    /// follows a section follows every change through it. The returned
    /// watch keeps the notifications coming; dropping it stops them.
    ///
    /// # Errors
    /// Fails when there are no settings to follow.
    pub fn follow(
        apply: impl Fn(Self) + Send + Sync + 'static,
    ) -> std::io::Result<settings_store::watch::SettingsWatch> {
        settings_store::watch::watch_settings(move || apply(Self::load()))
    }

    /// The settings `input` and `shortcuts` hold, as the session applies
    /// them.
    #[must_use]
    pub fn from_settings(input: &RichInputConfig, shortcuts: ShortcutConfig) -> Self {
        let gestures = input.touchpad_gestures.then(|| GestureConfig {
            natural_scroll: input.touchpad_natural_scroll,
            ..GestureConfig::default()
        });
        let assists = tilo_accessibility_settings::AccessibilitySettings::load();
        let sources = Sources::from_settings(input);
        let power = tilo_power_mgmt::PowerPolicy::default();
        Self {
            shortcuts,
            input: input_config(input),
            gestures,
            keyboard: sources.compiled(),
            keyboard_layouts: sources.ids.clone(),
            keyboard_active: sources.active,
            idle: idle_config(&power),
            power,
            a11y: crate::a11y_settings::a11y(&assists),
            comforts: sign_in_comforts(&assists, input, false),
            apply_comforts_to_sign_in: true,
            night_light: false,
            kept: false,
        }
    }
}

/// The power policy this machine runs, as the person's settings resolve for
/// its duty and its power source.
fn loaded_power() -> tilo_power_mgmt::PowerPolicy {
    tilo_power_mgmt::load_policy_for_profile(None, tilo_power_mgmt::sysfs_on_battery())
}

/// The login screens' comforts from the person's assists and input, and from
/// the theme's text scale and contrast where `theme` is read (it is not when
/// these are the compositor's default, which names none).
fn sign_in_comforts(
    accessibility: &AccessibilitySettings,
    input: &RichInputConfig,
    theme: bool,
) -> SignInComforts {
    let defaults = SignInComforts::default();
    let comforted = if theme {
        let theme = ui::theme::Theme::configured_comforts();
        SignInComforts {
            language: settings_store::appearance::LANGUAGE.get(),
            text_scale: theme.text_scale.unwrap_or(defaults.text_scale),
            high_contrast: theme.high_contrast.unwrap_or(defaults.high_contrast),
            ..defaults
        }
    } else {
        defaults
    };
    SignInComforts {
        screen_reader: accessibility.screen_reader,
        screen_reader_rate: accessibility.screen_reader_rate,
        screen_reader_voice: accessibility.screen_reader_voice,
        ..comforted
    }
    .with_keyboard_source(&input.keyboard_layout)
    .clamped()
}

/// When the displays dim, blank and the session locks and sleeps with no
/// input, as the person chose it: `None` is their "Never".
fn idle_config(power: &tilo_power_mgmt::PowerPolicy) -> IdleConfig {
    IdleConfig::from_env()
        .with_blank(power.idle_blank)
        .with_lock(power.idle_lock)
        .with_suspend(power.idle_suspend)
}

/// The person's keyboard input sources, in their order, and the one they
/// type in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sources {
    /// Source ids in the person's order (`de`, `us:intl`, `ime/rime`).
    pub ids: Vec<String>,
    /// The source of [`Self::ids`] that types now.
    pub active: String,
    /// The physical keyboard model the keymap compiles for.
    pub model: String,
    /// The XKB options their Compose and Caps Lock choices name.
    pub options: String,
}

impl Sources {
    /// The sources `input` names.
    #[must_use]
    pub fn from_settings(input: &RichInputConfig) -> Self {
        Self {
            ids: input.keyboard_layouts.clone(),
            active: input.keyboard_layout.clone(),
            model: input.keyboard_model.clone(),
            options: input.xkb_options().join(","),
        }
    }

    /// The sources of `config`.
    #[must_use]
    pub fn from_config(config: &SessionConfig) -> Self {
        Self {
            ids: config.keyboard_layouts.clone(),
            active: config.keyboard_active.clone(),
            model: config.keyboard.model.clone(),
            options: config.keyboard.options.clone(),
        }
    }

    /// The keymap for the source that types, then the others in the
    /// person's order: the first group is what a key produces now, and a
    /// group-toggle option still reaches the rest.
    #[must_use]
    pub fn compiled(&self) -> KeyboardLayout {
        let mut order: Vec<&str> = Vec::with_capacity(self.ids.len());
        if let Some(active) = self.ids.iter().find(|id| **id == self.active) {
            order.push(active);
        }
        for id in &self.ids {
            if *id != self.active {
                order.push(id);
            }
        }
        let parsed: Vec<KeyboardInputSource> = order
            .into_iter()
            .filter_map(KeyboardInputSource::parse)
            .collect();
        KeyboardLayout {
            layout: parsed
                .iter()
                .map(|source| source.layout.as_str())
                .collect::<Vec<_>>()
                .join(","),
            variant: parsed
                .iter()
                .map(|source| source.variant.as_deref().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(","),
            model: self.model.clone(),
            options: self.options.clone(),
        }
    }

    /// Make the source after the active one active and say which it is;
    /// `None` with a single source: there is nothing else to type in.
    pub fn next(&mut self) -> Option<String> {
        if self.ids.len() < 2 {
            return None;
        }
        let current = self
            .ids
            .iter()
            .position(|id| *id == self.active)
            .unwrap_or(0);
        let next = current.checked_add(1)?.checked_rem(self.ids.len())?;
        self.active.clone_from(self.ids.get(next)?);
        Some(self.active.clone())
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

    #[test]
    fn the_layouts_the_person_chose_are_the_groups_the_keys_compile() {
        let input = RichInputConfig {
            keyboard_layout: "de".into(),
            keyboard_layouts: vec!["de".into(), "us:intl".into()],
            keyboard_model: "pc104".into(),
            caps_lock: Some("escape".into()),
            compose_key: Some("ralt".into()),
            ..RichInputConfig::default()
        };
        let sources = Sources::from_settings(&input);
        let layout = sources.compiled();
        assert_eq!(sources.active, "de");
        assert_eq!(layout.layout, "de,us");
        assert_eq!(layout.variant, ",intl");
        assert_eq!(layout.model, "pc104");
        assert_eq!(layout.options, "caps:escape,compose:ralt");
    }

    #[test]
    fn the_power_settings_are_the_policy_and_the_idle_timers_the_session_runs() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        settings_store::update_section(
            settings_store::power::SECTION,
            serde_json::Map::from_iter([
                ("auto_suspend".to_owned(), serde_json::Value::Bool(true)),
                ("blank_minutes".to_owned(), serde_json::json!(2)),
                ("lock_minutes".to_owned(), serde_json::json!(1)),
                ("suspend_minutes".to_owned(), serde_json::json!(30)),
                ("lock_on_sleep".to_owned(), serde_json::Value::Bool(false)),
                ("lid_close_action".to_owned(), serde_json::json!("lock")),
                ("power_key_action".to_owned(), serde_json::json!("ignore")),
                ("critical_battery_percent".to_owned(), serde_json::json!(7)),
            ]),
        )
        .expect("the power settings are kept");
        let config = SessionConfig::load();
        assert_eq!(
            config.idle,
            IdleConfig::never()
                .with_blank(Some(Duration::from_secs(120)))
                .with_lock(Some(Duration::from_secs(60)))
                .with_suspend(Some(Duration::from_secs(1800))),
            "the idle timers are the person's minutes"
        );
        assert!(!config.power.lock_on_sleep);
        assert_eq!(
            config.power.lid_close_action,
            tilo_power_mgmt::PowerAction::Lock
        );
        assert_eq!(
            config.power.power_key_action,
            tilo_power_mgmt::PowerAction::Ignore
        );
        assert_eq!(config.power.critical_battery_percent, 7);
    }

    #[test]
    fn the_assists_the_person_turned_on_reach_the_compositors_filters() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        settings_store::update_section(
            tilo_accessibility_settings::SECTION,
            serde_json::Map::from_iter([
                ("bounce_keys".to_owned(), serde_json::Value::Bool(true)),
                ("bounce_keys_delay_ms".to_owned(), serde_json::json!(700)),
                ("mouse_keys".to_owned(), serde_json::Value::Bool(true)),
                ("large_cursor".to_owned(), serde_json::Value::Bool(true)),
                ("cursor_size".to_owned(), serde_json::json!(48)),
            ]),
        )
        .expect("the assists are kept");
        let config =
            SessionConfig::from_settings(&RichInputConfig::default(), ShortcutConfig::default());
        assert_eq!(
            config.a11y.assists.bounce_keys,
            Some(Duration::from_millis(700)),
            "a repeated key is dropped for as long as the person asked"
        );
        assert!(
            config.a11y.assists.mouse_keys,
            "the keypad moves the pointer"
        );
        assert_eq!(
            config.a11y.cursor.size, 48,
            "the pointer is the size they chose"
        );
    }
}
