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
//!   primary-selection pair, resolved against the clipboard of the current
//!   [`crate::host::HostServices`] (see [`crate::clipboard`]): the
//!   compositor's mirror for in-process chrome, the desktop clipboard for a
//!   native app, or an in-process buffer.
//!
//! Word operations follow Unicode Standard Annex #29, so apostrophes,
//! combining marks, non-Latin scripts, and Unicode numeric forms behave as
//! coherent words instead of being reduced to an ASCII-style predicate.

use std::ops::Range;

use crate::clipboard::ClipboardTarget;
use crate::text_break::grapheme_indices;

/// Which shaped run owns a caret at a shared logical byte boundary.
///
/// Most offsets have only one visual position, so callers rarely need to set
/// this explicitly. Mixed-direction text and soft wraps can expose both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAffinity {
    /// Associate the caret with the text ending at this byte.
    #[default]
    Before,
    /// Associate the caret with the text beginning at this byte.
    After,
}

/// A document byte offset plus the visual side of an ambiguous boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextPosition {
    pub byte: usize,
    pub affinity: TextAffinity,
}

impl TextPosition {
    /// The conventional position for APIs that only have a byte offset.
    pub const fn new(byte: usize) -> Self {
        Self {
            byte,
            affinity: TextAffinity::Before,
        }
    }

    pub const fn with_affinity(byte: usize, affinity: TextAffinity) -> Self {
        Self { byte, affinity }
    }
}

impl From<usize> for TextPosition {
    fn from(byte: usize) -> Self {
        Self::new(byte)
    }
}

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
        Self {
            anchor: pos,
            cursor: pos,
        }
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

use crate::text_break::unicode_word_ranges;

/// Walk back to the previous UAX #29 word start. If the cursor is inside a
/// word this returns that word's start; at its start it returns the preceding
/// word's start. The result is always a UTF-8 boundary.
pub fn prev_word_boundary(text: &str, byte_idx: usize) -> usize {
    let byte = text.floor_char_boundary(byte_idx);
    unicode_word_ranges(text)
        .rev()
        .find(|range| range.start < byte)
        .map_or(0, |range| range.start)
}

/// Mirror of [`prev_word_boundary`] for forward movement: the next UAX #29
/// word start, or the end of the string.
pub fn next_word_boundary(text: &str, byte_idx: usize) -> usize {
    let byte = text.ceil_char_boundary(byte_idx);
    unicode_word_ranges(text)
        .find(|range| range.start > byte)
        .map_or(text.len(), |range| range.start)
}

/// Byte range `(start, end)` of the UAX #29 word containing `byte_idx`, or the
/// word immediately on the left when the index lands in following punctuation
/// or whitespace. Used by double-click word selection.
///
/// Words never cross a line feed (UAX #29 breaks on both sides of one), so
/// only the index's own line is segmented; earlier lines are segmented, one
/// at a time, only when this line has no word up to the index.
pub fn word_boundaries(text: &str, byte_idx: usize) -> (usize, usize) {
    let byte = text.floor_char_boundary(byte_idx);
    let (line_start, line_end) = line_boundaries(text, byte);
    let mut previous = None;
    for range in unicode_word_ranges(text.get(line_start..line_end).unwrap_or_default()) {
        let range = range.start.saturating_add(line_start)..range.end.saturating_add(line_start);
        if range.start <= byte && byte <= range.end {
            return (range.start, range.end);
        }
        if range.end <= byte {
            previous = Some(range);
        } else {
            break;
        }
    }
    if let Some(range) = previous {
        return (range.start, range.end);
    }
    // The last word of the nearest earlier line that has one.
    let mut end = line_start;
    while let Some(line_end) = end.checked_sub(1) {
        let line = text.get(..line_end).unwrap_or_default();
        let start = line.rfind('\n').map_or(0, |at| at.saturating_add(1));
        let words = unicode_word_ranges(line.get(start..).unwrap_or_default());
        if let Some(range) = words.last() {
            return (
                range.start.saturating_add(start),
                range.end.saturating_add(start),
            );
        }
        end = start;
    }
    (byte, byte)
}

