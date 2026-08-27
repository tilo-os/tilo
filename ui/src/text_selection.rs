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
//! * Clipboard helpers — [`clipboard_copy`] / [`clipboard_paste`] and the
//!   primary-selection pair, resolved against whichever backend this process
//!   has (see [`crate::clipboard`]): the compositor's mirror for in-process
//!   chrome, `wl_data_device` for a Wayland app, arboard on X11, and an
//!   in-process buffer of last resort that announces itself in the log.
//!
//! The boundary predicate (`is_word_char`) mirrors POSIX `\w` and Vim's
//! `iskeyword`: letters, digits, underscore are word characters; whitespace
//! and punctuation are boundaries.

use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use crate::clipboard::{ClipboardTarget, clipboard_backend, warn_fallback_once};

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
    pub fn collapsed(pos: usize) -> Self {
        Self { anchor: pos, cursor: pos }
    }

    /// `true` when `anchor == cursor` — no characters are selected.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.cursor
    }

    pub fn min(&self) -> usize {
        self.anchor.min(self.cursor)
    }

    pub fn max(&self) -> usize {
        self.anchor.max(self.cursor)
    }

    pub fn range(&self) -> Range<usize> {
        self.min()..self.max()
    }

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

// --- char-indexed variants -------------------------------------------------
//
// The boundary walkers above index bytes; a reflowable reader works in *char*
// offsets into a block's canonical text (a line-run carries `char_start` /
// `char_len`, not byte spans). These thin wrappers convert char↔byte around the
// byte-based walkers so the same word/char semantics drive keyboard selection
// extension and double-click word selection in a char-indexed document.

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

/// Char index of the previous word boundary at or before `char_idx` — the
/// char-indexed mirror of [`prev_word_boundary`]. Used to extend a reader
/// selection leftward by a whole word (Shift+Ctrl+Left).
pub fn prev_word_boundary_chars(text: &str, char_idx: usize) -> usize {
    byte_to_char(text, prev_word_boundary(text, char_to_byte(text, char_idx)))
}

/// Char index of the next word boundary at or after `char_idx` — the
/// char-indexed mirror of [`next_word_boundary`]. Used to extend a reader
/// selection rightward by a whole word (Shift+Ctrl+Right).
pub fn next_word_boundary_chars(text: &str, char_idx: usize) -> usize {
    byte_to_char(text, next_word_boundary(text, char_to_byte(text, char_idx)))
}

/// Char range `(start, end)` of the word containing `char_idx` — the
/// char-indexed mirror of [`word_boundaries`]. Drives double-click word
/// selection in a reflowable reader.
pub fn word_boundaries_chars(text: &str, char_idx: usize) -> (usize, usize) {
    let (s, e) = word_boundaries(text, char_to_byte(text, char_idx));
    (byte_to_char(text, s), byte_to_char(text, e))
}

/// Word/line-granularity drag extension — the shared rule behind
/// double-click-then-drag in `TextInput` and the reader.
///
/// After a double-/triple-click snaps the anchor to a whole unit
/// `(anchor_lo, anchor_hi)` (`lo <= hi`), a continuing drag must keep that
/// unit *inside* the selection and grow by whole units. Given the boundary
/// span `(unit_lo, unit_hi)` the pointer now sits in and its own position
/// `raw` (to decide which side of the anchor it's on), returns the
/// `(anchor, cursor)` the selection should adopt.
///
/// `T` is whatever a selector indexes by — a byte offset (`TextInput`) or a
/// `(block, char)` document position (the reader) — so both share one rule.
pub fn extend_span_drag<T: Ord + Copy>(
    anchor_lo: T,
    anchor_hi: T,
    raw: T,
    unit_lo: T,
    unit_hi: T,
) -> (T, T) {
    if raw < anchor_lo {
        // Left of the original unit: pin the anchor to its far edge.
        (anchor_hi, unit_lo)
    } else {
        // Inside or right of the original unit: pin the anchor to its near edge.
        (anchor_lo, unit_hi)
    }
}

/// Lazy global `arboard` handle, used **only on X11** and other non-Wayland
/// platforms.
///
/// On Wayland `arboard` talks `zwlr_data_control_manager_v1`, which qOS does
/// not offer to sandboxed apps (see `shell/src/compositor/selection.rs`); a
/// `ui` app there goes through [`crate::clipboard::WaylandClipboard`] and
/// `wl_data_device` instead. Construction is skipped rather than attempted
/// on Wayland because arboard's probe there is slow, and a handle that
/// fails on every call is worse than no handle.
fn arboard_clipboard() -> Option<&'static Mutex<arboard::Clipboard>> {
    static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| {
        if running_on_wayland() {
            return None;
        }
        arboard::Clipboard::new().ok().map(Mutex::new)
    })
    .as_ref()
}

