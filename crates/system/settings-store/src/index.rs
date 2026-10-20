//! Where every setting lives: Settings' pages in sidebar order and the
//! settings people look for by name, each on its page, with the words people
//! use for them in every language Tilo speaks.
//!
//! This is the one list behind every way of finding a setting. Settings'
//! sidebar filter, its own search entries and the desktop palette (which
//! finds them while Settings is closed) all read it, so a page or a setting
//! is named the same everywhere and found by the same words.
//!
//! The copy is Fluent ([`CATALOGUES`]), resolved by whoever shows it in
//! their own language. A setting opens through its link,
//! `settings://<page>#<setting>` ([`Setting::link`]): Settings opens the
//! page, scrolls to the setting's control and puts the keyboard on it.

/// One page of Settings, by the slug its links and its sidebar row use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub slug: &'static str,
}

/// One setting, findable by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    /// Stable name, as links and search hits address the setting.
    pub id: &'static str,
    /// The slug of the page it is on.
    pub page: &'static str,
    /// The widget id of the control that changes it. One `*` stands for any
    /// run of characters (one display's id among several); `|` separates the
    /// ids of the controls the page may show instead (one layout's chooser
    /// or the list of several).
    pub control: &'static str,
}

impl Page {
    /// The catalogue key of the page's name.
    #[must_use]
    pub fn title_key(self) -> String {
        format!("settings-index-page-{}", key(self.slug))
    }

    /// The catalogue key of the line that says what the page holds.
    #[must_use]
    pub fn subtitle_key(self) -> String {
        format!("settings-index-page-{}-subtitle", key(self.slug))
    }

    /// The catalogue key of the other words people use for the page.
    #[must_use]
    pub fn words_key(self) -> String {
        format!("settings-index-page-{}-words", key(self.slug))
    }

    /// The link that opens the page.
    #[must_use]
    pub fn link(self) -> String {
        format!("settings://{}", self.slug)
    }

    /// The settings on this page, in index order.
    pub fn settings(self) -> impl Iterator<Item = &'static Setting> {
        SETTINGS
            .iter()
            .filter(move |setting| setting.page == self.slug)
    }
}

impl Setting {
    /// The catalogue key of the setting's name.
    #[must_use]
    pub fn name_key(self) -> String {
        format!("settings-index-setting-{}", key(self.id))
    }

    /// The catalogue key of the other words people use for the setting.
    #[must_use]
    pub fn words_key(self) -> String {
        format!("settings-index-setting-{}-words", key(self.id))
    }

    /// The link that opens the setting's page at the setting.
    #[must_use]
    pub fn link(self) -> String {
        format!("settings://{}#{}", self.page, self.id)
    }

    /// Whether `widget_id` is the id of a control that changes this setting.
    #[must_use]
    pub fn is_control(self, widget_id: &str) -> bool {
        self.control
            .split('|')
            .any(|pattern| glob_matches(pattern, widget_id))
    }
}

/// A catalogue key for `id`: words joined by hyphens.
fn key(id: &str) -> String {
    id.replace(['_', '.'], "-")
}

/// Whether `text` matches `pattern`, where one `*` stands for any
/// non-empty run.
fn glob_matches(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((prefix, suffix)) => text
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|middle| !middle.is_empty()),
    }
}

/// The page with `slug`.
#[must_use]
pub fn page(slug: &str) -> Option<Page> {
    PAGES.iter().copied().find(|page| page.slug == slug)
}

/// The setting named `id`.
#[must_use]
pub fn setting(id: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|setting| setting.id == id)
}

/// The index's copy, by locale tag: page names and summaries, setting names,
/// and the comma-separated words each is also found by.
pub const CATALOGUES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

/// Every message of every catalogue, by key: each language's value, in
/// [`CATALOGUES`] order. Parsed once; search asks for them per keystroke.
fn messages() -> &'static std::collections::HashMap<&'static str, Vec<&'static str>> {
    static MESSAGES: std::sync::OnceLock<
        std::collections::HashMap<&'static str, Vec<&'static str>>,
    > = std::sync::OnceLock::new();
    MESSAGES.get_or_init(|| {
        let mut messages: std::collections::HashMap<&'static str, Vec<&'static str>> =
            std::collections::HashMap::new();
        for (_, catalogue) in CATALOGUES {
            for line in catalogue.lines() {
                if line.starts_with(char::is_whitespace) || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once(" = ") {
                    messages.entry(key.trim()).or_default().push(value.trim());
                }
            }
        }
        messages
    })
}

