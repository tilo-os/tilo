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
//!   layer, each set by an [`AccessibilityEffect`].

use settings_store::dictation::DictationLanguage;
use settings_store::magnifier;
use tilo_accessibility_settings::{ColorFilter, ScreenReaderVoice};

use crate::accessibility_settings::AccessibilitySettings;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus};
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{AppModel, Dropdown, ModelPath, Picture, Widget};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::row::SettingRow;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, error_row, note, rows, section, section_with, value_row,
};

crate::effects::effect_family!(AccessibilityEffect => answer);

crate::area::pane_area! {
    Software;
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        statuses(self.speech.is_some())
    }
}

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
    // `total_cmp`, not `partial_cmp().unwrap()`: a hand-edited settings.json
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

fn labels<T>(table: &'static [(&'static str, T)]) -> impl Iterator<Item = &'static str> {
    table.iter().map(|(label, _)| *label)
}

fn values<T: Copy>(table: &'static [(&'static str, T)]) -> impl Iterator<Item = T> {
    table.iter().map(|(_, value)| *value)
}

/// The pointer, typing, hearing and screen-reader assists the compositor's
/// input and accessibility layer applies.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AccessibilityEffect {
    /// Turn the screen reader on or off.
    #[effect(idempotent)]
    #[effect(keywords = "narrator, voiceover, accessibility, read aloud")]
    SetScreenReader { on: bool },
    /// Set how fast the screen reader speaks.
    #[effect(idempotent)]
    SetSpeechRate {
        #[param("The rate", choices = labels(&SPEECH_RATES))]
        rate: String,
    },
    /// Set the screen reader's voice.
    #[effect(idempotent)]
    SetSpeechVoice {
        #[param("The voice", choices = ScreenReaderVoice::OPTIONS.iter().copied())]
        voice: String,
    },
    /// Select the local dictation language without starting capture.
    #[effect(idempotent)]
    SetDictationLanguage {
        #[param("Stable recognition language code", choices = DictationLanguage::ALL.iter().map(|language| language.code()))]
        language: String,
    },
}

/// Answer `effect`.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AccessibilityEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    use AccessibilityEffect as E;
    let not = |value: &str, what: &str| format!("`{value}` is not a {what}");
    let change: Box<dyn FnOnce(&mut AccessibilitySettings)> = match effect {
        E::SetScreenReader { on } => Box::new(move |p| p.screen_reader = on),
        E::SetSpeechRate { rate } => {
            let wpm = SPEECH_RATES
                .iter()
                .find(|(label, _)| *label == rate)
                .map(|(_, wpm)| *wpm)
                .ok_or_else(|| not(&rate, "speech rate"))?;
            Box::new(move |p| p.screen_reader_rate = wpm)
        }
        E::SetSpeechVoice { voice } => {
            let voice =
                ScreenReaderVoice::from_label(&voice).ok_or_else(|| not(&voice, "voice"))?;
            Box::new(move |p| p.screen_reader_voice = voice)
        }
        E::SetDictationLanguage { language } => {
            let language = DictationLanguage::from_code(&language)
                .ok_or_else(|| not(&language, "dictation language code"))?;
            Box::new(move |p| p.dictation_language = language)
        }
    };
    app.host().update(change)
}

/// What this device cannot do now: the screen reader needs a speech engine
/// that may not be installed. The on-screen keyboard ships with every image
/// and the compositor decides whether it runs; a sandboxed Settings cannot
/// read the package catalogue, so it does not second-guess that.
pub(crate) fn statuses(speech: bool) -> Vec<EffectStatus> {
    let mut statuses = Vec::new();
    if !speech {
        for unavailable in [
            effect(AccessibilityEffect::SET_SCREEN_READER).id,
            effect(AccessibilityEffect::SET_SPEECH_RATE).id,
            effect(AccessibilityEffect::SET_SPEECH_VOICE).id,
        ] {
            statuses.push(EffectStatus::unavailable(
                ActionRef::<SettingsEffect, ()>::new(
                    unavailable,
                    &[],
                    <SettingsEffect as ui::tilo_resource::Effect>::kind(),
                ),
                "not_installed",
                t("a11y-screen-reader-missing"),
            ));
        }
    }
    statuses
}

