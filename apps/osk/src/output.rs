//! What the keyboard sends to the focused text field, and how that reaches it.
//!
//! [`crate::OskWidget`] reduces taps, swipes and suggestion picks into an
//! ordered stream of [`OskOutput`]s. The in-progress word is *composed*: it is
//! shown in the field as preedit text and only committed at a word boundary
//! (space, punctuation, Return, a suggestion tap, the next swipe). That makes
//! replacing the word — a swipe correction, a suggestion chip, Backspace inside
//! it — a preedit update instead of a delete-and-retype.
//!
//! Two sinks consume the stream:
//!
//! * **Input method** (the Tilo session). Preedit and commits travel on the
//!   `input-method-v2` connection; editing keys (Backspace, Return, Tab) travel
//!   as keysyms on the compositor-gated `virtual-keyboard-v1` connection, so a
//!   single-line field sees a real Return and submits. Those are two Wayland
//!   connections, and the compositor orders requests only *within* one, so
//!   [`OrderedRouter`] inserts a [`Step::Barrier`] (a round trip on the
//!   connection just used) whenever the stream switches connection. A commit
//!   therefore always lands before the Backspace that follows it.
//! * **Virtual keyboard only** (other compositors). There is no preedit, so
//!   [`PreeditEmulator`] types the composed word directly and edits it with
//!   Backspace as it changes.
//!
//! Everything here is pure; the live Wayland halves live in
//! [`crate::imclient`] and [`crate::inject`].

use crate::inject::{Keypress, Symbol};

/// One ordered instruction for the focused text field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OskOutput {
    /// Replace the composing (preedit) text with this string. Empty clears it.
    SetPreedit(String),
    /// Replace the composing text with this string and make it final.
    Commit(String),
    /// Press a key, with the modifiers held for it. The keyboard never emits
    /// one while text is still composing: the composition is committed (or
    /// cleared) first.
    Key(Keypress),
}

impl OskOutput {
    /// Press `symbol` with no modifiers.
    #[must_use]
    pub fn key(symbol: Symbol) -> Self {
        Self::Key(symbol.into())
    }
}

/// The Wayland connection a [`Step`] travels on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// `input-method-v2`: preedit and commit strings.
    InputMethod,
    /// `virtual-keyboard-v1`: editing keysyms.
    Keys,
}

/// One request for a live sink, in the order it must be issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Preedit(String),
    /// `commit_string` + `commit` on the input method.
    Commit(String),
    /// Tap a keysym on the virtual keyboard, with its modifiers held.
    Key(Keypress),
    /// Round-trip on this channel before issuing anything on the other one, so
    /// the compositor has applied everything already sent.
    Barrier(Channel),
}

/// Routes [`OskOutput`]s onto the input method and the virtual keyboard while
/// keeping their relative order.
#[derive(Debug, Default)]
pub struct OrderedRouter {
    last: Option<Channel>,
}

impl OrderedRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The steps that deliver `output`, including the barrier needed when it
    /// travels on a different connection than the previous output.
    pub fn route(&mut self, output: OskOutput) -> Vec<Step> {
        let (channel, step) = match output {
            OskOutput::SetPreedit(text) => (Channel::InputMethod, Step::Preedit(text)),
            OskOutput::Commit(text) => (Channel::InputMethod, Step::Commit(text)),
            OskOutput::Key(keypress) => (Channel::Keys, Step::Key(keypress)),
        };
        let mut steps = Vec::with_capacity(2);
        if let Some(previous) = self.last
            && previous != channel
        {
            steps.push(Step::Barrier(previous));
        }
        self.last = Some(channel);
        steps.push(step);
        steps
    }
}

/// Delivers [`OskOutput`]s through keysyms alone, for compositors that offer a
/// virtual keyboard but no input method. The composed word is typed as it
/// grows and trimmed with Backspace as it shrinks or changes.
#[derive(Debug, Default)]
pub struct PreeditEmulator {
    shown: String,
}

impl PreeditEmulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// The keys that bring the field in line with `output`.
    pub fn keypresses(&mut self, output: &OskOutput) -> Vec<Keypress> {
        match output {
            OskOutput::Key(keypress) => {
                self.shown.clear();
                vec![*keypress]
            }
            other => self
                .symbols(other)
                .into_iter()
                .map(Keypress::from)
                .collect(),
        }
    }

    /// The keysyms that bring the field in line with `output`.
    pub fn symbols(&mut self, output: &OskOutput) -> Vec<Symbol> {
        match output {
            OskOutput::SetPreedit(text) => self.retype(text),
            OskOutput::Commit(text) => {
                let symbols = self.retype(text);
                self.shown.clear();
                symbols
            }
            OskOutput::Key(keypress) => {
                // The keyboard commits before any key; whatever was shown is
                // final text from here on.
                self.shown.clear();
                vec![keypress.symbol]
            }
        }
    }

    fn retype(&mut self, target: &str) -> Vec<Symbol> {
        let common = self
            .shown
            .chars()
            .zip(target.chars())
            .take_while(|(a, b)| a == b)
            .count();
        let erase = self.shown.chars().count().saturating_sub(common);
        let mut symbols: Vec<Symbol> = std::iter::repeat_n(Symbol::BackSpace, erase).collect();
        symbols.extend(target.chars().skip(common).map(Symbol::from_char));
        target.clone_into(&mut self.shown);
        symbols
    }
}

