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
//! ## Seams
//! * **Neural swipe:** the decoder is behind the
//!   [`crate::swipe::SwipeModel`] trait. The shipping model is the geometric
//!   [`crate::swipe::GeometricSwipeModel`]; a future FUTO/ExecuTorch
//!   `FutoSwipeModel` implementing the same trait can be swapped in via
//!   [`OskWidget::with_swipe_model`] with no other change.

use std::collections::{HashMap, VecDeque};
use ui::doc::{read_bool, read_str};

#[cfg(test)]
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::actions::{Preference, PreferenceDrafts};
use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{OperationKind, Resource};
use ui::widget::Widget;

use crate::futo::FutoSwipeModel;
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
use crate::presentation::{KeyboardPresentation, keyboard_geometry};
use crate::toolbar::{OskToolbarAction, OskToolbarItem};
use crate::touch_model::{NextKeyCache, TouchKey, TouchModel};

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
    /// Offline next-word predictor.
    predictor: Predictor,
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
    /// Where this user tends to hit each letter; adapts tap resolution.
    touch_model: TouchModel,
    /// Next-letter probabilities for the current typing context.
    next_key: NextKeyCache,
    /// Last pointer position over the widget (logical px).
    pointer: Option<Point>,
    /// While the left button is down, the captured pointer path (logical px).
    /// `Some` between press and release; `None` otherwise.
    gesture_path: Option<Vec<Point>>,

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
            suggestions: Vec::new(),
            current_word: String::new(),
            word_from_swipe: false,
            current_word_overflowed: false,
            prev_word: None,
            touch_model: TouchModel::new(),
            next_key: NextKeyCache::default(),
            pointer: None,
            gesture_path: None,
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
    /// neural [`Predictor`]).
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
        }
        self.refresh_next_word_suggestions();
        self.drafts.stage(Preference::Personalization(enabled));
    }

    /// Forget locally adapted suggestion ranking without changing the privacy
    /// preference. This has no effect on the bundled dictionary/bigram model.
    pub fn clear_personalization(&mut self) {
        self.predictor.clear_learned();
        self.touch_model.clear();
    }

    // --- runtime configuration -----------------------------------------

    /// Switch the keyboard to the layout for `purpose` at runtime (the focused
    /// field changed). Registers the layout if absent, makes it active, drops
    /// the symbols layer, and raises `structure_dirty` so the host relays out
    /// (the row count differs between, say, the dialpad and QWERTY).
    pub fn set_purpose(&mut self, purpose: InputPurpose) {
        self.apply_purpose(purpose);
        self.showing_symbols = false;
        self.structure_dirty = true;
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

    // --- programmatic activation (test/host helpers) -------------------

    /// Activate a key by id as if tapped. No-op for an out-of-range id.
    pub fn activate_key(&mut self, id: KeyId) {
        let action = match self.current_layout().key(id) {
            Some(k) => k.emit_action(self.shift),
            None => return,
        };
        self.apply_action(action);
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
        true
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
            KeyAction::Backspace => self.backspace(),
            KeyAction::Shift => {
                self.shift = self.shift.cycle();
            }
            KeyAction::SymbolsToggle => {
                self.showing_symbols = !self.showing_symbols;
                self.structure_dirty = true;
            }
            KeyAction::Layout(name) => {
                self.select_text_layout(name);
            }
        }
        if self.key_log.len() == MAX_KEY_LOG_ENTRIES {
            self.key_log.pop_front();
            self.dropped_key_actions = self.dropped_key_actions.saturating_add(1);
        }
        self.key_log.push_back(action);
    }

    /// Send one output to the field (callback, bounded queue, mirror).
    fn emit(&mut self, output: OskOutput) {
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

    /// Add typed letters to the composing word. A swiped word in front of them
    /// is committed first with a separating space.
    fn extend_word(&mut self, letters: &str) {
        if self.word_from_swipe {
            self.finish_word(" ");
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
        let word = std::mem::take(&mut self.current_word);
        if !word.is_empty() || !tail.is_empty() {
            let mut text = word.clone();
            text.push_str(tail);
            self.emit(OskOutput::Commit(text));
        }
        self.word_from_swipe = false;
        self.current_word_overflowed = false;
        if !word.is_empty() {
            let committed = word.to_lowercase();
            if self.personalization_enabled {
                self.predictor
                    .learn_transition(self.prev_word.as_deref(), &committed);
            }
            self.prev_word = Some(committed);
        }
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

    /// Drop the composition and next-word context. The focused field changed
    /// (or went away), so neither belongs to what the user edits next.
    pub fn reset_composition(&mut self) {
        self.current_word.clear();
        self.word_from_swipe = false;
        self.current_word_overflowed = false;
        self.prev_word = None;
        self.mirror.clear();
        self.refresh_next_word_suggestions();
    }

    /// Refill the bar with completions of the in-progress word (biased by the
    /// previous word). Called as the user types letters.
    fn refresh_completion_suggestions(&mut self) {
        if !self.suggestions_enabled {
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
    }

    /// Refill the bar with next-word predictions for the previous word.
    fn refresh_next_word_suggestions(&mut self) {
        if !self.suggestions_enabled {
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
        let Some(s) = self.suggestions.get(index).cloned() else {
            return false;
        };
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
        let layout = self.current_layout().clone();
        let cands = self
            .swipe_model
            .decode(path, &self.key_geom, &layout, MAX_SUGGESTIONS);
        let Some(top_cand) = cands.first() else {
            return false;
        };
        let top = top_cand.word.clone();
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
        true
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

impl Widget for OskWidget {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(crate::framework_history::definition(
            self.layouts.get("qwerty") == Some(&KeyboardLayout::qwerty()),
            self.layouts.get("azerty") == Some(&KeyboardLayout::azerty()),
        ))
    }

    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        if scope == crate::framework_history::SCOPE && self.drafts.pending() {
            Err("Return to live to finish pending keyboard preferences before restoring".into())
        } else {
            Ok(())
        }
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["osk.suggestions"]
    }
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let mut layouts = self
            .layouts
            .keys()
            .filter(|name| name.as_str() != "symbols")
            .cloned()
            .collect::<Vec<_>>();
        layouts.sort();
        crate::actions::resources(layouts, &self.preferred_text_layout)
    }
    fn invoke_resource_operation(
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
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.drafts.take()
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
            }
        }
    }
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
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
        let resources = self.resources(state);
        self.drafts.prepare_after_sync(&resources);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        // background + hit-test correctly (suggestion_rects is empty then).
        self.suggestion_area = geometry.suggestion_area;
        self.key_geom = geometry.keys;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
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
            hovered_tool: self.hovered_tool,
            pressed_tool: self.pressed_tool,
            hovered_sugg: self.hovered_sugg,
            pressed_sugg: self.pressed_sugg,
            empty_hint,
        }
        .draw(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // A hidden keyboard occupies no area and draws nothing, so it should
        // not consume or react to pointer events meant for the app behind it.
        if !self.visible {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    *handled = true;
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
                    *handled = true;
                } else if let Some(i) = self.hovered_sugg {
                    self.pressed_sugg = Some(i);
                    *handled = true;
                } else if let Some(i) = self.hovered_tool {
                    self.pressed_tool = Some(i);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let path = self.gesture_path.take();
                if let Some(id) = self.pressed_key.take() {
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
            _ => {}
        }
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
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Insert {}", suggestion.word))
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
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label.clone())
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(format!("key:{}:{}:{}", layout.name, id.row, id.col))
                    .with_action_target(format!(
                        "osk:key:{}:{}:{}:{}",
                        layout.name, id.row, id.col, label
                    )),
            );
        }
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
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if !self.visible || !matches!(action, AccessibilityAction::Activate) {
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
    ///   that belonged to the previous one.
    /// * `osk:purpose:<name>` — switch to the layout for an [`InputPurpose`]
    ///   (`text`, `number`, `decimal`, `phone`, `url`, `email`, `pin`,
    ///   `password`, `date`, `time`). An unknown name is ignored.
    ///
    /// Unrecognised payloads are ignored so an unrelated `External` event can't
    /// disturb the keyboard.
    fn on_external_event(&mut self, payload: &str, _state: &mut AutoCommit, _root: &ObjId) {
        match payload {
            "osk:show" => self.set_visible(true),
            "osk:hide" => self.set_visible(false),
            "osk:field" => self.reset_composition(),
            other => {
                if let Some(name) = other.strip_prefix("osk:purpose:")
                    && let Some(purpose) = input_purpose_from_name(name)
                {
                    self.set_purpose(purpose);
                }
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
    fn tapping_letter_appends_to_pending_text() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("h"));
        assert!(osk.activate_label("i"));
        assert_eq!(osk.pending_text(), "hi");
    }

    #[test]
    fn accessibility_exposes_and_operates_each_painted_key() {
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
        assert!(osk.control_action_bindings().iter().any(|binding| {
            binding.control_id == target
                && matches!(
                    binding.kind,
                    ui::widget::ControlBindingKind::SemanticAction(_)
                )
        }));
        assert!(osk.perform_semantic_action(
            target,
            &AccessibilityAction::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert_eq!(osk.pending_text(), "q");
    }

    #[test]
    fn tapping_letter_emits_char_action_and_fires_callback() {
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
    fn backspace_removes_last_char() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("⌫");
        assert_eq!(osk.pending_text(), "h");
    }

    #[test]
    fn take_output_drains() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("space");
        assert_eq!(osk.take_output(), vec![pre("h"), commit("h ")]);
        assert!(osk.take_output().is_empty(), "drained");
        assert_eq!(osk.pending_text(), "h ");
    }

    #[test]
    fn pointer_tap_routes_to_key_under_cursor() {
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
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                NodeId::new(0),
                &TaffyTree::new(),
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

    #[test]
    fn toolbar_tap_emits_action() {
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

    #[test]
    fn layout_action_switches_active_letter_layout() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.apply_action(KeyAction::Layout("azerty".into()));
        assert_eq!(osk.active_layout_name(), "azerty");
        lay_out(&mut osk, 800.0, 360.0);
        // AZERTY row 0 starts with 'a'.
        assert!(osk.activate_label("a"));
        assert_eq!(osk.pending_text(), "a");
    }

    // --- input-type layouts via purpose --------------------------------

    #[test]
    fn set_purpose_swaps_active_layout_and_marks_dirty() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        osk.set_purpose(InputPurpose::Telephone);
        assert_eq!(osk.purpose(), InputPurpose::Telephone);
        assert_eq!(osk.active_layout_name(), "phone");
        assert!(osk.take_structure_dirty(), "purpose change relays out");
    }

    #[test]
    fn with_purpose_starts_on_the_matching_layout() {
        let osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
            .with_purpose(InputPurpose::Pin);
        assert_eq!(osk.purpose(), InputPurpose::Pin);
        assert_eq!(osk.active_layout_name(), "pin");
    }

    #[test]
    fn numeric_purpose_types_digits_and_has_no_letters() {
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

    #[test]
    fn switching_purpose_then_back_registers_layouts_on_demand() {
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

    #[test]
    fn purpose_change_clears_stale_shift() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("⇧");
        assert_eq!(osk.shift_state(), ShiftState::Shift);
        // Moving to a number pad must drop the now-meaningless shift state.
        osk.set_purpose(InputPurpose::Number);
        assert_eq!(osk.shift_state(), ShiftState::Off);
    }

    #[test]
    fn numeric_layout_renders_offscreen() {
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

    #[test]
    fn swipe_inserts_top_candidate_and_fills_suggestion_bar() {
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

    #[test]
    fn tapping_suggestion_chip_replaces_current_word() {
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
    fn next_word_chip_inserts_word_and_space() {
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

    #[test]
    fn short_tap_is_not_treated_as_a_swipe() {
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
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                NodeId::new(0),
                &TaffyTree::new(),
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

    #[test]
    fn pointer_swipe_decodes_through_keys() {
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
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                NodeId::new(0),
                &TaffyTree::new(),
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

    #[test]
    fn suggestion_bar_geometry_matches_suggestion_count() {
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

    #[test]
    fn offscreen_render_includes_suggestion_bar() {
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

    // --- auto-show / visibility ---------------------------------------

    #[test]
    fn keyboard_is_visible_by_default() {
        assert!(
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).is_visible()
        );
    }

    #[test]
    fn hidden_builder_starts_invisible() {
        assert!(
            !OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
                .hidden()
                .is_visible()
        );
    }

    #[test]
    fn hidden_keyboard_takes_zero_height() {
        // A visible keyboard occupies its intrinsic height; when the compositor
        // has hidden it the widget requests no height and paints nothing.
        let mut visible = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut visible, 720.0, 360.0);
        assert!(visible.get_rect().height() > 0.0);

        let mut hidden =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        lay_out(&mut hidden, 720.0, 360.0);
        assert_eq!(hidden.get_rect().height(), 0.0);
    }

    #[test]
    fn set_visible_toggles_and_marks_structure_dirty() {
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

    #[test]
    fn keyboard_app_id_is_the_well_known_id() {
        // The compositor recognises the keyboard by this exact app-id to keep
        // it out of the tile layout and own its visibility. If this drifts, the
        // keyboard would be treated as an ordinary window — so pin the value.
        assert_eq!(crate::KEYBOARD_APP_ID, "so.tilo.osk");
    }

    #[test]
    fn external_show_hide_commands_drive_visibility() {
        let mut osk =
            OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english())).hidden();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        osk.on_external_event("osk:show", &mut doc, &root);
        assert!(osk.is_visible());

        osk.on_external_event("osk:hide", &mut doc, &root);
        assert!(!osk.is_visible());
    }

    #[test]
    fn external_purpose_command_switches_layout() {
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

    #[test]
    fn external_unknown_command_is_ignored() {
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

    #[test]
    fn input_purpose_name_round_trips() {
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

    #[test]
    fn hidden_keyboard_ignores_pointer_events() {
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
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            NodeId::new(0),
            &TaffyTree::new(),
        );
        assert!(!handled, "a hidden keyboard does not consume pointer input");
        assert_eq!(osk.pending_text(), "");
    }

    #[test]
    fn resource_catalogue_exports_configuration_not_keystrokes() {
        let osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        let resources = osk.resources(&AutoCommit::new());
        let keyboard = resources.first().expect("keyboard resource");
        assert!(
            keyboard.actions.is_empty(),
            "typing is not a replayable action"
        );
        assert_eq!(keyboard.children.len(), 2);
        let layout = keyboard
            .children
            .iter()
            .find(|resource| resource.id.0 == "osk.keyboard.layout")
            .expect("layout configuration resource");
        assert!(layout.actions.iter().any(|action| action.id == "qwerty"));
        assert!(layout.actions.iter().any(|action| action.id == "azerty"));
        assert_eq!(osk.local_resource_operation_handlers(), ["osk.suggestions"]);
    }

    #[test]
    fn configuration_resource_persists_and_restores_preferred_layout() {
        let mut osk = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        let mut doc = AutoCommit::new();
        let resources = osk.resources(&doc);
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
        osk.document_action_applied(&encoded, &[], &doc, &automerge::ROOT);
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

    #[test]
    fn responsive_content_caps_on_desktop_but_keeps_tablet_width() {
        let mut desktop = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut desktop, 1_920.0, 480.0);
        assert_eq!(desktop.content_rect.width(), MAX_KEYBOARD_CONTENT_WIDTH);
        assert!(desktop.content_rect.x0 > 0.0, "desktop keyboard is centred");

        let mut tablet = OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()));
        lay_out(&mut tablet, 768.0, 420.0);
        assert_eq!(tablet.content_rect.width(), 768.0);
        assert_eq!(tablet.content_rect.x0, 0.0);
    }

    #[test]
    fn disabled_suggestions_are_announced_and_not_materialised() {
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

    #[test]
    fn bounded_diagnostic_buffers_report_overflow_without_blocking_delivery() {
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
        resources: |doc| Widget::resources(&OskWidget::new(), doc),
        palette: ["Use QWERTY layout", "Use AZERTY layout", "Clear learned suggestions"],
    }
}

#[cfg(test)]
#[path = "framework_history_tests.rs"]
mod framework_history_tests;
