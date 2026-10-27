//! The multiline field: what IME does at a paragraph, Enter, Backspace and
//! copy across newlines, wrapping at Unicode line breaks, navigation by visual
//! line, and the binding that survives a controlled field.
use super::super::*;
use super::drive;
use super::ml_input;
use super::mods;
use super::shaped_row;
use crate::synthetic_input::{char_key, named_key};
use automerge::ObjType;
use automerge::transaction::Transactable as _;

#[test]
fn multiline_ime_intent_and_paragraph_commit_preserve_the_submit_boundary() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = ml_input("before old after", 400.0, 200.0).with_enter_submits(true);
    input.set_selection(7, 10);
    let intent = crate::ime::ImeWindowState::from_widget(&input).unwrap();
    assert!(intent.content.hints.multiline);
    assert_eq!(intent.content.purpose, crate::ime::InputPurpose::Text);
    let mut doc = AutoCommit::new();
    assert!(drive(
        &mut input,
        &mut doc,
        &WindowEvent::Ime(winit::event::Ime::Commit("First café.\n\nSecond 👋".into())),
        Modifiers::default(),
    ));
    assert_eq!(input.value(), "before First café.\n\nSecond 👋 after");
    assert!(!input.take_submit());
    input.set_selection(7, 7 + "First café.\n\nSecond 👋".len());
    assert!(drive(
        &mut input,
        &mut doc,
        &WindowEvent::Ime(winit::event::Ime::Commit("\n\n".into())),
        Modifiers::default(),
    ));
    assert_eq!(input.value(), "before \n\n after");
    assert!(!input.take_submit());
    input.set_multiline(false);
    assert!(
        !crate::ime::ImeWindowState::from_widget(&input)
            .unwrap()
            .content
            .hints
            .multiline
    );
    input.set_focus(false);
    assert!(crate::ime::ImeWindowState::from_widget(&input).is_none());
    let mut secret = TextInput::local().multiline(true).with_secret(true);
    secret.set_focus(true);
    let secret = crate::ime::ImeWindowState::from_widget(&secret).unwrap();
    assert_eq!(secret.content, crate::ime::InputContent::PASSWORD);
}

#[test]
fn multiline_enter_backspace_and_copy_handle_newlines() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_enter_inserts_newline`.
    {
        let mut ti = ml_input("ab", 300.0, 120.0);
        ti.set_cursor_pos(1); // between 'a' and 'b'
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(ti.value(), "a\nb", "Enter inserted a newline at the caret");
        assert_eq!(
            ti.cursor_pos(),
            2,
            "caret sits just after the inserted newline"
        );
    }

    // Case `enter_submits_signals_without_inserting_and_shift_enter_still_splits`.
    {
        let mut ti = TextInput::local()
            .with_enter_submits(true)
            .with_initial_value("hi");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(
            ti.value(),
            "hi",
            "Enter must not insert a newline in submit mode"
        );
        assert!(ti.take_submit(), "Enter fired the submit signal");
        assert!(!ti.take_submit(), "submit is one-shot — cleared on read");

        let mut ml = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_initial_value("hi");
        ml.set_focus(true);
        ml.set_cursor_pos(2);
        drive(
            &mut ml,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, true),
        ); // Shift+Enter
        assert_eq!(
            ml.value(),
            "hi\n",
            "Shift+Enter inserts a newline in multiline submit mode"
        );
        assert!(!ml.take_submit(), "Shift+Enter must not submit");
    }

    // Case `multiline_backspace_at_line_start_merges_lines`.
    // The newline is just a byte, so the shared delete path handles it.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(6); // start of "world", just after the newline
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Backspace),
            mods(false, false),
        );
        assert_eq!(
            ti.value(),
            "helloworld",
            "Backspace removed the newline, merging the lines"
        );
        assert_eq!(ti.cursor_pos(), 5);
    }

    // Case `multiline_selection_spans_lines_and_copy_includes_newline`.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(3); // inside "hello"
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, true),
        ); // Shift+Down
        let (s, e) = ti
            .selection_range_public()
            .expect("a cross-line selection is active");
        assert!(
            s <= 5 && e > 6,
            "selection crosses the newline at byte 5, got ({s},{e})"
        );
        let selected = ti.selected_str().expect("selected slice");
        assert!(
            selected.contains('\n'),
            "copy would emit the newline: {selected:?}"
        );
    }
}

#[test]
fn multiline_vertical_and_line_edge_navigation_follows_visual_lines() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_up_down_moves_between_visual_lines`.
    {
        let mut ti = ml_input("hello\nworld\nfoo", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // inside "world" (row 1)
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "precondition: row 1");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowUp),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 0, "Up moved to row 0");

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down returned to row 1"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 2, "Down reached row 2");
    }

    // Case `multiline_vertical_motion_preserves_goal_column`.
    {
        let mut ti = ml_input("abcdefgh\nx\nabcdefgh", 500.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // end of the first "abcdefgh" (row 0, high column)

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(
            shaped_row(&ti, ti.cursor_pos()),
            1,
            "Down reached short row"
        );

        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        let row = shaped_row(&ti, ti.cursor_pos());
        assert_eq!(row, 2, "Down reached the third row");
        assert!(
            ti.cursor_pos() >= 17,
            "goal column preserved across the short line — caret is near row 3's end, got {}",
            ti.cursor_pos()
        );
    }

    // Case `multiline_home_end_are_per_visual_line`.
    {
        let mut ti = ml_input("hello\nworld", 400.0, 200.0);
        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(8); // "wo|rld" on row 1
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Home),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            6,
            "Home went to the start of the second line, not the field"
        );
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::End),
            mods(false, false),
        );
        assert_eq!(
            ti.cursor_pos(),
            11,
            "End went to the end of the second line"
        );
    }
}

