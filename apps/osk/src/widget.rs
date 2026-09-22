//! [`OskWidget`] — the on-screen keyboard widget.
//!
//! Self-contained: it draws its own toolbar + keys (geometry from the pure
//! [`crate::layout::key_rects`] / [`crate::toolbar::toolbar_rects`] passes)
//! and hit-tests pointer taps itself, rather than composing dozens of
//! `Button`/`Row` widgets — the key set changes with the active layer and
//! shift state, so a custom draw + hit-test keeps the geometry honest and
//! the taffy tree a single node.
//!
//! ## Output channels (two ways, mirroring `FilePicker`)
//! Compositor text injection (`input-method-v2`) is **not** wired here.
//! Until it is, what the user "types" is observable two ways:
//! * [`OskWidget::on_key`] — an imperative callback fired per activation.
//! * [`OskWidget::pending_text`] / [`OskWidget::take_pending_text`] — a
//!   buffer of the resulting text (peek + drain, like
//!   `FilePicker::outcome` / `FilePicker::take_outcome`).
//!
//! ## Swipe-to-type + prediction (wired here)
//! The widget captures each pointer drag as a path. On release a short,
//! near-stationary path stays a normal **key tap** (existing behaviour); a
//! path that travelled across multiple keys is treated as a **swipe** and run
//! through the [`crate::swipe`] decoder. The top candidate is auto-inserted
//! (replacing the in-progress word); the ranked alternatives appear as
//! tappable chips in a **suggestion bar** above the keys. After any word
//! commits (space, swipe, or chip), [`crate::predict`] fills the bar with
//! next-word predictions; tapping a next-word chip inserts the word + a space.
//!
//! ## Seams for later work
//! * **IM injection:** a host will drain [`OskWidget::take_pending_text`]
//!   (and observe [`OskWidget::on_key`]) and forward commit-strings through a
//!   `zwp_input_method_v2` client to the focused surface. Nothing in this
//!   widget assumes a particular sink.
//! * **Neural swipe:** the decoder is behind the
//!   [`crate::swipe::SwipeModel`] trait. The shipping model is the geometric
//!   [`crate::swipe::GeometricSwipeModel`]; a future FUTO/ExecuTorch
//!   `FutoSwipeModel` implementing the same trait can be swapped in via
//!   [`OskWidget::with_swipe_model`] with no other change.

use std::collections::{HashMap, VecDeque};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::a11y::{AccessNode, AccessState, Action as AccessibilityAction, Role};
use ui::icons::{self, Icon};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{ActionSpec, OperationKind, Resource, RetryPolicy};
use ui::widget::Widget;