/// What the words message `key` lists in every language, lowercased: a
/// German desktop still finds "wifi" typed, and an English one "wlan".
#[must_use]
pub fn words_in_every_locale(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    for value in messages().get(key).into_iter().flatten() {
        for word in value.split(',') {
            let word = word.trim().to_lowercase();
            if !word.is_empty() && !words.contains(&word) {
                words.push(word);
            }
        }
    }
    words
}

/// What message `key` says in every language, lowercased: a page's or a
/// setting's name, which is found in any language ("zeitzone" on an English
/// desktop).
#[must_use]
pub fn names_in_every_locale(key: &str) -> Vec<String> {
    let mut names = Vec::new();
    for value in messages().get(key).into_iter().flatten() {
        let name = value.to_lowercase();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every page, in sidebar order.
pub const PAGES: &[Page] = &[
    Page { slug: "network" },
    Page { slug: "bluetooth" },
    Page {
        slug: "devices_sync",
    },
    Page { slug: "appearance" },
    Page {
        slug: "accessibility",
    },
    Page {
        slug: "notifications",
    },
    Page {
        slug: "default-apps",
    },
    Page { slug: "spelling" },
    Page {
        slug: "region_formats",
    },
    Page { slug: "time_date" },
    Page { slug: "displays" },
    Page { slug: "sound" },
    Page { slug: "speakers" },
    Page { slug: "keyboard" },
    Page { slug: "pointing" },
    Page {
        slug: "keyboard_shortcuts",
    },
    Page { slug: "printers" },
    Page { slug: "power" },
    Page { slug: "users" },
    Page {
        slug: "external_accounts",
    },
    Page {
        slug: "agent_models",
    },
    Page {
        slug: "permissions",
    },
    Page { slug: "storage" },
    Page { slug: "updates" },
    Page { slug: "about" },
];

const fn s(id: &'static str, page: &'static str, control: &'static str) -> Setting {
    Setting { id, page, control }
}

/// The settings people look for by name, page by page.
pub const SETTINGS: &[Setting] = &[
    // Network
    s("network.wifi", "network", "network.wifi"),
    s("network.wired", "network", "network.wired"),
    s("network.airplane", "network", "network.airplane"),
    s("network.hotspot", "network", "network.hotspot"),
    s("network.mobile", "network", "network.mobile"),
    // Bluetooth
    s("bluetooth.enabled", "bluetooth", "bluetooth.toggle"),
    // Displays
    s("display.brightness", "displays", "display.brightness"),
    s(
        "display.auto_brightness",
        "displays",
        "display.auto_brightness",
    ),
    s("display.night_light", "displays", "display.night_light"),
    s(
        "display.night_light_warmth",
        "displays",
        "display.night_light_temperature",
    ),
    s("display.scale", "displays", "display.output.*.scale"),
    s("display.resolution", "displays", "display.output.*.mode"),
    s("display.rotation", "displays", "display.output.*.rotation"),
    s("display.hdr", "displays", "display.output.*.hdr"),
    s("display.vrr", "displays", "display.output.*.vrr"),
    s("display.mirroring", "displays", "display.output.*.mirror"),
    s(
        "display.colour_profile",
        "displays",
        "display.output.*.color_profile",
    ),
    // Sound
    s("sound.volume", "sound", "audio.volume"),
    s("sound.output_device", "sound", "audio.output.*"),
    s("sound.input_device", "sound", "audio.input.*"),
    s("sound.input_level", "sound", "audio.input_volume"),
    s("sound.mute", "sound", "audio.mute"),
    s("sound.balance", "sound", "audio.balance"),
    s("sound.mono", "sound", "audio.mono"),
    s("sound.echo", "sound", "audio.echo_cancellation"),
    s("sound.system_sounds", "sound", "audio.system_sounds"),
    // Keyboard
    s(
        "keyboard.layout",
        "keyboard",
        "input.source.replace|input.source.add_picker",
    ),
    s("keyboard.key_repeat", "keyboard", "input.key_repeat"),
    s("keyboard.compose", "keyboard", "input.compose_key"),
    s("keyboard.caps_lock", "keyboard", "input.caps_lock"),
    // Mouse & touchpad
    s("pointing.touchpad", "pointing", "input.touchpad_enabled"),
    s(
        "pointing.touchpad_speed",
        "pointing",
        "input.touchpad_pointer_speed",
    ),
    s("pointing.tap_to_click", "pointing", "input.tap_click"),
    s(
        "pointing.natural_scrolling",
        "pointing",
        "input.touchpad_natural_scroll|input.mouse_natural_scroll",
    ),
    s(
        "pointing.disable_while_typing",
        "pointing",
        "input.disable_while_typing",
    ),
    s(
        "pointing.secondary_click",
        "pointing",
        "input.touchpad_click_method",
    ),
    s(
        "pointing.scroll_method",
        "pointing",
        "input.touchpad_scroll_method",
    ),
    s("pointing.gestures", "pointing", "input.touchpad_gestures"),
    s(
        "pointing.disable_with_mouse",
        "pointing",
        "input.touchpad_disabled_on_external_mouse",
    ),
    s(
        "pointing.mouse_speed",
        "pointing",
        "input.mouse_pointer_speed",
    ),
    s("pointing.acceleration", "pointing", "input.accel_profile"),
    s("pointing.left_handed", "pointing", "input.left_handed"),
    s("pointing.scroll_speed", "pointing", "input.scroll_speed"),
    s(
        "pointing.middle_click",
        "pointing",
        "input.middle_emulation",
    ),
    // Keyboard shortcuts
    s("shortcuts.vim", "keyboard_shortcuts", "shortcuts.vim_mode"),
    // Power
    s("power.profile", "power", "power.profile"),
    s("power.charge_limit", "power", "power.charge_limit"),
    s("power.auto_suspend", "power", "power.auto_suspend"),
    s("power.suspend_after", "power", "power.suspend"),
    s("power.blank", "power", "power.blank"),
    s("power.lock_after", "power", "power.lock"),
    s("power.lid", "power", "power.lid"),
    s("power.power_key", "power", "power.power_key"),
    s("power.critical", "power", "power.critical"),
    // Appearance
    s("appearance.style", "appearance", "appearance.theme"),
    s("appearance.schedule", "appearance", "appearance.schedule"),
    s(
        "appearance.wallpaper",
        "appearance",
        "appearance.wallpaper.*",
    ),
    s("appearance.text_size", "appearance", "appearance.text_size"),
    s("appearance.ui_scale", "appearance", "appearance.ui_scale"),
    s(
        "appearance.high_contrast",
        "appearance",
        "appearance.high_contrast",
    ),
    s(
        "appearance.reduce_motion",
        "appearance",
        "appearance.reduce_motion",
    ),
    s(
        "appearance.hide_top_bar",
        "appearance",
        "appearance.hide_top_bar",
    ),
    // Accessibility
    s("a11y.screen_reader", "accessibility", "a11y.screen_reader"),
    s(
        "a11y.on_screen_keyboard",
        "accessibility",
        "a11y.on_screen_keyboard",
    ),
    s("a11y.large_cursor", "accessibility", "a11y.large_cursor"),
    s("a11y.cursor_size", "accessibility", "a11y.cursor_size"),
    s("a11y.magnifier", "accessibility", "a11y.magnifier"),
    s("a11y.colour_filter", "accessibility", "a11y.color_filter"),
    s("a11y.sticky_keys", "accessibility", "a11y.sticky_keys"),
    s("a11y.slow_keys", "accessibility", "a11y.slow_keys"),
    s("a11y.bounce_keys", "accessibility", "a11y.bounce_keys"),
    s("a11y.mouse_keys", "accessibility", "a11y.mouse_keys"),
    s("a11y.visual_alerts", "accessibility", "a11y.visual_alerts"),
    s(
        "a11y.switch_control",
        "accessibility",
        "a11y.switch_control",
    ),
    s(
        "a11y.dictation_language",
        "accessibility",
        "a11y.dictation_language",
    ),
    // Notifications
    s("notifications.dnd", "notifications", "notifications.dnd"),
    s(
        "notifications.banners",
        "notifications",
        "notifications.banners",
    ),
    s(
        "notifications.previews",
        "notifications",
        "notifications.previews",
    ),
    s(
        "notifications.lock_screen",
        "notifications",
        "notifications.lock_screen",
    ),
    s(
        "notifications.sounds",
        "notifications",
        "notifications.sounds",
    ),
    s(
        "notifications.badges",
        "notifications",
        "notifications.badges",
    ),
    s(
        "notifications.quiet_hours",
        "notifications",
        "notifications.focus.enabled",
    ),
    // Spelling & dictionary
    s("spelling.check", "spelling", "text.check_spelling"),
    s("spelling.autocorrect", "spelling", "text.auto_correct"),
    s("spelling.language", "spelling", "text.spelling_language"),
    s("spelling.dictionary", "spelling", "dictionary.word"),
    // Region & formats
    s("region.language", "region_formats", "region.language"),
    s("region.currency", "region_formats", "region.currency"),
    s("region.date", "region_formats", "region.date"),
    s("region.time", "region_formats", "region.time"),
    s("region.first_day", "region_formats", "region.first_day"),
    s("region.measurement", "region_formats", "region.measurement"),
    s("region.numbers", "region_formats", "region.number_style"),
    // Time & date
    s("time.automatic", "time_date", "time.auto"),
    s("time.zone", "time_date", "time.timezone"),
    // Storage
    s("storage.cache", "storage", "storage.cache-size"),
    s("storage.clean", "storage", "storage.clean"),
    // Updates
    s("updates.automatic", "updates", "updates.auto"),
    s("updates.metered", "updates", "updates.metered"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    fn messages(catalogue: &str) -> BTreeSet<&str> {
        catalogue
            .lines()
            .filter(|line| !line.starts_with('#') && !line.starts_with(char::is_whitespace))
            .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            .collect()
    }

    /// Every page and setting is named, summarised and worded in every
    /// language, and the catalogues hold nothing else.
    #[test]
    fn every_page_and_setting_is_named_in_every_language() {
        let mut wanted = BTreeSet::new();
        for page in PAGES {
            wanted.insert(page.title_key());
            wanted.insert(page.subtitle_key());
            wanted.insert(page.words_key());
        }
        for setting in SETTINGS {
            wanted.insert(setting.name_key());
            wanted.insert(setting.words_key());
        }
        for (locale, catalogue) in CATALOGUES {
            let have: BTreeSet<String> =
                messages(catalogue).into_iter().map(str::to_owned).collect();
            let missing: Vec<_> = wanted.difference(&have).collect();
            let extra: Vec<_> = have.difference(&wanted).collect();
            assert!(missing.is_empty(), "{locale} lacks {missing:?}");
            assert!(extra.is_empty(), "{locale} holds unused {extra:?}");
        }
    }

    #[test]
    fn ids_are_unique_and_every_setting_is_on_a_listed_page() {
        let mut pages = HashSet::new();
        for page in PAGES {
            assert!(pages.insert(page.slug), "page {} twice", page.slug);
        }
        let mut ids = HashSet::new();
        for setting in SETTINGS {
            assert!(ids.insert(setting.id), "setting {} twice", setting.id);
            assert!(
                pages.contains(setting.page),
                "{} is on unknown page {}",
                setting.id,
                setting.page
            );
            assert!(
                !setting.control.is_empty(),
                "{} names no control",
                setting.id
            );
        }
    }

    #[test]
    fn a_setting_links_to_its_page_and_knows_its_controls() {
        let scale = setting("display.scale").expect("indexed");
        assert_eq!(scale.link(), "settings://displays#display.scale");
        assert!(scale.is_control("display.output.eDP-1.scale"));
        assert!(!scale.is_control("display.output.eDP-1.scale_milli"));
        assert!(!scale.is_control("display.brightness"));
        let layout = setting("keyboard.layout").expect("indexed");
        assert!(layout.is_control("input.source.replace"));
        assert!(layout.is_control("input.source.add_picker"));
        assert!(!layout.is_control("input.source"));
        assert_eq!(
            page("keyboard").map(Page::link).as_deref(),
            Some("settings://keyboard")
        );
    }

    #[test]
    fn names_come_from_every_language() {
        let zone = setting("time.zone").expect("indexed");
        assert_eq!(
            names_in_every_locale(&zone.name_key()),
            vec!["time zone".to_owned(), "zeitzone".to_owned()]
        );
    }

    #[test]
    fn words_come_from_every_language() {
        let words = words_in_every_locale(&page("network").expect("listed").words_key());
        assert!(words.iter().any(|word| word == "wifi"), "{words:?}");
        assert!(words.iter().any(|word| word == "wlan"), "{words:?}");
    }
}