/// Byte range `(start, end)` of the *line* containing `byte_idx`, where
/// lines are linefeed-delimited (`\n`). The end is the index of the
/// trailing `\n`, or `text.len()` for the final line. Triple-click line
/// selection uses this.
pub fn line_boundaries(text: &str, byte_idx: usize) -> (usize, usize) {
    let byte_idx = byte_idx.min(text.len());
    let start = text
        .get(..byte_idx)
        .and_then(|s| s.rfind('\n'))
        .map_or(0, |i| i.saturating_add(1));
    let end = text
        .get(byte_idx..)
        .and_then(|s| s.find('\n'))
        .map_or(text.len(), |i| byte_idx.saturating_add(i));
    (start, end)
}

/// The nearest extended-grapheme boundary at or before `byte_idx`.
///
/// This is the safe place to clamp carets after remote edits: a UTF-8 scalar
/// boundary may still sit inside a combining sequence or emoji ZWJ cluster.
pub fn grapheme_boundary_at_or_before(text: &str, byte_idx: usize) -> usize {
    if byte_idx >= text.len() {
        return text.len();
    }
    let byte_idx = byte_idx.min(text.len());
    grapheme_indices(text)
        .map(|(i, _)| i)
        .take_while(|&i| i <= byte_idx)
        .last()
        .unwrap_or(0)
}

/// Step one user-perceived character to the left of `byte_idx`. Extended
/// grapheme clusters keep combining marks, emoji modifiers, flags, and ZWJ
/// sequences together. The legacy function name is retained for API
/// compatibility.
pub fn prev_char_boundary(text: &str, byte_idx: usize) -> usize {
    let byte_idx = byte_idx.min(text.len());
    grapheme_indices(text)
        .map(|(i, _)| i)
        .take_while(|&i| i < byte_idx)
        .last()
        .unwrap_or(0)
}

/// Step one user-perceived character to the right of `byte_idx`. See
/// [`prev_char_boundary`] for the compatibility note.
pub fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
    let byte_idx = byte_idx.min(text.len());
    grapheme_indices(text)
        .map(|(i, _)| i)
        .find(|&i| i > byte_idx)
        .unwrap_or(text.len())
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
        .map_or(text.len(), |(b, _)| b)
}

