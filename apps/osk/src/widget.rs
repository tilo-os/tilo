//! [`OskWidget`] — the on-screen keyboard widget.
//!
//! Self-contained: it draws its own toolbar + keys (geometry from the pure
//! [`crate::layout::key_rects`] / [`crate::toolbar::toolbar_rects`] passes)
//! and hit-tests pointer taps itself, rather than composing dozens of
//! `Button`/`Row` widgets — the key set changes with the active layer and
//! shift state, so a custom draw + hit-test keeps the geometry honest and
//! the taffy tree a single node.
//!
//! ## Output
//! Every activation reduces to an ordered stream of [`OskOutput`]s: the
//! in-progress word is composed as preedit and committed at a word boundary;
//! editing keys (Backspace outside a word, Return) are keysyms. The host
//! receives them through [`OskWidget::on_output`] (or drains
//! [`OskWidget::take_output`]) and delivers them in order — see
//! [`crate::output`]. [`OskWidget::pending_text`] mirrors what the focused
//! field holds after that stream, for tests and diagnostics.
//!
//! ## Swipe-to-type + prediction (wired here)
//! The widget captures each pointer drag as a path. On release a short,
//! near-stationary path stays a normal **key tap** (existing behaviour); a
//! path that travelled across multiple keys is treated as a **swipe** and run
//! through the [`crate::swipe`] decoder. The top candidate is auto-inserted
//! (replacing the in-progress word); the ranked alternatives appear as
//! tappable chips in a **suggestion bar** above the keys. The swiped word stays
//! composing, so a chip replaces it in place; the next swipe or letter commits
//! it with a trailing space. After any word commits, [`crate::predict`] fills
//! the bar with next-word predictions; tapping a chip commits the word + a
//! space.
//!
//! ## Touch: typing with both thumbs
//! The key area claims raw touch ([`Widget::claims_raw_touch`]), so every
//! finger is tracked on its own instead of through the toolkit's single
//! emulated pointer. A key highlights the moment a finger lands on it and
//! types when that finger lifts. Rolling two-thumb input types in **press
//! order**, as iOS does: when a finger lands while another still holds a
//! key, the held key types at once and its later lift does nothing. So
//! A-down, B-down, A-up, B-up and A-down, B-down, B-up, A-up both type "ab",
//! with no key ever dropped or waiting on another finger. Only a finger that
//! stayed alone on the keys for its whole contact may swipe; once a second
//! finger lands, the first one's drag is a tap.
//!
//! ## Press feedback and holds
//! A character key previews its glyph in a raised bubble above the finger on
//! the same frame the finger lands (never in a secret field). Holding a
//! letter with accented forms for 350 ms opens them in a row above
//! the key; sliding picks one and lifting types it. Holding Backspace repeats
//! it after 400 ms. Both run off [`Widget::next_wake_deadline`], so
//! the event loop sleeps until the hold is due. Dragging along the space bar
//! commits the composing word and moves the caret one character per
//! quarter key of travel. The platform exposes no haptics interface yet,
//! so none of these vibrate.
//!
//! Tap resolution never moves a drawn key: the context only decides taps
//! outside a key's core zone (see [`crate::touch_model`]). `--debug-hit-areas`
//! paints the effective targets over the keys.
//!
//! ## Emoji
//! The emoji key opens the [`crate::emoji_panel`]: group tabs, a grid with
//! this user's recents first, and a search field the keys type into. A pick
//! commits through the same output stream as any key. While a word is
//! composing, words that name an emoji add emoji chips to the suggestion
//! strip. Secret fields get neither chips nor recents. Recents persist with
//! the other keyboard preferences in the keyboard's own per-user document.
//!
//! ## Preferences
//! The letter layout, suggestions, personalization and recent emoji are the
//! keyboard's [`crate::resource::Osk`] preferences. The widget shows what it
//! is [presented](OskWidget::present_preferences); a change it makes itself
//! (a layout key, an emoji pick) is queued as a
//! [`SetPreference`] for its host to submit
//! ([`OskWidget::take_preference_edits`]).
//!
//! ## Seams
//! * **Neural swipe:** the decoder is behind the
//!   [`crate::swipe::SwipeModel`] trait. The shipping model is the geometric
//!   [`crate::swipe::GeometricSwipeModel`]; a future FUTO/ExecuTorch
//!   `FutoSwipeModel` implementing the same trait can be swapped in via
//!   [`OskWidget::with_swipe_model`] with no other change.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent};

use crate::actions::SetPreference;
use crate::learned::{Learned, Lesson};
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::autocorrect::{Autocorrector, KeyAdjacency};
use crate::dict::Language;
use crate::emoji_panel::{EMOJI_SUGGESTIONS, EmojiPanel, PanelBands, PanelHit, PanelMode};
use crate::futo::FutoSwipeModel;
use crate::i18n;
use crate::imclient::{ImTransition, content_hint, may_learn};
use crate::inject::{Keypress, Symbol};
use crate::layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState};
use crate::output::{OskOutput, TextMirror};
use crate::predict::Predictor;
use crate::resource::KeyboardSize;
use crate::suggest::Suggestion;
use crate::swipe::{GeometricSwipeModel, SwipeModel};

pub fn default_swipe_model() -> Box<dyn SwipeModel> {
    if let Some(m) = FutoSwipeModel::default_load() {
        tracing::info!("osk: using FUTO ExecuTorch swipe decoder");
        Box::new(m)
    } else {
        tracing::info!("osk: FUTO unavailable — using geometric swipe decoder");
        Box::new(GeometricSwipeModel::english())
    }
}
use crate::presentation::{AlternatesPopup, KeyboardPresentation, keyboard_geometry};
use crate::toolbar::{Latch, ModifierKey, ModifierLatches, OskToolbarAction, OskToolbarItem};
use crate::touch_model::{HitCell, NextKeyCache, TouchKey, TouchModel};

/// A key row is one header-bar tall: well above the minimum touch target, and
/// the same code serves a phone (small surface) and a desktop (wide surface);
/// only the width differs.
pub fn key_row_height(metrics: &ui::tokens::Metrics) -> f32 {
    metrics.header_height
}
pub fn toolbar_row_height(spacing: &ui::tokens::Spacing) -> f32 {
    spacing.s12
}
pub fn suggestion_row_height(metrics: &ui::tokens::Metrics) -> f32 {
    metrics.bar_height
}
pub fn keyboard_height(
    rows: usize,
    spacing: &ui::tokens::Spacing,
    metrics: &ui::tokens::Metrics,
) -> f32 {
    toolbar_row_height(spacing)
        + suggestion_row_height(metrics)
        + rows as f32 * key_row_height(metrics)
}
pub fn keyboard_gutter(spacing: &ui::tokens::Spacing) -> f64 {
    f64::from(spacing.s2)
}
pub(crate) const MAX_SUGGESTIONS: usize = 5;
/// The diagnostic mirror is deliberately bounded: `on_output` is the delivery
/// path, while this buffer exists only for hosts/tests that inspect it.
pub const MAX_PENDING_TEXT_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_ENTRIES: usize = 1_024;
const MAX_KEY_LOG_ENTRIES: usize = 1_024;
const MAX_TOOLBAR_LOG_ENTRIES: usize = 128;
const MAX_GESTURE_POINTS: usize = 256;
const MAX_TRACKED_WORD_SCALARS: usize = 128;
type ContextHandler = Box<dyn FnMut(u32, u32) -> Result<(), String>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent, user-configurable interaction flags are clearer than a false state-machine invariant"
)]
pub struct OskWidget {
    layouts: HashMap<String, KeyboardLayout>,
    /// Name of the active *letter* layout (e.g. `"qwerty"`). For non-letter
    /// purposes (numeric/phone/pin) this holds the chosen specialised layout
    /// name; it is what `active_layout_name` reports when symbols are not shown.
    active_letter: String,
    preferred_text_layout: String,
    purpose: InputPurpose,
    hints: u32,
    sentence_start: bool,
    sentence_end: bool,
    showing_symbols: bool,
    shift: ShiftState,
    modifiers: ModifierLatches,
    toolbar: Vec<OskToolbarItem>,
    custom_toolbar: Option<Vec<OskToolbarItem>>,
    context_generation: Option<u32>,
    context_handler: Option<ContextHandler>,
    context_error: Option<String>,
    target_key_height: f32,
    toolbar_height: f32,
    suggestion_height: f32,
    suggestions_enabled: bool,
    autocorrect_enabled: bool,
    key_size: KeyboardSize,
    /// Whether the keyboard may adapt rankings and tap targets to this
    /// person's typing. What it learns is theirs alone: their host keeps it
    /// in the person's own record ([`crate::learned`]).
    personalization_enabled: bool,

    // --- frame caches (populated in apply_layout) ---
    rect: Rect,
    /// Centred responsive keyboard surface. On a wide desktop it caps the
    /// key span so each key remains comfortably scannable rather than becoming
    /// a very wide desktop tile; narrow/tablet surfaces retain full width.
    content_rect: Rect,
    key_geom: Vec<(KeyId, Rect)>,
    toolbar_geom: Vec<(usize, Rect)>,
    suggestion_geom: Vec<(usize, Rect)>,
    suggestion_area: Rect,
    key_area: Rect,

    // --- interaction state ---
    hovered_key: Option<KeyId>,
    pressed_key: Option<KeyId>,
    hovered_tool: Option<usize>,
    pressed_tool: Option<usize>,
    hovered_sugg: Option<usize>,
    pressed_sugg: Option<usize>,

    // --- swipe / prediction ---
    /// Decoder behind the [`SwipeModel`] seam (geometric today; FUTO later).
    swipe_model: Box<dyn SwipeModel>,
    predictor: Predictor,
    personal_vocabulary: Option<std::sync::Arc<dyn ui::personal_dictionary::Vocabulary>>,
    language: Language,
    typing_language: Option<Language>,
    suggestions: Vec<Suggestion>,
    /// The in-progress word at the caret (letters since the last word
    /// boundary). It is the field's preedit text: shown, not yet committed.
    current_word: String,
    word_from_swipe: bool,
    /// Once a word exceeds the predictor cap, do not begin tracking another
    /// suffix of that same word. It resets only at a real word boundary.
    current_word_overflowed: bool,
    prev_word: Option<String>,
    autocorrect: Option<Autocorrector>,
    last_correction: Option<Correction>,
    touch_model: TouchModel,
    next_key: NextKeyCache,
    /// Last pointer position over the widget (logical px).
    pointer: Option<Point>,
    /// While the left button is down, the captured pointer path (logical px).
    /// `Some` between press and release; `None` otherwise.
    gesture_path: Option<Vec<Point>>,
    /// Fingers currently down on the keys, in press order.
    fingers: Vec<Finger>,
    /// Geometry at contact start. Layout and painting only refresh caches;
    /// reject stale contacts when they act.
    contact_geometry: Option<Vec<(KeyId, Rect)>>,
    hold: Option<Hold>,
    accent_usage: Option<(Language, HashMap<char, f64>)>,
    alternates: Option<(Option<u64>, AlternatesPopup)>,
    cursor_drag: Option<CursorDrag>,
    hit_overlay: bool,

    // --- emoji ---
    emoji_panel: EmojiPanel,
    emoji_recents: emoji::Recents,
    /// The theme's spacing scale and metrics, captured at `build_layout`,
    /// for the emoji panel; and the whole theme, so the panel's search field
    /// places its controls in the type the frame measures with.
    spacing: ui::tokens::Spacing,
    metrics: ui::tokens::Metrics,
    layout_theme: Theme,

    // --- output ---
    mirror: TextMirror,
    output: VecDeque<OskOutput>,
    dropped_outputs: usize,
    key_log: VecDeque<KeyAction>,
    dropped_key_actions: usize,
    toolbar_log: VecDeque<OskToolbarAction>,
    dropped_toolbar_actions: usize,
    on_output: Option<Box<dyn FnMut(OskOutput)>>,
    on_toolbar: Option<Box<dyn FnMut(OskToolbarAction)>>,

