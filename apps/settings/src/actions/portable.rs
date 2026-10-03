//! Closed catalogue of **portable** preference keys Settings accepts for
//! collaborative replay.
//!
//! Each entry is a `(section, key, type)` row plus the presentation metadata
//! needed to publish the same Ctrl+K / resource-tree action the on-screen
//! control already uses. Hardware and service effects stay outside this table
//! — they use typed local-operation protocols in the parent module.

use automerge::{AutoCommit, ROOT};
use tilo_actions::ActionSpec;
use ui::doc::{read_bool, read_str_or_text};

use crate::categories::Category;
use crate::panes;
use crate::prefs::{AccessibilityPrefs, SystemPrefs};

use super::{bool_flip_spec, bool_set_spec, choice_prompt_spec};

/// Scalar type [`SettingsAction`] may write for a portable key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortablePrefType {
    Bool,
    Text,
}

/// How the preference is exposed on its section resource (if at all).
#[derive(Clone, Copy)]
enum PortablePresentation {
    /// Palette flip: label is "Turn on/off {noun}".
    BoolFlip {
        noun: &'static str,
        default: fn(&SystemPrefs) -> bool,
    },
    /// Value-carrying boolean for retained switches.
    BoolSet { label: &'static str },
    /// Choice prompt writing a text scalar.
    Choice {
        label: &'static str,
        placeholder: &'static str,
        options: &'static [&'static str],
        default: fn(&SystemPrefs) -> String,
    },
    /// Accepted by validation only — the pane writes it through another door,
    /// or it is not yet a top-level section action.
    ValidateOnly { ty: PortablePrefType },
}

/// One portable preference: stable key, value type, owning section, and the
/// Ctrl+K presentation used when `action_id` is set.
#[derive(Clone, Copy)]
pub struct PortablePreference {
    pub section: Category,
    /// Stable action id on the section resource. `None` means validate-only.
    pub action_id: Option<&'static str>,
    pub key: &'static str,
    presentation: PortablePresentation,
    /// When set, the action is published only if this returns true. Validation
    /// still accepts the key so a delayed envelope from an earlier session is
    /// not refused solely because a local service disappeared.
    available: Option<fn() -> bool>,
}

impl PortablePreference {
    pub const fn pref_type(self) -> PortablePrefType {
        match self.presentation {
            PortablePresentation::BoolFlip { .. } | PortablePresentation::BoolSet { .. } => {
                PortablePrefType::Bool
            }
            PortablePresentation::Choice { .. } => PortablePrefType::Text,
            PortablePresentation::ValidateOnly { ty } => ty,
        }
    }

    fn to_spec(self, doc: &AutoCommit, prefs: &SystemPrefs) -> Option<ActionSpec> {
        let action_id = self.action_id?;
        if self.available.is_some_and(|gate| !gate()) {
            return None;
        }
        Some(match self.presentation {
            PortablePresentation::BoolFlip { noun, default } => {
                let on_now = read_bool(doc, &ROOT, self.key).unwrap_or_else(|| default(prefs));
                bool_flip_spec(action_id, self.key, on_now, noun)
            }
            PortablePresentation::BoolSet { label } => bool_set_spec(action_id, label, self.key),
            PortablePresentation::Choice {
                label,
                placeholder,
                options,
                default,
            } => {
                let fallback =
                    read_str_or_text(doc, &ROOT, self.key).unwrap_or_else(|| default(prefs));
                choice_prompt_spec(action_id, label, self.key, placeholder, options, fallback)
            }
            PortablePresentation::ValidateOnly { .. } => return None,
        })
    }
}

