//! The menu a field opens and what it offers a word: the context menu's
//! copy and edit rows, spelling eligibility, the squiggles, the suggestion at a
//! settled word, and autocorrection.
use super::super::*;
use super::drive;
use super::mods;
use super::right_click;
use super::suggesting_field;
use crate::synthetic_input::cursor_moved;
use crate::testing::dispatch;
use crate::text_edit::SUGGESTION_SETTLE;
use std::time::Duration;

/// A field laid out wide enough for a menu, holding `text`.
fn menu_field(text: &str) -> TextInput {
    let mut ti = TextInput::local().with_initial_value(text);
    ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
    ti
}

tilo_test_env::scenario! {
    fn a_fields_menu_offers_what_the_field_allows;

    phase the_menus_rows_follow_what_the_field_allows;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let services = crate::process::ProcessServices::null();
        let _host = crate::process::enter(&services);

        // A menu opened while the field was editable cannot edit once the field is read-only.
        {
            crate::text_selection::clipboard_copy("clipboard");
            let mut input = menu_field("source");
            input.select_all();
            input.open_text_menu_at([20.0, 20.0]);
            let cut = input
                .context_actions
                .iter()
                .position(|action| matches!(action, Some(CtxAction::Cut)))
                .unwrap();
            input.set_read_only(true);
            assert!(
                !input.dispatch_context_action(cut),
                "a menu opened before read-only cannot edit"
            );
            assert_eq!(input.value(), "source");
            assert_eq!(
                crate::text_selection::clipboard_paste().as_deref(),
                Some("clipboard")
            );
            input.context_menu.close();
        }

        // Right-click on read-only text offers Copy and nothing that writes.
        {
            let mut input = menu_field("source");
            input.set_read_only(true);
            let mut doc = AutoCommit::new();
            drive(
                &mut input,
                &mut doc,
                &cursor_moved(20.0, 20.0),
                mods(false, false),
            );
            drive(
                &mut input,
                &mut doc,
                &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
                mods(false, false),
            );
            assert!(input.context_menu.is_open());
            let labels = input.context_menu_labels();
            assert!(labels.iter().any(|label| label == "Copy"));
            assert!(
                !labels
                    .iter()
                    .any(|label| label == "Cut" || label == "Paste")
            );
            let copy = input
                .context_actions
                .iter()
                .position(|action| matches!(action, Some(CtxAction::Copy)))
                .unwrap();
            input.dispatch_context_action(copy);
            assert_eq!(
                crate::text_selection::clipboard_paste().as_deref(),
                Some("source")
            );
            assert_eq!(input.value(), "source");
        }

        // A right-click selects the word under it and opens the menu.
        {
            let mut ti = menu_field("hello world");
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            assert!(ti.selection_range().is_none(), "precondition: no selection");
            assert!(
                right_click(&mut ti, &mut doc, 20.0, 20.0),
                "right-click is consumed"
            );

            // No char_x_positions yet → cursor_from_x resolves to byte 0, selecting "hello".
            assert_eq!(ti.selected_str(), Some("hello"), "clicked word is selected");
            assert!(ti.context_menu.is_open(), "menu opened");

            let labels = ti.context_menu_labels();
            assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
        }

        // The row a dispatcher picks is the action it maps to.
        {
            let mut ti = menu_field("hello world");
            // Pre-select "world" so Cut/Copy are present and the selection survives a copy.
            ti.core.set_selection(6, 11);

            ti.open_context_menu([50.0, 18.0]);
            let labels = ti.context_menu_labels();
            let copy_idx = labels
                .iter()
                .position(|l| l == "Copy")
                .expect("Copy row present");
            // Labels skip separators, but Cut/Copy precede any separator so indices align here.
            let item_idx = ti
                .context_menu
                .items()
                .iter()
                .position(|i| i.label() == Some("Copy"))
                .unwrap();
            assert_eq!(
                ti.context_actions[item_idx],
                Some(CtxAction::Copy),
                "Copy row maps to the Copy action (label idx {copy_idx})"
            );

            assert!(ti.dispatch_context_action(item_idx), "dispatch ran");
            assert!(!ti.context_menu.is_open(), "menu closed after dispatch");
            // Copy preserves the selection (it's a read-only action).
            assert_eq!(ti.selected_str(), Some("world"));
        }

        // Cutting removes what was selected.
        {
            let mut ti = menu_field("hello world");
            ti.core.set_selection(0, 5); // "hello"
            ti.open_context_menu([10.0, 18.0]);
            let cut_idx = ti
                .context_menu
                .items()
                .iter()
                .position(|i| i.label() == Some("Cut"))
                .expect("Cut row present");
            assert_eq!(ti.context_actions[cut_idx], Some(CtxAction::Cut));
            ti.dispatch_context_action(cut_idx);
            assert_eq!(ti.value(), " world", "cut removed the selected word");
        }

        // A click outside the open menu dismisses it.
        {
            let mut ti = menu_field("hello world");
            ti.set_focus(true);
            ti.open_context_menu([10.0, 18.0]);
            assert!(ti.context_menu.is_open());

            let moved = crate::synthetic_input::cursor_moved(280.0, 38.0);
            let press = crate::synthetic_input::mouse_press();
            let handled = dispatch(&mut ti, &moved) | dispatch(&mut ti, &press);
            assert!(handled, "click consumed while menu open");
            assert!(
                !ti.context_menu.is_open(),
                "menu dismissed by outside click"
            );
        }

        // A misspelling offers its correction, and taking it replaces the word.
        {
            let mut ti = TextInput::local()
                .with_spell_language("en_US")
                .with_initial_value("teh");
            ti.core.set_selection(0, 3);
            ti.open_context_menu([10.0, 10.0]);

            let item_idx = ti
                .context_actions
                .iter()
                .position(|action| {
                    matches!(
                        action,
                        Some(CtxAction::ApplySpelling { replacement, .. })
                            if replacement == "the"
                    )
                })
                .expect("the correction is offered");
            assert!(ti.dispatch_context_action(item_idx));
            assert_eq!(ti.value(), "the");
        }

        // A field's own action on the selection is reported without changing the text.
        {
            let mut ti = TextInput::local()
                .with_spellcheck(false)
                .with_text_action("translate", "Translate")
                .with_initial_value("hello world");
            ti.core.set_selection(0, 5);
            ti.open_context_menu([10.0, 10.0]);
            let item_idx = ti
                .context_actions
                .iter()
                .position(|action| matches!(action, Some(CtxAction::TextAction(0))))
                .expect("registered action is in the context menu");

            assert!(ti.dispatch_context_action(item_idx));
            assert_eq!(
                ti.take_text_action_request(),
                Some(TextActionRequest {
                    id: "translate".to_string(),
                    text: "hello".to_string(),
                    range: (0, 5),
                })
            );
            assert_eq!(ti.value(), "hello world", "dispatch itself is read-only");
        }
    }
}

