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
//! ## Seams
//! * **Neural swipe:** the decoder is behind the
//!   [`crate::swipe::SwipeModel`] trait. The shipping model is the geometric
//!   [`crate::swipe::GeometricSwipeModel`]; a future FUTO/ExecuTorch
//!   `FutoSwipeModel` implementing the same trait can be swapped in via
//!   [`OskWidget::with_swipe_model`] with no other change.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use ui::doc::{read_bool, read_str};
use ui::widget::{AppModel, ModelPath};

#[cfg(test)]
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent};

use crate::actions::{Preference, PreferenceDrafts};
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{OperationKind, Resource};
use ui::widget::Widget;

use crate::autocorrect::{Autocorrector, KeyAdjacency};
use crate::dict::Language;
use crate::emoji_panel::{EMOJI_SUGGESTIONS, EmojiPanel, PanelBands, PanelHit, PanelMode};
use crate::futo::FutoSwipeModel;
use crate::i18n;
use crate::imclient::{content_hint, may_learn};
use crate::inject::Symbol;
use crate::layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState};
use crate::output::{OskOutput, TextMirror};
use crate::predict::Predictor;
use crate::suggest::Suggestion;
use crate::swipe::{GeometricSwipeModel, SwipeModel};

/// The default swipe decoder: the real FUTO `ExecuTorch` model when its libs
/// and model assets are available, else the geometric SHARK²-style decoder.
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
use crate::toolbar::{OskToolbarAction, OskToolbarItem};
use crate::touch_model::{HitCell, NextKeyCache, TouchKey, TouchModel};

/// Default target height of a single key row, in logical px. Responsive
/// sizing keeps the *width* filling the surface and uses this for height —
/// the same code serves a phone (small surface) and a desktop (wide
/// surface); only the surface size differs. ≥ 48 px honours the design's
/// minimum touch target.
pub const DEFAULT_KEY_HEIGHT: f32 = 56.0;
/// Default toolbar row height.
pub const DEFAULT_TOOLBAR_HEIGHT: f32 = 48.0;
/// Default suggestion-bar row height (the candidate / next-word chips).
pub const DEFAULT_SUGGESTION_HEIGHT: f32 = 40.0;
/// How many suggestion chips to show at once.
pub(crate) const MAX_SUGGESTIONS: usize = 5;
/// The diagnostic mirror is deliberately bounded: `on_output` is the delivery
/// path, while this buffer exists only for hosts/tests that inspect it.
pub const MAX_PENDING_TEXT_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_ENTRIES: usize = 1_024;
const MAX_KEY_LOG_ENTRIES: usize = 1_024;
const MAX_TOOLBAR_LOG_ENTRIES: usize = 128;
const MAX_GESTURE_POINTS: usize = 256;
const MAX_TRACKED_WORD_SCALARS: usize = 128;
const CONFIG_LAYOUT_KEY: &str = "osk.preferred_layout";
const CONFIG_SUGGESTIONS_KEY: &str = "osk.suggestions_enabled";
const CONFIG_PERSONALIZATION_KEY: &str = "osk.personalization_enabled";
const CONFIG_EMOJI_RECENTS_KEY: &str = "osk.emoji_recents";

/// The on-screen keyboard widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent, user-configurable interaction flags are clearer than a false state-machine invariant"
)]
pub struct OskWidget {
    /// Registered layouts by name. Always contains the symbols layer plus
    /// at least one letter layout. Adding AZERTY/QWERTZ/… is just inserting
    /// data here (see [`OskWidget::with_layout`]).
    layouts: HashMap<String, KeyboardLayout>,
    /// Name of the active *letter* layout (e.g. `"qwerty"`). For non-letter
    /// purposes (numeric/phone/pin) this holds the chosen specialised layout
    /// name; it is what `active_layout_name` reports when symbols are not shown.
    active_letter: String,
    /// Last chosen alphabetic layout. Content-purpose layouts may temporarily
    /// replace `active_letter`, but returning to a text field restores this
    /// durable language preference.
    preferred_text_layout: String,
    /// The field intent the keyboard is currently serving. Drives which layout
    /// is active and whether the shift/symbols affordances make sense.
    purpose: InputPurpose,
    /// The focused field's content hints ([`content_hint`] bits): secrecy and
    /// capitalization.
    hints: u32,
    /// Whether the caret follows a sentence end (or the field start), for
    /// automatic capitalization.
    sentence_start: bool,
    /// Whether the last committed text ended a sentence with `.`, `!` or `?`
    /// that no space has followed yet.
    sentence_end: bool,
    /// Whether the symbols layer is currently displayed instead of letters.
    showing_symbols: bool,
    /// Current shift / caps state.
    shift: ShiftState,
    /// Toolbar descriptors (data-driven, replaceable).
    toolbar: Vec<OskToolbarItem>,
    /// Configurable target row height (responsive sizing).
    target_key_height: f32,
    /// Toolbar row height.
    toolbar_height: f32,
    /// Suggestion-bar row height.
    suggestion_height: f32,
    /// Whether candidate chips are shown. Turning this off leaves the stable
    /// suggestion band in place, avoiding geometry churn while typing.
    suggestions_enabled: bool,
    /// Whether this process may adapt rankings from committed words. The
    /// learned cache itself is never persisted or collaborated.
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
    /// The suggestion-bar band (drawn even when empty, for a stable chrome).
    suggestion_area: Rect,
    /// The band the keys fill; touches that start here are tracked per
    /// finger (see [`Widget::claims_raw_touch`]).
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
    /// Offline next-word predictor, for `language`.
    predictor: Predictor,
    /// The language the preferred letter layout types; the predictor, the
    /// autocorrector and the swipe decoder all serve it.
    language: Language,
    /// The current ranked suggestion chips (swipe candidates or next-word
    /// predictions). Empty → the bar collapses to a hint row.
    suggestions: Vec<Suggestion>,
    /// The in-progress word at the caret (letters since the last word
    /// boundary). It is the field's preedit text: shown, not yet committed.
    current_word: String,
    /// Whether `current_word` came from a swipe. A swiped word is replaced as
    /// a whole (chip correction, Backspace) and is followed by an automatic
    /// space when the next word starts.
    word_from_swipe: bool,
    /// Once a word exceeds the predictor cap, do not begin tracking another
    /// suffix of that same word. It resets only at a real word boundary.
    current_word_overflowed: bool,
    /// The previously committed word, for bigram next-word context.
    prev_word: Option<String>,
    /// Word-boundary corrector, indexed on first use.
    autocorrect: Option<Autocorrector>,
    /// The correction just committed, revertible until the next action.
    last_correction: Option<Correction>,
    /// Where this user tends to hit each letter; adapts tap resolution.
    touch_model: TouchModel,
    /// Next-letter probabilities for the current typing context.
    next_key: NextKeyCache,
    /// Last pointer position over the widget (logical px).
    pointer: Option<Point>,
    /// While the left button is down, the captured pointer path (logical px).
    /// `Some` between press and release; `None` otherwise.
    gesture_path: Option<Vec<Point>>,
    /// Fingers currently down on the keys, in press order.
    fingers: Vec<Finger>,
    /// A key held long enough to act on its own: a long press or a repeat.
    hold: Option<Hold>,
    /// How often the typing language uses each accented letter, computed on
    /// the first long press in that language; orders the alternates row.
    accent_usage: Option<(Language, HashMap<char, f64>)>,
    /// The long-press alternates row and the contact that opened it (`None`
    /// for the pointer).
    alternates: Option<(Option<u64>, AlternatesPopup)>,
    /// A contact that started on the space bar; once it travels it moves the
    /// caret instead of typing.
    cursor_drag: Option<CursorDrag>,
    /// Paint the effective hit areas over the keys (a debug aid).
    hit_overlay: bool,

    // --- emoji ---
    emoji_panel: EmojiPanel,
    /// This user's recently picked emoji, persisted with the preferences.
    emoji_recents: emoji::Recents,
    /// The persisted recents last read or written, so `sync` parses only a
    /// change.
    recents_seen: String,
    /// The theme's spacing scale and metrics, captured at `build_layout`,
    /// for the emoji panel.
    spacing: ui::tokens::Spacing,
    metrics: ui::tokens::Metrics,

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
    drafts: PreferenceDrafts,
    actions: crate::resource::ActionSink,

    /// Whether the keyboard is on screen. This is not the keyboard's own
    /// choice — visibility belongs to the compositor, which shows the keyboard
    /// while a text input is focused and hides it otherwise, and relays that
    /// decision to the keyboard (and only the keyboard) over the input method.
    /// The keyboard is the one surface the OS floats and hides this way; an
    /// ordinary app has no equivalent. The default is `true` for the
    /// always-visible fallback used when no input method drives auto-show.
    /// When hidden the widget paints nothing.
    visible: bool,
}

impl Default for OskWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl OskWidget {
    /// A keyboard pre-loaded with QWERTY + AZERTY letter layouts and the
    /// symbols layer, the default toolbar, and default sizing. QWERTY is
    /// active.
    pub fn new() -> Self {
        Self::new_with_swipe_model(default_swipe_model())
    }

