//! Accessibility pane — the dedicated assistive-technology surface.
//!
//! This panel aggregates the OS accessibility controls the way a
//! control-center's "Universal Access" panel does. It edits two shared
//! sections of `settings.json`:
//!
//! - the **vision comforts** a low-vision user reaches for first — high
//!   contrast, reduced motion, and text size — which live in
//!   [`crate::prefs::AppearancePrefs`] (the single source of truth the whole
//!   system themes against, also editable from the Appearance pane); and
//! - the **pointer, typing, hearing, and screen-reader** assists in
//!   [`crate::prefs::AccessibilityPrefs`], read by the compositor's input +
//!   accessibility layer.
//!
//! Editing a vision comfort here writes the *same* appearance field the
//! Appearance pane writes, so the two panes never disagree — there is one
//! stored value, surfaced in both places (as macOS/GNOME do with text size).
//!
//! Each control is bound to a CRDT key; `sync` folds the change into the
//! loaded prefs and persists the shared store, preserving every other section.

use settings_store::magnifier;
use std::collections::HashMap;
use ui::doc::{read_bool, read_str_or_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::panes::appearance::{text_size_label, text_size_options, text_size_scale};
use crate::prefs::{
    AccessibilityPrefs, AppearancePrefs, ColorFilter, OnScreenKeyboard, ScreenReaderVoice,
    SwitchKeyPair, SystemPrefs,
};
use crate::widgets::{El, caption, card, choice_row, rows, setting_row_described, value_row};

// Vision comforts (mirror the shared AppearancePrefs fields).
pub const HIGH_CONTRAST_KEY: &str = "a11y.high_contrast";
pub const REDUCE_MOTION_KEY: &str = "a11y.reduce_motion";
pub const TEXT_SIZE_KEY: &str = "a11y.text_size";
// Pointer.
pub const LARGE_CURSOR_KEY: &str = "a11y.large_cursor";
pub const CURSOR_SIZE_KEY: &str = "a11y.cursor_size";
// Typing.
pub const STICKY_KEYS_KEY: &str = "a11y.sticky_keys";
pub const SLOW_KEYS_KEY: &str = "a11y.slow_keys";
pub const BOUNCE_KEYS_KEY: &str = "a11y.bounce_keys";
pub const BOUNCE_DELAY_KEY: &str = "a11y.bounce_delay";
pub const BOUNCE_ALERT_KEY: &str = "a11y.bounce_keys_alert";
pub const MOUSE_KEYS_KEY: &str = "a11y.mouse_keys";
pub const OSK_KEY: &str = "a11y.on_screen_keyboard";
// Hearing + screen reader.
pub const VISUAL_ALERTS_KEY: &str = "a11y.visual_alerts";
pub const SCREEN_READER_KEY: &str = "a11y.screen_reader";
pub const COLOR_FILTER_KEY: &str = "a11y.color_filter";
pub const MAGNIFIER_KEY: &str = "a11y.magnifier";
pub const MAGNIFIER_VIEW_KEY: &str = "a11y.magnifier_view";
pub const CURSOR_CROSSHAIR_KEY: &str = "a11y.cursor_crosshair";
pub const THICK_CARET_KEY: &str = "a11y.thick_text_caret";
pub const SWITCH_CONTROL_KEY: &str = "a11y.switch_control";
pub const SWITCH_KEYS_KEY: &str = "a11y.switch_keys";
pub const SWITCH_SCAN_KEY: &str = "a11y.switch_scan";
pub const READER_RATE_KEY: &str = "a11y.reader_rate";
pub const READER_VOICE_KEY: &str = "a11y.reader_voice";
const OSK_PACKAGE_ID: &str = "so.tilo.osk";
const OSK_PACKAGE_KIND: &str = "service";

pub(crate) fn osk_service_available() -> bool {
    package_store::list_installed()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| launcher::manifest::read_manifest(&entry.manifest_path).ok())
        .any(|manifest| {
            manifest.package_id == OSK_PACKAGE_ID
                && manifest.kind == OSK_PACKAGE_KIND
                && manifest.entrypoint.gui_bin().is_some()
        })
}

