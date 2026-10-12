//! Accessibility pane — the dedicated assistive-technology surface.
//!
//! This panel aggregates the OS accessibility controls the way a
//! control-center's "Universal Access" panel does. It edits two sections of
//! the person's settings:
//!
//! - the **vision comforts** a low-vision user reaches for first — high
//!   contrast, reduced motion, and text size — which live in
//!   [`crate::prefs::AppearancePrefs`] (the single source of truth the whole
//!   system themes against, also editable from the Appearance pane, through
//!   the same [`AppearanceEffect`]s); and
//! - the **pointer, typing, hearing, and screen-reader** assists in
//!   [`crate::prefs::AccessibilityPrefs`], read by the compositor's input +
//!   accessibility layer, each set by an [`AccessibilityEffect`].

use std::collections::HashMap;

use settings_store::dictation::DictationLanguage;
use settings_store::magnifier;
use settings_store::screen_reader::ScreenReaderVoice;
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus};
use ui::theme::Theme;
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{AppModel, Dropdown, ModelPath, Widget};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::panes::appearance::{text_size_label, text_size_options};
use crate::prefs::{
    AccessibilityPrefs, AppearancePrefs, ColorFilter, OnScreenKeyboard, SwitchKeyPair,
};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{El, caption, card, error_row, rows, setting_row, value_row};

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

/// The colour filter a shown label stands for.
fn colour_filter(label: &str) -> Result<ColorFilter, String> {
    ColorFilter::from_label(label).ok_or_else(|| format!("`{label}` is not a colour filter"))
}

/// The magnifier zoom a shown label stands for.
fn magnifier_zoom(label: &str) -> Result<u8, String> {
    magnifier::zoom_from_label(label).ok_or_else(|| format!("`{label}` is not a magnifier zoom"))
}

/// The magnifier view a shown label stands for.
fn magnifier_view(label: &str) -> Result<magnifier::MagnifierView, String> {
    magnifier::MagnifierView::from_label(label)
        .ok_or_else(|| format!("`{label}` is not a magnifier view"))
}

/// The cursor size a shown label stands for.
fn cursor_size(label: &str) -> Result<u32, String> {
    AccessibilityPrefs::cursor_size_px(label)
        .ok_or_else(|| format!("`{label}` is not a cursor size"))
}

/// The osk mode a shown label stands for.
fn osk_mode(label: &str) -> Result<OnScreenKeyboard, String> {
    OnScreenKeyboard::from_label(label)
        .ok_or_else(|| format!("`{label}` is not an on-screen keyboard mode"))
}

/// The bounce delay a shown label stands for.
fn bounce_delay(label: &str) -> Result<u32, String> {
    AccessibilityPrefs::bounce_delay_ms(label)
        .ok_or_else(|| format!("`{label}` is not a bounce keys delay"))
}

/// The switch keys a shown label stands for.
fn switch_keys(label: &str) -> Result<SwitchKeyPair, String> {
    SwitchKeyPair::from_label(label).ok_or_else(|| format!("`{label}` is not a switch key pair"))
}

/// The switch scan a shown label stands for.
fn switch_scan(label: &str) -> Result<u16, String> {
    SWITCH_SCANS
        .iter()
        .find(|(known, _)| *known == label)
        .map(|(_, ms)| *ms)
        .ok_or_else(|| format!("`{label}` is not a scan interval"))
}

/// The pointer, typing, hearing and screen-reader assists the compositor's
/// input and accessibility layer applies.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AccessibilityEffect {
    /// Turn the screen reader on or off.
    #[effect(idempotent)]
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
    let change: Box<dyn FnOnce(&mut AccessibilityPrefs)> = match effect {
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
                ActionRef::<SettingsEffect, ()>::new(unavailable, &[]),
                "not_installed",
                t("a11y-screen-reader-missing"),
            ));
        }
    }
    statuses
}

