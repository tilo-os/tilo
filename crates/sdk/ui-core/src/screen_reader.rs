//! Built-in screen reader.
//!
//! Narrates the semantic tree ([`crate::a11y::AccessNode`]) through an
//! offline TTS stack. Focus changes interrupt the previous utterance;
//! everything funnels through one [`SpeechQueue`], so a
//! focus storm coalesces to the latest utterance instead of backing up.
//!
//! [`describe`] is the pure core (`AccessNode -> String`, no I/O) —
//! exhaustively unit-tested below as a role x state truth table.
//!
//! A reading cursor can also walk the flattened accessible tree (DFS,
//! presentational nodes collapsed) via [`ScreenReader::read_all`] /
//! `read_next` / `read_prev`.
//!
//! Narration vocabulary resolves through [`i18n!`](macro@crate::i18n) bundles;
//! with none loaded it falls back to English so headless builds narrate
//! unchanged.

use tts::{NullSpeaker, Priority, Speaker, SpeechQueue, Utterance};

use crate::a11y::{AccessNode, Role};
use crate::widget::Widget;

/// Centralized narration vocabulary: every word has a stable FTL key plus a
/// clear English fallback.
mod phrase {
    use crate::a11y::Role;

    /// `I18nManager::lookup` returns the key verbatim for an unknown
    /// message — that's the "nothing loaded" signal to substitute English.
    pub fn localized(key: &str, english: &str) -> String {
        let resolved = crate::i18n!(key);
        if resolved == key {
            english.to_string()
        } else {
            resolved
        }
    }

    /// Returns `""` for roles that read better with no role word at all
    /// (a screen reader saying "Hello, static text" is noise).
    pub fn role_word(role: Role) -> String {
        let (key, english) = match role {
            Role::Window => ("sr-role-window", "window"),
            Role::Dialog => ("sr-role-dialog", "dialog"),
            Role::Group => ("sr-role-group", "group"),
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
            Role::Separator => ("sr-role-separator", "separator"),
            Role::SpinButton => ("sr-role-spin-button", "spin button"),
            Role::ComboBox => ("sr-role-combo-box", "combo box"),
            Role::TextInput => ("sr-role-text-input", "edit text"),
            Role::MultilineTextInput => ("sr-role-multiline-text-input", "multi-line edit text"),
            Role::Link => ("sr-role-link", "link"),
            Role::ProgressBar => ("sr-role-progress-bar", "progress bar"),
            Role::Spinner => ("sr-role-spinner", "busy indicator"),
            Role::Tooltip => ("sr-role-tooltip", "tooltip"),

            Role::Image => ("sr-role-image", "image"),
            // Heading word is produced specially (carries a level); see `describe`.
            Role::Generic
            | Role::Label
            | Role::StaticText
            | Role::Paragraph
            | Role::Heading { .. }
            | Role::Unknown => ("", ""),
            Role::Document => ("sr-role-document", "document"),
            Role::Terminal => ("sr-role-terminal", "terminal"),
            Role::Canvas => ("sr-role-canvas", "canvas"),
        };
        if key.is_empty() {
            String::new()
        } else {
            localized(key, english)
        }
    }

    pub fn checked() -> String {
        localized("sr-state-checked", "checked")
    }
    pub fn not_checked() -> String {
        localized("sr-state-not-checked", "not checked")
    }
    pub fn partially_checked() -> String {
        localized("sr-state-partially-checked", "partially checked")
    }
    /// Reads more naturally than "checked" for a toggle/switch.
    pub fn on() -> String {
        localized("sr-state-on", "on")
    }
    pub fn off() -> String {
        localized("sr-state-off", "off")
    }
    pub fn selected() -> String {
        localized("sr-state-selected", "selected")
    }
    pub fn expanded() -> String {
        localized("sr-state-expanded", "expanded")
    }
    pub fn collapsed() -> String {
        localized("sr-state-collapsed", "collapsed")
    }
    /// The long-standing screen-reader word for disabled (Orca/VoiceOver
    /// convention) — clearer than "disabled" when spoken.
    pub fn dimmed() -> String {
        localized("sr-state-dimmed", "dimmed")
    }
    pub fn read_only() -> String {
        localized("sr-state-read-only", "read only")
    }
    pub fn required() -> String {
        localized("sr-state-required", "required")
    }
    pub fn invalid() -> String {
        localized("sr-state-invalid", "invalid entry")
    }
    pub fn busy() -> String {
        localized("sr-state-busy", "busy")
    }
    /// A protected (password) field — the value is never spoken.
    pub fn protected() -> String {
        localized("sr-state-protected", "protected")
    }