fn executable_available(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(name);
            std::fs::metadata(candidate).is_ok_and(|m| m.is_file())
        })
    })
}

fn tts_capability() -> Option<&'static str> {
    if executable_available("speech-dispatcher") {
        Some("Speech Dispatcher")
    } else if executable_available("espeak-ng") {
        Some("eSpeak NG")
    } else {
        None
    }
}

/// Whether the screen-reader controls can produce spoken output on this
/// device. Keep this separate from the display label so the resource tree can
/// omit an effect that would otherwise be an inert promise.
pub(crate) fn screen_reader_available() -> bool {
    tts_capability().is_some()
}

pub struct AccessibilityPane {
    appearance: AppearancePrefs,
    access: AccessibilityPrefs,
    needs_seed: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AccessibilityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessibilityPane {
    pub fn new() -> Self {
        let p = SystemPrefs::load();
        Self::with_prefs(p.appearance, p.accessibility)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(appearance: AppearancePrefs, access: AccessibilityPrefs) -> Self {
        Self {
            appearance,
            access,
            needs_seed: true,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn access(&self) -> &AccessibilityPrefs {
        &self.access
    }

    pub fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }

    fn toggle(id: &str, key: &str, action_id: &str) -> El {
        let toggle = Toggle::new(id, key).with_id(id);
        let toggle = match crate::actions::accessibility_bool_binding(action_id) {
            Some(binding) => toggle.on_action(binding),
            // A capability or resource can disappear between composition and
            // dispatch. Keep the current value legible but never fall back to
            // a raw document write when that happens.
            None => toggle.with_enabled(false),
        };
        Box::new(toggle)
    }

    fn choice(options: Vec<String>, key: &str, action_id: &str) -> Dropdown {
        let dropdown = Dropdown::new(options, key);
        match crate::actions::accessibility_text_binding(action_id) {
            Some(binding) => dropdown.on_action(binding),
            None => dropdown.with_enabled(false),
        }
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
        let size_opts = text_size_options();
        let size_current = text_size_label(self.appearance.text_scale);
        let size = Self::choice(size_opts, TEXT_SIZE_KEY, "text-size")
            .with_id("a11y.text_size")
            .with_placeholder(&size_current);
        card(
            "Vision",
            rows(vec![
                setting_row_described(
                    "High contrast",
                    "Maximise contrast for low-vision comfort",
                    Self::toggle("a11y.high_contrast", HIGH_CONTRAST_KEY, "high-contrast"),
                ),
                setting_row_described(
                    "Reduce motion",
                    "Skip animations and transitions",
                    Self::toggle("a11y.reduce_motion", REDUCE_MOTION_KEY, "reduce-motion"),
                ),
                choice_row("Text size", Box::new(size)),
                choice_row(
                    "Colour filter",
                    Box::new(
                        Self::choice(
                            ColorFilter::OPTIONS
                                .iter()
                                .map(|s| (*s).to_string())
                                .collect(),
                            COLOR_FILTER_KEY,
                            "colour-filter",
                        )
                        .with_id("a11y.color_filter")
                        .with_placeholder(self.access.color_filter.label()),
                    ),
                ),
                choice_row(
                    "Magnifier",
                    Box::new(
                        Self::choice(
                            magnifier::ZOOM_LABELS
                                .iter()
                                .map(|s| (*s).to_string())
                                .collect(),
                            MAGNIFIER_KEY,
                            "magnifier",
                        )
                        .with_id("a11y.magnifier")
                        .with_placeholder(magnifier::zoom_label(self.access.magnifier_zoom)),
                    ),
                ),
                choice_row(
                    "Magnifier view",
                    Box::new(
                        Self::choice(
                            magnifier::MagnifierView::ALL
                                .iter()
                                .map(|view| view.label().to_string())
                                .collect(),
                            MAGNIFIER_VIEW_KEY,
                            "magnifier-view",
                        )
                        .with_id("a11y.magnifier_view")
                        .with_placeholder(self.access.magnifier_view.label()),
                    ),
                ),
            ]),
        )
    }

    // --- Pointer: mouse keys, large cursor + size.
    fn build_pointer_card(&self) -> Box<dyn Widget> {
        let cursor_opts: Vec<String> = AccessibilityPrefs::CURSOR_SIZES
            .iter()
            .map(|(l, _)| l.to_string())
            .collect();
        let cursor = Self::choice(cursor_opts, CURSOR_SIZE_KEY, "cursor-size")
            .with_id("a11y.cursor_size")
            .with_placeholder(AccessibilityPrefs::cursor_size_label(
                self.access.cursor_size,
            ));
        card(
            "Pointer",
            rows(vec![
                setting_row_described(
                    "Mouse keys",
                    "Move the pointer with the numeric keypad",
                    Self::toggle("a11y.mouse_keys", MOUSE_KEYS_KEY, "mouse-keys"),
                ),
                caption(
                    "Keypad 1–9 move the pointer and speed up while held; 5 clicks, + double-clicks, 0 holds the button down and . releases it. / * − choose the left, middle or right button. Shift+Alt+Num Lock turns mouse keys on or off.",
                ),
                setting_row_described(
                    "Large cursor",
                    "Enlarge the mouse pointer so it is easy to find",
                    Self::toggle("a11y.large_cursor", LARGE_CURSOR_KEY, "large-cursor"),
                ),
                choice_row("Cursor size", Box::new(cursor)),
                setting_row_described(
                    "Pointer crosshair",
                    "Draw high-contrast guide lines through the pointer",
                    Self::toggle(
                        "a11y.cursor_crosshair",
                        CURSOR_CROSSHAIR_KEY,
                        "pointer-crosshair",
                    ),
                ),
                setting_row_described(
                    "Thicker text cursor",
                    "Use a wider insertion caret in Tilo applications",
                    Self::toggle("a11y.thick_text_caret", THICK_CARET_KEY, "thick-text-caret"),
                ),
            ]),
        )
    }

    // --- Typing.
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative row list mirroring the visible Typing card"
    )]
    fn build_typing_card(&self) -> Box<dyn Widget> {
        let osk_row = if osk_service_available() {
            setting_row_described(
                "On-screen keyboard",
                "Automatic shows it for text fields while you use touch without typing on a keyboard",
                Box::new(
                    Self::choice(
                        OnScreenKeyboard::OPTIONS
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect(),
                        OSK_KEY,
                        "on-screen-keyboard",
                    )
                    .with_id("a11y.on_screen_keyboard")
                    .with_placeholder(self.access.on_screen_keyboard.label()),
                ),
            )
        } else {
            value_row(
                "On-screen keyboard",
                "Not installed — add the signed so.tilo.osk system package",
            )
        };
        card(
            "Typing",
            rows(vec![
                setting_row_described(
                    "Sticky keys",
                    "Type shortcuts one key at a time instead of holding a chord",
                    Self::toggle("a11y.sticky_keys", STICKY_KEYS_KEY, "sticky-keys"),
                ),
                setting_row_described(
                    "Slow keys",
                    "Ignore brief, accidental key presses",
                    Self::toggle("a11y.slow_keys", SLOW_KEYS_KEY, "slow-keys"),
                ),
                setting_row_described(
                    "Bounce keys",
                    "Ignore a key pressed again right after it was released",
                    Self::toggle("a11y.bounce_keys", BOUNCE_KEYS_KEY, "bounce-keys"),
                ),
                choice_row(
                    "Bounce keys delay",
                    Box::new(
                        Self::choice(
                            AccessibilityPrefs::BOUNCE_DELAYS
                                .iter()
                                .map(|(label, _)| (*label).to_string())
                                .collect(),
                            BOUNCE_DELAY_KEY,
                            "bounce-delay",
                        )
                        .with_id("a11y.bounce_delay")
                        .with_placeholder(
                            AccessibilityPrefs::bounce_delay_label(
                                self.access.bounce_keys_delay_ms,
                            ),
                        ),
                    ),
                ),
                setting_row_described(
                    "Flash when a key is ignored",
                    "Show the visual alert each time bounce keys drops a press",
                    Self::toggle(
                        "a11y.bounce_keys_alert",
                        BOUNCE_ALERT_KEY,
                        "bounce-keys-alert",
                    ),
                ),
                osk_row,
                setting_row_described(
                    "Switch control",
                    "Translate a hardware switch pair into standard Tab and Enter actions",
                    Self::toggle("a11y.switch_control", SWITCH_CONTROL_KEY, "switch-control"),
                ),
                choice_row(
                    "Switch keys (next / activate)",
                    Box::new(
                        Self::choice(
                            SwitchKeyPair::OPTIONS
                                .iter()
                                .map(|s| (*s).to_string())
                                .collect(),
                            SWITCH_KEYS_KEY,
                            "switch-keys",
                        )
                        .with_id("a11y.switch_keys")
                        .with_placeholder(self.access.switch_key_pair.label()),
                    ),
                ),
                choice_row(
                    "Automatic scan",
                    Box::new(
                        Self::choice(
                            vec![
                                "Manual".into(),
                                "1 second".into(),
                                "2 seconds".into(),
                                "3 seconds".into(),
                            ],
                            SWITCH_SCAN_KEY,
                            "switch-scan",
                        )
                        .with_id("a11y.switch_scan")
                        .with_placeholder(
                            match self.access.switch_scan_ms {
                                1000 => "1 second",
                                2000 => "2 seconds",
                                3000 => "3 seconds",
                                _ => "Manual",
                            },
                        ),
                    ),
                ),
            ]),
        )
    }

    // Working cross-application assists stay writable here. Capabilities
    // without an apply path remain status rows below so Settings never
    // promises an inert preference.
    fn build_advanced_card(&self) -> Box<dyn Widget> {
        let reader_rows = if let Some(backend) = tts_capability() {
            vec![
                setting_row_described(
                    "Screen reader",
                    "Read focused controls and live announcements aloud",
                    Self::toggle("a11y.screen_reader", SCREEN_READER_KEY, "screen-reader"),
                ),
                value_row("Speech engine", backend),
                choice_row(
                    "Speech rate",
                    Box::new(
                        Self::choice(
                            vec![
                                "Slow".into(),
                                "Default".into(),
                                "Fast".into(),
                                "Very fast".into(),
                            ],
                            READER_RATE_KEY,
                            "speech-rate",
                        )
                        .with_id("a11y.reader_rate")
                        .with_placeholder(
                            match self.access.screen_reader_rate {
                                0..=75 => "Slow",
                                76..=115 => "Default",
                                116..=160 => "Fast",
                                _ => "Very fast",
                            },
                        ),
                    ),
                ),
                choice_row(
                    "Voice",
                    Box::new(
                        Self::choice(
                            ScreenReaderVoice::OPTIONS
                                .iter()
                                .map(|s| (*s).to_string())
                                .collect(),
                            READER_VOICE_KEY,
                            "speech-voice",
                        )
                        .with_id("a11y.reader_voice")
                        .with_placeholder(self.access.screen_reader_voice.label()),
                    ),
                ),
            ]
        } else {
            vec![value_row(
                "Screen reader",
                "Unavailable — install Speech Dispatcher or eSpeak NG",
            )]
        };
        card(
            "More assistive technology",
            rows(
                reader_rows
                    .into_iter()
                    .chain([
                        setting_row_described(
                            "Visual alerts",
                            "Flash a clear screen-edge alert when a notification arrives",
                            Self::toggle("a11y.visual_alerts", VISUAL_ALERTS_KEY, "visual-alerts"),
                        ),
                        value_row(
                            "Application focus highlight",
                            "Not yet available for non-Tilo applications",
                        ),
                        value_row(
                            "Live captions",
                            "Unavailable — no local speech-to-text engine is installed",
                        ),
                    ])
                    .collect(),
            ),
        )
    }

    fn build_explainer_card() -> Box<dyn Widget> {
        card(
            "Shared with your apps",
            rows(vec![caption(
                "Vision comforts are the same settings the Appearance pane \
                 offers — changing them here or there updates one shared value. \
                 Pointer sizing, mouse keys, and the sticky, slow and bounce key \
                 filters are applied by the compositor's input layer. Colour filters, the magnifier and \
                 pointer guides are final-scene compositor effects. Text-caret \
                 width is owned by the shared toolkit. The screen reader reloads in every \
                 Tilo app and visual alerts are rendered by shell chrome. The \
                 on-screen keyboard is a sandboxed system service: the \
                 compositor starts it on a private input-method socket and \
                 keeps its surface out of workspace tiles. \
                 Unsupported capabilities are identified explicitly instead \
                 of being stored as inert toggles.",
            )]),
        )
    }

    /// Persist both edited sections into the shared store, reading fresh so a
    /// change here never clobbers another pane's section.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.appearance = self.appearance.clone();
        p.accessibility = self.access;
        if let Err(e) = p.save() {
            warn!(target: "tilo::settings", op = "accessibility_save", error = %e, "failed to persist accessibility prefs");
        }
    }