use crate::futo::FutoSwipeModel;
use crate::layout::{InputPurpose, Key, KeyAction, KeyId, KeyboardLayout, ShiftState, key_rects};
use crate::predict::Predictor;
use crate::suggest::{Suggestion, suggestion_rects};
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
use crate::toolbar::{OskToolbarAction, OskToolbarItem, toolbar_rects};

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
/// The diagnostic mirror is deliberately bounded: the live injector is the
/// delivery path, while this buffer exists only for hosts/tests that drain it.
pub const MAX_PENDING_TEXT_BYTES: usize = 16 * 1024;
const MAX_KEY_LOG_ENTRIES: usize = 1_024;
const MAX_TOOLBAR_LOG_ENTRIES: usize = 128;
const MAX_GESTURE_POINTS: usize = 256;
const MAX_TRACKED_WORD_SCALARS: usize = 128;
const MAX_KEYBOARD_CONTENT_WIDTH: f64 = 1_120.0;
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
    /// boundary). Used to replace-on-suggestion and to feed prefix completion.
    current_word: String,
    /// Once a word exceeds the predictor cap, do not begin tracking another
    /// suffix of that same word. It resets only at a real word boundary.
    current_word_overflowed: bool,
    /// The previously committed word, for bigram next-word context.
    prev_word: Option<String>,
    /// While the left button is down, the captured pointer path (logical px).
    /// `Some` between press and release; `None` otherwise.
    gesture_path: Option<Vec<Point>>,

    // --- output ---
    pending_text: String,
    pending_text_dropped_bytes: usize,
    key_log: VecDeque<KeyAction>,
    dropped_key_actions: usize,
    toolbar_log: VecDeque<OskToolbarAction>,
    dropped_toolbar_actions: usize,
    on_key: Option<Box<dyn FnMut(KeyAction)>>,
    on_toolbar: Option<Box<dyn FnMut(OskToolbarAction)>>,

    // --- dirty tracking ---
    /// Set when the displayed layer/layout changes so the host rebuilds the
    /// taffy node (its intrinsic height tracks the row count). Cleared by
    /// `take_structure_dirty`.
    structure_dirty: bool,
    /// A resource operation changed durable per-user configuration. `sync`
    /// writes it into the app document once, then remote document changes are
    /// adopted on later syncs.
    configuration_dirty: bool,

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
            swipe_model: default_swipe_model(),
            predictor: Predictor::english(),
            suggestions: Vec::new(),
            current_word: String::new(),
            current_word_overflowed: false,
            prev_word: None,
            gesture_path: None,
            pending_text: String::new(),
            pending_text_dropped_bytes: 0,
            key_log: VecDeque::new(),
            dropped_key_actions: 0,
            toolbar_log: VecDeque::new(),
            dropped_toolbar_actions: 0,
            on_key: None,
            on_toolbar: None,
            structure_dirty: false,
            configuration_dirty: false,
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

    /// Imperative per-activation callback. Receives the *emitted* action
    /// (already shift-cased for letters).
    #[must_use]
    pub fn on_key<F: FnMut(KeyAction) + 'static>(mut self, f: F) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }

    /// Imperative toolbar-tap callback.
    #[must_use]
    pub fn on_toolbar<F: FnMut(OskToolbarAction) + 'static>(mut self, f: F) -> Self {
        self.on_toolbar = Some(Box::new(f));
        self
    }

    // --- output accessors ----------------------------------------------

    /// Peek at the accumulated typed text (mirror of `FilePicker::outcome`).
    pub fn pending_text(&self) -> &str {
        &self.pending_text
    }

    /// Drain the accumulated typed text (mirror of
    /// `FilePicker::take_outcome`). The host calls this and injects the
    /// returned string into the focused surface.
    pub fn take_pending_text(&mut self) -> String {
        std::mem::take(&mut self.pending_text)
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
    /// key delivery still reached `on_key`; callers that rely on this mirror
    /// must drain it promptly instead of treating it as an unbounded transcript.
    pub fn pending_text_dropped_bytes(&self) -> usize {
        self.pending_text_dropped_bytes
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
        self.configuration_dirty = true;
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
        }
        self.refresh_next_word_suggestions();
        self.configuration_dirty = true;
    }

    /// Forget locally adapted suggestion ranking without changing the privacy
    /// preference. This has no effect on the bundled dictionary/bigram model.
    pub fn clear_personalization(&mut self) {
        self.predictor.clear_learned();
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
        self.configuration_dirty = true;
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

    /// Apply an emitted [`KeyAction`]: update text/state, log it, fire the
    /// callback. Also maintains the in-progress **current word** buffer and
    /// refreshes the suggestion bar (prefix completion while typing; next-word
    /// predictions after a boundary).
    fn apply_action(&mut self, action: KeyAction) {
        match &action {
            KeyAction::Char(s) => {
                self.append_pending_text(s);
                // One-shot shift resets after a character; caps is sticky.
                if self.shift == ShiftState::Shift {
                    self.shift = ShiftState::Off;
                }
                // Track the current word: letters extend it, punctuation ends
                // it (commits a word boundary).
                if s.chars().all(char::is_alphabetic) {
                    if !self.current_word_overflowed
                        && self
                            .current_word
                            .chars()
                            .count()
                            .saturating_add(s.chars().count())
                            <= MAX_TRACKED_WORD_SCALARS
                    {
                        self.current_word.push_str(s);
                        self.refresh_completion_suggestions();
                    } else {
                        // Keep delivery intact but stop retaining a hostile or
                        // accidental mega-token solely for prediction.
                        self.current_word.clear();
                        self.current_word_overflowed = true;
                        self.suggestions.clear();
                    }
                } else {
                    self.commit_current_word();
                }
            }
            KeyAction::Space => {
                self.append_pending_text(" ");
                self.commit_current_word();
            }
            KeyAction::Enter => {
                self.append_pending_text("\n");
                self.commit_current_word();
                // A newline clears next-word context.
                self.prev_word = None;
                self.suggestions.clear();
            }
            KeyAction::Backspace => {
                self.pending_text.pop();
                // Keep the current-word buffer in sync with a backspace.
                if self.current_word.pop().is_none() {
                    // Backspaced past the word start — context is now stale.
                    self.prev_word = None;
                    self.suggestions.clear();
                } else {
                    self.refresh_completion_suggestions();
                }
            }
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
        self.key_log.push_back(action.clone());
        if let Some(cb) = &mut self.on_key {
            cb(action);
        }
    }

    /// Finish the in-progress word (at a space/punctuation/enter boundary):
    /// it becomes the bigram context for next-word prediction, and the bar is
    /// refilled with predictions.
    fn commit_current_word(&mut self) {
        if !self.current_word.is_empty() {
            let committed = self.current_word.to_lowercase();
            if self.personalization_enabled {
                self.predictor
                    .learn_transition(self.prev_word.as_deref(), &committed);
            }
            self.prev_word = Some(committed);
            self.current_word.clear();
        }
        self.current_word_overflowed = false;
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

    /// Replace the in-progress word in `pending_text` with `word`. Used when a
    /// swipe auto-inserts its top candidate and when the user taps a chip that
    /// completes/corrects the current word. The replaced word becomes the
    /// next-word context, and the bar is refilled with predictions.
    fn replace_current_word(&mut self, word: &str) {
        // Strip the in-progress word's letters from the tail of pending_text.
        let drop = self.current_word.chars().count();
        for _ in 0..drop {
            self.pending_text.pop();
        }
        self.append_pending_text(word);
        self.current_word.clear();
        self.current_word_overflowed = false;
        let committed = word.to_lowercase();
        if self.personalization_enabled {
            self.predictor
                .learn_transition(self.prev_word.as_deref(), &committed);
        }
        self.prev_word = Some(committed);
        self.refresh_next_word_suggestions();
    }

    /// Activate a suggestion chip by index. Two cases:
    ///
    /// * **completion/candidate** (there is an in-progress word) → replace it.
    /// * **next-word** (no in-progress word) → insert the word + a space.
    ///
    /// Returns whether a chip was activated.
    pub fn activate_suggestion(&mut self, index: usize) -> bool {
        let Some(s) = self.suggestions.get(index).cloned() else {
            return false;
        };
        if self.current_word.is_empty() {
            // Next-word pick: insert "word ".
            self.append_pending_text(&s.word);
            self.append_pending_text(" ");
            let committed = s.word.to_lowercase();
            if self.personalization_enabled {
                self.predictor
                    .learn_transition(self.prev_word.as_deref(), &committed);
            }
            self.prev_word = Some(committed);
            self.current_word_overflowed = false;
            self.refresh_next_word_suggestions();
        } else {
            self.replace_current_word(&s.word);
        }
        true
    }

    /// Decode a captured gesture `path` as a swipe. If it yields candidates,
    /// auto-insert the top one (replacing any in-progress word) and show the
    /// ranked alternatives in the bar. Returns whether the swipe produced a
    /// word.
    fn decode_and_apply_swipe(&mut self, path: &[Point]) -> bool {
        let layout = self.current_layout().clone();
        let cands = self
            .swipe_model
            .decode(path, &self.key_geom, &layout, MAX_SUGGESTIONS);
        // `cands.first()` also covers the empty case, so this doubles as the
        // emptiness check.
        let Some(top_cand) = cands.first() else {
            return false;
        };
        // The swipe inserts a fresh word: if mid-word (rare, e.g. a swipe
        // started after some taps) replace it; otherwise just append.
        let top = top_cand.word.clone();
        if self.current_word.is_empty() {
            // Auto-insert acts like a completed word.
            self.append_pending_text(&top);
            self.prev_word = Some(top.to_lowercase());
        } else {
            self.replace_current_word(&top);
        }
        // Show the ranked candidates so the user can correct the auto-pick.
        self.suggestions = cands
            .into_iter()
            .map(|c| Suggestion::new(c.word, c.score))
            .collect();
        // After a swipe the word is committed; the next tap on a chip should
        // *correct* it, so keep current_word empty but remember the candidates.
        self.current_word.clear();
        self.current_word_overflowed = false;
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

    fn append_pending_text(&mut self, text: &str) {
        let remaining = MAX_PENDING_TEXT_BYTES.saturating_sub(self.pending_text.len());
        if remaining == 0 {
            self.pending_text_dropped_bytes =
                self.pending_text_dropped_bytes.saturating_add(text.len());
            return;
        }
        let mut end = remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if let Some(prefix) = text.get(..end) {
            self.pending_text.push_str(prefix);
        }
        self.pending_text_dropped_bytes = self
            .pending_text_dropped_bytes
            .saturating_add(text.len().saturating_sub(end));
    }
}

fn read_config_text(state: &AutoCommit, path: &ObjId, key: &str) -> Option<String> {
    match state.get(path, key) {
        Ok(Some((Value::Scalar(value), _))) => value.to_str().map(ToOwned::to_owned),
        _ => None,
    }
}

fn read_config_bool(state: &AutoCommit, path: &ObjId, key: &str) -> Option<bool> {
    match state.get(path, key) {
        Ok(Some((Value::Scalar(value), _))) => value.to_bool(),
        _ => None,
    }
}

/// Interaction/visual flags for one drawn cell, bundled so [`draw_cell`]
/// doesn't take four separate `bool` parameters.
#[derive(Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this struct IS the bool-bundling fix for draw_cell's argument count; \
              its four fields are independent interaction/visual flags for one \
              cell, and splitting them into sub-structs would not express any \
              real invariant between them"
)]
struct CellState {
    hovered: bool,
    pressed: bool,
    /// Accent styling (e.g. an active toggle key).
    accent: bool,
    /// Solid (vs. outline) icon glyph — used for a latched state like
    /// Caps-Lock's Shift key.
    icon_filled: bool,
}

/// Draw a rounded "button" cell (used for both keys and toolbar items).
#[allow(
    clippy::too_many_arguments,
    reason = "the cell painter keeps renderer, geometry, label, theme and interaction state explicit"
)]
fn draw_cell(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    state: CellState,
    font_scale: f64,
) {
    let CellState {
        hovered,
        pressed,
        accent,
        icon_filled,
    } = state;
    let t = &theme.tokens;
    // Inset so adjacent cells have a visible gutter.
    let pad = (rect.height() * 0.06).clamp(2.0, 6.0);
    let cell = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
    let bg = if pressed {
        theme.bg_active
    } else if hovered {
        theme.bg_hover
    } else if accent {
        t.colors.surfaces.overlay
    } else {
        t.colors.surfaces.raised
    };
    let shape = RoundedRect::from_rect(cell, f64::from(t.radii.md));
    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
    let border = if accent {
        theme.border_focus
    } else {
        t.colors.border_standard
    };
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        border,
        None,
        &shape,
    );

    let fg = if accent {
        theme.accent_primary
    } else {
        t.colors.text_primary
    };

    if let Some(icon) = icon {
        let s = (cell.height() * 0.5).clamp(14.0, 26.0);
        let ix = cell.center().x - s / 2.0;
        let iy = cell.center().y - s / 2.0;
        let icon_rect = Rect::new(ix, iy, ix + s, iy + s);
        if icon_filled {
            // A latched state (e.g. Caps-Lock's Shift key) reads as a solid
            // glyph, distinct from the outline of the momentary state.
            icon.draw_filled(scene, transform, icon_rect, fg);
        } else {
            icons::draw_icon(
                scene,
                transform,
                icon_rect,
                icon,
                fg,
                icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        return;
    }

    if label.is_empty() {
        return;
    }
    let font = (cell.height() * font_scale).clamp(11.0, 30.0) as f32;
    let (tw, _th) = text_renderer.measure(label, font, None);
    let tx = cell.center().x - f64::from(tw) / 2.0;
    let ty = cell.y0 + (cell.height() - f64::from(font)) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        font,
        fg,
        transform * Affine::translate((tx, ty)),
        None,
    );
}