const FOCUS_TIME_OPTIONS: &[&str] = &[
    "18:00", "20:00", "22:00", "23:00", "00:00", "06:00", "07:00", "08:00", "09:00",
];
const FOCUS_DAYS_OPTIONS: &[&str] = &["Every day", "Weekdays", "Weekends"];
const TEXT_SIZE_OPTIONS: &[&str] = &["Small", "Default", "Large", "Larger"];
const UI_SCALE_OPTIONS: &[&str] = &["90%", "100%", "110%", "125%", "150%"];
const APPEARANCE_MODE: &[&str] = &panes::appearance::MODE_LABELS;
const APPEARANCE_DARK_THEMES: &[&str] = &panes::appearance::DARK_THEME_NAMES;
const APPEARANCE_LIGHT_THEMES: &[&str] = &panes::appearance::LIGHT_THEME_NAMES;
const APPEARANCE_DENSITY: &[&str] = &panes::appearance::DENSITY_LABELS;
const APPEARANCE_COLOUR: &[&str] = &[
    "Red", "Orange", "Amber", "Olive", "Green", "Teal", "Cyan", "Sky", "Blue", "Violet", "Magenta",
    "Pink",
];
const APPEARANCE_SCHEDULE: &[&str] = &panes::appearance::SCHEDULE_LABELS;
const APPEARANCE_TIMES: &[&str] = &panes::appearance::TIMES;
const APPEARANCE_WALLPAPER_SCOPE: &[&str] = &panes::appearance::WALLPAPER_SCOPE_LABELS;
const CURSOR_SIZE_OPTIONS: &[&str] = &["Default", "Large", "Larger", "Largest"];
const COLOR_FILTER_OPTIONS: &[&str] = &[
    "Off",
    "Greyscale",
    "Protanopia",
    "Deuteranopia",
    "Tritanopia",
];
const MAGNIFIER_ZOOM_OPTIONS: &[&str] = &settings_store::magnifier::ZOOM_LABELS;
const MAGNIFIER_VIEW_OPTIONS: &[&str] = &["Lens", "Full screen"];
const BOUNCE_DELAY_OPTIONS: &[&str] = &["100 ms", "200 ms", "300 ms", "500 ms", "800 ms"];
const SWITCH_KEYS_OPTIONS: &[&str] = &["Tab / Enter", "Space / Enter"];
const SWITCH_SCAN_OPTIONS: &[&str] = &["Manual", "1 second", "2 seconds", "3 seconds"];
const READER_RATE_OPTIONS: &[&str] = &["Slow", "Default", "Fast", "Very fast"];
const READER_VOICE_OPTIONS: &[&str] = &["Default", "Male", "Female"];
const CURRENCY_OPTIONS: &[&str] = &[
    "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR",
];
const DATE_OPTIONS: &[&str] = &[
    "ISO (2023-03-15)",
    "US (03/15/2023)",
    "European (15/03/2023)",
    "Long (15 Mar 2023)",
];
const TIME_OPTIONS: &[&str] = &["24-hour (14:05)", "12-hour (2:05 PM)"];
const MEASUREMENT_OPTIONS: &[&str] = &["Metric (°C, km, kg)", "Imperial (°F, mi, lb)"];
const NUMBER_OPTIONS: &[&str] = &["1,234,567.89", "1.234.567,89"];
const FIRST_DAY_OPTIONS: &[&str] = &["Monday", "Sunday", "Saturday"];

