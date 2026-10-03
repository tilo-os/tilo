//! Soft-keyboard text input on Android.
//!
//! winit's NativeActivity backend only shows and hides the keyboard, and most
//! soft keyboards send a NativeActivity no key events at all: an input method
//! talks to the focused view's `InputConnection`. `TiloTextInput.java` is that
//! view. It forwards the input method's commits, compositions, deletions and
//! editor keys through `bridge`, and [`TextInputSession`] turns them into the
//! `WindowEvent::Ime` and key events every other platform delivers, so text
//! fields need nothing Android-specific.

use winit::event::{ElementState, Ime, WindowEvent};
use winit::keyboard::{Key, NamedKey};

#[cfg(target_os = "android")]
pub(super) mod bridge;

/// One edit an input method made, as `TiloTextInput` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TextInputOp {
    /// Replaces the composition, if any. A `\n` is the Enter key.
    Commit(String),
    /// The composition, caret at its end; empty removes it.
    Preedit(String),
    Key(NamedKey),
    /// Graphemes to delete before and after the caret, outside the
    /// composition.
    Delete {
        before: u32,
        after: u32,
    },
}

/// The input method's work the focused widget has been shown.
#[derive(Debug, Default)]
pub(super) struct TextInputSession {
    preedit: String,
    focus: Option<taffy::NodeId>,
}

