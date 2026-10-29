//! Accessibility pane — the dedicated assistive-technology surface.
//!
//! This panel aggregates the OS accessibility controls the way a
//! control-center's "Universal Access" panel does. It edits two sections of
//! the person's settings:
//!
//! - the **vision comforts** a low-vision user reaches for first — high
//!   contrast, reduced motion, and text size — which live in
//!   [`crate::prefs::AppearancePrefs`] (the single source of truth the whole
//!   system themes against). They are edited here and nowhere else: the
//!   Appearance pane links to this pane rather than repeating them; and
//! - the **pointer, typing, hearing, and screen-reader** assists in
//!   [`AccessibilitySettings`], read by the compositor's input + accessibility
//!   layer.
//!
//! The page is view code over both sections ([`view`]): every control is
//! bound to its field, and the page shows what the sections hold, whoever
//! changed them.

use settings_store::dictation::DictationLanguage;
use settings_store::magnifier;
use tilo_accessibility_settings::{ColorFilter, ScreenReaderVoice};

use std::rc::Rc;

use provider_settings_ui::bound::{Setting, SettingRows as _, record_item};
use ui::framework::ViewCx;
use ui::widget::{Element, Picture};

use crate::accessibility_settings::{AccessibilityDevicePrefs, AccessibilitySettings};
use crate::i18n::{t, t_args};
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::setting::choose;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, error_row, note, page_of, rows, section, section_with, value_row,
};

crate::area::pane_area!(Software);

/// The assistive software the window found installed.
pub(crate) fn software(app: &crate::settings_app::SettingsApp) -> Software {
    app.areas()
        .find::<Software>()
        .copied()
        .unwrap_or(Software { speech: None })
}

fn executable_available(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(name);
            std::fs::metadata(candidate).is_ok_and(|m| m.is_file())
        })
    })
}

pub(crate) fn tts_capability() -> Option<&'static str> {
    if executable_available("speech-dispatcher") {
        Some("Speech Dispatcher")
    } else if executable_available("espeak-ng") {
        Some("eSpeak NG")
    } else {
        None
    }
}

/// The offered text sizes as `(display label, scale factor)`. The factor is
/// what [`AppearancePrefs::text_scale`] persists.
pub const TEXT_SIZES: &[(&str, f64)] = &[
    ("Small", 0.85),
    ("Default", 1.0),
    ("Large", 1.15),
    ("Larger", 1.30),
];

fn text_size_ftl(english: &str) -> &'static str {
    match english {
        "Small" => "settings-appearance-text-small",
        "Large" => "settings-appearance-text-large",
        "Larger" => "settings-appearance-text-larger",
        _ => "settings-appearance-text-default",
    }
}

/// Map a text-scale factor to the closest offered label (defaults to
/// "Default").
pub fn text_size_label(scale: f64) -> String {
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited setting
    // could hold a non-finite scale, and this must degrade to Default
    // rather than panic the pane.
    let english = TEXT_SIZES
        .iter()
        .min_by(|(_, a), (_, b)| (a - scale).abs().total_cmp(&(b - scale).abs()))
        .map_or("Default", |(label, _)| *label);
    t(text_size_ftl(english))
}

/// The automatic scan intervals switch control offers, and their periods.
pub const SWITCH_SCANS: [(&str, u16); 4] = [
    ("Manual", 0),
    ("1 second", 1000),
    ("2 seconds", 2000),
    ("3 seconds", 3000),
];

/// The speech rates the screen reader offers, and their words per minute
/// relative to the engine's default (100).
pub const SPEECH_RATES: [(&str, u16); 4] = [
    ("Slow", 65),
    ("Default", 100),
    ("Fast", 140),
    ("Very fast", 185),
];

fn switch_scan_label(ms: u16) -> &'static str {
    SWITCH_SCANS
        .iter()
        .find(|(_, period)| *period == ms)
        .map_or("Manual", |(label, _)| *label)
}

fn speech_rate_label(rate: u16) -> &'static str {
    match rate {
        0..=75 => "Slow",
        76..=115 => "Default",
        116..=160 => "Fast",
        _ => "Very fast",
    }
}

fn values<T: Copy>(table: &'static [(&'static str, T)]) -> impl Iterator<Item = T> {
    table.iter().map(|(_, value)| *value)
}

