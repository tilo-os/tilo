//! Which key sends, in every conversation.

use winit::keyboard::ModifiersState;
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerRule {
    /// Enter sends; Shift+Enter breaks the line (a conversation).
    #[default]
    Enter,
    ModEnter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerIntent {
    Submit,
    /// Break the line at the caret.
    Newline,
    Cancel,
    None,
}

impl ComposerRule {
    /// What `key` means, given `held`. Repeats are the field's business: a held
    /// Enter repeats a newline, never a send (a keystroke that repeats must not
    /// send the same message twice).
    #[must_use]
    pub fn intent(self, key: &Key, held: ModifiersState, repeat: bool) -> ComposerIntent {
        match key {
            Key::Named(NamedKey::Escape) => ComposerIntent::Cancel,
            Key::Named(NamedKey::Enter) => self.enter(held, repeat),
            _ => ComposerIntent::None,
        }
    }

    fn enter(self, held: ModifiersState, repeat: bool) -> ComposerIntent {
        let chord = held.control_key() || held.super_key() || held.alt_key();
        match self {
            Self::Enter => {
                if chord {
                    ComposerIntent::None
                } else if held.shift_key() {
                    ComposerIntent::Newline
                } else if repeat {
                    ComposerIntent::None
                } else {
                    ComposerIntent::Submit
                }
            }
            Self::ModEnter => {
                if held.control_key() || held.super_key() {
                    if repeat {
                        ComposerIntent::None
                    } else {
                        ComposerIntent::Submit
                    }
                } else if held.alt_key() {
                    ComposerIntent::None
                } else {
                    ComposerIntent::Newline
                }
            }
        }
    }
}
