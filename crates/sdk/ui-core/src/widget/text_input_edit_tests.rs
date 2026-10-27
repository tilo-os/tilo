//! What an edit does to the text: the splice, replace and repeat
//! primitives, a secret field's masking, what assistive technology and a binding
//! see, and the touch selection.
use super::super::*;
use super::draw_to_populate;
use super::drive;
use super::mods;
use super::touch;
use super::x_for_byte;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
use crate::testing::Dispatch;
use automerge::ObjType;
use automerge::transaction::Transactable as _;
use winit::event::TouchPhase;

#[test]
fn editing_primitives_splice_replace_filter_and_repeat() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `delete_selection_removes_range_and_collapses`.
    {
        let mut ti = TextInput::local();
        ti.text = "hello world".to_string();
        ti.core.set_selection(6, 11);
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.delete_selection();
        assert_eq!(ti.text, "hello ");
        assert_eq!(ti.core.caret(), 6);
        assert!(!ti.has_selection());
    }

    // Case `insert_at_cursor_splices_and_marks_dirty`.
    {
        let mut ti = TextInput::local().with_initial_value("hi world");
        ti.set_cursor_pos(2); // just after "hi"
        ti.insert_at_cursor(" there");
        assert_eq!(ti.value(), "hi there world");
        assert_eq!(
            ti.cursor_pos(),
            "hi there".len(),
            "caret advances past the insert"
        );
        assert_eq!(
            ti.take_dirty().as_deref(),
            Some("hi there world"),
            "the edit is observable"
        );
    }

    // Case `character_filter_applies_before_replacing_the_selection`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("A1")
            .with_char_filter(|ch| !ch.is_whitespace());
        ti.select_all();
        ti.insert_at_cursor(" B 2 : D 4 ");
        assert_eq!(ti.value(), "B2:D4");
        assert_eq!(ti.cursor_pos(), "B2:D4".len());
        assert_eq!(ti.take_dirty().as_deref(), Some("B2:D4"));
    }

    // Case `insert_str_replaces_selection`.
    // The path Ctrl+V uses when pasting over selected text.
    {
        let mut ti = TextInput::local();
        ti.text = "hello world".to_string();
        ti.core.set_selection(6, 11);
        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "text_1", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hello world");
        ti.insert_str("there");
        assert_eq!(ti.text, "hello there");
        assert_eq!(ti.core.caret(), "hello there".len());
        assert!(!ti.has_selection());
    }

    // Case `typing_replaces_selection_via_handle_event`.
    {
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        drive(&mut ti, &mut doc, &char_key("a"), mods(true, false)); // select all
        drive(&mut ti, &mut doc, &char_key("Z"), mods(false, false)); // type over it
        assert_eq!(ti.value(), "Z", "typing over a full selection replaces it");
    }

    // Case `key_text_preserves_shift_and_german_layout_characters`.
    {
        let mut ti = TextInput::local();
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        // `KeyEvent::text` is the layout/IME-resolved text. The widget must
        // insert it verbatim rather than reconstructing ASCII from a physical
        // key, which would lose Shift and German/AltGr punctuation.
        for text in ["A", "Ä", "@", "?"] {
            drive(&mut ti, &mut doc, &char_key(text), mods(false, false));
        }
        assert_eq!(ti.value(), "AÄ@?");
    }

    // Case `repeated_backspace_and_delete_press_events_keep_editing`.
    {
        let mut doc = AutoCommit::new();
        let mut backspace = TextInput::local().with_initial_value("abcd");
        backspace.set_focus(true);
        let first = named_key(NamedKey::Backspace);
        let mut repeat = named_key(NamedKey::Backspace);
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeat {
            event.repeat = true;
        }
        drive(&mut backspace, &mut doc, &first, mods(false, false));
        drive(&mut backspace, &mut doc, &repeat, mods(false, false));
        assert_eq!(
            backspace.value(),
            "ab",
            "held Backspace deletes on repeat events"
        );

        let mut delete = TextInput::local().with_initial_value("abcd");
        delete.set_focus(true);
        delete.set_cursor_pos(0);
        let first = named_key(NamedKey::Delete);
        let mut repeat = named_key(NamedKey::Delete);
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeat {
            event.repeat = true;
        }
        drive(&mut delete, &mut doc, &first, mods(false, false));
        drive(&mut delete, &mut doc, &repeat, mods(false, false));
        assert_eq!(delete.value(), "cd", "held Delete deletes on repeat events");
    }
}

