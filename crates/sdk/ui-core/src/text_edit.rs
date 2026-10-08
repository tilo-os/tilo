//! `EditCore` — the shared headless text-editing model.
//!
//! Caret + selection state (byte offsets), the in-progress IME composition,
//! an optional per-character admission filter, and the motion / edit
//! vocabulary every editable surface needs, built on the shared
//! [`text_selection`](crate::text_selection) boundary walkers. Edits come back
//! as byte-space [`Edit`] splices the host applies to its mirror
//! ([`Edit::apply`]) and its Automerge `Text` ([`Edit::splice_automerge`],
//! char-addressed). It is deliberately UI-free: no rendering, no CRDT handle.
//!
//! Every text field uses it: [`TextInput`] (and through it every form field,
//! `RichTextLine` and `BlockEditor`), the transient query fields of palettes
//! and menus ([`QueryComposition`]), [`NameField`], and the app-specific
//! editors. Laid-out geometry — visual/bidi arrow motion, wrapped
//! Up/Down, Home/End of a soft-wrapped row — stays in the widget, which feeds
//! the resolved position back through [`EditCore::motion_to`] or
//! [`EditCore::place_position`]; the core owns everything logical, which is
//! exactly what unit tests can pin without a GPU.
//!
//! [`TypingRun`] notes when the user last typed, so a word being typed is
//! not checked before it settles.
//!
//! [`NameField`]: crate::widget::name_field::NameField
//! [`QueryComposition`]: crate::query_text::QueryComposition
//! [`TextInput`]: crate::widget::TextInput

use std::borrow::Cow;
use std::ops::Range;

use automerge::{ObjId, transaction::Transactable};
use zeroize::Zeroize;

use crate::text_selection::{
    Selection, TextAffinity, TextPosition, grapheme_boundary_at_or_before, line_boundaries,
    next_char_boundary, next_word_boundary, prev_char_boundary, prev_word_boundary,
    word_boundaries,
};
use crate::time::{Duration, Instant};

/// A per-character admission rule (for example a spreadsheet cell-reference
/// box that only takes `[A-Za-z0-9$:]`). Applied to every insertion path:
/// typing, paste, IME preedit and commit, programmatic insertion.
pub type CharFilter = fn(char) -> bool;

/// A caret motion. Vertical motion across wrapped lines is the host's job
/// because it needs laid-out line geometry; [`Motion::LineUp`] and
/// [`Motion::LineDown`] move across `\n`-delimited lines only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// One user-perceived character (extended grapheme cluster) left / right.
    Left,
    Right,
    /// One word left / right (Ctrl+Arrow).
    WordLeft,
    WordRight,
    /// Start / end of the `\n`-delimited line (Home / End).
    LineStart,
    LineEnd,
    /// Start / end of the whole buffer.
    DocStart,
    DocEnd,
    /// The `\n`-delimited line above / below, at the same character column
    /// or the end of a shorter line; unchanged on the first / last line.
    LineUp,
    LineDown,
}

/// The byte offset in the `\n`-delimited line above (`up`) or below the one
/// holding `byte`, at the same character column, clamped to that line's end
/// and pulled back onto the start of a grapheme cluster it would split.
fn line_step(text: &str, byte: usize, up: bool) -> usize {
    let (start, end) = line_boundaries(text, byte);
    let column = text.get(start..byte).map_or(0, |line| line.chars().count());
    let (target_start, target_end) = if up {
        if start == 0 {
            return byte;
        }
        line_boundaries(text, start.saturating_sub(1))
    } else {
        if end >= text.len() {
            return byte;
        }
        line_boundaries(text, end.saturating_add(1))
    };
    let line = text.get(target_start..target_end).unwrap_or_default();
    let target = target_start.saturating_add(
        line.char_indices()
            .nth(column)
            .map_or(line.len(), |(offset, _)| offset),
    );
    grapheme_boundary_at_or_before(text, target)
}

/// A splice to apply to the buffer, in **byte** offsets over the pre-edit text:
/// `at..at+removed` is replaced by `inserted`. [`Edit::apply`] applies it to a
/// `String` mirror; [`Edit::splice_automerge`] to a char-addressed Automerge
/// `Text`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Edit {
    pub at: usize,
    pub removed: usize,
    pub inserted: String,
}

/// An [`Edit`] in Automerge's **char** offsets: delete `removed` chars at
/// char `at`, then insert `inserted` chars there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharSplice {
    pub at: usize,
    pub removed: usize,
    /// Chars the edit inserts, so a caller can mark the inserted run.
    pub inserted: usize,
}

impl CharSplice {
    /// `removed` as the signed count `splice_text` takes. A single edit is
    /// bounded by the buffer length, nowhere near `isize::MAX`.
    pub fn removed_isize(&self) -> isize {
        isize::try_from(self.removed).unwrap_or(isize::MAX)
    }
}

impl Edit {
    /// Whether nothing changed (no delete, no insert).
    pub fn is_noop(&self) -> bool {
        self.removed == 0 && self.inserted.is_empty()
    }
    /// New byte caret position after this edit is applied.
    pub fn caret_after(&self) -> usize {
        self.at.saturating_add(self.inserted.len())
    }
    /// The replaced byte range in the pre-edit text.
    pub fn range(&self) -> Range<usize> {
        self.at..self.at.saturating_add(self.removed)
    }

