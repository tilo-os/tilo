//! What the session reads of the person's settings when it starts: their
//! shortcut table, their input devices' preferences (pointer, touchpad,
//! key repeat), how touchpad gestures read, the layout they type with, the
//! accessibility assists the compositor carries out, when the screen blanks,
//! locks and the machine sleeps, what the lid, the power key and a failing
//! battery do, the pointer's size, and the comforts the login screens start
//! with.

use settings_store::hardware::{
    AccelProfile, KeyboardInputSource, RichInputConfig, ShortcutConfig,
};
use settings_store::sign_in_comforts::SignInComforts;
use tilo_accessibility_settings::{AccessibilitySettings, OnScreenKeyboard};
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
    /// The layouts the person types with, the one they type in first, and the options they chose
    /// for them.
    pub keyboard: KeyboardLayout,
    /// The input sources of [`Self::keyboard`] in the person's order: XKB layout ids (`de`,
    /// `us:intl` ) and input-method engines (`ime/rime`).
    pub keyboard_layouts: Vec<String>,
    /// The source of [`Self::keyboard_layouts`] that types now.
    pub keyboard_active: String,
    /// When the displays dim, blank, and the session locks and the machine sleeps with no input:
    /// [`Self::power`]'s timeouts, as the compositor runs them.
    pub idle: IdleConfig,
    /// The machine's power policy, resolved for this machine: what the lid,
    /// the power key and a failing battery do, and whether a sleep locks.
    pub power: tilo_power_mgmt::PowerPolicy,
    /// The person's accessibility settings: the assists, the pointer's size and shape, the colour
    /// filter, the magnifier and visual alerts.
    pub a11y: A11y,
    /// When the on-screen keyboard runs: never, over a field the person touches, or always.
    pub on_screen_keyboard: OnScreenKeyboard,
    /// The comforts the login screens start with: the device's copy, which the session keeps equal
    /// to these (`docs/security/sign-in.md` §3.4).
    pub comforts: SignInComforts,
    /// Whether the person's changes to those comforts apply to the login screens too, or only to
    /// their own session.
    pub apply_comforts_to_sign_in: bool,
    /// How warm the night light makes the displays, and whether the switch or the day/night
    /// schedule shows it.
    pub night_light: crate::night_light::Preference,
    /// Whether what the person changes in the session (the night light) is kept in their settings:
    /// for settings loaded from them.
    pub kept: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::from_settings(&RichInputConfig::default(), ShortcutConfig::default())
    }
}

/// What a settings change wakes, and what ends that worker: the reading is made off the thread
/// that wrote the setting, which is often the loop.
pub struct Following {
    _watch: settings_store::watch::SettingsWatch,
    news: std::sync::Arc<News>,
}

impl Drop for Following {
    fn drop(&mut self) {
        self.news.close();
    }
}

#[derive(Default)]
struct News {
    state: std::sync::Mutex<NewsState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct NewsState {
    /// A change arrived and was not read yet.
    pending: bool,
    /// The follow was dropped: the worker reads nothing more.
    closed: bool,
}

impl News {
    fn rung(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending = true;
        drop(state);
        self.changed.notify_one();
    }

    /// Whether a change waits to be read, waiting for one; `false` once it is closed.
    fn next(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if state.closed {
                return false;
            }
            if state.pending {
                state.pending = false;
                return true;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        drop(state);
        self.changed.notify_one();
    }
}

impl SessionConfig {
    /// The person's settings as stored.
    #[must_use]
    pub fn load() -> Self {
        // The sources of the keyboard typing now, as switching the source reads them.
        let input = settings_store::keyboard_sources::current(&RichInputConfig::load());
        let accessibility = tilo_accessibility_settings::AccessibilitySettings::load();
        let power = loaded_power();
        Self {
            night_light: crate::night_light::Preference::load(),
            kept: true,
            idle: idle_config(&power),
            power,
            a11y: crate::a11y_settings::a11y(&accessibility),
            on_screen_keyboard: accessibility.on_screen_keyboard,
            comforts: sign_in_comforts(&accessibility, &input, true),
            apply_comforts_to_sign_in: settings_store::sign_in_comforts::APPLY_TO_SIGN_IN.get(),
            ..Self::from_settings(&input, ShortcutConfig::load())
        }
    }