#[test]
fn secret_input_masks_rendering_and_accessibility_but_keeps_the_value() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `secret_input_masks_glyphs_but_keeps_value`.
    // Proven end-to-end through a real Automerge `Text` object.
    {
        let mut ti = TextInput::local().with_secret(true);

        let mut doc = AutoCommit::new();
        let obj_id = doc
            .put_object(&automerge::ROOT, "password", ObjType::Text)
            .unwrap();
        let _ = doc.splice_text(&obj_id, 0, 0, "hunter2");
        ti.bind_text(obj_id);

        ti.sync(&doc, &automerge::ROOT);
        assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

        assert_eq!(ti.display_text(), "•••••••");
        // The masked prefix tracks character count, not bytes.
        assert_eq!(ti.display_prefix(3), "•••");

        let mut plain = TextInput::new(Vec::new());
        plain.text = "hunter2".to_string();
        assert_eq!(plain.display_text(), "hunter2");
        assert_eq!(plain.display_prefix(3), "hun");
    }

    // Case `local_secret_mirrors_without_entering_the_document`.
    {
        let buffer = Arc::new(Mutex::new(String::new()));
        let mut input = TextInput::local()
            .with_external_buffer(Arc::clone(&buffer))
            .with_secret(true);
        input.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
        input.text = "correct horse battery staple".into();
        // An edit not yet measured nor published.
        input.dirty = true;
        input.layout_dirty = true;
        let document = AutoCommit::new();
        input.sync(&document, &automerge::ROOT);

        assert_eq!(&*buffer.lock().unwrap(), "correct horse battery staple");
        assert!(
            input.layout_needs_measure(),
            "publication must not consume layout invalidation"
        );
        buffer.lock().unwrap().clear();
        input.sync(&document, &automerge::ROOT);
        assert!(
            buffer.lock().unwrap().is_empty(),
            "an acknowledged secret must not be republished"
        );
        input.core.set_caret(input.text.len());
        input.insert_at_cursor("new draft");
        input.sync(&document, &automerge::ROOT);
        assert_eq!(
            &*buffer.lock().unwrap(),
            input.value(),
            "subsequent edits still publish"
        );
        assert!(buffer.lock().unwrap().ends_with("new draft"));
        assert!(document.keys(&automerge::ROOT).next().is_none());
    }

    // Case `accessibility_secret_input_masks_value_and_sets_protected`.
    {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "a secret value must not leak into the a11y tree"
        );
        assert!(node.state.protected, "secret input is marked protected");
        assert_eq!(
            node.text_selection, None,
            "selection offsets must not reveal a secret's length"
        );
    }

    // Case `password_and_key_kinds_never_leak_to_a11y_even_unmasked`.
    for kind in [TextInputKind::Password, TextInputKind::Secret] {
        let mut ti = TextInput::new(Vec::new()).with_kind(kind);
        ti.text = "JBSWY3DPEHPK3PXP".to_string();
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.value, None, "{kind:?}: a key is not exported");
        assert!(node.state.protected, "{kind:?}: a key field is protected");
        assert_eq!(node.text_selection, None, "{kind:?}: no selection offsets");
        assert_eq!(
            Widget::ime_content(&ti),
            Some(crate::ime::InputContent::PASSWORD),
            "{kind:?}: no input-method suggestions learn the key"
        );
    }

    // Case `secret_reveal_toggle_shows_glyphs_but_never_leaks_to_a11y`.
    {
        let mut ti = TextInput::new(Vec::new()).with_secret(true);
        ti.text = "hunter2".to_string();
        assert_eq!(ti.display_text(), "•••••••", "masked by default");

        ti.toggle_reveal();
        assert!(ti.is_revealed());
        assert_eq!(
            ti.display_text(),
            "hunter2",
            "revealed shows the real glyphs"
        );
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(
            node.value, None,
            "revealed value still must not reach the a11y tree"
        );
        assert!(
            node.state.protected,
            "still marked protected while revealed"
        );

        ti.toggle_reveal();
        assert_eq!(ti.display_text(), "•••••••", "toggles back to masked");
    }

    // Case `secret_caret_moves_over_the_real_value`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("s3cr3t");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        ti.set_cursor_pos(0);
        drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowRight),
            mods(false, false),
        );
        assert_eq!(ti.cursor_pos(), 1, "Right moved one real char boundary");
    }
}