    // --- dirty tracking ---
    /// Set when the displayed layer/layout changes so the host rebuilds the
    /// taffy node (its intrinsic height tracks the row count). Cleared by
    /// `take_structure_dirty`.
    structure_dirty: bool,
    preference_edits: Vec<SetPreference>,
    lesson: Lesson,
    forget_learned: bool,
    learned: std::rc::Rc<Learned>,

    /// The person put the keyboard away with its Dismiss, for the session to hear: it decides
    /// whether the keyboard belongs on screen, so hiding it here alone would not last the next
    /// reading.
    dismissed: bool,
    /// Whether the keyboard is on screen. This is not the keyboard's own
    /// choice — visibility belongs to the compositor, which shows the keyboard
    /// while a text input is focused and hides it otherwise, and relays that
    /// decision to the keyboard (and only the keyboard) over the input method.
    /// The keyboard is the one surface the OS floats and hides this way; an
    /// ordinary app has no equivalent. The default is `true` for the
    /// always-visible fallback used when no input method drives auto-show.
    /// When hidden the widget paints nothing.
    visible: bool,
    /// A session service fills its compositor-assigned panel; embedded
    /// keyboards keep their intrinsic height in ordinary document flow.
    fill_surface: bool,
    /// Parked: the keys are put away and the keyboard keeps a toolbar strip,
    /// so the panel it lives in shrinks to that strip and gives the rest of
    /// the screen back to the app the person is typing into. The keyboard is
    /// still on screen and still one tap from typing again.
    parked: bool,

    dirty: bool,
}

impl Default for OskWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl OskWidget {
    pub fn new() -> Self {
        Self::new_with_swipe_model(default_swipe_model())
    }

    pub fn new_with_swipe_model(swipe_model: Box<dyn SwipeModel>) -> Self {
        let mut layouts = HashMap::new();
        for l in [
            KeyboardLayout::qwerty(),
            KeyboardLayout::azerty(),
            KeyboardLayout::symbols(),
        ] {
            layouts.insert(l.name.clone(), l);
        }
        let mut me = Self {
            dirty: true,
            layouts,
            active_letter: "qwerty".to_string(),
            preferred_text_layout: "qwerty".to_string(),
            purpose: InputPurpose::Text,
            hints: content_hint::NONE,
            sentence_start: true,
            sentence_end: false,
            showing_symbols: false,
            shift: ShiftState::Off,
            modifiers: ModifierLatches::default(),
            toolbar: OskToolbarItem::default_set(),
            custom_toolbar: None,
            context_generation: None,
            context_handler: None,
            context_error: None,
            target_key_height: key_row_height(&ui::tokens::Metrics::BASE),
            toolbar_height: toolbar_row_height(&ui::tokens::Spacing::BASE),
            suggestion_height: suggestion_row_height(&ui::tokens::Metrics::BASE),
            suggestions_enabled: true,
            autocorrect_enabled: true,
            key_size: KeyboardSize::Standard,
            personalization_enabled: true,
            rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            key_geom: Vec::new(),
            toolbar_geom: Vec::new(),
            suggestion_geom: Vec::new(),
            suggestion_area: Rect::ZERO,
            key_area: Rect::ZERO,
            hovered_key: None,
            pressed_key: None,
            hovered_tool: None,
            pressed_tool: None,
            hovered_sugg: None,
            pressed_sugg: None,
            swipe_model,
            predictor: Predictor::english(),
            personal_vocabulary: None,
            language: Language::English,
            typing_language: None,
            suggestions: Vec::new(),
            current_word: String::new(),
            word_from_swipe: false,
            current_word_overflowed: false,
            prev_word: None,
            autocorrect: None,
            last_correction: None,
            touch_model: TouchModel::new(),
            next_key: NextKeyCache::default(),
            pointer: None,
            gesture_path: None,
            fingers: Vec::new(),
            contact_geometry: None,
            hold: None,
            accent_usage: None,
            alternates: None,
            cursor_drag: None,
            hit_overlay: false,
            emoji_panel: EmojiPanel::new(emoji::Catalogue::for_locale(
                &ui::i18n::I18nManager::current_locale(),
            )),
            emoji_recents: emoji::Recents::default(),
            spacing: ui::tokens::Spacing::BASE,
            metrics: ui::tokens::Metrics::BASE,
            layout_theme: Theme::default(),
            mirror: TextMirror::new(MAX_PENDING_TEXT_BYTES),
            output: VecDeque::new(),
            dropped_outputs: 0,
            key_log: VecDeque::new(),
            dropped_key_actions: 0,
            toolbar_log: VecDeque::new(),
            dropped_toolbar_actions: 0,
            on_output: None,
            on_toolbar: None,
            structure_dirty: false,
            preference_edits: Vec::new(),
            lesson: Lesson::default(),
            forget_learned: false,
            learned: std::rc::Rc::default(),
            visible: true,
            dismissed: false,
            fill_surface: false,
            parked: false,
        };
        me.refresh_next_word_suggestions();
        me
    }

    /// Use the host's keyboard panel, keeping themed row heights when it is
    /// taller than the keyboard and fitting shorter panels without overflow.
    #[must_use]
    pub fn with_surface_height(mut self) -> Self {
        self.fill_surface = true;
        self
    }

    #[must_use]
    pub fn with_layout(mut self, layout: KeyboardLayout) -> Self {
        self.layouts.insert(layout.name.clone(), layout);
        self
    }

    /// Choose the active letter layout by name (must be registered).
    #[must_use]
    pub fn with_active_layout(mut self, name: &str) -> Self {
        self.select_text_layout(name);
        self
    }

    #[must_use]
    pub fn with_purpose(mut self, purpose: InputPurpose) -> Self {
        self.apply_purpose(purpose);
        self
    }

    #[must_use]
    pub fn with_toolbar(mut self, items: Vec<OskToolbarItem>) -> Self {
        self.custom_toolbar = Some(items.clone());
        self.toolbar = items;
        self
    }

    pub(crate) fn set_context_handler(
        &mut self,
        handler: Box<dyn FnMut(u32, u32) -> Result<(), String>>,
    ) {
        self.context_handler = Some(handler);
    }

    pub(crate) fn set_context_toolbar(&mut self, generation: u32, items: Vec<OskToolbarItem>) {
        self.clear_context_toolbar();
        if !self.visible || self.is_secret_field() || items.is_empty() || items.len() > 6 {
            return;
        }
        if items.iter().any(|item| !matches!(item.action,
            OskToolbarAction::Contextual { generation: item_generation, .. } if item_generation == generation)) {
            return;
        }
        self.cancel_contacts();
        self.context_generation = Some(generation);
        self.toolbar = items;
        self.toolbar.push(OskToolbarItem::new(
            "dismiss",
            Some(ui::icons::Icon::ChevronDown),
            &crate::i18n::t("osk-toolbar-dismiss"),
            OskToolbarAction::Dismiss,
        ));
        self.structure_dirty = true;
    }

    pub(crate) fn clear_context_toolbar(&mut self) {
        if self.context_generation.take().is_none() {
            return;
        }
        self.cancel_contacts();
        self.context_error = None;
        self.toolbar_log
            .retain(|action| !matches!(action, OskToolbarAction::Contextual { .. }));
        self.restore_toolbar();
        self.structure_dirty = true;
    }

    pub(crate) fn context_result(
        &mut self,
        generation: u32,
        slot: u32,
        succeeded: bool,
        message: String,
    ) {
        if self.context_generation != Some(generation)
            || !self
                .toolbar
                .iter()
                .any(|item| item.action == OskToolbarAction::Contextual { generation, slot })
        {
            return;
        }
        self.context_error = (!succeeded).then_some(message);
        self.structure_dirty = true;
    }

    fn field_description(&self) -> String {
        self.context_error.clone().unwrap_or_else(|| {
            i18n::t(if self.suggestions_enabled {
                "osk-description"
            } else {
                "osk-description-no-suggestions"
            })
        })
    }

    fn restore_toolbar(&mut self) {
        if self.parked {
            // A parked keyboard keeps one strip: the way back to the keys.
            self.toolbar = OskToolbarItem::park_set();
            return;
        }
        self.toolbar = self.custom_toolbar.clone().unwrap_or_else(|| {
            if self.purpose == InputPurpose::Terminal {
                OskToolbarItem::terminal_set()
            } else {
                OskToolbarItem::default_set()
            }
        });
    }

    #[must_use]
    pub fn with_swipe_model(mut self, model: Box<dyn SwipeModel>) -> Self {
        self.swipe_model = model;
        self
    }

    #[must_use]
    pub fn with_predictor(mut self, predictor: Predictor) -> Self {
        self.predictor = predictor;
        self
    }

    /// Install or revoke the host's authenticated personal-vocabulary reader.
    /// The host calls this on account changes and refreshes on dictionary edits.
    /// This does not learn words or persist an OSK-specific vocabulary.
    pub fn set_personal_vocabulary(
        &mut self,
        source: Option<std::sync::Arc<dyn ui::personal_dictionary::Vocabulary>>,
    ) {
        self.personal_vocabulary = source;
        self.last_correction = None;
        self.refresh_completion_suggestions();
    }

    pub fn refresh_personal_vocabulary(&mut self) {
        self.refresh_completion_suggestions();
    }

    fn personal_words(&self) -> ui::personal_dictionary::Snapshot {
        self.personal_vocabulary
            .as_ref()
            .and_then(|source| source.snapshot().ok())
            .unwrap_or_default()
    }