    /// Load the person's settings now and again whenever they change,
    /// handing each reading to `apply`: the loader's one watch, so whoever
    /// follows a section follows every change through it. What it returns keeps the notifications
    /// coming; dropping it stops them.
    ///
    /// The change only wakes a worker, which reads and applies: the reading
    /// opens the keyboard files and resolves the power policy, and the
    /// change is often rung by the session's loop writing a setting.
    ///
    /// # Errors
    /// Fails when there are no settings to follow, or the worker cannot start.
    pub fn follow(apply: impl Fn(Self) + Send + Sync + 'static) -> std::io::Result<Following> {
        let news = std::sync::Arc::new(News::default());
        let worker = std::sync::Arc::clone(&news);
        std::thread::Builder::new()
            .name("session-config".into())
            .spawn(move || {
                while worker.next() {
                    apply(Self::load());
                }
            })?;
        let rung = std::sync::Arc::clone(&news);
        match settings_store::watch::watch_settings(move || rung.rung()) {
            Ok(watch) => Ok(Following {
                _watch: watch,
                news,
            }),
            Err(error) => {
                news.close();
                Err(error)
            }
        }
    }

    /// The settings `input` and `shortcuts` hold, as the session applies them.
    #[must_use]
    pub fn from_settings(input: &RichInputConfig, shortcuts: ShortcutConfig) -> Self {
        let gestures = input.touchpad_gestures.then(|| GestureConfig {
            natural_scroll: input.touchpad_natural_scroll,
            ..GestureConfig::default()
        });
        let assists = tilo_accessibility_settings::AccessibilitySettings::load();
        let sources = Sources::from_settings(input);
        let power = tilo_power_mgmt::PowerPolicy::default();
        let preferences = input_config(input, &shortcuts);
        Self {
            shortcuts,
            input: preferences,
            gestures,
            keyboard: sources.compiled(),
            keyboard_layouts: sources.ids.clone(),
            keyboard_active: sources.active,
            // Settings not loaded from a person (a test, a screenshot) have no one to walk away:
            // nothing blanks, locks or sleeps on a wall clock. Locking on request works.
            idle: IdleConfig::never(),
            power,
            a11y: crate::a11y_settings::a11y(&assists),
            on_screen_keyboard: assists.on_screen_keyboard,
            comforts: sign_in_comforts(&assists, input, false),
            apply_comforts_to_sign_in: true,
            night_light: crate::night_light::Preference::default(),
            kept: false,
        }
    }
}

/// The power policy this machine runs, as the person's settings resolve for its duty and its power
/// source.
fn loaded_power() -> tilo_power_mgmt::PowerPolicy {
    tilo_power_mgmt::load_policy_for_profile(None, tilo_power_mgmt::sysfs_on_battery())
}

/// The login screens' comforts from the person's assists and input, and from the theme's text scale
/// and contrast where `theme` is read (it is not when these are the compositor's default, which
/// names none).
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

/// When the displays dim, blank and the session locks and sleeps with no input, as the person chose
/// it: `None` is their "Never".
fn idle_config(power: &tilo_power_mgmt::PowerPolicy) -> IdleConfig {
    IdleConfig::from_env()
        .with_blank(power.idle_blank)
        .with_lock(power.idle_lock)
        .with_suspend(power.idle_suspend)
}

/// The person's keyboard input sources, in their order, and the one they type in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sources {
    /// Source ids in the person's order (`de`, `us:intl` , `ime/rime` ).
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

    /// The sources of `config` .
    #[must_use]
    pub fn from_config(config: &SessionConfig) -> Self {
        Self {
            ids: config.keyboard_layouts.clone(),
            active: config.keyboard_active.clone(),
            model: config.keyboard.model.clone(),
            options: config.keyboard.options.clone(),
        }
    }

