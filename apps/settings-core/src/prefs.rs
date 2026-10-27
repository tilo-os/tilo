//! The system preferences Settings' panes edit: the section models are
//! [`settings_sections::prefs`]'s, which the session keeps as records too;
//! what is here words them for the panes.

pub use settings_sections::prefs::*;

/// The catalogue key naming the mute duration `english`.
fn mute_ftl(english: &str) -> &'static str {
    match english {
        "For 1 hour" => "notif-mute-1h",
        "For 8 hours" => "notif-mute-8h",
        "For 24 hours" => "notif-mute-24h",
        _ => "notif-mute-off",
    }
}

/// The mute duration `english` in the person's language.
pub fn mute_shown(english: &str) -> String {
    crate::i18n::t(mute_ftl(english))
}

#[cfg(test)]
/// Config-directory isolation for the in-crate unit tests that construct the
/// app or drive a pane's real persist path. Running the suite must never read
/// or mutate the developer's (or a Tilo user's) live settings, accounts or
/// identity, and two tests must never see each other's files.
///
/// The guard points `XDG_CONFIG_HOME` at a fresh directory through
/// [`tilo_test_env::ScopedEnv`], which serialises on the process-wide
/// environment lock and restores the prior value on drop, also while
/// unwinding from a failed assertion. `HOME` and the cache directory stay put
/// so the text stack keeps its font cache instead of rebuilding it per test.
pub mod test_env {
    use crate::settings_app::SettingsApp;

    /// Holds the redirected environment for a test's duration.
    pub struct ConfigGuard {
        // Field order is drop order: restore the environment before the
        // directory it points at disappears.
        _env: tilo_test_env::ScopedEnv,
        _dir: tempfile::TempDir,
    }

