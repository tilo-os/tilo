//! Focused widget IME intent shared by native windows and browser input bridges.
/// Keyed, ephemeral equality check for a field's text and selection. This is
/// invalidation metadata, never a persisted text hash or insertion authority.
pub fn edit_fingerprint(text: &str, anchor: usize, focus: usize, key: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(&anchor.to_le_bytes());
    hash.update(&focus.to_le_bytes());
    hash.update(text.as_bytes());
    *hash.finalize().as_bytes()
}

/// Scope a private field stamp to its editable subtarget inside one widget.
pub fn scoped_fingerprint(scope: &[u8], field: [u8; 32], key: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(b"tilo.ime.subtarget.v1");
    hash.update(&field);
    hash.update(scope);
    *hash.finalize().as_bytes()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImeWindowState {
    pub purpose: winit::window::ImePurpose,
    /// The original editable field accepts line feeds, independently of its pixels.
    pub multiline: bool,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ImeWindowState {
    pub fn from_widget(widget: &dyn crate::Widget) -> Option<Self> {
        let purpose = widget.ime_purpose()?;
        let rect = widget.ime_cursor_rect()?;
        let mut intent = Self::from_rect(purpose, rect);
        intent.multiline = purpose == winit::window::ImePurpose::Normal && widget.ime_multiline();
        Some(intent)
    }

    #[must_use]
    pub fn from_rect(purpose: winit::window::ImePurpose, rect: vello::kurbo::Rect) -> Self {
        let bounded = |v: f64| {
            if v.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is clamped to [0.0, u32::MAX] on the line above, so the cast is exact"
                )]
                {
                    v.max(0.0).min(f64::from(u32::MAX)).round() as u32
                }
            } else {
                0
            }
        };
        ImeWindowState {
            purpose,
            multiline: false,
            x: bounded(rect.x0),
            y: bounded(rect.y0),
            width: bounded(rect.width()).max(1),
            height: bounded(rect.height()).max(1),
        }
    }
}

/// What an input method may read of the focused field: its text around the
/// selection, for suggestions, autocorrection and word deletion.
///
/// Read from the field's accessibility node, so a field that hides its value
/// from assistive technology (a password) hides it from the keyboard too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImeSurroundingText {
    pub text: String,
    /// Byte offsets into `text`.
    pub anchor: usize,
    pub focus: usize,
}

impl ImeSurroundingText {
    /// Bytes of context kept on each side of the selection.
    pub const CONTEXT: usize = 2048;

    pub fn from_widget(widget: &dyn crate::Widget) -> Option<Self> {
        let node = widget.accessibility()?;
        if node.state.protected {
            return None;
        }
        let selection = node.text_selection?;
        Self::around(&node.value?, selection.anchor, selection.focus)
    }

    /// The selection within `text`, with up to [`Self::CONTEXT`] bytes on
    /// each side. `None` when an offset is not a character boundary.
    pub fn around(text: &str, anchor: usize, focus: usize) -> Option<Self> {
        if !text.is_char_boundary(anchor) || !text.is_char_boundary(focus) {
            return None;
        }
        let start = text.ceil_char_boundary(anchor.min(focus).saturating_sub(Self::CONTEXT));
        let end = text.floor_char_boundary(anchor.max(focus).saturating_add(Self::CONTEXT));
        Some(Self {
            text: text.get(start..end)?.to_owned(),
            anchor: anchor - start,
            focus: focus - start,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ImeSurroundingText;
    use crate::Widget;
    use crate::widget::TextInput;

    #[test]
    fn surrounding_text_is_the_fields_text_and_selection() {
        let mut field = TextInput::local().with_initial_value("hello world");
        field.set_focus(true);
        assert!(field.perform_action(
            &crate::a11y::Action::SetSelection {
                anchor: 6,
                focus: 11
            },
            &[],
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            ImeSurroundingText::from_widget(&field),
            Some(ImeSurroundingText {
                text: "hello world".into(),
                anchor: 6,
                focus: 11,
            })
        );
    }

    #[test]
    fn deferred_input_fingerprint_changes_with_selection_at_the_same_caret() {
        let key = [7; 32];
        let mut field = TextInput::local().with_initial_value("hello");
        field.set_focus(true);
        field.set_cursor_pos(5);
        let before = field.ime_edit_fingerprint(&key).unwrap();
        let caret = field.cursor_pos();
        assert!(field.perform_action(
            &crate::a11y::Action::SetSelection {
                anchor: 0,
                focus: 5
            },
            &[],
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(field.cursor_pos(), caret);
        assert_ne!(field.ime_edit_fingerprint(&key).unwrap(), before);
        assert!(
            TextInput::local()
                .with_secret(true)
                .ime_edit_fingerprint(&key)
                .is_none()
        );
    }

    #[test]
    fn a_password_field_gives_the_keyboard_nothing() {
        let mut field = TextInput::local()
            .with_initial_value("hunter2")
            .with_secret(true);
        field.set_focus(true);
        assert_eq!(ImeSurroundingText::from_widget(&field), None);
    }

    #[test]
    fn long_text_is_cut_to_whole_characters_around_the_selection() {
        let before = "é".repeat(ImeSurroundingText::CONTEXT);
        let after = "ü".repeat(ImeSurroundingText::CONTEXT);
        let text = format!("{before}|{after}");
        let caret = before.len();
        let around = ImeSurroundingText::around(&text, caret, caret + 1).unwrap();
        assert_eq!(around.text.len(), 2 * ImeSurroundingText::CONTEXT + 1);
        assert_eq!(&around.text[around.anchor..around.focus], "|");
        assert!(around.text.starts_with('é') && around.text.ends_with('ü'));
        assert_eq!(ImeSurroundingText::around("é", 1, 1), None);
    }
}