/// Whether this process is a Wayland client.
fn running_on_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

/// In-process clipboard fallback, used only when neither an installed backend
/// nor arboard can serve the request — a unit test, a headless scenario run,
/// or a session with no clipboard owner at all.
///
/// Keeps copy → paste round-tripping **within the process** so the shared
/// caret/clipboard core stays testable headlessly. A silently-working
/// fallback can mask a genuinely dead clipboard, so every fall-through here
/// logs once via [`crate::clipboard::warn_fallback_once`].
fn clipboard_fallback() -> &'static Mutex<(Option<String>, Option<String>)> {
    static FB: OnceLock<Mutex<(Option<String>, Option<String>)>> = OnceLock::new();
    FB.get_or_init(|| Mutex::new((None, None)))
}

/// Write `text` to `target`, trying each backend in priority order.
fn write_selection(target: ClipboardTarget, text: &str) {
    if let Some(backend) = clipboard_backend() {
        if backend.set_text(target, text) {
            return;
        }
    }
    if let Some(cb) = arboard_clipboard() {
        if let Ok(mut handle) = cb.lock() {
            let wrote = match target {
                ClipboardTarget::Clipboard => handle.set_text(text.to_string()).is_ok(),
                #[cfg(target_os = "linux")]
                ClipboardTarget::Primary => {
                    use arboard::{LinuxClipboardKind, SetExtLinux};
                    handle
                        .set()
                        .clipboard(LinuxClipboardKind::Primary)
                        .text(text.to_string())
                        .is_ok()
                }
                // No primary selection outside Linux; the regular clipboard is
                // the closest honest equivalent.
                #[cfg(not(target_os = "linux"))]
                ClipboardTarget::Primary => handle.set_text(text.to_string()).is_ok(),
            };
            if wrote {
                return;
            }
        }
    }
    warn_fallback_once("copy");
    if let Ok(mut fb) = clipboard_fallback().lock() {
        match target {
            ClipboardTarget::Clipboard => fb.0 = Some(text.to_string()),
            ClipboardTarget::Primary => fb.1 = Some(text.to_string()),
        }
    }
}

/// Read `target`, trying each backend in priority order.
fn read_selection(target: ClipboardTarget) -> Option<String> {
    if let Some(backend) = clipboard_backend() {
        return backend.text(target);
    }
    if let Some(cb) = arboard_clipboard() {
        if let Ok(mut handle) = cb.lock() {
            return match target {
                ClipboardTarget::Clipboard => handle.get_text().ok(),
                #[cfg(target_os = "linux")]
                ClipboardTarget::Primary => {
                    use arboard::{GetExtLinux, LinuxClipboardKind};
                    handle
                        .get()
                        .clipboard(LinuxClipboardKind::Primary)
                        .text()
                        .ok()
                }
                #[cfg(not(target_os = "linux"))]
                ClipboardTarget::Primary => handle.get_text().ok(),
            };
        }
    }
    warn_fallback_once("paste");
    let fb = clipboard_fallback().lock().ok()?;
    match target {
        ClipboardTarget::Clipboard => fb.0.clone(),
        ClipboardTarget::Primary => fb.1.clone(),
    }
}

/// Push `text` onto the system clipboard — the Ctrl+C buffer.
///
/// Routes to whichever backend this process resolved to; see
/// [`crate::clipboard`] for the order. Falls back to an in-process buffer only
/// when there is no clipboard at all, and says so in the log when it does.
pub fn clipboard_copy(text: &str) {
    write_selection(ClipboardTarget::Clipboard, text);
}

/// Push an RGBA8 image (`width * height * 4` bytes, row-major, no padding) onto
/// the system clipboard as image data, so it can be pasted into an image editor
/// or chat. Returns `false` when the buffer size is wrong or no backend can
/// carry an image. Used by the PDF reader to copy a selected page region.
pub fn clipboard_copy_image(width: usize, height: usize, rgba: &[u8]) -> bool {
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        return false;
    }
    if let Some(backend) = clipboard_backend() {
        return backend.set_image(width, height, rgba);
    }
    let Some(cb) = arboard_clipboard() else {
        return false;
    };
    let Ok(mut handle) = cb.lock() else {
        return false;
    };
    let img = arboard::ImageData {
        width,
        height,
        bytes: std::borrow::Cow::Borrowed(rgba),
    };
    handle.set_image(img).is_ok()
}