/// What the pane's controls show: the stored preferences, as labels.
struct Shown {
    high_contrast: LocalState<bool>,
    reduce_motion: LocalState<bool>,
    text_size: LocalState<String>,
    color_filter: LocalState<String>,
    magnifier: LocalState<String>,
    magnifier_view: LocalState<String>,
    mouse_keys: LocalState<bool>,
    large_cursor: LocalState<bool>,
    cursor_size: LocalState<String>,
    cursor_crosshair: LocalState<bool>,
    thick_text_caret: LocalState<bool>,
    sticky_keys: LocalState<bool>,
    slow_keys: LocalState<bool>,
    bounce_keys: LocalState<bool>,
    bounce_delay: LocalState<String>,
    bounce_alert: LocalState<bool>,
    on_screen_keyboard: LocalState<String>,
    switch_control: LocalState<bool>,
    switch_keys: LocalState<String>,
    switch_scan: LocalState<String>,
    screen_reader: LocalState<bool>,
    speech_rate: LocalState<String>,
    speech_voice: LocalState<String>,
    dictation_language: LocalState<String>,
    dictation_literal: LocalState<bool>,
    visual_alerts: LocalState<bool>,
}

impl Shown {
    fn of(appearance: &AppearancePrefs, access: &AccessibilityPrefs) -> Self {
        let flag = || LocalState::new(false);
        let text = || LocalState::new(String::new());
        let shown = Self {
            high_contrast: flag(),
            reduce_motion: flag(),
            text_size: text(),
            color_filter: text(),
            magnifier: text(),
            magnifier_view: text(),
            mouse_keys: flag(),
            large_cursor: flag(),
            cursor_size: text(),
            cursor_crosshair: flag(),
            thick_text_caret: flag(),
            sticky_keys: flag(),
            slow_keys: flag(),
            bounce_keys: flag(),
            bounce_delay: text(),
            bounce_alert: flag(),
            on_screen_keyboard: text(),
            switch_control: flag(),
            switch_keys: text(),
            switch_scan: text(),
            screen_reader: flag(),
            speech_rate: text(),
            speech_voice: text(),
            dictation_language: text(),
            dictation_literal: flag(),
            visual_alerts: flag(),
        };
        shown.show_appearance(appearance);
        shown.show_access(access);
        shown
    }

    fn show_appearance(&self, appearance: &AppearancePrefs) {
        self.high_contrast.set(appearance.high_contrast);
        self.reduce_motion.set(appearance.reduce_motion);
        self.text_size.set(text_size_label(appearance.text_scale));
    }

    fn show_access(&self, access: &AccessibilityPrefs) {
        self.color_filter
            .set(access.color_filter.label().to_owned());
        self.magnifier
            .set(magnifier::zoom_label(access.magnifier_zoom).to_owned());
        self.magnifier_view
            .set(access.magnifier_view.label().to_owned());
        self.mouse_keys.set(access.mouse_keys);
        self.large_cursor.set(access.large_cursor);
        self.cursor_size
            .set(AccessibilityPrefs::cursor_size_label(access.cursor_size).to_owned());
        self.cursor_crosshair.set(access.cursor_crosshair);
        self.thick_text_caret.set(access.thick_text_caret);
        self.sticky_keys.set(access.sticky_keys);
        self.slow_keys.set(access.slow_keys);
        self.bounce_keys.set(access.bounce_keys);
        self.bounce_delay
            .set(AccessibilityPrefs::bounce_delay_label(access.bounce_keys_delay_ms).to_owned());
        self.bounce_alert.set(access.bounce_keys_alert);
        self.on_screen_keyboard
            .set(access.on_screen_keyboard.label().to_owned());
        self.switch_control.set(access.switch_control);
        self.switch_keys
            .set(access.switch_key_pair.label().to_owned());
        self.switch_scan
            .set(switch_scan_label(access.switch_scan_ms).to_owned());
        self.screen_reader.set(access.screen_reader);
        self.speech_rate
            .set(speech_rate_label(access.screen_reader_rate).to_owned());
        self.speech_voice
            .set(access.screen_reader_voice.label().to_owned());
        self.visual_alerts.set(access.visual_alerts);
        self.dictation_literal.set(access.dictation_literal);
        self.dictation_language
            .set(access.dictation_language.code().to_owned());
    }
}