tilo_test_env::scenario! {
    fn spellcheck_marks_and_suggests_as_words_settle;

    phase spellcheck_squiggles_appear_at_word_boundaries_and_after_settling;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `spelling_checks_immediately_but_only_hides_the_word_being_typed`.
        {
            let mut ti = TextInput::local()
                .with_spell_language("en_US")
                .with_initial_value("teh wrold");
            ti.set_focus(true);
            ti.core.set_caret(ti.text.len());
            ti.typing.touch();

            ti.refresh_spelling();
            // Dictionary warm-up can dominate this unit test in an unoptimised
            // build. Model a fresh edit after checking to test presentation timing.
            ti.typing.touch();

            assert_eq!(ti.spell_spans, vec![(0, 3), (4, 9)]);
            assert!(ti.spelling_span_is_visible(0, 3));
            assert!(!ti.spelling_span_is_visible(4, 9));
            assert!(!Widget::needs_animation_frame(&ti));
            assert!(Widget::next_wake_deadline(&ti).is_some());
        }

        // Case `a_word_becomes_visible_immediately_when_a_boundary_is_typed`.
        {
            let mut ti = TextInput::local()
                .with_spell_language("en_US")
                .with_initial_value("wrold ");
            ti.set_focus(true);
            ti.core.set_caret(ti.text.len());
            ti.typing.touch();

            ti.refresh_spelling();

            assert_eq!(ti.spell_spans, vec![(0, 5)]);
            assert!(ti.actively_edited_word().is_none());
            assert!(ti.spelling_span_is_visible(0, 5));
        }

        // Case `active_word_is_revealed_after_the_short_settle_period`.
        {
            let mut ti = TextInput::local()
                .with_spell_language("en_US")
                .with_initial_value("wrold");
            ti.set_focus(true);
            ti.core.set_caret(ti.text.len());
            if let Some(at) = crate::time::now()
                .checked_sub(SPELL_EDIT_SETTLE)
                .and_then(|t| t.checked_sub(Duration::from_millis(1)))
            {
                ti.typing.touch_at(at);
            }

            ti.refresh_spelling();

            assert_eq!(ti.spell_spans, vec![(0, 5)]);
            assert!(ti.spelling_span_is_visible(0, 5));
            assert!(!Widget::needs_animation_frame(&ti));
        }
    }

    phase a_settled_word_offers_its_suggestion_on_a_pause_wherever_the_caret_sets;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut ti = suggesting_field("so beautifull");
        let start = crate::time::now();

        ti.refresh_suggestion_at(start);
        assert!(
            ti.autocorrection_preview.is_none(),
            "nothing is offered while the word may still be growing"
        );
        assert_eq!(ti.caret_word.deadline(), Some(start + SUGGESTION_SETTLE));

        ti.refresh_suggestion_at(start + SUGGESTION_SETTLE - Duration::from_millis(1));
        assert!(ti.autocorrection_preview.is_none());

        ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
        let preview = ti
            .autocorrection_preview
            .clone()
            .expect("a settled misspelled word gets a suggestion");
        assert_eq!(
            (preview.start, preview.end, preview.original.as_str()),
            (3, 13, "beautifull")
        );
        assert_eq!(preview.replacement, "beautiful");
        assert!(
            !preview.automatic,
            "an ordinary suggestion, not autocorrect"
        );
        assert!(!ti.spelling_span_is_visible(3, 13));

        // A boundary does not apply an ordinary suggestion …
        let mut boundary = suggesting_field("so beautifull");
        boundary.refresh_suggestion_at(start);
        boundary.refresh_suggestion_at(start + SUGGESTION_SETTLE);
        assert!(boundary.autocorrection_preview.is_some());
        boundary.insert_word_boundary(" ");
        assert_eq!(boundary.value(), "so beautifull ");

        // … a click on its chip does.
        assert!(ti.accept_suggestion());
        assert_eq!(ti.value(), "so beautiful");
        assert_eq!(ti.cursor_pos(), "so beautiful".len());
        assert!(ti.autocorrection_preview.is_none());

        // Moving back into an earlier word settles it again; the word it left keeps its
        // suggestion while the caret is in it, and a selection or an unfocused field offers nothing.
        let mut ti = suggesting_field("beautifull day");
        let start = crate::time::now();
        ti.refresh_suggestion_at(start);
        ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
        assert!(
            ti.autocorrection_preview.is_none(),
            "`day` is spelled right"
        );

        // Back into the first word: it settles after the same short wait.
        let back = start + SUGGESTION_SETTLE * 2;
        ti.set_cursor_pos(4);
        ti.refresh_suggestion_at(back);
        assert!(ti.autocorrection_preview.is_none());
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
        let preview = ti.autocorrection_preview.clone().expect("suggestion");
        assert_eq!((preview.start, preview.end), (0, 10));
        assert_eq!(preview.replacement, "beautiful");

        // Moving within the word keeps the suggestion; leaving it clears it.
        ti.set_cursor_pos(7);
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
        assert!(ti.autocorrection_preview.is_some());
        ti.set_cursor_pos(10);
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
        assert!(
            ti.autocorrection_preview.is_some(),
            "the word's end is in it"
        );
        ti.set_cursor_pos(ti.text.len());
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE);
        assert!(ti.autocorrection_preview.is_none());

        // A selection or an unfocused field offers nothing.
        ti.set_selection(0, 4);
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE * 3);
        assert!(ti.autocorrection_preview.is_none());
        ti.set_cursor_pos(4);
        ti.set_focus(false);
        ti.refresh_suggestion_at(back + SUGGESTION_SETTLE * 4);
        assert!(ti.autocorrection_preview.is_none());

        // The settling word schedules its own wake, so the suggestion appears without further input.
        let mut ti = suggesting_field("so beautifull");
        // Far enough ahead that a slow test run never sees the deadline pass.
        let now = crate::time::now() + Duration::from_secs(3600);
        ti.refresh_suggestion_at(now);
        assert_eq!(
            Widget::next_wake_deadline(&ti),
            Some(now + SUGGESTION_SETTLE),
            "the suggestion appears without further input"
        );
        let mut off = TextInput::local()
            .with_spellcheck(false)
            .with_autocorrect(false)
            .with_initial_value("so beautifull");
        off.set_focus(true);
        off.refresh_suggestion_at(now);
        assert_eq!(Widget::next_wake_deadline(&off), None);
    }

    phase autocorrect_previews_applies_restores_and_respects_opt_outs;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        // Case `obvious_typo_autocorrects_at_boundary_and_backspace_restores_it`.
        {
            let mut ti = TextInput::local()
                .with_spellcheck(true)
                .with_autocorrect(true)
                .with_spell_language("en_US")
                .with_initial_value("teh");
            ti.core.set_caret(ti.text.len());
            ti.insert_word_boundary(" ");
            assert_eq!(ti.value(), "the ");
            assert!(ti.last_autocorrection.is_some());

            let mut handled = false;
            ti.handle_backspace_key(false, &mut handled);
            assert!(handled);
            assert_eq!(ti.value(), "teh");
            assert!(ti.last_autocorrection.is_none());
        }

        // Case `autocorrection_is_previewed_before_the_boundary_applies_it`.
        {
            let mut ti = TextInput::local()
                .with_autocorrect(true)
                .with_spell_language("en_US")
                .with_initial_value("adress");
            ti.set_focus(true);
            ti.core.set_caret(ti.text.len());

            ti.refresh_autocorrection_preview();
            ti.refresh_spelling();

            let preview = ti
                .autocorrection_preview
                .as_ref()
                .expect("a high-confidence correction is previewed");
            assert_eq!(preview.original, "adress");
            assert_eq!(preview.replacement, "address");
            assert!(!ti.spelling_span_is_visible(0, 6));

            ti.insert_word_boundary(" ");
            assert_eq!(ti.value(), "address ");
        }

        // Case `autocorrect_respects_field_opt_out_and_personal_vocabulary`.
        {
            let mut off = TextInput::local()
                .with_autocorrect(false)
                .with_spell_language("en_US")
                .with_initial_value("teh");
            off.core.set_caret(off.text.len());
            off.insert_word_boundary(" ");
            assert_eq!(off.value(), "teh ");

            let mut known = TextInput::local()
                .with_autocorrect(true)
                .with_spell_language("en_US")
                .with_initial_value("teh");
            known.core.set_caret(known.text.len());
            known.spell_active = true;
            known.refresh_personal_spelling();
            known.spell_dictionary = Some(spellcheck::Dictionary::from_words(["teh"]));
            known.insert_word_boundary(" ");
            assert_eq!(known.value(), "teh ");
        }
    }
}

