//! Keyboard editing for the chrome's inline text fields.
//!
//! Some of the compositor's own surfaces — the workspace-name inline edit,
//! the presence status editor — are painted immediate-mode rather than as
//! retained `ui` widgets, because they live inside popovers the chrome
//! lays out by hand. What they must not do is each grow their own idea
//! of what Backspace, Ctrl+A, Home or a paste means: that is how one
//! field ends up supporting word-wise deletion and the next one does
//! not, and how a selection replacement gets fixed in one place and
//! stays broken in the other.
//!
//! So the *editing* is shared even though the *painting* is not: every
//! key is applied by [`ui::EditCore`], the same editing core the toolkit's
//! text fields use, so caret, grapheme, word and input-method behaviour
//! match the apps. This module only maps the chrome's keys onto it.

use ui::text_selection::{Selection, clipboard_copy, clipboard_paste};
use ui::{EditCore, Motion};
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

/// Control characters never enter a buffer — invisible, unremovable by eye,
/// and they travel into whatever the field feeds (an nmcli argument, a
/// synced workspace name).
fn printable(c: char) -> bool {
    !c.is_control()
}

/// The editing core over the caller's selection.
fn core(sel: Selection) -> EditCore {
    let mut core = EditCore::new();
    core.set_filter(Some(printable));
    core.set_selection(sel.anchor, sel.cursor);
    core
}

/// Apply `edit` to `buf` and hand the core's selection back to `sel`.
fn finish(edit: &ui::Edit, core: &EditCore, buf: &mut String, sel: &mut Selection) -> Edit {
    let changed = !edit.is_noop() && edit.apply(buf);
    let moved = core.selection() != *sel;
    *sel = core.selection();
    if changed || moved {
        Edit::Changed
    } else {
        Edit::ConsumedQuiet
    }
}

/// Apply one key press to the caller's own `buf` / `sel`; `sel`'s anchor
/// and cursor are byte indices into `buf`, always left on a grapheme
/// boundary.
pub fn apply(key: &Key, mods: Mods, buf: &mut String, sel: &mut Selection) -> Edit {
    let Mods { shift, ctrl } = mods;
    let mut edit_core = core(*sel);
    let motion = |m: Motion, core: &mut EditCore, buf: &mut String, sel: &mut Selection| {
        core.motion(buf, m, shift);
        finish(&ui::Edit::default(), core, buf, sel)
    };
    match key {
        Key::Named(NamedKey::Enter) => Edit::Submit,
        Key::Named(NamedKey::Escape) => Edit::Cancel,
        Key::Named(NamedKey::ArrowLeft) => {
            let m = if ctrl { Motion::WordLeft } else { Motion::Left };
            motion(m, &mut edit_core, buf, sel)
        }
        Key::Named(NamedKey::ArrowRight) => {
            let m = if ctrl {
                Motion::WordRight
            } else {
                Motion::Right
            };
            motion(m, &mut edit_core, buf, sel)
        }
        Key::Named(NamedKey::Home) => motion(Motion::DocStart, &mut edit_core, buf, sel),
        Key::Named(NamedKey::End) => motion(Motion::DocEnd, &mut edit_core, buf, sel),
        Key::Named(NamedKey::Backspace) => {
            let edit = edit_core.delete_backward(buf, ctrl);
            finish(&edit, &edit_core, buf, sel)
        }
        Key::Named(NamedKey::Delete) => {
            let edit = edit_core.delete_forward(buf, ctrl);
            finish(&edit, &edit_core, buf, sel)
        }
        Key::Named(NamedKey::Space) => {
            let edit = edit_core.insert(" ");
            finish(&edit, &edit_core, buf, sel)
        }
        Key::Character(s) if ctrl => apply_ctrl_char(s, &mut edit_core, buf, sel),
        Key::Character(s) => commit(s, buf, sel),
        _ => Edit::Ignored,
    }
}

/// Text from an input method (or a typed character): it replaces the
/// selection at the caret. Text that is only control characters is not
/// ours.
pub fn commit(text: &str, buf: &mut String, sel: &mut Selection) -> Edit {
    let mut edit_core = core(*sel);
    let edit = edit_core.commit(text);
    if edit.is_noop() {
        return Edit::Ignored;
    }
    finish(&edit, &edit_core, buf, sel)
}

/// `Key::Character` under Ctrl/Super: select-all, copy, cut, paste, or
/// (any other character) ignored.
fn apply_ctrl_char(
    s: &str,
    edit_core: &mut EditCore,
    buf: &mut String,
    sel: &mut Selection,
) -> Edit {
    match s {
        "a" | "A" => {
            // The painted selection changed, so a repaint is owed.
            edit_core.select_all(buf);
            finish(&ui::Edit::default(), edit_core, buf, sel)
        }
        "c" | "C" => {
            if let Some(selected) = edit_core.selected(buf) {
                clipboard_copy(selected);
            }
            Edit::ConsumedQuiet
        }
        "x" | "X" => {
            let Some(selected) = edit_core.selected(buf) else {
                return Edit::ConsumedQuiet;
            };
            clipboard_copy(selected);
            let edit = edit_core.delete_selection();
            finish(&edit, edit_core, buf, sel)
        }
        "v" | "V" => match clipboard_paste() {
            Some(paste) => {
                let edit = edit_core.insert(&paste);
                finish(&edit, edit_core, buf, sel)
            }
            None => Edit::ConsumedQuiet,
        },
        _ => Edit::Ignored,
    }
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
    fn characters_are_what_a_person_sees_and_input_methods_type_at_the_caret() {
        // Phase `backspace_and_arrows_step_over_whole_clusters`.
        {
            let mut buf = String::new();
            let mut sel = Selection::collapsed(0);
            for text in ["a", "e\u{301}", "👍🏽", "🇳🇱"] {
                assert_eq!(commit(text, &mut buf, &mut sel), Edit::Changed);
            }
            assert_eq!(buf, "ae\u{301}👍🏽🇳🇱");
            apply(
                &Key::Named(NamedKey::ArrowLeft),
                plain(),
                &mut buf,
                &mut sel,
            );
            apply(
                &Key::Named(NamedKey::Backspace),
                plain(),
                &mut buf,
                &mut sel,
            );
            assert_eq!(buf, "ae\u{301}🇳🇱", "the skin-toned emoji goes whole");
            apply(
                &Key::Named(NamedKey::Backspace),
                plain(),
                &mut buf,
                &mut sel,
            );
            assert_eq!(buf, "a🇳🇱", "the accented letter goes whole");
        }

        // Phase `a_commit_replaces_the_selection_and_ignores_control_text`.
        {
            let (mut buf, mut sel) = typed("hello world");
            sel.anchor = 6;
            sel.cursor = 11;
            assert_eq!(commit("Welt", &mut buf, &mut sel), Edit::Changed);
            assert_eq!(buf, "hello Welt");
            assert_eq!(sel, Selection::collapsed(10));
            assert_eq!(commit("\u{1b}", &mut buf, &mut sel), Edit::Ignored);
            assert_eq!(buf, "hello Welt");
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
        // Phase `select_all_spans_the_buffer_and_repaints_its_highlight`.
        {
            let (mut buf, mut sel) = typed("abc");
            assert_eq!(apply(&ch("a"), ctrl(), &mut buf, &mut sel), Edit::Changed);
            assert_eq!(
                apply(&ch("a"), ctrl(), &mut buf, &mut sel),
                Edit::ConsumedQuiet,
                "selecting what is already selected changes nothing"
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
}