#[test]
fn multiline_wraps_at_unicode_line_breaks() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_soft_wraps_at_word_boundaries`.
    {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let mut ti = ml_input(text, 170.0, 220.0); // narrow → must wrap
        let layout = ti.single_layout.as_ref().expect("paragraph was shaped");
        let runs: Vec<_> = layout.buffer.layout_runs().collect();
        assert!(runs.len() > 1, "long paragraph wrapped into visual rows");
        let first_end = runs[0]
            .glyphs
            .iter()
            .map(|glyph| glyph.end)
            .max()
            .unwrap_or(0);
        assert!(
            ti.value()
                .get(first_end..)
                .is_some_and(|s| s.starts_with(' ')),
            "wrap happened at a word boundary (the break precedes whitespace): {:?}",
            ti.value().get(..first_end)
        );

        let mut doc = AutoCommit::new();
        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowDown),
            mods(false, false),
        );
        assert_eq!(shaped_row(&ti, ti.cursor_pos()), 1, "Down entered row 1");
        assert!(
            ti.cursor_pos() > 0,
            "caret advanced into the second visual row"
        );
    }

    // Case `multiline_row_geometry_uses_unicode_cjk_breaks`.
    {
        let mut ti = TextInput::local()
            .multiline(true)
            .with_initial_value("東京京都大阪名古屋");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 70.0, 220.0));
        let mut renderer = TextRenderer::new();
        ti.build_lines(&mut renderer, 50.0);
        assert!(ti.lines.len() > 1, "narrow CJK input creates visual rows");
        let opportunities: Vec<_> = line_break_opportunities(ti.value())
            .map(|(byte, _)| byte)
            .collect();
        for row in ti.lines.iter().filter(|row| !row.hard_break) {
            assert!(
                opportunities.contains(&row.end),
                "soft row end {} is a UAX #14 opportunity: {opportunities:?}",
                row.end
            );
        }
    }
}

#[test]
fn clearing_direct_binding_preserves_interaction_and_cannot_write_stale_object() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut doc = AutoCommit::new();
    let stale = doc
        .put_object(&automerge::ROOT, "stale", ObjType::Text)
        .expect("stale text");
    doc.splice_text(&stale, 0, 0, "draft")
        .expect("seed stale text");
    let mut input = TextInput::local();
    input.bind_text(stale.clone());
    input.sync(&doc, &automerge::ROOT);
    input.set_focus(true);
    input.core.set_selection(1, 4);
    input.core.set_preedit("編", Some((0, "編".len())));
    input.scroll_x = 17.0;
    input.scroll_y = 23.0;

    let heads = doc.get_heads();
    input.clear_text_binding();
    assert_eq!(
        doc.get_heads(),
        heads,
        "detach does not mutate the document"
    );
    assert_eq!(input.bound_obj(), None);
    assert_eq!(input.value(), "draft");
    assert!(input.is_focused);
    assert_eq!(input.core.caret(), 4);
    assert_eq!(input.core.anchor(), 1);
    assert_eq!(input.core.preedit(), "編");
    assert_eq!(input.core.preedit_cursor(), Some((0, "編".len())));
    assert_eq!(input.scroll_x, 17.0);
    assert_eq!(input.scroll_y, 23.0);

    doc.splice_text(&stale, 0, 5, "historical")
        .expect("replace stale text");
    input.sync(&doc, &automerge::ROOT);
    assert_eq!(input.value(), "draft", "sync ignores the stale object");

    input.core.clear_preedit();
    input.core.set_caret(input.value().len());
    assert!(drive(
        &mut input,
        &mut doc,
        &char_key("!"),
        mods(false, false)
    ));
    assert_eq!(input.value(), "draft!");
    assert_eq!(doc.text(&stale).expect("stale text"), "historical");
}

#[test]
fn clearing_without_a_binding_is_a_noop_for_local_users() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut doc = AutoCommit::new();
    let state = crate::LocalState::new(String::from("local"));
    let mut local_input = TextInput::from_state(&state);
    local_input.clear_text_binding();
    assert!(Widget::perform_action(
        &mut local_input,
        &crate::a11y::Action::SetValue("updated".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(state.get(), "updated");
}

#[test]
fn controlled_local_input_opt_outs_survive_draw_without_dictionary_work() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local_with_language("en_US")
        .with_spellcheck(false)
        .with_autocorrect(false)
        .with_initial_value("teh mispeled words");
    input.set_focus(true);
    input.rect = Rect::new(0.0, 0.0, 360.0, 42.0);
    let mut scene = Scene::new();
    let mut renderer = crate::font_catalog::fixture_renderer();
    input.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        &Theme::dark(),
        &crate::PresenceCtx::empty(),
    );
    assert!(input.is_focused);
    assert!(!input.text.is_empty());
    assert!(input.rect.width() > 0.0);
    assert!(input.spell_dictionary.is_none());
    assert!(input.spell_cache_key.is_none());
    assert!(input.spell_spans.is_empty());
    assert!(input.autocorrection_preview_key.is_none());
    assert!(input.autocorrection_preview.is_none());
    assert_eq!(input.text, "teh mispeled words");
}