/// Draw one suggestion chip: a pill with the word, themed by state. The `top`
/// (best / auto-inserted) chip is accented; the rest are neutral. All colours
/// come from theme tokens.
#[allow(
    clippy::too_many_arguments,
    reason = "the suggestion painter keeps renderer, geometry, word and chip state explicit"
)]
fn draw_suggestion_chip(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    word: &str,
    hovered: bool,
    pressed: bool,
    top: bool,
) {
    let t = &theme.tokens;
    let pad_x = (rect.width() * 0.06).clamp(3.0, 10.0);
    let pad_y = (rect.height() * 0.14).clamp(2.0, 8.0);
    let chip = Rect::new(
        rect.x0 + pad_x,
        rect.y0 + pad_y,
        rect.x1 - pad_x,
        rect.y1 - pad_y,
    );
    let bg = if pressed {
        theme.bg_active
    } else if hovered {
        theme.bg_hover
    } else if top {
        // The accent fill at low alpha reads as "selected" without shouting.
        theme.tokens.colors.surfaces.overlay
    } else {
        theme.tokens.colors.surfaces.base
    };
    let shape = RoundedRect::from_rect(chip, (chip.height() * 0.45).min(f64::from(t.radii.lg)));
    scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
    if top {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            theme.border_focus,
            None,
            &shape,
        );
    }

    let fg = if top {
        theme.accent_primary
    } else {
        t.colors.text_primary
    };
    if word.is_empty() {
        return;
    }
    let font = (chip.height() * 0.5).clamp(12.0, 22.0) as f32;
    let (tw, _th) = text_renderer.measure(word, font, None);
    let tx = chip.center().x - f64::from(tw) / 2.0;
    let ty = chip.y0 + (chip.height() - f64::from(font)) / 2.0;
    text_renderer.draw_text(
        scene,
        word,
        font,
        fg,
        transform * Affine::translate((tx, ty)),
        None,
    );
}