    /// The keymap for the source that types, then the others in the person's order: the first group
    /// is what a key produces now, and a group-toggle option still reaches the rest.
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
}

/// The compositor's input preferences from the stored ones: `shortcuts` says which chord each of
/// a tablet's remapped buttons presses now.
fn input_config(input: &RichInputConfig, shortcuts: &ShortcutConfig) -> InputConfig {
    use settings_store::hardware::{TouchpadClickMethod as Click, TouchpadScrollMethod as Scroll};
    InputConfig {
        tablets: input
            .tablets
            .iter()
            .map(|(device, config)| (device.clone(), tablet(config, shortcuts)))
            .collect(),
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

/// One tablet's stored settings as the compositor shapes its events by them: a button mapped to a
/// desktop shortcut presses the chord that shortcut is on now, and each ring and strip keeps what
/// it does in every mode its group has.
fn tablet(
    stored: &settings_store::hardware::TabletConfig,
    shortcuts: &ShortcutConfig,
) -> tilo_compositor::TabletConfig {
    use settings_store::hardware::{
        EraserAction as StoredEraser, StylusButtonAction as StoredStylus,
    };
    use tilo_compositor::backend::drm::tablet::{
        EraserAction as Eraser, StylusButtonAction as Stylus,
    };

    // A shortcut the person's table does not list is on the chord the desktop binds it by.
    let defaults = ShortcutConfig::default();
    let chord = |action: settings_store::hardware::ShortcutAction| {
        shortcuts
            .bindings
            .get(&action)
            .or_else(|| defaults.bindings.get(&action))
            .cloned()
    };
    let stylus = |action: StoredStylus| match action {
        StoredStylus::Shortcut(shortcut) => {
            chord(shortcut).map_or(Stylus::Default, |chord| Stylus::Shortcut(pressed(&chord)))
        }
        StoredStylus::Default => Stylus::Default,
        StoredStylus::Left => Stylus::Left,
        StoredStylus::Middle => Stylus::Middle,
        StoredStylus::Right => Stylus::Right,
        StoredStylus::Back => Stylus::Back,
        StoredStylus::Forward => Stylus::Forward,
        StoredStylus::Disabled => Stylus::Disabled,
    };
    tilo_compositor::TabletConfig {
        output: stored.output.clone(),
        keep_aspect: stored.keep_aspect,
        left_handed: stored.left_handed,
        pressure_curve: stored.pressure_curve,
        lower_button: stylus(stored.lower_button),
        upper_button: stylus(stored.upper_button),
        third_button: stylus(stored.third_button),
        eraser: match stored.eraser {
            StoredEraser::Native => Eraser::Native,
            StoredEraser::Left => Eraser::Left,
            StoredEraser::Middle => Eraser::Middle,
            StoredEraser::Right => Eraser::Right,
            StoredEraser::Back => Eraser::Back,
            StoredEraser::Forward => Eraser::Forward,
            StoredEraser::Shortcut(shortcut) => {
                chord(shortcut).map_or(Eraser::Native, |chord| Eraser::Shortcut(pressed(&chord)))
            }
        },
        pad_buttons: stored
            .pad_buttons
            .iter()
            .filter_map(|(button, shortcut)| {
                chord(*shortcut).map(|chord| (*button, pressed(&chord)))
            })
            .collect(),
        pad_rings: stored.pad_rings.clone(),
        pad_strips: stored.pad_strips.clone(),
    }
}

/// A stored chord as the compositor presses it.
fn pressed(
    stored: &settings_store::hardware::KeyChord,
) -> tilo_compositor::backend::drm::tablet::KeyChord {
    tilo_compositor::backend::drm::tablet::KeyChord {
        modifiers: stored.modifiers.clone(),
        key: stored.key.clone(),
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

    #[test]
    fn a_tablets_buttons_press_their_shortcuts_and_its_dials_keep_every_mode() {
        use settings_store::hardware::{
            PadDialAction, ShortcutAction, StylusButtonAction, TabletConfig,
        };
        use tilo_compositor::backend::drm::tablet::StylusButtonAction as Pressed;

        let mut stored = TabletConfig::default();
        stored.upper_button = StylusButtonAction::Shortcut(ShortcutAction::Search);
        stored.pad_buttons.insert(2, ShortcutAction::Lock);
        stored.pad_rings.insert(
            0,
            vec![
                PadDialAction::Zoom,
                PadDialAction::Nothing,
                PadDialAction::Volume,
            ],
        );
        stored
            .pad_strips
            .insert(0, vec![PadDialAction::Nothing, PadDialAction::Brightness]);
        let mut shortcuts = ShortcutConfig::default();
        shortcuts.bindings.insert(
            ShortcutAction::Lock,
            settings_store::hardware::KeyChord {
                modifiers: std::collections::BTreeSet::from(["Ctrl".to_owned()]),
                key: "j".into(),
            },
        );

        let config = SessionConfig::from_settings(
            &RichInputConfig {
                tablets: [("056a:0374:Wacom".to_owned(), stored)].into(),
                ..RichInputConfig::default()
            },
            shortcuts,
        );
        let tablet = config
            .input
            .tablets
            .get("056a:0374:Wacom")
            .expect("the tablet");
        assert_eq!(
            tablet.upper_button,
            Pressed::Shortcut(tilo_compositor::backend::drm::tablet::KeyChord {
                modifiers: std::collections::BTreeSet::from(["Super".to_owned()]),
                key: "space".into(),
            }),
            "a pen button presses the chord its shortcut is on"
        );
        assert_eq!(
            tablet.pad_buttons.get(&2).map(|chord| chord.key.as_str()),
            Some("j"),
            "a pad button presses the person's own chord for its shortcut"
        );
        assert_eq!(
            tablet.dial_action(settings_store::hardware::PadDial::Ring(0), 2),
            PadDialAction::Volume,
            "mode 2 of the first ring keeps what the pane set for it"
        );
        assert_eq!(
            tablet.dial_action(settings_store::hardware::PadDial::Ring(0), 7),
            PadDialAction::Scroll,
            "a mode the ring does not have scrolls"
        );
        assert_eq!(
            tablet.dial_action(settings_store::hardware::PadDial::Strip(0), 1),
            PadDialAction::Brightness
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