    /// Receives every [`OskOutput`] in the order the field must apply it.
    #[must_use]
    pub fn on_output<F: FnMut(OskOutput) + 'static>(mut self, f: F) -> Self {
        self.on_output = Some(Box::new(f));
        self
    }

    pub fn set_output(&mut self, output: Box<dyn FnMut(OskOutput)>) {
        self.on_output = Some(output);
    }

    #[must_use]
    pub fn on_toolbar<F: FnMut(OskToolbarAction) + 'static>(mut self, f: F) -> Self {
        self.on_toolbar = Some(Box::new(f));
        self
    }

    pub fn pending_text(&self) -> &str {
        self.mirror.text()
    }

    pub fn take_output(&mut self) -> Vec<OskOutput> {
        self.output.drain(..).collect()
    }

    /// Number of oldest outputs evicted from the bounded [`take_output`]
    /// queue. `on_output` still received them.
    ///
    /// [`take_output`]: Self::take_output
    pub fn dropped_outputs(&self) -> usize {
        self.dropped_outputs
    }

    pub fn take_key_actions(&mut self) -> Vec<KeyAction> {
        self.key_log.drain(..).collect()
    }

    pub fn take_toolbar_actions(&mut self) -> Vec<OskToolbarAction> {
        self.toolbar_log.drain(..).collect()
    }

    pub fn take_dismissed(&mut self) -> bool {
        std::mem::take(&mut self.dismissed)
    }

    /// Bytes that could not fit in the bounded diagnostic text mirror. Live
    /// delivery still reached `on_output`.
    pub fn pending_text_dropped_bytes(&self) -> usize {
        self.mirror.dropped_bytes()
    }

    /// Number of oldest raw key events evicted from the bounded diagnostic log.
    pub fn dropped_key_actions(&self) -> usize {
        self.dropped_key_actions
    }

    /// Number of oldest toolbar events evicted from the bounded diagnostic log.
    pub fn dropped_toolbar_actions(&self) -> usize {
        self.dropped_toolbar_actions
    }

    pub fn suggestions_enabled(&self) -> bool {
        self.suggestions_enabled
    }

    pub fn personalization_enabled(&self) -> bool {
        self.personalization_enabled
    }

    pub fn set_suggestions_enabled(&mut self, enabled: bool) {
        if self.suggestions_enabled == enabled {
            return;
        }
        self.suggestions_enabled = enabled;
        if enabled {
            self.refresh_next_word_suggestions();
        } else {
            self.suggestions.clear();
        }
    }

    pub fn set_personalization_enabled(&mut self, enabled: bool) {
        if self.personalization_enabled == enabled {
            return;
        }
        self.personalization_enabled = enabled;
        if enabled {
            self.adopt_learned();
        } else {
            self.predictor.clear_learned();
            self.touch_model.clear();
            self.emoji_recents = emoji::Recents::default();
            self.lesson = Lesson::default();
        }
        self.refresh_next_word_suggestions();
    }

    pub fn clear_personalization(&mut self) {
        self.predictor.clear_learned();
        self.touch_model.clear();
        if let Some(ac) = &mut self.autocorrect {
            ac.clear_kept();
        }
        self.lesson = Lesson::default();
        self.learned = std::rc::Rc::default();
        self.forget_learned = true;
        if !self.emoji_recents.is_empty() {
            self.emoji_recents = emoji::Recents::default();
            self.stage_emoji_recents();
        }
    }

    /// Switch the keyboard to the layout for `purpose` at runtime (the focused
    /// field changed). Registers the layout if absent, makes it active, drops
    /// the symbols layer, and raises `structure_dirty` so the host relays out
    /// (the row count differs between, say, the dialpad and QWERTY).
    pub fn set_purpose(&mut self, purpose: InputPurpose) {
        self.clear_context_toolbar();
        self.cancel_contacts();
        self.close_emoji_panel();
        self.apply_purpose(purpose);
        self.showing_symbols = false;
        self.structure_dirty = true;
        self.enforce_secrecy();
        self.apply_hint_case();
    }

    pub fn set_content_hints(&mut self, hints: u32) {
        self.hints = hints;
        if self.is_secret_field() {
            self.clear_context_toolbar();
        }
        self.enforce_secrecy();
        self.apply_hint_case();
    }

    pub fn is_secret_field(&self) -> bool {
        !may_learn(self.purpose, self.hints)
    }

    fn enforce_secrecy(&mut self) {
        if !self.is_secret_field() {
            return;
        }
        if !self.current_word.is_empty() {
            self.current_word.clear();
            self.emit(OskOutput::SetPreedit(String::new()));
        }
        self.word_from_swipe = false;
        self.current_word_overflowed = false;
        self.prev_word = None;
        self.suggestions.clear();
    }

    fn apply_hint_case(&mut self) {
        if self.hints & content_hint::UPPERCASE != 0 {
            self.shift = ShiftState::CapsLock;
        } else if self.hints & content_hint::LOWERCASE != 0 {
            self.shift = ShiftState::Off;
        } else {
            self.auto_capitalize();
        }
    }

    fn auto_capitalize(&mut self) {
        let wanted = self.hints & content_hint::AUTO_CAPITALIZATION != 0
            && self.hints & content_hint::LOWERCASE == 0;
        if wanted
            && self.sentence_start
            && self.current_word.is_empty()
            && self.shift == ShiftState::Off
            && !self.is_secret_field()
        {
            self.shift = ShiftState::Shift;
        }
    }

    /// Start hidden (builder). Used by the auto-show input-method mode, where
    /// the compositor only reveals the keyboard once a text field is focused.
    #[must_use]
    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    /// Apply the compositor's show/hide decision at runtime. This is set from
    /// the input-method activate/deactivate the compositor sends only to the
    /// keyboard, never from the keyboard's own logic. Raises `structure_dirty`
    /// on a change so the host rebuilds the taffy node. A no-op when already in
    /// the requested state (so it doesn't churn relayout on repeated
    /// activations).
    pub fn set_visible(&mut self, visible: bool) {
        if !visible {
            self.clear_context_toolbar();
        }
        if self.visible != visible {
            self.visible = visible;
            self.structure_dirty = true;
            self.cancel_contacts();
            self.close_emoji_panel();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Put the keys away, keeping the toolbar strip: the panel the keyboard
    /// lives in shrinks to it, so the app behind gets the screen back, and one
    /// tap on the strip brings the keys up again.
    pub fn set_parked(&mut self, parked: bool) {
        if self.parked != parked {
            self.parked = parked;
            self.structure_dirty = true;
            self.cancel_contacts();
            self.close_emoji_panel();
            self.restore_toolbar();
        }
    }

    pub fn is_parked(&self) -> bool {
        self.parked
    }

    /// Whether the keys are up: on screen and not parked.
    pub fn keys_up(&self) -> bool {
        self.visible && !self.parked
    }

    pub fn purpose(&self) -> InputPurpose {
        self.purpose
    }

    pub fn shift_state(&self) -> ShiftState {
        self.shift
    }

    pub fn showing_symbols(&self) -> bool {
        self.showing_symbols
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn active_layout_name(&self) -> &str {
        if self.showing_symbols {
            "symbols"
        } else {
            &self.active_letter
        }
    }

    pub fn key_geometry(&self) -> &[(KeyId, Rect)] {
        &self.key_geom
    }

    pub fn toolbar_items(&self) -> &[OskToolbarItem] {
        &self.toolbar
    }

    pub fn toolbar_geometry(&self) -> &[(usize, Rect)] {
        &self.toolbar_geom
    }

    pub fn suggestion_geometry(&self) -> &[(usize, Rect)] {
        &self.suggestion_geom
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    pub fn current_word(&self) -> &str {
        &self.current_word
    }

    pub fn preferred_text_layout(&self) -> &str {
        &self.preferred_text_layout
    }

    pub fn present_preferences(&mut self, preferences: &crate::resource::OskPreferences) {
        self.present_layout_order(preferences);
        self.select_text_layout_from_preference(&preferences.layout);
        self.set_typing_language(preferences.typing_language.as_deref());
        self.set_suggestions_enabled(preferences.suggestions);
        self.set_autocorrect_enabled(preferences.autocorrect);
        self.set_key_size(preferences.size);
        self.set_personalization_enabled(preferences.personalization);
        self.set_emoji_recents(&preferences.emoji_recents);
    }

    /// Layout keys follow configured order; an old profile keeps the historic choices.
    fn present_layout_order(&mut self, preferences: &crate::resource::OskPreferences) {
        let mut order: Vec<String> = if preferences.layouts.is_empty() {
            crate::actions::LAYOUT_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        } else {
            preferences
                .layouts
                .iter()
                .filter(|name| crate::actions::LAYOUT_NAMES.contains(&name.as_str()))
                .cloned()
                .collect()
        };
        let mut seen = std::collections::HashSet::new();
        order.retain(|name| seen.insert(name.clone()));
        if crate::actions::LAYOUT_NAMES.contains(&preferences.layout.as_str())
            && !order.contains(&preferences.layout)
        {
            order.push(preferences.layout.clone());
        }
        let mut changed = false;
        for layout in self.layouts.values_mut() {
            if !crate::actions::LAYOUT_NAMES.contains(&layout.name.as_str()) {
                continue;
            }
            let next = order
                .iter()
                .position(|name| name == &layout.name)
                .and_then(|index| index.checked_add(1))
                .and_then(|index| order.get(index).or_else(|| order.first()))
                .or_else(|| order.first());
            let Some(next) = next else {
                continue;
            };
            for key in layout.rows.iter_mut().flatten() {
                if matches!(key.action, KeyAction::Layout(_)) {
                    changed |= key.action != KeyAction::Layout(next.clone());
                    key.action = KeyAction::Layout(next.clone());
                    key.label = match next.as_str() {
                        "qwerty" => "EN".into(),
                        "azerty" => "FR".into(),
                        _ => key.label.clone(),
                    };
                }
            }
        }
        if changed {
            self.cancel_contacts();
        }
    }

    /// The preference changes the keyboard made itself since the last call
    /// (a layout key, an emoji pick), in order, for its host to submit.
    pub fn take_preference_edits(&mut self) -> Vec<SetPreference> {
        std::mem::take(&mut self.preference_edits)
    }

    pub fn present_learned(&mut self, learned: std::rc::Rc<Learned>) {
        self.learned = learned;
        if self.personalization_enabled {
            self.adopt_learned();
        }
    }

    pub fn take_lesson(&mut self) -> Option<Lesson> {
        if self.lesson.is_empty() {
            return None;
        }
        let mut lesson = std::mem::take(&mut self.lesson);
        self.language.code().clone_into(&mut lesson.language);
        Some(lesson)
    }

    pub fn take_forget_learned(&mut self) -> bool {
        std::mem::take(&mut self.forget_learned)
    }

    fn adopt_learned(&mut self) {
        let learned = std::rc::Rc::clone(&self.learned);
        if let Some(transitions) = learned.transitions.get(self.language.code()) {
            self.predictor.adopt_learned(transitions);
        }
        self.touch_model.adopt(&learned.touch_offsets());
        if let Some(ac) = &mut self.autocorrect {
            ac.adopt_kept(learned.kept.iter());
        }
    }

    /// Follow what the compositor said about the focused text field over the
    /// input method: a new field drops the composition that belonged to the
    /// previous one, its purpose picks the layout, its hints decide secrecy
    /// and capitalization, and the keyboard is shown only while a field is
    /// focused. The compositor relays this to the keyboard alone.
    pub fn follow_input_method(&mut self, transition: ImTransition) {
        if transition.field_changed {
            self.clear_context_toolbar();
            self.reset_composition();
        }
        // Purpose first, so the first mapped frame already has the right
        // layout; hints after it, since a purpose change resets shift.
        if let Some(purpose) = transition.purpose_changed {
            self.set_purpose(purpose);
        }
        if let Some(hints) = transition.hints_changed {
            self.set_content_hints(hints);
        }
        if let Some(visible) = transition.visibility_changed {
            self.set_visible(visible);
        }
    }

    pub fn activate_key(&mut self, id: KeyId) {
        let action = match self.current_layout().key(id) {
            Some(k) => k.emit_action(self.shift),
            None => return,
        };
        if self.emoji_panel.is_searching() {
            self.apply_search_action(action);
        } else {
            self.apply_action(action);
        }
    }

    pub fn activate_label(&mut self, label: &str) -> bool {
        let id = {
            let layout = self.current_layout();
            let mut found = None;
            'outer: for (r, row) in layout.rows.iter().enumerate() {
                for (c, key) in row.iter().enumerate() {
                    if key.label == label {
                        found = Some(KeyId::new(r, c));
                        break 'outer;
                    }
                }
            }
            found
        };
        match id {
            Some(id) => {
                self.activate_key(id);
                true
            }
            None => false,
        }
    }

    /// Feed a gesture path directly to the swipe decoder (host/test helper,
    /// bypassing the pointer event capture). Returns whether it decoded to a
    /// word. The path must be in the same coordinate space as the laid-out
    /// keys (see [`OskWidget::key_geometry`]).
    pub fn swipe(&mut self, path: &[Point]) -> bool {
        if path.len() < 2 {
            return false;
        }
        self.decode_and_apply_swipe(path)
    }

    pub fn activate_tool(&mut self, index: usize) {
        let action = match self.toolbar.get(index) {
            Some(it) => it.action.clone(),
            None => return,
        };
        if self.toolbar_log.len() == MAX_TOOLBAR_LOG_ENTRIES {
            self.toolbar_log.pop_front();
            self.dropped_toolbar_actions = self.dropped_toolbar_actions.saturating_add(1);
        }
        self.toolbar_log.push_back(action.clone());
        if matches!(action, OskToolbarAction::Contextual { .. }) {
            self.activate_default_tool(&action);
        } else if let Some(cb) = &mut self.on_toolbar {
            cb(action);
        } else {
            self.activate_default_tool(&action);
        }
    }

    fn activate_default_tool(&mut self, action: &OskToolbarAction) {
        match action {
            OskToolbarAction::Contextual { generation, slot } => {
                let admitted = self.visible
                    && !self.is_secret_field()
                    && self.context_generation == Some(*generation);
                self.context_error = if admitted {
                    self.context_handler.as_mut().map_or_else(
                        || Some(crate::i18n::t("osk-context-unavailable")),
                        |handler| handler(*generation, *slot).err(),
                    )
                } else {
                    Some(crate::i18n::t("osk-context-stale"))
                };
            }
            OskToolbarAction::Key(symbol) => {
                self.finish_editing_word();
                self.press_key(*symbol);
            }
            OskToolbarAction::Modifier(key) => self.modifiers.tap(*key),
            OskToolbarAction::Paste => {
                if let Some(text) = ui::clipboard_paste() {
                    self.finish_editing_word();
                    // Clipboard text is literal and never training input.
                    self.emit(OskOutput::Commit(text));
                }
            }
            OskToolbarAction::SelectionLeft | OskToolbarAction::SelectionRight => {
                self.finish_editing_word();
                self.emit(OskOutput::key(
                    if *action == OskToolbarAction::SelectionLeft {
                        Symbol::Left
                    } else {
                        Symbol::Right
                    },
                ));
            }
            OskToolbarAction::Dismiss => {
                self.finish_editing_word();
                self.set_visible(false);
                // The session decides whether the keyboard belongs on screen: it hears this and
                // stops saying it does, so the next reading does not bring it back.
                self.dismissed = true;
            }
            OskToolbarAction::Park => {
                self.finish_editing_word();
                self.set_parked(true);
                self.restore_toolbar();
            }
            OskToolbarAction::Unpark => {
                self.set_parked(false);
                self.restore_toolbar();
            }
            OskToolbarAction::Custom(_) => {}
        }
    }

    fn press_key(&mut self, symbol: Symbol) {
        let modifiers = self.modifiers.held();
        self.modifiers.spend();
        self.emit(OskOutput::Key(Keypress { symbol, modifiers }));
    }

    fn spoken_tool(&self, item: &OskToolbarItem) -> String {
        let OskToolbarAction::Modifier(key) = item.action else {
            return item.label.clone();
        };
        match (key, self.modifiers.get(key)) {
            (_, Latch::Off) => item.label.clone(),
            (ModifierKey::Ctrl, Latch::Once) => i18n::t("osk-key-ctrl-latched"),
            (ModifierKey::Ctrl, Latch::Locked) => i18n::t("osk-key-ctrl-locked"),
            (ModifierKey::Alt, Latch::Once) => i18n::t("osk-key-alt-latched"),
            (ModifierKey::Alt, Latch::Locked) => i18n::t("osk-key-alt-locked"),
        }
    }

    #[must_use]
    pub fn modifier_latches(&self) -> ModifierLatches {
        self.modifiers
    }

    /// With Ctrl or Alt held, a key from the letters is a chord: the composing
    /// word is finished and the key is pressed with the modifiers, never typed
    /// as text. Returns whether `action` was pressed that way.
    fn press_chord(&mut self, action: &KeyAction) -> bool {
        if self.modifiers.held().is_empty() {
            return false;
        }
        let symbol = match action {
            KeyAction::Char(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Symbol::Unicode(c),
                    _ => return false,
                }
            }
            KeyAction::Space => Symbol::Space,
            KeyAction::Enter => Symbol::Return,
            KeyAction::Backspace => Symbol::BackSpace,
            _ => return false,
        };
        if self.shift == ShiftState::Shift {
            self.shift = ShiftState::Off;
        }
        self.finish_editing_word();
        self.press_key(symbol);
        true
    }

    fn finish_editing_word(&mut self) {
        self.last_correction = None;
        self.finish_word_with("", false);
        self.prev_word = None;
        self.suggestions.clear();
    }

    /// Make `purpose` the active intent: pick its layout name, register that
    /// layout on demand, set it active, and reset shift (a fresh numeric/pin
    /// pad should not carry a stale upper-case state). Shared by the builder
    /// [`with_purpose`](Self::with_purpose) and the runtime
    /// [`set_purpose`](Self::set_purpose).
    fn apply_purpose(&mut self, purpose: InputPurpose) {
        self.purpose = purpose;
        self.modifiers = ModifierLatches::default();
        self.restore_toolbar();
        let name = if matches!(purpose, InputPurpose::Text | InputPurpose::Terminal) {
            self.preferred_text_layout.as_str()
        } else {
            KeyboardLayout::for_purpose(purpose)
        };
        if !self.layouts.contains_key(name)
            && let Some(layout) = KeyboardLayout::by_name(name)
        {
            self.layouts.insert(layout.name.clone(), layout);
        }
        if self.layouts.contains_key(name) {
            self.active_letter = name.to_string();
        }
        self.shift = ShiftState::Off;
    }

    fn select_text_layout(&mut self, name: &str) -> bool {
        if name == "symbols" || !self.layouts.contains_key(name) {
            return false;
        }
        if self.active_layout_name() != name {
            self.cancel_contacts();
        }
        self.preferred_text_layout = name.to_string();
        self.active_letter = name.to_string();
        if self.purpose != InputPurpose::Terminal {
            self.purpose = InputPurpose::Text;
        }
        self.showing_symbols = false;
        self.shift = ShiftState::Off;
        self.structure_dirty = true;
        self.preference_edits
            .push(SetPreference::Layout(name.to_owned()));
        self.follow_layout_language();
        true
    }

    fn select_text_layout_from_preference(&mut self, name: &str) {
        if name == "symbols"
            || !self.layouts.contains_key(name)
            || self.preferred_text_layout == name
        {
            return;
        }
        self.cancel_contacts();
        self.preferred_text_layout = name.to_string();
        if matches!(self.purpose, InputPurpose::Text | InputPurpose::Terminal) {
            self.active_letter = name.to_string();
            self.showing_symbols = false;
            self.shift = ShiftState::Off;
            self.structure_dirty = true;
        }
        self.follow_layout_language();
    }

    /// The layout currently on screen.
    ///
    /// # Panics
    /// Never in practice: `layouts` always holds at least the symbols layer
    /// plus one letter layout (see the field doc on `OskWidget::layouts`),
    /// so the last-resort fallback below always has something to return.
    #[allow(
        clippy::unreachable,
        reason = "layouts is guaranteed non-empty by construction; see this fn's # Panics doc"
    )]
    fn current_layout(&self) -> &KeyboardLayout {
        let name = self.active_layout_name();
        self.layouts
            .get(name)
            .or_else(|| self.layouts.get(&self.active_letter))
            .or_else(|| self.layouts.values().next())
            .unwrap_or_else(|| unreachable!("OskWidget always registers at least one layout"))
    }

    fn apply_action(&mut self, action: KeyAction) {
        let correction = self.last_correction.take();
        if self.press_chord(&action) {
            self.log_key_action(action);
            return;
        }
        match &action {
            KeyAction::Char(s) => {
                if self.shift == ShiftState::Shift {
                    self.shift = ShiftState::Off;
                }
                if (!s.is_empty() && s.chars().all(char::is_alphabetic))
                    || self.apostrophe_continues_word(s)
                {
                    self.extend_word(&s.replace('\u{2019}', "'"));
                } else {
                    self.finish_word(s);
                }
            }
            KeyAction::Space => self.finish_word(" "),
            KeyAction::Enter => {
                self.finish_word("");
                self.emit(OskOutput::key(Symbol::Return));
                self.prev_word = None;
                self.suggestions.clear();
            }
            KeyAction::Backspace => match correction {
                Some(c) => self.revert_correction(c),
                None => self.backspace(),
            },
            KeyAction::Shift => {
                self.shift = self.shift.cycle();
                self.last_correction = correction;
            }
            KeyAction::SymbolsToggle => {
                self.showing_symbols = !self.showing_symbols;
                self.structure_dirty = true;
                self.last_correction = correction;
            }
            KeyAction::Layout(name) => {
                if name != &self.preferred_text_layout || name != self.active_layout_name() {
                    self.select_text_layout(name);
                }
            }
            KeyAction::Emoji => {
                self.open_emoji_panel();
                self.last_correction = correction;
            }
        }
        if matches!(
            action,
            KeyAction::Char(_) | KeyAction::Space | KeyAction::Enter | KeyAction::Backspace
        ) {
            self.auto_capitalize();
        }
        self.log_key_action(action);
    }

    fn log_key_action(&mut self, action: KeyAction) {
        if self.key_log.len() == MAX_KEY_LOG_ENTRIES {
            self.key_log.pop_front();
            self.dropped_key_actions = self.dropped_key_actions.saturating_add(1);
        }
        self.key_log.push_back(action);
    }

    /// Send one output to the field (callback, bounded queue, mirror).
    fn emit(&mut self, output: OskOutput) {
        self.track_sentence(&output);
        self.mirror.apply(&output);
        if self.output.len() == MAX_OUTPUT_ENTRIES {
            self.output.pop_front();
            self.dropped_outputs = self.dropped_outputs.saturating_add(1);
        }
        self.output.push_back(output.clone());
        if let Some(cb) = &mut self.on_output {
            cb(output);
        }
    }

    fn track_sentence(&mut self, output: &OskOutput) {
        match output {
            OskOutput::SetPreedit(_) => {}
            OskOutput::Commit(text) => {
                for c in text.chars() {
                    match c {
                        '.' | '!' | '?' => {
                            self.sentence_end = true;
                            self.sentence_start = false;
                        }
                        ' ' => self.sentence_start |= self.sentence_end,
                        '\n' => {
                            self.sentence_start = true;
                            self.sentence_end = false;
                        }
                        _ => {
                            self.sentence_start = false;
                            self.sentence_end = false;
                        }
                    }
                }
            }
            OskOutput::Key(Keypress {
                symbol: Symbol::Return,
                modifiers,
            }) if modifiers.is_empty() => {
                self.sentence_start = true;
                self.sentence_end = false;
            }
            OskOutput::Key(_) => {
                self.sentence_start = false;
                self.sentence_end = false;
            }
        }
    }

    /// Add typed letters to the composing word. A swiped word in front of them
    /// is committed first with a separating space. In a secret field letters
    /// are committed as typed, never composed.
    fn extend_word(&mut self, letters: &str) {
        if self.word_from_swipe {
            self.finish_word(" ");
        }
        if self.is_secret_field() {
            self.emit(OskOutput::Commit(letters.to_string()));
            return;
        }
        let fits = self
            .current_word
            .chars()
            .count()
            .saturating_add(letters.chars().count())
            <= MAX_TRACKED_WORD_SCALARS;
        if !self.current_word_overflowed && fits {
            self.current_word.push_str(letters);
            self.emit(OskOutput::SetPreedit(self.current_word.clone()));
            self.refresh_completion_suggestions();
        } else {
            // Keep delivery intact but stop composing (and predicting from) a
            // hostile or accidental mega-token.
            let mut text = std::mem::take(&mut self.current_word);
            text.push_str(letters);
            self.emit(OskOutput::Commit(text));
            self.current_word_overflowed = true;
            self.suggestions.clear();
        }
    }

    fn finish_word(&mut self, tail: &str) {
        self.finish_word_with(tail, true);
    }

    /// [`finish_word`](Self::finish_word), autocorrecting the word only when
    /// `may_correct`.
    fn finish_word_with(&mut self, tail: &str, may_correct: bool) {
        let mut word = std::mem::take(&mut self.current_word);
        let typed = (may_correct && !tail.is_empty() && !self.word_from_swipe)
            .then(|| self.correction_for(&word))
            .flatten()
            .map(|fixed| std::mem::replace(&mut word, fixed));
        if !word.is_empty() || !tail.is_empty() {
            let mut text = word.clone();
            text.push_str(tail);
            self.emit(OskOutput::Commit(text));
        }
        self.word_from_swipe = false;
        self.current_word_overflowed = false;
        if !word.is_empty() && !self.is_secret_field() {
            let committed = word.to_lowercase();
            let pieces = match self.predictor.split_elision(&committed) {
                Some((elision, rest)) if !rest.is_empty() => {
                    vec![elision.to_owned(), rest.to_owned()]
                }
                _ => vec![committed],
            };
            for piece in pieces {
                if self.personalization_enabled
                    && let Some(learned) = self
                        .predictor
                        .learn_transition(self.prev_word.as_deref(), &piece)
                {
                    self.lesson.transitions.push(learned);
                }
                self.prev_word = Some(piece);
            }
        }
        self.refresh_next_word_suggestions();
        if let Some(typed) = typed {
            self.suggestions
                .insert(0, Suggestion::new(format!("\u{201c}{typed}\u{201d}"), 1.0));
            self.suggestions.truncate(MAX_SUGGESTIONS);
            self.last_correction = Some(Correction {
                typed,
                corrected: word,
                tail: tail.to_string(),
            });
        }
    }

    fn apostrophe_continues_word(&self, s: &str) -> bool {
        if !matches!(s, "'" | "\u{2019}") || self.current_word.is_empty() || self.word_from_swipe {
            return false;
        }
        let stem = format!("{}'", self.current_word.to_lowercase());
        self.predictor
            .dictionary()
            .iter_prefix(&stem)
            .next()
            .is_some()
    }

    fn autocorrect_allowed(&self) -> bool {
        self.purpose == InputPurpose::Text && self.autocorrect_enabled && !self.is_secret_field()
    }

    fn correction_for(&mut self, word: &str) -> Option<String> {
        if word.is_empty()
            || !self.autocorrect_allowed()
            || self.personal_words().contains(word, self.language.code())
            || self.predictor.has_learned(word)
        {
            return None;
        }
        // After an elision only the word itself is corrected: "l'hommr" ->
        // "l'homme".
        if let Some((elision, rest)) = self.predictor.split_elision(word) {
            let (elision, rest) = (elision.to_owned(), rest.to_owned());
            return (!rest.is_empty())
                .then(|| self.correction_for(&rest))
                .flatten()
                .map(|fixed| format!("{elision}{fixed}"));
        }
        let adjacency = KeyAdjacency::from_layout(self.current_layout());
        let dict = self.predictor.dictionary();
        let learned = &self.learned;
        let ac = self.autocorrect.get_or_insert_with(|| {
            let mut ac = Autocorrector::new(dict, KeyAdjacency::default());
            ac.adopt_kept(learned.kept.iter());
            ac
        });
        ac.set_adjacency(adjacency);
        ac.correct(word)
    }

    /// Undo a just-committed correction: erase it, commit the word as typed
    /// and never correct that word again.
    fn revert_correction(&mut self, c: Correction) {
        for _ in c.corrected.chars().chain(c.tail.chars()) {
            self.emit(OskOutput::key(Symbol::BackSpace));
        }
        if let Some(ac) = &mut self.autocorrect {
            ac.keep(&c.typed);
        }
        if self.personalization_enabled && !self.is_secret_field() {
            self.lesson.kept.push(c.typed.to_lowercase());
        }
        self.prev_word = Some(c.typed.to_lowercase());
        let mut text = c.typed;
        text.push_str(&c.tail);
        self.emit(OskOutput::Commit(text));
        self.refresh_next_word_suggestions();
    }

    fn backspace(&mut self) {
        if self.current_word.is_empty() {
            self.emit(OskOutput::key(Symbol::BackSpace));
            self.prev_word = None;
            self.suggestions.clear();
            return;
        }
        if self.word_from_swipe {
            self.current_word.clear();
            self.word_from_swipe = false;
        } else {
            self.current_word.pop();
        }
        self.emit(OskOutput::SetPreedit(self.current_word.clone()));
        self.refresh_completion_suggestions();
    }

    /// Drop the composition, next-word context and every in-memory trace of
    /// typed text (mirror, output queue, key log). The focused field changed
    /// (or went away), so none of it belongs to what the user edits next — and
    /// a password typed there must not outlive the field.
    pub fn reset_composition(&mut self) {
        self.close_emoji_panel();
        self.cancel_contacts();
        self.last_correction = None;
        self.current_word.clear();
        self.word_from_swipe = false;
        self.current_word_overflowed = false;
        self.prev_word = None;
        self.mirror.clear();
        self.output.clear();
        self.key_log.clear();
        self.sentence_start = true;
        self.sentence_end = false;
        self.shift = ShiftState::Off;
        self.modifiers = ModifierLatches::default();
        self.refresh_next_word_suggestions();
        self.apply_hint_case();
    }

    /// Cancel input that belonged to a previous field, layout or key geometry. A
    /// later release must not type, decode a swipe or restart an editing hold.
    fn cancel_contacts(&mut self) {
        self.fingers.clear();
        self.contact_geometry = None;
        self.gesture_path = None;
        self.hold = None;
        self.alternates = None;
        self.cursor_drag = None;
        self.pointer = None;
        self.pressed_key = None;
        self.pressed_sugg = None;
        self.pressed_tool = None;
        self.hovered_key = None;
        self.hovered_sugg = None;
        self.hovered_tool = None;
    }

    fn cancel_stale_contacts(&mut self) {
        if self
            .contact_geometry
            .as_ref()
            .is_some_and(|geometry| *geometry != self.key_geom)
        {
            self.cancel_contacts();
        }
    }

    fn refresh_completion_suggestions(&mut self) {
        if !self.suggestions_enabled || self.is_secret_field() {
            self.suggestions.clear();
            return;
        }
        if self.current_word.is_empty() {
            self.refresh_next_word_suggestions();
            return;
        }
        let preds = self.predictor.predict_next_n(
            self.prev_word.as_deref(),
            &self.current_word,
            MAX_SUGGESTIONS,
        );
        self.suggestions = preds
            .into_iter()
            .map(|(w, s)| Suggestion::new(w, s))
            .collect();
        let words = self.personal_words();
        let personal = words.completions(&self.current_word, self.language.code(), MAX_SUGGESTIONS);
        self.suggestions
            .retain(|suggestion| !words.contains(&suggestion.word, self.language.code()));
        for word in personal.into_iter().rev() {
            self.suggestions.insert(0, Suggestion::personal(word));
        }
        self.suggestions.truncate(MAX_SUGGESTIONS);
        self.add_emoji_suggestions();
    }

    /// Add chips for the emoji the composing word names ("pizza" -> 🍕),
    /// making room at the end of the strip. Never in a secret field.
    fn add_emoji_suggestions(&mut self) {
        if !self.suggestions_enabled || self.is_secret_field() || self.current_word.is_empty() {
            return;
        }
        let word = self.current_word.to_lowercase();
        let found: Vec<String> = self
            .emoji_panel
            .catalogue()
            .suggest(&word, EMOJI_SUGGESTIONS)
            .into_iter()
            .map(|e| e.text().to_owned())
            .collect();
        if found.is_empty() {
            return;
        }
        self.suggestions
            .truncate(MAX_SUGGESTIONS.saturating_sub(found.len()));
        self.suggestions
            .extend(found.into_iter().map(Suggestion::emoji));
    }

    fn refresh_next_word_suggestions(&mut self) {
        if !self.suggestions_enabled
            || self.is_secret_field()
            || self.purpose == InputPurpose::Terminal
        {
            self.suggestions.clear();
            return;
        }
        let preds = self
            .predictor
            .predict_next_n(self.prev_word.as_deref(), "", MAX_SUGGESTIONS);
        self.suggestions = preds
            .into_iter()
            .map(|(w, s)| Suggestion::new(w, s))
            .collect();
    }

    /// Activate a suggestion chip by index. A chip correcting a swiped word
    /// replaces it and keeps it composing, so the alternatives stay available;
    /// any other chip commits its word plus a space, replacing whatever was
    /// composing.
    ///
    /// Returns whether a chip was activated.
    pub fn activate_suggestion(&mut self, index: usize) -> bool {
        if self.context_error.is_some() {
            return false;
        }
        let correction = self.last_correction.take();
        if index == 0
            && let Some(c) = correction
        {
            self.revert_correction(c);
            return true;
        }
        let Some(s) = self.suggestions.get(index).cloned() else {
            return false;
        };
        if s.personal
            && !self
                .personal_words()
                .hints(self.language.code())
                .any(|word| word == s.word)
        {
            self.refresh_completion_suggestions();
            return false;
        }
        if s.emoji {
            self.finish_word_with(" ", false);
            self.commit_emoji(&s.word, " ");
            return true;
        }
        self.current_word = s.word;
        if self.word_from_swipe || self.current_word.ends_with('\'') {
            // An elision ("l'") keeps composing: the next word joins it.
            self.emit(OskOutput::SetPreedit(self.current_word.clone()));
            self.refresh_completion_suggestions();
        } else {
            self.finish_word(" ");
        }
        true
    }

    fn decode_and_apply_swipe(&mut self, path: &[Point]) -> bool {
        if self.is_secret_field() {
            return false;
        }
        let layout = self.current_layout().clone();
        let cands = self
            .swipe_model
            .decode(path, &self.key_geom, &layout, MAX_SUGGESTIONS);
        let Some(top_cand) = cands.first() else {
            return false;
        };
        let shift = self.shift;
        let top = case_swipe_word(&top_cand.word, shift);
        if shift == ShiftState::Shift {
            self.shift = ShiftState::Off;
        }
        self.last_correction = None;
        if self.word_from_swipe {
            self.finish_word(" ");
        }
        self.current_word = top;
        self.word_from_swipe = true;
        self.current_word_overflowed = false;
        self.emit(OskOutput::SetPreedit(self.current_word.clone()));
        // Show the ranked candidates so the user can correct the pick.
        self.suggestions = cands
            .into_iter()
            .map(|c| Suggestion::new(case_swipe_word(&c.word, shift), c.score))
            .collect();
        self.add_emoji_suggestions();
        true
    }

    pub fn emoji_recents(&self) -> &[String] {
        self.emoji_recents.items()
    }

    pub fn emoji_panel(&self) -> &EmojiPanel {
        &self.emoji_panel
    }

    fn stage_emoji_recents(&mut self) {
        self.preference_edits.push(SetPreference::EmojiRecents(
            self.emoji_recents.items().to_vec(),
        ));
    }

    fn set_emoji_recents(&mut self, recents: &[String]) {
        if self.emoji_recents.items() == recents {
            return;
        }
        let mut adopted = emoji::Recents::default();
        for emoji in recents.iter().rev() {
            adopted.record(emoji);
        }
        self.emoji_recents = adopted;
        self.layout_bands();
    }

    pub(crate) fn open_emoji_panel(&mut self) {
        self.emoji_panel.open();
        self.fingers.clear();
        self.structure_dirty = true;
        self.layout_bands();
    }

    fn close_emoji_panel(&mut self) {
        if self.emoji_panel.is_open() {
            self.emoji_panel.close();
            self.structure_dirty = true;
            self.layout_bands();
        }
    }

    fn commit_emoji(&mut self, text: &str, tail: &str) {
        self.last_correction = None;
        if !self.current_word.is_empty() {
            self.finish_word_with("", false);
        }
        let mut out = text.to_owned();
        out.push_str(tail);
        self.emit(OskOutput::Commit(out));
        if self.personalization_enabled && !self.is_secret_field() {
            self.emoji_recents.record(text);
            self.stage_emoji_recents();
        }
    }

    fn apply_search_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::Char(s) => {
                if self.shift == ShiftState::Shift {
                    self.shift = ShiftState::Off;
                }
                self.emoji_panel.type_text(&s);
            }
            KeyAction::Space => self.emoji_panel.type_text(" "),
            KeyAction::Backspace => self.emoji_panel.backspace(),
            KeyAction::Enter => {
                if let Some(best) = self.emoji_panel.results().into_iter().next() {
                    self.commit_emoji(&best, "");
                }
            }
            KeyAction::Shift => self.shift = self.shift.cycle(),
            KeyAction::SymbolsToggle => {
                self.showing_symbols = !self.showing_symbols;
                self.structure_dirty = true;
            }
            KeyAction::Layout(name) => {
                if name != self.preferred_text_layout || name != self.active_layout_name() {
                    self.select_text_layout(&name);
                }
            }
            KeyAction::Emoji => {
                self.emoji_panel.end_search();
                self.structure_dirty = true;
            }
        }
        self.layout_bands();
    }

    fn activate_panel(&mut self, target: PanelHit) {
        match target {
            PanelHit::Search => {
                self.emoji_panel.start_search();
                self.structure_dirty = true;
            }
            PanelHit::Group(i) => self.emoji_panel.select_group(i),
            PanelHit::Emoji(text) => self.commit_emoji(&text, ""),
            PanelHit::Tone(text) => {
                self.emoji_panel.close_tones();
                self.commit_emoji(&text, "");
            }
            PanelHit::Letters => {
                self.close_emoji_panel();
                return;
            }
            PanelHit::Delete => self.apply_action(KeyAction::Backspace),
            PanelHit::EndSearch => {
                self.emoji_panel.end_search();
                self.structure_dirty = true;
            }
            PanelHit::Nothing => self.emoji_panel.close_tones(),
        }
        self.layout_bands();
    }

    fn open_emoji_tones(&mut self, text: &str) -> bool {
        let anchor = self
            .emoji_panel
            .emoji_rect(text)
            .unwrap_or(self.content_rect);
        let opened = self.emoji_panel.open_tones(text, anchor);
        self.layout_bands();
        opened
    }

    fn handle_panel_event(&mut self, event: &WindowEvent, scale: f64, handled: &mut bool) -> bool {
        if !self.emoji_panel.is_open() {
            return false;
        }
        let hit = self.pointer.and_then(|p| self.emoji_panel.hit(p));
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(hit) = hit else { return false };
                self.emoji_panel.press(hit);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some(pressed) = self.emoji_panel.take_pressed() else {
                    return false;
                };
                if hit.as_ref() == Some(&pressed) {
                    self.activate_panel(pressed);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let Some(hit) = hit else { return false };
                if let PanelHit::Emoji(text) = hit {
                    self.open_emoji_tones(&text);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.pointer.is_some_and(|p| self.emoji_panel.over_grid(p)) {
                    return false;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        -f64::from(*y) * f64::from(self.metrics.bar_height)
                    }
                    MouseScrollDelta::PixelDelta(d) => -d.y / scale,
                };
                self.emoji_panel.scroll_by(dy);
                self.layout_bands();
            }
            _ => return false,
        }
        *handled = true;
        true
    }

    /// Semantic actions on the emoji panel's nodes; `None` when `target` is
    /// not one of them. Only what the panel currently shows can be acted on.
    fn perform_emoji_action(&mut self, target: &str, action: &AccessibilityAction) -> Option<bool> {
        if !target.starts_with("osk:emoji") {
            return None;
        }
        let panel = &self.emoji_panel;
        if !panel.is_open() {
            return Some(false);
        }
        if target == "osk:emoji-search" {
            match action {
                AccessibilityAction::Activate | AccessibilityAction::Focus => {
                    self.activate_panel(PanelHit::Search);
                }
                AccessibilityAction::SetValue(query) => {
                    self.emoji_panel.start_search();
                    self.emoji_panel.set_query(query);
                    self.structure_dirty = true;
                    self.layout_bands();
                }
                _ => return Some(false),
            }
            return Some(true);
        }
        if let Some(text) = target.strip_prefix("osk:emoji:") {
            if panel.emoji_rect(text).is_none() {
                return Some(false);
            }
            return Some(match action {
                AccessibilityAction::Activate => {
                    self.activate_panel(PanelHit::Emoji(text.to_owned()));
                    true
                }
                AccessibilityAction::ShowContextMenu => self.open_emoji_tones(text),
                _ => false,
            });
        }
        if !matches!(action, AccessibilityAction::Activate) {
            return Some(false);
        }
        let hit = if let Some(text) = target.strip_prefix("osk:emoji-tone:") {
            if !panel.tone_options().iter().any(|t| t == text) {
                return Some(false);
            }
            PanelHit::Tone(text.to_owned())
        } else if let Some(index) = target.strip_prefix("osk:emoji-group:") {
            let Ok(index) = index.parse::<usize>() else {
                return Some(false);
            };
            PanelHit::Group(index)
        } else {
            match (target, panel.mode()) {
                ("osk:emoji-letters", PanelMode::Browse) => PanelHit::Letters,
                ("osk:emoji-delete", PanelMode::Browse) => PanelHit::Delete,
                ("osk:emoji-end-search", PanelMode::Search) => PanelHit::EndSearch,
                _ => return Some(false),
            }
        };
        self.activate_panel(hit);
        Some(true)
    }

    fn key_scale(&self) -> f64 {
        if self.key_geom.is_empty() {
            return f64::from(self.target_key_height);
        }
        if let Some(scale) = crate::swipe::letter_key_scale(&self.key_geom, self.current_layout()) {
            return scale;
        }
        let sum: f64 = self
            .key_geom
            .iter()
            .map(|(_, r)| (r.width() + r.height()) * 0.5)
            .sum();
        sum / self.key_geom.len() as f64
    }

    fn resolve_letter_tap(&mut self, hit: KeyId, p: Option<Point>) -> KeyId {
        let Some(p) = p else { return hit };
        // Identifiers and secret fields use the literal hit target. In
        // particular, password taps must not update personal touch offsets.
        if !self.autocorrect_allowed() {
            return hit;
        }
        let layout = self.current_layout();
        if !layout.key(hit).is_some_and(Key::is_letter) {
            return hit;
        }
        let keys: Vec<TouchKey> = self
            .key_geom
            .iter()
            .filter_map(|&(id, rect)| {
                let key = layout.key(id).filter(|k| k.is_letter())?;
                let ch = key.label.chars().next()?.to_lowercase().next()?;
                Some(TouchKey { id, ch, rect })
            })
            .collect();
        let predictor = &self.predictor;
        let (prev, word) = (self.prev_word.as_deref(), self.current_word.as_str());
        let p_next = self
            .next_key
            .get(prev, word, || predictor.next_key_distribution(prev, word));
        let target =
            crate::touch_model::resolve(p, &keys, p_next, &self.touch_model).unwrap_or(hit);
        if self.personalization_enabled
            && let Some(k) = keys.iter().find(|k| k.id == target)
        {
            self.touch_model.learn(k.ch, p, k.rect);
            if !self.is_secret_field()
                && let Some(offset) = self.touch_model.offset(k.ch)
            {
                self.lesson.touch.retain(|(letter, _)| *letter != k.ch);
                self.lesson.touch.push((k.ch, offset));
            }
        }
        target
    }

    /// A trail is feedback for a solo gesture, never for secret input,
    /// multi-thumb taps, caret steering or a contact in stale geometry.
    fn active_swipe_path(&self) -> &[Point] {
        if self.is_secret_field()
            || self.emoji_panel.is_open()
            || self.cursor_drag_active()
            || self.alternates.is_some()
            || self
                .contact_geometry
                .as_ref()
                .is_some_and(|geometry| *geometry != self.key_geom)
        {
            return &[];
        }
        let path = self
            .fingers
            .iter()
            .find(|finger| finger.solo && !finger.spent)
            .map(|finger| finger.path.as_slice())
            .or(self.gesture_path.as_deref())
            .unwrap_or_default();
        if crate::swipe::is_swipe(path, self.key_scale()) {
            path
        } else {
            &[]
        }
    }

    fn handle_touch(&mut self, touch: &Touch, scale_factor: f64, handled: &mut bool) {
        self.cancel_stale_contacts();
        let p = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            TouchPhase::Started => {
                // Only contacts that began on the keys are ours; the toolkit
                // turns every other one into pointer events.
                if !self.claims_raw_touch(p) {
                    return;
                }
                self.contact_geometry = Some(self.key_geom.clone());
                // A second finger ends any hold, open alternates row or caret
                // drag of the first: the rows keep typing in press order.
                self.hold = None;
                self.alternates = None;
                self.cursor_drag = None;
                // Press order: a held key types before the new one can.
                let held: Vec<Finger> = self
                    .fingers
                    .iter_mut()
                    .filter(|f| !f.spent)
                    .map(|f| {
                        f.spent = true;
                        f.clone()
                    })
                    .collect();
                for f in &mut self.fingers {
                    f.solo = false;
                }
                for f in held {
                    self.type_finger(&f, false);
                }
                self.fingers.retain(|f| f.id != touch.id);
                let key = self.key_at(p);
                self.fingers.push(Finger {
                    id: touch.id,
                    key,
                    at: p,
                    path: vec![p],
                    solo: self.fingers.is_empty(),
                    spent: false,
                });
                if let Some(key) = key {
                    self.begin_hold(key, Some(touch.id), Instant::now());
                    self.begin_cursor_drag(key, Some(touch.id), p.x);
                }
                *handled = true;
            }
            TouchPhase::Moved => {
                let key = self.key_at(p);
                let Some(f) = self.fingers.iter_mut().find(|f| f.id == touch.id) else {
                    return;
                };
                f.at = p;
                f.key = key;
                record_gesture_point(&mut f.path, p);
                self.track_hold(Some(touch.id), p, key);
                *handled = true;
            }
            TouchPhase::Ended => {
                let Some(i) = self.fingers.iter().position(|f| f.id == touch.id) else {
                    return;
                };
                let finger = Some(touch.id);
                self.track_hold(finger, p, self.key_at(p));
                let mut f = self.fingers.remove(i);
                f.at = p;
                record_gesture_point(&mut f.path, p);
                if self.alternates.as_ref().is_some_and(|(o, _)| *o == finger) {
                    self.commit_alternate();
                } else if !f.spent {
                    let may_swipe = f.solo;
                    self.type_finger(&f, may_swipe);
                }
                if self.hold.is_some_and(|h| h.finger == finger) {
                    self.hold = None;
                }
                if self.cursor_drag.is_some_and(|d| d.finger == finger) {
                    self.cursor_drag = None;
                }
                *handled = true;
            }
            TouchPhase::Cancelled => {
                let finger = Some(touch.id);
                if self.alternates.as_ref().is_some_and(|(o, _)| *o == finger) {
                    self.alternates = None;
                }
                if self.hold.is_some_and(|h| h.finger == finger) {
                    self.hold = None;
                }
                if self.cursor_drag.is_some_and(|d| d.finger == finger) {
                    self.cursor_drag = None;
                }
                let before = self.fingers.len();
                self.fingers.retain(|f| f.id != touch.id);
                *handled |= self.fingers.len() != before;
            }
        }
    }

    fn type_finger(&mut self, f: &Finger, may_swipe: bool) {
        if may_swipe
            && crate::swipe::is_swipe(&f.path, self.key_scale())
            && self.decode_and_apply_swipe(&f.path)
        {
            return;
        }
        if let Some(id) = self.key_at(f.at) {
            let target = self.resolve_letter_tap(id, Some(f.at));
            self.activate_key(target);
        }
    }

    fn layout_bands(&mut self) {
        // A host may assign a whole phone viewport to this surface. Keep the
        // keyboard docked at its bottom at the themed row height; a keyboard
        // panel must not turn that spare space into enormous key wells.
        let height = if self.parked {
            f64::from(self.toolbar_height).min(self.rect.height())
        } else {
            f64::from(
                self.toolbar_height
                    + self.suggestion_height
                    + self.current_layout().row_count() as f32 * self.target_key_height,
            )
            .min(self.rect.height())
        };
        let keyboard_rect = Rect::new(
            self.rect.x0,
            self.rect.y1 - height,
            self.rect.x1,
            self.rect.y1,
        );
        let geometry = keyboard_geometry(
            keyboard_rect,
            self.current_layout(),
            &self.toolbar,
            if self.parked { &[] } else { &self.suggestions },
            self.shift,
            self.toolbar_height,
            if self.parked {
                0.0
            } else {
                self.suggestion_height
            },
            keyboard_gutter(&self.spacing),
        );
        self.content_rect = geometry.content_rect;
        self.toolbar_geom = geometry.toolbar;
        self.suggestion_geom = geometry.suggestions;
        self.suggestion_area = geometry.suggestion_area;
        self.key_area = geometry.key_area;
        self.key_geom = geometry.keys;
        let (toolbar_area, suggestion_area, key_rows) = (
            geometry.toolbar_area,
            geometry.suggestion_area,
            geometry.key_rows,
        );
        let mode = self.emoji_panel.mode();
        if mode != PanelMode::Closed {
            self.suggestion_geom.clear();
        }
        if self.parked {
            // Nothing under the strip is a key: the panel is not covering it.
            self.suggestion_geom.clear();
            self.key_geom.clear();
            self.key_area = Rect::ZERO;
        }
        match mode {
            PanelMode::Closed => {}
            PanelMode::Browse => {
                self.key_geom.clear();
                self.key_area = Rect::ZERO;
            }
            PanelMode::Search => self.toolbar_geom.clear(),
        }
        let bands = PanelBands {
            toolbar: toolbar_area,
            suggestion: suggestion_area,
            keys: key_rows,
            row_height: f64::from(self.target_key_height),
            ..PanelBands::default()
        }
        .sized_by(&self.spacing, &self.metrics);
        let none = emoji::Recents::default();
        let recents = if self.personalization_enabled {
            &self.emoji_recents
        } else {
            &none
        };
        let theme = self.layout_theme.clone();
        self.emoji_panel.layout(bands, recents, &theme);
    }

    fn pointer_released(&mut self, handled: &mut bool) {
        self.cancel_stale_contacts();
        let path = self.gesture_path.take();
        let held = self.hold.take().filter(|h| h.finger.is_none());
        let steering = self.cursor_steering(None);
        if self.cursor_drag.is_some_and(|d| d.finger.is_none()) {
            self.cursor_drag = None;
        }
        if self.alternates.as_ref().is_some_and(|(o, _)| o.is_none()) {
            self.pressed_key = None;
            self.commit_alternate();
            *handled = true;
        } else if steering || held.is_some_and(|h| h.fired) {
            self.pressed_key = None;
            *handled = true;
        } else if let Some(id) = self.pressed_key.take() {
            let scale = self.key_scale();
            let is_swipe = path
                .as_ref()
                .is_some_and(|p| crate::swipe::is_swipe(p, scale));
            if is_swipe {
                let decoded = path
                    .as_ref()
                    .is_some_and(|p| self.decode_and_apply_swipe(p));
                if !decoded && self.hovered_key == Some(id) {
                    self.activate_key(id);
                }
            } else if self.hovered_key == Some(id) {
                let target = self.resolve_letter_tap(id, self.pointer);
                self.activate_key(target);
            }
            *handled = true;
        } else if let Some(i) = self.pressed_sugg.take() {
            if self.hovered_sugg == Some(i) {
                self.activate_suggestion(i);
            }
            *handled = true;
        } else if let Some(i) = self.pressed_tool.take() {
            if self.hovered_tool == Some(i) {
                self.activate_tool(i);
            }
            *handled = true;
        }
    }

    fn begin_hold(&mut self, key: KeyId, finger: Option<u64>, now: Instant) {
        let shift = self.shift;
        let secret = self.is_secret_field();
        self.hold = self.current_layout().key(key).and_then(|k| {
            let (kind, delay) = match &k.action {
                KeyAction::Backspace => (HoldKind::Repeat, REPEAT_DELAY),
                _ if !secret && !k.alternates(shift, |_| 0.0).is_empty() => {
                    (HoldKind::Alternates, LONG_PRESS)
                }
                _ => return None,
            };
            Some(Hold {
                key,
                finger,
                due: now + delay,
                kind,
                fired: false,
            })
        });
    }

    fn poll_hold(&mut self, now: Instant) {
        if self.hold.is_none_or(|hold| now < hold.due) {
            return;
        }
        self.cancel_stale_contacts();
        let Some(hold) = self.hold.as_mut() else {
            return;
        };
        if now < hold.due {
            return;
        }
        let (key, finger, kind) = (hold.key, hold.finger, hold.kind);
        match kind {
            HoldKind::Repeat => {
                hold.fired = true;
                hold.due = now + REPEAT_INTERVAL;
                self.spend_finger(finger);
                self.apply_action(KeyAction::Backspace);
            }
            HoldKind::Alternates => {
                self.hold = None;
                let language = self.language;
                if self
                    .accent_usage
                    .as_ref()
                    .is_none_or(|(l, _)| *l != language)
                {
                    let usage = self.predictor.dictionary().accent_usage();
                    self.accent_usage = Some((language, usage));
                }
                let usage = self.accent_usage.as_ref().map(|(_, u)| u);
                let options = self
                    .current_layout()
                    .key(key)
                    .map(|k| {
                        k.alternates(self.shift, |c| {
                            usage.and_then(|u| u.get(&c)).copied().unwrap_or(0.0)
                        })
                    })
                    .unwrap_or_default();
                let Some(&(_, rect)) = self.key_geom.iter().find(|(id, _)| *id == key) else {
                    return;
                };
                self.alternates = Some((
                    finger,
                    AlternatesPopup::new(rect, options, self.content_rect),
                ));
                self.spend_finger(finger);
            }
        }
    }

    fn spend_finger(&mut self, finger: Option<u64>) {
        if let Some(f) = finger.and_then(|id| self.fingers.iter_mut().find(|f| f.id == id)) {
            f.spent = true;
        }
    }

    fn commit_alternate(&mut self) {
        let Some((_, popup)) = self.alternates.take() else {
            return;
        };
        if let Some(text) = popup.chosen() {
            let action = KeyAction::Char(text.to_string());
            if self.shift == ShiftState::Shift {
                self.shift = ShiftState::Off;
            }
            if self.emoji_panel.is_searching() {
                self.apply_search_action(action);
            } else {
                self.apply_action(action);
            }
        }
    }

    /// A contact holding `finger` moved to `p` (the key under it is `key`).
    /// Returns whether the move belongs to a hold, the alternates row or the
    /// caret drag (so it is not a swipe).
    fn track_hold(&mut self, finger: Option<u64>, p: Point, key: Option<KeyId>) -> bool {
        if let Some((owner, popup)) = self.alternates.as_mut()
            && *owner == finger
        {
            popup.track(p.x);
            return true;
        }
        if self
            .hold
            .as_ref()
            .is_some_and(|h| h.finger == finger && !h.fired && Some(h.key) != key)
        {
            // Sliding off the key before the timer is a swipe, not a hold.
            self.hold = None;
        }
        self.drag_cursor(finger, p.x)
    }

    fn begin_cursor_drag(&mut self, key: KeyId, finger: Option<u64>, x: f64) {
        let on_space = self
            .current_layout()
            .key(key)
            .is_some_and(|k| k.action == KeyAction::Space);
        self.cursor_drag = on_space.then_some(CursorDrag {
            finger,
            anchor: x,
            active: false,
        });
    }

    fn drag_cursor(&mut self, finger: Option<u64>, x: f64) -> bool {
        let scale = self.key_scale();
        let Some(drag) = self.cursor_drag.as_mut().filter(|d| d.finger == finger) else {
            return false;
        };
        let started = !drag.active;
        if started {
            if (x - drag.anchor).abs() < scale * CURSOR_START {
                return false;
            }
            drag.active = true;
            drag.anchor = x;
        }
        let step = scale * CURSOR_STEP;
        let steps = ((x - drag.anchor) / step).trunc();
        drag.anchor += steps * step;
        if started {
            self.spend_finger(finger);
            self.hold = None;
            self.last_correction = None;
            self.finish_word_with("", false);
            self.prev_word = None;
            self.suggestions.clear();
        }
        let symbol = if steps < 0.0 {
            Symbol::Left
        } else {
            Symbol::Right
        };
        for _ in 0..(steps.abs() as usize) {
            self.emit(OskOutput::key(symbol));
        }
        true
    }

    fn cursor_steering(&self, finger: Option<u64>) -> bool {
        self.cursor_drag
            .as_ref()
            .is_some_and(|d| d.finger == finger && d.active)
    }

    fn preview_key(&self, finger_keys: &[KeyId]) -> Option<KeyId> {
        if self.is_secret_field() || self.alternates.is_some() || self.cursor_drag_active() {
            return None;
        }
        let key = self.pressed_key.or_else(|| finger_keys.last().copied())?;
        let k = self.current_layout().key(key)?;
        let single = matches!(&k.action, KeyAction::Char(s) if s.chars().count() == 1);
        single.then_some(key)
    }

    fn cursor_drag_active(&self) -> bool {
        self.cursor_drag.as_ref().is_some_and(|d| d.active)
    }

    pub fn set_hit_overlay(&mut self, on: bool) {
        self.hit_overlay = on;
    }

    /// The effective hit areas in the current typing context, sampled every
    /// `step` logical px over the letter keys.
    fn hit_cells(&mut self, step: f64) -> Vec<HitCell> {
        let layout = self.current_layout();
        let keys: Vec<TouchKey> = self
            .key_geom
            .iter()
            .filter_map(|&(id, rect)| {
                let key = layout.key(id).filter(|k| k.is_letter())?;
                let ch = key.label.chars().next()?.to_lowercase().next()?;
                Some(TouchKey { id, ch, rect })
            })
            .collect();
        let area = keys
            .iter()
            .map(|k| k.rect)
            .reduce(|a, b| a.union(b))
            .unwrap_or(Rect::ZERO);
        let predictor = &self.predictor;
        let (prev, word) = (self.prev_word.as_deref(), self.current_word.as_str());
        let p_next = self
            .next_key
            .get(prev, word, || predictor.next_key_distribution(prev, word));
        crate::touch_model::hit_map(
            area,
            step,
            &keys,
            p_next,
            &self.touch_model,
            crate::touch_model::ResolveParams::DEFAULT,
        )
    }

    fn key_at(&self, p: Point) -> Option<KeyId> {
        self.key_geom
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(id, _)| *id)
    }

    fn tool_at(&self, p: Point) -> Option<usize> {
        self.toolbar_geom
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }

    fn sugg_at(&self, p: Point) -> Option<usize> {
        self.suggestion_geom
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(i, _)| *i)
    }
}

