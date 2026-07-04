//! Built-in screen reader.
//!
//! This is the first-party, spatially-aware reader that makes qOS narrate
//! itself. It sits on top of two already-shipped layers:
//!
//!   * the **semantic tree** ([`crate::a11y::AccessNode`], produced per-widget
//!     by [`Widget::accessibility`](crate::widget::Widget::accessibility) and
//!     assembled by the [`App`](crate::app::App)); and
//!   * the **speech engine** [`tts`] — an offline, interrupt-aware TTS stack
//!     whose [`Speaker`](tts::Speaker) trait this module drives.
//!
//! The reader owns a `Box<dyn Speaker>` (defaulting to
//! [`tts::default_speaker`], which never panics — it falls back to a
//! [`NullSpeaker`](tts::NullSpeaker) when no speech stack is installed) and
//! a [`SpeechQueue`](tts::SpeechQueue) so chatty focus narration coalesces
//! (rapid arrow navigation interrupts the previous utterance instead of queuing
//! a backlog — see [`ScreenReader::announce_focus`]).
//!
//! ## What it narrates
//!
//!   * **Focus changes** — when keyboard focus moves, the reader composes a
//!     phrase from the focused node's role + name + value + state
//!     ([`describe`]) and speaks it, *interrupting* the previous utterance.
//!   * **Value changes** — a slider drag / spin-button step announces the new
//!     value ([`ScreenReader::announce_value_change`]).
//!   * **Typing echo** — character / word / deletion feedback while editing a
//!     text field ([`ScreenReader::echo_char`] / [`echo_word`] /
//!     [`echo_deletion`]).
//!   * **Live regions** — out-of-band announcements (errors, toasts) at an
//!     explicit priority ([`ScreenReader::announce`]).
//!
//! ## Reading commands
//!
//! Beyond reacting to focus, the reader can drive a **reading cursor** through
//! the flattened accessible tree (DFS, presentational nodes collapsed — the
//! same reading order the AccessKit bridge exports):
//!
//!   * [`ScreenReader::read_all`] — speak the whole reading order;
//!   * [`ScreenReader::read_next`] / [`read_prev`] — step the cursor and speak
//!     the node it lands on.
//!
//! ## The pure core: [`describe`]
//!
//! [`describe`] is a **pure function** `AccessNode -> String`. It carries no
//! speech engine, no I/O, and no state, so it is exhaustively unit-testable —
//! the test module at the bottom is a role × state truth table. `ScreenReader`
//! is the thin imperative shell that pushes `describe`'s output through the
//! queue into a [`Speaker`](tts::Speaker).
//!
//! ## i18n
//!
//! The narration vocabulary (role words like "checkbox", state words like
//! "checked", "dimmed") is resolved through the toolkit's
//! [`i18n!`](crate::i18n) bundles. Each word has a stable key (`sr-role-checkbox`,
//! `sr-state-checked`, …); when the active locale has loaded a matching FTL
//! resource the narration speaks the translated word, otherwise it falls back to
//! the **clear English constant** baked into this module. So the default locale
//! and headless builds (which load no resources) narrate exactly as before, and
//! switching locale changes the spoken language without touching the reader
//! logic. The constants live in the `phrase` submodule next to their keys.

use tts::{NullSpeaker, Priority, Speaker, SpeechQueue, Utterance, default_speaker};

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;

/// Centralized narration vocabulary. Every word has a stable FTL key plus a
/// clear English fallback. [`localized`] resolves the key through the toolkit's
/// i18n bundles and uses the fallback when no resource is loaded for the active
/// locale — so the default-locale and headless paths narrate in English while a
/// loaded `screen-reader.ftl` localizes the same narration logic.
mod phrase {
    use crate::a11y::Role;

    /// Resolve `key` through i18n, falling back to `english` when no override is
    /// loaded. [`crate::i18n::I18nManager::lookup`] returns the key verbatim for
    /// an unknown message (no resource / no such key), which is exactly the
    /// "nothing loaded" signal — we substitute the English default in that case.
    pub fn localized(key: &str, english: &str) -> String {
        let resolved = crate::i18n!(key);
        if resolved == key {
            english.to_string()
        } else {
            resolved
        }
    }

    /// A human-spoken word for a [`Role`]. Returns `""` for roles that read
    /// better with no role word at all (plain static text / labels / paragraphs
    /// — a screen reader saying "Hello, static text" is noise; it should just
    /// say "Hello").
    pub fn role_word(role: Role) -> String {
        // (FTL key, English fallback). An empty fallback means "no role word".
        let (key, english) = match role {
            Role::Window => ("sr-role-window", "window"),
            Role::Dialog => ("sr-role-dialog", "dialog"),
            Role::Group => ("sr-role-group", "group"),
            // Generic / presentational containers contribute no role word.
            Role::Generic => ("", ""),
            Role::Toolbar => ("sr-role-toolbar", "toolbar"),
            Role::TabBar => ("sr-role-tab-bar", "tab bar"),
            Role::Tab => ("sr-role-tab", "tab"),
            Role::TabPanel => ("sr-role-tab-panel", "tab panel"),
            Role::List => ("sr-role-list", "list"),
            Role::ListItem => ("sr-role-list-item", "list item"),
            Role::Table => ("sr-role-table", "table"),
            Role::Row => ("sr-role-row", "row"),
            Role::Cell => ("sr-role-cell", "cell"),
            Role::Tree => ("sr-role-tree", "tree"),
            Role::TreeItem => ("sr-role-tree-item", "tree item"),
            Role::Menu => ("sr-role-menu", "menu"),
            Role::MenuItem => ("sr-role-menu-item", "menu item"),

            Role::Button => ("sr-role-button", "button"),
            Role::ToggleButton => ("sr-role-toggle-button", "toggle button"),
            Role::Checkbox => ("sr-role-checkbox", "checkbox"),
            Role::RadioButton => ("sr-role-radio-button", "radio button"),
            Role::Switch => ("sr-role-switch", "switch"),
            Role::Slider => ("sr-role-slider", "slider"),
            Role::SpinButton => ("sr-role-spin-button", "spin button"),
            Role::ComboBox => ("sr-role-combo-box", "combo box"),
            Role::TextInput => ("sr-role-text-input", "edit text"),
            Role::MultilineTextInput => ("sr-role-multiline-text-input", "multi-line edit text"),
            Role::Link => ("sr-role-link", "link"),
            Role::ProgressBar => ("sr-role-progress-bar", "progress bar"),
            Role::Spinner => ("sr-role-spinner", "busy indicator"),
            Role::Tooltip => ("sr-role-tooltip", "tooltip"),

            Role::Image => ("sr-role-image", "image"),
            // Labels / static text / paragraphs read as their content only.
            Role::Label => ("", ""),
            Role::StaticText => ("", ""),
            Role::Paragraph => ("", ""),
            // Heading word is produced specially (carries a level), so the
            // generic word is empty here; see `describe`.
            Role::Heading { .. } => ("", ""),
            Role::Document => ("sr-role-document", "document"),
            Role::Canvas => ("sr-role-canvas", "canvas"),
            Role::Unknown => ("", ""),
        };
        if key.is_empty() {
            String::new()
        } else {
            localized(key, english)
        }
    }

