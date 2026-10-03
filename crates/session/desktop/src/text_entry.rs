//! Keyboard editing for the chrome's inline text fields.
//!
//! The compositor's own surfaces — the workspace-name inline edit, the
//! Wi-Fi password prompt — are painted immediate-mode rather than as
//! retained `ui` widgets, because they live inside popovers the chrome
//! lays out by hand. What they must not do is each grow their own idea
//! of what Backspace, Ctrl+A, Home or a paste means: that is how one
//! field ends up supporting word-wise deletion and the next one does
//! not, and how a selection replacement gets fixed in one place and
//! stays broken in the other.
//!
//! So the *editing* is shared here even though the *painting* is not.
//! The selection primitives themselves come from
//! [`ui::text_selection`], the same ones the toolkit's real text widgets
//! use, so caret and word-boundary behaviour matches the apps.

use ui::text_selection::{
    Selection, clipboard_copy, clipboard_paste, next_char_boundary, next_word_boundary,
    prev_char_boundary, prev_word_boundary,
};
use winit::keyboard::{Key, NamedKey};

/// What the caller should do after handing a key to [`apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Not ours — let it fall through to whatever is behind the field.
    Ignored,
    /// Consumed, nothing visible changed (a no-op `Ctrl+C`, say): swallow
    /// the key, skip the repaint.
    ConsumedQuiet,
    /// Consumed and the buffer or selection changed — repaint.
    Changed,
    /// Enter: the user is done. The caller commits.
    Submit,
    /// Escape: the user backed out. The caller discards.
    Cancel,
}

/// Modifier state as this module cares about it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    /// Ctrl *or* Super — the chrome treats them alike so the same chords
    /// work whichever the user's muscle memory reaches for.
    pub ctrl: bool,
}

impl Mods {
    pub fn from_state(state: winit::keyboard::ModifiersState) -> Self {
        Self {
            shift: state.shift_key(),
            ctrl: state.control_key() || state.super_key(),
        }
    }
}

/// Apply one key press to the caller's own `buf` / `sel`; `sel`'s anchor
/// and cursor are byte indices into `buf`, always left on a char boundary.
pub fn apply(key: &Key, mods: Mods, buf: &mut String, sel: &mut Selection) -> Edit {
    let Mods { shift, ctrl } = mods;
    match key {
        Key::Named(NamedKey::Enter) => Edit::Submit,
        Key::Named(NamedKey::Escape) => Edit::Cancel,
        Key::Named(NamedKey::ArrowLeft) => {
            sel.cursor = if ctrl {
                prev_word_boundary(buf, sel.cursor)
            } else {
                prev_char_boundary(buf, sel.cursor)
            };
            if !shift {
                sel.collapse_to_cursor();
            }
            Edit::Changed
        }
        Key::Named(NamedKey::ArrowRight) => {
            sel.cursor = if ctrl {
                next_word_boundary(buf, sel.cursor)
            } else {
                next_char_boundary(buf, sel.cursor)
            };
            if !shift {
                sel.collapse_to_cursor();
            }
            Edit::Changed
        }
        Key::Named(NamedKey::Home) => {
            sel.cursor = 0;
            if !shift {
                sel.collapse_to_cursor();
            }
            Edit::Changed
        }
        Key::Named(NamedKey::End) => {
            sel.cursor = buf.len();
            if !shift {
                sel.collapse_to_cursor();
            }
            Edit::Changed
        }
        Key::Named(NamedKey::Backspace) => {
            if sel.is_collapsed() {
                if sel.cursor == 0 {
                    return Edit::ConsumedQuiet;
                }
                let prev = if ctrl {
                    prev_word_boundary(buf, sel.cursor)
                } else {
                    prev_char_boundary(buf, sel.cursor)
                };
                buf.replace_range(prev..sel.cursor, "");
                *sel = Selection::collapsed(prev);
            } else {
                let r = sel.range();
                buf.replace_range(r.clone(), "");
                *sel = Selection::collapsed(r.start);
            }
            Edit::Changed
        }
        Key::Named(NamedKey::Delete) => {
            if sel.is_collapsed() {
                if sel.cursor >= buf.len() {
                    return Edit::ConsumedQuiet;
                }
                let next = if ctrl {
                    next_word_boundary(buf, sel.cursor)
                } else {
                    next_char_boundary(buf, sel.cursor)
                };
                buf.replace_range(sel.cursor..next, "");
            } else {
                let r = sel.range();
                buf.replace_range(r.clone(), "");
                *sel = Selection::collapsed(r.start);
            }
            Edit::Changed
        }
        Key::Named(NamedKey::Space) => {
            let r = sel.range();
            buf.replace_range(r.clone(), " ");
            *sel = Selection::collapsed(r.start.saturating_add(1));
            Edit::Changed
        }
        Key::Character(s) if ctrl => apply_ctrl_char(s, buf, sel),
        Key::Character(s) => apply_plain_char(s, buf, sel),
        _ => Edit::Ignored,
    }
}