/// The assistive software installed on this device, looked up once per
/// window: the speech engine the screen reader would speak through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Software {
    pub speech: Option<&'static str>,
}

impl Software {
    /// What this device has installed.
    pub fn installed() -> Self {
        Self {
            speech: tts_capability(),
        }
    }

    /// What a screenshot and the crate's tests show installed: everything.
    pub fn sample() -> Self {
        Self {
            speech: Some("Speech Dispatcher"),
        }
    }

    /// This device's software where the areas reach it (`devices`), else
    /// the sample.
    pub fn for_device(devices: bool) -> Self {
        if devices {
            Self::installed()
        } else {
            Self::sample()
        }
    }
}

/// The Accessibility page as view code over the person's accessibility
/// section, with the vision comforts of their appearance section and this
/// device's sign-in screens: each control bound to its field, the page
/// following every change of them.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let speech = software(app).speech;
    let access = cx.settings::<AccessibilitySettings>();
    cx.record(&access, move |cx| {
        page_of(vec![
            cx.switch(
                &cx.value(|access: &AccessibilitySettings| access.magnifier_zoom != magnifier::OFF),
                |cx, magnifying| vision_card(cx, *magnifying).into_element(),
            ),
            cx.switch(
                &cx.value(|access: &AccessibilitySettings| access.large_cursor),
                |cx, large| pointer_card(cx, *large).into_element(),
            ),
            cx.switch(
                &cx.value(|access: &AccessibilitySettings| {
                    (access.bounce_keys, access.switch_control)
                }),
                |cx, (bounce, switch)| typing_card(cx, *bounce, *switch).into_element(),
            ),
            reader_card(cx, speech).into_element(),
            hearing_card(cx).into_element(),
            shared_card(cx).into_element(),
        ])
    })
}

/// The row of the assist labelled by `label` and its `-desc` line, its
/// control `id`.
fn assist<'a>(
    cx: &'a ViewCx<AccessibilitySettings>,
    label: &str,
    id: &str,
) -> Setting<'a, AccessibilitySettings> {
    cx.setting(t(label))
        .described(t(&format!("{label}-desc")))
        .id(id)
}

/// Vision: high contrast, reduce motion, text size, the colour filter, the
/// magnifier and, while it magnifies, how.
fn vision_card(cx: &ViewCx<AccessibilitySettings>, magnifying: bool) -> SettingsSection {
    let appearance = cx.settings::<AppearancePrefs>();
    let mut content: Vec<Item> = [high_contrast, reduce_motion, text_size]
        .into_iter()
        .filter_map(|row| record_item(cx, &appearance, row))
        .collect();
    content.extend([colour_filter(cx), magnifier_zoom(cx)]);
    if magnifying {
        content.push(choose(
            cx.setting(t("a11y-magnifier-view"))
                .id("a11y.magnifier_view")
                .nested(),
            AccessibilitySettings::paths().magnifier_view,
        ));
    }
    section(&t("a11y-vision-title"), rows(content))
}

/// Pointer: mouse keys, a large cursor and, while it is on, its size.
fn pointer_card(cx: &ViewCx<AccessibilitySettings>, large: bool) -> SettingsSection {
    let access = AccessibilitySettings::paths();
    let mut content = vec![mouse_keys(cx), large_cursor(cx)];
    if large {
        content.push(cursor_size(cx));
    }
    content.extend([
        assist(cx, "a11y-pointer-crosshair", "a11y.cursor_crosshair")
            .toggle(access.cursor_crosshair),
        assist(cx, "a11y-thick-caret", "a11y.thick_text_caret").toggle(access.thick_text_caret),
    ]);
    section(&t("a11y-pointer-title"), rows(content))
}