    /// "heading level N".
    pub fn heading_word(level: u8) -> String {
        let resolved = crate::i18n::I18nManager::format(
            "sr-heading-level",
            &[("level", f64::from(level).into())],
        );
        if resolved == "sr-heading-level" {
            format!("heading level {level}")
        } else {
            resolved
        }
    }

    /// "N of M" — an item's position within its collection.
    pub fn position_word(pos: usize, size: usize) -> String {
        #[allow(
            clippy::cast_precision_loss,
            reason = "pos/size are a list position and length, always far below where \
                      f64 loses integer precision"
        )]
        let (pos_f, size_f) = (pos as f64, size as f64);
        let resolved = crate::i18n::I18nManager::format(
            "sr-position",
            &[("pos", pos_f.into()), ("size", size_f.into())],
        );
        if resolved == "sr-position" {
            format!("{pos} of {size}")
        } else {
            resolved
        }
    }

    /// "level N" — hierarchy depth for nested structures (tree rows).
    pub fn level_word(level: usize) -> String {
        #[allow(
            clippy::cast_precision_loss,
            reason = "level is a hierarchy depth, always far below where f64 loses \
                      integer precision"
        )]
        let level_f = level as f64;
        let resolved = crate::i18n::I18nManager::format("sr-level", &[("level", level_f.into())]);
        if resolved == "sr-level" {
            format!("level {level}")
        } else {
            resolved
        }
    }

    pub fn deleted() -> String {
        localized("sr-deleted", "deleted")
    }
    pub fn no_next() -> String {
        localized("sr-no-next", "no more")
    }
    pub fn no_prev() -> String {
        localized("sr-no-prev", "no previous")
    }
}

/// Compose the spoken phrase for an accessible node — the pure, heavily
/// unit-tested core of the screen reader.
///
/// Segments join in order: name, value ([`protected`](crate::a11y::AccessState::protected)
/// contributes "protected" instead), role word, states (dimmed last), then
/// position-in-set and hierarchy level. Empty segments are dropped.
///
/// ```
/// use tilo_ui::a11y::{AccessNode, AccessState, Role};
/// use tilo_ui::screen_reader::describe;
///
/// let node = AccessNode::new(Role::Checkbox)
///     .with_name("Accept")
///     .with_state(AccessState::new().checked(true).disabled(true));
/// assert_eq!(describe(&node), "Accept, checkbox, checked, dimmed");
/// ```
pub fn describe(node: &AccessNode) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(name) = node.name.as_deref()
        && !name.is_empty()
    {
        parts.push(name.to_string());
    }

    match node.role {
        Role::Heading { level } => parts.push(phrase::heading_word(level)),
        other => {
            let w = phrase::role_word(other);
            if !w.is_empty() {
                parts.push(w);
            }
        }
    }

    if node.state.protected {
        parts.push(phrase::protected());
    } else if let Some(value) = node.value.as_deref()
        && !value.is_empty()
    {
        parts.push(value.to_string());
    }

    push_state_words(node, &mut parts);

    if let (Some(pos), Some(size)) = (node.pos_in_set, node.set_size) {
        parts.push(phrase::position_word(pos, size));
    }
    if let Some(level) = node.level {
        parts.push(phrase::level_word(level));
    }

    parts.join(", ")
}

/// Value + state, without name or role — confirms the result of
/// activating or adjusting a control (e.g. "checked") without
/// re-reading the whole label.
pub fn describe_state(node: &AccessNode) -> String {
    let mut parts: Vec<String> = Vec::new();
    if node.state.protected {
        parts.push(phrase::protected());
    } else if let Some(value) = node.value.as_deref()
        && !value.is_empty()
    {
        parts.push(value.to_string());
    }
    push_state_words(node, &mut parts);
    parts.join(", ")
}