    impl ConfigGuard {
        pub fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("tilo-settings-config-")
                .tempdir()
                .expect("create test config dir");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", dir.path());
            Self {
                _env: env,
                _dir: dir,
            }
        }
    }

    /// A private config root handed to the app explicitly, leaving the
    /// environment alone: for tests that construct and lay out the app but
    /// persist nothing through the per-user settings file, so they run in
    /// parallel with each other and with [`ConfigGuard`] tests.
    pub struct ScratchConfig {
        dir: tempfile::TempDir,
    }

    impl ScratchConfig {
        pub fn new() -> Self {
            Self {
                dir: tempfile::Builder::new()
                    .prefix("tilo-settings-config-")
                    .tempdir()
                    .expect("create test config dir"),
            }
        }

        /// The app over this scratch config root.
        pub fn app(&self) -> SettingsApp {
            SettingsApp::with_config_root(self.dir.path().join("tilo"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::accessibility_settings::AccessibilitySettings;

    use ui::storage::SettingsSchema;

    /// The settings file at `path` after storing `value` as its section, as
    /// the panes and the door store it.
    fn store<T: SettingsSchema>(path: &std::path::Path, value: &T) -> serde_json::Value {
        crate::host::PaneCx::in_file(path.to_owned())
            .settings::<T>()
            .set(value)
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The settings file at `path` with every leaf of `value` written to its
    /// section: what a check enumerating the leaves needs, since a store
    /// writes only what differs from what is read.
    fn store_whole<T: SettingsSchema + serde::Serialize>(
        path: &std::path::Path,
        value: &T,
    ) -> serde_json::Value {
        let values = serde_json::to_value(value)
            .unwrap()
            .as_object()
            .cloned()
            .expect("a section serializes to an object");
        settings_store::update_section_at(path, T::SCOPE.section(), values).unwrap();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The settings file holding `existing` after storing `value` as its
    /// section.
    fn stored<T: SettingsSchema>(existing: &str, value: &T) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, existing).unwrap();
        store(&path, value)
    }

    /// Section `T` as read from a settings file holding `text`.
    fn read<T: SettingsSchema>(text: &str) -> T {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, text).unwrap();
        crate::host::PaneCx::in_file(path).settings::<T>().get()
    }

    /// `value` stored and read back.
    fn round_trip<T: SettingsSchema>(value: &T) -> T {
        read(&stored("{}", value).to_string())
    }

    /// Every default is sensible and every owned leaf has a continuity policy.
    #[test]
    fn prefs_defaults_are_sensible_and_classified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        store_whole(&path, &AudioPrefs::default());
        store_whole(&path, &DisplayPrefs::default());
        store_whole(&path, &InputPrefs::default());
        store_whole(&path, &PowerPrefs::default());
        store_whole(&path, &UpdatePrefs::default());
        store_whole(&path, &AppearancePrefs::default());
        store_whole(&path, &NotificationPrefs::default());
        store_whole(&path, &AccessibilitySettings::default());
        store_whole(&path, &tilo_locale::Locale::default());
        let document = store_whole(&path, &crate::panes::personal_apps::DefaultApps::default());
        let parsed = settings_store::parse(&document.to_string());
        let missing = settings_store::continuity::unclassified_leaves(&parsed);
        assert!(
            missing.is_empty(),
            "new settings must explicitly choose personal, device-local, secret, or derived: \
             {missing:?}"
        );

        assert!(AudioPrefs::default().system_sounds.enabled);
        assert!(!DisplayPrefs::default().night_light);
        let input = InputPrefs::default();
        assert!(input.tap_to_click);
        assert!(input.touchpad_natural_scroll);
        assert!(!input.mouse_natural_scroll);
        assert_eq!(
            input.keyboard_layouts,
            settings_store::keyboard_sources::default_sources()
        );
        assert_eq!(input.keyboard_layout, input.keyboard_layouts[0]);
        let power = PowerPrefs::default();
        assert!(
            !power.auto_suspend,
            "a fresh install does not suspend on idle; the lid still suspends"
        );
        assert_eq!(
            power.auto_suspend,
            settings_store::power::AUTO_SUSPEND.default_value()
        );
        assert_eq!(power.lid_close_action, PowerActionPref::Suspend);
        assert_eq!(power.blank_minutes, 10);
        assert_eq!(power.lock_minutes, 5);
        let updates = UpdatePrefs::default();
        assert_eq!(updates.last_checked, "Never");

        let n = NotificationPrefs::default();
        assert!(!n.do_not_disturb, "notifications on by default");
        assert!(n.show_banners);
        assert!(n.show_previews);
        assert!(n.play_sounds);
        assert!(
            !n.show_on_lock_screen,
            "lock screen is the most exposed surface"
        );
    }

    #[test]
    fn keyboard_filters_and_touchpad_choices_round_trip_through_json() {
        use settings_store::hardware::{TouchpadClickMethod, TouchpadScrollMethod};
        let mut access = AccessibilitySettings::default();
        assert_eq!(access.bounce_keys_delay_ms, 300);
        access.bounce_keys = true;
        access.bounce_keys_delay_ms = 500;
        access.bounce_keys_alert = true;
        access.mouse_keys = true;
        let mut input = InputPrefs {
            touchpad_click_method: TouchpadClickMethod::Clickfinger,
            touchpad_scroll_method: TouchpadScrollMethod::Edge,
            scroll_speed: 2.0,
            ..InputPrefs::default()
        };
        assert_eq!(round_trip(&access), access);
        input = round_trip(&input);
        assert_eq!(
            input.touchpad_click_method,
            TouchpadClickMethod::Clickfinger
        );
        assert_eq!(input.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(input.scroll_speed, 2.0);

        let root = stored("{}", &input);
        let rich = settings_store::hardware::RichInputConfig::from_section(
            root["input"].as_object().unwrap(),
        );
        assert_eq!(rich.touchpad_click_method, TouchpadClickMethod::Clickfinger);
        assert_eq!(rich.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(rich.scroll_speed, 2.0);
        let root = stored("{}", &access);
        assert_eq!(root["accessibility"]["bounce_keys_delay_ms"], 500);
        assert_eq!(root["accessibility"]["mouse_keys"], true);
    }

    #[test]
    fn touchpad_power_choices_round_trip_and_share_the_toggle_keys_spelling() {
        use settings_store::hardware::{
            TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY, TOUCHPAD_ENABLED_KEY,
        };
        let defaults = InputPrefs::default();
        assert!(defaults.touchpad_enabled);
        assert!(!defaults.touchpad_disabled_on_external_mouse);

        let input = InputPrefs {
            touchpad_enabled: false,
            touchpad_disabled_on_external_mouse: true,
            ..InputPrefs::default()
        };
        let back = round_trip(&input);
        assert!(!back.touchpad_enabled);
        assert!(back.touchpad_disabled_on_external_mouse);

        let root = stored("{}", &input);
        assert_eq!(root["input"][TOUCHPAD_ENABLED_KEY], false);
        assert_eq!(root["input"][TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY], true);

        // A key press that switched the touchpad off survives an unrelated
        // Settings save instead of being overwritten with the default.
        let pressed = r#"{"input":{"touchpad_enabled":false}}"#;
        let mut input = read::<InputPrefs>(pressed);
        assert!(!input.touchpad_enabled);
        input.tap_to_click = !input.tap_to_click;
        assert_eq!(
            stored(pressed, &input)["input"][TOUCHPAD_ENABLED_KEY],
            false
        );
    }

    #[test]
    fn prefs_round_trip_through_json() {
        let mut audio = AudioPrefs::default();
        audio.system_sounds.enabled = false;
        audio.system_sounds.theme = Some("ocean".into());
        audio.system_sounds.events.error = false;
        assert_eq!(round_trip(&audio), audio);
        let display = DisplayPrefs {
            night_light: true,
            ..DisplayPrefs::default()
        };
        assert_eq!(round_trip(&display), display);
        let input = InputPrefs {
            pointer_speed: 0.9,
            keyboard_layout: "de:nodeadkeys".into(),
            keyboard_layouts: vec!["us".into(), "de:nodeadkeys".into()],
            ..InputPrefs::default()
        };
        assert_eq!(
            round_trip(&input),
            input,
            "source order and the active source survive"
        );
        let power = PowerPrefs {
            auto_suspend: true,
            blank_minutes: 30,
            lock_minutes: 0,
            ..PowerPrefs::default()
        };
        assert_eq!(round_trip(&power), power);
        let updates = UpdatePrefs {
            auto: false,
            ..UpdatePrefs::default()
        };
        assert_eq!(round_trip(&updates), updates);
        let mut appearance = AppearancePrefs {
            high_contrast: true,
            reduce_motion: true,
            text_scale: 1.25,
            ui_scale: 1.15,
            ..AppearancePrefs::default()
        };
        appearance.choose_theme("Dark soft");
        assert_eq!(round_trip(&appearance), appearance);
        let notifications = NotificationPrefs {
            do_not_disturb: true,
            show_previews: false,
            ..NotificationPrefs::default()
        };
        assert_eq!(round_trip(&notifications), notifications);
        let access = AccessibilitySettings {
            large_cursor: true,
            cursor_size: 48,
            screen_reader: true,
            ..AccessibilitySettings::default()
        };
        assert_eq!(round_trip(&access), access);
    }

    /// Partial, malformed or missing sections load with defaults for
    /// whatever they lack, and never lose the keys that did parse.
    #[test]
    fn partial_and_malformed_documents_load_with_defaults() {
        assert_eq!(read::<InputPrefs>(""), InputPrefs::default());
        assert_eq!(read::<InputPrefs>("garbage {"), InputPrefs::default());
        assert_eq!(
            read::<NotificationPrefs>("{}"),
            NotificationPrefs::default()
        );

        let input = read::<InputPrefs>(r#"{"input":{}}"#);
        assert!(input.touchpad_natural_scroll);
        assert!(!input.mouse_natural_scroll);

        let p = read::<AppearancePrefs>(
            r#"{"appearance":{"mode":"dark","high_contrast":false,"reduce_motion":false,"text_scale":1.15}}"#,
        );
        assert_eq!(p.ui_scale, 1.0, "missing ui_scale defaults to neutral");
        assert_eq!(p.text_scale, 1.15);

        // A garbage `input` section reads as defaults while the valid audio
        // section beside it still parses.
        let document = r#"{"input":"not-an-object","audio":{"system_sounds":{"enabled":false}}}"#;
        assert_eq!(read::<InputPrefs>(document), InputPrefs::default());
        assert!(
            !read::<AudioPrefs>(document).system_sounds.enabled,
            "the valid audio section still parsed"
        );

        // Only some power keys: the rest read as their defaults.
        let p = read::<PowerPrefs>(r#"{"power":{"auto_suspend":false,"blank_minutes":15}}"#);
        assert!(!p.auto_suspend);
        // A section that never chose: idle-suspend stays off, as it does in
        // the compositor that reads the same document.
        let p = read::<PowerPrefs>(r#"{"power":{"blank_minutes":15,"suspend_minutes":15}}"#);
        assert!(!p.auto_suspend);
        assert_eq!(p.blank_minutes, 15);
        assert_eq!(p.blank_minutes, 15);
        assert_eq!(p.lock_minutes, 5);
        assert_eq!(p.suspend_minutes, 15);
        assert_eq!(p.lid_close_action, PowerActionPref::Suspend);
        assert_eq!(p.critical_action, PowerActionPref::Suspend);
    }

    #[test]
    fn appearance_prefs_resolve_to_real_themes() {
        let p = AppearancePrefs::default();
        assert_eq!(p.theme_choice(), "Dark");
        assert_eq!(
            p.base_theme().name,
            ui::Theme::configured_from(None).name,
            "Settings and the desktop agree on the theme nobody chose"
        );
        assert!(!p.high_contrast);
        assert_eq!(p.text_scale, 1.0);

        let prefs = |theme: &str, high_contrast, reduce_motion, text_scale, ui_scale| {
            let mut prefs = AppearancePrefs {
                high_contrast,
                reduce_motion,
                text_scale,
                ui_scale,
                ..AppearancePrefs::default()
            };
            prefs.choose_theme(theme);
            prefs
        };
        // (case, prefs, resolved name, text scale, reduced motion)
        let cases = [
            ("default resolves to dark, unscaled", p, "Dark", 1.0, false),
            (
                "named preset with comforts",
                prefs("Dark cool", false, true, 1.5, 1.0),
                "Dark cool",
                1.5,
                true,
            ),
            // high_contrast wins over the named preset.
            (
                "high contrast overrides the preset",
                prefs("Dark graphite", true, false, 1.0, 1.0),
                "High contrast",
                1.0,
                false,
            ),
        ];
        for (case, prefs, name, text_scale, reduce_motion) in cases {
            let theme = prefs.resolve_theme();
            assert_eq!(theme.name, name, "case {case}");
            assert_eq!(theme.text_scale, text_scale, "case {case}");
            assert_eq!(theme.reduce_motion, reduce_motion, "case {case}");
        }

        for name in AppearancePrefs::THEMES
            .iter()
            .filter(|name| !settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()))
        {
            let t = ui::Theme::named(name).expect("an advertised preset");
            assert_eq!(&t.name, name, "{name} must resolve to its own theme");
        }

        let base = ui::Theme::dark();
        let base_gap = base.tokens.spacing.s3;
        let base_type = base.tokens.typography.size(ui::tokens::TextRole::Body);
        let theme = prefs("Dark", false, false, 1.2, 1.25).resolve_theme();
        assert_eq!(theme.ui_scale, 1.25, "UI scale folds in");
        assert_eq!(theme.text_scale, 1.2, "text scale folds in");
        // Spacing grows by the UI factor only; type by UI × text.
        assert!((theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
        assert!(
            (theme.tokens.typography.size(ui::tokens::TextRole::Body) - base_type * 1.25 * 1.2)
                .abs()
                < 1e-2
        );

        // A pinned distribution emphasis wins over the named preset's emphasis
        // only. The palette stays the one the pane shows.
        let mut pinned = prefs("Light", false, false, 1.0, 1.0);
        pinned.distribution_defaults = Some(ui::theme::DistributionDefaults {
            document: Some(ui::theme::ThemeDocument {
                emphasis: ui::tokens::Emphasis::Tinted { hue: 260.0 },
                ..ui::theme::Preset::Dark.document()
            }),
            policy: ui::theme::DistributionPolicy {
                pins: vec![ui::theme::PIN_EMPHASIS.into()],
            },
            ..ui::theme::DistributionDefaults::default()
        });
        let pinned_theme = pinned.resolve_theme();
        assert_eq!(pinned_theme.name, "Light");
        assert_eq!(
            pinned_theme.tokens.colors.emphasis,
            ui::tokens::Emphasis::Tinted { hue: 260.0 }
        );
    }

    /// Storing one section never clobbers the region store's locale, a theme
    /// studio artifact, the compositor's own keys or another app's section.
    #[test]
    fn storing_a_section_preserves_foreign_keys() {
        let locale = r#""locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}"#;
        let existing = format!(r#"{{"version":1,{locale},"weather":{{"city":"Delft"}}}}"#);
        let notifications = NotificationPrefs {
            do_not_disturb: true,
            ..NotificationPrefs::default()
        };
        let out = stored(&existing, &notifications);
        assert_eq!(out["notifications"]["do_not_disturb"], true);
        assert_eq!(out["locale"]["currency"], "EUR");
        assert_eq!(out["weather"]["city"], "Delft");
        let display = DisplayPrefs {
            night_light: true,
            ..DisplayPrefs::default()
        };
        let out = stored(&existing, &display);
        assert_eq!(out["display"]["night_light"], true);
        assert_eq!(out["locale"]["currency"], "EUR");

        let existing = r#"{"appearance":{"theme":"Dark","studio":{"id":"sunset","theme":{"accent":[251,146,60]}}}}"#;
        let mut appearance = read::<AppearancePrefs>(existing);
        appearance.text_scale = 1.25;
        let out = stored(existing, &appearance);
        assert_eq!(out["appearance"]["studio"]["id"], "sunset");
        assert_eq!(out["appearance"]["studio"]["theme"]["accent"][0], 251);
        assert_eq!(out["appearance"]["text_scale"], 1.25);

        let existing = r#"{"accessibility":{"magnifier_zoom":1,"magnifier_last_zoom":4}}"#;
        let mut access = read::<AccessibilitySettings>(existing);
        access.sticky_keys = true;
        let out = stored(existing, &access);
        assert_eq!(out["accessibility"]["magnifier_last_zoom"], 4);
        assert_eq!(out["accessibility"]["sticky_keys"], true);
    }

    /// The `power` section serialises to exactly the keys and lowercase action
    /// labels the compositor's `tilo_power_mgmt::config` reader parses. This is
    /// the write side of the cross-crate JSON contract (the read side is
    /// pinned in that crate's `settings_json_parses_power_sections_and_falls_back_to_defaults` test); if either drifts
    /// this test — or that one — fails.
    #[test]
    fn power_section_matches_the_compositor_contract() {
        let power = PowerPrefs {
            auto_suspend: true,
            suspend_minutes: 20,
            blank_minutes: 5,
            lock_minutes: 10,
            lock_on_sleep: false,
            lid_close_action: PowerActionPref::Lock,
            power_key_action: PowerActionPref::Ignore,
            low_battery_percent: 18,
            critical_battery_percent: 6,
            critical_action: PowerActionPref::Suspend,
            ..PowerPrefs::default()
        };
        // Stored over values that differ from each of these, so every leaf
        // is written (a store writes only what changes).
        let root = stored(
            r#"{"power":{"auto_suspend":false,"suspend_minutes":30,"blank_minutes":1,"lock_minutes":1,"lock_on_sleep":true,"lid_close_action":"suspend","power_key_action":"suspend","low_battery_percent":30,"critical_battery_percent":2,"critical_action":"ignore"}}"#,
            &power,
        );
        let power = &root["power"];
        assert_eq!(power["auto_suspend"], serde_json::json!(true));
        assert_eq!(power["suspend_minutes"], serde_json::json!(20));
        assert_eq!(power["blank_minutes"], serde_json::json!(5));
        assert_eq!(power["lock_minutes"], serde_json::json!(10));
        assert_eq!(power["lock_on_sleep"], serde_json::json!(false));
        // Actions must be the lowercase labels the reader expects.
        assert_eq!(power["lid_close_action"], serde_json::json!("lock"));
        assert_eq!(power["power_key_action"], serde_json::json!("ignore"));
        assert_eq!(power["critical_action"], serde_json::json!("suspend"));
        assert_eq!(power["low_battery_percent"], serde_json::json!(18));
        assert_eq!(power["critical_battery_percent"], serde_json::json!(6));
    }
}