/// Typing: each key assist, with what tunes it shown under it while it is
/// on (`bounce`, `switch`).
fn typing_card(cx: &ViewCx<AccessibilitySettings>, bounce: bool, switch: bool) -> SettingsSection {
    let access = AccessibilitySettings::paths();
    let mut content = vec![sticky_keys(cx), slow_keys(cx), bounce_keys(cx)];
    if bounce {
        content.extend([
            cx.setting(t("a11y-bounce-delay"))
                .id("a11y.bounce_delay")
                .nested()
                .among(
                    access.bounce_keys_delay_ms,
                    values(AccessibilitySettings::BOUNCE_DELAYS),
                    |ms: &u32| AccessibilitySettings::bounce_delay_label(*ms).to_owned(),
                ),
            cx.setting(t("a11y-bounce-alert"))
                .described(t("a11y-bounce-alert-desc"))
                .id("a11y.bounce_keys_alert")
                .nested()
                .toggle(access.bounce_keys_alert),
        ]);
    }
    content.extend([on_screen_keyboard(cx), switch_control(cx)]);
    if switch {
        content.extend([
            choose(
                cx.setting(t("a11y-switch-keys"))
                    .id("a11y.switch_keys")
                    .nested(),
                access.switch_key_pair,
            ),
            cx.setting(t("a11y-switch-scan"))
                .id("a11y.switch_scan")
                .nested()
                .among(access.switch_scan_ms, values(&SWITCH_SCANS), |ms: &u16| {
                    switch_scan_label(*ms).to_owned()
                }),
        ]);
    }
    content.push(value_row(
        &t("a11y-focus-highlight"),
        &t("a11y-focus-highlight-status"),
    ));
    section(&t("a11y-typing-title"), rows(content))
}

/// Screen reader: its switch heads the group; how it speaks under it.
/// Without a speech engine (`speech`) the group says why it cannot read.
fn reader_card(
    cx: &ViewCx<AccessibilitySettings>,
    speech: Option<&'static str>,
) -> SettingsSection {
    let access = AccessibilitySettings::paths();
    let Some(backend) = speech else {
        let row = match super::installed_programs_unreadable() {
            Some(error) => error_row(
                &t("a11y-screen-reader"),
                &t_args("settings-programs-error", &[("error", error.into())]),
            ),
            None => value_row(&t("a11y-screen-reader"), &t("a11y-screen-reader-missing")),
        };
        return section(&t("a11y-screen-reader"), rows(vec![row]));
    };
    let on = cx.value_at(&access.screen_reader);
    let switch = cx.actions().bind(
        ui::widget::Toggle::from_state(&on)
            .with_id("a11y.screen_reader")
            .with_accessible_name(&t("a11y-screen-reader")),
        AccessibilitySettings::SET_SCREEN_READER,
    );
    section_with(
        &t("a11y-screen-reader"),
        Box::new(switch),
        rows(vec![
            note(&t("a11y-screen-reader-desc")),
            value_row(&t("a11y-speech-engine"), backend),
            cx.setting(t("a11y-speech-rate"))
                .id("a11y.reader_rate")
                .among(
                    access.screen_reader_rate,
                    values(&SPEECH_RATES),
                    |rate: &u16| speech_rate_label(*rate).to_owned(),
                ),
            cx.setting(t("a11y-voice")).id("a11y.reader_voice").among(
                access.screen_reader_voice,
                ScreenReaderVoice::ALL.iter().copied(),
                |voice: &ScreenReaderVoice| voice.label().to_owned(),
            ),
        ]),
    )
}

/// Hearing and dictation: what replaces sound, and how speech becomes text.
fn hearing_card(cx: &ViewCx<AccessibilitySettings>) -> SettingsSection {
    let access = AccessibilitySettings::paths();
    section(
        &t("a11y-hearing-title"),
        rows(vec![
            visual_alerts(cx),
            value_row(&t("a11y-live-captions"), &t("a11y-live-captions-status")),
            dictation_language_row(cx),
            assist(cx, "a11y-dictation-literal", "a11y.dictation_literal")
                .toggle(access.dictation_literal),
            note(&t("a11y-dictation-language-desc")),
        ]),
    )
}

/// Whether the person's comforts reach this device's sign-in screens.
fn shared_card(cx: &ViewCx<AccessibilitySettings>) -> SettingsSection {
    let device = cx.settings::<AccessibilityDevicePrefs>();
    let sign_in = record_item(cx, &device, |cx| {
        cx.setting(t("a11y-sign-in"))
            .described(t("a11y-sign-in-desc"))
            .id("a11y.apply_to_sign_in")
            .toggle(AccessibilityDevicePrefs::paths().apply_to_sign_in)
    });
    section(
        &t("a11y-shared-title"),
        rows(
            std::iter::once(note(&t("a11y-shared-caption")))
                .chain(sign_in)
                .collect(),
        ),
    )
}