#[derive(Clone, Debug)]
struct Finger {
    id: u64,
    key: Option<KeyId>,
    /// Latest position (logical px).
    at: Point,
    path: Vec<Point>,
    solo: bool,
    /// Its key already typed because another finger landed; lifting it types
    /// nothing more.
    spent: bool,
}

fn case_swipe_word(word: &str, shift: ShiftState) -> String {
    shift.case_word(word)
}

/// Keep gesture sampling bounded while retaining the latest endpoint.
fn record_gesture_point(path: &mut Vec<Point>, point: Point) {
    if path.last().is_none_or(|&last| last.distance(point) > 1.0) {
        if path.len() < MAX_GESTURE_POINTS {
            path.push(point);
        } else if let Some(last) = path.last_mut() {
            *last = point;
        }
    }
}

const LONG_PRESS: Duration = Duration::from_millis(350);
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_INTERVAL: Duration = Duration::from_millis(50);
const CURSOR_START: f64 = 0.5;
const CURSOR_STEP: f64 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    Alternates,
    Repeat,
}

#[derive(Clone, Copy, Debug)]
struct Hold {
    key: KeyId,
    finger: Option<u64>,
    due: Instant,
    kind: HoldKind,
    fired: bool,
}

#[derive(Clone, Copy, Debug)]
struct CursorDrag {
    finger: Option<u64>,
    anchor: f64,
    active: bool,
}