#[test]
fn a_secret_never_reaches_the_clipboard() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let services = crate::host::HostServices::null();
    let _host = crate::host::enter(&services);
    let clipboard = || crate::text_selection::clipboard_paste();

    for revealed in [false, true] {
        {
            crate::text_selection::clipboard_copy("before");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t");
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            drive(&mut ti, &mut doc, &char_key("a"), mods(true, false));
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "Select All still works");
            drive(&mut ti, &mut doc, &char_key("c"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            drive(&mut ti, &mut doc, &char_key("x"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            assert_eq!(ti.value(), "s3cr3t", "a refused cut keeps the value");
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "and the selection");
        }

        {
            crate::text_selection::clipboard_copy("pasted");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t")
                .with_text_action("translate", "Translate");
            if revealed {
                ti.toggle_reveal();
            }
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            ti.select_all();
            ti.open_context_menu([20.0, 18.0]);
            let labels = ti.context_menu_labels();
            assert!(!labels.contains(&"Cut".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Copy".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Translate".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Paste".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
        }

        {
            crate::text_selection::clipboard_copy("hunter2");
            let mut ti = TextInput::local().with_secret(true);
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();
            drive(&mut ti, &mut doc, &char_key("v"), mods(true, false));
            assert_eq!(ti.value(), "hunter2");
        }
    }
}

#[test]
fn secret_field_carries_a_show_hide_control_off_the_tab_order() {
    use crate::a11y::{Action, Role};

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `secret_is_hidden_by_default_with_a_named_show_control`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        assert!(!ti.is_revealed(), "a secret starts hidden");
        assert_eq!(ti.display_text(), "•••••••");
        assert!(ti.has_reveal_control());
        let control = ti.reveal_control_rect().unwrap();
        assert_eq!(
            control,
            Rect::new(200.0, 0.0, 240.0, 40.0),
            "trailing square"
        );

        let children = Widget::children(&ti);
        assert_eq!(children.len(), 1, "the control is a real child widget");
        let child = children.first().unwrap();
        assert!(
            !child.is_focusable(),
            "an inline accessory is not a Tab stop"
        );
        let node = child.accessibility().unwrap();
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Show password"));
        assert_eq!(
            Widget::tooltip_target_at(&ti, control.center()).map(|(_, tip)| tip),
            Some("Show password".to_string())
        );
        assert_eq!(
            Widget::cursor_shape(&ti, control.center()),
            Some(winit::window::CursorIcon::Pointer)
        );
    }

    // Case `activating_the_control_toggles_glyphs_and_its_name_only`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        let mut doc = AutoCommit::new();
        let activated = Widget::children_mut(&mut ti)
            .into_iter()
            .next()
            .unwrap()
            .perform_action(
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
        assert!(activated, "Enter/Space and AT activation toggle");
        assert!(ti.is_revealed());
        assert_eq!(ti.display_text(), "hunter2");
        let child = Widget::children(&ti).into_iter().next().unwrap();
        assert_eq!(
            child.accessibility().unwrap().name.as_deref(),
            Some("Hide password")
        );
        let field = Widget::accessibility(&ti).unwrap();
        assert_eq!(field.value, None, "revealing never exposes the value");
        assert!(field.state.protected);
        assert!(Widget::text_review(&ti).is_none());
        assert!(
            doc.keys(&automerge::ROOT).next().is_none(),
            "nothing persisted"
        );

        let _ = Widget::children_mut(&mut ti)
            .into_iter()
            .next()
            .unwrap()
            .perform_action(
                &Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
        assert!(!ti.is_revealed(), "a second activation hides it again");
    }

    // Case `pointer_toggle_keeps_focus_and_caret_in_the_field`.
    {
        let mut ti = TextInput::local()
            .with_secret(true)
            .with_initial_value("hunter2");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        ti.set_focus(true);
        draw_to_populate(&mut ti);
        let caret = ti.cursor_pos();
        let mut doc = AutoCommit::new();
        let target = ti.reveal_control_rect().unwrap().center();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(target.x, target.y),
            mods(false, false),
        );

        let mut sender = Dispatch::new().doc(&mut doc);
        let press = sender.send(&mut ti, &mouse_press());
        let release = sender.send(&mut ti, &mouse_release());
        let handled = press.handled | release.handled;
        let focus_target = release.focus_target.or(press.focus_target);
        assert!(handled);
        assert!(ti.is_revealed(), "a click reveals");
        assert_eq!(focus_target, None, "the click does not move focus");
        assert!(ti.is_focused());
        assert_eq!(ti.cursor_pos(), caret, "the caret stays where it was");

        let tap = Point::new(target.x, target.y);
        drive(
            &mut ti,
            &mut doc,
            &touch(TouchPhase::Started, 7, tap),
            mods(false, false),
        );
        drive(
            &mut ti,
            &mut doc,
            &touch(TouchPhase::Ended, 7, tap),
            mods(false, false),
        );
        assert!(!ti.is_revealed(), "a tap hides it again");
    }

    // Case `tab_moves_from_field_to_field_past_the_control`.
    {
        let mut ti = TextInput::local().with_secret(true);
        let mut taffy = TaffyTree::new();
        let mut node_map = std::collections::HashMap::new();
        let root = ti.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let (order, trapped) = crate::focus_navigation::order(root, &ti, &taffy);
        assert!(!trapped);
        assert_eq!(order, [root], "the field alone, not its show/hide control");
    }

    // Case `only_single_line_secrets_carry_the_control`.
    {
        let plain = TextInput::local();
        assert!(!plain.has_reveal_control());
        assert!(Widget::children(&plain).is_empty());
        let multiline = TextInput::local().with_secret(true).multiline(true);
        assert!(!multiline.has_reveal_control());
    }

    // Case `a_disabled_secret_offers_no_reachable_control`.
    {
        let mut ti = TextInput::local().with_secret(true).disabled(true);
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
        let child = Widget::children(&ti).into_iter().next().unwrap();
        assert!(!child.is_focusable());
        assert!(child.accessibility().unwrap().state.disabled);
    }
}

#[test]
fn accessibility_exposes_role_name_value_and_grapheme_actions() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `accessibility_reports_text_input_role_and_value`.
    {
        use crate::a11y::{AccessibleTextSelection, Role};
        let mut ti = TextInput::new(Vec::new());
        ti.text = "hello".to_string();
        ti.core.set_selection(1, 4);
        let node = Widget::accessibility(&ti).unwrap();
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("hello"));
        assert!(node.state.focusable, "enabled input is focusable");
        assert!(!node.state.disabled);
        assert!(!node.state.protected);
        assert_eq!(
            node.text_selection,
            Some(AccessibleTextSelection {
                anchor: 1,
                focus: 4,
            })
        );
    }

    // Case `accessibility_uses_explicit_name_not_placeholder`.
    {
        let input = TextInput::local()
            .with_placeholder("name@example.com")
            .with_accessible_name("Email address");
        let node = Widget::accessibility(&input).unwrap();
        assert_eq!(node.name.as_deref(), Some("Email address"));
        assert_ne!(node.name.as_deref(), Some(input.placeholder()));
    }

    // Case `accessibility_actions_use_extended_grapheme_indices_and_leave_the_document_to_the_owner`.
    {
        use crate::a11y::Action;

        let family = "👩‍👩‍👧‍👦";
        let original = format!("a{family}b");
        let mut ti = TextInput::local();
        ti.text = original.clone();
        let mut doc = AutoCommit::new();
        let obj = doc
            .put_object(&automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        doc.splice_text(&obj, 0, 0, &original).unwrap();
        ti.bind_text(obj.clone());

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetSelection {
                anchor: 1,
                focus: 2,
            },
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            ti.selection_range_public(),
            Some((1, 1 + family.len())),
            "one AccessKit character selects the whole ZWJ family"
        );

        assert!(Widget::perform_action(
            &mut ti,
            &Action::SetValue("new value".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(ti.value(), "new value");
        assert_eq!(doc.text(&obj).unwrap(), original, "the owner writes it");
        assert_eq!(ti.take_dirty().as_deref(), Some("new value"));
    }

    // Case `multiline_sets_accessibility_flag`.
    {
        let ml = TextInput::local().multiline(true);
        assert!(
            Widget::accessibility(&ml).unwrap().state.multiline,
            "multiline flagged in a11y"
        );
        let single = TextInput::local();
        assert!(!Widget::accessibility(&single).unwrap().state.multiline);
    }
}

#[test]
fn a_bound_text_is_only_read_and_edits_wait_for_the_owner() {
    use crate::a11y::Action;

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut document = AutoCommit::new();
    let title = document
        .put_object(&automerge::ROOT, "title", ObjType::Text)
        .unwrap();
    document.splice_text(&title, 0, 0, "Draft").unwrap();
    let mut input = TextInput::local();
    input.bind_text(title.clone());
    input.sync(&document, &automerge::ROOT);
    assert_eq!(input.value(), "Draft");
    assert!(
        input.take_dirty().is_none(),
        "showing the text is not an edit"
    );

    assert!(Widget::perform_action(
        &mut input,
        &Action::SetSelection {
            anchor: 0,
            focus: 0,
        },
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &automerge::ROOT,
    ));
    assert_eq!(input.value(), "Draft", "moving the caret changes no text");

    assert!(Widget::perform_action(
        &mut input,
        &Action::SetValue("Project Atlas".into()),
        &[],
        &mut crate::widget::Edits::lend(&mut document),
        &automerge::ROOT,
    ));
    assert_eq!(
        document.text(&title).unwrap(),
        "Draft",
        "the input never writes the document"
    );
    assert_eq!(input.take_dirty().as_deref(), Some("Project Atlas"));
}

#[test]
fn touch_owns_text_selection_until_release() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `touch_places_caret_drags_selection_and_releases_ownership`.
    {
        let mut input = TextInput::local().with_initial_value("abcdef");
        input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 44.0));
        draw_to_populate(&mut input);
        let mut doc = AutoCommit::new();
        let start = Point::new(x_for_byte(&input, 1), 22.0);
        let end = Point::new(x_for_byte(&input, 5), 22.0);

        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 7, start),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Moved, 7, end),
            Modifiers::default()
        ));
        assert_eq!(input.selection_range(), Some((1, 5)));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Ended, 7, end),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, None);
        assert!(!input.is_dragging);
    }

    // Case `secondary_touch_cannot_steal_text_selection`.
    {
        let mut input = TextInput::local().with_initial_value("abcdef");
        input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 44.0));
        draw_to_populate(&mut input);
        let mut doc = AutoCommit::new();
        let point = Point::new(x_for_byte(&input, 2), 22.0);
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 7, point),
            Modifiers::default()
        ));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Started, 8, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Ended, 8, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, Some(7));
        assert!(drive(
            &mut input,
            &mut doc,
            &touch(TouchPhase::Cancelled, 7, point),
            Modifiers::default()
        ));
        assert_eq!(input.active_touch, None);
    }
}
