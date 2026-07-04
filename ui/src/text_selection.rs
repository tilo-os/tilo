//! Shared text-selection primitives for `TextInput`, `NotesApp`, and any
//! future text editor (terminal scrollback selector, code editor, ...).
//!
//! This module owns:
//!
//! * [`Selection`] — anchor+cursor pair with helpers for the visible range.
//! * Boundary walkers — `prev_word_boundary` / `next_word_boundary` /
//!   [`word_boundaries`] / [`line_boundaries`] / `prev_char_boundary` /
//!   `next_char_boundary`. All return byte indices and stay on UTF-8
//!   char boundaries by construction.
//! * Clipboard helpers — [`clipboard_copy`] / [`clipboard_paste`] backed
//!   by a process-wide cached `arboard::Clipboard` handle (arboard is
//!   slow to construct on Wayland; we pay that cost once).
//!
//! The boundary predicate (`is_word_char`) mirrors POSIX `\w` and Vim's
//! `iskeyword`: letters, digits, underscore are word characters; whitespace
//! and punctuation are boundaries.

use std::ops::Range;
use std::sync::{Mutex, OnceLock};

/// Anchor + cursor pair. `anchor == cursor` means "no live selection".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection was anchored (typically: where the user
    /// pressed the mouse, or the caret position when shift was first
    /// held). Stays fixed while the cursor moves.
    pub anchor: usize,
    /// Live caret position. Moves while extending the selection.
    pub cursor: usize,
}

impl Selection {
    /// Construct a collapsed selection at `pos`.
    pub fn collapsed(pos: usize) -> Self {
        Self { anchor: pos, cursor: pos }
    }

    /// `true` when `anchor == cursor` — no characters are selected.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.cursor
    }

    /// Lower byte index of the selection.
    pub fn min(&self) -> usize {
        self.anchor.min(self.cursor)
    }

    /// Upper byte index of the selection.
    pub fn max(&self) -> usize {
        self.anchor.max(self.cursor)
    }

    /// Sorted byte range covering the selection.
    pub fn range(&self) -> Range<usize> {
        self.min()..self.max()
    }

    /// Collapse the selection to the cursor position (anchor follows).
    pub fn collapse_to_cursor(&mut self) {
        self.anchor = self.cursor;
    }

    /// Move the cursor to `pos`, snapping the anchor as well so the
    /// selection collapses. Use when the caller wants a pure move.
    pub fn move_to(&mut self, pos: usize) {
        self.anchor = pos;
        self.cursor = pos;
    }

    /// Move the cursor to `pos`, keeping the anchor where it is so the
    /// selection extends. Use this for shift+arrow / shift+click.
    pub fn extend_to(&mut self, pos: usize) {
        self.cursor = pos;
    }
}

/// A character counts as a "word" character if it's a letter, digit,
/// or underscore — same convention as POSIX `\w` and Vim's `iskeyword`.
/// Whitespace and punctuation are word boundaries; double-click word
/// selection and Ctrl+Left/Right use this predicate.
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Walk back to the previous "word boundary" (start of the word the
/// cursor sits in, or start of the previous word if already at a
/// boundary). Returns a byte index that is always on a char boundary.
pub fn prev_word_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx == 0 {
        return 0;
    }
    let mut i = byte_idx.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    // Skip non-word chars walking left (whitespace + punctuation eaten
    // before reaching the previous word's body).
    while i > 0 {
        let c = text[..i].chars().next_back().unwrap();
        if is_word_char(c) {
            break;
        }
        i -= c.len_utf8();
    }
    // Skip the word body itself.
    while i > 0 {
        let c = text[..i].chars().next_back().unwrap();
        if !is_word_char(c) {
            break;
        }
        i -= c.len_utf8();
    }
    i
}