    /// Hydrate the live document from the durable preferences only when a
    /// key is genuinely absent. This is model initialization, not a user
    /// action: it never enters action history and it never overwrites a value
    /// that arrived from another replica before this pane was opened.
    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        for (key, value) in [
            (HIGH_CONTRAST_KEY, self.appearance.high_contrast),
            (REDUCE_MOTION_KEY, self.appearance.reduce_motion),
            (LARGE_CURSOR_KEY, self.access.large_cursor),
            (STICKY_KEYS_KEY, self.access.sticky_keys),
            (SLOW_KEYS_KEY, self.access.slow_keys),
            (BOUNCE_KEYS_KEY, self.access.bounce_keys),
            (BOUNCE_ALERT_KEY, self.access.bounce_keys_alert),
            (MOUSE_KEYS_KEY, self.access.mouse_keys),
            (VISUAL_ALERTS_KEY, self.access.visual_alerts),
            (SCREEN_READER_KEY, self.access.screen_reader),
            (CURSOR_CROSSHAIR_KEY, self.access.cursor_crosshair),
            (THICK_CARET_KEY, self.access.thick_text_caret),
            (SWITCH_CONTROL_KEY, self.access.switch_control),
        ] {
            if state.get(root, key).ok().flatten().is_none() {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(value));
            }
        }
        let text_size = text_size_label(self.appearance.text_scale);
        for (key, value) in [
            (TEXT_SIZE_KEY, text_size.as_str()),
            (
                CURSOR_SIZE_KEY,
                AccessibilityPrefs::cursor_size_label(self.access.cursor_size),
            ),
            (COLOR_FILTER_KEY, self.access.color_filter.label()),
            (OSK_KEY, self.access.on_screen_keyboard.label()),
            (
                MAGNIFIER_KEY,
                magnifier::zoom_label(self.access.magnifier_zoom),
            ),
            (MAGNIFIER_VIEW_KEY, self.access.magnifier_view.label()),
            (SWITCH_KEYS_KEY, self.access.switch_key_pair.label()),
            (
                BOUNCE_DELAY_KEY,
                AccessibilityPrefs::bounce_delay_label(self.access.bounce_keys_delay_ms),
            ),
            (
                SWITCH_SCAN_KEY,
                match self.access.switch_scan_ms {
                    1000 => "1 second",
                    2000 => "2 seconds",
                    3000 => "3 seconds",
                    _ => "Manual",
                },
            ),
            (
                READER_RATE_KEY,
                match self.access.screen_reader_rate {
                    0..=75 => "Slow",
                    76..=115 => "Default",
                    116..=160 => "Fast",
                    _ => "Very fast",
                },
            ),
            (READER_VOICE_KEY, self.access.screen_reader_voice.label()),
        ] {
            if state.get(root, key).ok().flatten().is_none() {
                let _ = state.put(root, key, value);
            }
        }
        self.needs_seed = false;
    }

    // Vision comforts fold into the shared appearance section.
    fn fold_vision(&mut self, state: &AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        if let Some(b) = read_bool(state, root, HIGH_CONTRAST_KEY)
            && b != self.appearance.high_contrast
        {
            self.appearance.high_contrast = b;
            changed = true;
        }
        if let Some(b) = read_bool(state, root, REDUCE_MOTION_KEY)
            && b != self.appearance.reduce_motion
        {
            self.appearance.reduce_motion = b;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, TEXT_SIZE_KEY)
            && let Some(scale) = text_size_scale(&label)
            && (scale - self.appearance.text_scale).abs() > f64::EPSILON
        {
            self.appearance.text_scale = scale;
            changed = true;
        }
        changed
    }

    // Pointer / typing / hearing fold into the accessibility section.
    fn fold_pointer_typing_hearing(&mut self, state: &AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        let mut adopt = |cur: &mut bool, v: Option<bool>| {
            if let Some(b) = v
                && b != *cur
            {
                *cur = b;
                changed = true;
            }
        };
        adopt(
            &mut self.access.large_cursor,
            read_bool(state, root, LARGE_CURSOR_KEY),
        );
        adopt(
            &mut self.access.sticky_keys,
            read_bool(state, root, STICKY_KEYS_KEY),
        );
        adopt(
            &mut self.access.slow_keys,
            read_bool(state, root, SLOW_KEYS_KEY),
        );
        adopt(
            &mut self.access.bounce_keys,
            read_bool(state, root, BOUNCE_KEYS_KEY),
        );
        adopt(
            &mut self.access.bounce_keys_alert,
            read_bool(state, root, BOUNCE_ALERT_KEY),
        );
        adopt(
            &mut self.access.mouse_keys,
            read_bool(state, root, MOUSE_KEYS_KEY),
        );
        adopt(
            &mut self.access.visual_alerts,
            read_bool(state, root, VISUAL_ALERTS_KEY),
        );
        adopt(
            &mut self.access.screen_reader,
            read_bool(state, root, SCREEN_READER_KEY),
        );
        adopt(
            &mut self.access.cursor_crosshair,
            read_bool(state, root, CURSOR_CROSSHAIR_KEY),
        );
        adopt(
            &mut self.access.thick_text_caret,
            read_bool(state, root, THICK_CARET_KEY),
        );
        adopt(
            &mut self.access.switch_control,
            read_bool(state, root, SWITCH_CONTROL_KEY),
        );
        if let Some(label) = read_str_or_text(state, root, SWITCH_KEYS_KEY)
            && let Some(pair) = SwitchKeyPair::from_label(&label)
            && pair != self.access.switch_key_pair
        {
            self.access.switch_key_pair = pair;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, SWITCH_SCAN_KEY) {
            let ms = match label.as_str() {
                "1 second" => 1000,
                "2 seconds" => 2000,
                "3 seconds" => 3000,
                _ => 0,
            };
            if ms != self.access.switch_scan_ms {
                self.access.switch_scan_ms = ms;
                changed = true;
            }
        }
        changed
    }

    fn fold_choice_settings(&mut self, state: &AutoCommit, root: &ObjId) -> bool {
        let mut changed = false;
        if let Some(label) = read_str_or_text(state, root, COLOR_FILTER_KEY)
            && let Some(filter) = ColorFilter::from_label(&label)
            && filter != self.access.color_filter
        {
            self.access.color_filter = filter;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, MAGNIFIER_VIEW_KEY)
            && let Some(view) = magnifier::MagnifierView::from_label(&label)
            && view != self.access.magnifier_view
        {
            self.access.magnifier_view = view;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, OSK_KEY)
            && let Some(mode) = OnScreenKeyboard::from_label(&label)
            && mode != self.access.on_screen_keyboard
        {
            self.access.on_screen_keyboard = mode;
            changed = true;
        }
        if let Some(label) = read_str_or_text(state, root, MAGNIFIER_KEY) {
            let zoom = magnifier::zoom_from_label(&label).unwrap_or(magnifier::OFF);
            if zoom != self.access.magnifier_zoom {
                self.access.magnifier_zoom = zoom;
                changed = true;
            }
        }
        if let Some(label) = read_str_or_text(state, root, READER_RATE_KEY) {
            let rate = match label.as_str() {
                "Slow" => 65,
                "Fast" => 140,
                "Very fast" => 185,
                _ => 100,
            };
            if rate != self.access.screen_reader_rate {
                self.access.screen_reader_rate = rate;
                changed = true;
            }
        }
        if let Some(label) = read_str_or_text(state, root, READER_VOICE_KEY)
            && let Some(voice) = ScreenReaderVoice::from_label(&label)
            && voice != self.access.screen_reader_voice
        {
            self.access.screen_reader_voice = voice;
            changed = true;
        }

        if let Some(label) = read_str_or_text(state, root, BOUNCE_DELAY_KEY)
            && let Some(ms) = AccessibilityPrefs::bounce_delay_ms(&label)
            && ms != self.access.bounce_keys_delay_ms
        {
            self.access.bounce_keys_delay_ms = ms;
            changed = true;
        }

        if let Some(label) = read_str_or_text(state, root, CURSOR_SIZE_KEY)
            && let Some(px) = AccessibilityPrefs::cursor_size_px(&label)
            && px != self.access.cursor_size
        {
            self.access.cursor_size = px;
            changed = true;
        }
        changed
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AccessibilityPane::new())
}