    /// The same splice in char offsets over `before` (the pre-edit text).
    /// `None` when the byte range does not land on `before`'s char
    /// boundaries — the edit was computed against different text.
    pub fn char_splice(&self, before: &str) -> Option<CharSplice> {
        let head = before.get(..self.at)?;
        let removed = before.get(self.range())?;
        Some(CharSplice {
            at: head.chars().count(),
            removed: removed.chars().count(),
            inserted: self.inserted.chars().count(),
        })
    }

    /// Apply the splice to `text`. Returns `false` (leaving `text` untouched)
    /// when the range does not land on char boundaries.
    pub fn apply(&self, text: &mut String) -> bool {
        if text.get(self.range()).is_none() {
            return false;
        }
        text.replace_range(self.range(), &self.inserted);
        true
    }

    /// Splice the edit into the Automerge `Text` `obj`, whose current content
    /// is `before`. Returns the char-space splice so a caller can stamp marks
    /// over the inserted run, or `None` when the edit does not fit `before`.
    pub fn splice_automerge<T: Transactable>(
        &self,
        doc: &mut T,
        obj: &ObjId,
        before: &str,
    ) -> Option<CharSplice> {
        let splice = self.char_splice(before)?;
        // A rejected splice (a stale object id) leaves the local mirror ahead
        // of the document until the next sync re-reads it, the same recovery
        // a remote change gets.
        let _ = doc.splice_text(obj, splice.at, splice.removed_isize(), &self.inserted);
        Some(splice)
    }
}

/// The caret + selection model, the IME composition in progress, and the
/// field's admission filter. Selection offsets are **bytes** into the bound
/// text (matching [`text_selection`](crate::text_selection) and cosmic-text's
/// byte-indexed layout).
///
/// The composition can be part of a secret being typed: [`Self::clear_preedit`]
/// zeroes it, and an owner of secret text calls it before dropping the core.
#[derive(Clone, Debug, Default)]
pub struct EditCore {
    sel: Selection,
    affinity: TextAffinity,
    /// Composition text, shown at the caret but never part of the value.
    preedit: String,
    /// The IME's own cursor inside `preedit` (byte range).
    preedit_cursor: Option<(usize, usize)>,
    filter: Option<CharFilter>,
}

impl EditCore {
    /// A collapsed caret at the start.
    pub fn new() -> Self {
        Self::default()
    }
    /// A collapsed caret at byte `pos`.
    pub fn at(pos: usize) -> Self {
        Self {
            sel: Selection::collapsed(pos),
            ..Self::default()
        }
    }

    // --- admission filter --------------------------------------------------