/// Switches / toggle buttons read on/off; checkboxes / radios read
/// checked / not checked. A `mixed` (indeterminate) checkbox reads
/// "partially checked" regardless of the nominal `checked` flag.
fn push_state_words(node: &AccessNode, parts: &mut Vec<String>) {
    let s = &node.state;
    let switchy = matches!(node.role, Role::Switch | Role::ToggleButton);

    if s.mixed {
        parts.push(phrase::partially_checked());
    } else if let Some(checked) = s.checked {
        if switchy {
            parts.push(if checked { phrase::on() } else { phrase::off() });
        } else {
            parts.push(if checked {
                phrase::checked()
            } else {
                phrase::not_checked()
            });
        }
    }

    if s.selected {
        parts.push(phrase::selected());
    }

    if let Some(expanded) = s.expanded {
        parts.push(if expanded {
            phrase::expanded()
        } else {
            phrase::collapsed()
        });
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

pub struct ScreenReader {
    speaker: Box<dyn Speaker>,
    queue: SpeechQueue,
    /// `None` until the first `read_next/read_prev/read_all`.
    cursor: Option<usize>,
    /// The flattened reading order captured by the most recent reading command.
    reading_order: Vec<AccessNode>,
}

impl Default for ScreenReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenReader {
    /// Speaks through the current host's [`crate::host::SpeechFactory`].
    /// Never panics: with no speech available this is a
    /// [`NullSpeaker`] and narration is silently dropped.
    pub fn new() -> Self {
        Self::with_speaker(crate::host::current().speech.make())
    }

    pub fn with_speaker(speaker: Box<dyn Speaker>) -> Self {
        Self {
            speaker,
            queue: SpeechQueue::new(),
            cursor: None,
            reading_order: Vec::new(),
        }
    }

    pub fn null() -> Self {
        Self::with_speaker(Box::new(NullSpeaker::new()))
    }

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

    /// Speak at the rate and in the voice the person chose in Settings.
    pub fn configure(&mut self, settings: &settings_store::screen_reader::ScreenReaderSettings) {
        self.set_rate(settings.rate);
        if let Some(voice) = settings.voice.engine_voice() {
            self.set_voice(voice);
        }
    }

    /// The single funnel every utterance goes through. An interrupting push
    /// first stops in-flight speech, then drains the coalesced queue into
    /// the speaker — immediately, which keeps the `NullSpeaker` test path
    /// simple and is also correct for the daemon backend (it serializes
    /// server-side).
    fn enqueue(&mut self, utterance: Utterance) {
        let interrupted = self.queue.push(utterance);
        if interrupted {
            self.speaker.stop();
        }
        self.queue.drive(self.speaker.as_mut());
    }

    /// Speaks [`describe`] of the newly-focused node, interrupting any
    /// previous narration — rapid arrow navigation speaks only the control
    /// landed on, never a backlog (see `focus_storm` test). Resets the
    /// reading cursor so the next `read_next/read_prev` re-anchors here.
    pub fn announce_focus(&mut self, node: &AccessNode) {
        let text = describe(node);
        if text.is_empty() {
            return;
        }
        self.enqueue(Utterance::new(text));
        self.cursor = None;
    }

    /// The live-region entry point: errors/permission prompts use
    /// [`Priority::Important`](tts::Priority) (survives a focus interrupt);
    /// polite toasts use [`Priority::Notification`](tts::Priority).
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

    /// Interrupting — dragging a slider should track the latest value,
    /// not queue every intermediate one.
    pub fn announce_value_change(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.enqueue(Utterance::new(value));
    }

    /// Spoken at [`Priority::Text`](tts::Priority), interrupting, so a
    /// fast typist's echoes coalesce.
    pub fn echo_char(&mut self, ch: char) {
        // A space is announced as the word so it isn't silence.
        let text = if ch == ' ' {
            "space".to_string()
        } else {
            ch.to_string()
        };
        self.enqueue(Utterance::new(text).with_priority(Priority::Text));
    }

    pub fn echo_word(&mut self, word: &str) {
        if word.is_empty() {
            return;
        }
        self.enqueue(Utterance::new(word.to_string()).with_priority(Priority::Text));
    }

    /// A single deleted character is spoken as "X deleted"; otherwise
    /// just "deleted".
    pub fn echo_deletion(&mut self, deleted: &str) {
        let text = match deleted.chars().count() {
            1 => format!("{deleted} {}", phrase::deleted()),
            _ => phrase::deleted(),
        };
        self.enqueue(Utterance::new(text).with_priority(Priority::Text));
    }

    /// The "silence key" (Orca convention): stop all speech and discard
    /// anything pending.
    pub fn stop(&mut self) {
        self.queue.clear();
        self.speaker.stop();
    }

    pub fn is_speaking(&self) -> bool {
        self.speaker.is_speaking()
    }

    /// A "say all": speaks every node's [`describe`] as bulk
    /// [`Priority::Text`](tts::Priority) (so a later focus change
    /// interrupts it). Seeds the reading cursor at the start.
    pub fn read_all(&mut self, root: &dyn Widget) {
        self.reading_order = flatten_reading_order(root);
        self.cursor = if self.reading_order.is_empty() {
            None
        } else {
            Some(0)
        };
        // First utterance interrupts (clears focus chatter); the rest are
        // non-interrupting continuations so they don't cancel each other.
        let mut first = true;
        // Collected first to avoid borrowing `self` across the speaker call.
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

    /// Refreshes the reading order without moving the cursor.
    pub fn seed_reading_order(&mut self, root: &dyn Widget) -> usize {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
        }
        self.reading_order.len()
    }

    /// Returns `None` (speaking nothing) at the end of the document.
    pub fn read_next(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(i) => {
                if i.saturating_add(1) >= self.reading_order.len() {
                    return None;
                }
                i.saturating_add(1)
            }
        };
        self.cursor = Some(next);
        #[allow(
            clippy::indexing_slicing,
            reason = "next is freshly bounds-checked against self.reading_order.len() \
                      immediately above"
        )]
        let node = self.reading_order[next].clone();
        let text = describe(&node);
        if !text.is_empty() {
            self.enqueue(Utterance::new(text));
        }
        Some(node)
    }

    /// Returns `None` at the start of the document.
    pub fn read_prev(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.reading_order = flatten_reading_order(root);
        if self.reading_order.is_empty() {
            self.cursor = None;
            return None;
        }
        let prev = match self.cursor {
            // `self.reading_order` is non-empty here (checked above), so len() >= 1.
            None => self.reading_order.len().saturating_sub(1),
            Some(0) => return None,
            // The `Some(0)` arm above already took the i == 0 case.
            Some(i) => i.saturating_sub(1),
        };
        self.cursor = Some(prev);
        #[allow(
            clippy::indexing_slicing,
            reason = "prev is freshly bounds-checked against self.reading_order.len() \
                      immediately above"
        )]
        let node = self.reading_order[prev].clone();
        let text = describe(&node);
        if !text.is_empty() {
            self.enqueue(Utterance::new(text));
        }
        Some(node)
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Speaks a short "no more" cue and returns `None` when nothing ahead
    /// matches `pred`. Shared engine behind `read_next_heading/read_next_control`.
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
        let start = self.cursor.map_or(0, |i| i.saturating_add(1));
        #[allow(
            clippy::indexing_slicing,
            reason = "i is drawn from the start..len() range, so it always indexes \
                      within self.reading_order"
        )]
        let found = (start..self.reading_order.len()).find(|&i| pred(&self.reading_order[i]));
        if let Some(i) = found {
            self.cursor = Some(i);
            #[allow(
                clippy::indexing_slicing,
                reason = "i is the index `find` just matched inside self.reading_order"
            )]
            let node = self.reading_order[i].clone();
            let text = describe(&node);
            if !text.is_empty() {
                self.enqueue(Utterance::new(text));
            }
            Some(node)
        } else {
            self.enqueue(Utterance::new(phrase::no_next()));
            None
        }
    }

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
        #[allow(
            clippy::indexing_slicing,
            reason = "i is drawn from the 0..end range, so it always indexes within \
                      self.reading_order"
        )]
        let found = (0..end).rev().find(|&i| pred(&self.reading_order[i]));
        if let Some(i) = found {
            self.cursor = Some(i);
            #[allow(
                clippy::indexing_slicing,
                reason = "i is the index `find` just matched inside self.reading_order"
            )]
            let node = self.reading_order[i].clone();
            let text = describe(&node);
            if !text.is_empty() {
                self.enqueue(Utterance::new(text));
            }
            Some(node)
        } else {
            self.enqueue(Utterance::new(phrase::no_prev()));
            None
        }
    }

    /// Jump to the next heading and speak it ("Insert + H").
    pub fn read_next_heading(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_next_matching(root, is_heading)
    }

    /// Jump to the previous heading and speak it ("Insert + Shift + H").
    pub fn read_prev_heading(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_prev_matching(root, is_heading)
    }

    /// Jump to the next form field / interactive control and speak it
    /// ("Insert + F").
    pub fn read_next_control(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_next_matching(root, is_form_field)
    }

    /// Jump to the previous form field / interactive control and speak it
    /// ("Insert + Shift + F").
    pub fn read_prev_control(&mut self, root: &dyn Widget) -> Option<AccessNode> {
        self.read_prev_matching(root, is_form_field)
    }
}