fn byte_to_char(text: &str, byte_idx: usize) -> usize {
    text.get(..byte_idx.min(text.len()))
        .map_or(0, |s| s.chars().count())
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

fn clipboard() -> std::sync::Arc<dyn crate::clipboard::ClipboardBackend> {
    crate::host::current().clipboard
}

pub(crate) fn request_clipboard_paste() -> std::sync::mpsc::Receiver<Option<String>> {
    clipboard().request_text(ClipboardTarget::Clipboard)
}

/// Push `text` onto the system clipboard — the Ctrl+C buffer.
///
/// Routes to the current host's clipboard; see [`crate::clipboard`]. Without
/// a system clipboard the copy stays inside this process, and the native host
/// says so in the log when it does.
pub fn clipboard_copy(text: &str) {
    clipboard().set_text(ClipboardTarget::Clipboard, text);
}

/// [`clipboard_copy`] that reports whether the host's clipboard took the
/// text, including the in-process fallback (which pastes inside this
/// process). Cut uses it so a failed copy never deletes the selection.
#[must_use]
pub(crate) fn clipboard_copy_accepted(text: &str) -> bool {
    clipboard().set_text(ClipboardTarget::Clipboard, text)
}

/// Copy text only if a system clipboard backend accepts it.
///
/// Returns `false` on failure and never retains the text in an in-process
/// buffer. Use this for secrets and success messages that require an actual
/// clipboard write rather than an editor-local copy.
#[must_use]
pub fn try_clipboard_copy(text: &str) -> bool {
    clipboard().set_shared_text(ClipboardTarget::Clipboard, text)
}

/// Copy a secret — a password, a one-time code, a recovery key, an enrolment
/// token — so it pastes anywhere as plain text while clipboard histories and
/// device sync are told not to keep it
/// ([`ClipboardBackend::set_secret_text`](crate::clipboard::ClipboardBackend::set_secret_text)).
///
/// After `clear_after` the clipboard is cleared if it still holds this
/// secret; something copied since is left alone. A browser page has no
/// thread to wait on, so there the clipboard is not cleared.
///
/// Returns `false` when no shared clipboard took the secret: it is never kept
/// in a buffer private to this process.
#[must_use]
pub fn try_clipboard_copy_secret(text: &str, clear_after: std::time::Duration) -> bool {
    let clipboard = clipboard();
    if !clipboard.set_secret_text(text) {
        return false;
    }
    let lease = SecretLease {
        value: zeroize::Zeroizing::new(text.to_owned()),
    };
    // The clear goes to the clipboard that took the copy, whichever host is
    // current on that thread later.
    let _ = crate::rt::spawn_thread("clipboard-secret-clear", move || {
        if crate::rt::sleep_thread(clear_after).is_ok() {
            lease.clear_if_unchanged(&*clipboard);
        }
    });
    true
}

/// A secret this process put on the clipboard, held only to recognise it.
struct SecretLease {
    value: zeroize::Zeroizing<String>,
}

impl SecretLease {
    /// Clear the clipboard if it still holds the secret, so a later copy is
    /// never destroyed.
    fn clear_if_unchanged(&self, clipboard: &dyn crate::clipboard::ClipboardBackend) {
        let current = clipboard
            .text(ClipboardTarget::Clipboard)
            .map(zeroize::Zeroizing::new);
        if current.is_some_and(|current| current.as_bytes() == self.value.as_bytes()) {
            // An empty secret, so the clear itself stays out of history.
            let _ = clipboard.set_secret_text("");
        }
    }
}

/// Push an RGBA8 image (`width * height * 4` bytes, row-major, no padding) onto
/// the system clipboard as image data, so it can be pasted into an image editor
/// or chat. Returns `false` when the buffer size is wrong or no backend can
/// carry an image. Used by the PDF reader to copy a selected page region.
pub fn clipboard_copy_image(width: usize, height: usize, rgba: &[u8]) -> bool {
    let Some(expected_len) = width
        .checked_mul(height)
        .and_then(|area| area.checked_mul(4))
    else {
        return false;
    };
    if width == 0 || height == 0 || rgba.len() != expected_len {
        return false;
    }
    clipboard().set_image(width, height, rgba)
}

/// Pull the current clipboard contents as a UTF-8 string. Returns
/// `None` when the clipboard is empty / unavailable / non-text.
pub fn clipboard_paste() -> Option<String> {
    clipboard()
        .text(ClipboardTarget::Clipboard)
        .filter(|text| !text.is_empty())
}

/// The image on the clipboard, when another app copied one (a screenshot,
/// an image from a viewer or a browser). `None` when it holds no image.
pub fn clipboard_paste_image() -> Option<crate::clipboard::ClipboardImage> {
    clipboard().image()
}

/// Push `text` onto the **primary selection** — the mouse-driven "select then
/// middle-click paste" buffer, distinct from the Ctrl+C clipboard. Terminals
/// and editors set this whenever a range is selected so a middle-click paste
/// in any app receives it. Goes through the same clipboard as
/// [`clipboard_copy`].
pub fn clipboard_copy_primary(text: &str) {
    clipboard().set_text(ClipboardTarget::Primary, text);
}

/// Read the **primary selection** as a UTF-8 string — what a middle-click
/// paste should insert. `None` when empty or unavailable.
pub fn clipboard_paste_primary() -> Option<String> {
    clipboard().text(ClipboardTarget::Primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `word_boundaries` over the whole text, as it was before it segmented
    /// one line.
    fn whole_text_word_boundaries(text: &str, byte_idx: usize) -> (usize, usize) {
        let byte = text.floor_char_boundary(byte_idx);
        let mut previous = None;
        for range in unicode_word_ranges(text) {
            if range.start <= byte && byte <= range.end {
                return (range.start, range.end);
            }
            if range.end <= byte {
                previous = Some(range);
            } else {
                break;
            }
        }
        previous.map_or((byte, byte), |range| (range.start, range.end))
    }

    #[test]
    fn line_local_word_boundaries_match_whole_text_segmentation() {
        for text in [
            "",
            "\n",
            "hello",
            "hello\n\n  \nworld.",
            "first line, done.\n\n\n   ... \n(second) line\n",
            "שלום עולם\nمرحبا بالعالم!\n\nภาษาไทยไม่มีช่องว่าง\n日本語の文章。\n",
            "emoji 👩‍💻 then e\u{301}\n  — \n can't stop\r\nnow",
            "...\n!!\n",
        ] {
            for byte in 0..=text.len() + 1 {
                assert_eq!(
                    word_boundaries(text, byte),
                    whole_text_word_boundaries(text, byte),
                    "{text:?} at {byte}"
                );
            }
        }
    }

    #[test]
    fn empty_clipboard_is_unavailable_but_whitespace_is_content() {
        let (services, _) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        clipboard_copy("");
        assert_eq!(clipboard_paste(), None);
        clipboard_copy(" \n");
        assert_eq!(clipboard_paste().as_deref(), Some(" \n"));
    }

    #[test]
    fn a_secret_copy_is_cleared_only_while_the_clipboard_still_holds_it() {
        let (services, recorder) = crate::host::HostServices::recording();
        let clipboard = services.clipboard.clone();
        let _host = crate::host::enter(&services);
        assert!(try_clipboard_copy_secret(
            "hunter2",
            std::time::Duration::from_secs(3600)
        ));
        assert_eq!(recorder.secret_copies(), ["hunter2"]);
        assert!(
            recorder.copies().is_empty(),
            "a secret is never written as an ordinary copy"
        );
        assert_eq!(clipboard_paste().as_deref(), Some("hunter2"));

        let lease = SecretLease {
            value: zeroize::Zeroizing::new("hunter2".to_owned()),
        };
        clipboard_copy("copied since");
        lease.clear_if_unchanged(&*clipboard);
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("copied since"),
            "a later copy must survive the secret's timeout"
        );
        assert!(clipboard.set_secret_text("hunter2"));
        lease.clear_if_unchanged(&*clipboard);
        assert_eq!(clipboard_paste(), None, "the unchanged secret is cleared");
        assert_eq!(recorder.secret_copies(), ["hunter2", "hunter2", ""]);
    }

    #[test]
    fn a_secret_copy_is_refused_without_a_shared_clipboard_and_cleared_on_time() {
        let services = crate::host::HostServices::null();
        {
            let _host = crate::host::enter(&services);
            assert!(
                !try_clipboard_copy_secret("hunter2", std::time::Duration::ZERO),
                "an in-process buffer must not claim a secret copy"
            );
            assert_eq!(clipboard_paste(), None);
        }
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        assert!(try_clipboard_copy_secret(
            "hunter2",
            std::time::Duration::ZERO
        ));
        let deadline = tilo_test_env::hang_deadline();
        while recorder.secret_copies().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the expired secret was never cleared"
            );
            std::thread::yield_now();
        }
        assert_eq!(recorder.secret_copies(), ["hunter2", ""]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_primary_selection_does_not_replace_explicit_copy() {
        clipboard_copy("explicit Windows copy");
        clipboard_copy_primary("selected terminal text");
        assert_eq!(
            clipboard_paste_primary().as_deref(),
            Some("selected terminal text")
        );
        assert_eq!(clipboard_paste().as_deref(), Some("explicit Windows copy"));
    }

    #[test]
    fn clipboard_and_selection_helpers_move_extend_and_round_trip() {
        // Case `clipboard_round_trips_in_process`:
        // Copy → paste must round-trip within the process even with no system
        // clipboard (the headless test env), and equally when a real clipboard
        // is present.
        {
            let payload = "https://example.com/feed.xml";
            clipboard_copy(payload);
            assert_eq!(
                clipboard_paste().as_deref(),
                Some(payload),
                "copy then paste must return the copied text (system clipboard or in-process fallback)",
            );
        }

        // Case `selection_helpers_min_max_range_collapsed`.
        {
            let s = Selection {
                anchor: 3,
                cursor: 7,
            };
            assert_eq!(s.min(), 3);
            assert_eq!(s.max(), 7);
            assert_eq!(s.range(), 3..7);
            assert!(!s.is_collapsed());

            let s2 = Selection {
                anchor: 9,
                cursor: 4,
            };
            assert_eq!(s2.min(), 4);
            assert_eq!(s2.max(), 9);
            assert_eq!(s2.range(), 4..9);

            let collapsed = Selection::collapsed(5);
            assert!(collapsed.is_collapsed());
            assert_eq!(collapsed.range(), 5..5);
        }

        // Case `selection_move_and_extend`.
        {
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

        // Case `extend_span_drag_contains_anchor_and_grows_by_whole_units`.
        {
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
    }

    #[test]
    fn word_boundaries_follow_unicode_words_and_dictionaries() {
        // Case `prev_word_boundary_walks_word_starts`.
        {
            let s = "hello world quick ui";
            assert_eq!(prev_word_boundary(s, s.len()), 18);
            assert_eq!(prev_word_boundary(s, 18), 12);
            assert_eq!(prev_word_boundary(s, 12), 6);
            assert_eq!(prev_word_boundary(s, 6), 0);
            assert_eq!(prev_word_boundary(s, 0), 0);
        }

        // Case `next_word_boundary_walks_word_ends`.
        {
            let s = "hello world quick ui";
            assert_eq!(next_word_boundary(s, 0), 6);
            assert_eq!(next_word_boundary(s, 6), 12);
            assert_eq!(next_word_boundary(s, 12), 18);
            assert_eq!(next_word_boundary(s, 18), s.len());
            assert_eq!(next_word_boundary(s, s.len()), s.len());
        }

        // Case `word_boundaries_snaps_to_word_bounds_ascii`.
        {
            let s = "hello world";
            assert_eq!(word_boundaries(s, 2), (0, 5));
            assert_eq!(word_boundaries(s, 0), (0, 5));
            // At the space (idx 5), the left edge is the start of the previous
            // word body — anchor snaps left.
            assert_eq!(word_boundaries(s, 5), (0, 5));
            assert_eq!(word_boundaries(s, 6), (6, 11));
            assert_eq!(word_boundaries(s, 11), (6, 11));
        }

        // Case `word_boundaries_utf8_clean`:
        // No slicing into the middle of a code point ("café" — é is 2 bytes).
        {
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

        // Case `unicode_words_keep_apostrophes_and_decimal_numbers_together`.
        {
            let text = "can't pay 32.3 now";
            assert_eq!(word_boundaries(text, 3), (0, "can't".len()));
            let number_start = text.find("32.3").unwrap();
            assert_eq!(
                word_boundaries(text, number_start + 2),
                (number_start, number_start + "32.3".len())
            );
            assert_eq!(next_word_boundary(text, 0), "can't ".len());
        }

        // Case `unicode_words_keep_combining_marks_with_their_base`.
        {
            let text = "nai\u{308}ve café";
            let first_end = text.find(' ').unwrap();
            assert_eq!(word_boundaries(text, 4), (0, first_end));
            assert_eq!(prev_word_boundary(text, first_end), 0);
        }

        // Case `dictionary_word_motion_segments_thai_without_spaces`.
        {
            let text = "ทุกสองสัปดาห์";
            assert_eq!(next_word_boundary(text, 0), 9);
            assert_eq!(next_word_boundary(text, 9), 18);
            assert_eq!(prev_word_boundary(text, text.len()), 18);
            assert_eq!(word_boundaries(text, 12), (9, 18));
        }

        // Case `word_boundaries_empty_string`.
        {
            assert_eq!(word_boundaries("", 0), (0, 0));
        }

        // Case `char_word_boundaries_snap_to_the_word`.
        {
            let s = "alpha beta gamma";
            // A char index inside "beta" (chars 6..10) snaps to the word's char span.
            assert_eq!(word_boundaries_chars(s, 7), (6, 10));
            // Forward walker lands at the *start* of the next word (standard word-wise
            // motion); back walker lands at the start of the current/previous word.
            assert_eq!(next_word_boundary_chars(s, 0), 6); // start of "beta"
            assert_eq!(next_word_boundary_chars(s, 6), 11); // start of "gamma"
            assert_eq!(prev_word_boundary_chars(s, 10), 6); // start of "beta"
        }
    }

    #[test]
    fn character_and_line_boundaries_are_utf8_and_grapheme_clean() {
        // Case `line_boundaries_basic`.
        {
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

        // Case `line_boundaries_empty_string`.
        {
            assert_eq!(line_boundaries("", 0), (0, 0));
        }

        // Case `line_boundaries_only_newlines`.
        {
            let s = "\n\n";
            // Before first \n: empty line, (0,0).
            assert_eq!(line_boundaries(s, 0), (0, 0));
            // Right after first \n: empty middle line, (1,1).
            assert_eq!(line_boundaries(s, 1), (1, 1));
            // After last \n: empty final line, (2,2).
            assert_eq!(line_boundaries(s, 2), (2, 2));
        }

        // Case `prev_next_char_boundary_utf8`.
        {
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

        // Case `character_motion_keeps_extended_graphemes_whole`.
        {
            let combining = "a\u{301}b";
            let first_cluster_end = "a\u{301}".len();
            assert_eq!(next_char_boundary(combining, 0), first_cluster_end);
            assert_eq!(prev_char_boundary(combining, first_cluster_end), 0);

            let family = "👩‍👩‍👧‍👦!";
            let emoji_end = family.len() - 1;
            assert_eq!(next_char_boundary(family, 0), emoji_end);
            assert_eq!(prev_char_boundary(family, emoji_end), 0);
            assert_eq!(
                grapheme_boundary_at_or_before(family, family.len()),
                family.len()
            );
            assert_eq!(grapheme_boundary_at_or_before(family, 4), 0);
        }

        // Case `boundaries_at_edges`.
        {
            let s = "abc";
            assert_eq!(prev_word_boundary(s, s.len()), 0);
            assert_eq!(next_word_boundary(s, s.len()), s.len());
            assert_eq!(prev_word_boundary(s, 0), 0);
            assert_eq!(next_word_boundary(s, 0), s.len());
            assert_eq!(prev_word_boundary("", 0), 0);
            assert_eq!(next_word_boundary("", 0), 0);
        }

        // Case `char_boundaries_are_utf8_clean`.
        {
            // Multibyte chars: indices returned are *char* indices, not bytes.
            let s = "café złto";
            // "café" is chars 0..4 ('c','a','f','é'); index 2 lands inside it.
            assert_eq!(word_boundaries_chars(s, 2), (0, 4));
            // The next word "złto" is chars 5..9.
            assert_eq!(word_boundaries_chars(s, 6), (5, 9));
        }
    }
}