    // --- State words. Each resolves its FTL key with the English fallback. ---
    pub fn checked() -> String { localized("sr-state-checked", "checked") }
    pub fn not_checked() -> String { localized("sr-state-not-checked", "not checked") }
    pub fn partially_checked() -> String {
        localized("sr-state-partially-checked", "partially checked")
    }
    /// Switch-flavoured on/off (reads more naturally than "checked" for a
    /// toggle / switch).
    pub fn on() -> String { localized("sr-state-on", "on") }
    pub fn off() -> String { localized("sr-state-off", "off") }
    pub fn selected() -> String { localized("sr-state-selected", "selected") }
    pub fn expanded() -> String { localized("sr-state-expanded", "expanded") }
    pub fn collapsed() -> String { localized("sr-state-collapsed", "collapsed") }
    /// "dimmed" is the long-standing screen-reader word for a disabled control
    /// (Orca / VoiceOver convention) — clearer than "disabled" when spoken.
    pub fn dimmed() -> String { localized("sr-state-dimmed", "dimmed") }
    pub fn read_only() -> String { localized("sr-state-read-only", "read only") }
    pub fn required() -> String { localized("sr-state-required", "required") }
    pub fn invalid() -> String { localized("sr-state-invalid", "invalid entry") }
    pub fn busy() -> String { localized("sr-state-busy", "busy") }
    /// A protected (password) field — the value is never spoken.
    pub fn protected() -> String { localized("sr-state-protected", "protected") }

    /// "heading level N". Localizable with a `$level` variable; falls back to
    /// the English phrasing when no resource is loaded.
    pub fn heading_word(level: u8) -> String {
        let resolved = crate::i18n::I18nManager::format(
            "sr-heading-level",
            &[("level", (level as f64).into())],
        );
        if resolved == "sr-heading-level" {
            format!("heading level {level}")
        } else {
            resolved
        }
    }

    /// "N of M" — an item's position within its collection.
    pub fn position_word(pos: usize, size: usize) -> String {
        let resolved = crate::i18n::I18nManager::format(
            "sr-position",
            &[("pos", (pos as f64).into()), ("size", (size as f64).into())],
        );
        if resolved == "sr-position" {
            format!("{pos} of {size}")
        } else {
            resolved
        }
    }

    /// "level N" — hierarchy depth for nested structures (tree rows).
    pub fn level_word(level: usize) -> String {
        let resolved = crate::i18n::I18nManager::format(
            "sr-level",
            &[("level", (level as f64).into())],
        );
        if resolved == "sr-level" {
            format!("level {level}")
        } else {
            resolved
        }
    }

    // --- Reading-command / live-region phrases ---
    pub fn deleted() -> String { localized("sr-deleted", "deleted") }
    /// Spoken when quick-nav finds no further matching element ahead.
    pub fn no_next() -> String { localized("sr-no-next", "no more") }
    /// Spoken when quick-nav finds no matching element behind.
    pub fn no_prev() -> String { localized("sr-no-prev", "no previous") }
}

/// Compose the spoken phrase for an accessible node — the **pure, heavily
/// unit-tested core** of the screen reader.
///
/// The phrase is assembled as comma-separated segments in a deliberate,
/// screen-reader-conventional order:
///
/// 1. **name** (the primary label), if any;
/// 2. **value** (a text field's contents, a slider's formatted value) — *never*
///    for a [`protected`](crate::a11y::AccessState::protected) node, which
///    instead contributes the word "protected";
/// 3. **role** spoken as a human word ("checkbox", "button"; headings read as
///    "heading level N"; plain text/labels contribute no role word);
/// 4. **states** in priority order: checked/on/off, mixed, selected,
///    expanded/collapsed, required, invalid, read-only, busy, and finally
///    **dimmed** (disabled) last;
/// 5. **position** in its collection ("2 of 4") and hierarchy **level**
///    ("level 3"), if the node carries them.
///
/// Empty segments are dropped, so a bare unnamed `Generic` node yields `""`.
///
/// # Examples
///
/// ```
/// use ui::a11y::{AccessNode, AccessState, Role};
/// use ui::screen_reader::describe;
///
/// // A disabled, checked checkbox named "Accept".
/// let node = AccessNode::new(Role::Checkbox)
///     .with_name("Accept")
///     .with_state(AccessState::new().checked(true).disabled(true));
/// assert_eq!(describe(&node), "Accept, checkbox, checked, dimmed");
/// ```
pub fn describe(node: &AccessNode) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 1. Name.
    if let Some(name) = node.name.as_deref() {
        if !name.is_empty() {
            parts.push(name.to_string());
        }
    }

    // 2. Role word, right after the name — screen readers announce a control
    //    as "name, role, value" (e.g. "Volume, slider, 50 percent"). Headings
    //    carry a level; everything else uses the table.
    match node.role {
        Role::Heading { level } => parts.push(phrase::heading_word(level)),
        other => {
            let w = phrase::role_word(other);
            if !w.is_empty() {
                parts.push(w);
            }
        }
    }

    // 3. Value — after the role. Never for protected fields (the secret must
    //    not be spoken); a protected field instead surfaces the word
    //    "protected" so the user knows a value exists without hearing it.
    if node.state.protected {
        parts.push(phrase::protected());
    } else if let Some(value) = node.value.as_deref() {
        if !value.is_empty() {
            parts.push(value.to_string());
        }
    }

    // 4. States, in spoken priority order.
    push_state_words(node, &mut parts);

    // 5. Position in set ("2 of 4") and hierarchy level ("level 3"), last —
    //    they orient the user within a collection after the control itself is
    //    fully described.
    if let (Some(pos), Some(size)) = (node.pos_in_set, node.set_size) {
        parts.push(phrase::position_word(pos, size));
    }
    if let Some(level) = node.level {
        parts.push(phrase::level_word(level));
    }

    parts.join(", ")
}

/// Concise spoken summary of a node's *current value + state* — without its
/// name or role. Used to announce the **result of activating or adjusting** a
/// focused control (e.g. "checked", "on", "75 percent") so the reader confirms
/// the change without re-reading the whole label every time. Returns an empty
/// string when the node carries no value or state worth speaking.
pub fn describe_state(node: &AccessNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if node.state.protected {
        parts.push(phrase::protected());
    } else if let Some(value) = node.value.as_deref() {
        if !value.is_empty() {
            parts.push(value.to_string());
        }
    }
    push_state_words(node, &mut parts);
    parts.join(", ")
}

