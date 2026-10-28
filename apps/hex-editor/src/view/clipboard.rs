//! Copying bytes out and pasting them back in, in the mode the grid is in.
//!
//! The grid has one focus: the hex column or the ASCII column (`Tab`
//! switches). Copy takes the selection in that mode — hexadecimal bytes
//! (`de ad be ef`) from the hex column, characters from the ASCII one, with
//! a byte that is not printable copied as the `.` the column shows. Paste
//! reads the same way round, through [`crate::actions::parse_bytes`] for hex
//! and the text's own UTF-8 bytes for ASCII, and lands where the selection
//! is (or at the cursor when there is none) as **one undo step** — the same
//! discipline a find-and-replace batch keeps.
//!
//! Overwrite mode keeps the file's length, so a paste that would change it is
//! refused with the fix named, exactly as a replacement of another length is.

use super::HexView;
use crate::model::{ChangesLength, Column, EditMode};

impl HexView {
    /// The selection as the clipboard text of the focus mode, with how many
    /// bytes it covers. `None` when nothing is selected.
    fn selection_as_text(&self) -> Option<(String, usize)> {
        let (lo, hi) = self.model.selection()?;
        let bytes = self.model.bytes().get(lo..=hi)?;
        let text = match self.model.column() {
            Column::Hex => bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" "),
            // The ASCII column shows a byte it cannot print as a dot; the
            // copy says the same rather than pretending the bytes are text.
            Column::Ascii => bytes
                .iter()
                .map(|&byte| {
                    if byte.is_ascii_graphic() || byte == b' ' {
                        char::from(byte)
                    } else {
                        '.'
                    }
                })
                .collect(),
        };
        Some((text, bytes.len()))
    }

    /// Copy the selection in the focus mode. Returns whether anything was
    /// copied.
    pub(super) fn copy_selection(&mut self) -> bool {
        let Some((text, count)) = self.selection_as_text() else {
            self.notice("Nothing selected: hold Shift while moving to select bytes");
            return false;
        };
        ui::clipboard_copy(&text);
        self.notice(&format!("Copied {count} bytes as {}", self.mode_word()));
        true
    }

    /// Cut the selection: copy it, then delete it as one undo step.
    pub(super) fn cut_selection(&mut self) -> bool {
        if !self.copy_selection() {
            return false;
        }
        let count = self.model.selection_len();
        self.model.delete_at_cursor();
        self.submit_one_step();
        self.notice(&format!("Cut {count} bytes"));
        true
    }

    /// Paste the clipboard at the selection (or at the cursor) in the focus
    /// mode, as one undo step. Returns whether the buffer changed.
    pub(super) fn paste_clipboard(&mut self) -> bool {
        let Some(text) = ui::clipboard_paste() else {
            self.notice("The clipboard holds no text");
            return false;
        };
        let bytes = match self.model.column() {
            Column::Hex => match crate::actions::parse_bytes(&text) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    self.problem("Couldn't paste as hexadecimal bytes", reason);
                    return false;
                }
            },
            Column::Ascii => text.as_bytes().to_vec(),
        };
        if bytes.is_empty() {
            self.notice("The clipboard holds no bytes");
            return false;
        }
        // A selection is replaced; without one the bytes go in at the cursor,
        // which overwrite mode can only do by replacing what is there.
        let span = self
            .model
            .selection()
            .map(|(lo, hi)| (lo, hi.saturating_add(1)));
        match span {
            Some(span) => self.replace_one_span(span, &bytes),
            None if self.model.mode() == EditMode::Insert => {
                let at = self.model.cursor();
                for (offset, byte) in bytes.iter().enumerate() {
                    self.model.insert_byte(at.saturating_add(offset), *byte);
                }
                self.model.move_to(at.saturating_add(bytes.len()), false);
                self.submit_one_step();
                self.notice(&format!("Pasted {} bytes", bytes.len()));
                true
            }
            None => {
                let at = self.model.cursor();
                let end = at.saturating_add(bytes.len());
                if end > self.model.len() {
                    self.problem(
                        "Couldn't paste in Overwrite mode",
                        "What you pasted is longer than the file has left. Switch to Insert to make the file longer.",
                    );
                    return false;
                }
                self.replace_one_span((at, end), &bytes)
            }
        }
    }

    /// Replace `span` with `bytes` as one undo step, saying what went wrong
    /// when overwrite mode cannot take it.
    fn replace_one_span(&mut self, span: (usize, usize), bytes: &[u8]) -> bool {
        match self.model.replace_spans(&[span], bytes) {
            Ok(0) => {
                self.notice("What you pasted is already there");
                false
            }
            Ok(_) => {
                self.submit_one_step();
                self.notice(&format!("Pasted {} bytes", bytes.len()));
                true
            }
            Err(ChangesLength) => {
                self.problem(
                    "Couldn't paste in Overwrite mode",
                    "What you pasted has another length than the selection. Switch to Insert to change the file's length.",
                );
                false
            }
        }
    }

    /// How the focus mode is named in a notice.
    fn mode_word(&self) -> &'static str {
        match self.model.column() {
            Column::Hex => "hexadecimal bytes",
            Column::Ascii => "text",
        }
    }
}