/// The field's menu ends in "Send to…", and a click on it opens the send-to
/// box beside the menu, which stays open: the field routes the pointer
/// through the menu as it routes the keys, so the row a click takes is the
/// row Enter takes.
#[test]
fn a_click_on_send_to_opens_the_box_in_the_fields_menus_place() {
    let _settled = crate::motion::testing::settled();
    crate::shared_selection::set_send_host_available(true);
    let mut ti = TextInput::local().with_initial_value("hello world");
    ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
    ti.set_focus(true);
    let mut doc = AutoCommit::new();
    crate::widget::context_menu::note_input(&crate::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));
    assert!(right_click(&mut ti, &mut doc, 20.0, 20.0));
    let send = ti.context_menu.items().len() - 1;
    assert_eq!(
        ti.context_menu.items()[send].label(),
        Some(crate::i18n::widget_text("menu-send").as_str()),
        "the field's menu ends in Send to…"
    );
    let row = ti
        .context_menu
        .item_rect(send)
        .expect("the send row is drawn")
        .center();
    for event in [
        cursor_moved(row.x, row.y),
        crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
        crate::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
    ] {
        drive(&mut ti, &mut doc, &event, mods(false, false));
    }
    crate::shared_selection::set_send_host_available(false);
    assert_eq!(
        Widget::modal_capture(ti.context_menu.as_ref()),
        Some(ti.context_menu.send_to().rect()),
        "the menu went as for any row, and the box took its place"
    );
    let box_ = ti.context_menu.send_to();
    assert!(box_.is_open(), "the box opened, typing");
    assert_eq!(
        box_.subjects()
            .first()
            .and_then(|about| about.anchor.excerpt.as_deref()),
        Some("hello"),
        "about the word the menu was opened on"
    );
    assert_eq!(ti.value(), "hello world", "nothing beneath was edited");
}