/// Append the spoken state words for `node` to `parts`. Switches / toggle
/// buttons read on/off; checkboxes / radios read checked / not checked. A
/// `mixed` (indeterminate) checkbox reads "partially checked" regardless of the
/// nominal `checked` flag.
fn push_state_words(node: &AccessNode, parts: &mut Vec<String>) {
    let s = &node.state;
    let switchy = matches!(node.role, Role::Switch | Role::ToggleButton);

    // Checked / on / off — `mixed` wins (tri-state indeterminate).
    if s.mixed {
        parts.push(phrase::partially_checked());
    } else if let Some(checked) = s.checked {
        if switchy {
            parts.push(if checked { phrase::on() } else { phrase::off() });
        } else {
            parts.push(if checked { phrase::checked() } else { phrase::not_checked() });
        }
    }

    if s.selected {
        parts.push(phrase::selected());
    }

    if let Some(expanded) = s.expanded {
        parts.push(if expanded { phrase::expanded() } else { phrase::collapsed() });
    }

    if s.required {
        parts.push(phrase::required());
    }
    if s.invalid {
        parts.push(phrase::invalid());
    }
    // Read-only only matters for editable controls; a static label is
    // trivially "read only" and saying so is noise.
    if s.read_only && is_editable_role(node.role) {
        parts.push(phrase::read_only());
    }
    if s.busy {
        parts.push(phrase::busy());
    }

    // Disabled ("dimmed") is spoken LAST — it qualifies everything before it.
    if s.disabled {
        parts.push(phrase::dimmed());
    }
}

fn is_editable_role(role: Role) -> bool {
    matches!(
        role,
        Role::TextInput | Role::MultilineTextInput | Role::ComboBox | Role::SpinButton
    )
}

/// The built-in screen reader: an interrupt-coalescing narrator over a
/// [`Speaker`](tts::Speaker).
///
/// Hold one of these in the [`App`](crate::app::App) when
/// [`A11ySettings::screen_reader`](crate::a11y::A11ySettings::screen_reader) is
/// on. The App calls [`announce_focus`](Self::announce_focus) on every focus
/// change and [`announce_value_change`](Self::announce_value_change) when a
/// value-bearing control's value changes; widgets that echo typing call the
/// `echo_*` helpers. Everything funnels through an internal
/// [`SpeechQueue`](tts::SpeechQueue) so a focus storm coalesces to the
/// latest utterance (responsive arrow navigation).
pub struct ScreenReader {
    speaker: Box<dyn Speaker>,
    queue: SpeechQueue,
    /// Reading cursor: an index into the most-recently-flattened reading order.
    /// `None` until the first [`read_next`](Self::read_next) /
    /// [`read_prev`](Self::read_prev) / [`read_all`](Self::read_all).
    cursor: Option<usize>,
    /// The flattened reading order captured by the last reading command, so
    /// `read_next`/`read_prev` can step without re-walking the tree (the App
    /// re-seeds this whenever it issues a reading command on the live tree).
    reading_order: Vec<AccessNode>,
}

impl Default for ScreenReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenReader {
    /// A reader wired to the platform default speech engine
    /// ([`tts::default_speaker`]). Never panics: with no TTS installed this
    /// is a [`NullSpeaker`](tts::NullSpeaker) and narration is silently
    /// dropped, so the reader is safe to construct unconditionally.
    pub fn new() -> Self {
        Self::with_speaker(default_speaker())
    }

    /// A reader over a caller-supplied speaker. Tests inject a
    /// [`NullSpeaker`](tts::NullSpeaker) and assert on
    /// [`NullSpeaker::spoken`](tts::NullSpeaker::spoken).
    pub fn with_speaker(speaker: Box<dyn Speaker>) -> Self {
        Self {
            speaker,
            queue: SpeechQueue::new(),
            cursor: None,
            reading_order: Vec::new(),
        }
    }

    /// A reader over a [`NullSpeaker`](tts::NullSpeaker) — convenience for
    /// tests and headless paths that want to inspect what *would* be spoken.
    pub fn null() -> Self {
        Self::with_speaker(Box::new(NullSpeaker::new()))
    }

    /// Borrow the underlying speaker (e.g. to read back a `NullSpeaker`'s
    /// recorded utterances in a test).
    pub fn speaker(&self) -> &dyn Speaker {
        self.speaker.as_ref()
    }

    /// Set the speech rate (1.0 = engine default; 2.0 ≈ twice as fast).
    pub fn set_rate(&mut self, rate: f32) {
        self.speaker.set_rate(rate);
    }

    /// Select a backend voice (e.g. an espeak-ng voice id like `"en-us"`).
    pub fn set_voice(&mut self, voice: &str) {
        self.speaker.set_voice(voice);
    }

    /// The single funnel every utterance goes through. Pushing onto the queue
    /// applies the interrupt/coalesce rule; an interrupting push first stops the
    /// in-flight speech, then we drain the (now-coalesced) queue into the
    /// speaker. Draining immediately keeps the [`NullSpeaker`] test path simple
    /// and is correct for the daemon backend too (it serializes server-side).
    fn enqueue(&mut self, utterance: Utterance) {
        let interrupted = self.queue.push(utterance);
        if interrupted {
            self.speaker.stop();
        }
        self.queue.drive(self.speaker.as_mut());
    }

    /// Narrate a focus change: compose [`describe`] for the newly-focused node
    /// and speak it, **interrupting** any previous narration. This is the
    /// interrupt-coalescing path — rapid arrow navigation through a toolbar
    /// speaks only the control the user lands on, never a backlog (see the
    /// `focus_storm` test). A reading command's cursor is reset so the next
    /// `read_next`/`read_prev` starts from the current focus context.
    pub fn announce_focus(&mut self, node: &AccessNode) {
        let text = describe(node);
        if text.is_empty() {
            return;
        }
        // Focus narration is the default Message priority with interrupt=true,
        // so a newer focus change drops the older pending one.
        self.enqueue(Utterance::new(text));
        // A focus change relocates the user; invalidate the reading cursor so
        // the next read_next/read_prev re-anchors (the App re-seeds the order).
        self.cursor = None;
    }