/// Pull the current clipboard contents as a UTF-8 string. Returns
/// `None` when the clipboard is empty / unavailable / non-text.
pub fn clipboard_paste() -> Option<String> {
    read_selection(ClipboardTarget::Clipboard)
}

/// Push `text` onto the **primary selection** — the mouse-driven "select then
/// middle-click paste" buffer, distinct from the Ctrl+C clipboard. Terminals
/// and editors set this whenever a range is selected so a middle-click paste
/// in any app receives it. Goes through the same backend chain as the
/// clipboard, including the in-process fallback.
pub fn clipboard_copy_primary(text: &str) {
    write_selection(ClipboardTarget::Primary, text);
}

/// Read the **primary selection** as a UTF-8 string — what a middle-click
/// paste should insert. `None` when empty or unavailable.
pub fn clipboard_paste_primary() -> Option<String> {
    read_selection(ClipboardTarget::Primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy → paste must round-trip within the process even with no system
    /// clipboard (the headless test env), and equally when a real clipboard
    /// is present.
    #[test]
    fn clipboard_round_trips_in_process() {
        let payload = "https://example.com/feed.xml";
        clipboard_copy(payload);
        assert_eq!(
            clipboard_paste().as_deref(),
            Some(payload),
            "copy then paste must return the copied text (system clipboard or in-process fallback)",
        );
    }

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

    /// No slicing into the middle of a code point ("café" — é is 2 bytes).
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
        assert_eq!(next_char_boundary(s, 0), 1);
        assert_eq!(next_char_boundary(s, 1), 3);
        assert_eq!(next_char_boundary(s, 3), 6);
        assert_eq!(next_char_boundary(s, 6), 7);
        assert_eq!(next_char_boundary(s, 7), 7);
        assert_eq!(prev_char_boundary(s, 7), 6);
        assert_eq!(prev_char_boundary(s, 6), 3);
        assert_eq!(prev_char_boundary(s, 3), 1);
        assert_eq!(prev_char_boundary(s, 1), 0);
        assert_eq!(prev_char_boundary(s, 0), 0);
    }

    #[test]
    fn boundaries_at_edges() {
        let s = "abc";
        assert_eq!(prev_word_boundary(s, s.len()), 0);
        assert_eq!(next_word_boundary(s, s.len()), s.len());
        assert_eq!(prev_word_boundary(s, 0), 0);
        assert_eq!(next_word_boundary(s, 0), s.len());
        assert_eq!(prev_word_boundary("", 0), 0);
        assert_eq!(next_word_boundary("", 0), 0);
    }

    #[test]
    fn char_word_boundaries_snap_to_the_word() {
        let s = "alpha beta gamma";
        // A char index inside "beta" (chars 6..10) snaps to the word's char span.
        assert_eq!(word_boundaries_chars(s, 7), (6, 10));
        // Forward walker lands at the *start* of the next word (standard word-wise
        // motion); back walker lands at the start of the current/previous word.
        assert_eq!(next_word_boundary_chars(s, 0), 6); // start of "beta"
        assert_eq!(next_word_boundary_chars(s, 6), 11); // start of "gamma"
        assert_eq!(prev_word_boundary_chars(s, 10), 6); // start of "beta"
    }

    #[test]
    fn extend_span_drag_contains_anchor_and_grows_by_whole_units() {
        // A double-click snapped the anchor to the word "beta" at bytes 6..10.
        let (lo, hi) = (6usize, 10usize);
        // Dragging right into "gamma" (11..16): anchor pins to the word's left
        // edge, cursor walks to the right edge of the word under the pointer.
        assert_eq!(extend_span_drag(lo, hi, 13, 11, 16), (6, 16));
        // Dragging back left of the original word, into "alpha" (0..5): anchor
        // flips to the original word's right edge, cursor to the left word's start.
        assert_eq!(extend_span_drag(lo, hi, 2, 0, 5), (10, 0));
        // Still inside the original word: the whole word stays selected.
        assert_eq!(extend_span_drag(lo, hi, 8, 6, 10), (6, 10));
    }

    #[test]
    fn char_boundaries_are_utf8_clean() {
        // Multibyte chars: indices returned are *char* indices, not bytes.
        let s = "café złto";
        // "café" is chars 0..4 ('c','a','f','é'); index 2 lands inside it.
        assert_eq!(word_boundaries_chars(s, 2), (0, 4));
        // The next word "złto" is chars 5..9.
        assert_eq!(word_boundaries_chars(s, 6), (5, 9));
    }
}