fn focus_start_default(_: &SystemPrefs) -> String {
    "22:00".into()
}
fn focus_end_default(_: &SystemPrefs) -> String {
    "07:00".into()
}
fn focus_days_default(_: &SystemPrefs) -> String {
    "Every day".into()
}
fn mode_default(d: &SystemPrefs) -> String {
    panes::appearance::mode_label(&d.appearance.mode)
}
fn dark_theme_default(d: &SystemPrefs) -> String {
    d.appearance.theme_name(&d.appearance.dark_theme)
}
fn light_theme_default(d: &SystemPrefs) -> String {
    d.appearance.theme_name(&d.appearance.light_theme)
}
fn density_default(d: &SystemPrefs) -> String {
    panes::appearance::density_label(d.appearance.density)
}
fn colour_default(d: &SystemPrefs) -> String {
    d.identity_colour()
        .map_or("", |hue| hue.label())
        .to_string()
}
fn text_size_default(d: &SystemPrefs) -> String {
    panes::appearance::text_size_label(d.appearance.text_scale)
}
fn ui_scale_default(_: &SystemPrefs) -> String {
    "100%".into()
}
fn schedule_default(d: &SystemPrefs) -> String {
    panes::appearance::schedule_label(&d.appearance.schedule)
}
fn sunrise_default(d: &SystemPrefs) -> String {
    d.appearance.sunrise.clone()
}
fn sunset_default(d: &SystemPrefs) -> String {
    d.appearance.sunset.clone()
}
fn wallpaper_scope_default(d: &SystemPrefs) -> String {
    panes::appearance::scope_label(&d.appearance.wallpaper_scope)
}
fn a11y_text_size_default(d: &SystemPrefs) -> String {
    panes::appearance::text_size_label(d.appearance.text_scale)
}
fn cursor_size_default(d: &SystemPrefs) -> String {
    AccessibilityPrefs::cursor_size_label(d.accessibility.cursor_size).into()
}
fn color_filter_default(d: &SystemPrefs) -> String {
    d.accessibility.color_filter.label().into()
}
fn on_screen_keyboard_default(d: &SystemPrefs) -> String {
    d.accessibility.on_screen_keyboard.label().into()
}
fn magnifier_default(d: &SystemPrefs) -> String {
    settings_store::magnifier::zoom_label(d.accessibility.magnifier_zoom).into()
}
fn magnifier_view_default(d: &SystemPrefs) -> String {
    d.accessibility.magnifier_view.label().into()
}
fn bounce_delay_default(d: &SystemPrefs) -> String {
    AccessibilityPrefs::bounce_delay_label(d.accessibility.bounce_keys_delay_ms).into()
}
fn switch_keys_default(d: &SystemPrefs) -> String {
    d.accessibility.switch_key_pair.label().into()
}
fn switch_scan_default(d: &SystemPrefs) -> String {
    match d.accessibility.switch_scan_ms {
        1000 => "1 second",
        2000 => "2 seconds",
        3000 => "3 seconds",
        _ => "Manual",
    }
    .into()
}
fn reader_rate_default(d: &SystemPrefs) -> String {
    match d.accessibility.screen_reader_rate {
        0..=75 => "Slow",
        76..=115 => "Default",
        116..=160 => "Fast",
        _ => "Very fast",
    }
    .into()
}
fn reader_voice_default(d: &SystemPrefs) -> String {
    d.accessibility.screen_reader_voice.label().into()
}
fn currency_default(_: &SystemPrefs) -> String {
    "USD".into()
}
fn date_default(_: &SystemPrefs) -> String {
    "ISO".into()
}
fn time_format_default(_: &SystemPrefs) -> String {
    "24-hour (14:05)".into()
}
fn measurement_default(_: &SystemPrefs) -> String {
    "Metric (°C, km, kg)".into()
}
fn number_default(_: &SystemPrefs) -> String {
    "1,234,567.89".into()
}
fn first_day_default(_: &SystemPrefs) -> String {
    "Monday".into()
}
fn dnd_default(d: &SystemPrefs) -> bool {
    d.notifications.do_not_disturb
}
fn banners_default(d: &SystemPrefs) -> bool {
    d.notifications.show_banners
}
fn previews_default(d: &SystemPrefs) -> bool {
    d.notifications.show_previews
}
fn sounds_default(d: &SystemPrefs) -> bool {
    d.notifications.play_sounds
}
fn lock_screen_default(d: &SystemPrefs) -> bool {
    d.notifications.show_on_lock_screen
}
fn badges_default(d: &SystemPrefs) -> bool {
    d.notifications.show_badges
}
fn focus_enabled_default(d: &SystemPrefs) -> bool {
    d.notifications.focus.enabled
}
fn high_contrast_default(d: &SystemPrefs) -> bool {
    d.appearance.high_contrast
}
fn reduce_transparency_default(d: &SystemPrefs) -> bool {
    d.appearance.reduce_transparency
}
fn reduce_motion_default(d: &SystemPrefs) -> bool {
    d.appearance.reduce_motion
}
fn text_assist_default(_: &SystemPrefs) -> bool {
    true
}
fn vim_mode_default(_: &SystemPrefs) -> bool {
    false
}