/// A misspelled word the person selected offers its corrections, can be corrected as one step,
/// and can be kept: ignored in this field, or added to the person's dictionary.
#[test]
fn a_misspelled_word_offers_corrections_and_can_be_kept() {
    let services = crate::process::ProcessServices::null();
    let _host = crate::process::enter(&services);

    let mut input = TextInput::local()
        .with_spell_language("en-US")
        .with_initial_value("teh");
    input.set_focus(true);
    input.spell_dictionary = Some(spellcheck::Dictionary::from_words(["the", "tea", "ten"]));
    input.select_all();
    let menu = input.spelling_menu().expect("a misspelled word has a menu");
    assert_eq!(menu.word, "teh");
    assert_eq!((menu.start, menu.end), (0, 3));
    assert!(
        menu.suggestions.iter().any(|word| word == "the"),
        "the corrections are offered: {:?}",
        menu.suggestions
    );

    assert!(input.replace_spelling(menu.start, menu.end, "teh", "the"));
    assert_eq!(input.value(), "the", "the word is corrected");
    assert!(
        input.spelling_menu().is_none(),
        "a word the dictionary knows is not misspelled"
    );

    let mut kept = TextInput::local()
        .with_spell_language("en-US")
        .with_initial_value("qwordz");
    kept.set_focus(true);
    kept.select_all();
    assert!(kept.spelling_menu().is_some(), "qwordz is misspelled");
    kept.ignore_word_in_field("qwordz");
    assert!(
        kept.spelling_menu().is_none(),
        "the word is kept in this field"
    );

    // A field that does not spell prose (a secret) has no spelling menu at all.
    let mut secret = TextInput::local()
        .with_secret(true)
        .with_spell_language("en-US")
        .with_initial_value("teh");
    secret.set_focus(true);
    secret.select_all();
    assert!(secret.spelling_menu().is_none());
}