/// The dictation language, by its stable code, among every language
/// dictation knows by name.
fn dictation_language_row(cx: &ViewCx<AccessibilitySettings>) -> Item {
    cx.setting(t("a11y-dictation-language"))
        .id("a11y.dictation_language")
        .keyed(
            AccessibilitySettings::paths().dictation_language,
            DictationLanguage::ALL
                .iter()
                .map(|language| (language.code().to_owned(), t(&language.label_key())))
                .collect(),
            |access: &AccessibilitySettings| access.dictation_language.code().to_owned(),
            |code: String| {
                DictationLanguage::from_code(&code)
                    .ok_or_else(|| format!("`{code}` is not a dictation language code"))
            },
        )
}

/// What the accessibility settings found by search that their page draws
/// its own way are set to, as stored.
pub(crate) mod values {
    use provider_settings_ui::bound::SettingRows as _;
    use ui::framework::ViewCx;

    use super::AccessibilitySettings;
    use crate::i18n::t;
    use crate::setting::{Follows, Read, on_off};
    use crate::settings_app::SettingsApp;
    use provider_settings_ui::widgets::Item;

    /// The screen reader, a switch where there is a speech engine for it.
    pub(crate) fn screen_reader(app: &SettingsApp, follows: &Follows) -> Read {
        let on = follows
            .get::<AccessibilitySettings>(app.storage())
            .screen_reader;
        if super::software(app).speech.is_some() {
            Read::Switch(on)
        } else {
            Read::Value(Some(on_off(on)))
        }
    }

    pub(crate) fn screen_reader_switch(cx: &ViewCx<AccessibilitySettings>) -> Item {
        cx.setting(t("a11y-screen-reader"))
            .described(t("a11y-screen-reader-desc"))
            .id("a11y.screen_reader")
            .toggle_verb(
                |access: &AccessibilitySettings| access.screen_reader,
                AccessibilitySettings::SET_SCREEN_READER,
            )
    }

    pub(crate) fn dictation_language(app: &SettingsApp, follows: &Follows) -> String {
        let language = follows
            .get::<AccessibilitySettings>(app.storage())
            .dictation_language;
        crate::i18n::t(&language.label_key())
    }
}

pub(crate) fn high_contrast(cx: &ViewCx<AppearancePrefs>) -> Item {
    cx.setting(t("a11y-high-contrast"))
        .described(t("a11y-high-contrast-desc"))
        .id("a11y.high_contrast")
        .toggle(AppearancePrefs::paths().high_contrast)
}

pub(crate) fn reduce_motion(cx: &ViewCx<AppearancePrefs>) -> Item {
    cx.setting(t("a11y-reduce-motion"))
        .described(t("a11y-reduce-motion-desc"))
        .id("a11y.reduce_motion")
        .toggle(AppearancePrefs::paths().reduce_motion)
}

pub(crate) fn text_size(cx: &ViewCx<AppearancePrefs>) -> Item {
    cx.setting(t("a11y-text-size"))
        .id("a11y.text_size")
        .pictures(
            AppearancePrefs::paths().text_scale,
            TEXT_SIZES.iter().map(|(_, scale)| *scale),
            |scale: &f64| text_size_label(*scale),
            |scale: &f64| Picture::TextSize(*scale as f32),
        )
}

pub(crate) fn colour_filter(cx: &ViewCx<AccessibilitySettings>) -> Item {
    cx.setting(t("a11y-colour-filter"))
        .id("a11y.color_filter")
        .pictures(
            AccessibilitySettings::paths().color_filter,
            <ColorFilter as settings_store::Labelled>::CHOICES
                .iter()
                .copied(),
            |filter: &ColorFilter| settings_store::Labelled::label(*filter).to_owned(),
            |filter: &ColorFilter| {
                Picture::ColourFilter(match filter {
                    ColorFilter::None => ui::widget::ColourFilter::None,
                    ColorFilter::Grayscale => ui::widget::ColourFilter::Grayscale,
                    ColorFilter::Inverted => ui::widget::ColourFilter::Inverted,
                    ColorFilter::GrayscaleInverted => ui::widget::ColourFilter::GrayscaleInverted,
                })
            },
        )
}