pub struct AccessibilityPane {
    cx: PaneCx,
    appearance: Followed<AppearancePrefs>,
    access: Followed<AccessibilitySettings>,
    /// The speech engine the screen reader would speak through.
    speech: Option<&'static str>,
    composed: ui::widget::Composed,
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

    /// What `cx` reaches: this device's software, or the sample.
    pub fn for_host(cx: &PaneCx) -> Self {
        if cx.reaches_devices() {
            Self::installed()
        } else {
            Self::sample()
        }
    }
}

impl AccessibilityPane {
    pub fn new(cx: PaneCx, software: Software) -> Self {
        Self {
            appearance: cx.follow::<AppearancePrefs>(),
            access: cx.follow::<AccessibilitySettings>(),
            cx,
            speech: software.speech,
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn access(&self) -> &AccessibilitySettings {
        self.access.get()
    }

    pub fn appearance(&self) -> &AppearancePrefs {
        self.appearance.get()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        crate::widgets::page(vec![
            self.build_vision_card(),
            self.build_pointer_card(),
            self.build_typing_card(),
            self.build_reader_card(),
            self.build_hearing_card(),
            self.build_explainer_card(),
        ])
    }

    /// The row of the assist at `path`, labelled by `label` and its
    /// `-desc` line, its control `id`.
    fn assist<V>(
        &self,
        label: &str,
        id: &str,
        path: tilo_resource::Path<AccessibilitySettings, V>,
    ) -> SettingRow<'_, AccessibilitySettings, V> {
        assist(&self.cx, &self.access, label, id, path)
    }

    // --- Vision: high contrast, reduce motion, text size, the magnifier
    // and, while it magnifies, how.
    fn build_vision_card(&self) -> SettingsSection {
        let mut content = vec![
            high_contrast(&self.cx, &self.appearance),
            reduce_motion(&self.cx, &self.appearance),
            text_size(&self.cx, &self.appearance),
            colour_filter(&self.cx, &self.access),
            magnifier_zoom(&self.cx, &self.access),
        ];
        if self.access().magnifier_zoom != magnifier::OFF {
            content.push(
                self.cx
                    .row(t("a11y-magnifier-view"))
                    .id("a11y.magnifier_view")
                    .nested()
                    .of(&self.access, AccessibilitySettings::paths().magnifier_view)
                    .choose(),
            );
        }
        section(&t("a11y-vision-title"), rows(content))
    }

    // --- Pointer: mouse keys, a large cursor and, while it is on, its size.
    fn build_pointer_card(&self) -> SettingsSection {
        let access = AccessibilitySettings::paths();
        let mut content = vec![
            mouse_keys(&self.cx, &self.access),
            large_cursor(&self.cx, &self.access),
        ];
        if self.access().large_cursor {
            content.push(cursor_size(&self.cx, &self.access));
        }
        content.extend([
            self.assist(
                "a11y-pointer-crosshair",
                "a11y.cursor_crosshair",
                access.cursor_crosshair,
            )
            .toggle(),
            self.assist(
                "a11y-thick-caret",
                "a11y.thick_text_caret",
                access.thick_text_caret,
            )
            .toggle(),
        ]);
        section(&t("a11y-pointer-title"), rows(content))
    }

    // --- Typing: each key assist, with what tunes it shown under it while
    // it is on.
    fn build_typing_card(&self) -> SettingsSection {
        let access = AccessibilitySettings::paths();
        let on = self.access();
        let mut content = vec![
            sticky_keys(&self.cx, &self.access),
            slow_keys(&self.cx, &self.access),
            bounce_keys(&self.cx, &self.access),
        ];
        if on.bounce_keys {
            content.extend([
                self.cx
                    .row(t("a11y-bounce-delay"))
                    .id("a11y.bounce_delay")
                    .nested()
                    .of(&self.access, access.bounce_keys_delay_ms)
                    .among(values(AccessibilitySettings::BOUNCE_DELAYS), |ms: &u32| {
                        AccessibilitySettings::bounce_delay_label(*ms).to_owned()
                    }),
                self.cx
                    .row(t("a11y-bounce-alert"))
                    .described(t("a11y-bounce-alert-desc"))
                    .id("a11y.bounce_keys_alert")
                    .nested()
                    .of(&self.access, access.bounce_keys_alert)
                    .toggle(),
            ]);
        }
        content.extend([
            on_screen_keyboard(&self.cx, &self.access),
            switch_control(&self.cx, &self.access),
        ]);
        let focus_highlight = value_row(
            &t("a11y-focus-highlight"),
            &t("a11y-focus-highlight-status"),
        );
        if on.switch_control {
            content.extend([
                self.cx
                    .row(t("a11y-switch-keys"))
                    .id("a11y.switch_keys")
                    .nested()
                    .of(&self.access, access.switch_key_pair)
                    .choose(),
                self.cx
                    .row(t("a11y-switch-scan"))
                    .id("a11y.switch_scan")
                    .nested()
                    .of(&self.access, access.switch_scan_ms)
                    .among(values(&SWITCH_SCANS), |ms: &u16| {
                        switch_scan_label(*ms).to_owned()
                    }),
            ]);
        }
        content.push(focus_highlight);
        section(&t("a11y-typing-title"), rows(content))
    }

    // --- Screen reader: its switch heads the group; how it speaks under
    // it. Without a speech engine the group says why it cannot read.
    fn build_reader_card(&self) -> SettingsSection {
        let access = AccessibilitySettings::paths();
        let Some(backend) = self.speech else {
            let row = match super::installed_programs_unreadable() {
                Some(error) => error_row(
                    &t("a11y-screen-reader"),
                    &t_args("settings-programs-error", &[("error", error.into())]),
                ),
                None => value_row(&t("a11y-screen-reader"), &t("a11y-screen-reader-missing")),
            };
            return section(&t("a11y-screen-reader"), rows(vec![row]));
        };
        let switch = self.cx.bind(
            ui::widget::Toggle::from_state(&self.access.shown(&access.screen_reader, bool::clone))
                .with_id("a11y.screen_reader")
                .with_accessible_name(&t("a11y-screen-reader")),
            effect(AccessibilityEffect::SET_SCREEN_READER),
        );
        section_with(
            &t("a11y-screen-reader"),
            Box::new(switch),
            rows(vec![
                note(&t("a11y-screen-reader-desc")),
                value_row(&t("a11y-speech-engine"), backend),
                self.cx
                    .row(t("a11y-speech-rate"))
                    .id("a11y.reader_rate")
                    .choice_effect(
                        labels(&SPEECH_RATES),
                        &self.access.shown(&access.screen_reader_rate, |rate: &u16| {
                            speech_rate_label(*rate).to_owned()
                        }),
                        effect(AccessibilityEffect::SET_SPEECH_RATE),
                    ),
                self.cx
                    .row(t("a11y-voice"))
                    .id("a11y.reader_voice")
                    .choice_effect(
                        ScreenReaderVoice::OPTIONS.iter().copied(),
                        &self
                            .access
                            .shown(&access.screen_reader_voice, |voice: &ScreenReaderVoice| {
                                voice.label().to_owned()
                            }),
                        effect(AccessibilityEffect::SET_SPEECH_VOICE),
                    ),
            ]),
        )
    }

    // --- Hearing and dictation: what replaces sound, and how speech
    // becomes text.
    fn build_hearing_card(&self) -> SettingsSection {
        let access = AccessibilitySettings::paths();
        section(
            &t("a11y-hearing-title"),
            rows(vec![
                visual_alerts(&self.cx, &self.access),
                value_row(&t("a11y-live-captions"), &t("a11y-live-captions-status")),
                self.dictation_language_row(),
                self.assist(
                    "a11y-dictation-literal",
                    "a11y.dictation_literal",
                    access.dictation_literal,
                )
                .toggle(),
                note(&t("a11y-dictation-language-desc")),
            ]),
        )
    }

    fn build_explainer_card(&self) -> SettingsSection {
        section(
            &t("a11y-shared-title"),
            rows(vec![
                note(&t("a11y-shared-caption")),
                self.assist(
                    "a11y-sign-in",
                    "a11y.apply_to_sign_in",
                    AccessibilitySettings::paths().apply_to_sign_in,
                )
                .toggle(),
            ]),
        )
    }

    /// The dictation language, by its stable code, among every language
    /// dictation knows by name.
    fn dictation_language_row(&self) -> Item {
        self.cx
            .row(t("a11y-dictation-language"))
            .of(
                &self.access,
                AccessibilitySettings::paths().dictation_language,
            )
            .custom(
                |language: &DictationLanguage| language.code().to_owned(),
                |language, _| {
                    let mut dropdown = Dropdown::from_state(Vec::new(), language)
                        .with_id("a11y.dictation_language")
                        .with_resolved_accessible_name(t("a11y-dictation-language"));
                    dropdown.set_value_options(
                        DictationLanguage::ALL
                            .iter()
                            .map(|language| (language.code().to_owned(), t(&language.label_key())))
                            .collect(),
                    );
                    Box::new(self.cx.bind(
                        dropdown,
                        effect(AccessibilityEffect::SET_DICTATION_LANGUAGE),
                    ))
                },
            )
    }
}

/// The row of the assist at `path` of `access`, labelled by `label` and its
/// `-desc` line, its control `id`.
fn assist<'a, V>(
    cx: &'a PaneCx,
    access: &'a Followed<AccessibilitySettings>,
    label: &str,
    id: &str,
    path: tilo_resource::Path<AccessibilitySettings, V>,
) -> SettingRow<'a, AccessibilitySettings, V> {
    cx.row(t(label))
        .described(t(&format!("{label}-desc")))
        .id(id)
        .of(access, path)
}