    /// Speak arbitrary `text` at the given `priority`, optionally interrupting.
    /// This is the **live-region** entry point: errors / permission prompts use
    /// [`Priority::Important`](tts::Priority) (never dropped, survives a
    /// focus interrupt); polite toasts use
    /// [`Priority::Notification`](tts::Priority).
    pub fn announce(&mut self, text: impl Into<String>, priority: Priority, interrupt: bool) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.enqueue(
            Utterance::new(text)
                .with_priority(priority)
                .with_interrupt(interrupt),
        );
    }

    /// Announce a value change on the focused control (a slider %, a spin
    /// button's number). Spoken at Message priority, interrupting — dragging a
    /// slider should track the latest value, not queue every intermediate one.
    pub fn announce_value_change(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.enqueue(Utterance::new(value));
    }

    /// Typing echo: speak a single just-typed character. Spoken at
    /// [`Priority::Text`](tts::Priority), interrupting, so a fast typist's
    /// echoes coalesce and a focus change cuts them off cleanly.
    pub fn echo_char(&mut self, ch: char) {
        // A space is announced as the word so it isn't silence.
        let text = if ch == ' ' { "space".to_string() } else { ch.to_string() };
        self.enqueue(Utterance::new(text).with_priority(Priority::Text));
    }

    /// Typing echo: speak a just-completed word (e.g. on space/punctuation).
    pub fn echo_word(&mut self, word: &str) {
        if word.is_empty() {
            return;
        }
        self.enqueue(Utterance::new(word.to_string()).with_priority(Priority::Text));
    }

    /// Typing echo: announce a deleted character / selection. If `deleted` is a
    /// single character it's spoken as "X deleted"; otherwise just "deleted".
    pub fn echo_deletion(&mut self, deleted: &str) {
        let text = match deleted.chars().count() {
            0 => phrase::deleted(),
            1 => format!("{deleted} {}", phrase::deleted()),
            _ => phrase::deleted(),
        };
        self.enqueue(Utterance::new(text).with_priority(Priority::Text));
    }

    /// The "silence key": stop all speech immediately and discard anything
    /// pending. Speaks nothing; just shuts up (Orca convention).
    pub fn stop(&mut self) {
        self.queue.clear();
        self.speaker.stop();
    }

    /// Whether the reader currently has speech in progress (delegates to the
    /// backend).
    pub fn is_speaking(&self) -> bool {
        self.speaker.is_speaking()
    }

    // --- Reading commands over the accessible tree ---

    /// Speak the entire reading order of `root` — a "say all". Walks the tree
    /// in DFS order, collapsing presentational (`accessibility() == None`)
    /// nodes exactly as the AccessKit bridge does, and speaks every node's
    /// [`describe`] as bulk [`Priority::Text`](tts::Priority) content (so a
    /// later focus change interrupts it). Seeds the reading cursor at the start
    /// so a subsequent [`read_next`](Self::read_next) continues from there.
    pub fn read_all(&mut self, root: &dyn Widget) {
        self.reading_order = flatten_reading_order(root);
        self.cursor = if self.reading_order.is_empty() { None } else { Some(0) };
        // First utterance interrupts (clears focus chatter); the rest are
        // non-interrupting continuations so they don't cancel each other.
        let mut first = true;
        // Collect phrases first to avoid borrowing `self` across the speaker
        // call while also reading `self.reading_order`.
        let phrases: Vec<String> = self
            .reading_order
            .iter()
            .map(describe)
            .filter(|p| !p.is_empty())
            .collect();
        for p in phrases {
            self.enqueue(
                Utterance::new(p)
                    .with_priority(Priority::Text)
                    .with_interrupt(first),
            );
            first = false;
        }
    }

    /// Re-seed the reading cursor against a freshly-walked tree, anchoring it at
    /// the node matching the given focus (by structural identity is impractical
    /// from an `AccessNode`, so we anchor at index 0 / the start). The App calls
    /// this before a `read_next`/`read_prev` so the cursor walks the *current*
    /// tree. Returns the number of accessible nodes found.
    pub fn seed_reading_order(&mut self, root: &dyn Widget) -> usize {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
        }
        self.reading_order.len()
    }

    /// Move the reading cursor to the next node in reading order and speak it.
    /// Re-walks `root` so the order reflects the live tree. Returns the spoken
    /// node, or `None` at the end of the document (then it speaks nothing).
    pub fn read_next(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(i) => {
                if i + 1 >= self.reading_order.len() {
                    return None;
                }
                i + 1
            }
        };
        self.cursor = Some(next);
        let node = self.reading_order[next].clone();
        let text = describe(&node);
        if !text.is_empty() {
            self.enqueue(Utterance::new(text));
        }
        Some(node)
    }

    /// Move the reading cursor to the previous node in reading order and speak
    /// it. Returns the spoken node, or `None` at the start of the document.
    pub fn read_prev(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        let prev = match self.cursor {
            None => self.reading_order.len() - 1,
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.cursor = Some(prev);
        let node = self.reading_order[prev].clone();
        let text = describe(&node);
        if !text.is_empty() {
            self.enqueue(Utterance::new(text));
        }
        Some(node)
    }

    /// The current reading-cursor index, if any (for tests / diagnostics).
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    // --- Quick navigation: jump the reading cursor by element type ---

    /// Move the reading cursor forward to the next node satisfying `pred` and
    /// speak it. Re-walks `root` so the order is live. Returns the landed-on
    /// node, or `None` (speaking a short "no more" cue) when none remains ahead.
    /// The shared engine behind [`read_next_heading`](Self::read_next_heading)
    /// and [`read_next_control`](Self::read_next_control).
    pub fn read_next_matching(
        &mut self,
        root: &dyn Widget,
        pred: impl Fn(&AccessNode) -> bool,
    ) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        let start = self.cursor.map(|i| i + 1).unwrap_or(0);
        let found = (start..self.reading_order.len()).find(|&i| pred(&self.reading_order[i]));
        match found {
            Some(i) => {
                self.cursor = Some(i);
                let node = self.reading_order[i].clone();
                let text = describe(&node);
                if !text.is_empty() {
                    self.enqueue(Utterance::new(text));
                }
                Some(node)
            }
            None => {
                self.enqueue(Utterance::new(phrase::no_next()));
                None
            }
        }
    }

    /// Move the reading cursor backward to the previous node satisfying `pred`
    /// and speak it. Mirror of [`read_next_matching`](Self::read_next_matching).
    pub fn read_prev_matching(
        &mut self,
        root: &dyn Widget,
        pred: impl Fn(&AccessNode) -> bool,
    ) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        // Search strictly before the cursor; from an unset cursor, scan from the
        // end so the first jump lands on the last matching element.
        let end = self.cursor.unwrap_or(self.reading_order.len());
        let found = (0..end).rev().find(|&i| pred(&self.reading_order[i]));
        match found {
            Some(i) => {
                self.cursor = Some(i);
                let node = self.reading_order[i].clone();
                let text = describe(&node);
                if !text.is_empty() {
                    self.enqueue(Utterance::new(text));
                }
                Some(node)
            }
            None => {
                self.enqueue(Utterance::new(phrase::no_prev()));
                None
            }
        }
    }

    /// Jump to the next heading and speak it ("Insert + H").
    pub fn read_next_heading(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_next_matching(root, |n| is_heading(n))
    }

    /// Jump to the previous heading and speak it ("Insert + Shift + H").
    pub fn read_prev_heading(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_prev_matching(root, |n| is_heading(n))
    }

    /// Jump to the next form field / interactive control and speak it
    /// ("Insert + F").
    pub fn read_next_control(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_next_matching(root, |n| is_form_field(n))
    }

    /// Jump to the previous form field / interactive control and speak it
    /// ("Insert + Shift + F").
    pub fn read_prev_control(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_prev_matching(root, |n| is_form_field(n))
    }
}