pub(crate) fn magnifier_zoom(cx: &ViewCx<AccessibilitySettings>) -> Item {
    cx.setting(t("a11y-magnifier")).id("a11y.magnifier").among(
        AccessibilitySettings::paths().magnifier_zoom,
        magnifier::zoom_levels(),
        |zoom: &u8| magnifier::zoom_label(*zoom).to_owned(),
    )
}

pub(crate) fn mouse_keys(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-mouse-keys", "a11y.mouse_keys")
        .toggle(AccessibilitySettings::paths().mouse_keys)
}

pub(crate) fn large_cursor(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-large-cursor", "a11y.large_cursor")
        .toggle(AccessibilitySettings::paths().large_cursor)
}

pub(crate) fn cursor_size(cx: &ViewCx<AccessibilitySettings>) -> Item {
    cx.setting(t("a11y-cursor-size"))
        .id("a11y.cursor_size")
        .pictures(
            AccessibilitySettings::paths().cursor_size,
            values(AccessibilitySettings::CURSOR_SIZES),
            |size: &u32| AccessibilitySettings::cursor_size_label(*size).to_owned(),
            |size: &u32| Picture::Pointer(*size as f32),
        )
}

pub(crate) fn sticky_keys(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-sticky-keys", "a11y.sticky_keys")
        .toggle(AccessibilitySettings::paths().sticky_keys)
}

pub(crate) fn slow_keys(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-slow-keys", "a11y.slow_keys").toggle(AccessibilitySettings::paths().slow_keys)
}

pub(crate) fn bounce_keys(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-bounce-keys", "a11y.bounce_keys")
        .toggle(AccessibilitySettings::paths().bounce_keys)
}

pub(crate) fn on_screen_keyboard(cx: &ViewCx<AccessibilitySettings>) -> Item {
    choose(
        assist(cx, "a11y-osk", "a11y.on_screen_keyboard"),
        AccessibilitySettings::paths().on_screen_keyboard,
    )
}

pub(crate) fn switch_control(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-switch-control", "a11y.switch_control")
        .toggle(AccessibilitySettings::paths().switch_control)
}