/// Mirror of [`prev_word_boundary`] for the forward direction.
pub fn next_word_boundary(text: &str, byte_idx: usize) -> usize {
    let mut i = byte_idx.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if !is_word_char(c) {
            break;
        }
        i += c.len_utf8();
    }
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if is_word_char(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// Byte range `(start, end)` of the word containing `byte_idx` (or the
/// word immediately on the left when `byte_idx` lands on whitespace
/// right after a word — matches every native editor's double-click
/// behaviour). Used by double-click word selection.
pub fn word_boundaries(text: &str, byte_idx: usize) -> (usize, usize) {
    let mut start = byte_idx.min(text.len());
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    // Find left edge of current word.
    while start > 0 {
        let c = text[..start].chars().next_back().unwrap();
        if !is_word_char(c) {
            break;
        }
        start -= c.len_utf8();
    }
    // Find right edge.
    let mut end = start;
    while end < text.len() {
        let c = text[end..].chars().next().unwrap();
        if !is_word_char(c) {
            break;
        }
        end += c.len_utf8();
    }
    (start, end)
}

/// Byte range `(start, end)` of the *line* containing `byte_idx`, where
/// lines are linefeed-delimited (`\n`). The end is the index of the
/// trailing `\n`, or `text.len()` for the final line. Triple-click line
/// selection uses this.
pub fn line_boundaries(text: &str, byte_idx: usize) -> (usize, usize) {
    let byte_idx = byte_idx.min(text.len());
    let start = text[..byte_idx].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[byte_idx..]
        .find('\n')
        .map(|i| byte_idx + i)
        .unwrap_or(text.len());
    (start, end)
}

/// Step one Unicode scalar to the left of `byte_idx`. Always returns a
/// char-boundary byte index.
pub fn prev_char_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx == 0 {
        return 0;
    }
    let mut i = byte_idx.min(text.len()) - 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Step one Unicode scalar to the right of `byte_idx`. Always returns
/// a char-boundary byte index.
pub fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
    if byte_idx >= text.len() {
        return text.len();
    }
    let mut i = byte_idx + 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Lazy global clipboard handle. `arboard::Clipboard::new()` can be
/// slow on Wayland the first time (sets up the data-control device),
/// so we cache one handle process-wide and lock it per access. Kept
/// behind a `Mutex` because arboard's handle isn't `Sync`.
fn clipboard() -> Option<&'static Mutex<arboard::Clipboard>> {
    static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| arboard::Clipboard::new().ok().map(Mutex::new))
        .as_ref()
}

/// Push `text` onto the system clipboard. Silent no-op if arboard
/// couldn't open (headless test env, locked-down session, ...).
pub fn clipboard_copy(text: &str) {
    if let Some(cb) = clipboard() {
        if let Ok(mut handle) = cb.lock() {
            let _ = handle.set_text(text.to_string());
        }
    }
}