/// Whether a node is a heading (any level) — the predicate behind heading
/// quick-navigation.
pub fn is_heading(node: &AccessNode) -> bool {
    matches!(node.role, Role::Heading { .. })
}

/// Whether a node is a "form field" — an interactive control a user can edit or
/// activate. The predicate behind form-field quick-navigation. Deliberately
/// covers the editable + actionable controls (buttons, toggles, inputs, ranges,
/// selectors) but not structural containers or static content.
pub fn is_form_field(node: &AccessNode) -> bool {
    matches!(
        node.role,
        Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
    )
}

/// Flatten the widget tree into a DFS reading order of [`AccessNode`]s,
/// collapsing presentational widgets (those returning `None` from
/// `accessibility()`) exactly as [`crate::accesskit_bridge::build_tree_update`]
/// does. The result is the order a "say all" / reading-cursor traverses.
///
/// Pure and `node_map`-free — it walks [`Widget::children`], so it is
/// unit-testable on a hand-built tree without layout.
pub fn flatten_reading_order(root: &dyn Widget) -> Vec<AccessNode> {
    let mut out = Vec::new();
    fn visit(w: &dyn Widget, out: &mut Vec<AccessNode>) {
        if let Some(node) = w.accessibility() {
            out.push(node);
        }
        for child in w.children() {
            visit(child, out);
        }
    }
    visit(root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Role};
    use tts::NullSpeaker;

    // ----- describe(): the role × state truth table -----

    fn n(role: Role) -> AccessNode {
        AccessNode::new(role)
    }

    #[test]
    fn describe_plain_button() {
        let node = n(Role::Button).with_name("Save");
        assert_eq!(describe(&node), "Save, button");
    }

    #[test]
    fn describe_unnamed_button_is_just_role() {
        let node = n(Role::Button);
        assert_eq!(describe(&node), "button");
    }

    #[test]
    fn describe_checkbox_checked_and_unchecked() {
        let checked = n(Role::Checkbox)
            .with_name("Accept")
            .with_state(AccessState::new().checked(true));
        assert_eq!(describe(&checked), "Accept, checkbox, checked");

        let unchecked = n(Role::Checkbox)
            .with_name("Accept")
            .with_state(AccessState::new().checked(false));
        assert_eq!(describe(&unchecked), "Accept, checkbox, not checked");
    }

    #[test]
    fn describe_checkbox_mixed_is_partially_checked() {
        let mut state = AccessState::new().checked(true);
        state.mixed = true;
        let node = n(Role::Checkbox).with_name("Toppings").with_state(state);
        // Mixed wins over the nominal checked flag.
        assert_eq!(describe(&node), "Toppings, checkbox, partially checked");
    }

    #[test]
    fn describe_switch_reads_on_off_not_checked() {
        let on = n(Role::Switch)
            .with_name("Wi-Fi")
            .with_state(AccessState::new().checked(true));
        assert_eq!(describe(&on), "Wi-Fi, switch, on");

        let off = n(Role::Switch)
            .with_name("Wi-Fi")
            .with_state(AccessState::new().checked(false));
        assert_eq!(describe(&off), "Wi-Fi, switch, off");
    }

    #[test]
    fn describe_toggle_button_reads_on_off() {
        let node = n(Role::ToggleButton)
            .with_name("Bold")
            .with_state(AccessState::new().checked(true));
        assert_eq!(describe(&node), "Bold, toggle button, on");
    }

    #[test]
    fn describe_slider_with_value() {
        let node = n(Role::Slider).with_name("Volume").with_value("50%");
        assert_eq!(describe(&node), "Volume, slider, 50%");
    }

    #[test]
    fn describe_text_input_with_value() {
        let node = n(Role::TextInput).with_name("Name").with_value("Ada");
        assert_eq!(describe(&node), "Name, edit text, Ada");
    }

    #[test]
    fn describe_protected_field_never_speaks_value() {
        let node = n(Role::TextInput)
            .with_name("Password")
            .with_value("hunter2")
            .with_state(AccessState::new().protected(true));
        let phrase = describe(&node);
        assert!(
            !phrase.contains("hunter2"),
            "protected value must never be spoken, got {phrase:?}"
        );
        assert_eq!(phrase, "Password, edit text, protected");
    }

    #[test]
    fn describe_multiline_text_input() {
        let node = n(Role::MultilineTextInput).with_name("Notes");
        assert_eq!(describe(&node), "Notes, multi-line edit text");
    }

    #[test]
    fn describe_disabled_checked_checkbox_full_phrase() {
        // The spec's worked example.
        let node = n(Role::Checkbox)
            .with_name("Accept")
            .with_state(AccessState::new().checked(true).disabled(true));
        assert_eq!(describe(&node), "Accept, checkbox, checked, dimmed");
    }

    #[test]
    fn describe_disabled_is_spoken_last() {
        let node = n(Role::Button)
            .with_name("Go")
            .with_state(AccessState::new().disabled(true));
        assert_eq!(describe(&node), "Go, button, dimmed");
    }

    #[test]
    fn describe_required_and_invalid() {
        let mut state = AccessState::new();
        state.required = true;
        state.invalid = true;
        let node = n(Role::TextInput).with_name("Email").with_state(state);
        assert_eq!(describe(&node), "Email, edit text, required, invalid entry");
    }

    #[test]
    fn describe_read_only_only_for_editable_roles() {
        let mut state = AccessState::new();
        state.read_only = true;
        // Editable role: read-only is spoken.
        let editable = n(Role::TextInput).with_name("Id").with_state(state);
        assert_eq!(describe(&editable), "Id, edit text, read only");
        // Static text: read-only is suppressed (noise).
        let static_text = n(Role::StaticText).with_name("Label").with_state(state);
        assert_eq!(describe(&static_text), "Label");
    }

    #[test]
    fn describe_expanded_and_collapsed() {
        let expanded = n(Role::TreeItem)
            .with_name("Folder")
            .with_state(AccessState::new().expanded(true));
        assert_eq!(describe(&expanded), "Folder, tree item, expanded");

        let collapsed = n(Role::ComboBox)
            .with_name("Country")
            .with_state(AccessState::new().expanded(false));
        assert_eq!(describe(&collapsed), "Country, combo box, collapsed");
    }

    #[test]
    fn describe_selected_list_item_and_tab() {
        let item = n(Role::ListItem)
            .with_name("Inbox")
            .with_state(AccessState::new().selected(true));
        assert_eq!(describe(&item), "Inbox, list item, selected");

        let tab = n(Role::Tab)
            .with_name("General")
            .with_state(AccessState::new().selected(true));
        assert_eq!(describe(&tab), "General, tab, selected");
    }

    #[test]
    fn describe_heading_includes_level() {
        let node = n(Role::Heading { level: 2 }).with_name("Introduction");
        assert_eq!(describe(&node), "Introduction, heading level 2");
    }

    #[test]
    fn describe_link() {
        let node = n(Role::Link).with_name("Learn more");
        assert_eq!(describe(&node), "Learn more, link");
    }

    #[test]
    fn describe_menu_item() {
        let node = n(Role::MenuItem).with_name("Open");
        assert_eq!(describe(&node), "Open, menu item");
    }

    #[test]
    fn describe_static_text_is_content_only() {
        let node = n(Role::StaticText).with_name("Hello world");
        assert_eq!(describe(&node), "Hello world");
    }

    #[test]
    fn describe_busy_progress() {
        let node = n(Role::ProgressBar)
            .with_name("Loading")
            .with_value("40%")
            .with_state(AccessState::new().busy(true));
        assert_eq!(describe(&node), "Loading, progress bar, 40%, busy");
    }

    #[test]
    fn describe_empty_generic_is_empty_string() {
        let node = n(Role::Generic);
        assert_eq!(describe(&node), "");
    }

    #[test]
    fn describe_appends_position_in_set() {
        // "Inbox, tab, selected, 2 of 4".
        let node = n(Role::Tab)
            .with_name("Inbox")
            .with_state(AccessState::new().selected(true))
            .with_position(2, 4);
        assert_eq!(describe(&node), "Inbox, tab, selected, 2 of 4");
    }

    #[test]
    fn describe_appends_level_for_tree_depth() {
        // A nested tree row reads its depth after position: "Src, tree item,
        // 3 of 7, level 2".
        let node = n(Role::TreeItem)
            .with_name("Src")
            .with_position(3, 7)
            .with_level(2);
        assert_eq!(describe(&node), "Src, tree item, 3 of 7, level 2");
    }

    #[test]
    fn describe_position_requires_both_pos_and_size() {
        // A lone pos_in_set (or set_size) with no partner is dropped — "N of M"
        // is only spoken when both are present.
        let mut node = n(Role::ListItem).with_name("Item");
        node.pos_in_set = Some(2);
        assert_eq!(describe(&node), "Item, list item");
    }

    // ----- ScreenReader narration via NullSpeaker -----

    /// Build a reader over a fresh NullSpeaker and return a handle to inspect
    /// it. Because the speaker is boxed inside the reader, we read it back via
    /// `reader.speaker()` and downcast-free string comparison through the
    /// recorded `spoken()` — but `spoken()` lives on `NullSpeaker`, so we keep a
    /// parallel raw pointer is not needed: we instead expose via `speaker()` +
    /// a small accessor. Simpler: construct the NullSpeaker, but we can't get it
    /// back out of the Box typed. So tests use a thin wrapper.
    fn null_reader() -> ScreenReader {
        ScreenReader::with_speaker(Box::new(NullSpeaker::new()))
    }

    /// Downcast-free readback: re-run the same logic the reader would and assert
    /// on the NullSpeaker we own directly. To inspect recorded utterances we
    /// instead drive a NullSpeaker we keep, via a custom speaker handle.
    ///
    /// Since `Speaker` is not downcastable here, the narration tests use a
    /// dedicated recording speaker built on `NullSpeaker` exposed through a
    /// shared `Rc<RefCell<…>>` so we can read it after moving a clone into the
    /// reader.
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct SharedRecorder(Rc<RefCell<NullSpeaker>>);

    impl Speaker for SharedRecorder {
        fn speak_utterance(&mut self, u: Utterance) {
            self.0.borrow_mut().speak_utterance(u);
        }
        fn stop(&mut self) {
            self.0.borrow_mut().stop();
        }
        fn set_rate(&mut self, r: f32) {
            self.0.borrow_mut().set_rate(r);
        }
        fn set_voice(&mut self, v: &str) {
            self.0.borrow_mut().set_voice(v);
        }
        fn is_speaking(&self) -> bool {
            self.0.borrow().is_speaking()
        }
        fn backend_name(&self) -> &'static str {
            "shared-null"
        }
    }
    // SAFETY-ish: Speaker requires Send; SharedRecorder is only used in
    // single-threaded tests. We satisfy the bound with an unsafe impl scoped to
    // the test module.
    unsafe impl Send for SharedRecorder {}

    fn recording_reader() -> (ScreenReader, SharedRecorder) {
        let rec = SharedRecorder::default();
        let reader = ScreenReader::with_speaker(Box::new(rec.clone()));
        (reader, rec)
    }

    #[test]
    fn null_reader_constructs_and_is_silent_safe() {
        // Constructing over the default speaker must never panic, even with no
        // TTS installed (default_speaker falls back to NullSpeaker).
        let mut r = ScreenReader::new();
        r.announce_focus(&n(Role::Button).with_name("Ok"));
        // No assertion on output (could be a real backend in some envs); the
        // contract under test is "does not panic".
        let _ = null_reader();
    }

    #[test]
    fn announce_focus_speaks_described_phrase() {
        let (mut r, rec) = recording_reader();
        r.announce_focus(&n(Role::Button).with_name("Save"));
        assert_eq!(rec.0.borrow().spoken(), &["Save, button".to_string()]);
    }

    #[test]
    fn announce_focus_skips_empty_phrase() {
        let (mut r, rec) = recording_reader();
        r.announce_focus(&n(Role::Generic)); // describes to ""
        assert!(rec.0.borrow().spoken().is_empty());
    }

    #[test]
    fn focus_storm_coalesces_to_latest() {
        // Rapid navigation: each focus interrupts the previous. Because the
        // reader drains immediately, every focus is recorded, BUT each
        // interrupting push first issues a stop() so a new focus replaces any
        // pending utterance. We assert both: the queue
        // never backs up (one stop per focus after the first speak) and the
        // LAST thing said is the final control.
        let (mut r, rec) = recording_reader();
        for name in ["New", "Open", "Save", "Print", "Close"] {
            r.announce_focus(&n(Role::Button).with_name(name));
        }
        let recref = rec.0.borrow();
        // The last utterance is the final focus.
        assert_eq!(recref.last(), Some("Close, button"));
        // Each focus after the first interrupted in-flight speech → a stop().
        // (5 focuses, the first has nothing to interrupt yet it still pushes an
        // interrupting utterance and stops, so 5 stops total.)
        assert!(recref.stop_count() >= 4, "interrupts should stop in-flight speech");
    }

    #[test]
    fn interrupt_replaces_pending_in_queue() {
        // Drive the queue semantics directly: two interrupting focus pushes,
        // only the latest survives in the pending queue before draining.
        let mut q = SpeechQueue::new();
        q.push(Utterance::new("First, button"));
        q.push(Utterance::new("Second, button"));
        assert_eq!(q.len(), 1, "newer focus must replace pending older focus");
        assert_eq!(q.peek().unwrap().text, "Second, button");
    }

    #[test]
    fn announce_value_change_is_spoken() {
        let (mut r, rec) = recording_reader();
        r.announce_value_change("75%");
        assert_eq!(rec.0.borrow().spoken(), &["75%".to_string()]);
    }

    #[test]
    fn echo_char_records_typed_char() {
        let (mut r, rec) = recording_reader();
        r.echo_char('a');
        r.echo_char(' ');
        let recref = rec.0.borrow();
        assert_eq!(recref.spoken(), &["a".to_string(), "space".to_string()]);
        // Echo is Text priority.
        assert_eq!(recref.utterances()[0].priority, Priority::Text);
    }

    #[test]
    fn echo_word_records_word() {
        let (mut r, rec) = recording_reader();
        r.echo_word("hello");
        assert_eq!(rec.0.borrow().spoken(), &["hello".to_string()]);
    }

    #[test]
    fn echo_deletion_speaks_char_and_deleted() {
        let (mut r, rec) = recording_reader();
        r.echo_deletion("x");
        r.echo_deletion("multiple chars");
        let recref = rec.0.borrow();
        assert_eq!(
            recref.spoken(),
            &["x deleted".to_string(), "deleted".to_string()]
        );
    }

    #[test]
    fn announce_live_region_important_survives_focus_interrupt() {
        // An Important live-region message queued, then a focus change: the
        // queue keeps the Important one (strictly higher priority survives).
        let mut q = SpeechQueue::new();
        q.push(Utterance::new("Permission denied").with_priority(Priority::Important));
        q.push(Utterance::new("Allow, button")); // Message, interrupt
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop().unwrap().text, "Permission denied");
        assert_eq!(q.pop().unwrap().text, "Allow, button");
    }

    #[test]
    fn stop_clears_and_silences() {
        let (mut r, rec) = recording_reader();
        r.announce_focus(&n(Role::Button).with_name("Go"));
        assert!(rec.0.borrow().is_speaking());
        r.stop();
        assert!(!rec.0.borrow().is_speaking());
        assert!(rec.0.borrow().stop_count() >= 1);
    }

    #[test]
    fn set_rate_and_voice_reach_speaker() {
        let (mut r, rec) = recording_reader();
        r.set_rate(1.5);
        r.set_voice("en-us");
        assert_eq!(rec.0.borrow().rate(), 1.5);
        assert_eq!(rec.0.borrow().voice(), Some("en-us"));
    }

    // ----- Reading cursor over the accessible tree -----

    // A tiny test widget mirroring the bridge's: configurable accessibility +
    // children, no layout machinery.
    use crate::theme::Theme;
    use std::collections::HashMap;
    use taffy::TaffyTree;

    struct TW {
        access: Option<AccessNode>,
        kids: Vec<Box<dyn Widget>>,
    }
    impl TW {
        fn new(access: Option<AccessNode>) -> Self {
            Self { access, kids: vec![] }
        }
        fn child(mut self, c: Box<dyn Widget>) -> Self {
            self.kids.push(c);
            self
        }
    }
    impl Widget for TW {
        fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let kids: Vec<_> = self
                .kids
                .iter_mut()
                .map(|c| c.build_layout(taffy, theme, node_map))
                .collect();
            let node = taffy.new_with_children(taffy::Style::default(), &kids).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn apply_layout(
            &mut self,
            _t: &TaffyTree<()>,
            _n: taffy::NodeId,
            _o: vello::kurbo::Point,
        ) {
        }
        fn draw(
            &mut self,
            _s: &mut vello::Scene,
            _t: vello::kurbo::Affine,
            _tr: &mut crate::text_render::TextRenderer,
            _th: &Theme,
            _p: &crate::presence::PresenceCtx,
        ) {
        }
        fn handle_event(
            &mut self,
            _e: &winit::event::WindowEvent,
            _sf: f64,
            _m: &winit::event::Modifiers,
            _s: &mut automerge::AutoCommit,
            _p: &automerge::ObjId,
            _h: &mut bool,
            _ft: &mut Option<taffy::NodeId>,
            _ni: taffy::NodeId,
            _t: &TaffyTree<()>,
        ) {
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(|c| c.as_ref()).collect()
        }
        fn name(&self) -> &str {
            "tw"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn accessibility(&self) -> Option<AccessNode> {
            self.access.clone()
        }
    }

    /// root(None) -> [ heading(H1 "Title"), group(Group "Form") -> [ name(TextInput "Name"), ok(Button "OK") ], wrapper(None) -> [ link(Link "Help") ] ]
    fn sample_tree() -> TW {
        let heading = TW::new(Some(n(Role::Heading { level: 1 }).with_name("Title")));
        let name = TW::new(Some(n(Role::TextInput).with_name("Name")));
        let ok = TW::new(Some(n(Role::Button).with_name("OK")));
        let group = TW::new(Some(n(Role::Group).with_name("Form")))
            .child(Box::new(name))
            .child(Box::new(ok));
        let link = TW::new(Some(n(Role::Link).with_name("Help")));
        let wrapper = TW::new(None).child(Box::new(link)); // presentational
        TW::new(None) // presentational root
            .child(Box::new(heading))
            .child(Box::new(group))
            .child(Box::new(wrapper))
    }

    #[test]
    fn flatten_reading_order_is_dfs_with_presentational_collapsed() {
        let root = sample_tree();
        let order = flatten_reading_order(&root);
        let roles: Vec<Role> = order.iter().map(|n| n.role).collect();
        // Presentational root + wrapper produce no node; everything else in DFS.
        assert_eq!(
            roles,
            vec![
                Role::Heading { level: 1 },
                Role::Group,
                Role::TextInput,
                Role::Button,
                Role::Link,
            ]
        );
    }

    #[test]
    fn read_next_walks_forward_in_order() {
        let root = sample_tree();
        let (mut r, rec) = recording_reader();
        // First read_next anchors at index 0 (the heading).
        let a = r.read_next(&root).unwrap();
        assert_eq!(a.role, Role::Heading { level: 1 });
        let b = r.read_next(&root).unwrap();
        assert_eq!(b.role, Role::Group);
        let c = r.read_next(&root).unwrap();
        assert_eq!(c.role, Role::TextInput);
        // Spoken phrases match describe().
        let recref = rec.0.borrow();
        assert_eq!(
            recref.spoken(),
            &[
                "Title, heading level 1".to_string(),
                "Form, group".to_string(),
                "Name, edit text".to_string(),
            ]
        );
    }

    #[test]
    fn read_next_stops_at_end() {
        let root = sample_tree();
        let mut r = recording_reader().0;
        // Walk to the very end (5 nodes), then one past.
        for _ in 0..5 {
            assert!(r.read_next(&root).is_some());
        }
        assert!(r.read_next(&root).is_none(), "cursor must not run off the end");
        assert_eq!(r.cursor(), Some(4));
    }

    #[test]
    fn read_prev_walks_backward() {
        let root = sample_tree();
        let mut r = recording_reader().0;
        // Advance two, then step back one.
        r.read_next(&root); // Heading (0)
        r.read_next(&root); // Group (1)
        let back = r.read_prev(&root).unwrap();
        assert_eq!(back.role, Role::Heading { level: 1 });
        assert_eq!(r.cursor(), Some(0));
        // One more prev stops at the start.
        assert!(r.read_prev(&root).is_none());
    }

    #[test]
    fn read_prev_from_unset_starts_at_last() {
        let root = sample_tree();
        let mut r = recording_reader().0;
        let last = r.read_prev(&root).unwrap();
        assert_eq!(last.role, Role::Link);
        assert_eq!(r.cursor(), Some(4));
    }

    #[test]
    fn read_all_speaks_every_node_then_seeds_cursor() {
        let root = sample_tree();
        let (mut r, rec) = recording_reader();
        r.read_all(&root);
        let recref = rec.0.borrow();
        assert_eq!(
            recref.spoken(),
            &[
                "Title, heading level 1".to_string(),
                "Form, group".to_string(),
                "Name, edit text".to_string(),
                "OK, button".to_string(),
                "Help, link".to_string(),
            ]
        );
        drop(recref);
        assert_eq!(r.cursor(), Some(0));
        // First utterance interrupts, the rest are continuations.
        let recref = rec.0.borrow();
        assert!(recref.utterances()[0].interrupt);
        assert!(!recref.utterances()[1].interrupt);
    }

    #[test]
    fn announce_focus_resets_reading_cursor() {
        let root = sample_tree();
        let mut r = recording_reader().0;
        r.read_next(&root);
        r.read_next(&root);
        assert_eq!(r.cursor(), Some(1));
        r.announce_focus(&n(Role::Button).with_name("Elsewhere"));
        assert_eq!(r.cursor(), None, "focus change re-anchors the reading cursor");
    }

    #[test]
    fn seed_reading_order_counts_accessible_nodes() {
        let root = sample_tree();
        let mut r = recording_reader().0;
        assert_eq!(r.seed_reading_order(&root), 5);
    }

    // ----- Quick navigation: jump by element type -----

    /// root(None) -> [ H1 "Top", para "intro", H2 "Body", button "Go",
    ///                 text "Name", H2 "Footer" ]
    fn quicknav_tree() -> TW {
        TW::new(None)
            .child(Box::new(TW::new(Some(
                n(Role::Heading { level: 1 }).with_name("Top"),
            ))))
            .child(Box::new(TW::new(Some(
                n(Role::Paragraph).with_name("intro"),
            ))))
            .child(Box::new(TW::new(Some(
                n(Role::Heading { level: 2 }).with_name("Body"),
            ))))
            .child(Box::new(TW::new(Some(n(Role::Button).with_name("Go")))))
            .child(Box::new(TW::new(Some(
                n(Role::TextInput).with_name("Name"),
            ))))
            .child(Box::new(TW::new(Some(
                n(Role::Heading { level: 2 }).with_name("Footer"),
            ))))
    }

    #[test]
    fn read_next_heading_lands_on_each_heading() {
        let root = quicknav_tree();
        let mut r = recording_reader().0;
        // From an unset cursor, jump to the first heading.
        let a = r.read_next_heading(&root).unwrap();
        assert_eq!(a.role, Role::Heading { level: 1 });
        assert_eq!(a.name.as_deref(), Some("Top"));
        // Next skips the paragraph and the controls, landing on the next H2.
        let b = r.read_next_heading(&root).unwrap();
        assert_eq!(b.role, Role::Heading { level: 2 });
        assert_eq!(b.name.as_deref(), Some("Body"));
        let c = r.read_next_heading(&root).unwrap();
        assert_eq!(c.name.as_deref(), Some("Footer"));
        // No more headings ahead.
        assert!(r.read_next_heading(&root).is_none());
    }

    #[test]
    fn read_prev_heading_walks_backward() {
        let root = quicknav_tree();
        let mut r = recording_reader().0;
        // Unset cursor → previous-heading starts from the end (last heading).
        let last = r.read_prev_heading(&root).unwrap();
        assert_eq!(last.name.as_deref(), Some("Footer"));
        let mid = r.read_prev_heading(&root).unwrap();
        assert_eq!(mid.name.as_deref(), Some("Body"));
        let first = r.read_prev_heading(&root).unwrap();
        assert_eq!(first.name.as_deref(), Some("Top"));
        assert!(r.read_prev_heading(&root).is_none());
    }

    #[test]
    fn read_next_control_skips_non_interactive() {
        let root = quicknav_tree();
        let mut r = recording_reader().0;
        // First control is the button (headings/paragraph are skipped).
        let a = r.read_next_control(&root).unwrap();
        assert_eq!(a.role, Role::Button);
        assert_eq!(a.name.as_deref(), Some("Go"));
        // Next control is the text input.
        let b = r.read_next_control(&root).unwrap();
        assert_eq!(b.role, Role::TextInput);
        // No further controls.
        assert!(r.read_next_control(&root).is_none());
    }

    #[test]
    fn read_prev_control_from_end_walks_backward() {
        let root = quicknav_tree();
        let mut r = recording_reader().0;
        let last = r.read_prev_control(&root).unwrap();
        assert_eq!(last.role, Role::TextInput);
        let prev = r.read_prev_control(&root).unwrap();
        assert_eq!(prev.role, Role::Button);
        assert!(r.read_prev_control(&root).is_none());
    }

    #[test]
    fn quick_nav_speaks_a_no_more_cue_when_nothing_matches() {
        // A tree with no headings: read_next_heading speaks the "no more" cue.
        let root = TW::new(None).child(Box::new(TW::new(Some(n(Role::Button).with_name("Solo")))));
        let (mut r, rec) = recording_reader();
        assert!(r.read_next_heading(&root).is_none());
        assert_eq!(rec.0.borrow().last(), Some(phrase::no_next().as_str()));
    }

    #[test]
    fn is_heading_and_is_form_field_predicates() {
        assert!(is_heading(&n(Role::Heading { level: 1 })));
        assert!(!is_heading(&n(Role::Button)));
        for role in [
            Role::Button,
            Role::Checkbox,
            Role::Switch,
            Role::Slider,
            Role::ComboBox,
            Role::TextInput,
            Role::Link,
        ] {
            assert!(is_form_field(&n(role)), "{role:?} should be a form field");
        }
        assert!(!is_form_field(&n(Role::StaticText)));
        assert!(!is_form_field(&n(Role::Group)));
    }
}