impl Widget for OskWidget {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["osk.layout", "osk.suggestions"]
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        // Resource actions intentionally cover only durable configuration:
        // layouts and privacy/suggestion preferences can be searched, audited,
        // replayed and invoked by an agent. Individual keystrokes are
        // position-dependent, privacy-sensitive input, so they remain direct
        // IM delivery rather than invented collaborative/history actions.
        let mut keyboard = Resource::new("osk.keyboard", "osk.keyboard", "On-screen keyboard");

        let mut layout = Resource::new(
            "osk.keyboard.layout",
            "osk.layout",
            format!(
                "Keyboard layout ({})",
                self.preferred_text_layout.to_uppercase()
            ),
        );
        for (id, label, value) in [
            ("qwerty", "Use QWERTY layout", "qwerty"),
            ("azerty", "Use AZERTY layout", "azerty"),
        ] {
            if let Ok(spec) =
                ActionSpec::operation(id, label, OperationKind::Effect, "osk.layout", &value)
            {
                layout
                    .actions
                    .push(spec.retry_policy(RetryPolicy::AtMostOnce));
            }
        }
        let mut suggestions = Resource::new(
            "osk.keyboard.suggestions",
            "osk.suggestions",
            "Suggestions and privacy",
        );
        let suggestion_action = if self.suggestions_enabled {
            ("disable", "Turn off suggestions", "disable")
        } else {
            ("enable", "Turn on suggestions", "enable")
        };
        let personal_action = if self.personalization_enabled {
            (
                "disable-personal",
                "Turn off personalized suggestions",
                "disable-personal",
            )
        } else {
            (
                "enable-personal",
                "Turn on personalized suggestions",
                "enable-personal",
            )
        };
        for (id, label, value) in [
            suggestion_action,
            personal_action,
            (
                "clear-personal",
                "Clear learned suggestions",
                "clear-personal",
            ),
        ] {
            if let Ok(spec) =
                ActionSpec::operation(id, label, OperationKind::Effect, "osk.suggestions", &value)
            {
                suggestions
                    .actions
                    .push(spec.retry_policy(RetryPolicy::AtMostOnce));
            }
        }
        keyboard.children.push(layout);
        keyboard.children.push(suggestions);
        vec![keyboard]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != OperationKind::Effect {
            return Ok(false);
        }
        let value: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        match type_id {
            "osk.layout" => return Ok(self.select_text_layout(&value)),
            "osk.suggestions" => match value.as_str() {
                "enable" => self.set_suggestions_enabled(true),
                "disable" => self.set_suggestions_enabled(false),
                "enable-personal" => self.set_personalization_enabled(true),
                "disable-personal" => self.set_personalization_enabled(false),
                "clear-personal" => self.clear_personalization(),
                _ => return Ok(false),
            },
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.configuration_dirty {
            let _ = state.put(path, CONFIG_LAYOUT_KEY, self.preferred_text_layout.as_str());
            let _ = state.put(path, CONFIG_SUGGESTIONS_KEY, self.suggestions_enabled);
            let _ = state.put(
                path,
                CONFIG_PERSONALIZATION_KEY,
                self.personalization_enabled,
            );
            self.configuration_dirty = false;
            return;
        }
        let layout = read_config_text(state, path, CONFIG_LAYOUT_KEY);
        let suggestions = read_config_bool(state, path, CONFIG_SUGGESTIONS_KEY);
        let personalization = read_config_bool(state, path, CONFIG_PERSONALIZATION_KEY);
        if let Some(layout) = layout {
            // Remote/persisted config cannot introduce an arbitrary layout: it
            // must still be one registered in this OSK process.
            self.select_text_layout_from_sync(&layout);
        }
        if let Some(enabled) = suggestions {
            self.set_suggestions_enabled_from_sync(enabled);
        }
        if let Some(enabled) = personalization {
            self.set_personalization_enabled_from_sync(enabled);
        }
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
        let content_width = self.rect.width().min(MAX_KEYBOARD_CONTENT_WIDTH);
        let content_x0 = self.rect.center().x - content_width * 0.5;
        self.content_rect = Rect::new(
            content_x0,
            self.rect.y0,
            content_x0 + content_width,
            self.rect.y1,
        );

        // Top → bottom bands: toolbar, suggestion bar, keys (keys fill the
        // rest). Clamp each band so a too-short surface degrades gracefully.
        let h = self.content_rect.height();
        let toolbar_h = f64::from(self.toolbar_height).min(h);
        let suggestion_h = f64::from(self.suggestion_height).min((h - toolbar_h).max(0.0));
        let toolbar_y1 = self.content_rect.y0 + toolbar_h;
        let suggestion_y1 = toolbar_y1 + suggestion_h;

        let toolbar_area = Rect::new(
            self.content_rect.x0,
            self.content_rect.y0,
            self.content_rect.x1,
            toolbar_y1,
        );
        let suggestion_area = Rect::new(
            self.content_rect.x0,
            toolbar_y1,
            self.content_rect.x1,
            suggestion_y1,
        );
        let key_area = Rect::new(
            self.content_rect.x0,
            suggestion_y1,
            self.content_rect.x1,
            self.content_rect.y1,
        );

        self.toolbar_geom = toolbar_rects(&self.toolbar, toolbar_area);
        self.suggestion_geom = suggestion_rects(&self.suggestions, suggestion_area);
        // Stash the suggestion-bar area so an empty bar can still render its
        // background + hit-test correctly (suggestion_rects is empty then).
        self.suggestion_area = suggestion_area;
        let geom = {
            let layout = self.current_layout();
            key_rects(layout, key_area, self.shift)
        };
        self.key_geom = geom;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the custom key-grid renderer keeps paint order and hit-state styling auditable in one pass"
    )]
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
        // Surface background.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // Toolbar.
        for (i, r) in &self.toolbar_geom {
            // `toolbar_geom` is built from `self.toolbar` each frame, so `*i`
            // is always in range; `get` avoids a panic if the geometry is
            // ever stale for one frame.
            let Some(item) = self.toolbar.get(*i) else {
                continue;
            };
            let hovered = self.hovered_tool == Some(*i);
            let pressed = self.pressed_tool == Some(*i);
            draw_cell(
                scene,
                transform,
                text_renderer,
                theme,
                *r,
                item.icon,
                &item.label,
                CellState {
                    hovered,
                    pressed,
                    ..Default::default()
                },
                0.34,
            );
        }

        // Suggestion bar: a subtle band, then a tappable chip per suggestion.
        if self.suggestion_area.area() > 0.0 {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &self.suggestion_area,
            );
            for (i, r) in &self.suggestion_geom {
                let Some(s) = self.suggestions.get(*i) else {
                    continue;
                };
                let hovered = self.hovered_sugg == Some(*i);
                let pressed = self.pressed_sugg == Some(*i);
                // The top chip is the highlighted (auto / best) pick.
                let top = *i == 0;
                draw_suggestion_chip(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *r,
                    &s.word,
                    hovered,
                    pressed,
                    top,
                );
            }
            if self.suggestion_geom.is_empty() {
                let hint = if self.suggestions_enabled {
                    "Type or swipe to see suggestions"
                } else {
                    "Suggestions are off"
                };
                let font = (self.suggestion_area.height() * 0.38).clamp(12.0, 18.0) as f32;
                let (width, _) = text_renderer.measure(hint, font, None);
                let x = self.suggestion_area.center().x - f64::from(width) * 0.5;
                let y = self.suggestion_area.y0
                    + (self.suggestion_area.height() - f64::from(font)) * 0.5;
                text_renderer.draw_text(
                    scene,
                    hint,
                    font,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((x, y)),
                    None,
                );
            }
        }

        // Keys.
        let shift = self.shift;
        let layout = self.current_layout();
        for (id, rect) in &self.key_geom {
            let Some(key) = layout.key(*id) else { continue };
            let label = key.display_label(shift);
            let is_action = !matches!(&key.action, KeyAction::Char(_));
            let shift_active = matches!(&key.action, KeyAction::Shift) && shift.is_upper();
            let hovered = self.hovered_key == Some(*id);
            let pressed = self.pressed_key == Some(*id);
            // Letters get a larger glyph; multi-char action labels smaller.
            let font_scale = if Key::is_letter(key) { 0.42 } else { 0.30 };
            // The Shift key's icon fills solid while Caps-Lock is latched, so a
            // sticky upper-case reads differently from a one-shot shift.
            let icon_filled =
                matches!(&key.action, KeyAction::Shift) && shift == ShiftState::CapsLock;
            draw_cell(
                scene,
                transform,
                text_renderer,
                theme,
                *rect,
                key.icon,
                &label,
                CellState {
                    hovered,
                    pressed,
                    accent: is_action || shift_active,
                    icon_filled,
                },
                font_scale,
            );
        }
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
                        // A short tap stays a normal key press.
                        self.activate_key(id);
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        assert!(osk.activate_label("h"));
        assert!(osk.activate_label("i"));
        assert_eq!(osk.pending_text(), "hi");
    }

    #[test]
    fn accessibility_exposes_and_operates_each_painted_key() {
        let mut osk = OskWidget::new();
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
            OskWidget::new().on_key(move |a| seen.borrow_mut().push(a))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("a");
        let log = osk.take_key_actions();
        assert_eq!(log, vec![KeyAction::Char("a".into())]);
        assert_eq!(*seen.borrow(), vec![KeyAction::Char("a".into())]);
    }

    #[test]
    fn shift_uppercases_next_letter_then_resets() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("?123");
        assert!(osk.take_structure_dirty());
        assert!(!osk.take_structure_dirty(), "flag is rising-edge");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("⌫");
        assert_eq!(osk.pending_text(), "h");
    }

    #[test]
    fn space_and_take_pending_text_drains() {
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_label("h");
        osk.activate_label("i");
        osk.activate_label("space");
        assert_eq!(osk.take_pending_text(), "hi ");
        assert_eq!(osk.pending_text(), "", "drained");
    }

    #[test]
    fn pointer_tap_routes_to_key_under_cursor() {
        use winit::event::{DeviceId, Modifiers};
        let mut osk = OskWidget::new();
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
            OskWidget::new().on_toolbar(move |a| seen.borrow_mut().push(a))
        };
        lay_out(&mut osk, 800.0, 360.0);
        osk.activate_tool(0); // default[0] == Paste
        assert_eq!(osk.take_toolbar_actions(), vec![OskToolbarAction::Paste]);
        assert_eq!(*seen.borrow(), vec![OskToolbarAction::Paste]);
    }

    #[test]
    fn layout_action_switches_active_letter_layout() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 800.0, 360.0);
        assert_eq!(osk.active_layout_name(), "qwerty");
        osk.set_purpose(InputPurpose::Telephone);
        assert_eq!(osk.purpose(), InputPurpose::Telephone);
        assert_eq!(osk.active_layout_name(), "phone");
        assert!(osk.take_structure_dirty(), "purpose change relays out");
    }

    #[test]
    fn with_purpose_starts_on_the_matching_layout() {
        let osk = OskWidget::new().with_purpose(InputPurpose::Pin);
        assert_eq!(osk.purpose(), InputPurpose::Pin);
        assert_eq!(osk.active_layout_name(), "pin");
    }

    #[test]
    fn numeric_purpose_types_digits_and_has_no_letters() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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

        let mut osk = OskWidget::new().with_purpose(InputPurpose::Telephone);
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
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        let path = path_through(&osk, "quick");
        assert!(osk.swipe(&path), "swipe decoded a word");
        // Top candidate auto-inserted.
        assert_eq!(osk.pending_text(), "quick");
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
        let mut osk = OskWidget::new();
        lay_out(&mut osk, 720.0, 360.0);
        // Type a prefix that yields completions, e.g. "th" -> the/this/that...
        osk.activate_label("t");
        osk.activate_label("h");
        assert_eq!(osk.current_word(), "th");
        let chips: Vec<String> = osk.suggestions().iter().map(|s| s.word.clone()).collect();
        assert!(!chips.is_empty(), "completions shown for 'th': {chips:?}");
        // Tap the top chip; it replaces the in-progress word with the full word.
        let top = chips[0].clone();
        assert!(osk.activate_suggestion(0));
        assert_eq!(osk.pending_text(), top, "chip replaced 'th' with '{top}'");
        assert_eq!(osk.current_word(), "", "word committed after chip tap");
    }

    #[test]
    fn next_word_chip_inserts_word_and_space() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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

        let mut osk = OskWidget::new();
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
        assert!(OskWidget::new().is_visible());
    }

    #[test]
    fn hidden_builder_starts_invisible() {
        assert!(!OskWidget::new().hidden().is_visible());
    }

    #[test]
    fn hidden_keyboard_takes_zero_height() {
        // A visible keyboard occupies its intrinsic height; when the compositor
        // has hidden it the widget requests no height and paints nothing.
        let mut visible = OskWidget::new();
        lay_out(&mut visible, 720.0, 360.0);
        assert!(visible.get_rect().height() > 0.0);

        let mut hidden = OskWidget::new().hidden();
        lay_out(&mut hidden, 720.0, 360.0);
        assert_eq!(hidden.get_rect().height(), 0.0);
    }

    #[test]
    fn set_visible_toggles_and_marks_structure_dirty() {
        let mut osk = OskWidget::new().hidden();
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
        let mut osk = OskWidget::new().hidden();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        osk.on_external_event("osk:show", &mut doc, &root);
        assert!(osk.is_visible());

        osk.on_external_event("osk:hide", &mut doc, &root);
        assert!(!osk.is_visible());
    }

    #[test]
    fn external_purpose_command_switches_layout() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new().hidden();
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
        let osk = OskWidget::new();
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
        assert_eq!(
            osk.local_resource_operation_handlers(),
            ["osk.layout", "osk.suggestions"]
        );
    }

    #[test]
    fn configuration_resource_persists_and_restores_preferred_layout() {
        let mut osk = OskWidget::new();
        let mut doc = AutoCommit::new();
        let payload = serde_json::to_vec("azerty").expect("serialize layout action");
        assert!(
            osk.invoke_resource_operation(
                OperationKind::Effect,
                "osk.layout",
                &payload,
                &doc,
                &automerge::ROOT,
            )
            .expect("valid layout action")
        );
        osk.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            read_config_text(&doc, &automerge::ROOT, CONFIG_LAYOUT_KEY).as_deref(),
            Some("azerty")
        );

        let mut restored = OskWidget::new();
        restored.sync(&mut doc, &automerge::ROOT);
        assert_eq!(restored.active_layout_name(), "azerty");
    }

    #[test]
    fn responsive_content_caps_on_desktop_but_keeps_tablet_width() {
        let mut desktop = OskWidget::new();
        lay_out(&mut desktop, 1_920.0, 480.0);
        assert_eq!(desktop.content_rect.width(), MAX_KEYBOARD_CONTENT_WIDTH);
        assert!(desktop.content_rect.x0 > 0.0, "desktop keyboard is centred");

        let mut tablet = OskWidget::new();
        lay_out(&mut tablet, 768.0, 420.0);
        assert_eq!(tablet.content_rect.width(), 768.0);
        assert_eq!(tablet.content_rect.x0, 0.0);
    }

    #[test]
    fn disabled_suggestions_are_announced_and_not_materialised() {
        let mut osk = OskWidget::new();
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
        let mut osk = OskWidget::new();
        osk.apply_action(KeyAction::Char("x".repeat(MAX_PENDING_TEXT_BYTES + 11)));
        assert_eq!(osk.pending_text().len(), MAX_PENDING_TEXT_BYTES);
        assert_eq!(osk.pending_text_dropped_bytes(), 11);

        for _ in 0..(MAX_KEY_LOG_ENTRIES + 5) {
            osk.apply_action(KeyAction::Shift);
        }
        assert_eq!(osk.key_log.len(), MAX_KEY_LOG_ENTRIES);
        assert!(osk.dropped_key_actions() >= 6);
    }
}