    /// Construct with a prepared decoder, without probing native model assets.
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
            layouts,
            active_letter: "qwerty".to_string(),
            preferred_text_layout: "qwerty".to_string(),
            purpose: InputPurpose::Text,
            hints: content_hint::NONE,
            sentence_start: true,
            sentence_end: false,
            showing_symbols: false,
            shift: ShiftState::Off,
            toolbar: OskToolbarItem::default_set(),
            target_key_height: DEFAULT_KEY_HEIGHT,
            toolbar_height: DEFAULT_TOOLBAR_HEIGHT,
            suggestion_height: DEFAULT_SUGGESTION_HEIGHT,
            suggestions_enabled: true,
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
            // Prefer the real FUTO ExecuTorch decoder when its libs +
            // models are present; otherwise fall back to the geometric one.
            swipe_model,
            predictor: Predictor::english(),
            language: Language::English,
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
            hold: None,
            accent_usage: None,
            alternates: None,
            cursor_drag: None,
            hit_overlay: false,
            emoji_panel: EmojiPanel::new(emoji::Catalogue::for_locale(
                &ui::i18n::I18nManager::current_locale(),
            )),
            emoji_recents: emoji::Recents::default(),
            recents_seen: String::new(),
            spacing: ui::tokens::Spacing::BASE,
            metrics: ui::tokens::Metrics::BASE,
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
            drafts: PreferenceDrafts::default(),
            actions: crate::resource::ActionSink::default(),
            visible: true,
        };
        // Seed the bar with cold-start (most-common-word) suggestions so it is
        // useful before the first keypress.
        me.refresh_next_word_suggestions();
        me
    }

    // --- builders -------------------------------------------------------

    /// Register an additional layout (e.g. a community AZERTY/Dvorak/emoji
    /// file parsed into a [`KeyboardLayout`]). Letter layouts become
    /// selectable via [`KeyAction::Layout`].
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

    /// Start the keyboard on the layout matching `purpose` (e.g. a dialpad for
    /// [`InputPurpose::Telephone`]). The needed layout is registered on demand.
    #[must_use]
    pub fn with_purpose(mut self, purpose: InputPurpose) -> Self {
        self.apply_purpose(purpose);
        self
    }

    /// Replace the toolbar with a custom data-driven set.
    #[must_use]
    pub fn with_toolbar(mut self, items: Vec<OskToolbarItem>) -> Self {
        self.toolbar = items;
        self
    }

    /// Set the responsive target key-row height (logical px).
    #[must_use]
    pub fn with_target_key_height(mut self, h: f32) -> Self {
        self.target_key_height = h.max(1.0);
        self
    }

    /// Replace the swipe decoder. The default is the geometric model; this is
    /// the seam for dropping in a FUTO/ExecuTorch [`SwipeModel`] later.
    #[must_use]
    pub fn with_swipe_model(mut self, model: Box<dyn SwipeModel>) -> Self {
        self.swipe_model = model;
        self
    }

    /// Replace the next-word predictor (e.g. a custom-corpus or a future
    /// neural [`Predictor`]). Selecting a letter layout of another language
    /// replaces it with that language's embedded predictor.
    #[must_use]
    pub fn with_predictor(mut self, predictor: Predictor) -> Self {
        self.predictor = predictor;
        self
    }

    /// Receives every [`OskOutput`] in the order the field must apply it.
    #[must_use]
    pub fn on_output<F: FnMut(OskOutput) + 'static>(mut self, f: F) -> Self {
        self.on_output = Some(Box::new(f));
        self
    }

    /// Imperative toolbar-tap callback.
    #[must_use]
    pub fn on_toolbar<F: FnMut(OskToolbarAction) + 'static>(mut self, f: F) -> Self {
        self.on_toolbar = Some(Box::new(f));
        self
    }

    // --- output accessors ----------------------------------------------

    /// What the focused field holds after the emitted output: committed text
    /// followed by the composing word.
    pub fn pending_text(&self) -> &str {
        self.mirror.text()
    }

    /// Drain the emitted output, oldest first (for hosts without an
    /// [`on_output`](Self::on_output) callback, and tests).
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

    /// Drain the log of emitted key actions (for hosts/tests that want the
    /// raw [`KeyAction`] stream rather than the flattened text).
    pub fn take_key_actions(&mut self) -> Vec<KeyAction> {
        self.key_log.drain(..).collect()
    }

    /// Drain the log of toolbar actions.
    pub fn take_toolbar_actions(&mut self) -> Vec<OskToolbarAction> {
        self.toolbar_log.drain(..).collect()
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

    /// Whether suggestions are visibly offered above the key grid.
    pub fn suggestions_enabled(&self) -> bool {
        self.suggestions_enabled
    }

    /// Whether this process may adapt future rankings from committed words.
    pub fn personalization_enabled(&self) -> bool {
        self.personalization_enabled
    }

    /// Toggle visible suggestions. This is a durable preference, while the
    /// candidate words themselves remain deliberately ephemeral.
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
        self.drafts.stage(Preference::Suggestions(enabled));
    }

    /// Toggle local adaptive ranking. Disabling it immediately forgets the
    /// in-memory profile; neither profile entries nor typed text are persisted.
    pub fn set_personalization_enabled(&mut self, enabled: bool) {
        if self.personalization_enabled == enabled {
            return;
        }
        self.personalization_enabled = enabled;
        if !enabled {
            self.predictor.clear_learned();
            self.touch_model.clear();
            if !self.emoji_recents.is_empty() {
                self.emoji_recents = emoji::Recents::default();
                self.stage_emoji_recents();
            }
        }
        self.refresh_next_word_suggestions();
        self.drafts.stage(Preference::Personalization(enabled));
    }

    /// Forget locally adapted suggestion ranking without changing the privacy
    /// preference. This has no effect on the bundled dictionary/bigram model.
    pub fn clear_personalization(&mut self) {
        self.predictor.clear_learned();
        self.touch_model.clear();
        if let Some(ac) = &mut self.autocorrect {
            ac.clear_kept();
        }
        if !self.emoji_recents.is_empty() {
            self.emoji_recents = emoji::Recents::default();
            self.stage_emoji_recents();
        }
    }

    // --- runtime configuration -----------------------------------------

    /// Switch the keyboard to the layout for `purpose` at runtime (the focused
    /// field changed). Registers the layout if absent, makes it active, drops
    /// the symbols layer, and raises `structure_dirty` so the host relays out
    /// (the row count differs between, say, the dialpad and QWERTY).
    pub fn set_purpose(&mut self, purpose: InputPurpose) {
        self.close_emoji_panel();
        self.apply_purpose(purpose);
        self.showing_symbols = false;
        self.structure_dirty = true;
        self.enforce_secrecy();
        self.apply_hint_case();
    }

    /// Apply the focused field's content hints ([`content_hint`] bits). A
    /// secret field drops any composition and suggestions immediately;
    /// capitalization hints set the shift state.
    pub fn set_content_hints(&mut self, hints: u32) {
        self.hints = hints;
        self.enforce_secrecy();
        self.apply_hint_case();
    }

    /// The focused field's content hints.
    pub fn content_hints(&self) -> u32 {
        self.hints
    }

    /// Whether the focused field holds a secret: nothing typed there is
    /// composed, suggested from or learned.
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

    /// Set the shift state a field's case hints ask for at a fresh start.
    fn apply_hint_case(&mut self) {
        if self.hints & content_hint::UPPERCASE != 0 {
            self.shift = ShiftState::CapsLock;
        } else if self.hints & content_hint::LOWERCASE != 0 {
            self.shift = ShiftState::Off;
        } else {
            self.auto_capitalize();
        }
    }

    /// One-shot shift at a sentence start in a field asking for automatic
    /// capitalization.
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
        if self.visible != visible {
            self.visible = visible;
            self.structure_dirty = true;
            self.fingers.clear();
            self.close_emoji_panel();
        }
    }

    /// Whether the keyboard is currently on screen.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    // --- state accessors -----------------------------------------------

    /// The field intent the keyboard is currently serving.
    pub fn purpose(&self) -> InputPurpose {
        self.purpose
    }

    pub fn shift_state(&self) -> ShiftState {
        self.shift
    }

    pub fn showing_symbols(&self) -> bool {
        self.showing_symbols
    }

    /// The language the keyboard predicts, corrects and decodes swipes in:
    /// the one the preferred letter layout types (QWERTY English, AZERTY
    /// French).
    pub fn language(&self) -> Language {
        self.language
    }

    /// The name of the currently *displayed* layout (`"symbols"` when the
    /// symbol layer is up, otherwise the active letter layout).
    pub fn active_layout_name(&self) -> &str {
        if self.showing_symbols {
            "symbols"
        } else {
            &self.active_letter
        }
    }

    /// Frame key geometry (id → rect), for hosts/tests/swipe decoding.
    pub fn key_geometry(&self) -> &[(KeyId, Rect)] {
        &self.key_geom
    }

    /// Frame toolbar geometry (index → rect).
    pub fn toolbar_geometry(&self) -> &[(usize, Rect)] {
        &self.toolbar_geom
    }

    /// Frame suggestion-bar geometry (chip index → rect).
    pub fn suggestion_geometry(&self) -> &[(usize, Rect)] {
        &self.suggestion_geom
    }

    /// The current suggestion chips (swipe candidates or next-word
    /// predictions), best-first.
    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// The in-progress word at the caret (letters since the last boundary).
    pub fn current_word(&self) -> &str {
        &self.current_word
    }

    /// Preferred alphabetic layout name (e.g. `"qwerty"`).
    pub fn preferred_text_layout(&self) -> &str {
        &self.preferred_text_layout
    }

    /// Label of the pressed or hovered key, when one is highlighted.
    pub(crate) fn highlighted_key_label(&self) -> Option<String> {
        let id = self.pressed_key.or(self.hovered_key)?;
        self.current_layout()
            .key(id)
            .map(|key| key.display_label(self.shift))
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Osk>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Adopt collaborative prefs when present — never clear a local layout
    /// that has not been published.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::OskPreferences) {
        if let Some(layout) = state.layout.as_deref() {
            self.select_text_layout_from_sync(layout);
        }
        if let Some(enabled) = state.suggestions {
            self.set_suggestions_enabled_from_sync(enabled);
        }
        if let Some(enabled) = state.personalization {
            self.set_personalization_enabled_from_sync(enabled);
        }
    }

    pub(crate) fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let mut choices = self
            .layouts
            .keys()
            .filter(|name| name.as_str() != "symbols")
            .cloned()
            .collect::<Vec<_>>();
        choices.sort();
        // Layout, suggestions, and personalization are the document. The
        // installed layout list stays on this device. An unwritten field
        // still uses the widget until sync stores it.
        let from_doc = <crate::resource::Osk as ui::tilo_resource::Resource>::read(doc).ok();
        let state = crate::resource::OskPreferences {
            layout: from_doc
                .as_ref()
                .and_then(|state| state.layout.clone())
                .or_else(|| Some(self.preferred_text_layout.clone())),
            suggestions: from_doc
                .as_ref()
                .and_then(|state| state.suggestions)
                .or(Some(self.suggestions_enabled)),
            personalization: from_doc
                .as_ref()
                .and_then(|state| state.personalization)
                .or(Some(self.personalization_enabled)),
            layout_choices: choices,
        };
        crate::resource::provide_resources(&state)
    }

    pub(crate) fn commit_preference(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::SetPreference,
    ) {
        self.actions.dispatch(doc, action);
        match &action.value {
            Preference::Layout(value) => self.select_text_layout_from_sync(value),
            Preference::Suggestions(value) => self.set_suggestions_enabled_from_sync(*value),
            Preference::Personalization(value) => {
                self.set_personalization_enabled_from_sync(*value);
            }
            Preference::EmojiRecents(value) => self.set_emoji_recents_from_sync(value.clone()),
        }
    }

    pub(crate) fn last_document_action(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // --- programmatic activation (test/host helpers) -------------------

    /// Activate a key by id as if tapped. No-op for an out-of-range id.
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

    /// Activate the first key whose *base* label matches `label`
    /// (e.g. `"a"`, `"⇧"`, `"?123"`), returning whether one was found.
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

    /// Activate a toolbar item by index as if tapped.
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
        if let Some(cb) = &mut self.on_toolbar {
            cb(action);
        }
    }

    // --- internals ------------------------------------------------------

    /// Make `purpose` the active intent: pick its layout name, register that
    /// layout on demand, set it active, and reset shift (a fresh numeric/pin
    /// pad should not carry a stale upper-case state). Shared by the builder
    /// [`with_purpose`](Self::with_purpose) and the runtime
    /// [`set_purpose`](Self::set_purpose).
    fn apply_purpose(&mut self, purpose: InputPurpose) {
        self.purpose = purpose;
        let name = if purpose == InputPurpose::Text {
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
        self.preferred_text_layout = name.to_string();
        self.active_letter = name.to_string();
        self.purpose = InputPurpose::Text;
        self.showing_symbols = false;
        self.shift = ShiftState::Off;
        self.structure_dirty = true;
        self.drafts.stage(Preference::Layout(name.into()));
        self.follow_layout_language();
        true
    }

    /// Switch the word data to the language the preferred letter layout
    /// types. The previous language's context, corrections and learned words
    /// do not carry over.
    fn follow_layout_language(&mut self) {
        let Some(language) = Language::for_layout(&self.preferred_text_layout) else {
            return;
        };
        if language == self.language {
            return;
        }
        self.language = language;
        self.predictor = Predictor::for_language(language);
        self.swipe_model.set_language(language);
        self.autocorrect = None;
        self.last_correction = None;
        self.prev_word = None;
        self.refresh_completion_suggestions();
    }

    fn select_text_layout_from_sync(&mut self, name: &str) {
        if name == "symbols"
            || !self.layouts.contains_key(name)
            || self.preferred_text_layout == name
        {
            return;
        }
        self.preferred_text_layout = name.to_string();
        if self.purpose == InputPurpose::Text {
            self.active_letter = name.to_string();
            self.showing_symbols = false;
            self.shift = ShiftState::Off;
            self.structure_dirty = true;
        }
        self.follow_layout_language();
    }

    fn set_suggestions_enabled_from_sync(&mut self, enabled: bool) {
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

    fn set_personalization_enabled_from_sync(&mut self, enabled: bool) {
        if self.personalization_enabled == enabled {
            return;
        }
        self.personalization_enabled = enabled;
        if !enabled {
            self.predictor.clear_learned();
            self.touch_model.clear();
            self.emoji_recents = emoji::Recents::default();
        }
        self.refresh_next_word_suggestions();
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
            // Symbols/active layout are always registered, but fall back to
            // the active letter layout defensively rather than panic.
            .or_else(|| self.layouts.get(&self.active_letter))
            // `layouts` always holds at least the symbols layer plus one
            // letter layout (see the field doc on `OskWidget::layouts`), so
            // this last-resort fallback always has something to return.
            .or_else(|| self.layouts.values().next())
            .unwrap_or_else(|| unreachable!("OskWidget always registers at least one layout"))
    }

    /// Apply an emitted [`KeyAction`]: update keyboard state, reduce it into
    /// [`OskOutput`]s and log it. Letters extend the composing word; spaces,
    /// punctuation and Return commit it; Backspace edits it while it exists
    /// and becomes a real key press otherwise.
    fn apply_action(&mut self, action: KeyAction) {
        let correction = self.last_correction.take();
        match &action {
            KeyAction::Char(s) => {
                // One-shot shift resets after a character; caps is sticky.
                if self.shift == ShiftState::Shift {
                    self.shift = ShiftState::Off;
                }
                if !s.is_empty() && s.chars().all(char::is_alphabetic) {
                    self.extend_word(s);
                } else {
                    self.finish_word(s);
                }
            }
            KeyAction::Space => self.finish_word(" "),
            KeyAction::Enter => {
                self.finish_word("");
                self.emit(OskOutput::Key(Symbol::Return));
                // A new line clears next-word context.
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
                self.select_text_layout(name);
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
            OskOutput::Key(Symbol::Return) => {
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

    /// Commit the composing word followed by `tail` (a space, punctuation, or
    /// nothing before an editing key). The word becomes the bigram context for
    /// next-word prediction and the bar is refilled with predictions.
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
            if self.personalization_enabled {
                self.predictor
                    .learn_transition(self.prev_word.as_deref(), &committed);
            }
            self.prev_word = Some(committed);
        }
        self.refresh_next_word_suggestions();
        if let Some(typed) = typed {
            // Offer the word as typed first, the way it was before the fix.
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

    /// Whether the focused field may be autocorrected: ordinary, non-secret
    /// text with suggestions on. Passwords, addresses and codes are typed
    /// verbatim.
    fn autocorrect_allowed(&self) -> bool {
        self.purpose == InputPurpose::Text && self.suggestions_enabled && !self.is_secret_field()
    }

    /// The correction for a finished `word`, if autocorrection applies and the
    /// word is neither known nor one this user has typed or kept before.
    fn correction_for(&mut self, word: &str) -> Option<String> {
        if word.is_empty() || !self.autocorrect_allowed() || self.predictor.has_learned(word) {
            return None;
        }
        let adjacency = KeyAdjacency::from_layout(self.current_layout());
        let dict = self.predictor.dictionary();
        let ac = self
            .autocorrect
            .get_or_insert_with(|| Autocorrector::new(dict, KeyAdjacency::default()));
        ac.set_adjacency(adjacency);
        ac.correct(word)
    }

    /// Undo a just-committed correction: erase it, commit the word as typed
    /// and never correct that word again.
    fn revert_correction(&mut self, c: Correction) {
        for _ in c.corrected.chars().chain(c.tail.chars()) {
            self.emit(OskOutput::Key(Symbol::BackSpace));
        }
        if let Some(ac) = &mut self.autocorrect {
            ac.keep(&c.typed);
        }
        self.prev_word = Some(c.typed.to_lowercase());
        let mut text = c.typed;
        text.push_str(&c.tail);
        self.emit(OskOutput::Commit(text));
        self.refresh_next_word_suggestions();
    }

    /// Backspace edits the composing word (a swiped word goes as a whole);
    /// with nothing composing it is a real key press.
    fn backspace(&mut self) {
        if self.current_word.is_empty() {
            self.emit(OskOutput::Key(Symbol::BackSpace));
            // Backspaced past the word start — context is now stale.
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
        self.hold = None;
        self.alternates = None;
        self.cursor_drag = None;
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
        self.refresh_next_word_suggestions();
        self.apply_hint_case();
    }

    /// Refill the bar with completions of the in-progress word (biased by the
    /// previous word). Called as the user types letters.
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

    /// Refill the bar with next-word predictions for the previous word.
    fn refresh_next_word_suggestions(&mut self) {
        if !self.suggestions_enabled || self.is_secret_field() {
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
        if s.emoji {
            // The word stays as typed; the emoji follows it.
            self.finish_word_with(" ", false);
            self.commit_emoji(&s.word, " ");
            return true;
        }
        self.current_word = s.word;
        if self.word_from_swipe {
            self.emit(OskOutput::SetPreedit(self.current_word.clone()));
        } else {
            self.finish_word(" ");
        }
        true
    }

    /// Decode a captured gesture `path` as a swipe. If it yields candidates,
    /// compose the top one (replacing a partly typed word, committing a
    /// previously swiped one with a space) and show the ranked alternatives in
    /// the bar. Returns whether the swipe produced a word.
    fn decode_and_apply_swipe(&mut self, path: &[Point]) -> bool {
        // A swiped word is composed and suggested from; a secret field takes
        // taps only.
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
        let top = top_cand.word.clone();
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
            .map(|c| Suggestion::new(c.word, c.score))
            .collect();
        self.add_emoji_suggestions();
        true
    }

    // --- emoji ------------------------------------------------------------

    /// This user's recently picked emoji, most recent first.
    pub fn emoji_recents(&self) -> &[String] {
        self.emoji_recents.items()
    }

    /// The emoji panel's state, for hosts and tests.
    pub fn emoji_panel(&self) -> &EmojiPanel {
        &self.emoji_panel
    }

    /// Save the recents in the keyboard's document through the preference
    /// action, like every other keyboard preference.
    fn stage_emoji_recents(&mut self) {
        if let Ok(recents) = serde_json::to_string(&self.emoji_recents) {
            self.recents_seen.clone_from(&recents);
            self.drafts.stage(Preference::EmojiRecents(recents));
        }
    }

    /// Adopt the recents saved in the keyboard's document, parsing only a
    /// change.
    fn set_emoji_recents_from_sync(&mut self, recents: String) {
        if recents == self.recents_seen {
            return;
        }
        if let Ok(parsed) = serde_json::from_str::<emoji::Recents>(&recents) {
            self.emoji_recents = parsed;
            self.layout_bands();
        }
        self.recents_seen = recents;
    }

    fn open_emoji_panel(&mut self) {
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

    /// Type `text` (an emoji) into the field, after any composing word left
    /// as typed, followed by `tail`. Remembered as recent unless the field is
    /// secret or personalization is off.
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

    /// A key pressed while the emoji search is open edits the query.
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
                self.select_text_layout(&name);
            }
            KeyAction::Emoji => {
                self.emoji_panel.end_search();
                self.structure_dirty = true;
            }
        }
        self.layout_bands();
    }

    /// Act on a panel target the user activated.
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

    /// Open the skin-tone row on `text`, where it was last laid out.
    fn open_emoji_tones(&mut self, text: &str) -> bool {
        let anchor = self
            .emoji_panel
            .emoji_rect(text)
            .unwrap_or(self.content_rect);
        let opened = self.emoji_panel.open_tones(text, anchor);
        self.layout_bands();
        opened
    }

    /// Pointer input while the emoji panel is open. Returns whether the panel
    /// consumed the event.
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
            // A long press, or a right click.
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

    /// Average key dimension of the current frame (the swipe/tap threshold
    /// scale). Falls back to a sane default before the first layout.
    fn key_scale(&self) -> f64 {
        if self.key_geom.is_empty() {
            return f64::from(self.target_key_height);
        }
        let sum: f64 = self
            .key_geom
            .iter()
            .map(|(_, r)| (r.width() + r.height()) * 0.5)
            .sum();
        sum / self.key_geom.len() as f64
    }

    /// Resolve a tap on letter key `hit` at `p` to the letter the user most
    /// likely meant (see [`crate::touch_model`]). Non-letter keys, and letter
    /// keys without a known pointer position, resolve to themselves.
    fn resolve_letter_tap(&mut self, hit: KeyId, p: Option<Point>) -> KeyId {
        let Some(p) = p else { return hit };
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
        }
        target
    }

    /// One raw finger event on the keys (see the module docs on touch).
    fn handle_touch(&mut self, touch: &Touch, scale_factor: f64, handled: &mut bool) {
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
                if f.path.last().is_none_or(|&l| l.distance(p) > 1.0) {
                    if f.path.len() < MAX_GESTURE_POINTS {
                        f.path.push(p);
                    } else if let Some(last) = f.path.last_mut() {
                        *last = p;
                    }
                }
                self.track_hold(Some(touch.id), p, key);
                *handled = true;
            }
            TouchPhase::Ended => {
                let Some(i) = self.fingers.iter().position(|f| f.id == touch.id) else {
                    return;
                };
                let mut f = self.fingers.remove(i);
                f.at = p;
                let finger = Some(touch.id);
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
                // The system took the contact back (e.g. a compositor
                // gesture): it types nothing.
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

    /// Type what finger `f` pressed: its swipe when it may swipe and its path
    /// reads as one, otherwise the key under it (resolved to the letter most
    /// likely meant).
    fn type_finger(&mut self, f: &Finger, may_swipe: bool) {
        if may_swipe
            && crate::swipe::is_swipe(&f.path, self.key_scale())
            && self.decode_and_apply_swipe(&f.path)
        {
            return;
        }
        // Hit-test afresh: a layer switch typed by another finger may have
        // replaced the keys since this one landed.
        if let Some(id) = self.key_at(f.at) {
            let target = self.resolve_letter_tap(id, Some(f.at));
            self.activate_key(target);
        }
    }

    /// Split the laid-out rect into the toolbar, suggestion and key bands and
    /// place what each shows: the keys, or the emoji panel's face.
    fn layout_bands(&mut self) {
        let geometry = keyboard_geometry(
            self.rect,
            self.current_layout(),
            &self.toolbar,
            &self.suggestions,
            self.shift,
            self.toolbar_height,
            self.suggestion_height,
        );
        self.content_rect = geometry.content_rect;
        self.toolbar_geom = geometry.toolbar;
        self.suggestion_geom = geometry.suggestions;
        // Stash the suggestion-bar area so an empty bar can still render its
        // background + hit-test correctly (the suggestion rects are empty then).
        self.suggestion_area = geometry.suggestion_area;
        self.key_area = geometry.key_area;
        self.key_geom = geometry.keys;
        let (toolbar_area, suggestion_area, key_area) = (
            geometry.toolbar_area,
            geometry.suggestion_area,
            geometry.key_area,
        );
        let mode = self.emoji_panel.mode();
        if mode != PanelMode::Closed {
            self.suggestion_geom.clear();
        }
        match mode {
            PanelMode::Closed => {}
            PanelMode::Browse => {
                // The panel takes touch as taps, drags and long presses.
                self.key_geom.clear();
                self.key_area = Rect::ZERO;
            }
            PanelMode::Search => self.toolbar_geom.clear(),
        }
        let bands = PanelBands {
            toolbar: toolbar_area,
            suggestion: suggestion_area,
            keys: key_area,
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
        self.emoji_panel.layout(bands, recents);
    }

    /// The left button came up: finish whatever its press started — an
    /// alternates pick, a caret drag or repeat (which already acted), a swipe
    /// or tap on a key, a suggestion or a toolbar item.
    fn pointer_released(&mut self, handled: &mut bool) {
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
            // The press already acted (caret moves, repeats).
            self.pressed_key = None;
            *handled = true;
        } else if let Some(id) = self.pressed_key.take() {
            // Decide swipe vs. tap from the captured path length.
            let scale = self.key_scale();
            let is_swipe = path
                .as_ref()
                .is_some_and(|p| crate::swipe::is_swipe(p, scale));
            if is_swipe {
                // A swipe: decode the path. If it yields nothing, fall
                // back to the key under the release point as a tap.
                let decoded = path
                    .as_ref()
                    .is_some_and(|p| self.decode_and_apply_swipe(p));
                if !decoded && self.hovered_key == Some(id) {
                    self.activate_key(id);
                }
            } else if self.hovered_key == Some(id) {
                // A short tap is a key press; a letter tap resolves to
                // the letter most likely meant near that point.
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

    // --- press and hold, space-bar caret ----------------------------------

    /// Arm the long press (alternates) or repeat (Backspace) for `key`, held
    /// by `finger` (`None` for the pointer).
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

    /// Fire a hold whose time has come. Called when the wake deadline from
    /// [`Widget::next_wake_deadline`] passes.
    fn poll_hold(&mut self, now: Instant) {
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

    /// The contact `finger` already acted; lifting it types nothing more.
    fn spend_finger(&mut self, finger: Option<u64>) {
        if let Some(f) = finger.and_then(|id| self.fingers.iter_mut().find(|f| f.id == id)) {
            f.spent = true;
        }
    }

    /// Type the alternate under the contact that opened the row, and close it.
    fn commit_alternate(&mut self) {
        let Some((_, popup)) = self.alternates.take() else {
            return;
        };
        if let Some(text) = popup.chosen() {
            let action = KeyAction::Char(text.to_string());
            if self.shift == ShiftState::Shift {
                // The chosen form is already cased; the one-shot shift is
                // used up as for any letter.
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

    /// Start watching a contact on the space bar for a caret drag.
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

    /// Move the caret with a space-bar drag now at `x`: nothing until it has
    /// travelled [`CURSOR_START`] keys, then one character per
    /// [`CURSOR_STEP`] keys of travel. Returns whether the drag steers.
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
            // The caret leaves the word: commit it as typed.
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
            self.emit(OskOutput::Key(symbol));
        }
        true
    }

    /// Whether the caret drag of `finger` is steering (so its lift types
    /// nothing).
    fn cursor_steering(&self, finger: Option<u64>) -> bool {
        self.cursor_drag
            .as_ref()
            .is_some_and(|d| d.finger == finger && d.active)
    }

    /// The character key to preview above the finger, if any.
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

    /// Show the effective tap targets over the keys (a debug aid).
    #[must_use]
    pub fn with_hit_overlay(mut self, on: bool) -> Self {
        self.hit_overlay = on;
        self
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

/// One finger on the keys.
#[derive(Clone, Debug)]
struct Finger {
    /// The contact's `Touch::id`.
    id: u64,
    /// The key under the finger, highlighted while it is down.
    key: Option<KeyId>,
    /// Latest position (logical px).
    at: Point,
    /// The path travelled, for swipe decoding.
    path: Vec<Point>,
    /// No other finger touched the keys during this contact, so it may swipe.
    solo: bool,
    /// Its key already typed because another finger landed; lifting it types
    /// nothing more.
    spent: bool,
}

/// How long a letter is held before its accented forms appear.
const LONG_PRESS: Duration = Duration::from_millis(350);
/// How long Backspace is held before it repeats, and how fast it then does.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_INTERVAL: Duration = Duration::from_millis(50);
/// Travel, in average key sizes, before a space-bar drag moves the caret,
/// and per character it then moves.
const CURSOR_START: f64 = 0.5;
const CURSOR_STEP: f64 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    /// Open the key's accented alternates.
    Alternates,
    /// Repeat the key (Backspace).
    Repeat,
}

/// A key held down with a timer running.
#[derive(Clone, Copy, Debug)]
struct Hold {
    key: KeyId,
    /// The contact's `Touch::id`, or `None` for the pointer.
    finger: Option<u64>,
    /// When the hold next acts.
    due: Instant,
    kind: HoldKind,
    /// It has acted at least once, so its release types nothing.
    fired: bool,
}

/// A contact that began on the space bar.
#[derive(Clone, Copy, Debug)]
struct CursorDrag {
    finger: Option<u64>,
    /// Where the last caret step was taken (or the contact began).
    anchor: f64,
    /// It travelled far enough to steer the caret.
    active: bool,
}

/// An autocorrection committed at the last word boundary.
#[derive(Clone, Debug)]
struct Correction {
    typed: String,
    corrected: String,
    /// The space or punctuation committed after the word.
    tail: String,
}

impl OskWidget {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != OperationKind::Effect || type_id != "osk.suggestions" {
            return Ok(false);
        }
        let value: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        if value != "clear-personal" {
            return Ok(false);
        }
        self.clear_personalization();
        Ok(true)
    }
}

impl Widget for OskWidget {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["osk.suggestions"]
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.poll_hold(Instant::now());
        if !self.drafts.has(0) {
            if let Some(layout) = read_str(state, path, CONFIG_LAYOUT_KEY) {
                // Remote/persisted config cannot introduce an arbitrary layout: it
                // must still be one registered in this OSK process.
                self.select_text_layout_from_sync(&layout);
            } else if matches!(state.get(path, CONFIG_LAYOUT_KEY), Ok(None)) {
                self.select_text_layout_from_sync("qwerty");
            }
        }
        if !self.drafts.has(1) {
            if let Some(value) = read_bool(state, path, CONFIG_SUGGESTIONS_KEY) {
                self.set_suggestions_enabled_from_sync(value);
            } else if matches!(state.get(path, CONFIG_SUGGESTIONS_KEY), Ok(None)) {
                self.set_suggestions_enabled_from_sync(true);
            }
        }
        if !self.drafts.has(2) {
            if let Some(value) = read_bool(state, path, CONFIG_PERSONALIZATION_KEY) {
                self.set_personalization_enabled_from_sync(value);
            } else if matches!(state.get(path, CONFIG_PERSONALIZATION_KEY), Ok(None)) {
                self.set_personalization_enabled_from_sync(true);
            }
        }
        if !self.drafts.has(3)
            && let Some(recents) = read_str(state, path, CONFIG_EMOJI_RECENTS_KEY)
        {
            self.set_emoji_recents_from_sync(recents);
        }
        let resources = ui::resources::resources(self, state);
        self.drafts.prepare_after_sync(&resources);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        let rows = self.current_layout().row_count();
        // Toolbar + suggestion bar + key rows. The suggestion bar is always
        // reserved (it shows next-word predictions even before any swipe), so
        // the keyboard height is stable as suggestions come and go. When the
        // compositor has hidden the keyboard it requests no height and paints
        // nothing — the real overlay surface is mapped/unmapped by the
        // compositor, so a hidden keyboard occupies no surface either way.
        let total_h = if self.visible {
            self.toolbar_height + self.suggestion_height + rows as f32 * self.target_key_height
        } else {
            0.0
        };
        let node = match taffy.new_leaf(Style {
            flex_shrink: 0.0,
            size: Size {
                // Fill the available width (phone → desktop); intrinsic
                // height from the row count × target key height.
                width: Dimension::percent(1.0),
                height: Dimension::length(total_h),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            // `TaffyTree::new_leaf` only ever returns `Err` when the tree is
            // at capacity; this arm exists so a future upstream change
            // degrades the app instead of aborting the whole session.
            Err(_) => NodeId::from(0u64),
        };
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
        // A hidden keyboard paints nothing (and `build_layout` gave it zero
        // height, so there is no area to fill anyway).
        if !self.visible {
            return;
        }
        let empty_hint = if self.suggestions_enabled {
            "Type or swipe to see suggestions"
        } else {
            "Suggestions are off"
        };
        let finger_keys: Vec<KeyId> = self
            .fingers
            .iter()
            .filter(|f| !f.spent)
            .filter_map(|f| f.key)
            .collect();
        let preview = self.preview_key(&finger_keys);
        let hit_cells = if self.hit_overlay && !self.emoji_panel.is_open() {
            // Sampled every 2 px: a debug resolution, not a layout size.
            self.hit_cells(2.0)
        } else {
            Vec::new()
        };
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
            hovered_sugg: self.hovered_sugg,
            pressed_sugg: self.pressed_sugg,
            empty_hint,
            emoji_panel_open: self.emoji_panel.is_open(),
            preview,
            alternates: self.alternates.as_ref().map(|(_, popup)| popup),
            cursor_mode: self.cursor_drag_active(),
            hit_cells: &hit_cells,
        }
        .draw(scene, transform, text_renderer, theme);

        // Last, so the skin-tone row floats over the keys.
        self.emoji_panel
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // A hidden keyboard occupies no area and draws nothing, so it should
        // not consume or react to pointer events meant for the app behind it.
        if !self.visible {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if self.handle_panel_event(event, ctx.scale_factor, ctx.handled) {
            return;
        }
        match event {
            WindowEvent::Touch(touch) => self.handle_touch(touch, ctx.scale_factor, ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.pointer = Some(p);
                // While the button is held over the keys, accumulate the
                // gesture path for swipe decoding.
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
                // Mirror `Button`: press latches onto whatever the last
                // CursorMoved hovered.
                if let Some(id) = self.hovered_key {
                    self.pressed_key = Some(id);
                    // Begin capturing a gesture path from the pressed key's
                    // centre (a swipe usually starts there).
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
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(item.label.clone())
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(format!("tool:{}:{}", index, item.label))
                    .with_action_target(format!("osk:tool:{}:{}", index, item.label)),
            );
        }
        for (index, rect) in &self.suggestion_geom {
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
                format!("Insert {}", suggestion.word)
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
            let name = if key.action == KeyAction::Emoji {
                i18n::t("osk-emoji-key")
            } else {
                label.clone()
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
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
                .with_name("On-screen keyboard")
                .with_description(if self.suggestions_enabled {
                    "Keyboard keys, suggestions, and input controls"
                } else {
                    "Keyboard keys and input controls. Suggestions are turned off."
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        _state: &mut AppModel,
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
        if let Some(rest) = target.strip_prefix("osk:tool:") {
            let Some((index, label)) = rest.split_once(':') else {
                return false;
            };
            let Ok(index) = index.parse::<usize>() else {
                return false;
            };
            if self
                .toolbar
                .get(index)
                .is_none_or(|item| item.label != label)
            {
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

    /// Out-of-band commands from the host's input-method thread, which runs on
    /// its own Wayland connection and can't touch this widget directly. The
    /// compositor decides when the keyboard is shown (a text input is focused)
    /// and relays that decision — only to the keyboard — over the input method;
    /// the host forwards it here as short text commands so the keyboard
    /// shows/hides and re-layouts in step with the focused field:
    ///
    /// * `osk:show` / `osk:hide` — apply the compositor's show/hide decision.
    /// * `osk:field` — a text field gained or lost focus; drop the composition
    ///   and typed-text traces that belonged to the previous one.
    /// * `osk:hints:<bits>` — the focused field's content hints.
    /// * `osk:purpose:<name>` — switch to the layout for an [`InputPurpose`]
    ///   (`text`, `number`, `decimal`, `phone`, `url`, `email`, `pin`,
    ///   `password`, `date`, `time`). An unknown name is ignored.
    ///
    /// Unrecognised payloads are ignored so an unrelated `External` event can't
    /// disturb the keyboard.
    fn on_external_event(&mut self, payload: &str, _state: &mut AppModel, _root: &ModelPath) {
        match payload {
            "osk:show" => self.set_visible(true),
            "osk:hide" => self.set_visible(false),
            "osk:field" => self.reset_composition(),
            other => {
                if let Some(name) = other.strip_prefix("osk:purpose:")
                    && let Some(purpose) = input_purpose_from_name(name)
                {
                    self.set_purpose(purpose);
                } else if let Some(bits) = other.strip_prefix("osk:hints:")
                    && let Ok(hints) = bits.parse::<u32>()
                {
                    self.set_content_hints(hints);
                }
            }
        }
    }
}

impl ui::resources::PublishesResources for OskWidget {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the keyboard door — keep only live compose / key-highlight extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.is_hosted() && id == crate::actions::KEYBOARD_RESOURCE_ID {
            return None;
        }
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id).or_else(|| {
            self.is_hosted().then(|| {
                crate::resource::live_extras(self)
                    .into_iter()
                    .find(|resource| resource.id.as_str() == id)
            })?
        })
    }
}

impl ui::widget::controls::ActionControls for OskWidget {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.drafts.take()
    }
}

impl ui::history_mode::capability::DocumentHistory for OskWidget {
    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        if scope == crate::framework_history::SCOPE && self.drafts.pending() {
            Err("Return to live to finish pending keyboard preferences before restoring".into())
        } else {
            Ok(())
        }
    }
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(crate::framework_history::definition(
            self.layouts.get("qwerty") == Some(&KeyboardLayout::qwerty()),
            self.layouts.get("azerty") == Some(&KeyboardLayout::azerty()),
        ))
    }
    fn document_action_applied(
        &mut self,
        action: &ui::tilo_actions::EncodedAction,
        _before: &[automerge::ChangeHash],
        doc: &AutoCommit,
        _path: &ObjId,
    ) {
        let Some(action) = crate::actions::decode(action) else {
            return;
        };
        if self.drafts.acknowledge(&action, doc) {
            match action.value {
                Preference::Layout(value) => self.select_text_layout_from_sync(&value),
                Preference::Suggestions(value) => self.set_suggestions_enabled_from_sync(value),
                Preference::Personalization(value) => {
                    self.set_personalization_enabled_from_sync(value);
                }
                Preference::EmojiRecents(value) => self.set_emoji_recents_from_sync(value),
            }
        }
    }
}

/// Parse the canonical name of an [`InputPurpose`] (as used in the `osk:purpose:`
/// command). Returns `None` for an unknown name so the caller can ignore it.
pub fn input_purpose_from_name(name: &str) -> Option<InputPurpose> {
    Some(match name {
        "text" => InputPurpose::Text,
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

/// The canonical name for an [`InputPurpose`], the inverse of
/// [`input_purpose_from_name`]. Used to build the `osk:purpose:<name>` command.
pub fn input_purpose_name(purpose: InputPurpose) -> &'static str {
    match purpose {
        InputPurpose::Text => "text",
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

/// Test readers for the persisted configuration keys (the framework History
/// tests assert on the stored document through these names).
#[cfg(test)]
fn read_config_text(state: &impl ReadDoc, path: &ObjId, key: &str) -> Option<String> {
    read_str(state, path, key)
}
#[cfg(test)]
fn read_config_bool(state: &impl ReadDoc, path: &ObjId, key: &str) -> Option<bool> {
    read_bool(state, path, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::MAX_KEYBOARD_CONTENT_WIDTH;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn lay_out(osk: &mut OskWidget, w: f32, h: f32) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
            )
            .unwrap();
        osk.apply_layout(&taffy, child, Point::ORIGIN);
    }

    #[test]
    fn key_taps_edit_pending_text_and_fire_callbacks() {
        eprintln!("phase tapping_letter_appends_to_pending_text");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            assert!(osk.activate_label("h"));
            assert!(osk.activate_label("i"));
            assert_eq!(osk.pending_text(), "hi");
        }

        eprintln!("phase tapping_letter_emits_char_action_and_fires_callback");
        {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let mut osk = {
                let seen = seen.clone();
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                    .on_output(move |o| seen.borrow_mut().push(o))
            };
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("a");
            let log = osk.take_key_actions();
            assert_eq!(log, vec![KeyAction::Char("a".into())]);
            assert_eq!(*seen.borrow(), vec![pre("a")]);
        }

        eprintln!("phase backspace_removes_last_char");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("h");
            osk.activate_label("i");
            osk.activate_label("⌫");
            assert_eq!(osk.pending_text(), "h");
        }

        eprintln!("phase take_output_drains");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("h");
            osk.activate_label("space");
            assert_eq!(osk.take_output(), vec![pre("h"), commit("h ")]);
            assert!(osk.take_output().is_empty(), "drained");
            assert_eq!(osk.pending_text(), "h ");
        }
    }

    fn pre(s: &str) -> OskOutput {
        OskOutput::SetPreedit(s.into())
    }

    fn commit(s: &str) -> OskOutput {
        OskOutput::Commit(s.into())
    }

    #[test]
    fn typed_word_composes_as_preedit_and_commits_at_space() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("space");
        osk.activate_label("m");
        assert_eq!(
            osk.take_output(),
            vec![pre("h"), pre("hi"), commit("hi "), pre("m")]
        );
        assert_eq!(osk.pending_text(), "hi m");
    }

    fn type_word(osk: &mut OskWidget, word: &str) {
        for c in word.chars() {
            osk.apply_action(KeyAction::Char(c.to_string()));
        }
    }

    #[test]
    fn space_autocorrects_a_non_word_and_offers_it_as_typed() {
        let mut osk = OskWidget::new();
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.pending_text(), "the ");
        assert_eq!(osk.suggestions()[0].word, "\u{201c}teh\u{201d}");
        type_word(&mut osk, "tge");
        osk.apply_action(KeyAction::Char(",".into()));
        assert_eq!(osk.pending_text(), "the the,");
    }

    #[test]
    fn backspace_after_a_correction_reverts_it_and_keeps_the_word() {
        let mut osk = OskWidget::new();
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        osk.take_output();
        osk.apply_action(KeyAction::Backspace);
        assert_eq!(
            osk.take_output(),
            vec![
                OskOutput::Key(Symbol::BackSpace),
                OskOutput::Key(Symbol::BackSpace),
                OskOutput::Key(Symbol::BackSpace),
                OskOutput::Key(Symbol::BackSpace),
                commit("teh "),
            ]
        );
        assert_eq!(osk.pending_text(), "teh ");
        // Kept as typed: the same word is no longer corrected.
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.pending_text(), "teh teh ");
        // A second Backspace is an ordinary one.
        osk.apply_action(KeyAction::Backspace);
        assert_eq!(osk.pending_text(), "teh teh");
    }

    #[test]
    fn as_typed_chip_reverts_a_correction() {
        let mut osk = OskWidget::new();
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert!(osk.activate_suggestion(0));
        assert_eq!(osk.pending_text(), "teh ");
    }

    #[test]
    fn words_the_user_has_typed_are_never_corrected() {
        let mut predictor = Predictor::english();
        predictor.learn_transition(None, "teh");
        let mut osk = OskWidget::new().with_predictor(predictor);
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.pending_text(), "teh ");
    }

    #[test]
    fn password_fields_are_never_autocorrected() {
        let mut osk = OskWidget::new().with_purpose(InputPurpose::Password);
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.pending_text(), "teh ");
    }

    #[test]
    fn sensitive_text_fields_are_never_autocorrected() {
        let mut osk = OskWidget::new();
        osk.set_content_hints(content_hint::SENSITIVE_DATA);
        type_word(&mut osk, "teh");
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.pending_text(), "teh ");
    }

    #[test]
    fn punctuation_commits_the_word_with_it() {
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("o".into()));
        osk.apply_action(KeyAction::Char("k".into()));
        osk.apply_action(KeyAction::Char(",".into()));
        osk.apply_action(KeyAction::Char(".".into()));
        assert_eq!(
            osk.take_output(),
            vec![pre("o"), pre("ok"), commit("ok,"), commit(".")]
        );
    }

    #[test]
    fn enter_commits_the_word_before_the_return_key() {
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("g".into()));
        osk.apply_action(KeyAction::Char("o".into()));
        osk.apply_action(KeyAction::Enter);
        osk.apply_action(KeyAction::Enter);
        assert_eq!(
            osk.take_output(),
            vec![
                pre("g"),
                pre("go"),
                commit("go"),
                OskOutput::Key(Symbol::Return),
                OskOutput::Key(Symbol::Return),
            ]
        );
        assert_eq!(osk.pending_text(), "go\n\n");
    }

    #[test]
    fn backspace_edits_the_preedit_then_becomes_a_key() {
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("a".into()));
        osk.apply_action(KeyAction::Space);
        osk.apply_action(KeyAction::Char("n".into()));
        osk.apply_action(KeyAction::Char("o".into()));
        osk.take_output();
        osk.apply_action(KeyAction::Backspace);
        osk.apply_action(KeyAction::Backspace);
        // Nothing composing: the next Backspace deletes committed text, which
        // only a key press can do.
        osk.apply_action(KeyAction::Backspace);
        osk.apply_action(KeyAction::Char("t".into()));
        assert_eq!(
            osk.take_output(),
            vec![
                pre("n"),
                pre(""),
                OskOutput::Key(Symbol::BackSpace),
                pre("t"),
            ]
        );
        assert_eq!(osk.pending_text(), "at");
    }

    #[test]
    fn shift_uppercases_next_letter_then_resets() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.shift_state(), ShiftState::Off);
        osk.activate_label("⇧");
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        osk.activate_label("a"); // -> "A", one-shot shift resets
        osk.activate_label("b"); // -> "b"
        assert_eq!(osk.pending_text(), "Ab");
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }

    #[test]
    fn caps_lock_is_sticky() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("⇧"); // Off -> Shift
        osk.activate_label("⇧"); // Shift -> CapsLock
        assert_eq!(osk.shift_state(), ShiftState::CapsLock);
        osk.activate_label("a");
        osk.activate_label("b");
        assert_eq!(osk.pending_text(), "AB");
        assert_eq!(osk.shift_state(), ShiftState::CapsLock, "caps stays on");
    }

    #[test]
    fn symbols_toggle_switches_layer() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        assert!(!osk.showing_symbols());
        osk.activate_label("?123");
        assert!(osk.showing_symbols());
        assert_eq!(osk.active_layout_name(), "symbols");
        // Re-layout (host would after structure_dirty) and type a digit.
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("1"));
        assert_eq!(osk.pending_text(), "1");
        // The structure-dirty flag was raised by the toggle.
    }

    #[test]
    fn toggle_raises_structure_dirty() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("?123");
        assert!(osk.take_structure_dirty());
        assert!(!osk.take_structure_dirty(), "flag is rising-edge");
    }

    #[test]
    fn resources_and_accessibility_expose_configuration_not_keystrokes() {
        eprintln!("phase accessibility_exposes_and_operates_each_painted_key");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            let semantics = osk.accessibility().expect("visible keyboard semantics");
            let q = semantics
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("q"))
                .expect("q key semantic node");
            assert_eq!(q.role, Role::Button);
            assert!(q.bounds.is_some());
            let target = q.action_target.as_deref().expect("stable action target");
            assert!(
                ui::widget::controls::control_action_bindings(&osk)
                    .iter()
                    .any(|binding| {
                        binding.control_id == target
                            && matches!(
                                binding.kind,
                                ui::widget::ControlBindingKind::SemanticAction(_)
                            )
                    })
            );
            assert!(osk.perform_semantic_action(
                target,
                &AccessibilityAction::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(osk.pending_text(), "q");
        }

        eprintln!("phase resource_catalogue_exports_configuration_not_keystrokes");
        {
            let osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            let resources = ui::resources::resources(&osk, &AutoCommit::new());
            let keyboard = resources.first().expect("keyboard resource");
            assert!(
                keyboard.actions.iter().all(|action| {
                    action.id == ui::tilo_resource::APPLY
                        || action.id == ui::tilo_resource::GROUP
                        || action.id.starts_with("set-")
                }),
                "root carries preference declarations and programmatic apply/group, not keystrokes: {:?}",
                keyboard
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>()
            );
            assert_eq!(keyboard.children.len(), 3);
            let layout = keyboard
                .children
                .iter()
                .find(|resource| resource.id.0 == "osk.keyboard.layout")
                .expect("layout configuration resource");
            assert!(layout.actions.iter().any(|action| action.id == "qwerty"));
            assert!(layout.actions.iter().any(|action| action.id == "azerty"));
            assert_eq!(osk.local_resource_operation_handlers(), ["osk.suggestions"]);
        }

        eprintln!("phase configuration_resource_persists_and_restores_preferred_layout");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            let mut doc = AutoCommit::new();
            let resources = ui::resources::resources(&osk, &doc);
            let encoded = ui::tilo_actions::door::encode(
                &resources,
                &ui::tilo_actions::ActionAddress::new("osk.keyboard.layout", "azerty"),
                None,
            )
            .unwrap();
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            crate::actions::register(&mut registry);
            let action = registry.decode(&encoded).unwrap();
            action.apply(&mut doc).unwrap();
            ui::history_mode::capability::document_action_applied(
                &mut osk,
                &encoded,
                &[],
                &doc,
                &automerge::ROOT,
            );
            osk.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                read_str(&doc, &automerge::ROOT, CONFIG_LAYOUT_KEY).as_deref(),
                Some("azerty")
            );

            let mut restored =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            restored.sync(&mut doc, &automerge::ROOT);
            assert_eq!(restored.active_layout_name(), "azerty");
        }

        eprintln!("phase disabled_suggestions_are_announced_and_not_materialised");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            osk.set_suggestions_enabled(false);
            lay_out(&mut osk, 720.0, 360.0);
            assert!(osk.suggestions().is_empty());
            let node = osk.accessibility().expect("visible semantics");
            assert!(
                node.description
                    .as_deref()
                    .is_some_and(|text| text.contains("turned off"))
            );
        }
    }

    #[test]
    fn shift_caps_and_symbol_layers_toggle_and_mark_structure_dirty() {
        eprintln!("phase shift_uppercases_next_letter_then_resets");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            assert_eq!(osk.shift_state(), ShiftState::Off);
            osk.activate_label("⇧");
            assert_eq!(osk.shift_state(), ShiftState::Shift);
            osk.activate_label("a"); // -> "A", one-shot shift resets
            osk.activate_label("b"); // -> "b"
            assert_eq!(osk.pending_text(), "Ab");
            assert_eq!(osk.shift_state(), ShiftState::Off);
        }

        eprintln!("phase caps_lock_is_sticky");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("⇧"); // Off -> Shift
            osk.activate_label("⇧"); // Shift -> CapsLock
            assert_eq!(osk.shift_state(), ShiftState::CapsLock);
            osk.activate_label("a");
            osk.activate_label("b");
            assert_eq!(osk.pending_text(), "AB");
            assert_eq!(osk.shift_state(), ShiftState::CapsLock, "caps stays on");
        }

        eprintln!("phase symbols_toggle_switches_layer");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            assert_eq!(osk.active_layout_name(), "qwerty");
            assert!(!osk.showing_symbols());
            osk.activate_label("?123");
            assert!(osk.showing_symbols());
            assert_eq!(osk.active_layout_name(), "symbols");
            // Re-layout (host would after structure_dirty) and type a digit.
            lay_out(&mut osk, 800.0, 360.0);
            assert!(osk.activate_label("1"));
            assert_eq!(osk.pending_text(), "1");
            // The structure-dirty flag was raised by the toggle.
        }

        eprintln!("phase toggle_raises_structure_dirty");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("?123");
            assert!(osk.take_structure_dirty());
            assert!(!osk.take_structure_dirty(), "flag is rising-edge");
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn pointer_and_toolbar_taps_route_to_their_targets() {
        eprintln!("phase pointer_tap_routes_to_key_under_cursor");
        {
            use winit::event::{DeviceId, Modifiers};
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);

            // Pick the rect of the 'q' key (row 0, col 0) and aim at its centre.
            let target = KeyId::new(0, 0);
            let rect = osk
                .key_geometry()
                .iter()
                .find(|(id, _)| *id == target)
                .map(|(_, r)| *r)
                .expect("q key laid out");
            let c = rect.center();

            let device: DeviceId = unsafe { std::mem::zeroed() };
            let mods = Modifiers::default();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
                let mut handled = false;
                let mut focus = None;
                osk.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: NodeId::new(0),
                        taffy: &TaffyTree::new(),
                    },
                );
            };

            dispatch(
                &mut osk,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (c.x, c.y).into(),
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert_eq!(osk.pending_text(), "q", "tap over 'q' typed q");
        }

        eprintln!("phase toolbar_tap_emits_action");
        {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let mut osk = {
                let seen = seen.clone();
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                    .on_toolbar(move |a| seen.borrow_mut().push(a))
            };
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_tool(0); // default[0] == Paste
            assert_eq!(osk.take_toolbar_actions(), vec![OskToolbarAction::Paste]);
            assert_eq!(*seen.borrow(), vec![OskToolbarAction::Paste]);
        }

        eprintln!("phase layout_action_switches_active_letter_layout");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.apply_action(KeyAction::Layout("azerty".into()));
            assert_eq!(osk.active_layout_name(), "azerty");
            lay_out(&mut osk, 800.0, 360.0);
            // AZERTY row 0 starts with 'a'.
            assert!(osk.activate_label("a"));
            assert_eq!(osk.pending_text(), "a");
        }

        eprintln!("phase letter_layout_selects_the_word_language");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            assert_eq!(osk.language(), Language::English);
            lay_out(&mut osk, 800.0, 360.0);
            osk.apply_action(KeyAction::Layout("azerty".into()));
            assert_eq!(osk.language(), Language::French);
            lay_out(&mut osk, 800.0, 360.0);
            for letter in ["t", "r", "e", "s"] {
                assert!(osk.activate_label(letter));
            }
            let words: Vec<&str> = osk.suggestions().iter().map(|s| s.word.as_str()).collect();
            assert!(words.contains(&"très"), "French completes tres: {words:?}");
            osk.apply_action(KeyAction::Layout("qwerty".into()));
            assert_eq!(osk.language(), Language::English);
        }

        eprintln!("phase hidden_keyboard_ignores_pointer_events");
        {
            // A hidden keyboard must not consume taps meant for the app behind it:
            // `handle_event` returns early and `handled` stays false.
            use winit::event::{DeviceId, Modifiers};
            let mut osk =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
            lay_out(&mut osk, 800.0, 360.0);

            let device: DeviceId = unsafe { std::mem::zeroed() };
            let mods = Modifiers::default();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            let mut focus = None;
            // Aim where the 'q' key would be on a visible keyboard.
            osk.handle_event(
                &WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: NodeId::new(0),
                    taffy: &TaffyTree::new(),
                },
            );
            assert!(!handled, "a hidden keyboard does not consume pointer input");
            assert_eq!(osk.pending_text(), "");
        }
    }

    // --- input-type layouts via purpose --------------------------------

    #[test]
    fn purpose_changes_swap_layouts_and_clear_stale_state() {
        eprintln!("phase set_purpose_swaps_active_layout_and_marks_dirty");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            assert_eq!(osk.active_layout_name(), "qwerty");
            osk.set_purpose(InputPurpose::Telephone);
            assert_eq!(osk.purpose(), InputPurpose::Telephone);
            assert_eq!(osk.active_layout_name(), "phone");
            assert!(osk.take_structure_dirty(), "purpose change relays out");
        }

        eprintln!("phase switching_purpose_then_back_registers_layouts_on_demand");
        {
            // Tilo only preloads qwerty/azerty/symbols; the specialised layouts must
            // be registered the first time a purpose needs them.
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            osk.set_purpose(InputPurpose::Email);
            lay_out(&mut osk, 800.0, 360.0);
            assert_eq!(osk.active_layout_name(), "email");
            assert!(osk.activate_label("@"));
            osk.set_purpose(InputPurpose::Text);
            lay_out(&mut osk, 800.0, 360.0);
            assert_eq!(osk.active_layout_name(), "qwerty");
        }

        eprintln!("phase purpose_change_clears_stale_shift");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 800.0, 360.0);
            osk.activate_label("⇧");
            assert_eq!(osk.shift_state(), ShiftState::Shift);
            // Moving to a number pad must drop the now-meaningless shift state.
            osk.set_purpose(InputPurpose::Number);
            assert_eq!(osk.shift_state(), ShiftState::Off);
        }
    }

    #[test]
    fn initial_purpose_selects_matching_layout_and_names_round_trip() {
        eprintln!("phase with_purpose_starts_on_the_matching_layout");
        {
            let osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .with_purpose(InputPurpose::Pin);
            assert_eq!(osk.purpose(), InputPurpose::Pin);
            assert_eq!(osk.active_layout_name(), "pin");
        }

        eprintln!("phase numeric_purpose_types_digits_and_has_no_letters");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            osk.set_purpose(InputPurpose::Number);
            lay_out(&mut osk, 800.0, 360.0);
            assert!(osk.activate_label("1"));
            assert!(osk.activate_label("2"));
            assert!(osk.activate_label("3"));
            assert_eq!(osk.pending_text(), "123");
            // There is no Shift key to tap on a number pad.
            assert!(!osk.activate_label("⇧"), "no Shift on the number pad");
        }

        eprintln!("phase input_purpose_name_round_trips");
        {
            use InputPurpose::*;
            for p in [
                Text, Number, Decimal, Telephone, Url, Email, Pin, Password, Date, Time,
            ] {
                let name = input_purpose_name(p);
                assert_eq!(
                    input_purpose_from_name(name),
                    Some(p),
                    "{p:?} should round-trip through its name {name:?}"
                );
            }
        }
    }

    #[test]
    fn offscreen_render_and_geometry_follow_layout_and_suggestions() {
        eprintln!("phase numeric_layout_renders_offscreen");
        {
            // A non-letter layout must lay out and draw without panicking even
            // though it has no Shift / symbols affordances.
            use ui::offscreen::OffscreenRenderer;
            use ui::permissions::PermissionContext;
            use vello::peniko::Color;

            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .with_purpose(InputPurpose::Telephone);
            lay_out(&mut osk, 720.0, 360.0);
            assert!(!osk.key_geometry().is_empty(), "dialpad laid out keys");

            let theme = Theme::dark();
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            osk.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );

            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("osk_dialpad.png");
            let mut renderer = OffscreenRenderer::new(720, 360);
            renderer.render_png(&scene, Color::BLACK, &out);
            let bytes = std::fs::read(&out).unwrap();
            assert!(bytes.len() > 1000, "rendered a non-trivial PNG");
        }

        eprintln!("phase offscreen_render_includes_suggestion_bar");
        {
            // A full offscreen frame with a populated suggestion bar must produce
            // non-empty pixels (the bar + chips draw).
            use ui::offscreen::OffscreenRenderer;
            use ui::permissions::PermissionContext;
            use vello::peniko::Color;

            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            // Populate the bar via a swipe.
            let path = path_through(&osk, "quick");
            assert!(osk.swipe(&path));
            assert!(!osk.suggestions().is_empty());
            lay_out(&mut osk, 720.0, 360.0);

            let theme = Theme::dark();
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            osk.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );

            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("osk_suggestion_bar.png");
            let mut renderer = OffscreenRenderer::new(720, 360);
            renderer.render_png(&scene, Color::BLACK, &out);
            let bytes = std::fs::read(&out).unwrap();
            assert!(
                bytes.len() > 1000,
                "rendered a non-trivial PNG ({} bytes)",
                bytes.len()
            );
        }

        eprintln!("phase suggestion_bar_geometry_matches_suggestion_count");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            // Cold start: the bar shows common-word predictions, so geometry is
            // populated and chips fit within the suggestion band.
            let path = path_through(&osk, "hello");
            osk.swipe(&path);
            // Re-layout so suggestion_rects is rebuilt from the new suggestions.
            lay_out(&mut osk, 720.0, 360.0);
            let n = osk.suggestions().len();
            assert!(n > 0);
            assert_eq!(osk.suggestion_geometry().len(), n, "one rect per chip");
            // Chips fill the full width and sit within the toolbar..keys band.
            let geom = osk.suggestion_geometry();
            let leftmost = geom.iter().map(|(_, r)| r.x0).fold(f64::MAX, f64::min);
            let rightmost = geom.iter().map(|(_, r)| r.x1).fold(f64::MIN, f64::max);
            assert!((leftmost - 0.0).abs() < 1.0, "chips start at left edge");
            assert!((rightmost - 720.0).abs() < 1.0, "chips fill width");
        }

        eprintln!("phase responsive_content_caps_on_desktop_but_keeps_tablet_width");
        {
            let mut desktop =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut desktop, 1_920.0, 480.0);
            assert_eq!(desktop.content_rect.width(), MAX_KEYBOARD_CONTENT_WIDTH);
            assert!(desktop.content_rect.x0 > 0.0, "desktop keyboard is centred");

            let mut tablet =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut tablet, 768.0, 420.0);
            assert_eq!(tablet.content_rect.width(), 768.0);
            assert_eq!(tablet.content_rect.x0, 0.0);
        }
    }

    // --- swipe + prediction + suggestion bar -----------------------

    /// Build a dense pointer path through the live key centres of `word`,
    /// using the widget's own laid-out geometry (the same source of truth the
    /// decoder consumes).
    fn path_through(osk: &OskWidget, word: &str) -> Vec<Point> {
        // letter -> key centre from the current frame's geometry.
        let layout = osk.current_layout();
        let mut centres: HashMap<char, Point> = HashMap::new();
        for (id, r) in osk.key_geometry() {
            let Some(k) = layout.key(*id) else { continue };
            let KeyAction::Char(s) = &k.action else {
                continue;
            };
            if let Some(c) = s.chars().next()
                && c.is_alphabetic()
                && s.chars().count() == 1
            {
                centres
                    .entry(c.to_ascii_lowercase())
                    .or_insert_with(|| r.center());
            }
        }
        let mut pts = Vec::new();
        for c in word.chars() {
            let centre = centres[&c];
            if let Some(prev) = pts.last().copied() {
                let prev: Point = prev;
                for k in 1..=5 {
                    let t = f64::from(k) / 5.0;
                    pts.push(Point::new(
                        prev.x + (centre.x - prev.x) * t,
                        prev.y + (centre.y - prev.y) * t,
                    ));
                }
            } else {
                pts.push(centre);
            }
        }
        pts
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn swipes_and_suggestion_chips_edit_the_current_word() {
        eprintln!("phase swipe_inserts_top_candidate_and_fills_suggestion_bar");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            let path = path_through(&osk, "quick");
            assert!(osk.swipe(&path), "swipe decoded a word");
            // Top candidate composed in the field.
            assert_eq!(osk.pending_text(), "quick");
            assert_eq!(osk.take_output(), vec![pre("quick")]);
            // The suggestion bar shows the candidates, with "quick" present.
            let words: Vec<&str> = osk.suggestions().iter().map(|s| s.word.as_str()).collect();
            assert!(!words.is_empty(), "suggestion bar populated");
            assert!(
                words.contains(&"quick"),
                "candidates include quick: {words:?}"
            );
        }

        eprintln!("phase tapping_suggestion_chip_replaces_current_word");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            // Type a prefix that yields completions, e.g. "th" -> the/this/that...
            osk.activate_label("t");
            osk.activate_label("h");
            assert_eq!(osk.current_word(), "th");
            let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
            assert!(!chips.is_empty(), "completions shown for 'th': {chips:?}");
            // Tap the top chip; it replaces the in-progress word with the full
            // word and a space, committed in one step.
            let top = chips[0].clone();
            osk.take_output();
            assert!(osk.activate_suggestion(0));
            assert_eq!(osk.take_output(), vec![commit(&format!("{top} "))]);
            assert_eq!(osk.pending_text(), format!("{top} "));
            assert_eq!(osk.current_word(), "", "word committed after chip tap");
        }

        eprintln!("phase next_word_chip_inserts_word_and_space");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            // Commit "of" so the bar fills with next-word predictions ("the" tops).
            osk.activate_label("o");
            osk.activate_label("f");
            osk.activate_label("space");
            let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
            assert!(
                chips.contains(&"the".to_string()),
                "next-word after 'of': {chips:?}"
            );
            // Find and tap the "the" chip; it inserts "the ".
            let idx = chips.iter().position(|w| w == "the").unwrap();
            assert!(osk.activate_suggestion(idx));
            assert_eq!(osk.pending_text(), "of the ");
        }

        eprintln!("phase short_tap_is_not_treated_as_a_swipe");
        {
            use winit::event::{DeviceId, Modifiers};
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);

            let target = KeyId::new(0, 0); // 'q'
            let rect = osk
                .key_geometry()
                .iter()
                .find(|(id, _)| *id == target)
                .map(|(_, r)| *r)
                .unwrap();
            let c = rect.center();

            let device: DeviceId = unsafe { std::mem::zeroed() };
            let mods = Modifiers::default();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
                let mut handled = false;
                let mut focus = None;
                osk.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: NodeId::new(0),
                        taffy: &TaffyTree::new(),
                    },
                );
            };

            // Press, a tiny jitter (1px), release — all on 'q'. This must type
            // exactly "q", NOT decode a swipe.
            dispatch(
                &mut osk,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (c.x, c.y).into(),
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (c.x + 1.0, c.y + 0.5).into(),
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert_eq!(
                osk.pending_text(),
                "q",
                "short tap typed 'q', not a swiped word"
            );
        }

        eprintln!("phase pointer_swipe_decodes_through_keys");
        {
            use winit::event::{DeviceId, Modifiers};
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut osk, 720.0, 360.0);
            let path = path_through(&osk, "the");
            let start = *path.first().unwrap();

            let device: DeviceId = unsafe { std::mem::zeroed() };
            let mods = Modifiers::default();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut dispatch = |osk: &mut OskWidget, ev: WindowEvent| {
                let mut handled = false;
                let mut focus = None;
                osk.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: NodeId::new(0),
                        taffy: &TaffyTree::new(),
                    },
                );
            };

            // Hover the first key, press, drag along the whole path, release.
            dispatch(
                &mut osk,
                WindowEvent::CursorMoved {
                    device_id: device,
                    position: (start.x, start.y).into(),
                },
            );
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            for p in &path {
                dispatch(
                    &mut osk,
                    WindowEvent::CursorMoved {
                        device_id: device,
                        position: (p.x, p.y).into(),
                    },
                );
            }
            dispatch(
                &mut osk,
                WindowEvent::MouseInput {
                    device_id: device,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
            );

            assert_eq!(
                osk.pending_text(),
                "the",
                "pointer swipe through t-h-e typed 'the'"
            );
        }
    }

    // --- auto-show / visibility ---------------------------------------

    #[test]
    fn chip_corrects_a_swiped_word_in_place() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path));
        let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
        assert!(chips.len() > 1, "swipe offers alternatives: {chips:?}");
        osk.take_output();
        assert!(osk.activate_suggestion(1));
        assert_eq!(osk.take_output(), vec![pre(&chips[1])]);
        assert_eq!(osk.pending_text(), chips[1]);
        // The alternatives stay up for another correction.
        assert_eq!(osk.suggestions().len(), chips.len());
    }

    #[test]
    fn next_swipe_commits_the_previous_word_with_a_space() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let first = path_through(&osk, "quick");
        let second = path_through(&osk, "the");
        assert!(osk.swipe(&first));
        let swiped = osk.current_word().to_string();
        assert!(osk.swipe(&second));
        let out = osk.take_output();
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[1], commit(&format!("{swiped} ")));
        assert!(matches!(&out[2], OskOutput::SetPreedit(w) if !w.is_empty()));
    }

    #[test]
    fn typing_after_a_swipe_starts_a_new_word() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path));
        let swiped = osk.current_word().to_string();
        osk.take_output();
        osk.apply_action(KeyAction::Char("a".into()));
        assert_eq!(
            osk.take_output(),
            vec![commit(&format!("{swiped} ")), pre("a")]
        );
    }

    #[test]
    fn backspace_removes_a_swiped_word_whole() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path));
        osk.take_output();
        osk.apply_action(KeyAction::Backspace);
        assert_eq!(osk.take_output(), vec![pre("")]);
        assert_eq!(osk.pending_text(), "");
    }

    #[test]
    fn swipe_replaces_a_partly_typed_word() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        osk.apply_action(KeyAction::Char("x".into()));
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path));
        let out = osk.take_output();
        assert_eq!(out[0], pre("x"));
        assert_eq!(out.len(), 2, "no commit of the replaced prefix: {out:?}");
        assert_eq!(osk.pending_text(), osk.current_word());
    }

    #[test]
    fn password_field_never_composes_suggests_or_learns() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        osk.set_purpose(InputPurpose::Password);
        let before = osk.predictor.learned_context_count();
        for c in ["h", "u", "n", "t", "e", "r"] {
            osk.apply_action(KeyAction::Char(c.into()));
        }
        osk.apply_action(KeyAction::Space);
        for c in ["t", "w", "o"] {
            osk.apply_action(KeyAction::Char(c.into()));
        }
        osk.apply_action(KeyAction::Enter);
        assert_eq!(osk.predictor.learned_context_count(), before);
        assert!(osk.suggestions().is_empty());
        assert_eq!(osk.current_word(), "");
        assert!(
            osk.take_output()
                .iter()
                .all(|o| !matches!(o, OskOutput::SetPreedit(_))),
            "a secret is never shown as preedit"
        );
        let path = path_through(&osk, "quick");
        assert!(!osk.swipe(&path), "no swiping into a secret field");
    }

    #[test]
    fn sensitive_hint_is_secret_in_any_purpose() {
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("a".into()));
        osk.set_content_hints(content_hint::SENSITIVE_DATA);
        assert!(osk.is_secret_field());
        assert_eq!(osk.current_word(), "");
        assert_eq!(osk.take_output().last(), Some(&pre("")));
        assert!(osk.suggestions().is_empty());
        let before = osk.predictor.learned_context_count();
        osk.apply_action(KeyAction::Char("b".into()));
        osk.apply_action(KeyAction::Space);
        assert_eq!(osk.predictor.learned_context_count(), before);
    }

    #[test]
    fn blur_clears_typed_text_traces() {
        let mut osk = OskWidget::new();
        osk.set_purpose(InputPurpose::Password);
        osk.apply_action(KeyAction::Char("s".into()));
        let mut doc = AutoCommit::new();
        osk.on_external_event("osk:field", &mut doc, &automerge::ROOT);
        assert_eq!(osk.pending_text(), "");
        assert!(osk.take_output().is_empty());
        assert!(osk.take_key_actions().is_empty());
    }

    #[test]
    fn auto_capitalization_shifts_at_sentence_starts() {
        let mut osk = OskWidget::new();
        let mut doc = AutoCommit::new();
        let hints = content_hint::AUTO_CAPITALIZATION;
        osk.on_external_event(&format!("osk:hints:{hints}"), &mut doc, &automerge::ROOT);
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        lay_out(&mut osk, 800.0, 360.0);
        for label in ["h", "i", ".", "space", "o", "k", "space"] {
            osk.activate_label(label);
        }
        assert_eq!(osk.pending_text(), "Hi. Ok ");
        assert_eq!(osk.shift_state(), ShiftState::Off);

        osk.set_content_hints(content_hint::AUTO_CAPITALIZATION | content_hint::LOWERCASE);
        osk.on_external_event("osk:field", &mut doc, &automerge::ROOT);
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }

    #[test]
    fn field_change_drops_the_composition() {
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("h".into()));
        let mut doc = AutoCommit::new();
        osk.on_external_event("osk:field", &mut doc, &automerge::ROOT);
        assert_eq!(osk.current_word(), "");
        osk.apply_action(KeyAction::Backspace);
        assert_eq!(
            osk.take_output().last(),
            Some(&OskOutput::Key(Symbol::BackSpace))
        );
    }

    #[test]
    fn visibility_defaults_toggle_and_collapse_height() {
        eprintln!("phase keyboard_is_visible_by_default");
        {
            assert!(
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                    .is_visible()
            );
        }

        eprintln!("phase hidden_builder_starts_invisible");
        {
            assert!(
                !OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                    .hidden()
                    .is_visible()
            );
        }

        eprintln!("phase hidden_keyboard_takes_zero_height");
        {
            // A visible keyboard occupies its intrinsic height; when the compositor
            // has hidden it the widget requests no height and paints nothing.
            let mut visible =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            lay_out(&mut visible, 720.0, 360.0);
            assert!(visible.get_rect().height() > 0.0);

            let mut hidden =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
            lay_out(&mut hidden, 720.0, 360.0);
            assert_eq!(hidden.get_rect().height(), 0.0);
        }

        eprintln!("phase set_visible_toggles_and_marks_structure_dirty");
        {
            let mut osk =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
            assert!(!osk.is_visible());
            osk.set_visible(true);
            assert!(osk.is_visible());
            assert!(osk.take_structure_dirty(), "showing relays out");
            // Idempotent: setting the same state again is a no-op (no relayout).
            osk.set_visible(true);
            assert!(!osk.take_structure_dirty(), "no-op when unchanged");
            osk.set_visible(false);
            assert!(!osk.is_visible());
            assert!(osk.take_structure_dirty(), "hiding relays out");
        }
    }

    #[test]
    fn external_commands_drive_visibility_and_purpose_without_blocking() {
        eprintln!("phase keyboard_app_id_is_the_well_known_id");
        {
            // The compositor recognises the keyboard by this exact app-id to keep
            // it out of the tile layout and own its visibility. If this drifts, the
            // keyboard would be treated as an ordinary window — so pin the value.
            assert_eq!(crate::KEYBOARD_APP_ID, "so.tilo.osk");
        }

        eprintln!("phase external_show_hide_commands_drive_visibility");
        {
            let mut osk =
                OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            osk.on_external_event("osk:show", &mut doc, &root);
            assert!(osk.is_visible());

            osk.on_external_event("osk:hide", &mut doc, &root);
            assert!(!osk.is_visible());
        }

        eprintln!("phase external_purpose_command_switches_layout");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            osk.on_external_event("osk:purpose:phone", &mut doc, &root);
            assert_eq!(osk.purpose(), InputPurpose::Telephone);
            assert_eq!(osk.active_layout_name(), "phone");

            osk.on_external_event("osk:purpose:email", &mut doc, &root);
            assert_eq!(osk.purpose(), InputPurpose::Email);
            assert_eq!(osk.active_layout_name(), "email");
        }

        eprintln!("phase external_unknown_command_is_ignored");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            let before = osk.purpose();
            let visible_before = osk.is_visible();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            osk.on_external_event("osk:purpose:dvorak", &mut doc, &root); // unknown name
            osk.on_external_event("something:else", &mut doc, &root); // unrelated payload
            assert_eq!(osk.purpose(), before);
            assert_eq!(osk.is_visible(), visible_before);
        }

        eprintln!("phase bounded_diagnostic_buffers_report_overflow_without_blocking_delivery");
        {
            let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
            osk.apply_action(KeyAction::Char("x".repeat(MAX_PENDING_TEXT_BYTES + 11)));
            assert_eq!(osk.pending_text().len(), MAX_PENDING_TEXT_BYTES);
            assert_eq!(osk.pending_text_dropped_bytes(), 11);
            assert_eq!(
                osk.take_output().len(),
                1,
                "an overlong word is committed, not composed"
            );
            for _ in 0..(MAX_OUTPUT_ENTRIES + 3) {
                osk.apply_action(KeyAction::Enter);
            }
            assert_eq!(osk.output.len(), MAX_OUTPUT_ENTRIES);
            assert_eq!(osk.dropped_outputs(), 3);

            for _ in 0..(MAX_KEY_LOG_ENTRIES + 5) {
                osk.apply_action(KeyAction::Shift);
            }
            assert_eq!(osk.key_log.len(), MAX_KEY_LOG_ENTRIES);
            assert!(osk.dropped_key_actions() >= 6);
        }
    }

    // The keyboard publishes device-local configuration (layout, suggestion
    // privacy) as client-local `osk.preference` document actions plus the
    // clear-personal effect; keystrokes never become actions. The registry is
    // the one the app installs, so every published document type decodes.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            crate::actions::register(&mut registry);
            registry
        },
        resources: |doc| ui::resources::resources(&OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())), doc),
        palette: ["English (QWERTY)", "French (AZERTY)", "Clear learned suggestions"],
    }

    fn semantic(osk: &mut OskWidget, target: &str, action: &AccessibilityAction) -> bool {
        osk.perform_semantic_action(target, action, &mut AutoCommit::new(), &automerge::ROOT)
    }

    fn activate(osk: &mut OskWidget, target: &str) -> bool {
        semantic(osk, target, &AccessibilityAction::Activate)
    }

    fn open_panel(osk: &mut OskWidget) {
        lay_out(osk, 720.0, 360.0);
        assert!(
            osk.activate_label("\u{1F642}"),
            "the emoji key is on the letters"
        );
        assert_eq!(osk.emoji_panel().mode(), PanelMode::Browse);
    }

    fn named_nodes(osk: &OskWidget) -> Vec<(String, String)> {
        osk.accessibility()
            .into_iter()
            .flat_map(|n| n.children)
            .flat_map(|n| std::iter::once(n.clone()).chain(n.children))
            .filter_map(|n| Some((n.name?, n.action_target?)))
            .collect()
    }

    #[test]
    fn emoji_key_opens_the_panel_in_place_of_the_keys() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        assert!(osk.key_geometry().is_empty(), "the grid replaces the keys");
        assert!(
            osk.suggestion_geometry().is_empty(),
            "the search field replaces the strip"
        );
        assert!(
            !osk.claims_raw_touch(osk.rect.center()),
            "the grid takes taps and drags"
        );
        let names = named_nodes(&osk);
        assert!(names.iter().any(|(n, _)| n == &i18n::t("osk-emoji-search")));
        assert!(names.iter().any(|(_, t)| t == "osk:emoji-group:0"));
        assert!(activate(&mut osk, "osk:emoji-letters"));
        assert_eq!(osk.emoji_panel().mode(), PanelMode::Closed);
        assert!(!osk.key_geometry().is_empty(), "the keys are back");
    }

    #[test]
    fn emoji_key_is_named_for_assistive_tech() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let names = named_nodes(&osk);
        assert!(
            names
                .iter()
                .any(|(n, t)| n == &i18n::t("osk-emoji-key") && t.starts_with("osk:key:"))
        );
    }

    #[test]
    fn picking_an_emoji_commits_it_after_the_composing_word() {
        let mut osk = OskWidget::new();
        type_word(&mut osk, "hi");
        let _ = osk.take_output();
        open_panel(&mut osk);
        let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
        assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
        assert_eq!(osk.take_output(), vec![commit("hi"), commit(&first)]);
        assert_eq!(osk.emoji_recents(), [first]);
        assert_eq!(
            osk.emoji_panel().mode(),
            PanelMode::Browse,
            "the panel stays for more"
        );
    }

    #[test]
    fn recents_lead_the_grid() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        assert!(activate(&mut osk, "osk:emoji-group:3"));
        let second = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[1].clone();
        assert!(activate(&mut osk, &format!("osk:emoji:{second}")));
        let names = named_nodes(&osk);
        let first_emoji = names
            .iter()
            .find(|(_, t)| t.starts_with("osk:emoji:"))
            .map(|(_, t)| t.clone());
        assert_eq!(first_emoji, Some(format!("osk:emoji:{second}")));
    }

    #[test]
    fn search_is_typed_with_the_keys_and_enter_picks_the_best_match() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        assert!(activate(&mut osk, "osk:emoji-search"));
        assert_eq!(osk.emoji_panel().mode(), PanelMode::Search);
        assert!(!osk.key_geometry().is_empty(), "the keys type the query");
        for c in ["p", "i", "z", "z", "a"] {
            assert!(osk.activate_label(c));
        }
        assert_eq!(osk.emoji_panel().query(), "pizza");
        assert!(
            osk.take_output().is_empty(),
            "the query never reaches the field"
        );
        assert!(named_nodes(&osk).iter().any(|(_, t)| t == "osk:emoji:🍕"));
        assert!(osk.activate_label("⌫"));
        assert_eq!(osk.emoji_panel().query(), "pizz");
        assert!(osk.activate_label("a"));
        assert!(osk.activate_label("⏎"));
        assert_eq!(osk.take_output(), vec![commit("🍕")]);
        assert!(activate(&mut osk, "osk:emoji-end-search"));
        assert_eq!(osk.emoji_panel().mode(), PanelMode::Browse);
        assert_eq!(osk.emoji_panel().query(), "");
    }

    #[test]
    fn long_press_offers_skin_tones_and_commits_the_toned_form() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        // Thumbs up is in People & Body; search puts it on screen.
        assert!(semantic(
            &mut osk,
            "osk:emoji-search",
            &AccessibilityAction::SetValue("thumbs up".into()),
        ));
        assert!(!semantic(
            &mut osk,
            "osk:emoji:🍕",
            &AccessibilityAction::ShowContextMenu
        ));
        assert!(semantic(
            &mut osk,
            "osk:emoji:👍",
            &AccessibilityAction::ShowContextMenu
        ));
        assert_eq!(osk.emoji_panel().tones_for(), Some("👍"));
        let toned = emoji::Catalogue::english()
            .with_skin_tone("👍", Some(emoji::SkinTone::MediumDark))
            .unwrap()
            .to_owned();
        assert!(activate(&mut osk, &format!("osk:emoji-tone:{toned}")));
        assert_eq!(osk.take_output(), vec![commit(&toned)]);
        assert_eq!(osk.emoji_panel().tones_for(), None);
        assert_eq!(osk.emoji_recents(), [toned]);
    }

    #[test]
    fn emoji_without_tones_open_no_tone_row() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
        assert!(!semantic(
            &mut osk,
            &format!("osk:emoji:{first}"),
            &AccessibilityAction::ShowContextMenu,
        ));
    }

    #[test]
    fn a_word_that_names_an_emoji_offers_it_and_the_chip_commits_it() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        type_word(&mut osk, "pizza");
        let chip = osk
            .suggestions()
            .iter()
            .position(|s| s.emoji && s.word == "🍕")
            .expect("an emoji chip for pizza");
        assert!(osk.suggestions().len() <= MAX_SUGGESTIONS);
        let _ = osk.take_output();
        assert!(osk.activate_suggestion(chip));
        assert_eq!(osk.take_output(), vec![commit("pizza "), commit("🍕 ")]);
        assert_eq!(osk.pending_text(), "pizza 🍕 ");
        assert_eq!(osk.emoji_recents(), ["🍕".to_owned()]);
    }

    #[test]
    fn emoji_chips_are_named_for_the_emoji() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        type_word(&mut osk, "pizza");
        lay_out(&mut osk, 720.0, 360.0);
        let expected = i18n::t_args("osk-emoji-insert", &[("name", "pizza".into())]);
        assert!(named_nodes(&osk).iter().any(|(n, _)| n == &expected));
    }

    #[test]
    fn password_fields_get_no_emoji_suggestions_or_recents() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        osk.set_purpose(InputPurpose::Password);
        type_word(&mut osk, "pizza");
        assert!(osk.suggestions().iter().all(|s| !s.emoji));
        osk.set_purpose(InputPurpose::Text);
        osk.set_content_hints(content_hint::SENSITIVE_DATA);
        type_word(&mut osk, "pizza");
        assert!(osk.suggestions().is_empty());
        open_panel(&mut osk);
        let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
        assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
        assert!(
            osk.emoji_recents().is_empty(),
            "a secret field leaves no recents"
        );
    }

    #[test]
    fn recents_follow_the_personalization_preference() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
        assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
        assert_eq!(osk.emoji_recents().len(), 1);
        osk.clear_personalization();
        assert!(osk.emoji_recents().is_empty());
        osk.set_personalization_enabled(false);
        assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
        assert!(osk.emoji_recents().is_empty());
    }

    #[test]
    fn recents_round_trip_through_the_keyboard_document() {
        let mut doc = AutoCommit::new();
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        let first = osk.emoji_panel().sections(&emoji::Recents::default())[0].emoji[0].clone();
        assert!(activate(&mut osk, &format!("osk:emoji:{first}")));
        // Recents are saved through the keyboard's preference action.
        osk.sync(&mut doc, &automerge::ROOT);
        let offer = ui::widget::controls::take_control_invocation(&mut osk)
            .expect("the recents are offered as a preference action");
        let encoded = ui::tilo_actions::door::encode_json(
            &ui::resources::resources(&osk, &doc),
            &offer.address,
            &offer.arguments,
        )
        .unwrap();
        let mut registry = ui::tilo_actions::ActionRegistry::new();
        crate::actions::register(&mut registry);
        registry.decode(&encoded).unwrap().apply(&mut doc).unwrap();
        ui::history_mode::capability::document_action_applied(
            &mut osk,
            &encoded,
            &[],
            &doc,
            &automerge::ROOT,
        );
        assert!(!osk.drafts.pending(), "the saved recents settle the draft");
        let mut restarted = OskWidget::new();
        restarted.sync(&mut doc, &automerge::ROOT);
        assert_eq!(restarted.emoji_recents(), [first]);
    }

    #[test]
    fn every_panel_face_paints() {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        let mut paint = |osk: &mut OskWidget| {
            let mut scene = Scene::new();
            osk.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            assert!(!scene.encoding().is_empty());
        };
        paint(&mut osk);
        assert!(semantic(
            &mut osk,
            "osk:emoji-search",
            &AccessibilityAction::SetValue("thumbs".into()),
        ));
        paint(&mut osk);
        assert!(semantic(
            &mut osk,
            "osk:emoji:👍",
            &AccessibilityAction::ShowContextMenu,
        ));
        paint(&mut osk);
    }

    #[test]
    fn a_field_change_closes_the_panel() {
        let mut osk = OskWidget::new();
        open_panel(&mut osk);
        osk.on_external_event("osk:field", &mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(osk.emoji_panel().mode(), PanelMode::Closed);
    }

    // --- press and hold, previews, caret drag ---------------------------

    fn finger(osk: &mut OskWidget, id: u64, phase: TouchPhase, at: Point) {
        let touch = Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(at.x, at.y),
            force: None,
            id,
        };
        let mut handled = false;
        osk.handle_touch(&touch, 1.0, &mut handled);
    }

    fn key_rect(osk: &OskWidget, label: &str) -> (KeyId, Rect) {
        let layout = osk.current_layout();
        osk.key_geom
            .iter()
            .find(|(id, _)| layout.key(*id).is_some_and(|k| k.label == label))
            .copied()
            .unwrap()
    }

    #[test]
    fn long_press_offers_accents_and_slide_picks_one() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let (_, e) = key_rect(&osk, "e");
        finger(&mut osk, 1, TouchPhase::Started, e.center());
        let deadline = osk
            .next_wake_deadline()
            .expect("a letter with accents arms a long press");
        osk.poll_hold(deadline.checked_sub(Duration::from_millis(1)).unwrap());
        assert!(osk.alternates.is_none(), "nothing before the delay");
        osk.poll_hold(deadline);
        let cells = osk.alternates.as_ref().unwrap().1.cells.clone();
        assert_eq!(osk.alternates.as_ref().unwrap().1.chosen(), Some("e"));
        // The row sits above the key, inside the keyboard.
        assert!(
            cells
                .iter()
                .all(|c| c.y1 <= e.y0 && osk.content_rect.contains(c.origin()))
        );
        finger(&mut osk, 1, TouchPhase::Moved, cells[1].center());
        finger(&mut osk, 1, TouchPhase::Ended, cells[1].center());
        assert_eq!(osk.pending_text(), "é");
        assert!(osk.alternates.is_none());
    }

    fn long_press_options(osk: &mut OskWidget, label: &str) -> Vec<String> {
        let (id, _) = key_rect(osk, label);
        let now = Instant::now();
        osk.begin_hold(id, None, now);
        osk.poll_hold(now + LONG_PRESS);
        osk.alternates.take().unwrap().1.options
    }

    #[test]
    fn french_accents_come_in_the_order_french_uses_them() {
        let mut osk = OskWidget::new().with_active_layout("azerty");
        lay_out(&mut osk, 390.0, 312.0);
        assert_eq!(osk.language(), Language::French);
        for (key, leading) in [
            ("e", &["e", "é", "è", "ê", "ë"][..]),
            ("a", &["a", "à", "â"][..]),
            ("u", &["u", "ù", "û"][..]),
            ("o", &["o", "ô", "œ"][..]),
            ("c", &["c", "ç"][..]),
        ] {
            let options = long_press_options(&mut osk, key);
            assert_eq!(&options[..leading.len()], leading, "{options:?}");
        }
        // Every French accent is on offer.
        let all: Vec<String> = ["e", "a", "c", "u", "o", "i", "y"]
            .into_iter()
            .flat_map(|k| long_press_options(&mut osk, k))
            .collect();
        for form in [
            "é", "è", "ê", "ë", "à", "â", "ç", "ù", "û", "ü", "ô", "œ", "î", "ï", "ÿ",
        ] {
            assert!(all.iter().any(|f| f == form), "{form} missing");
        }
    }

    #[test]
    fn the_plain_letter_sits_above_the_finger_even_at_the_edges() {
        let mut osk = OskWidget::new().with_active_layout("azerty");
        lay_out(&mut osk, 390.0, 312.0);
        for label in ["a", "e", "o", "p", "m", "q"] {
            let (id, rect) = key_rect(&osk, label);
            let now = Instant::now();
            osk.begin_hold(id, None, now);
            osk.poll_hold(now + LONG_PRESS);
            let Some((_, mut popup)) = osk.alternates.take() else {
                continue;
            };
            let first = popup.cells[0];
            assert!(
                (first.center().x - rect.center().x).abs() < 1.0,
                "{label}: the letter is not above the key"
            );
            assert!(
                popup
                    .cells
                    .iter()
                    .all(|c| c.x0 >= osk.content_rect.x0 - 0.5 && c.x1 <= osk.content_rect.x1 + 0.5),
                "{label}: row spills out"
            );
            // A small wobble keeps the letter.
            popup.track(rect.center().x + rect.width() * 0.3);
            assert_eq!(popup.chosen(), Some(label));
        }
    }

    #[test]
    fn shifted_long_press_offers_capitals() {
        let mut osk = OskWidget::new().with_active_layout("azerty");
        lay_out(&mut osk, 390.0, 312.0);
        osk.apply_action(KeyAction::Shift);
        let e = long_press_options(&mut osk, "e");
        assert_eq!(&e[..3], ["E", "É", "È"], "{e:?}");
        // A form whose capital is two letters (ß → SS) stays as it is.
        let mut s = OskWidget::new();
        lay_out(&mut s, 390.0, 312.0);
        s.apply_action(KeyAction::Shift);
        assert!(long_press_options(&mut s, "s").contains(&"ß".to_string()));
    }

    #[test]
    fn a_long_press_released_without_sliding_types_the_letter_once() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let (_, e) = key_rect(&osk, "e");
        finger(&mut osk, 1, TouchPhase::Started, e.center());
        osk.poll_hold(Instant::now() + LONG_PRESS);
        finger(&mut osk, 1, TouchPhase::Ended, e.center());
        assert_eq!(osk.pending_text(), "e");
    }

    #[test]
    fn a_quick_tap_opens_nothing_and_types_the_letter() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let (_, e) = key_rect(&osk, "e");
        finger(&mut osk, 1, TouchPhase::Started, e.center());
        finger(&mut osk, 1, TouchPhase::Ended, e.center());
        assert!(osk.hold.is_none() && osk.next_wake_deadline().is_none());
        assert_eq!(osk.pending_text(), "e");
    }

    #[test]
    fn held_backspace_repeats_and_its_release_adds_nothing() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        osk.apply_action(KeyAction::Char(".".into()));
        type_word(&mut osk, "hello");
        let (_, bs) = key_rect(&osk, "⌫");
        finger(&mut osk, 1, TouchPhase::Started, bs.center());
        let mut now = Instant::now() + REPEAT_DELAY;
        for _ in 0..3 {
            osk.poll_hold(now);
            now += REPEAT_INTERVAL;
        }
        finger(&mut osk, 1, TouchPhase::Ended, bs.center());
        assert_eq!(osk.pending_text(), ".he");
    }

    #[test]
    fn dragging_the_space_bar_moves_the_caret_instead_of_typing() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        type_word(&mut osk, "hello");
        osk.take_output();
        let (_, space) = key_rect(&osk, "space");
        let c = space.center();
        let step = osk.key_scale() * CURSOR_STEP;
        finger(&mut osk, 1, TouchPhase::Started, c);
        // A wobble under the threshold is still a tap-in-waiting.
        finger(&mut osk, 1, TouchPhase::Moved, Point::new(c.x - 3.0, c.y));
        assert!(!osk.cursor_drag_active());
        let start = c.x - osk.key_scale() * CURSOR_START;
        finger(&mut osk, 1, TouchPhase::Moved, Point::new(start, c.y));
        finger(
            &mut osk,
            1,
            TouchPhase::Moved,
            Point::new(start - 3.5 * step, c.y),
        );
        finger(
            &mut osk,
            1,
            TouchPhase::Ended,
            Point::new(start - 3.5 * step, c.y),
        );
        assert_eq!(
            osk.take_output(),
            vec![
                commit("hello"),
                OskOutput::Key(Symbol::Left),
                OskOutput::Key(Symbol::Left),
                OskOutput::Key(Symbol::Left),
            ],
            "the word commits as typed, the caret steps left, no space"
        );
    }

    #[test]
    fn a_touch_down_previews_its_character_but_never_in_a_password() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let (g, rect) = key_rect(&osk, "g");
        finger(&mut osk, 1, TouchPhase::Started, rect.center());
        assert_eq!(osk.preview_key(&[g]), Some(g));
        let bubble = crate::presentation::preview_rect(rect, osk.content_rect);
        assert!(bubble.y1 <= rect.y0 && bubble.width() > rect.width() * 0.8);
        let (space, _) = key_rect(&osk, "space");
        assert_eq!(osk.preview_key(&[space]), None, "only characters preview");
        osk.set_purpose(InputPurpose::Password);
        assert_eq!(osk.preview_key(&[g]), None);
    }

    #[test]
    fn previews_stay_inside_the_keyboard_at_its_edges() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 390.0, 312.0);
        for label in ["q", "p", "a", "l", "z", "m"] {
            let (_, rect) = key_rect(&osk, label);
            let bubble = crate::presentation::preview_rect(rect, osk.content_rect);
            assert!(
                bubble.x0 >= osk.content_rect.x0 && bubble.x1 <= osk.content_rect.x1,
                "{label} preview spills out: {bubble:?}"
            );
            assert!(bubble.y0 >= osk.content_rect.y0);
        }
    }

    #[test]
    fn hit_areas_move_with_context_but_the_drawn_keys_never_do() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 390.0, 312.0);
        let before = osk.key_geometry().to_vec();
        let start = osk.hit_cells(2.0);
        type_word(&mut osk, "th");
        lay_out(&mut osk, 390.0, 312.0);
        assert_eq!(
            osk.key_geometry(),
            before.as_slice(),
            "context never moves a key"
        );
        let th = osk.hit_cells(2.0);
        let (e, _) = key_rect(&osk, "e");
        let (r, rr) = key_rect(&osk, "r");
        let e_after_th = th.iter().filter(|c| c.key == e).count();
        let e_at_start = start.iter().filter(|c| c.key == e).count();
        assert!(e_after_th > e_at_start, "'e' grows after 'th'");
        assert!(
            th.iter().any(|c| c.drawn == r && c.key == e),
            "the r/e border goes to 'e' after 'th'"
        );
        let core = crate::touch_model::core_zone(rr, crate::touch_model::CORE_FRACTION);
        assert!(
            th.iter()
                .filter(|c| core.contains(c.rect.center()))
                .all(|c| c.key == r),
            "r's core zone stays r's"
        );
    }
}

#[cfg(test)]
#[path = "framework_history_tests.rs"]
mod framework_history_tests;

#[cfg(test)]
#[path = "visual_review.rs"]
mod visual_review;