/// What the accessibility settings found by search that their pane draws
/// its own way are set to, as stored.
pub(crate) mod values {
    use ui::LocalState;

    use super::{AccessibilityEffect, AccessibilitySettings};
    use crate::effects::effect;
    use crate::host::PaneCx;
    use crate::i18n::t;
    use crate::setting::{Follows, Read, on_off};
    use crate::settings_app::SettingsApp;
    use crate::widgets::Item;

    /// The screen reader, a switch where there is a speech engine for it.
    pub(crate) fn screen_reader(app: &SettingsApp, follows: &Follows) -> Read {
        let on = follows
            .get::<AccessibilitySettings>(app.host())
            .screen_reader;
        if super::software(app).speech.is_some() {
            Read::Switch(on)
        } else {
            Read::Value(Some(on_off(on)))
        }
    }

    pub(crate) fn screen_reader_switch(cx: &PaneCx, on: bool) -> Item {
        cx.row(t("a11y-screen-reader"))
            .described(t("a11y-screen-reader-desc"))
            .id("a11y.screen_reader")
            .toggle_effect(
                &LocalState::new(on),
                effect(AccessibilityEffect::SET_SCREEN_READER),
            )
    }

    pub(crate) fn dictation_language(app: &SettingsApp, follows: &Follows) -> String {
        let language = follows
            .get::<AccessibilitySettings>(app.host())
            .dictation_language;
        crate::i18n::t(&language.label_key())
    }
}