    /// Install (or clear) the per-character admission filter.
    pub fn set_filter(&mut self, filter: Option<CharFilter>) {
        self.filter = filter;
    }
    /// The installed admission filter.
    pub fn filter(&self) -> Option<CharFilter> {
        self.filter
    }
    /// `s` with every character the filter rejects removed.
    pub fn admit<'a>(&self, s: &'a str) -> Cow<'a, str> {
        match self.filter {
            Some(filter) if !s.chars().all(filter) => {
                Cow::Owned(s.chars().filter(|ch| filter(*ch)).collect())
            }
            _ => Cow::Borrowed(s),
        }
    }

    // --- caret + selection -------------------------------------------------

    /// The full selection (anchor + cursor).
    pub fn selection(&self) -> Selection {
        self.sel
    }
    /// Live caret byte offset.
    pub fn caret(&self) -> usize {
        self.sel.cursor
    }
    /// The selection anchor (equal to the caret when nothing is selected).
    pub fn anchor(&self) -> usize {
        self.sel.anchor
    }
    /// The caret's visual side at an ambiguous (bidi / soft-wrap) boundary.
    pub fn affinity(&self) -> TextAffinity {
        self.affinity
    }
    /// Live caret offset plus its visual side at an ambiguous boundary.
    pub fn caret_position(&self) -> TextPosition {
        TextPosition::with_affinity(self.sel.cursor, self.affinity)
    }
    /// Whether any characters are selected.
    pub fn has_selection(&self) -> bool {
        !self.sel.is_collapsed()
    }
    /// Sorted `[start, end)` byte range of the selection.
    pub fn range(&self) -> Range<usize> {
        self.sel.range()
    }
    /// The selected byte range, `None` when collapsed.
    pub fn selected_range(&self) -> Option<Range<usize>> {
        self.has_selection().then(|| self.sel.range())
    }
    /// The selected slice of `text`.
    pub fn selected<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.selected_range()?)
    }

    /// Move the caret to `byte`, collapsing the selection.
    pub fn set_caret(&mut self, byte: usize) {
        self.sel.move_to(byte);
        self.affinity = TextAffinity::Before;
    }
    /// Move to an exact shaped position, collapsing the selection.
    pub fn set_caret_position(&mut self, position: TextPosition) {
        self.sel.move_to(position.byte);
        self.affinity = position.affinity;
    }
    /// Set both endpoints explicitly.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.sel = Selection { anchor, cursor };
        self.affinity = TextAffinity::Before;
    }
    /// Set both endpoints and the caret's visual side.
    pub fn set_selection_position(&mut self, anchor: usize, cursor: TextPosition) {
        self.sel = Selection {
            anchor,
            cursor: cursor.byte,
        };
        self.affinity = cursor.affinity;
    }
    /// Collapse the selection onto the cursor.
    pub fn collapse(&mut self) {
        self.sel.collapse_to_cursor();
    }

    /// Clamp both endpoints into `[0, len]` and onto grapheme boundaries. Call after
    /// the bound text changed underneath the caret (a remote / external edit) so
    /// motion never slices a code point.
    pub fn clamp_to(&mut self, text: &str) {
        let clamp = |b: usize| grapheme_boundary_at_or_before(text, b);
        self.sel.anchor = clamp(self.sel.anchor);
        self.sel.cursor = clamp(self.sel.cursor);
    }

    /// Select the entire buffer.
    pub fn select_all(&mut self, text: &str) {
        self.set_selection(0, text.len());
    }

    fn target(&self, text: &str, m: Motion) -> usize {
        let c = self.sel.cursor.min(text.len());
        match m {
            Motion::Left => prev_char_boundary(text, c),
            Motion::Right => next_char_boundary(text, c),
            Motion::WordLeft => prev_word_boundary(text, c),
            Motion::WordRight => next_word_boundary(text, c),
            Motion::LineStart => line_boundaries(text, c).0,
            Motion::LineEnd => line_boundaries(text, c).1,
            Motion::DocStart => 0,
            Motion::DocEnd => text.len(),
            Motion::LineUp => line_step(text, c, true),
            Motion::LineDown => line_step(text, c, false),
        }
    }

    /// Apply a caret motion. `extend` keeps the anchor fixed (shift-select). A
    /// plain (non-extend) horizontal step with a live selection collapses to
    /// the near edge rather than stepping past it — the native editor
    /// behaviour.
    pub fn motion(&mut self, text: &str, m: Motion, extend: bool) {
        let target = self.target(text, m);
        self.motion_to(m, TextPosition::new(target), extend);
    }

    /// Apply motion `m` whose destination the host already resolved through
    /// laid-out geometry (visual bidi order, a soft-wrapped row edge). Same
    /// selection rules as [`Self::motion`].
    pub fn motion_to(&mut self, m: Motion, target: TextPosition, extend: bool) {
        let horizontal = matches!(
            m,
            Motion::Left | Motion::Right | Motion::WordLeft | Motion::WordRight
        );
        if !extend && horizontal && self.has_selection() {
            let edge = if matches!(m, Motion::Left | Motion::WordLeft) {
                self.sel.min()
            } else {
                self.sel.max()
            };
            self.set_caret(edge);
            return;
        }
        self.place_position(target, extend);
    }

    /// Seat the caret from a click at byte `byte`; `extend` (shift-click) keeps
    /// the anchor so the selection grows.
    pub fn place(&mut self, byte: usize, extend: bool) {
        self.place_position(TextPosition::new(byte), extend);
    }

    /// Seat the caret from an exact shaped position.
    pub fn place_position(&mut self, position: TextPosition, extend: bool) {
        if extend {
            self.sel.extend_to(position.byte);
        } else {
            self.sel.move_to(position.byte);
        }
        self.affinity = position.affinity;
    }

    /// Select the word under `byte` (double-click) and anchor to it.
    pub fn select_word(&mut self, text: &str, byte: usize) {
        let (s, e) = word_boundaries(text, byte);
        self.set_selection(s, e);
    }
    /// Select the `\n`-delimited line under `byte` (triple-click).
    pub fn select_line(&mut self, text: &str, byte: usize) {
        let (s, e) = line_boundaries(text, byte);
        self.set_selection(s, e);
    }

    // --- edits -------------------------------------------------------------

    /// Insert `s` through the admission filter, replacing any selection.
    /// Returns the byte-space [`Edit`] and moves the caret to just after the
    /// inserted text. An insert the filter empties (or an empty `s`) is a
    /// no-op that leaves the selection alone.
    pub fn insert(&mut self, s: &str) -> Edit {
        let admitted = self.admit(s);
        if admitted.is_empty() {
            return Edit::default();
        }
        let range = self.sel.range();
        let edit = Edit {
            at: range.start,
            removed: range.len(),
            inserted: admitted.into_owned(),
        };
        self.set_caret(edit.caret_after());
        edit
    }

    /// Replace `range` (bytes) with `s` through the admission filter, leaving
    /// the caret after the replacement — for edits that target a span other
    /// than the selection (autocorrect, a spelling suggestion).
    pub fn replace(&mut self, range: Range<usize>, s: &str) -> Edit {
        self.set_selection(range.start, range.end);
        let admitted = self.admit(s);
        let edit = Edit {
            at: range.start,
            removed: range.len(),
            inserted: admitted.into_owned(),
        };
        self.set_caret(edit.caret_after());
        edit
    }

    /// Delete the selection; a no-op when nothing is selected.
    pub fn delete_selection(&mut self) -> Edit {
        let range = self.sel.range();
        self.set_caret(range.start);
        Edit {
            at: range.start,
            removed: range.len(),
            inserted: String::new(),
        }
    }

    /// Backspace: delete the selection, else the grapheme (or word, with
    /// `word`) before the caret. Returns the resulting [`Edit`] (a no-op at
    /// the start).
    pub fn delete_backward(&mut self, text: &str, word: bool) -> Edit {
        if self.has_selection() {
            return self.delete_selection();
        }
        let c = self.sel.cursor.min(text.len());
        if c == 0 {
            return Edit::default();
        }
        let from = if word {
            prev_word_boundary(text, c)
        } else {
            prev_char_boundary(text, c)
        };
        self.set_caret(from);
        Edit {
            at: from,
            removed: c.saturating_sub(from),
            inserted: String::new(),
        }
    }

    /// Delete forward (Delete key): the selection, else the grapheme (or
    /// word) after the caret. The caret stays put. No-op at the end of the
    /// buffer.
    pub fn delete_forward(&mut self, text: &str, word: bool) -> Edit {
        if self.has_selection() {
            return self.delete_selection();
        }
        let c = self.sel.cursor.min(text.len());
        if c >= text.len() {
            return Edit::default();
        }
        let to = if word {
            next_word_boundary(text, c)
        } else {
            next_char_boundary(text, c)
        };
        self.set_caret(c);
        Edit {
            at: c,
            removed: to.saturating_sub(c),
            inserted: String::new(),
        }
    }

    // --- IME composition ---------------------------------------------------

    /// The composition in progress (empty when none).
    pub fn preedit(&self) -> &str {
        &self.preedit
    }
    /// Whether an IME composition is in progress.
    pub fn has_preedit(&self) -> bool {
        !self.preedit.is_empty()
    }
    /// The IME's cursor inside the composition, as it reported it.
    pub fn preedit_cursor(&self) -> Option<(usize, usize)> {
        self.preedit_cursor
    }

    /// Show `text` as the composition at the caret. The admission filter
    /// applies; when it drops characters the IME's cursor no longer addresses
    /// the shown text and falls back to the composition's end.
    pub fn set_preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        let admitted = self.admit(text);
        let cursor = if admitted.len() == text.len() {
            cursor
        } else {
            None
        };
        self.preedit.zeroize();
        self.preedit.push_str(&admitted);
        self.preedit_cursor = cursor;
    }

    /// Drop (and zero) the composition without committing it.
    pub fn clear_preedit(&mut self) {
        self.preedit.zeroize();
        self.preedit_cursor = None;
    }

    /// The IME committed `text`: the composition ends and `text` replaces the
    /// selection like typing would.
    pub fn commit(&mut self, text: &str) -> Edit {
        self.clear_preedit();
        self.insert(text)
    }

    /// The caret clamped into `text` and onto a char boundary — where the
    /// composition is shown.
    fn preedit_at(&self, text: &str) -> usize {
        text.floor_char_boundary(self.sel.cursor)
    }

    /// `text` with the composition shown at the caret.
    pub fn display_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.preedit.is_empty() {
            return Cow::Borrowed(text);
        }
        let (head, tail) = text.split_at(self.preedit_at(text));
        let mut display = String::with_capacity(text.len().saturating_add(self.preedit.len()));
        display.push_str(head);
        display.push_str(&self.preedit);
        display.push_str(tail);
        Cow::Owned(display)
    }

    /// The display byte range the composition occupies.
    pub fn preedit_display_range(&self, text: &str) -> Option<Range<usize>> {
        if self.preedit.is_empty() {
            return None;
        }
        let at = self.preedit_at(text);
        Some(at..at.saturating_add(self.preedit.len()))
    }

    /// A committed-text byte mapped into the display text: offsets after the
    /// caret shift past the composition.
    pub fn to_display(&self, text: &str, byte: usize) -> usize {
        if self.preedit.is_empty() || byte <= self.preedit_at(text) {
            byte
        } else {
            byte.saturating_add(self.preedit.len())
        }
    }

    /// A display byte mapped back into the committed text; anything inside
    /// the composition maps to the caret.
    pub fn from_display(&self, text: &str, byte: usize) -> usize {
        if self.preedit.is_empty() {
            return byte;
        }
        let at = self.preedit_at(text);
        if byte <= at {
            byte
        } else if byte < at.saturating_add(self.preedit.len()) {
            at
        } else {
            byte.saturating_sub(self.preedit.len())
        }
    }

    /// Where the caret draws in the display text: inside the composition at
    /// the IME's cursor, snapped to a grapheme boundary of the composition.
    pub fn display_caret(&self, text: &str) -> usize {
        if self.preedit.is_empty() {
            return self.sel.cursor.min(text.len());
        }
        let within = self
            .preedit_cursor
            .map_or(self.preedit.len(), |(_, end)| end.min(self.preedit.len()));
        self.preedit_at(text)
            .saturating_add(grapheme_boundary_at_or_before(&self.preedit, within))
    }
}