#[derive(Clone, Debug)]
struct Correction {
    typed: String,
    corrected: String,
    tail: String,
}

#[path = "keyboard_preferences.rs"]
mod keyboard_preferences;

#[cfg(test)]
#[path = "personal_vocabulary_tests.rs"]
mod personal_vocabulary_tests;

impl Widget for OskWidget {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.dirty = true;
        self.poll_hold(Instant::now());
    }

    /// The size this keyboard needs its surface to be.
    ///
    /// Parked, that is one toolbar strip: the desktop shapes the panel to it
    /// (`Widget::preferred_window_size` reaches `xdg_toplevel` min/max), so
    /// the part of the screen the keys were covering belongs to the app again
    /// instead of being an empty rectangle that only the keyboard could hear
    /// clicks in.
    fn preferred_window_size(&self) -> Option<(f64, f64)> {
        (self.visible && self.parked && self.rect.width() > 0.0).then(|| {
            (
                self.rect.width(),
                f64::from(toolbar_row_height(&self.spacing) + self.spacing.s1),
            )
        })
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        self.layout_theme = theme.clone();
        self.target_key_height = key_row_height(&self.metrics) * self.key_size.scale();
        self.toolbar_height = toolbar_row_height(&self.spacing);
        self.suggestion_height =
            if self.purpose == InputPurpose::Terminal && self.context_error.is_none() {
                0.0
            } else {
                suggestion_row_height(&self.metrics)
            };
        let rows = self.current_layout().row_count();
        // Prose reserves its suggestion band so predictions never move the
        // keys. Code has no word predictions and needs only its editing strip.
        // Hidden by the compositor, the keyboard requests no height and paints
        // nothing, and its window stands on no ground: a keyboard put away
        // leaves the app under it showing.
        let total_h = if self.visible && self.parked {
            self.toolbar_height + self.spacing.s1
        } else if self.visible {
            self.toolbar_height + self.suggestion_height + rows as f32 * self.target_key_height
        } else {
            0.0
        };
        taffy.widget_leaf(
            Style {
                flex_shrink: 0.0,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: if self.visible && self.fill_surface {
                        Dimension::percent(1.0)
                    } else {
                        Dimension::length(total_h)
                    },
                },
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.layout_bands();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.visible {
            return;
        }
        let empty_hint = i18n::t(if self.suggestions_enabled {
            "osk-suggestions-empty"
        } else {
            "osk-suggestions-off"
        });
        let finger_keys: Vec<KeyId> = self
            .fingers
            .iter()
            .filter(|f| !f.spent)
            .filter_map(|f| f.key)
            .collect();
        let hit_cells = if self.hit_overlay && !self.emoji_panel.is_open() {
            // Sampled every 2 px: a debug resolution, not a layout size.
            self.hit_cells(2.0)
        } else {
            Vec::new()
        };
        let swipe_path = self.active_swipe_path();
        let preview = swipe_path
            .is_empty()
            .then(|| self.preview_key(&finger_keys))
            .flatten();
        KeyboardPresentation {
            rect: self.rect,
            layout: self.current_layout(),
            toolbar: &self.toolbar,
            suggestions: &self.suggestions,
            toolbar_geom: &self.toolbar_geom,
            suggestion_geom: &self.suggestion_geom,
            key_geom: &self.key_geom,
            suggestion_area: self.suggestion_area,
            shift: self.shift,
            hovered_key: self.hovered_key,
            pressed_key: self.pressed_key,
            finger_keys: &finger_keys,
            hovered_tool: self.hovered_tool,
            pressed_tool: self.pressed_tool,
            modifiers: self.modifiers,
            hovered_sugg: self.hovered_sugg,
            pressed_sugg: self.pressed_sugg,
            empty_hint: &empty_hint,
            context_error: self.context_error.as_deref(),
            emoji_panel_open: self.emoji_panel.is_open(),
            preview,
            alternates: self.alternates.as_ref().map(|(_, popup)| popup),
            cursor_mode: self.cursor_drag_active(),
            hit_cells: &hit_cells,
            swipe_path,
        }
        .draw(scene, transform, text_renderer, theme);

        // Last, so the skin-tone row floats over the keys.
        self.emoji_panel
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        // A hidden keyboard occupies no area and draws nothing, so it should
        // not consume or react to pointer events meant for the app behind it.
        if !self.visible {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(ctx.logical(position));
        }
        self.cancel_stale_contacts();
        if self.handle_panel_event(event, ctx.scale_factor, ctx.handled) {
            return;
        }
        match event {
            WindowEvent::Touch(touch) => self.handle_touch(touch, ctx.scale_factor, ctx.handled),
            WindowEvent::Focused(false) => self.cancel_contacts(),
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.pointer = Some(p);
                if let Some(path) = self.gesture_path.as_mut() {
                    // Only record once the pointer has moved a touch, to keep
                    // the path tight.
                    if path.last().is_none_or(|&l| l.distance(p) > 1.0) {
                        if path.len() < MAX_GESTURE_POINTS {
                            path.push(p);
                        } else if let Some(last) = path.last_mut() {
                            // Preserve the start plus the most recent endpoint
                            // under very high-rate pointer streams without
                            // retaining an unbounded gesture trace.
                            *last = p;
                        }
                    }
                }
                let k = self.key_at(p);
                if self.pressed_key.is_some() && self.track_hold(None, p, k) {
                    *ctx.handled = true;
                }
                let s = if k.is_some() { None } else { self.sugg_at(p) };
                let t = if k.is_some() || s.is_some() {
                    None
                } else {
                    self.tool_at(p)
                };
                if k != self.hovered_key || t != self.hovered_tool || s != self.hovered_sugg {
                    self.hovered_key = k;
                    self.hovered_tool = t;
                    self.hovered_sugg = s;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.contact_geometry = Some(self.key_geom.clone());
                if let Some(id) = self.hovered_key {
                    self.pressed_key = Some(id);
                    let start = self
                        .key_geom
                        .iter()
                        .find(|(kid, _)| *kid == id)
                        .map_or_else(|| self.rect.center(), |(_, r)| r.center());
                    self.gesture_path = Some(vec![start]);
                    self.begin_hold(id, None, Instant::now());
                    if let Some(p) = self.pointer {
                        self.begin_cursor_drag(id, None, p.x);
                    }
                    *ctx.handled = true;
                } else if let Some(i) = self.hovered_sugg {
                    self.pressed_sugg = Some(i);
                    *ctx.handled = true;
                } else if let Some(i) = self.hovered_tool {
                    self.pressed_tool = Some(i);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.pointer_released(ctx.handled);
            }
            _ => {}
        }
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.visible && self.key_area.contains(point)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        // A held key acts on its own (long press, repeat); the host syncs at
        // this instant, and `sync` fires it.
        self.hold.map(|h| h.due)
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "OnScreenKeyboard"
    }

    fn test_id(&self) -> Option<&str> {
        Some("on_screen_keyboard")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if !self.visible {
            return None;
        }
        let mut children = Vec::new();
        for (index, rect) in &self.toolbar_geom {
            let Some(item) = self.toolbar.get(*index) else {
                continue;
            };
            let target = item.semantic_target(*index);
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(self.spoken_tool(item))
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(&target)
                    .with_action_target(target),
            );
        }
        for (index, rect) in self
            .suggestion_geom
            .iter()
            .filter(|_| self.context_error.is_none())
        {
            let Some(suggestion) = self.suggestions.get(*index) else {
                continue;
            };
            let name = if suggestion.emoji {
                i18n::t_args(
                    "osk-emoji-insert",
                    &[(
                        "name",
                        self.emoji_panel.spoken_name(&suggestion.word).into(),
                    )],
                )
            } else {
                i18n::t_args(
                    "osk-suggestion-insert",
                    &[("word", suggestion.word.clone().into())],
                )
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(format!("suggestion:{}:{}", index, suggestion.word))
                    .with_action_target(format!("osk:suggestion:{}:{}", index, suggestion.word)),
            );
        }
        let shift = self.shift;
        let layout = self.current_layout();
        for (id, rect) in &self.key_geom {
            let Some(key) = layout.key(*id) else {
                continue;
            };
            let label = key.display_label(shift);
            let name = match &key.action {
                KeyAction::Char(_) => label.clone(),
                KeyAction::Backspace => i18n::t("osk-key-backspace"),
                KeyAction::Enter => i18n::t("osk-key-enter"),
                KeyAction::Space => i18n::t("osk-key-space"),
                KeyAction::Shift => i18n::t(match shift {
                    ShiftState::Off => "osk-key-shift-off",
                    ShiftState::Shift => "osk-key-shift-on",
                    ShiftState::CapsLock => "osk-key-caps-lock",
                }),
                KeyAction::SymbolsToggle => i18n::t(if self.showing_symbols {
                    "osk-key-letters"
                } else {
                    "osk-key-symbols"
                }),
                KeyAction::Layout(layout) => {
                    i18n::t_args("osk-key-layout", &[("layout", layout.clone().into())])
                }
                KeyAction::Emoji => i18n::t("osk-emoji-key"),
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true).disabled(matches!(&key.action, KeyAction::Layout(name) if name == &self.preferred_text_layout && name == self.active_layout_name())))
                    .with_hashed_key(format!("key:{}:{}:{}", layout.name, id.row, id.col))
                    .with_action_target(format!(
                        "osk:key:{}:{}:{}:{}",
                        layout.name, id.row, id.col, label
                    )),
            );
        }
        children.extend(self.emoji_panel.accessibility());
        Some(
            AccessNode::new(Role::Group)
                .with_name(i18n::t("osk-name"))
                .with_description(self.field_description())
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !self.visible {
            return false;
        }
        if let Some(handled) = self.perform_emoji_action(target, action) {
            return handled;
        }
        if !matches!(action, AccessibilityAction::Activate) {
            return false;
        }
        if let Some(rest) = target.strip_prefix("osk:key:") {
            let mut parts = rest.splitn(4, ':');
            let (Some(layout), Some(row), Some(col), Some(label)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            let (Ok(row), Ok(col)) = (row.parse::<usize>(), col.parse::<usize>()) else {
                return false;
            };
            let id = KeyId::new(row, col);
            let current = self.current_layout();
            if current.name != layout
                || current
                    .key(id)
                    .is_none_or(|key| key.display_label(self.shift) != label)
            {
                return false;
            }
            self.activate_key(id);
            return true;
        }
        if let Some(rest) = target.strip_prefix("osk:suggestion:") {
            let Some((index, word)) = rest.split_once(':') else {
                return false;
            };
            let Ok(index) = index.parse::<usize>() else {
                return false;
            };
            if self
                .suggestions
                .get(index)
                .is_none_or(|suggestion| suggestion.word != word)
            {
                return false;
            }
            return self.activate_suggestion(index);
        }
        if let Some(rest) = target.strip_prefix("osk:context:") {
            let mut parts = rest.splitn(3, ':');
            let (Some(generation), Some(slot), Some(label)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            let (Ok(generation), Ok(slot)) = (generation.parse::<u32>(), slot.parse::<u32>())
            else {
                return false;
            };
            if self.context_generation != Some(generation) || self.is_secret_field() {
                return false;
            }
            let Some(index) = self.toolbar.iter().position(|item| {
                item.label == label
                    && item.action == OskToolbarAction::Contextual { generation, slot }
            }) else {
                return false;
            };
            self.activate_tool(index);
            return self.context_error.is_none();
        }
        if let Some(rest) = target.strip_prefix("osk:tool:") {
            let Some((index, label)) = rest.split_once(':') else {
                return false;
            };
            let Ok(index) = index.parse::<usize>() else {
                return false;
            };
            if self.toolbar.get(index).is_none_or(|item| {
                item.label != label || matches!(item.action, OskToolbarAction::Contextual { .. })
            }) {
                return false;
            }
            self.activate_tool(index);
            return true;
        }
        false
    }

    /// A layer/layout switch changes the displayed row set (and, for custom
    /// layouts, the row count → the intrinsic height), so the host must
    /// rebuild the taffy node. Rising-edge: reading clears the flag.
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }
}