pub(crate) fn visual_alerts(cx: &ViewCx<AccessibilitySettings>) -> Item {
    assist(cx, "a11y-visual-alerts", "a11y.visual_alerts")
        .toggle(AccessibilitySettings::paths().visual_alerts)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use crate::testing::{ACCESSIBILITY, edit_field};
    use tilo_accessibility_settings::OnScreenKeyboard;
    use ui::prelude::TestApp;
    use ui::widget::Widget;

    #[test]
    fn dictation_language_action_persists_stable_code_and_preserves_other_assists() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, ACCESSIBILITY, "sticky_keys", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "dictation_language", "de").unwrap();
        let preferences = app.settings::<AccessibilitySettings>();
        let section = preferences.document().section("accessibility");
        assert_eq!(
            AccessibilitySettings::from_section(&section)
                .dictation_language
                .code(),
            "de"
        );
        assert!(preferences.get().sticky_keys);
        // A code dictation does not know is refused at the field, and
        // nothing changes.
        let before = app.settings::<AccessibilitySettings>().get();
        assert!(edit_field(&mut app, ACCESSIBILITY, "dictation_language", "Deutsch").is_err());
        assert_eq!(app.settings::<AccessibilitySettings>().get(), before);
        for language in DictationLanguage::ALL {
            assert_ne!(t(&language.label_key()), language.label_key());
        }
    }

    #[test]
    fn dictation_literal_edit_preserves_language_and_other_assists() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, ACCESSIBILITY, "sticky_keys", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "dictation_language", "en").unwrap();
        edit_field(&mut app, ACCESSIBILITY, "dictation_literal", true).unwrap();
        let preferences = app.settings::<AccessibilitySettings>();
        assert!(preferences.get().dictation_literal);
        assert!(preferences.get().sticky_keys);
        assert_eq!(preferences.get().dictation_language.code(), "en");
        assert_eq!(
            preferences
                .document()
                .section("accessibility")
                .get("dictation_literal"),
            Some(&serde_json::json!(true))
        );
        edit_field(&mut app, ACCESSIBILITY, "dictation_literal", false).unwrap();
        assert!(
            !app.settings::<AccessibilitySettings>()
                .get()
                .dictation_literal
        );
    }

    /// The vision comforts are the appearance section's (one stored value,
    /// shown in both panes); every other assist lands in the accessibility
    /// section the compositor's input layer reads, keeping what the
    /// compositor itself keeps there.
    #[test]
    fn accessibility_effects_write_their_sections() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::testing::edit_field(&mut app, crate::testing::APPEARANCE, "high_contrast", true)
            .unwrap();
        crate::testing::edit_field(&mut app, crate::testing::APPEARANCE, "text_scale", 1.3)
            .unwrap();
        let appearance = app.settings::<AppearancePrefs>().get();
        assert!(appearance.high_contrast);
        assert_eq!(appearance.text_scale, 1.30);

        edit_field(&mut app, ACCESSIBILITY, "large_cursor", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "cursor_size", 64).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "sticky_keys", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "bounce_keys", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "bounce_keys_delay_ms", 500).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "bounce_keys_alert", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "mouse_keys", true).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "on_screen_keyboard", "always").unwrap();
        edit_field(&mut app, ACCESSIBILITY, "switch_scan_ms", 2000).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "screen_reader_rate", 140).unwrap();
        let access = app.settings::<AccessibilitySettings>().get();
        assert!(access.large_cursor);
        assert_eq!(access.cursor_size, 64);
        assert!(access.sticky_keys);
        assert!(access.bounce_keys && access.bounce_keys_alert && access.mouse_keys);
        assert_eq!(access.bounce_keys_delay_ms, 500);
        assert_eq!(access.on_screen_keyboard, OnScreenKeyboard::Always);
        assert_eq!(access.switch_scan_ms, 2000);
        assert_eq!(access.screen_reader_rate, 140);

        assert!(edit_field(&mut app, ACCESSIBILITY, "cursor_size", 9999).is_err());
        assert_eq!(app.settings::<AccessibilitySettings>().get(), access);
    }

    /// The magnifier's zoom and view land where the compositor's magnifier
    /// reads them, beside the zoom it remembers itself.
    #[test]
    fn magnifier_effects_reach_the_compositor_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let document = app.settings::<AccessibilitySettings>().document().clone();
        let mut remembered = serde_json::Map::new();
        remembered.insert("magnifier_last_zoom".into(), 3.into());
        document
            .update_section(magnifier::SECTION, remembered)
            .unwrap();

        edit_field(&mut app, ACCESSIBILITY, "magnifier_zoom", 4).unwrap();
        edit_field(&mut app, ACCESSIBILITY, "magnifier_view", "full_screen").unwrap();
        let section = document.section(magnifier::SECTION);
        assert_eq!(
            magnifier::MagnifierState::from_section(&section),
            magnifier::MagnifierState {
                zoom: 4,
                view: magnifier::MagnifierView::FullScreen,
            }
        );
        assert_eq!(section.get("magnifier_last_zoom"), Some(&3.into()));
    }

    /// The control ids the window shows now.
    fn shown_ids(app: &mut TestApp<SettingsShell>) -> Vec<String> {
        fn ids(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                out.push(id.to_owned());
            }
            for child in widget.children() {
                ids(child, out);
            }
        }
        let mut out = Vec::new();
        ids(app.harness().root_widget(), &mut out);
        out
    }

    /// An assist's own options show under it only while it is on, and turn
    /// up as soon as it is turned on elsewhere.
    #[test]
    fn an_assist_s_options_show_only_while_it_is_on() {
        let options = [
            "a11y.cursor_size",
            "a11y.magnifier_view",
            "a11y.bounce_delay",
            "a11y.bounce_keys_alert",
            "a11y.switch_keys",
            "a11y.switch_scan",
        ];
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1400, 4000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Accessibility);
        settle(&mut app);
        let off = shown_ids(&mut app);
        for option in options {
            assert!(
                !off.iter().any(|id| id == option),
                "{option} hides while its assist is off"
            );
        }
        assert!(off.iter().any(|id| id == "a11y.large_cursor"));

        app.settings::<AccessibilitySettings>()
            .update(|access| {
                access.large_cursor = true;
                access.magnifier_zoom = 2;
                access.bounce_keys = true;
                access.switch_control = true;
            })
            .unwrap();
        settle(&mut app);
        let on = shown_ids(&mut app);
        for option in options {
            assert!(
                on.iter().any(|id| id == option),
                "{option} shows once its assist is on"
            );
        }
    }

    /// The page shows both sections and follows a change made elsewhere: a
    /// zoom set by the magnifier shortcut is shown, not reset to off.
    #[test]
    fn the_page_follows_both_sections() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.settings::<AccessibilitySettings>()
            .set(&AccessibilitySettings {
                magnifier_zoom: 5,
                cursor_size: 64,
                large_cursor: true,
                ..AccessibilitySettings::default()
            })
            .unwrap();
        app.harness().resize((1400, 4000)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Accessibility);
        settle(&mut app);
        assert!(app.harness().a11y_node_named("5×").is_some());
        assert!(app.harness().a11y_node_named("Largest").is_some());

        app.settings::<AccessibilitySettings>()
            .update(|access| access.magnifier_zoom = 3)
            .unwrap();
        settle(&mut app);
        assert!(app.harness().a11y_node_named("3×").is_some());
    }

    fn access_nodes(app: &mut TestApp<SettingsShell>) -> Vec<ui::a11y::AccessNode> {
        fn flatten(node: ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
            let children = node.children.clone();
            out.push(node);
            for child in children {
                flatten(child, out);
            }
        }
        let mut nodes = Vec::new();
        for node in app.harness().focus_access_nodes() {
            flatten(node, &mut nodes);
        }
        nodes
    }

    fn settle(app: &mut TestApp<SettingsShell>) {
        for _ in 0..10 {
            app.settle();
            let _ = app.harness().poll_frame();
        }
    }

    #[test]
    fn a_click_on_text_size_and_high_contrast_writes_the_appearance_section() {
        ui::i18n::I18nManager::init("en-US");
        click_and_check(TestApp::<SettingsShell>::new().unwrap());
    }

    #[test]
    fn a_click_writes_the_appearance_section_kept_in_the_persons_records() {
        ui::i18n::I18nManager::init("en-US");
        click_and_check(TestApp::<SettingsShell>::for_person(&"ad".repeat(32)).unwrap());
    }

    /// Where text size picture `index` is drawn, as its accessibility node says.
    fn segment_rect(app: &mut TestApp<SettingsShell>, index: usize) -> vello::kurbo::Rect {
        let target = format!("a11y.text_size.{index}");
        access_nodes(app)
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some(target.as_str()))
            .unwrap_or_else(|| panic!("the text size choice publishes {target}"))
            .bounds
            .expect("the picture publishes its bounds")
            .to_rect()
    }

    /// The window's language is chosen before `app` is made: a manager
    /// installed under a live window replaces the catalogues it loaded.
    fn click_and_check(mut app: TestApp<SettingsShell>) {
        app.harness().resize((1400, 2400)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Accessibility);
        settle(&mut app);
        let large = segment_rect(&mut app, 2);
        app.harness().click_at(large.center().x, large.center().y);
        settle(&mut app);
        assert_eq!(
            app.settings::<AppearancePrefs>().get().text_scale,
            1.15,
            "a click on Large writes"
        );
        // The click focused the choice: the next picture by key writes too.
        for key in [
            ui::winit::keyboard::NamedKey::ArrowRight,
            ui::winit::keyboard::NamedKey::Enter,
        ] {
            app.harness().send(&ui::synthetic_input::named_key(key));
        }
        settle(&mut app);
        assert_eq!(
            app.settings::<AppearancePrefs>().get().text_scale,
            1.30,
            "ArrowRight and Enter on Larger writes"
        );
        let toggle = ui::widget::find_test_id(app.harness().root_widget(), "a11y.high_contrast")
            .expect("toggle")
            .get_rect();
        app.harness().click_at(toggle.center().x, toggle.center().y);
        settle(&mut app);
        assert!(
            app.settings::<AppearancePrefs>().get().high_contrast,
            "a click on the toggle writes"
        );
        // And a tap on Small (last: a touch leaves the window without hover).
        let small = segment_rect(&mut app, 0);
        let (x, y) = (small.center().x, small.center().y);
        app.harness()
            .touch(1, ui::winit::event::TouchPhase::Started, x, y);
        app.harness()
            .touch(1, ui::winit::event::TouchPhase::Ended, x, y);
        settle(&mut app);
        assert_eq!(
            app.settings::<AppearancePrefs>().get().text_scale,
            0.85,
            "a tap on Small writes"
        );
    }
}