/// Backspace for text typed only at its end (a type-ahead query, a digit
/// entry): remove the last character as a person sees it, never half of a
/// combining sequence or an emoji. Returns whether anything was removed.
pub fn delete_last_grapheme(text: &mut String) -> bool {
    let mut caret = EditCore::at(text.len());
    let edit = caret.delete_backward(text, false);
    !edit.is_noop() && edit.apply(text)
}

/// How long the word under the caret must stay the same before a field
/// offers a spelling suggestion for it: a short pause at the end of a word,
/// or the caret coming to rest in a word typed earlier.
pub const SUGGESTION_SETTLE: Duration = Duration::from_millis(300);

/// The word the caret is in or just after, as a byte range: letters plus
/// in-word apostrophes, so `don't` is one word. `None` between words.
pub fn word_at_caret(text: &str, caret: usize) -> Option<Range<usize>> {
    let caret = caret.min(text.len());
    let is_word_char =
        |character: char| character.is_alphabetic() || matches!(character, '\'' | '\u{2019}');
    let after = text
        .get(caret..)
        .and_then(|tail| tail.chars().next())
        .filter(|character| is_word_char(*character))
        .map(|_| caret);
    let before = text
        .get(..caret)
        .and_then(|head| head.char_indices().next_back())
        .filter(|(_, character)| is_word_char(*character))
        .map(|(byte, _)| byte);
    let (start, end) = word_boundaries(text, after.or(before)?);
    (start < end && start <= caret && caret <= end).then_some(start..end)
}