/// The text a field would hold after receiving a stream of [`OskOutput`]s,
/// with the composing word as its tail. Bounded: bytes past `limit` are
/// counted, not stored.
#[derive(Debug)]
pub struct TextMirror {
    text: String,
    preedit_start: usize,
    limit: usize,
    dropped_bytes: usize,
}

impl TextMirror {
    pub fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            preedit_start: 0,
            limit,
            dropped_bytes: 0,
        }
    }

    /// Committed text followed by the composing text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Bytes that did not fit under the limit.
    pub fn dropped_bytes(&self) -> usize {
        self.dropped_bytes
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.preedit_start = 0;
    }

    pub fn apply(&mut self, output: &OskOutput) {
        match output {
            OskOutput::SetPreedit(text) => {
                self.text.truncate(self.preedit_start);
                self.push(text);
            }
            OskOutput::Commit(text) => {
                self.text.truncate(self.preedit_start);
                self.push(text);
                self.preedit_start = self.text.len();
            }
            OskOutput::Key(keypress) if !keypress.modifiers.is_empty() => {
                // A chord is a command to the program, not text.
                self.preedit_start = self.text.len();
            }
            OskOutput::Key(keypress) => {
                self.preedit_start = self.text.len();
                match keypress.symbol {
                    Symbol::BackSpace => {
                        self.text.pop();
                    }
                    Symbol::Return => self.push("\n"),
                    Symbol::Tab => self.push("\t"),
                    Symbol::Space => self.push(" "),
                    Symbol::Unicode(c) => self.push(c.encode_utf8(&mut [0; 4])),
                    // The mirror keeps no caret; it stays a record of what
                    // was typed, in order.
                    Symbol::Left | Symbol::Right | Symbol::Up | Symbol::Down | Symbol::Escape => {}
                }
                self.preedit_start = self.text.len();
            }
        }
    }

    fn push(&mut self, text: &str) {
        let remaining = self.limit.saturating_sub(self.text.len());
        let mut end = remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if let Some(prefix) = text.get(..end) {
            self.text.push_str(prefix);
        }
        self.dropped_bytes = self
            .dropped_bytes
            .saturating_add(text.len().saturating_sub(end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(s: &str) -> OskOutput {
        OskOutput::SetPreedit(s.into())
    }
    fn commit(s: &str) -> OskOutput {
        OskOutput::Commit(s.into())
    }

    #[test]
    fn router_inserts_a_barrier_only_when_the_connection_changes() {
        let mut router = OrderedRouter::new();
        assert_eq!(router.route(pre("he")), vec![Step::Preedit("he".into())]);
        assert_eq!(router.route(commit("he")), vec![Step::Commit("he".into())]);
        assert_eq!(
            router.route(OskOutput::key(Symbol::Return)),
            vec![
                Step::Barrier(Channel::InputMethod),
                Step::Key(Keypress::from(Symbol::Return))
            ]
        );
        assert_eq!(
            router.route(OskOutput::key(Symbol::BackSpace)),
            vec![Step::Key(Keypress::from(Symbol::BackSpace))]
        );
        assert_eq!(
            router.route(commit("x")),
            vec![Step::Barrier(Channel::Keys), Step::Commit("x".into())]
        );
    }

    #[test]
    fn emulator_types_grows_and_trims_the_composed_word() {
        let mut vk = PreeditEmulator::new();
        assert_eq!(vk.symbols(&pre("t")), vec![Symbol::Unicode('t')]);
        assert_eq!(vk.symbols(&pre("th")), vec![Symbol::Unicode('h')]);
        assert_eq!(
            vk.symbols(&pre("the")),
            vec![Symbol::Unicode('e')],
            "extension types only the new letter"
        );
        assert_eq!(
            vk.symbols(&commit("they ")),
            vec![Symbol::Unicode('y'), Symbol::Space]
        );
        // After a commit nothing is composing: a new word starts from scratch.
        assert_eq!(vk.symbols(&pre("a")), vec![Symbol::Unicode('a')]);
        assert_eq!(vk.symbols(&pre("")), vec![Symbol::BackSpace]);
        assert_eq!(
            vk.symbols(&OskOutput::key(Symbol::BackSpace)),
            vec![Symbol::BackSpace]
        );
    }

    #[test]
    fn emulator_replaces_a_differing_word() {
        let mut vk = PreeditEmulator::new();
        vk.symbols(&pre("quick"));
        assert_eq!(
            vk.symbols(&pre("quiet")),
            vec![
                Symbol::BackSpace,
                Symbol::BackSpace,
                Symbol::Unicode('e'),
                Symbol::Unicode('t')
            ]
        );
    }

    #[test]
    fn mirror_replaces_the_composing_tail_and_keeps_commits() {
        let mut m = TextMirror::new(64);
        m.apply(&pre("he"));
        m.apply(&pre("hel"));
        assert_eq!(m.text(), "hel");
        m.apply(&commit("help "));
        m.apply(&pre("m"));
        assert_eq!(m.text(), "help m");
        m.apply(&pre(""));
        m.apply(&OskOutput::key(Symbol::BackSpace));
        assert_eq!(m.text(), "help");
        m.apply(&OskOutput::key(Symbol::Return));
        assert_eq!(m.text(), "help\n");
    }

    #[test]
    fn mirror_is_bounded() {
        let mut m = TextMirror::new(4);
        m.apply(&commit("abcdef"));
        assert_eq!(m.text(), "abcd");
        assert_eq!(m.dropped_bytes(), 2);
    }
}
