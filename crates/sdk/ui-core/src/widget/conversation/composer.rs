//! Which key sends, in every conversation.
//!
//! One rule, in both apps: **Enter sends**, **Shift+Enter breaks the line**,
//! and **Escape lets the draft go**. A field that submits on Mod+Enter is for a
//! comment inside a document, not for a conversation — a messenger where Return
//! adds a newline is a messenger where nothing is ever sent.
//!
//! The rule is a pure function of the rule, the key and the modifiers held, so a
//! test can pin it and both apps can apply it to whatever receives their
//! keystrokes: a plain [`TextInput`](crate::widget::TextInput) (configured with
//! `with_enter_submits`, which implements the same rule) or a rich composer.

use winit::keyboard::ModifiersState;
use winit::keyboard::{Key, NamedKey};

/// How a composer's field ends a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerRule {
    /// Enter sends; Shift+Enter breaks the line (a conversation).
    #[default]
    Enter,
    /// Ctrl+Enter (Cmd+Enter) sends; Enter starts a paragraph (a comment).
    ModEnter,
}

/// What a keystroke means to a composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerIntent {
    /// Send what the field holds.
    Submit,
    /// Break the line at the caret.
    Newline,
    /// Let the draft go (Escape).
    Cancel,
    /// Anything else: the field handles it.
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
        // A shortcut chord (Ctrl/Cmd+Enter, Ctrl+Enter with any other modifier)
        // is the window's, not the composer's.
        let chord = held.control_key() || held.super_key() || held.alt_key();
        match self {
            Self::Enter => {
                if chord {
                    ComposerIntent::None
                } else if held.shift_key() {
                    ComposerIntent::Newline
                } else if repeat {
                    // One press, one send; a held key does not send again.
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