pub struct AccessibilityPane {
    cx: PaneCx,
    appearance: Followed<AppearancePrefs>,
    access: Followed<AccessibilityPrefs>,
    shown: Shown,
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
        let appearance = cx.follow::<AppearancePrefs>();
        let access = cx.follow::<AccessibilityPrefs>();
        let shown = Shown::of(appearance.get(), access.get());
        Self {
            cx,
            appearance,
            access,
            shown,
            speech: software.speech,
            composed: ui::widget::Composed::default(),
        }
    }

    pub fn access(&self) -> &AccessibilityPrefs {
        self.access.get()
    }

    pub fn appearance(&self) -> &AppearancePrefs {
        self.appearance.get()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        crate::widgets::cards(vec![
            self.build_vision_card(),
            self.build_pointer_card(),
            self.build_typing_card(),
            self.build_advanced_card(),
            Self::build_explainer_card(),
        ])
    }

    // --- Vision: high contrast, reduce motion, text size.
    fn build_vision_card(&self) -> Box<dyn Widget> {
        let shown = &self.shown;
        card(
            &t("a11y-vision-title"),
            rows(vec![
                self.cx
                    .row(t("a11y-high-contrast"))
                    .described(t("a11y-high-contrast-desc"))
                    .id("a11y.high_contrast")
                    .toggle(&shown.high_contrast, AppearancePrefs::paths().high_contrast),
                self.cx
                    .row(t("a11y-reduce-motion"))
                    .described(t("a11y-reduce-motion-desc"))
                    .id("a11y.reduce_motion")
                    .toggle(&shown.reduce_motion, AppearancePrefs::paths().reduce_motion),
                self.cx
                    .row(t("a11y-text-size"))
                    .id("a11y.text_size")
                    .choice(
                        text_size_options(),
                        &shown.text_size,
                        AppearancePrefs::paths().text_scale,
                        |label: String| crate::panes::appearance::text_size_value(&label),
                    ),
                self.cx
                    .row(t("a11y-colour-filter"))
                    .id("a11y.color_filter")
                    .choice(
                        ColorFilter::OPTIONS.iter().copied(),
                        &shown.color_filter,
                        AccessibilityPrefs::paths().color_filter,
                        |label: String| colour_filter(&label),
                    ),
                self.cx
                    .row(t("a11y-magnifier"))
                    .id("a11y.magnifier")
                    .choice(
                        magnifier::ZOOM_LABELS,
                        &shown.magnifier,
                        AccessibilityPrefs::paths().magnifier_zoom,
                        |label: String| magnifier_zoom(&label),
                    ),
                self.cx
                    .row(t("a11y-magnifier-view"))
                    .id("a11y.magnifier_view")
                    .choice(
                        magnifier::MagnifierView::ALL.map(magnifier::MagnifierView::label),
                        &shown.magnifier_view,
                        AccessibilityPrefs::paths().magnifier_view,
                        |label: String| magnifier_view(&label),
                    ),
            ]),
        )
    }

    // --- Pointer: mouse keys, large cursor + size.
    fn build_pointer_card(&self) -> Box<dyn Widget> {
        let shown = &self.shown;
        card(
            &t("a11y-pointer-title"),
            rows(vec![
                self.cx
                    .row(t("a11y-mouse-keys"))
                    .described(t("a11y-mouse-keys-desc"))
                    .id("a11y.mouse_keys")
                    .toggle(&shown.mouse_keys, AccessibilityPrefs::paths().mouse_keys),
                caption(&t("a11y-mouse-keys-caption")),
                self.cx
                    .row(t("a11y-large-cursor"))
                    .described(t("a11y-large-cursor-desc"))
                    .id("a11y.large_cursor")
                    .toggle(
                        &shown.large_cursor,
                        AccessibilityPrefs::paths().large_cursor,
                    ),
                self.cx
                    .row(t("a11y-cursor-size"))
                    .id("a11y.cursor_size")
                    .choice(
                        labels(AccessibilityPrefs::CURSOR_SIZES),
                        &shown.cursor_size,
                        AccessibilityPrefs::paths().cursor_size,
                        |label: String| cursor_size(&label),
                    ),
                self.cx
                    .row(t("a11y-pointer-crosshair"))
                    .described(t("a11y-pointer-crosshair-desc"))
                    .id("a11y.cursor_crosshair")
                    .toggle(
                        &shown.cursor_crosshair,
                        AccessibilityPrefs::paths().cursor_crosshair,
                    ),
                self.cx
                    .row(t("a11y-thick-caret"))
                    .described(t("a11y-thick-caret-desc"))
                    .id("a11y.thick_text_caret")
                    .toggle(
                        &shown.thick_text_caret,
                        AccessibilityPrefs::paths().thick_text_caret,
                    ),
            ]),
        )
    }

    // --- Typing.
    fn build_typing_card(&self) -> Box<dyn Widget> {
        let shown = &self.shown;
        let osk_row = self
            .cx
            .row(t("a11y-osk"))
            .described(t("a11y-osk-desc"))
            .id("a11y.on_screen_keyboard")
            .choice(
                OnScreenKeyboard::OPTIONS.iter().copied(),
                &shown.on_screen_keyboard,
                AccessibilityPrefs::paths().on_screen_keyboard,
                |label: String| osk_mode(&label),
            );
        card(
            &t("a11y-typing-title"),
            rows(vec![
                self.cx
                    .row(t("a11y-sticky-keys"))
                    .described(t("a11y-sticky-keys-desc"))
                    .id("a11y.sticky_keys")
                    .toggle(&shown.sticky_keys, AccessibilityPrefs::paths().sticky_keys),
                self.cx
                    .row(t("a11y-slow-keys"))
                    .described(t("a11y-slow-keys-desc"))
                    .id("a11y.slow_keys")
                    .toggle(&shown.slow_keys, AccessibilityPrefs::paths().slow_keys),
                self.cx
                    .row(t("a11y-bounce-keys"))
                    .described(t("a11y-bounce-keys-desc"))
                    .id("a11y.bounce_keys")
                    .toggle(&shown.bounce_keys, AccessibilityPrefs::paths().bounce_keys),
                self.cx
                    .row(t("a11y-bounce-delay"))
                    .id("a11y.bounce_delay")
                    .choice(
                        labels(AccessibilityPrefs::BOUNCE_DELAYS),
                        &shown.bounce_delay,
                        AccessibilityPrefs::paths().bounce_keys_delay_ms,
                        |label: String| bounce_delay(&label),
                    ),
                self.cx
                    .row(t("a11y-bounce-alert"))
                    .described(t("a11y-bounce-alert-desc"))
                    .id("a11y.bounce_keys_alert")
                    .toggle(
                        &shown.bounce_alert,
                        AccessibilityPrefs::paths().bounce_keys_alert,
                    ),
                osk_row,
                self.cx
                    .row(t("a11y-switch-control"))
                    .described(t("a11y-switch-control-desc"))
                    .id("a11y.switch_control")
                    .toggle(
                        &shown.switch_control,
                        AccessibilityPrefs::paths().switch_control,
                    ),
                self.cx
                    .row(t("a11y-switch-keys"))
                    .id("a11y.switch_keys")
                    .choice(
                        SwitchKeyPair::OPTIONS.iter().copied(),
                        &shown.switch_keys,
                        AccessibilityPrefs::paths().switch_key_pair,
                        |label: String| switch_keys(&label),
                    ),
                self.cx
                    .row(t("a11y-switch-scan"))
                    .id("a11y.switch_scan")
                    .choice(
                        labels(&SWITCH_SCANS),
                        &shown.switch_scan,
                        AccessibilityPrefs::paths().switch_scan_ms,
                        |label: String| switch_scan(&label),
                    ),
            ]),
        )
    }

    // Working cross-application assists stay writable here. Capabilities
    // without an apply path remain status rows below so Settings never
    // promises an inert preference.
    fn build_advanced_card(&self) -> Box<dyn Widget> {
        let shown = &self.shown;
        let reader_rows = if let Some(backend) = self.speech {
            vec![
                self.cx
                    .row(t("a11y-screen-reader"))
                    .described(t("a11y-screen-reader-desc"))
                    .id("a11y.screen_reader")
                    .toggle_effect(
                        &shown.screen_reader,
                        effect(AccessibilityEffect::SET_SCREEN_READER),
                    ),
                value_row(&t("a11y-speech-engine"), backend),
                self.cx
                    .row(t("a11y-speech-rate"))
                    .id("a11y.reader_rate")
                    .choice_effect(
                        labels(&SPEECH_RATES),
                        &shown.speech_rate,
                        effect(AccessibilityEffect::SET_SPEECH_RATE),
                    ),
                self.cx
                    .row(t("a11y-voice"))
                    .id("a11y.reader_voice")
                    .choice_effect(
                        ScreenReaderVoice::OPTIONS.iter().copied(),
                        &shown.speech_voice,
                        effect(AccessibilityEffect::SET_SPEECH_VOICE),
                    ),
            ]
        } else if let Some(error) = super::installed_programs_unreadable() {
            vec![error_row(
                &t("a11y-screen-reader"),
                &t_args("settings-programs-error", &[("error", error.into())]),
            )]
        } else {
            vec![value_row(
                &t("a11y-screen-reader"),
                &t("a11y-screen-reader-missing"),
            )]
        };
        card(
            &t("a11y-advanced-title"),
            rows(
                reader_rows
                    .into_iter()
                    .chain([
                        setting_row(
                            &t("a11y-dictation-language"),
                            self.dictation_language_choice(),
                        ),
                        caption(&t("a11y-dictation-language-desc")),
                        self.cx
                            .row(t("a11y-dictation-literal"))
                            .described(t("a11y-dictation-literal-desc"))
                            .id("a11y.dictation_literal")
                            .toggle(
                                &shown.dictation_literal,
                                AccessibilityPrefs::paths().dictation_literal,
                            ),
                        self.cx
                            .row(t("a11y-visual-alerts"))
                            .described(t("a11y-visual-alerts-desc"))
                            .id("a11y.visual_alerts")
                            .toggle(
                                &shown.visual_alerts,
                                AccessibilityPrefs::paths().visual_alerts,
                            ),
                        value_row(
                            &t("a11y-focus-highlight"),
                            &t("a11y-focus-highlight-status"),
                        ),
                        value_row(&t("a11y-live-captions"), &t("a11y-live-captions-status")),
                    ])
                    .collect(),
            ),
        )
    }

    fn build_explainer_card() -> Box<dyn Widget> {
        card(
            &t("a11y-shared-title"),
            rows(vec![caption(&t("a11y-shared-caption"))]),
        )
    }

    fn dictation_language_choice(&self) -> El {
        let mut dropdown = Dropdown::from_state(Vec::new(), &self.shown.dictation_language)
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
    }
}