/// `Key::Character` under Ctrl/Super: select-all, copy, cut, paste, or
/// (any other character) ignored.
fn apply_ctrl_char(s: &str, buf: &mut String, sel: &mut Selection) -> Edit {
    match s {
        "a" | "A" => {
            sel.anchor = 0;
            sel.cursor = buf.len();
            Edit::ConsumedQuiet
        }
        "c" | "C" => {
            let r = sel.range();
            if let Some(selected) = buf.get(r) {
                clipboard_copy(selected);
            }
            Edit::ConsumedQuiet
        }
        "x" | "X" => {
            let r = sel.range();
            if r.is_empty() {
                return Edit::ConsumedQuiet;
            }
            let Some(selected) = buf.get(r.clone()) else {
                return Edit::ConsumedQuiet;
            };
            clipboard_copy(selected);
            buf.replace_range(r.clone(), "");
            *sel = Selection::collapsed(r.start);
            Edit::Changed
        }
        "v" | "V" => match clipboard_paste() {
            Some(paste) => {
                let r = sel.range();
                buf.replace_range(r.clone(), &paste);
                *sel = Selection::collapsed(r.start.saturating_add(paste.len()));
                Edit::Changed
            }
            None => Edit::ConsumedQuiet,
        },
        _ => Edit::Ignored,
    }
}

/// An ordinary typed character (not a chord): control characters are
/// filtered out, everything else replaces the current selection.
fn apply_plain_char(s: &str, buf: &mut String, sel: &mut Selection) -> Edit {
    // Control characters never enter a buffer — invisible, unremovable
    // by eye, and travels into whatever the field feeds (an nmcli
    // argument, a synced workspace name).
    let filtered: String = s.chars().filter(|c| !c.is_control()).collect();
    if filtered.is_empty() {
        return Edit::Ignored;
    }
    let r = sel.range();
    buf.replace_range(r.clone(), &filtered);
    *sel = Selection::collapsed(r.start.saturating_add(filtered.len()));
    Edit::Changed
}

