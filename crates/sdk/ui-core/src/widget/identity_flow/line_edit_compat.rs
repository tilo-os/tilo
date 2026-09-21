//! Compatibility state for applications built against the original identity
//! flow inline editor. New identity screens embed [`super::super::TextInput`].

/// Single-line editing state retained for the stable SDK surface.
#[deprecated(note = "embed tilo_ui::widget::TextInput for new single-line fields")]
#[derive(Clone, Debug, Default)]
pub struct LineEditState {
    pub text: String,
    /// Byte index into `text`, always clamped to a character boundary.
    pub cursor: usize,
    /// Anchor end of the selection, or `None` when nothing is selected.
    pub selection_anchor: Option<usize>,
}

#[allow(
    deprecated,
    reason = "implementation of the deprecated compatibility type"
)]
impl LineEditState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            selection_anchor: None,
        }
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
        }
    }

    pub fn selected_text(&self) -> &str {
        self.selection_range()
            .and_then(|(lo, hi)| self.text.get(lo..hi))
            .unwrap_or("")
    }

    pub fn move_cursor(&mut self, pos: usize, extend: bool) {
        let mut pos = pos.min(self.text.len());
        while pos > 0 && !self.text.is_char_boundary(pos) {
            pos = pos.saturating_sub(1);
        }
        if extend {
            self.selection_anchor.get_or_insert(self.cursor);
        } else {
            self.selection_anchor = None;
        }
        self.cursor = pos;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection_range() else {
            return false;
        };
        self.text.replace_range(lo..hi, "");
        self.cursor = lo;
        self.selection_anchor = None;
        true
    }

    pub fn insert_str(&mut self, value: &str) {
        let _ = self.delete_selection();
        self.text.insert_str(self.cursor, value);
        self.cursor = self.cursor.saturating_add(value.len());
        self.selection_anchor = None;
    }

    fn previous_character(&self) -> usize {
        let mut position = self.cursor.saturating_sub(1);
        while position > 0 && !self.text.is_char_boundary(position) {
            position = position.saturating_sub(1);
        }
        position
    }

    fn next_character(&self) -> usize {
        let mut position = self.cursor.saturating_add(1).min(self.text.len());
        while position < self.text.len() && !self.text.is_char_boundary(position) {
            position = position.saturating_add(1);
        }
        position
    }

    fn previous_word(&self) -> usize {
        let bytes = self.text.as_bytes();
        let alphanumeric_before = |position: usize| {
            bytes
                .get(position.saturating_sub(1))
                .is_some_and(u8::is_ascii_alphanumeric)
        };
        let mut position = self.cursor;
        while position > 0 && !alphanumeric_before(position) {
            position = position.saturating_sub(1);
        }
        while position > 0 && alphanumeric_before(position) {
            position = position.saturating_sub(1);
        }
        position
    }

    fn next_word(&self) -> usize {
        let bytes = self.text.as_bytes();
        let alphanumeric_at =
            |position: usize| bytes.get(position).is_some_and(u8::is_ascii_alphanumeric);
        let mut position = self.cursor;
        while position < bytes.len() && alphanumeric_at(position) {
            position = position.saturating_add(1);
        }
        while position < bytes.len() && !alphanumeric_at(position) {
            position = position.saturating_add(1);
        }
        position
    }

    /// Apply one keyboard edit using the original SDK contract.
    pub fn apply_key(
        &mut self,
        key: &winit::keyboard::Key,
        modifiers: &winit::event::Modifiers,
        text_input: Option<&str>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};

        let control = modifiers.state().control_key();
        let extend = modifiers.state().shift_key();
        match key {
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_cursor(
                    if control {
                        self.previous_word()
                    } else {
                        self.previous_character()
                    },
                    extend,
                );
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_cursor(
                    if control {
                        self.next_word()
                    } else {
                        self.next_character()
                    },
                    extend,
                );
                true
            }
            Key::Named(NamedKey::Home) => {
                self.move_cursor(0, extend);
                true
            }
            Key::Named(NamedKey::End) => {
                self.move_cursor(self.text.len(), extend);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if self.delete_selection() {
                    return true;
                }
                if self.cursor == 0 {
                    return false;
                }
                let target = if control {
                    self.previous_word()
                } else {
                    self.previous_character()
                };
                self.text.replace_range(target..self.cursor, "");
                self.cursor = target;
                true
            }
            Key::Named(NamedKey::Delete) => {
                if self.delete_selection() {
                    return true;
                }
                if self.cursor >= self.text.len() {
                    return false;
                }
                let target = if control {
                    self.next_word()
                } else {
                    self.next_character()
                };
                self.text.replace_range(self.cursor..target, "");
                true
            }
            Key::Character(value) if control => self.apply_control_shortcut(value),
            _ => {
                let cleaned: String = text_input
                    .unwrap_or_default()
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                if cleaned.is_empty() {
                    false
                } else {
                    self.insert_str(&cleaned);
                    true
                }
            }
        }
    }

    fn apply_control_shortcut(&mut self, value: &str) -> bool {
        match value.to_lowercase().as_str() {
            "a" => {
                self.selection_anchor = Some(0);
                self.cursor = self.text.len();
                true
            }
            "c" => {
                let selection = self.selected_text();
                crate::text_selection::clipboard_copy(if selection.is_empty() {
                    &self.text
                } else {
                    selection
                });
                true
            }
            "x" if self.selection_range().is_some() => {
                crate::text_selection::clipboard_copy(self.selected_text());
                self.delete_selection()
            }
            "v" => crate::text_selection::clipboard_paste().is_some_and(|pasted| {
                let cleaned: String = pasted
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                self.insert_str(&cleaned);
                true
            }),
            _ => false,
        }
    }
}