impl Widget for AccessibilityPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // Controls follow the stored values; the composition itself does not
        // change with them.
        if self.appearance.refresh() {
            self.shown.show_appearance(self.appearance.get());
        }
        if self.access.refresh() {
            self.shown.show_access(self.access.get());
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        crate::panes::pane_node(taffy, child, node_map, self)
    }

    ui::forward_to_composed!(composed);

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
    use ui::prelude::TestApp;

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
        let preferences = app.settings::<AccessibilityPrefs>();
        let section = preferences.document().section("accessibility");
        assert_eq!(DictationLanguage::from_section(&section).code(), "de");
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
        assert_eq!(app.settings::<AccessibilityPrefs>().get(), before);
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
        let preferences = app.settings::<AccessibilityPrefs>();
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
        assert!(!app.settings::<AccessibilityPrefs>().get().dictation_literal);
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
        let access = app.settings::<AccessibilityPrefs>().get();
        assert!(access.large_cursor);
        assert_eq!(access.cursor_size, 64);
        assert!(access.sticky_keys);
        assert!(access.bounce_keys && access.bounce_keys_alert && access.mouse_keys);
        assert_eq!(access.bounce_keys_delay_ms, 500);
        assert_eq!(access.on_screen_keyboard, OnScreenKeyboard::Always);
        assert_eq!(access.switch_scan_ms, 2000);
        assert_eq!(access.screen_reader_rate, 140);

        assert!(edit_field(&mut app, ACCESSIBILITY, "cursor_size", 9999).is_err());
        assert_eq!(app.settings::<AccessibilityPrefs>().get(), access);
    }

    /// The magnifier's zoom and view land where the compositor's magnifier
    /// reads them, beside the zoom it remembers itself.
    #[test]
    fn magnifier_effects_reach_the_compositor_section() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let document = app.settings::<AccessibilityPrefs>().document().clone();
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

    /// The pane shows both sections and follows a change made elsewhere: a
    /// zoom set by the magnifier shortcut is shown, not reset to off.
    #[test]
    fn the_pane_follows_both_sections() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        cx.settings::<AccessibilityPrefs>()
            .set(&AccessibilityPrefs {
                magnifier_zoom: 5,
                cursor_size: 64,
                ..AccessibilityPrefs::default()
            })
            .unwrap();
        let mut pane = AccessibilityPane::new(cx.clone(), Software::installed());
        assert_eq!(pane.shown.magnifier.get(), "5×");
        assert_eq!(pane.shown.cursor_size.get(), "Largest");

        cx.settings::<AppearancePrefs>()
            .update(|prefs| prefs.high_contrast = true)
            .unwrap();
        cx.settings::<AccessibilityPrefs>()
            .update(|prefs| prefs.sticky_keys = true)
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert!(pane.shown.high_contrast.get());
        assert!(pane.shown.sticky_keys.get());
        assert!(pane.appearance().high_contrast);
        assert!(pane.access().sticky_keys);

        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
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
}
