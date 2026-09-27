//! `EditCore` — the shared headless text-editing model.
//!
//! Caret + selection state (byte offsets) plus the motion / edit vocabulary
//! every editable surface needs, built on the shared [`text_selection`](crate::text_selection) boundary
//! walkers and producing splices for an Automerge `Text`. It is deliberately
//! UI-free: no rendering, no CRDT handle. The host widget applies the returned
//! [`Edit`] to its CRDT (`AutoCommit::splice_text`, char-addressed) and feeds the
//! resulting text back in.
//!
//! This is the reusable caret core the WYSIWYG [`RichTextEditor`] uses;
//! [`TextInput`] keeps equivalent inline logic. Vertical (Up/Down) motion needs
//! laid-out line geometry, so it lives in the widget — the core owns everything
//! horizontal and range-wise, which is exactly what unit tests can pin without a
//! GPU.
//!
//! [`RichTextEditor`]: crate::widget::RichTextEditor
//! [`TextInput`]: crate::widget::TextInput

use crate::text_selection::{
    Selection, TextAffinity, TextPosition, grapheme_boundary_at_or_before, line_boundaries,
    next_char_boundary, next_word_boundary, prev_char_boundary, prev_word_boundary,
    word_boundaries,
};

/// A caret motion. Vertical motion across wrapped lines is the host's job
/// because it needs laid-out line geometry; [`Motion::LineUp`] and
/// [`Motion::LineDown`] move across `\n`-delimited lines only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// One Unicode scalar left / right.
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
/// holding `byte`, at the same character column, clamped to that line's end.
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
    target_start.saturating_add(
        line.char_indices()
            .nth(column)
            .map_or(line.len(), |(offset, _)| offset),
    )
}

/// A splice to apply to the buffer, in **byte** offsets over the pre-edit text:
/// `at..at+removed` is replaced by `inserted`. The host converts to char offsets
/// for `AutoCommit::splice_text` (Automerge `Text` is char-addressed).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Edit {
    pub at: usize,
    pub removed: usize,
    pub inserted: String,
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
}

/// The caret + selection model. Selection offsets are **bytes** into the bound
/// text (matching [`text_selection`](crate::text_selection) and cosmic-text's byte-indexed layout).
#[derive(Clone, Debug, Default)]
pub struct EditCore {
    sel: Selection,
    affinity: TextAffinity,
}

impl EditCore {
    /// A collapsed caret at the start.
    pub fn new() -> Self {
        Self {
            sel: Selection::collapsed(0),
            affinity: TextAffinity::Before,
        }
    }
    /// A collapsed caret at byte `pos`.
    pub fn at(pos: usize) -> Self {
        Self {
            sel: Selection::collapsed(pos),
            affinity: TextAffinity::Before,
        }
    }

    /// The full selection (anchor + cursor).
    pub fn selection(&self) -> Selection {
        self.sel
    }
    /// Live caret byte offset.
    pub fn caret(&self) -> usize {
        self.sel.cursor
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
    pub fn range(&self) -> std::ops::Range<usize> {
        self.sel.range()
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
        self.sel = Selection {
            anchor: 0,
            cursor: text.len(),
        };
        self.affinity = TextAffinity::Before;
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
    /// plain (non-extend) Left/Right with a live selection collapses to the near
    /// edge rather than stepping past it — the native editor behaviour.
    pub fn motion(&mut self, text: &str, m: Motion, extend: bool) {
        if !extend && self.has_selection() && matches!(m, Motion::Left | Motion::Right) {
            let edge = if m == Motion::Left {
                self.sel.min()
            } else {
                self.sel.max()
            };
            self.sel.move_to(edge);
            self.affinity = TextAffinity::Before;
            return;
        }
        let t = self.target(text, m);
        if extend {
            self.sel.extend_to(t);
        } else {
            self.sel.move_to(t);
        }
        self.affinity = TextAffinity::Before;
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
        self.sel = Selection {
            anchor: s,
            cursor: e,
        };
        self.affinity = TextAffinity::Before;
    }
    /// Select the `\n`-delimited line under `byte` (triple-click).
    pub fn select_line(&mut self, text: &str, byte: usize) {
        let (s, e) = line_boundaries(text, byte);
        self.sel = Selection {
            anchor: s,
            cursor: e,
        };
        self.affinity = TextAffinity::Before;
    }

    /// Insert `s`, replacing any selection. Returns the byte-space [`Edit`] and
    /// moves the caret to just after the inserted text.
    pub fn insert(&mut self, s: &str) -> Edit {
        let (at, removed) = if self.has_selection() {
            (
                self.sel.min(),
                self.sel.max().saturating_sub(self.sel.min()),
            )
        } else {
            (self.sel.cursor, 0)
        };
        let edit = Edit {
            at,
            removed,
            inserted: s.to_string(),
        };
        self.sel.move_to(edit.caret_after());
        self.affinity = TextAffinity::Before;
        edit
    }

    /// Backspace: delete the selection, else the char (or word, with `word`)
    /// before the caret. Returns the resulting [`Edit`] (a no-op at the start).
    pub fn delete_backward(&mut self, text: &str, word: bool) -> Edit {
        if self.has_selection() {
            let at = self.sel.min();
            let removed = self.sel.max().saturating_sub(at);
            self.sel.move_to(at);
            self.affinity = TextAffinity::Before;
            return Edit {
                at,
                removed,
                inserted: String::new(),
            };
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
        self.sel.move_to(from);
        self.affinity = TextAffinity::Before;
        Edit {
            at: from,
            removed: c.saturating_sub(from),
            inserted: String::new(),
        }
    }

    /// Delete forward (Delete key): the selection, else the char (or word) after
    /// the caret. The caret stays put. No-op at the end of the buffer.
    pub fn delete_forward(&mut self, text: &str, word: bool) -> Edit {
        if self.has_selection() {
            let at = self.sel.min();
            let removed = self.sel.max().saturating_sub(at);
            self.sel.move_to(at);
            self.affinity = TextAffinity::Before;
            return Edit {
                at,
                removed,
                inserted: String::new(),
            };
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
        self.sel.move_to(c);
        self.affinity = TextAffinity::Before;
        Edit {
            at: c,
            removed: to.saturating_sub(c),
            inserted: String::new(),
        }
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
}