/// Debounces the word under the caret: it is *settled* once the same word
/// (same text at the same place) has been under a collapsed caret for
/// [`SUGGESTION_SETTLE`]. Typing into the word or moving to another one
/// restarts the wait; moving the caret within the word does not.
///
/// Time comes in as an argument so a host drives it from its clock and tests
/// from synthetic instants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaretWord {
    word: Option<(Range<usize>, String)>,
    since: Option<Instant>,
}

impl CaretWord {
    /// Record what is under the caret at `now`. Returns whether the word
    /// changed (and its wait restarted).
    pub fn observe(&mut self, text: &str, selection: Selection, now: Instant) -> bool {
        let word = selection
            .is_collapsed()
            .then(|| word_at_caret(text, selection.cursor))
            .flatten()
            .and_then(|range| Some((range.clone(), text.get(range)?.to_owned())));
        if word == self.word {
            return false;
        }
        self.since = word.is_some().then_some(now);
        self.word = word;
        true
    }

    /// Forget the current word, e.g. when the field loses focus.
    pub fn clear(&mut self) {
        self.word = None;
        self.since = None;
    }

    /// The word under the caret and its range, whether settled or not.
    pub fn current(&self) -> Option<(Range<usize>, &str)> {
        self.word
            .as_ref()
            .map(|(range, word)| (range.clone(), word.as_str()))
    }

    /// The word under the caret once it has settled at `now`.
    pub fn settled(&self, now: Instant) -> Option<(Range<usize>, &str)> {
        let since = self.since?;
        (now.saturating_duration_since(since) >= SUGGESTION_SETTLE)
            .then(|| self.current())
            .flatten()
    }

    /// When the current word settles; `None` without a word.
    pub fn deadline(&self) -> Option<Instant> {
        self.since?.checked_add(SUGGESTION_SETTLE)
    }
}

/// A burst of typing: every edit [`touch`]es the run, and a programmatic
/// replacement of the text [`close`]s it.
///
/// [`touch`]: TypingRun::touch
/// [`close`]: TypingRun::close
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypingRun {
    last_edit: Option<Instant>,
}