impl Widget for AccessibilityPane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.seed_if_needed(state, root);

        let mut changed = self.fold_vision(state, root);
        changed |= self.fold_pointer_typing_hearing(state, root);
        changed |= self.fold_choice_settings(state, root);

        if changed {
            self.persist();
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "AccessibilityPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for AccessibilityPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "accessibility",
            "Accessibility",
            "Change accessibility preferences",
        )
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Seeding writes current prefs, is not a user action or draft, never overwrites peer values, and lays out.
    #[test]
    fn accessibility_pane_seeds_without_user_actions_or_overwriting_peers() {
        // Case `seed_writes_current_prefs_into_crdt`.
        {
            let mut pane = AccessibilityPane::with_prefs(
                AppearancePrefs {
                    high_contrast: true,
                    text_scale: 1.15,
                    ..AppearancePrefs::default()
                },
                AccessibilityPrefs {
                    large_cursor: true,
                    cursor_size: 48,
                    screen_reader: true,
                    ..AccessibilityPrefs::default()
                },
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert_eq!(read_bool(&doc, &root, HIGH_CONTRAST_KEY), Some(true));
            assert_eq!(
                read_str_or_text(&doc, &root, TEXT_SIZE_KEY).as_deref(),
                Some("Large")
            );
            assert_eq!(read_bool(&doc, &root, LARGE_CURSOR_KEY), Some(true));
            assert_eq!(
                read_str_or_text(&doc, &root, CURSOR_SIZE_KEY).as_deref(),
                Some("Larger")
            );
            assert_eq!(read_bool(&doc, &root, SCREEN_READER_KEY), Some(true));
        }

        // Case `initial_hydration_is_not_a_user_action_or_a_replicated_draft`.
        {
            let mut pane = AccessibilityPane::with_prefs(
                AppearancePrefs {
                    high_contrast: true,
                    ..AppearancePrefs::default()
                },
                AccessibilityPrefs::default(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            assert!(
                ui::undo::take_commit_context(&mut pane).is_none(),
                "hydration is not undoable user intent"
            );
            for key in [
                HIGH_CONTRAST_KEY,
                TEXT_SIZE_KEY,
                LARGE_CURSOR_KEY,
                SWITCH_SCAN_KEY,
            ] {
                assert!(
                    doc.get(&root, format!("{key}.edit")).unwrap().is_none(),
                    "no presentation draft for {key} is replicated"
                );
            }
        }

        // Case `hydration_never_overwrites_a_value_already_received_from_a_peer`.
        {
            // The pane persists through the per-user settings file.
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            doc.put(
                &root,
                HIGH_CONTRAST_KEY,
                automerge::ScalarValue::Boolean(true),
            )
            .unwrap();
            doc.put(&root, CURSOR_SIZE_KEY, "Largest").unwrap();
            let mut pane = AccessibilityPane::with_prefs(
                AppearancePrefs::default(),
                AccessibilityPrefs::default(),
            );
            pane.sync(&mut doc, &root);

            assert_eq!(read_bool(&doc, &root, HIGH_CONTRAST_KEY), Some(true));
            assert_eq!(
                read_str_or_text(&doc, &root, CURSOR_SIZE_KEY).as_deref(),
                Some("Largest")
            );
            assert!(pane.appearance().high_contrast);
            assert_eq!(pane.access().cursor_size, 64);
        }

        // Case `builds_layout`.
        {
            let mut pane = build();
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }

    /// Vision edits fold into shared appearance; pointer/typing edits persist to the accessibility section.
    #[test]
    fn accessibility_edits_persist_to_their_sections() {
        // Case `vision_edit_folds_into_shared_appearance_and_persists`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = AccessibilityPane::with_prefs(
                AppearancePrefs::default(),
                AccessibilityPrefs::default(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Turn on high contrast + bump the text size here in Accessibility.
            let _ = doc.put(
                &root,
                HIGH_CONTRAST_KEY,
                automerge::ScalarValue::Boolean(true),
            );
            let _ = doc.put(&root, TEXT_SIZE_KEY, "Larger");
            pane.sync(&mut doc, &root);
            assert!(pane.appearance().high_contrast);
            assert_eq!(pane.appearance().text_scale, 1.30);
            // The shared appearance section is what got written — the Appearance
            // pane would load the same value.
            let loaded = SystemPrefs::load();
            assert!(
                loaded.appearance.high_contrast,
                "written to the shared section"
            );
            assert_eq!(loaded.appearance.text_scale, 1.30);
        }

        // Case `pointer_and_typing_edits_persist_to_accessibility_section`.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = AccessibilityPane::with_prefs(
                AppearancePrefs::default(),
                AccessibilityPrefs::default(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(
                &root,
                LARGE_CURSOR_KEY,
                automerge::ScalarValue::Boolean(true),
            );
            let _ = doc.put(&root, CURSOR_SIZE_KEY, "Largest");
            let _ = doc.put(
                &root,
                STICKY_KEYS_KEY,
                automerge::ScalarValue::Boolean(true),
            );
            let _ = doc.put(&root, OSK_KEY, "Always");
            pane.sync(&mut doc, &root);
            assert!(pane.access().large_cursor);
            assert_eq!(pane.access().cursor_size, 64);
            assert!(pane.access().sticky_keys);
            assert_eq!(pane.access().on_screen_keyboard, OnScreenKeyboard::Always);
            let loaded = SystemPrefs::load().accessibility;
            assert_eq!(
                loaded.cursor_size, 64,
                "persisted to the accessibility section"
            );
            assert_eq!(loaded.on_screen_keyboard, OnScreenKeyboard::Always);
        }
    }

    #[test]
    fn bounce_and_mouse_keys_edits_persist_to_accessibility_section() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = AccessibilityPane::with_prefs(
            AppearancePrefs::default(),
            AccessibilityPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, BOUNCE_DELAY_KEY).as_deref(),
            Some("300 ms"),
            "the default window is seeded"
        );
        for key in [BOUNCE_KEYS_KEY, BOUNCE_ALERT_KEY, MOUSE_KEYS_KEY] {
            let _ = doc.put(&root, key, automerge::ScalarValue::Boolean(true));
        }
        let _ = doc.put(&root, BOUNCE_DELAY_KEY, "500 ms");
        pane.sync(&mut doc, &root);
        let loaded = SystemPrefs::load().accessibility;
        assert!(loaded.bounce_keys);
        assert!(loaded.bounce_keys_alert);
        assert!(loaded.mouse_keys);
        assert_eq!(loaded.bounce_keys_delay_ms, 500);
    }

    #[test]
    fn magnifier_zoom_and_view_persist_where_the_compositor_reads_them() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = AccessibilityPane::with_prefs(
            AppearancePrefs::default(),
            AccessibilityPrefs {
                magnifier_zoom: 5,
                ..AccessibilityPrefs::default()
            },
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(
            read_str_or_text(&doc, &root, MAGNIFIER_KEY).as_deref(),
            Some("5×"),
            "a zoom set by shortcut is shown, not reset to off"
        );
        let _ = doc.put(&root, MAGNIFIER_KEY, "4×");
        let _ = doc.put(&root, MAGNIFIER_VIEW_KEY, "Full screen");
        pane.sync(&mut doc, &root);
        let path = settings_store::settings_path().expect("test config dir");
        let shown = magnifier::MagnifierState::from_section(&settings_store::load_section_at(
            &path,
            magnifier::SECTION,
        ));
        assert_eq!(
            shown,
            magnifier::MagnifierState {
                zoom: 4,
                view: magnifier::MagnifierView::FullScreen,
            }
        );
    }
}