/// Pull the current clipboard contents as a UTF-8 string. Returns
/// `None` when the clipboard is empty / unavailable / non-text.
pub fn clipboard_paste() -> Option<String> {
    let cb = clipboard()?;
    let mut handle = cb.lock().ok()?;
    handle.get_text().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_helpers_min_max_range_collapsed() {
        let s = Selection { anchor: 3, cursor: 7 };
        assert_eq!(s.min(), 3);
        assert_eq!(s.max(), 7);
        assert_eq!(s.range(), 3..7);
        assert!(!s.is_collapsed());

        let s2 = Selection { anchor: 9, cursor: 4 };
        assert_eq!(s2.min(), 4);
        assert_eq!(s2.max(), 9);
        assert_eq!(s2.range(), 4..9);

        let collapsed = Selection::collapsed(5);
        assert!(collapsed.is_collapsed());
        assert_eq!(collapsed.range(), 5..5);
    }

    #[test]
    fn selection_move_and_extend() {
        let mut s = Selection::collapsed(0);
        s.extend_to(4);
        assert_eq!(s.anchor, 0);
        assert_eq!(s.cursor, 4);
        assert!(!s.is_collapsed());

        s.collapse_to_cursor();
        assert!(s.is_collapsed());
        assert_eq!(s.cursor, 4);

        s.move_to(10);
        assert!(s.is_collapsed());
        assert_eq!(s.anchor, 10);
        assert_eq!(s.cursor, 10);
    }

    /// Walking back through a sentence with `prev_word_boundary` must
    /// land on the start of each word.
    #[test]
    fn prev_word_boundary_walks_word_starts() {
        let s = "hello world quick ui";
        assert_eq!(prev_word_boundary(s, s.len()), 18);
        assert_eq!(prev_word_boundary(s, 18), 12);
        assert_eq!(prev_word_boundary(s, 12), 6);
        assert_eq!(prev_word_boundary(s, 6), 0);
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    #[test]
    fn next_word_boundary_walks_word_ends() {
        let s = "hello world quick ui";
        assert_eq!(next_word_boundary(s, 0), 6);
        assert_eq!(next_word_boundary(s, 6), 12);
        assert_eq!(next_word_boundary(s, 12), 18);
        assert_eq!(next_word_boundary(s, 18), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
    }

    #[test]
    fn word_boundaries_snaps_to_word_bounds_ascii() {
        let s = "hello world";
        assert_eq!(word_boundaries(s, 2), (0, 5));
        assert_eq!(word_boundaries(s, 0), (0, 5));
        // At the space (idx 5), the left edge is the start of the previous
        // word body — anchor snaps left.
        assert_eq!(word_boundaries(s, 5), (0, 5));
        assert_eq!(word_boundaries(s, 6), (6, 11));
        assert_eq!(word_boundaries(s, 11), (6, 11));
    }

    /// Multi-byte UTF-8 must be handled cleanly — no slicing into the
    /// middle of a code point. "café" — é is 2 bytes.
    #[test]
    fn word_boundaries_utf8_clean() {
        let s = "café latte";
        // Bytes: c=0 a=1 f=2 é=3..5 (space=5) l=6 a=7 t=8 t=9 e=10. len=11.
        assert_eq!(s.len(), 11);
        // Cursor inside "café": should select 0..5.
        assert_eq!(word_boundaries(s, 3), (0, 5));
        // Cursor at the space: snap left to 0..5.
        assert_eq!(word_boundaries(s, 5), (0, 5));
        // Cursor in "latte": 6..11.
        assert_eq!(word_boundaries(s, 8), (6, 11));
    }

    #[test]
    fn word_boundaries_empty_string() {
        assert_eq!(word_boundaries("", 0), (0, 0));
    }

    #[test]
    fn line_boundaries_basic() {
        let s = "first\nsecond\nthird";
        // In "first" (idx 2): start 0, end 5 (the \n).
        assert_eq!(line_boundaries(s, 2), (0, 5));
        // At the first newline (idx 5): treated as end-of-first-line.
        assert_eq!(line_boundaries(s, 5), (0, 5));
        // In "second" (idx 8): start 6, end 12.
        assert_eq!(line_boundaries(s, 8), (6, 12));
        // In "third" (idx 14): start 13, end = len = 18.
        assert_eq!(line_boundaries(s, 14), (13, 18));
        // At very end.
        assert_eq!(line_boundaries(s, s.len()), (13, 18));
    }

    #[test]
    fn line_boundaries_empty_string() {
        assert_eq!(line_boundaries("", 0), (0, 0));
    }

    #[test]
    fn line_boundaries_only_newlines() {
        let s = "\n\n";
        // Before first \n: empty line, (0,0).
        assert_eq!(line_boundaries(s, 0), (0, 0));
        // Right after first \n: empty middle line, (1,1).
        assert_eq!(line_boundaries(s, 1), (1, 1));
        // After last \n: empty final line, (2,2).
        assert_eq!(line_boundaries(s, 2), (2, 2));
    }

    #[test]
    fn prev_next_char_boundary_utf8() {
        let s = "aé€b"; // a=0, é=1..3, €=3..6, b=6..7. len=7.
        assert_eq!(s.len(), 7);
        // Step right across multi-byte chars.
        assert_eq!(next_char_boundary(s, 0), 1);
        assert_eq!(next_char_boundary(s, 1), 3);
        assert_eq!(next_char_boundary(s, 3), 6);
        assert_eq!(next_char_boundary(s, 6), 7);
        assert_eq!(next_char_boundary(s, 7), 7);
        // And left.
        assert_eq!(prev_char_boundary(s, 7), 6);
        assert_eq!(prev_char_boundary(s, 6), 3);
        assert_eq!(prev_char_boundary(s, 3), 1);
        assert_eq!(prev_char_boundary(s, 1), 0);
        assert_eq!(prev_char_boundary(s, 0), 0);
    }

    #[test]
    fn boundaries_at_edges() {
        let s = "abc";
        // At end.
        assert_eq!(prev_word_boundary(s, s.len()), 0);
        assert_eq!(next_word_boundary(s, s.len()), s.len());
        // At start.
        assert_eq!(prev_word_boundary(s, 0), 0);
        assert_eq!(next_word_boundary(s, 0), s.len());
        // Empty string.
        assert_eq!(prev_word_boundary("", 0), 0);
        assert_eq!(next_word_boundary("", 0), 0);
    }
}