/// Every static portable preference Settings validates for replay, and the
/// subset published as section-level Ctrl+K actions.
pub const PORTABLE_PREFERENCES: &[PortablePreference] = &[
    // --- Notifications -------------------------------------------------------
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("dnd"),
        key: panes::notifications::DND_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "Do Not Disturb",
            default: dnd_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("banners"),
        key: panes::notifications::BANNERS_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "notification banners",
            default: banners_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("previews"),
        key: panes::notifications::PREVIEWS_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "message previews",
            default: previews_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("sounds"),
        key: panes::notifications::SOUNDS_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "notification sounds",
            default: sounds_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("lock_screen"),
        key: panes::notifications::LOCK_SCREEN_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "lock-screen notifications",
            default: lock_screen_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("badges"),
        key: panes::notifications::BADGES_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "notification badges",
            default: badges_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("focus-enabled"),
        key: panes::notifications::FOCUS_ENABLED_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "scheduled quiet hours",
            default: focus_enabled_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("focus-start"),
        key: panes::notifications::FOCUS_START_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set focus start time",
            placeholder: "Choose a focus schedule value",
            options: FOCUS_TIME_OPTIONS,
            default: focus_start_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("focus-end"),
        key: panes::notifications::FOCUS_END_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set focus end time",
            placeholder: "Choose a focus schedule value",
            options: FOCUS_TIME_OPTIONS,
            default: focus_end_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Notifications,
        action_id: Some("focus-days"),
        key: panes::notifications::FOCUS_DAYS_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set focus days",
            placeholder: "Choose a focus schedule value",
            options: FOCUS_DAYS_OPTIONS,
            default: focus_days_default,
        },
        available: None,
    },
    // --- Appearance ----------------------------------------------------------
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("mode"),
        key: panes::appearance::MODE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set appearance",
            placeholder: "Light, Dark, Automatic",
            options: APPEARANCE_MODE,
            default: mode_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("dark_theme"),
        key: panes::appearance::DARK_THEME_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set dark theme",
            placeholder: "Dark, Dark cool, Dark graphite, Dark soft",
            options: APPEARANCE_DARK_THEMES,
            default: dark_theme_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("light_theme"),
        key: panes::appearance::LIGHT_THEME_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set light theme",
            placeholder: "Light",
            options: APPEARANCE_LIGHT_THEMES,
            default: light_theme_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("density"),
        key: panes::appearance::DENSITY_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set density",
            placeholder: "Compact, Regular, Roomy",
            options: APPEARANCE_DENSITY,
            default: density_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("colour"),
        key: panes::appearance::COLOUR_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set your colour",
            placeholder: "One of the twelve presence colours",
            options: APPEARANCE_COLOUR,
            default: colour_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("reduce_transparency"),
        key: panes::appearance::REDUCE_TRANSPARENCY_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "reduced transparency",
            default: reduce_transparency_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("high_contrast"),
        key: panes::appearance::HIGH_CONTRAST_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "high contrast",
            default: high_contrast_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("reduce_motion"),
        key: panes::appearance::REDUCE_MOTION_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "reduced motion",
            default: reduce_motion_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("text_size"),
        key: panes::appearance::TEXT_SIZE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set text size",
            placeholder: "Small, Default, Large, Larger",
            options: TEXT_SIZE_OPTIONS,
            default: text_size_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("ui_scale"),
        key: panes::appearance::UI_SCALE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set UI scale",
            placeholder: "90%, 100%, 110%, 125%, 150%",
            options: UI_SCALE_OPTIONS,
            default: ui_scale_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("schedule"),
        key: panes::appearance::SCHEDULE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set day and night schedule",
            placeholder: "Sunset to sunrise, Custom times",
            options: APPEARANCE_SCHEDULE,
            default: schedule_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("sunrise"),
        key: panes::appearance::SUNRISE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set when the day starts",
            placeholder: "HH:MM, in half hours",
            options: APPEARANCE_TIMES,
            default: sunrise_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("sunset"),
        key: panes::appearance::SUNSET_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set when the night starts",
            placeholder: "HH:MM, in half hours",
            options: APPEARANCE_TIMES,
            default: sunset_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: Some("wallpaper_scope"),
        key: panes::appearance::WALLPAPER_SCOPE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set which workspaces a new wallpaper covers",
            placeholder: "This workspace, Every workspace",
            options: APPEARANCE_WALLPAPER_SCOPE,
            default: wallpaper_scope_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Appearance,
        action_id: None,
        key: panes::appearance::WALLPAPER_TARGET_KEY,
        presentation: PortablePresentation::ValidateOnly {
            ty: PortablePrefType::Text,
        },
        available: None,
    },
    // --- Accessibility -------------------------------------------------------
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("high-contrast"),
        key: panes::accessibility::HIGH_CONTRAST_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set high contrast",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("reduce-motion"),
        key: panes::accessibility::REDUCE_MOTION_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set reduced motion",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("large-cursor"),
        key: panes::accessibility::LARGE_CURSOR_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set large cursor",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("sticky-keys"),
        key: panes::accessibility::STICKY_KEYS_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set sticky keys",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("slow-keys"),
        key: panes::accessibility::SLOW_KEYS_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set slow keys",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("bounce-keys"),
        key: panes::accessibility::BOUNCE_KEYS_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set bounce keys",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("bounce-keys-alert"),
        key: panes::accessibility::BOUNCE_ALERT_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set bounce keys alert",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("mouse-keys"),
        key: panes::accessibility::MOUSE_KEYS_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set mouse keys",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("visual-alerts"),
        key: panes::accessibility::VISUAL_ALERTS_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set visual alerts",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("pointer-crosshair"),
        key: panes::accessibility::CURSOR_CROSSHAIR_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set pointer crosshair",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("thick-text-caret"),
        key: panes::accessibility::THICK_CARET_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set thicker text cursor",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("switch-control"),
        key: panes::accessibility::SWITCH_CONTROL_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set switch control",
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("on-screen-keyboard"),
        key: panes::accessibility::OSK_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set on-screen keyboard",
            placeholder: "Automatic, Always, Never",
            options: crate::prefs::OnScreenKeyboard::OPTIONS,
            default: on_screen_keyboard_default,
        },
        available: Some(panes::accessibility::osk_service_available),
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("screen-reader"),
        key: panes::accessibility::SCREEN_READER_KEY,
        presentation: PortablePresentation::BoolSet {
            label: "Set screen reader",
        },
        available: Some(panes::accessibility::screen_reader_available),
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("text-size"),
        key: panes::accessibility::TEXT_SIZE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set text size",
            placeholder: "Small, Default, Large, Larger",
            options: TEXT_SIZE_OPTIONS,
            default: a11y_text_size_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("cursor-size"),
        key: panes::accessibility::CURSOR_SIZE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set cursor size",
            placeholder: "Default, Large, Larger, Largest",
            options: CURSOR_SIZE_OPTIONS,
            default: cursor_size_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("colour-filter"),
        key: panes::accessibility::COLOR_FILTER_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set colour filter",
            placeholder: "Off, Greyscale, Protanopia, Deuteranopia, Tritanopia",
            options: COLOR_FILTER_OPTIONS,
            default: color_filter_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("magnifier"),
        key: panes::accessibility::MAGNIFIER_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set magnifier zoom",
            placeholder: "Off, 2×, 3×, 4×, 5×, 6×",
            options: MAGNIFIER_ZOOM_OPTIONS,
            default: magnifier_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("magnifier-view"),
        key: panes::accessibility::MAGNIFIER_VIEW_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set magnifier view",
            placeholder: "Lens, Full screen",
            options: MAGNIFIER_VIEW_OPTIONS,
            default: magnifier_view_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("bounce-delay"),
        key: panes::accessibility::BOUNCE_DELAY_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set bounce keys delay",
            placeholder: "100 ms, 200 ms, 300 ms, 500 ms, 800 ms",
            options: BOUNCE_DELAY_OPTIONS,
            default: bounce_delay_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("switch-keys"),
        key: panes::accessibility::SWITCH_KEYS_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set switch keys",
            placeholder: "Tab / Enter, Space / Enter",
            options: SWITCH_KEYS_OPTIONS,
            default: switch_keys_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("switch-scan"),
        key: panes::accessibility::SWITCH_SCAN_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set automatic switch scan",
            placeholder: "Manual, 1 second, 2 seconds, 3 seconds",
            options: SWITCH_SCAN_OPTIONS,
            default: switch_scan_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("speech-rate"),
        key: panes::accessibility::READER_RATE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set screen-reader speech rate",
            placeholder: "Slow, Default, Fast, Very fast",
            options: READER_RATE_OPTIONS,
            default: reader_rate_default,
        },
        available: Some(panes::accessibility::screen_reader_available),
    },
    PortablePreference {
        section: Category::Accessibility,
        action_id: Some("speech-voice"),
        key: panes::accessibility::READER_VOICE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set screen-reader voice",
            placeholder: "Default, Male, Female",
            options: READER_VOICE_OPTIONS,
            default: reader_voice_default,
        },
        available: Some(panes::accessibility::screen_reader_available),
    },
    // --- Region & Formats ----------------------------------------------------
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("currency"),
        key: panes::region::CURRENCY_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set currency",
            placeholder: "Set currency",
            options: CURRENCY_OPTIONS,
            default: currency_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("date"),
        key: panes::region::DATE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set date format",
            placeholder: "Set date format",
            options: DATE_OPTIONS,
            default: date_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("time"),
        key: panes::region::TIME_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set time format",
            placeholder: "Set time format",
            options: TIME_OPTIONS,
            default: time_format_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("measurement"),
        key: panes::region::MEASUREMENT_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set measurement units",
            placeholder: "Set measurement units",
            options: MEASUREMENT_OPTIONS,
            default: measurement_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("number"),
        key: panes::region::NUMBER_STYLE_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set number format",
            placeholder: "Set number format",
            options: NUMBER_OPTIONS,
            default: number_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::RegionFormats,
        action_id: Some("first-day"),
        key: panes::region::FIRST_DAY_KEY,
        presentation: PortablePresentation::Choice {
            label: "Set first day of week",
            placeholder: "Set first day of week",
            options: FIRST_DAY_OPTIONS,
            default: first_day_default,
        },
        available: None,
    },
    // --- Mouse & Keyboard (portable text assistance only) --------------------
    PortablePreference {
        section: Category::MouseKeyboard,
        action_id: Some("spell-checking"),
        key: panes::mouse_keyboard::CHECK_SPELLING_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "spell checking while typing",
            default: text_assist_default,
        },
        available: None,
    },
    PortablePreference {
        section: Category::MouseKeyboard,
        action_id: Some("auto-correct"),
        key: panes::mouse_keyboard::AUTO_CORRECT_KEY,
        presentation: PortablePresentation::BoolFlip {
            noun: "automatic spelling correction",
            default: text_assist_default,
        },
        available: None,
    },
    // --- Keyboard Shortcuts --------------------------------------------------
    PortablePreference {
        section: Category::KeyboardShortcuts,
        action_id: Some("vim-mode"),
        key: "shortcuts.vim_mode",
        presentation: PortablePresentation::BoolFlip {
            noun: "Vim-style desktop navigation",
            default: vim_mode_default,
        },
        available: None,
    },
];

/// Attach every published portable preference for `section` to `resource`.
pub(super) fn attach_portable_preferences(
    mut resource: tilo_actions::Resource,
    section: Category,
    doc: &AutoCommit,
    prefs: &SystemPrefs,
) -> tilo_actions::Resource {
    for pref in PORTABLE_PREFERENCES
        .iter()
        .copied()
        .filter(|pref| pref.section == section)
    {
        if let Some(spec) = pref.to_spec(doc, prefs) {
            resource = resource.with_action(spec);
        }
    }
    resource
}

/// Look up a static portable key's accepted value type.
pub(super) fn portable_pref_type(key: &str) -> Option<PortablePrefType> {
    PORTABLE_PREFERENCES
        .iter()
        .find(|pref| pref.key == key)
        .map(|pref| pref.pref_type())
}

/// Published action ids for a section (tests / coverage assertions).
#[cfg(test)]
pub(super) fn published_action_ids(section: Category) -> Vec<&'static str> {
    PORTABLE_PREFERENCES
        .iter()
        .filter(|pref| pref.section == section)
        .filter(|pref| pref.available.is_none_or(|gate| gate()))
        .filter_map(|pref| pref.action_id)
        .collect()
}