pub fn is_heading(node: &AccessNode) -> bool {
    matches!(node.role, Role::Heading { .. })
}

/// Deliberately covers the editable + actionable controls (buttons,
/// toggles, inputs, ranges, selectors) but not structural containers or
/// static content.
pub fn is_form_field(node: &AccessNode) -> bool {
    matches!(
        node.role,
        Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::Separator
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
    )
}

/// DFS, collapsing presentational widgets (`accessibility() == None`)
/// exactly as [`crate::accesskit_bridge::build_tree_update`] does.
pub fn flatten_reading_order(root: &dyn Widget) -> Vec<AccessNode> {
    fn visit(w: &dyn Widget, out: &mut Vec<AccessNode>) {
        if let Some(node) = w.accessibility() {
            out.push(node);
        }
        for child in w.children() {
            visit(child, out);
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessNode, AccessState, Role};
    use crate::widget::TaffyTreeExt;
    use tts::NullSpeaker;

    fn n(role: Role) -> AccessNode {
        AccessNode::new(role)
    }

    #[test]
    fn describe_speaks_role_name_value_and_content() {
        // Case `describe_plain_button`.
        {
            let node = n(Role::Button).with_name("Save");
            assert_eq!(describe(&node), "Save, button");
        }

        // Case `describe_unnamed_button_is_just_role`.
        {
            let node = n(Role::Button);
            assert_eq!(describe(&node), "button");
        }

        // Case `describe_slider_with_value`.
        {
            let node = n(Role::Slider).with_name("Volume").with_value("50%");
            assert_eq!(describe(&node), "Volume, slider, 50%");
        }

        // Case `describe_text_input_with_value`.
        {
            let node = n(Role::TextInput).with_name("Name").with_value("Ada");
            assert_eq!(describe(&node), "Name, edit text, Ada");
        }

        // Case `describe_multiline_text_input`.
        {
            let node = n(Role::MultilineTextInput).with_name("Notes");
            assert_eq!(describe(&node), "Notes, multi-line edit text");
        }

        // Case `describe_heading_includes_level`.
        {
            let node = n(Role::Heading { level: 2 }).with_name("Introduction");
            assert_eq!(describe(&node), "Introduction, heading level 2");
        }

        // Case `describe_link`.
        {
            let node = n(Role::Link).with_name("Learn more");
            assert_eq!(describe(&node), "Learn more, link");
        }

        // Case `describe_menu_item`.
        {
            let node = n(Role::MenuItem).with_name("Open");
            assert_eq!(describe(&node), "Open, menu item");
        }

        // Case `describe_static_text_is_content_only`.
        {
            let node = n(Role::StaticText).with_name("Hello world");
            assert_eq!(describe(&node), "Hello world");
        }

        // Case `describe_empty_generic_is_empty_string`.
        {
            let node = n(Role::Generic);
            assert_eq!(describe(&node), "");
        }

        // Case `describe_busy_progress`.
        {
            let node = n(Role::ProgressBar)
                .with_name("Loading")
                .with_value("40%")
                .with_state(AccessState::new().busy(true));
            assert_eq!(describe(&node), "Loading, progress bar, 40%, busy");
        }
    }

    #[test]
    fn describe_speaks_checked_toggled_expanded_and_selected_states() {
        // Case `describe_checkbox_checked_and_unchecked`.
        {
            let checked = n(Role::Checkbox)
                .with_name("Accept")
                .with_state(AccessState::new().checked(true));
            assert_eq!(describe(&checked), "Accept, checkbox, checked");

            let unchecked = n(Role::Checkbox)
                .with_name("Accept")
                .with_state(AccessState::new().checked(false));
            assert_eq!(describe(&unchecked), "Accept, checkbox, not checked");
        }

        // Case `describe_checkbox_mixed_is_partially_checked`.
        {
            let mut state = AccessState::new().checked(true);
            state.mixed = true;
            let node = n(Role::Checkbox).with_name("Toppings").with_state(state);
            assert_eq!(describe(&node), "Toppings, checkbox, partially checked");
        }

        // Case `describe_switch_reads_on_off_not_checked`.
        {
            let on = n(Role::Switch)
                .with_name("Wi-Fi")
                .with_state(AccessState::new().checked(true));
            assert_eq!(describe(&on), "Wi-Fi, switch, on");

            let off = n(Role::Switch)
                .with_name("Wi-Fi")
                .with_state(AccessState::new().checked(false));
            assert_eq!(describe(&off), "Wi-Fi, switch, off");
        }

        // Case `describe_toggle_button_reads_on_off`.
        {
            let node = n(Role::ToggleButton)
                .with_name("Bold")
                .with_state(AccessState::new().checked(true));
            assert_eq!(describe(&node), "Bold, toggle button, on");
        }

        // Case `describe_expanded_and_collapsed`.
        {
            let expanded = n(Role::TreeItem)
                .with_name("Folder")
                .with_state(AccessState::new().expanded(true));
            assert_eq!(describe(&expanded), "Folder, tree item, expanded");

            let collapsed = n(Role::ComboBox)
                .with_name("Country")
                .with_state(AccessState::new().expanded(false));
            assert_eq!(describe(&collapsed), "Country, combo box, collapsed");
        }

        // Case `describe_selected_list_item_and_tab`.
        {
            let item = n(Role::ListItem)
                .with_name("Inbox")
                .with_state(AccessState::new().selected(true));
            assert_eq!(describe(&item), "Inbox, list item, selected");

            let tab = n(Role::Tab)
                .with_name("General")
                .with_state(AccessState::new().selected(true));
            assert_eq!(describe(&tab), "General, tab, selected");
        }
    }

    #[test]
    fn describe_orders_qualifiers_and_never_speaks_protected_values() {
        // Case `describe_protected_field_never_speaks_value`.
        {
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

        // Case `describe_disabled_checked_checkbox_full_phrase`.
        {
            let node = n(Role::Checkbox)
                .with_name("Accept")
                .with_state(AccessState::new().checked(true).disabled(true));
            assert_eq!(describe(&node), "Accept, checkbox, checked, dimmed");
        }

        // Case `describe_disabled_is_spoken_last`.
        {
            let node = n(Role::Button)
                .with_name("Go")
                .with_state(AccessState::new().disabled(true));
            assert_eq!(describe(&node), "Go, button, dimmed");
        }

        // Case `describe_required_and_invalid`.
        {
            let mut state = AccessState::new();
            state.required = true;
            state.invalid = true;
            let node = n(Role::TextInput).with_name("Email").with_state(state);
            assert_eq!(describe(&node), "Email, edit text, required, invalid entry");
        }

        // Case `describe_read_only_only_for_editable_roles`.
        {
            let mut state = AccessState::new();
            state.read_only = true;
            let editable = n(Role::TextInput).with_name("Id").with_state(state);
            assert_eq!(describe(&editable), "Id, edit text, read only");
            let static_text = n(Role::StaticText).with_name("Label").with_state(state);
            assert_eq!(describe(&static_text), "Label");
        }

        // Case `describe_appends_position_in_set`.
        {
            let node = n(Role::Tab)
                .with_name("Inbox")
                .with_state(AccessState::new().selected(true))
                .with_position(2, 4);
            assert_eq!(describe(&node), "Inbox, tab, selected, 2 of 4");
        }

        // Case `describe_appends_level_for_tree_depth`.
        {
            let node = n(Role::TreeItem)
                .with_name("Src")
                .with_position(3, 7)
                .with_level(2);
            assert_eq!(describe(&node), "Src, tree item, 3 of 7, level 2");
        }

        // Case `describe_position_requires_both_pos_and_size`.
        {
            // "N of M" is only spoken when both are present.
            let mut node = n(Role::ListItem).with_name("Item");
            node.pos_in_set = Some(2);
            assert_eq!(describe(&node), "Item, list item");
        }
    }

    fn null_reader() -> ScreenReader {
        ScreenReader::with_speaker(Box::new(NullSpeaker::new()))
    }

    /// `Speaker` isn't downcastable, so recorded utterances go through a
    /// `NullSpeaker` shared via `Rc<RefCell<_>>`: a clone goes into the
    /// reader, the original stays here to assert on.
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
    #[allow(
        clippy::non_send_fields_in_send_ty,
        reason = "the Rc<RefCell<_>> is deliberately asserted Send above — this test \
                  double never actually crosses a thread"
    )]
    unsafe impl Send for SharedRecorder {}

    fn recording_reader() -> (ScreenReader, SharedRecorder) {
        let rec = SharedRecorder::default();
        let reader = ScreenReader::with_speaker(Box::new(rec.clone()));
        (reader, rec)
    }

    #[test]
    fn announcements_coalesce_interrupt_and_reach_the_speaker() {
        // Case `null_reader_constructs_and_is_silent_safe`.
        {
            let mut r = ScreenReader::new();
            r.announce_focus(&n(Role::Button).with_name("Ok"));
            // Contract under test is "does not panic" (output may be a real backend).
            let _ = null_reader();
        }

        // Case `announce_focus_speaks_described_phrase`.
        {
            let (mut r, rec) = recording_reader();
            r.announce_focus(&n(Role::Button).with_name("Save"));
            assert_eq!(rec.0.borrow().spoken(), &["Save, button".to_string()]);
        }

        // Case `announce_focus_skips_empty_phrase`.
        {
            let (mut r, rec) = recording_reader();
            r.announce_focus(&n(Role::Generic)); // describes to ""
            assert!(rec.0.borrow().spoken().is_empty());
        }

        // Case `focus_storm_coalesces_to_latest`.
        {
            // Drains immediately, so every focus is recorded, but each
            // interrupting push first issues a stop().
            let (mut r, rec) = recording_reader();
            for name in ["New", "Open", "Save", "Print", "Close"] {
                r.announce_focus(&n(Role::Button).with_name(name));
            }
            let recref = rec.0.borrow();
            assert_eq!(recref.last(), Some("Close, button"));
            assert!(
                recref.stop_count() >= 4,
                "interrupts should stop in-flight speech"
            );
        }

        // Case `interrupt_replaces_pending_in_queue`.
        {
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("First, button"));
            q.push(Utterance::new("Second, button"));
            assert_eq!(q.len(), 1, "newer focus must replace pending older focus");
            assert_eq!(q.peek().unwrap().text, "Second, button");
        }

        // Case `announce_value_change_is_spoken`.
        {
            let (mut r, rec) = recording_reader();
            r.announce_value_change("75%");
            assert_eq!(rec.0.borrow().spoken(), &["75%".to_string()]);
        }

        // Case `announce_live_region_important_survives_focus_interrupt`.
        {
            // Strictly higher priority survives a focus interrupt.
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("Permission denied").with_priority(Priority::Important));
            q.push(Utterance::new("Allow, button")); // Message, interrupt
            assert_eq!(q.len(), 2);
            assert_eq!(q.pop().unwrap().text, "Permission denied");
            assert_eq!(q.pop().unwrap().text, "Allow, button");
        }

        // Case `stop_clears_and_silences`.
        {
            let (mut r, rec) = recording_reader();
            r.announce_focus(&n(Role::Button).with_name("Go"));
            assert!(rec.0.borrow().is_speaking());
            r.stop();
            assert!(!rec.0.borrow().is_speaking());
            assert!(rec.0.borrow().stop_count() >= 1);
        }

        // Case `set_rate_and_voice_reach_speaker`.
        {
            let (mut r, rec) = recording_reader();
            r.set_rate(1.5);
            r.set_voice("en-us");
            assert_eq!(rec.0.borrow().rate(), 1.5);
            assert_eq!(rec.0.borrow().voice(), Some("en-us"));
        }
    }

    #[test]
    fn typing_echo_speaks_chars_words_and_deletions() {
        // Case `echo_char_records_typed_char`.
        {
            let (mut r, rec) = recording_reader();
            r.echo_char('a');
            r.echo_char(' ');
            let recref = rec.0.borrow();
            assert_eq!(recref.spoken(), &["a".to_string(), "space".to_string()]);
            // Echo is Text priority.
            assert_eq!(recref.utterances()[0].priority, Priority::Text);
        }

        // Case `echo_word_records_word`.
        {
            let (mut r, rec) = recording_reader();
            r.echo_word("hello");
            assert_eq!(rec.0.borrow().spoken(), &["hello".to_string()]);
        }

        // Case `echo_deletion_speaks_char_and_deleted`.
        {
            let (mut r, rec) = recording_reader();
            r.echo_deletion("x");
            r.echo_deletion("multiple chars");
            let recref = rec.0.borrow();
            assert_eq!(
                recref.spoken(),
                &["x deleted".to_string(), "deleted".to_string()]
            );
        }
    }

    // Configurable accessibility + children, no layout machinery.
    use crate::theme::Theme;
    use std::collections::HashMap;
    use taffy::TaffyTree;

    struct TW {
        access: Option<AccessNode>,
        kids: Vec<Box<dyn Widget>>,
    }
    impl TW {
        fn new(access: Option<AccessNode>) -> Self {
            Self {
                access,
                kids: vec![],
            }
        }
        fn child(mut self, c: Box<dyn Widget>) -> Self {
            self.kids.push(c);
            self
        }
    }
    impl Widget for TW {
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
            taffy.widget_node(taffy::Style::default(), &kids, node_map, self)
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.kids.iter().map(Box::as_ref).collect()
        }
        fn name(&self) -> &'static str {
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
    fn reading_cursor_walks_the_flattened_order() {
        // Case `flatten_reading_order_is_dfs_with_presentational_collapsed`.
        {
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

        // Case `read_next_walks_forward_in_order`.
        {
            let root = sample_tree();
            let (mut r, rec) = recording_reader();
            let a = r.read_next(&root).unwrap();
            assert_eq!(a.role, Role::Heading { level: 1 });
            let b = r.read_next(&root).unwrap();
            assert_eq!(b.role, Role::Group);
            let c = r.read_next(&root).unwrap();
            assert_eq!(c.role, Role::TextInput);
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

        // Case `read_next_stops_at_end`.
        {
            let root = sample_tree();
            let mut r = recording_reader().0;
            for _ in 0..5 {
                assert!(r.read_next(&root).is_some());
            }
            assert!(
                r.read_next(&root).is_none(),
                "cursor must not run off the end"
            );
            assert_eq!(r.cursor(), Some(4));
        }

        // Case `read_prev_walks_backward`.
        {
            let root = sample_tree();
            let mut r = recording_reader().0;
            r.read_next(&root); // Heading (0)
            r.read_next(&root); // Group (1)
            let back = r.read_prev(&root).unwrap();
            assert_eq!(back.role, Role::Heading { level: 1 });
            assert_eq!(r.cursor(), Some(0));
            assert!(r.read_prev(&root).is_none());
        }

        // Case `read_prev_from_unset_starts_at_last`.
        {
            let root = sample_tree();
            let mut r = recording_reader().0;
            let last = r.read_prev(&root).unwrap();
            assert_eq!(last.role, Role::Link);
            assert_eq!(r.cursor(), Some(4));
        }

        // Case `read_all_speaks_every_node_then_seeds_cursor`.
        {
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
            let recref = rec.0.borrow();
            assert!(recref.utterances()[0].interrupt);
            assert!(!recref.utterances()[1].interrupt);
        }

        // Case `announce_focus_resets_reading_cursor`.
        {
            let root = sample_tree();
            let mut r = recording_reader().0;
            r.read_next(&root);
            r.read_next(&root);
            assert_eq!(r.cursor(), Some(1));
            r.announce_focus(&n(Role::Button).with_name("Elsewhere"));
            assert_eq!(
                r.cursor(),
                None,
                "focus change re-anchors the reading cursor"
            );
        }

        // Case `seed_reading_order_counts_accessible_nodes`.
        {
            let root = sample_tree();
            let mut r = recording_reader().0;
            assert_eq!(r.seed_reading_order(&root), 5);
        }
    }

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
    fn quick_nav_jumps_between_headings_and_controls() {
        // Case `read_next_heading_lands_on_each_heading`.
        {
            let root = quicknav_tree();
            let mut r = recording_reader().0;
            let a = r.read_next_heading(&root).unwrap();
            assert_eq!(a.role, Role::Heading { level: 1 });
            assert_eq!(a.name.as_deref(), Some("Top"));
            // Skips the paragraph and the controls, landing on the next H2.
            let b = r.read_next_heading(&root).unwrap();
            assert_eq!(b.role, Role::Heading { level: 2 });
            assert_eq!(b.name.as_deref(), Some("Body"));
            let c = r.read_next_heading(&root).unwrap();
            assert_eq!(c.name.as_deref(), Some("Footer"));
            assert!(r.read_next_heading(&root).is_none());
        }

        // Case `read_prev_heading_walks_backward`.
        {
            let root = quicknav_tree();
            let mut r = recording_reader().0;
            let last = r.read_prev_heading(&root).unwrap();
            assert_eq!(last.name.as_deref(), Some("Footer"));
            let mid = r.read_prev_heading(&root).unwrap();
            assert_eq!(mid.name.as_deref(), Some("Body"));
            let first = r.read_prev_heading(&root).unwrap();
            assert_eq!(first.name.as_deref(), Some("Top"));
            assert!(r.read_prev_heading(&root).is_none());
        }

        // Case `read_next_control_skips_non_interactive`.
        {
            let root = quicknav_tree();
            let mut r = recording_reader().0;
            let a = r.read_next_control(&root).unwrap();
            assert_eq!(a.role, Role::Button);
            assert_eq!(a.name.as_deref(), Some("Go"));
            let b = r.read_next_control(&root).unwrap();
            assert_eq!(b.role, Role::TextInput);
            assert!(r.read_next_control(&root).is_none());
        }

        // Case `read_prev_control_from_end_walks_backward`.
        {
            let root = quicknav_tree();
            let mut r = recording_reader().0;
            let last = r.read_prev_control(&root).unwrap();
            assert_eq!(last.role, Role::TextInput);
            let prev = r.read_prev_control(&root).unwrap();
            assert_eq!(prev.role, Role::Button);
            assert!(r.read_prev_control(&root).is_none());
        }

        // Case `quick_nav_speaks_a_no_more_cue_when_nothing_matches`.
        {
            let root =
                TW::new(None).child(Box::new(TW::new(Some(n(Role::Button).with_name("Solo")))));
            let (mut r, rec) = recording_reader();
            assert!(r.read_next_heading(&root).is_none());
            assert_eq!(rec.0.borrow().last(), Some(phrase::no_next().as_str()));
        }

        // Case `is_heading_and_is_form_field_predicates`.
        {
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
}