pub(crate) fn high_contrast(cx: &PaneCx, appearance: &Followed<AppearancePrefs>) -> Item {
    cx.row(t("a11y-high-contrast"))
        .described(t("a11y-high-contrast-desc"))
        .id("a11y.high_contrast")
        .of(appearance, AppearancePrefs::paths().high_contrast)
        .toggle()
}

pub(crate) fn reduce_motion(cx: &PaneCx, appearance: &Followed<AppearancePrefs>) -> Item {
    cx.row(t("a11y-reduce-motion"))
        .described(t("a11y-reduce-motion-desc"))
        .id("a11y.reduce_motion")
        .of(appearance, AppearancePrefs::paths().reduce_motion)
        .toggle()
}

pub(crate) fn text_size(cx: &PaneCx, appearance: &Followed<AppearancePrefs>) -> Item {
    cx.row(t("a11y-text-size"))
        .id("a11y.text_size")
        .of(appearance, AppearancePrefs::paths().text_scale)
        .pictures(
            TEXT_SIZES.iter().map(|(_, scale)| *scale),
            |scale: &f64| text_size_label(*scale),
            |scale: &f64| Picture::TextSize(*scale as f32),
        )
}

pub(crate) fn colour_filter(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    cx.row(t("a11y-colour-filter"))
        .id("a11y.color_filter")
        .of(access, AccessibilitySettings::paths().color_filter)
        .pictures(
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

pub(crate) fn magnifier_zoom(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    cx.row(t("a11y-magnifier"))
        .id("a11y.magnifier")
        .of(access, AccessibilitySettings::paths().magnifier_zoom)
        .among(magnifier::zoom_levels(), |zoom: &u8| {
            magnifier::zoom_label(*zoom).to_owned()
        })
}

pub(crate) fn mouse_keys(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().mouse_keys;
    assist(cx, access, "a11y-mouse-keys", "a11y.mouse_keys", path).toggle()
}

pub(crate) fn large_cursor(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().large_cursor;
    assist(cx, access, "a11y-large-cursor", "a11y.large_cursor", path).toggle()
}

pub(crate) fn cursor_size(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    cx.row(t("a11y-cursor-size"))
        .id("a11y.cursor_size")
        .of(access, AccessibilitySettings::paths().cursor_size)
        .pictures(
            values(AccessibilitySettings::CURSOR_SIZES),
            |size: &u32| AccessibilitySettings::cursor_size_label(*size).to_owned(),
            |size: &u32| Picture::Pointer(*size as f32),
        )
}

pub(crate) fn sticky_keys(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().sticky_keys;
    assist(cx, access, "a11y-sticky-keys", "a11y.sticky_keys", path).toggle()
}

pub(crate) fn slow_keys(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().slow_keys;
    assist(cx, access, "a11y-slow-keys", "a11y.slow_keys", path).toggle()
}

pub(crate) fn bounce_keys(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().bounce_keys;
    assist(cx, access, "a11y-bounce-keys", "a11y.bounce_keys", path).toggle()
}

pub(crate) fn on_screen_keyboard(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().on_screen_keyboard;
    assist(cx, access, "a11y-osk", "a11y.on_screen_keyboard", path).choose()
}

pub(crate) fn switch_control(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().switch_control;
    assist(
        cx,
        access,
        "a11y-switch-control",
        "a11y.switch_control",
        path,
    )
    .toggle()
}

pub(crate) fn visual_alerts(cx: &PaneCx, access: &Followed<AccessibilitySettings>) -> Item {
    let path = AccessibilitySettings::paths().visual_alerts;
    assist(cx, access, "a11y-visual-alerts", "a11y.visual_alerts", path).toggle()
}

impl Widget for AccessibilityPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // Controls follow the stored values; the composition changes only
        // when an assist that reveals its own options is turned on or off.
        let revealed = |access: &AccessibilitySettings| {
            (
                access.magnifier_zoom != magnifier::OFF,
                access.large_cursor,
                access.bounce_keys,
                access.switch_control,
            )
        };
        let before = revealed(self.access());
        self.appearance.refresh();
        self.access.refresh();
        if revealed(self.access()) != before {
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    crate::panes::composed_pane!(kept);

    fn name(&self) -> &'static str {
        "AccessibilityPane"
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::host::testing::{ACCESSIBILITY, edit_field};
    use crate::resource::SettingsShell;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use tilo_accessibility_settings::OnScreenKeyboard;
    use ui::prelude::TestApp;
    use ui::theme::Theme;

    fn set(app: &mut TestApp<SettingsShell>, effect: impl Into<SettingsEffect>) {
        app.effect(&effect.into())
            .expect("the accessibility effect is answered");
    }

    #[test]
    fn dictation_language_action_persists_stable_code_and_preserves_other_assists() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, ACCESSIBILITY, "sticky_keys", true).unwrap();
        set(
            &mut app,
            AccessibilityEffect::SetDictationLanguage {
                language: "de".into(),
            },
        );
        let preferences = app.settings::<AccessibilitySettings>();
        let section = preferences.document().section("accessibility");
        assert_eq!(
            AccessibilitySettings::from_section(&section)
                .dictation_language
                .code(),
            "de"
        );
        assert!(preferences.get().sticky_keys);
        let before = preferences.get();
        assert!(
            app.effect(&SettingsEffect::from(
                AccessibilityEffect::SetDictationLanguage {
                    language: "Deutsch".into(),
                }
            ))
            .is_err()
        );
        assert_eq!(app.settings::<AccessibilitySettings>().get(), before);
        for language in DictationLanguage::ALL {
            assert_ne!(t(&language.label_key()), language.label_key());
        }
    }

    #[test]
    fn dictation_literal_edit_preserves_language_and_other_assists() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        edit_field(&mut app, ACCESSIBILITY, "sticky_keys", true).unwrap();
        set(
            &mut app,
            AccessibilityEffect::SetDictationLanguage {
                language: "en".into(),
            },
        );
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
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "high_contrast",
            true,
        )
        .unwrap();
        crate::host::testing::edit_field(
            &mut app,
            crate::host::testing::APPEARANCE,
            "text_scale",
            1.3,
        )
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
        set(
            &mut app,
            AccessibilityEffect::SetSpeechRate {
                rate: "Fast".into(),
            },
        );
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

    /// An assist's own options show under it only while it is on, and turn
    /// up as soon as it is turned on elsewhere.
    #[test]
    fn an_assist_s_options_show_only_while_it_is_on() {
        fn ids(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                out.push(id.to_owned());
            }
            for child in widget.children() {
                ids(child, out);
            }
        }
        let shown = |pane: &mut AccessibilityPane| {
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
            let mut out = Vec::new();
            ids(pane, &mut out);
            out
        };
        let options = [
            "a11y.cursor_size",
            "a11y.magnifier_view",
            "a11y.bounce_delay",
            "a11y.bounce_keys_alert",
            "a11y.switch_keys",
            "a11y.switch_scan",
        ];
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = AccessibilityPane::new(cx.clone(), Software::installed());
        let off = shown(&mut pane);
        for option in options {
            assert!(
                !off.iter().any(|id| id == option),
                "{option} hides while its assist is off"
            );
        }
        assert!(off.iter().any(|id| id == "a11y.large_cursor"));

        cx.settings::<AccessibilitySettings>()
            .update(|access| {
                access.large_cursor = true;
                access.magnifier_zoom = 2;
                access.bounce_keys = true;
                access.switch_control = true;
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        let on = shown(&mut pane);
        for option in options {
            assert!(
                on.iter().any(|id| id == option),
                "{option} shows once its assist is on"
            );
        }
    }

    /// The pane shows both sections and follows a change made elsewhere: a
    /// zoom set by the magnifier shortcut is shown, not reset to off.
    #[test]
    fn the_pane_follows_both_sections() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<AccessibilitySettings>()
            .set(&AccessibilitySettings {
                magnifier_zoom: 5,
                cursor_size: 64,
                ..AccessibilitySettings::default()
            })
            .unwrap();
        let mut pane = AccessibilityPane::new(cx.clone(), Software::installed());
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let access = AccessibilitySettings::paths();
        let zoom = pane.access.shown(&access.magnifier_zoom, |zoom| {
            magnifier::zoom_label(*zoom).to_owned()
        });
        assert_eq!(zoom.get(), "5×");
        let cursor = pane.access.shown(&access.cursor_size, |size| {
            AccessibilitySettings::cursor_size_label(*size).to_owned()
        });
        assert_eq!(cursor.get(), "Largest");

        cx.settings::<AppearancePrefs>()
            .update(|prefs| prefs.high_contrast = true)
            .unwrap();
        cx.settings::<AccessibilitySettings>()
            .update(|prefs| prefs.sticky_keys = true)
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        let contrast = AppearancePrefs::paths().high_contrast;
        assert!(pane.appearance.shown(&contrast, bool::clone).get());
        assert!(pane.access.shown(&access.sticky_keys, bool::clone).get());
        assert!(pane.appearance().high_contrast);
        assert!(pane.access().sticky_keys);
    }

    /// Without a speech engine, the screen reader's effects say so instead
    /// of storing an inert preference; the on-screen keyboard choice is
    /// always open.
    #[test]
    fn missing_software_makes_its_effects_unavailable() {
        assert!(statuses(true).is_empty());
        let statuses = statuses(false);
        assert_eq!(statuses.len(), 3);
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