/// One bullet per character. Bullets over asterisks: `•` is round and even
/// at the chrome's text size, where `*` sits high like a footnote marker.
pub fn masked(buf: &str) -> String {
    "•".repeat(buf.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn plain() -> Mods {
        Mods::default()
    }

    fn ctrl() -> Mods {
        Mods {
            shift: false,
            ctrl: true,
        }
    }

    fn typed(s: &str) -> (String, Selection) {
        let mut buf = String::new();
        let mut sel = Selection::collapsed(0);
        for c in s.chars() {
            let key = if c == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                ch(&c.to_string())
            };
            apply(&key, plain(), &mut buf, &mut sel);
        }
        (buf, sel)
    }

    #[test]
    fn typing_inserts_characters_at_the_caret_and_filters_the_rest() {
        // Phase `typing_appends_and_leaves_the_caret_at_the_end`.
        {
            let (buf, sel) = typed("hunter2");
            assert_eq!(buf, "hunter2");
            assert_eq!(sel.cursor, 7);
            assert!(sel.is_collapsed());
        }

        // Phase `space_is_a_character_not_a_command`.
        {
            // Space arrives as a NamedKey, not a Character.
            let (buf, _) = typed("correct horse");
            assert_eq!(buf, "correct horse");
        }

        // Phase `typing_over_a_selection_replaces_it`.
        {
            let (mut buf, mut sel) = typed("secret");
            sel.anchor = 0;
            sel.cursor = 6;
            apply(&ch("x"), plain(), &mut buf, &mut sel);
            assert_eq!(buf, "x");
            assert_eq!(sel.cursor, 1);
        }

        // Phase `multibyte_input_never_splits_a_character`.
        {
            let mut buf = String::new();
            let mut sel = Selection::collapsed(0);
            apply(&ch("é"), plain(), &mut buf, &mut sel);
            apply(&ch("漢"), plain(), &mut buf, &mut sel);
            assert_eq!(buf, "é漢");
            apply(
                &Key::Named(NamedKey::Backspace),
                plain(),
                &mut buf,
                &mut sel,
            );
            assert_eq!(buf, "é");
            assert!(buf.is_char_boundary(sel.cursor));
        }

        // Phase `control_characters_are_dropped_rather_than_stored`.
        {
            let mut buf = String::new();
            let mut sel = Selection::collapsed(0);
            assert_eq!(
                apply(&ch("\u{7}"), plain(), &mut buf, &mut sel),
                Edit::Ignored
            );
            assert!(buf.is_empty(), "an invisible byte must never reach nmcli");
        }
    }

    #[test]
    fn deletion_keys_remove_characters_and_words_or_say_nothing_changed() {
        // Phase `backspace_removes_one_character_and_a_word_under_ctrl`.
        {
            let (mut buf, mut sel) = typed("hello world");
            assert_eq!(
                apply(
                    &Key::Named(NamedKey::Backspace),
                    plain(),
                    &mut buf,
                    &mut sel
                ),
                Edit::Changed
            );
            assert_eq!(buf, "hello worl");
            apply(&Key::Named(NamedKey::Backspace), ctrl(), &mut buf, &mut sel);
            assert_eq!(buf, "hello ", "ctrl+backspace deletes the word");
        }

        // Phase `backspace_at_the_start_changes_nothing_and_says_so`.
        {
            let mut buf = String::new();
            let mut sel = Selection::collapsed(0);
            assert_eq!(
                apply(
                    &Key::Named(NamedKey::Backspace),
                    plain(),
                    &mut buf,
                    &mut sel
                ),
                Edit::ConsumedQuiet,
                "swallowed, but no repaint is owed"
            );
        }

        // Phase `ctrl_delete_removes_the_next_word`.
        {
            let (mut buf, _) = typed("hello world");
            let mut sel = Selection::collapsed(0);
            apply(&Key::Named(NamedKey::Delete), ctrl(), &mut buf, &mut sel);
            assert_eq!(buf, "world");
            assert_eq!(sel, Selection::collapsed(0));
        }
    }

    #[test]
    fn caret_keys_move_and_extend_the_selection() {
        // Phase `select_all_spans_the_buffer_without_owing_a_repaint`.
        {
            let (mut buf, mut sel) = typed("abc");
            assert_eq!(
                apply(&ch("a"), ctrl(), &mut buf, &mut sel),
                Edit::ConsumedQuiet
            );
            assert_eq!(sel.anchor, 0);
            assert_eq!(sel.cursor, 3);
            assert_eq!(buf, "abc", "ctrl+a must not type an 'a'");
        }

        // Phase `home_and_end_move_the_caret_to_the_extremes`.
        {
            let (mut buf, mut sel) = typed("abcdef");
            apply(&Key::Named(NamedKey::Home), plain(), &mut buf, &mut sel);
            assert_eq!(sel.cursor, 0);
            apply(&Key::Named(NamedKey::End), plain(), &mut buf, &mut sel);
            assert_eq!(sel.cursor, 6);
        }

        // Phase `shift_arrow_extends_a_selection_rather_than_collapsing_it`.
        {
            let (mut buf, mut sel) = typed("abcdef");
            let shift = Mods {
                shift: true,
                ctrl: false,
            };
            apply(&Key::Named(NamedKey::ArrowLeft), shift, &mut buf, &mut sel);
            apply(&Key::Named(NamedKey::ArrowLeft), shift, &mut buf, &mut sel);
            assert!(!sel.is_collapsed());
            assert_eq!(sel.range(), 4..6);
        }

        // Phase `ctrl_shift_arrow_extends_by_words_and_shift_home_end_keep_anchor`.
        {
            let (mut buf, _) = typed("hello world");
            let mut sel = Selection::collapsed(0);
            let ctrl_shift = Mods {
                shift: true,
                ctrl: true,
            };
            apply(
                &Key::Named(NamedKey::ArrowRight),
                ctrl_shift,
                &mut buf,
                &mut sel,
            );
            assert_eq!(sel.range(), 0..6);
            apply(
                &Key::Named(NamedKey::End),
                Mods {
                    shift: true,
                    ctrl: false,
                },
                &mut buf,
                &mut sel,
            );
            assert_eq!(sel.range(), 0..buf.len());
        }
    }

    #[test]
    fn non_text_keys_are_reported_or_fall_through_without_editing() {
        // Phase `enter_and_escape_are_reported_rather_than_typed`.
        {
            let (mut buf, mut sel) = typed("pw");
            assert_eq!(
                apply(&Key::Named(NamedKey::Enter), plain(), &mut buf, &mut sel),
                Edit::Submit
            );
            assert_eq!(
                apply(&Key::Named(NamedKey::Escape), plain(), &mut buf, &mut sel),
                Edit::Cancel
            );
            assert_eq!(buf, "pw", "neither key changes the buffer");
        }

        // Phase `unrelated_keys_fall_through`.
        {
            let mut buf = String::new();
            let mut sel = Selection::collapsed(0);
            assert_eq!(
                apply(&Key::Named(NamedKey::F5), plain(), &mut buf, &mut sel),
                Edit::Ignored
            );
            assert_eq!(apply(&ch("q"), ctrl(), &mut buf, &mut sel), Edit::Ignored);
        }
    }

    #[test]
    fn masking_is_one_bullet_per_character_not_per_byte() {
        assert_eq!(masked(""), "");
        assert_eq!(masked("abc"), "•••");
        assert_eq!(
            masked("é漢"),
            "••",
            "a masked field must not leak a password's byte length"
        );
    }
}