/// Parse the canonical name of an [`InputPurpose`]. Returns `None` for an
/// unknown name so the caller can ignore it.
pub fn input_purpose_from_name(name: &str) -> Option<InputPurpose> {
    Some(match name {
        "text" => InputPurpose::Text,
        "terminal" | "code" => InputPurpose::Terminal,
        "number" => InputPurpose::Number,
        "decimal" => InputPurpose::Decimal,
        "phone" | "telephone" => InputPurpose::Telephone,
        "url" => InputPurpose::Url,
        "email" => InputPurpose::Email,
        "pin" => InputPurpose::Pin,
        "password" => InputPurpose::Password,
        "date" => InputPurpose::Date,
        "time" => InputPurpose::Time,
        _ => return None,
    })
}

pub fn input_purpose_name(purpose: InputPurpose) -> &'static str {
    match purpose {
        InputPurpose::Text => "text",
        InputPurpose::Terminal => "terminal",
        InputPurpose::Number => "number",
        InputPurpose::Decimal => "decimal",
        InputPurpose::Telephone => "phone",
        InputPurpose::Url => "url",
        InputPurpose::Email => "email",
        InputPurpose::Pin => "pin",
        InputPurpose::Password => "password",
        InputPurpose::Date => "date",
        InputPurpose::Time => "time",
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "visual_review.rs"]
mod visual_review;