impl TextInputSession {
    pub(super) fn events(&mut self, op: TextInputOp) -> Vec<WindowEvent> {
        match op {
            TextInputOp::Commit(text) => {
                let mut events = self.clear_preedit();
                for (index, line) in text.split('\n').enumerate() {
                    if index > 0 {
                        push_key(&mut events, NamedKey::Enter);
                    }
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    if !line.is_empty() {
                        events.push(WindowEvent::Ime(Ime::Commit(line.to_owned())));
                    }
                }
                events
            }
            TextInputOp::Preedit(text) if text.is_empty() => self.clear_preedit(),
            TextInputOp::Preedit(text) => {
                let caret = text.len();
                self.preedit.clone_from(&text);
                vec![WindowEvent::Ime(Ime::Preedit(text, Some((caret, caret))))]
            }
            TextInputOp::Key(key) => {
                let mut events = self.settle();
                push_key(&mut events, key);
                events
            }
            TextInputOp::Delete { before, after } => {
                let composing = std::mem::take(&mut self.preedit);
                let mut events = Vec::new();
                if !composing.is_empty() {
                    events.push(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                }
                for _ in 0..before {
                    push_key(&mut events, NamedKey::Backspace);
                }
                for _ in 0..after {
                    push_key(&mut events, NamedKey::Delete);
                }
                if !composing.is_empty() {
                    events.extend(self.events(TextInputOp::Preedit(composing)));
                }
                events
            }
        }
    }

    /// Commits the composition in place, as Android does when the user
    /// touches the window or the keyboard sends an editor key.
    pub(super) fn settle(&mut self) -> Vec<WindowEvent> {
        let composing = std::mem::take(&mut self.preedit);
        if composing.is_empty() {
            return Vec::new();
        }
        vec![
            WindowEvent::Ime(Ime::Preedit(String::new(), None)),
            WindowEvent::Ime(Ime::Commit(composing)),
        ]
    }

    /// Records the field the keyboard now edits. `true` when it is another
    /// field, whose text the input method must not carry over.
    pub(super) fn refocus(&mut self, focus: Option<taffy::NodeId>) -> bool {
        if focus == self.focus {
            return false;
        }
        self.focus = focus;
        self.preedit.clear();
        true
    }

    fn clear_preedit(&mut self) -> Vec<WindowEvent> {
        if std::mem::take(&mut self.preedit).is_empty() {
            Vec::new()
        } else {
            vec![WindowEvent::Ime(Ime::Preedit(String::new(), None))]
        }
    }
}

fn push_key(events: &mut Vec<WindowEvent>, key: NamedKey) {
    let key = Key::Named(key);
    for state in [ElementState::Pressed, ElementState::Released] {
        events.push(crate::synthetic_input::key_event(&key, state));
    }
}

/// The editor keys an input method sends, by `android.view.KeyEvent` code.
pub(super) fn named_key(key_code: i32) -> Option<NamedKey> {
    Some(match key_code {
        19 => NamedKey::ArrowUp,
        20 => NamedKey::ArrowDown,
        21 => NamedKey::ArrowLeft,
        22 => NamedKey::ArrowRight,
        61 => NamedKey::Tab,
        66 | 160 => NamedKey::Enter,
        67 => NamedKey::Backspace,
        92 => NamedKey::PageUp,
        93 => NamedKey::PageDown,
        111 => NamedKey::Escape,
        112 => NamedKey::Delete,
        122 => NamedKey::Home,
        123 => NamedKey::End,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(events: &[WindowEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                WindowEvent::Ime(Ime::Commit(text)) => format!("commit {text}"),
                WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                    format!("preedit {text:?} {cursor:?}")
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    format!("{:?} {:?}", event.logical_key, event.state)
                }
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn composition_then_commit_replaces_the_preedit() {
        let mut session = TextInputSession::default();
        let composing = session.events(TextInputOp::Preedit("hel".into()));
        assert_eq!(describe(&composing), ["preedit \"hel\" Some((3, 3))"]);
        let committed = session.events(TextInputOp::Commit("hello".into()));
        assert_eq!(describe(&committed), ["preedit \"\" None", "commit hello"]);
        let spaced = session.events(TextInputOp::Commit(" ".into()));
        assert_eq!(describe(&spaced), ["commit  "]);
    }

    #[test]
    fn newline_in_a_commit_is_the_enter_key() {
        let mut session = TextInputSession::default();
        let events = session.events(TextInputOp::Commit("a\nb\r\n".into()));
        assert_eq!(
            describe(&events),
            [
                "commit a",
                "Named(Enter) Pressed",
                "Named(Enter) Released",
                "commit b",
                "Named(Enter) Pressed",
                "Named(Enter) Released",
            ]
        );
    }

    #[test]
    fn deletion_outside_a_composition_keeps_the_composition() {
        let mut session = TextInputSession::default();
        session.events(TextInputOp::Preedit("wor".into()));
        let events = session.events(TextInputOp::Delete {
            before: 2,
            after: 1,
        });
        assert_eq!(
            describe(&events),
            [
                "preedit \"\" None",
                "Named(Backspace) Pressed",
                "Named(Backspace) Released",
                "Named(Backspace) Pressed",
                "Named(Backspace) Released",
                "Named(Delete) Pressed",
                "Named(Delete) Released",
                "preedit \"wor\" Some((3, 3))",
            ]
        );
    }

    #[test]
    fn editor_keys_and_touches_commit_the_composition_first() {
        let mut session = TextInputSession::default();
        session.events(TextInputOp::Preedit("ok".into()));
        let events = session.events(TextInputOp::Key(NamedKey::Enter));
        assert_eq!(
            describe(&events),
            [
                "preedit \"\" None",
                "commit ok",
                "Named(Enter) Pressed",
                "Named(Enter) Released",
            ]
        );
        session.events(TextInputOp::Preedit("tap".into()));
        assert_eq!(
            describe(&session.settle()),
            ["preedit \"\" None", "commit tap"]
        );
        assert!(session.settle().is_empty());
    }

    #[test]
    fn a_new_field_drops_the_old_composition() {
        let mut session = TextInputSession::default();
        let mut taffy = taffy::TaffyTree::<()>::new();
        let first = taffy.new_leaf(taffy::Style::default()).ok();
        let second = taffy.new_leaf(taffy::Style::default()).ok();
        assert!(session.refocus(first));
        session.events(TextInputOp::Preedit("half".into()));
        assert!(!session.refocus(first));
        assert!(session.refocus(second));
        assert!(session.settle().is_empty());
    }

    #[test]
    fn android_editor_key_codes() {
        assert_eq!(named_key(67), Some(NamedKey::Backspace));
        assert_eq!(named_key(112), Some(NamedKey::Delete));
        assert_eq!(named_key(66), Some(NamedKey::Enter));
        assert_eq!(named_key(21), Some(NamedKey::ArrowLeft));
        assert_eq!(named_key(29), None);
    }
}
