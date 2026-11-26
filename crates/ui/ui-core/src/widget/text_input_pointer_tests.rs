//! How the pointer and the keys move through a field's text: clicks,
//! drags, word and range selection, arrows, Home and End, and the measurement a
//! draw leaves behind for them.
use super::super::*;
use super::command_mods;
use super::draw_to_populate;
use super::drive;
use super::ml_input;
use super::mods;
use super::test_node;
use super::x_for_byte;
use tilo_ui_base::{synthetic_input::char_key, synthetic_input::cursor_moved, synthetic_input::mouse_press, synthetic_input::mouse_release, synthetic_input::named_key};
use automerge::ObjType;
use automerge::transaction::Transactable as _;

/// The one action the test binding names, allowed.
fn answered() -> std::collections::HashSet<tilo_actions::ActionAddress> {
    std::collections::HashSet::from([tilo_actions::ActionAddress::new("test:control", "set")])
}

#[test]
fn action_bindings_own_edits_and_accessibility_set_value() {
    let _settled = tilo_ui_base::motion::testing::settled();
    {
        let mut doc = AutoCommit::new();
        let text = doc
            .put_object(&automerge::ROOT, "note", ObjType::Text)
            .expect("note text");
        doc.splice_text(&text, 0, 0, "before")
            .expect("seed note text");
        let mut input = TextInput::local();
        input.bind_text(text.clone());
        input.sync(&doc, &automerge::ROOT);
        input.set_focus(true);
        input.set_cursor_pos("before".len());

        assert!(drive(
            &mut input,
            &mut doc,
            &char_key("!"),
            mods(false, false)
        ));
        assert_eq!(input.value(), "before!", "optimistic text remains visible");
        assert_eq!(doc.text(&text).expect("document text"), "before");
    }

    {
        let mut doc = AutoCommit::new();
        let object = doc
            .put_object(&automerge::ROOT, "legacy", ObjType::Text)
            .unwrap();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()))
            .on_action(tilo_ui_base::widget::test_string_action_binding());
        input.bind_text(object.clone());
        tilo_ui_base::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::new(),
        );
        assert!(input.is_disabled());
        let allowed = answered();
        tilo_ui_base::widget::controls::refresh_control_action_availability(&mut input, &allowed);
        assert!(!input.is_disabled());
        assert!(Widget::perform_action(
            &mut input,
            &tilo_ui_base::a11y::Action::SetValue("Project Atlas".into()),
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(doc.text(&object).unwrap(), "");
        assert_eq!(
            tilo_ui_base::widget::controls::take_control_invocation(&mut input)
                .unwrap()
                .arguments["value"],
            "Project Atlas"
        );
        assert_eq!(
            *changes.borrow(),
            ["Project Atlas"],
            "the local callback runs beside the bound action"
        );
        doc.splice_text(&object, 0, 0, "Remote title").unwrap();
        Widget::sync(&mut input, &doc, &automerge::ROOT);
        assert_eq!(input.value(), "Remote title");
        assert_eq!(changes.borrow().len(), 1, "remote text is not a user edit");
    }

    {
        let mut doc = AutoCommit::new();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_action(tilo_ui_base::widget::test_string_action_binding())
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        let allowed = answered();
        tilo_ui_base::widget::controls::refresh_control_action_availability(&mut input, &allowed);
        assert!(Widget::perform_action(
            &mut input,
            &tilo_ui_base::a11y::Action::SetValue("Atlas".into()),
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            tilo_ui_base::widget::controls::take_control_invocation(&mut input)
                .unwrap()
                .arguments["value"],
            "Atlas"
        );
        assert_eq!(*changes.borrow(), ["Atlas"]);
    }
}

tilo_test_env::scenario! {
    fn keys_and_the_pointer_move_select_and_edit_a_field;

    phase arrow_keys_move_and_extend_by_char_and_word;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_initial_value("abcdef");
            ti.set_focus(true);
            ti.set_cursor_pos(3);
            let mut doc = AutoCommit::new();

            assert!(drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(false, true)
            ));
            assert_eq!(
                ti.selection_range(),
                Some((3, 4)),
                "shift+Right extends by one char"
            );
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(false, true),
            );
            assert_eq!(ti.selection_range(), Some((3, 5)));
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowLeft),
                mods(false, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((3, 4)),
                "shift+Left shrinks it back"
            );
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world foo");
            ti.set_focus(true);
            ti.set_cursor_pos(0);
            let mut doc = AutoCommit::new();

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(true, false),
            );
            assert_eq!(
                ti.cursor_pos(),
                6,
                "ctrl+Right jumps to start of the next word"
            );
            assert_eq!(ti.selection_range(), None, "no shift → no selection");
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(true, false),
            );
            assert_eq!(ti.cursor_pos(), 12, "ctrl+Right again → start of 'foo'");
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowLeft),
                mods(true, false),
            );
            assert_eq!(ti.cursor_pos(), 6, "ctrl+Left walks back a word");
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world foo");
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            ti.set_selection(3, 12);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowLeft),
                mods(true, false),
            );
            assert_eq!(
                ti.cursor_pos(),
                3,
                "Ctrl+Left collapses to the selection start"
            );
            assert_eq!(ti.selection_range(), None);

            ti.set_selection(3, 12);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(true, false),
            );
            assert_eq!(
                ti.cursor_pos(),
                12,
                "Ctrl+Right collapses to the selection end"
            );
            assert_eq!(ti.selection_range(), None);
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world foo");
            ti.set_focus(true);
            ti.set_cursor_pos(0);
            let mut doc = AutoCommit::new();

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(true, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, 6)),
                "shift+ctrl+Right selects the first word span"
            );
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(true, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, 12)),
                "extends by another word"
            );
        }
    }

    phase home_end_and_select_all_shortcuts_move_extend_and_cut;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.set_focus(true);
            ti.set_cursor_pos(5);
            let mut doc = AutoCommit::new();

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::End),
                mods(false, false),
            );
            assert_eq!(ti.cursor_pos(), 11, "End → end of field");
            assert_eq!(ti.selection_range(), None);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::Home),
                mods(false, false),
            );
            assert_eq!(ti.cursor_pos(), 0, "Home → start of field");

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::End),
                mods(false, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, 11)),
                "shift+End selects to line end"
            );
            ti.set_cursor_pos(11);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::Home),
                mods(false, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, 11)),
                "shift+Home selects to line start"
            );
        }

        {
            let mut ti = ml_input("first\nsecond\nthird", 400.0, 200.0);
            let mut doc = AutoCommit::new();
            ti.set_cursor_pos(8); // middle of the second visual line

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::Home),
                mods(true, false),
            );
            assert_eq!(ti.cursor_pos(), 0, "Ctrl+Home reaches document start");

            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::End),
                mods(true, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, ti.value().len())),
                "Ctrl+Shift+End selects through the whole document"
            );

            ti.set_cursor_pos(8);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::Home),
                mods(true, true),
            );
            assert_eq!(
                ti.selection_range(),
                Some((0, 8)),
                "Ctrl+Shift+Home selects back to document start"
            );
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            drive(&mut ti, &mut doc, &char_key("a"), mods(true, false));
            assert_eq!(
                ti.selection_range(),
                Some((0, 11)),
                "ctrl+A selects everything"
            );

            drive(&mut ti, &mut doc, &char_key("x"), mods(true, false));
            assert_eq!(ti.value(), "", "ctrl+X cuts the selection");
            assert_eq!(ti.selection_range(), None, "selection collapsed after cut");
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello 👋");
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            drive(&mut ti, &mut doc, &char_key("a"), command_mods());
            assert_eq!(ti.selection_range(), Some((0, ti.value().len())));
        }
    }

    phase pointer_clicks_and_drags_select_word_range_and_all;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let mut doc = AutoCommit::new();

            let bx = x_for_byte(&ti, 8); // inside "world" (bytes 6..11)
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(bx, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));

            assert_eq!(
                ti.selection_range(),
                Some((6, 11)),
                "double-click selected the word"
            );
            assert_eq!(ti.selected_str(), Some("world"));
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let mut doc = AutoCommit::new();

            let x_start = x_for_byte(&ti, 2);
            let x_end = x_for_byte(&ti, 9);
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(x_start, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            assert!(ti.is_dragging(), "press starts a drag");
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(x_end, 20.0),
                mods(false, false),
            );
            assert_eq!(
                ti.selection_range(),
                Some((2, 9)),
                "drag selected the swept range"
            );
            drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
            assert!(!ti.is_dragging(), "release ends the drag");
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let mut doc = AutoCommit::new();

            let bx = x_for_byte(&ti, 8);
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(bx, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            assert_eq!(
                ti.selection_range(),
                Some((0, 11)),
                "triple-click selects the whole field"
            );
        }
    }

    phase primary_selection_publishes_pointer_selections_and_middle_click_pastes;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let mut doc = AutoCommit::new();
            let start = x_for_byte(&ti, 0);
            let end = x_for_byte(&ti, 5);
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(start, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(end, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));

            assert_eq!(ti.shareable_selection(), Some("hello"));
        }

        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let mut doc = AutoCommit::new();
            let at = x_for_byte(&ti, 8);
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(at, 20.0),
                mods(false, false),
            );
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            assert_eq!(ti.shareable_selection(), Some("world"));
            drive(&mut ti, &mut doc, &mouse_release(), mods(false, false));
            drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));
            assert_eq!(ti.shareable_selection(), Some("hello world"));
        }

        {
            let mut ti = TextInput::local()
                .with_initial_value("leftright")
                .with_char_filter(|ch| !ch.is_whitespace());
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            ti.set_selection(0, ti.value().len());
            let mut doc = AutoCommit::new();
            let at = x_for_byte(&ti, 4);
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(at, 20.0),
                mods(false, false),
            );
            let mut focus_target = None;
            let mut handled = false;
            ti.handle_middle_click_with_primary(
                Some(" P A "),
                &mut focus_target,
                test_node().1,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(ti.value(), "leftPAright");
            assert_eq!(ti.cursor_pos(), 6);
            assert_eq!(ti.selection_range(), None);
        }

        {
            let mut secret = TextInput::local()
                .with_secret(true)
                .with_initial_value("secret");
            secret.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut secret);
            let mut doc = AutoCommit::new();
            let start = x_for_byte(&secret, 0);
            let end = x_for_byte(&secret, 6);
            drive(
                &mut secret,
                &mut doc,
                &cursor_moved(start, 20.0),
                mods(false, false),
            );
            drive(&mut secret, &mut doc, &mouse_press(), mods(false, false));
            drive(
                &mut secret,
                &mut doc,
                &cursor_moved(end, 20.0),
                mods(false, false),
            );
            drive(&mut secret, &mut doc, &mouse_release(), mods(false, false));
            assert_eq!(secret.shareable_selection(), None);

            drive(
                &mut secret,
                &mut doc,
                &cursor_moved(start, 20.0),
                mods(false, false),
            );
            let mut focus_target = None;
            let mut handled = false;
            secret.handle_middle_click_with_primary(
                Some("public"),
                &mut focus_target,
                test_node().1,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(secret.value(), "publicsecret");
        }
    }
}