impl TypingRun {
    /// An edit landed now.
    pub fn touch(&mut self) {
        self.touch_at(crate::time::now());
    }
    /// An edit landed at `at`.
    pub fn touch_at(&mut self, at: Instant) {
        self.last_edit = Some(at);
    }
    /// When the latest edit of the open run landed.
    pub fn last_edit(&self) -> Option<Instant> {
        self.last_edit
    }
    /// The text was replaced; the next edit starts a new run.
    pub fn close(&mut self) {
        self.last_edit = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_up_and_down_keep_the_column_and_stop_at_the_edges() {
        let text = "abc\ndéf\nghij";
        let at = |core: &EditCore| core.selection().cursor;
        let mut core = EditCore::new();
        // Line 0, column 2 → line 1, column 2 (after the two-byte "é").
        core.set_caret(2);
        core.motion(text, Motion::LineDown, false);
        assert_eq!(at(&core), "abc\ndé".len());
        core.motion(text, Motion::LineUp, false);
        assert_eq!(at(&core), 2);
        // Up on the first line and down on the last are no-ops.
        core.motion(text, Motion::LineUp, false);
        assert_eq!(at(&core), 2);
        core.set_caret(text.len());
        core.motion(text, Motion::LineDown, false);
        assert_eq!(at(&core), text.len());
        // Column 4 clamps to the end of a shorter line, and extends a selection.
        core.motion(text, Motion::LineUp, true);
        assert_eq!(at(&core), "abc\ndéf".len());
        assert!(core.has_selection());

        // A column that would split a cluster lands on the cluster's start.
        let text = "abc\ne\u{301}e\u{301}";
        let mut core = EditCore::at(3);
        core.motion(text, Motion::LineDown, false);
        assert_eq!(at(&core), "abc\ne\u{301}".len());
    }

    /// Apply an `Edit` to a `String` the way the host CRDT splice would, so the
    /// tests can assert the model produces coherent text.
    fn apply(text: &str, e: &Edit) -> String {
        let mut s = String::from(text);
        s.replace_range(e.at..e.at.saturating_add(e.removed), &e.inserted);
        s
    }

    #[test]
    fn caret_motion_selection_and_clamping_land_on_shared_boundaries() {
        // (case, text, starting caret, [(motion, caret after it, why)])
        let cases: &[(&str, &str, usize, &[(Motion, usize, &str)])] = &[
            (
                "char motion clamps at the start",
                "abc",
                1,
                &[
                    (Motion::Left, 0, "left steps"),
                    (Motion::Left, 0, "left at start stays"),
                ],
            ),
            (
                "char motion clamps at the end",
                "abc",
                2,
                &[
                    (Motion::Right, 3, "right steps"),
                    (Motion::Right, 3, "right at end stays"),
                ],
            ),
            (
                "word motion uses shared boundaries",
                "hello world quick",
                0,
                &[
                    (
                        Motion::WordRight,
                        6,
                        "word-right lands on the next word start",
                    ),
                    (Motion::WordRight, 12, "word-right again"),
                    (
                        Motion::WordLeft,
                        6,
                        "word-left walks back to the word start",
                    ),
                ],
            ),
            (
                "line and document motion",
                "first\nsecond\nthird",
                8, // inside "second"
                &[
                    (Motion::LineStart, 6, "Home goes to the line start"),
                    (
                        Motion::LineEnd,
                        12,
                        "End goes to the line end (the trailing \\n)",
                    ),
                    (Motion::DocStart, 0, "document start"),
                    (Motion::DocEnd, 18, "document end"),
                ],
            ),
        ];
        for (case, text, start, steps) in cases {
            let mut c = EditCore::at(*start);
            for (motion, want, why) in *steps {
                c.motion(text, *motion, false);
                assert_eq!(c.caret(), *want, "case {case}: {why}");
            }
        }
        assert_eq!("first\nsecond\nthird".len(), 18);

        // Shift motion extends; a plain Left collapses to the LEFT edge of
        // the selection, not 3.
        let t = "abcdef";
        let mut c = EditCore::at(2);
        c.motion(t, Motion::Right, true);
        c.motion(t, Motion::Right, true);
        assert!(c.has_selection());
        assert_eq!(c.range(), 2..4, "shift-right extends the selection");
        c.motion(t, Motion::Left, false);
        assert!(!c.has_selection());
        assert_eq!(
            c.caret(),
            2,
            "plain left collapses to the selection's near edge"
        );

        // Select all, then a word.
        let t = "alpha beta";
        let mut c = EditCore::new();
        c.select_all(t);
        assert_eq!(c.range(), 0..t.len());
        c.select_word(t, 7); // inside "beta"
        assert_eq!(c.range(), 6..10);

        // "café" — é is two bytes (3..5). A caret past a shrunk text must land
        // on a char boundary, never inside the é.
        let mut c = EditCore::at(20);
        c.clamp_to("café");
        assert_eq!(c.caret(), "café".len());
        c.set_caret(4); // mid-é byte
        c.clamp_to("café");
        assert!(
            "café".is_char_boundary(c.caret()),
            "clamp lands on a char boundary"
        );
        let combining = "a\u{301}b";
        c.set_caret(1); // between the base and combining mark
        c.clamp_to(combining);
        assert_eq!(c.caret(), 0, "clamp never lands inside a grapheme");
    }

    #[test]
    fn edits_insert_and_delete_by_char_word_selection_and_grapheme() {
        // Case `insert_at_caret_and_over_selection`.
        {
            let t = "abc";
            let mut c = EditCore::at(1);
            let e = c.insert("XY");
            assert_eq!(
                e,
                Edit {
                    at: 1,
                    removed: 0,
                    inserted: "XY".into()
                }
            );
            assert_eq!(apply(t, &e), "aXYbc");
            assert_eq!(c.caret(), 3, "caret sits after the insert");

            // Insert over a selection replaces it.
            let t2 = "aXYbc";
            let mut c2 = EditCore::new();
            c2.set_selection(1, 3); // "XY"
            let e2 = c2.insert("Z");
            assert_eq!(apply(t2, &e2), "aZbc");
            assert_eq!(c2.caret(), 2);
            assert!(!c2.has_selection());
        }

        // Case `backspace_char_word_and_selection`.
        {
            // Char-wise.
            let t = "abc";
            let mut c = EditCore::at(2);
            let e = c.delete_backward(t, false);
            assert_eq!(apply(t, &e), "ac");
            assert_eq!(c.caret(), 1);
            // No-op at the start.
            let mut c0 = EditCore::at(0);
            assert!(c0.delete_backward(t, false).is_noop());
            // Word-wise.
            let tw = "hello world";
            let mut cw = EditCore::at(tw.len());
            let ew = cw.delete_backward(tw, true);
            assert_eq!(apply(tw, &ew), "hello ");
            // Over a selection.
            let mut cs = EditCore::new();
            cs.set_selection(0, 5);
            let es = cs.delete_backward(tw, false);
            assert_eq!(apply(tw, &es), " world");
        }

        // Case `delete_forward_char_word_and_end`.
        {
            let t = "abc";
            let mut c = EditCore::at(1);
            let e = c.delete_forward(t, false);
            assert_eq!(apply(t, &e), "ac");
            assert_eq!(c.caret(), 1, "forward delete keeps the caret put");
            let mut cend = EditCore::at(t.len());
            assert!(cend.delete_forward(t, false).is_noop());
            let tw = "hello world";
            let mut cw = EditCore::at(0);
            let ew = cw.delete_forward(tw, true);
            assert_eq!(apply(tw, &ew), "world");
        }

        // Case `delete_treats_emoji_sequence_as_one_character`.
        {
            let text = "A👩‍👩‍👧‍👦B";
            let after_emoji = text.len() - 1;
            let mut backward = EditCore::at(after_emoji);
            let edit = backward.delete_backward(text, false);
            assert_eq!(apply(text, &edit), "AB");
            assert_eq!(backward.caret(), 1);

            let mut forward = EditCore::at(1);
            let edit = forward.delete_forward(text, false);
            assert_eq!(apply(text, &edit), "AB");
        }
    }

    /// Clusters chosen so no concatenation of them merges into a different
    /// cluster: combining marks, ZWJ sequences, flags and skin tones each
    /// arrive with their own base.
    const CLUSTERS: &[&str] = &[
        "a",
        "Z",
        " ",
        "中",
        "e\u{301}",
        "👩\u{200d}👩\u{200d}👧",
        "👍🏽",
        "🇩🇪",
        "\u{915}\u{94d}\u{937}",
    ];

    /// A small deterministic generator (SplitMix64) so the sequences are
    /// reproducible without a property-testing dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
        }
        fn cluster(&mut self) -> &'static str {
            CLUSTERS[self.below(CLUSTERS.len())]
        }
    }

    fn type_into(core: &mut EditCore, text: &mut String, s: &str) {
        let edit = core.insert(s);
        assert!(edit.apply(text));
    }

    #[test]
    fn typing_char_by_char_reproduces_the_typed_string() {
        let mut rng = Rng(7);
        for _ in 0..200 {
            let len = rng.below(24);
            let typed: String = (0..len).map(|_| rng.cluster()).collect();
            let mut core = EditCore::new();
            let mut text = String::new();
            for ch in typed.chars() {
                let mut buf = [0; 4];
                type_into(&mut core, &mut text, ch.encode_utf8(&mut buf));
            }
            assert_eq!(text, typed);
            assert_eq!(core.caret(), typed.len());
        }
    }

    /// Random edits at the caret against a grapheme-list model: the buffer
    /// always equals the model's concatenation and the caret sits on the
    /// model's cluster boundary, so no motion or delete ever splits one.
    #[test]
    fn random_edits_at_the_caret_match_a_grapheme_model() {
        for seed in 0..64 {
            let mut rng = Rng(seed);
            let mut core = EditCore::new();
            let mut text = String::new();
            let mut model: Vec<&str> = Vec::new();
            let mut at = 0usize;
            for step in 0..120 {
                match rng.below(6) {
                    0 | 1 => {
                        let cluster = rng.cluster();
                        type_into(&mut core, &mut text, cluster);
                        model.insert(at, cluster);
                        at += 1;
                    }
                    2 => {
                        core.motion(&text, Motion::Left, false);
                        at = at.saturating_sub(1);
                    }
                    3 => {
                        core.motion(&text, Motion::Right, false);
                        at = (at + 1).min(model.len());
                    }
                    4 => {
                        let edit = core.delete_backward(&text, false);
                        assert!(edit.apply(&mut text));
                        if at > 0 {
                            at -= 1;
                            model.remove(at);
                        }
                    }
                    _ => {
                        let edit = core.delete_forward(&text, false);
                        assert!(edit.apply(&mut text));
                        if at < model.len() {
                            model.remove(at);
                        }
                    }
                }
                assert_eq!(text, model.concat(), "seed {seed} step {step}");
                let caret: usize = model[..at].iter().map(|c| c.len()).sum();
                assert_eq!(core.caret(), caret, "seed {seed} step {step}");
            }
        }
    }

    #[test]
    fn motion_and_deletion_never_split_a_grapheme_cluster() {
        for cluster in CLUSTERS {
            let text = format!("<{cluster}>");
            let (start, end) = (1, 1 + cluster.len());

            let mut core = EditCore::at(start);
            core.motion(&text, Motion::Right, false);
            assert_eq!(core.caret(), end, "Right steps over {cluster:?}");
            core.motion(&text, Motion::Left, false);
            assert_eq!(core.caret(), start, "Left steps over {cluster:?}");

            let mut back = EditCore::at(end);
            let edit = back.delete_backward(&text, false);
            assert_eq!(apply(&text, &edit), "<>", "Backspace removes {cluster:?}");
            assert_eq!(back.caret(), start);

            let mut forward = EditCore::at(start);
            let edit = forward.delete_forward(&text, false);
            assert_eq!(apply(&text, &edit), "<>", "Delete removes {cluster:?}");
            assert_eq!(forward.caret(), start);

            // Shift-select across the cluster covers all of it.
            let mut select = EditCore::at(start);
            select.motion(&text, Motion::Right, true);
            assert_eq!(select.range(), start..end);
        }
    }

    #[test]
    fn insert_replaces_the_selection_and_filter_admits_per_char() {
        let mut core = EditCore::new();
        let mut text = String::from("hello world");
        core.set_selection(11, 6);
        type_into(&mut core, &mut text, "there");
        assert_eq!(text, "hello there");
        assert_eq!(core.caret(), 11);
        assert!(!core.has_selection());

        // Plain arrows collapse a selection to its edge, word arrows too.
        core.set_selection(0, 5);
        core.motion(&text, Motion::WordRight, false);
        assert_eq!((core.caret(), core.has_selection()), (5, false));
        core.set_selection(5, 0);
        core.motion(&text, Motion::Left, false);
        assert_eq!(core.caret(), 0);

        let mut digits = EditCore::new();
        digits.set_filter(Some(|ch: char| ch.is_ascii_digit()));
        let mut text = String::from("12");
        digits.set_selection(0, 2);
        // A wholly rejected insert leaves the selection alone.
        assert!(digits.insert("ab").is_noop());
        assert_eq!(digits.range(), 0..2);
        type_into(&mut digits, &mut text, "a3b4");
        assert_eq!(text, "34");
        assert_eq!(digits.caret(), 2);
        assert_eq!(digits.admit("x5y"), "5");
    }

    #[test]
    fn ime_composition_is_display_only_until_commit() {
        let mut text = String::from("ab");
        let mut core = EditCore::at(1);
        core.set_preedit("漢字", Some(("漢".len(), "漢".len())));
        assert!(core.has_preedit());
        let comp = "漢字".len();
        assert_eq!(core.display_text(&text), "a漢字b");
        assert_eq!(core.preedit_display_range(&text), Some(1..1 + comp));
        assert_eq!(core.display_caret(&text), 1 + "漢".len());
        // Committed <-> display mapping around the composition.
        assert_eq!(core.to_display(&text, 0), 0);
        assert_eq!(core.to_display(&text, 1), 1);
        assert_eq!(core.to_display(&text, 2), 2 + comp);
        assert_eq!(core.from_display(&text, 1 + comp), 1);
        assert_eq!(core.from_display(&text, 2), 1, "inside maps to the caret");
        assert_eq!(core.from_display(&text, 2 + comp), 2);

        // An IME cursor inside a cluster snaps to the cluster start.
        core.set_preedit("e\u{301}x", Some((1, 1)));
        assert_eq!(core.display_caret(&text), 1);

        let edit = core.commit("漢字");
        assert!(edit.apply(&mut text));
        assert_eq!(text, "a漢字b");
        assert!(!core.has_preedit());
        assert_eq!(core.caret(), 1 + comp);
        assert_eq!(core.display_text(&text), text.as_str());

        // Commit replaces the selection; the filter governs both stages.
        let mut text = String::from("x12y");
        let mut core = EditCore::new();
        core.set_filter(Some(|ch: char| ch != 'q'));
        core.set_selection(1, 3);
        core.set_preedit("aqb", Some((3, 3)));
        assert_eq!(core.preedit(), "ab");
        assert_eq!(
            core.preedit_cursor(),
            None,
            "a filtered composition drops the IME cursor"
        );
        let edit = core.commit("aqb");
        assert!(edit.apply(&mut text));
        assert_eq!(text, "xaby");
        assert_eq!(core.caret(), 3);

        core.set_preedit("z", None);
        core.clear_preedit();
        assert!(!core.has_preedit());
    }

    #[test]
    fn edits_convert_to_char_splices_for_automerge() {
        use automerge::{AutoCommit, ObjType, ReadDoc};
        let mut doc = AutoCommit::new();
        let obj = doc.put_object(automerge::ROOT, "t", ObjType::Text).unwrap();
        let mut text = String::from("añb👍🏽c");
        doc.splice_text(&obj, 0, 0, &text).unwrap();

        let mut core = EditCore::new();
        core.set_selection("añb".len(), "añb👍🏽".len());
        let edit = core.insert("é");
        assert_eq!(
            edit.char_splice(&text),
            Some(CharSplice {
                at: 3,
                removed: 2,
                inserted: 1
            })
        );
        assert!(edit.splice_automerge(&mut doc, &obj, &text).is_some());
        assert!(edit.apply(&mut text));
        assert_eq!(text, "añbéc");
        assert_eq!(doc.text(&obj).unwrap(), text);

        // An edit that doesn't land on the text's boundaries is refused.
        let bad = Edit {
            at: 2,
            removed: 0,
            inserted: "x".into(),
        };
        assert_eq!(bad.char_splice(&text), None);
        assert!(!bad.apply(&mut text));
        assert!(bad.splice_automerge(&mut doc, &obj, &text).is_none());
        assert_eq!(doc.text(&obj).unwrap(), "añbéc");
    }

    #[test]
    fn the_caret_word_settles_and_restarts_on_change() {
        let text = "don't wrold  x";
        assert_eq!(word_at_caret(text, 0), Some(0..5), "apostrophes stay in");
        assert_eq!(word_at_caret(text, 5), Some(0..5), "the word's end");
        assert_eq!(word_at_caret(text, 8), Some(6..11));
        assert_eq!(word_at_caret(text, 12), None, "between words");

        let start = crate::time::now();
        let mut word = CaretWord::default();
        assert!(word.observe(text, Selection::collapsed(8), start));
        assert_eq!(word.deadline(), Some(start + SUGGESTION_SETTLE));
        assert!(word.settled(start).is_none());
        let settled = start + SUGGESTION_SETTLE;
        assert_eq!(word.settled(settled), Some((6..11, "wrold")));

        // Moving inside the same word keeps it settled.
        assert!(!word.observe(text, Selection::collapsed(11), settled));
        assert!(word.settled(settled).is_some());

        // Editing the word restarts the wait.
        let edited = "don't wrolds  x";
        assert!(word.observe(edited, Selection::collapsed(12), settled));
        assert!(word.settled(settled).is_none());
        assert!(word.settled(settled + SUGGESTION_SETTLE).is_some());

        // A selection or a gap between words has no word.
        assert!(word.observe(
            edited,
            Selection {
                anchor: 6,
                cursor: 8
            },
            settled
        ));
        assert_eq!(word.current(), None);
        assert_eq!(word.deadline(), None);
        word.observe(edited, Selection::collapsed(8), settled);
        word.clear();
        assert_eq!(word.current(), None);
    }
}
