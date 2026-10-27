//! How the pointer and the keys move through a field's text: clicks,
//! drags, word and range selection, arrows, Home and End, and the measurement a
//! draw leaves behind for them.
use super::super::*;
use super::command_mods;
use super::draw_to_populate;
use super::draw_with_theme;
use super::drive;
use super::ml_input;
use super::mods;
use super::test_node;
use super::x_for_byte;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
use automerge::ObjType;
use automerge::transaction::Transactable as _;

#[test]
fn action_bindings_own_edits_and_accessibility_set_value() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `bound_text_edits_optimistically_without_splicing_document`.
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

    // Case `action_binding_owns_accessibility_set_value`.
    {
        let mut doc = AutoCommit::new();
        let object = doc
            .put_object(&automerge::ROOT, "legacy", ObjType::Text)
            .unwrap();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()))
            .on_action(crate::widget::test_string_action_binding());
        input.bind_text(object.clone());
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::new(),
        );
        assert!(input.is_disabled());
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(!input.is_disabled());
        assert!(Widget::perform_action(
            &mut input,
            &crate::a11y::Action::SetValue("Project Atlas".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(doc.text(&object).unwrap(), "");
        assert_eq!(
            crate::widget::controls::take_control_invocation(&mut input)
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

    // Case `change_callback_and_action_both_fire_in_either_order`.
    {
        let mut doc = AutoCommit::new();
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut input = TextInput::local()
            .on_action(crate::widget::test_string_action_binding())
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        crate::widget::controls::refresh_control_action_availability(
            &mut input,
            &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(Widget::perform_action(
            &mut input,
            &crate::a11y::Action::SetValue("Atlas".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            crate::widget::controls::take_control_invocation(&mut input)
                .unwrap()
                .arguments["value"],
            "Atlas"
        );
        assert_eq!(*changes.borrow(), ["Atlas"]);
    }
}

#[test]
fn multiline_measurement_matches_draw_scale_and_invalidation() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `multiline_shaped_editor_scales_font_and_line_metrics_with_theme`.
    {
        let mut normal = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Caption)
            .with_initial_value("A durable comment body that wraps onto another row.");
        normal.apply_layout_external(Rect::new(0.0, 0.0, 260.0, 160.0));
        draw_with_theme(&mut normal, &Theme::dark());
        let normal_layout = normal.single_layout.as_ref().expect("normal shaped layout");
        let normal_height = normal_layout.height();
        let (_, _, normal_line_h) = normal_layout.caret_geometry_at(TextPosition::new(0));

        let mut large = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Caption)
            .with_initial_value("A durable comment body that wraps onto another row.");
        large.apply_layout_external(Rect::new(0.0, 0.0, 260.0, 160.0));
        let large_theme = Theme::dark().with_text_scale(2.0);
        draw_with_theme(&mut large, &large_theme);
        let large_layout = large.single_layout.as_ref().expect("large shaped layout");
        let large_height = large_layout.height();
        let (_, _, large_line_h) = large_layout.caret_geometry_at(TextPosition::new(0));

        assert!(
            large_line_h >= normal_line_h * 1.9,
            "editor line metric scales with the theme ({large_line_h} vs {normal_line_h})"
        );
        assert!(
            large_height >= normal_height * 1.9,
            "editor paragraph geometry scales with the theme ({large_height} vs {normal_height})"
        );
    }

    // Case `multiline_measure_uses_the_same_scaled_shaped_rows_as_draw`.
    {
        let text = "A long wrapping line whose measured height must match the caret rows produced by the shaped draw path at large text.";
        let mut input = TextInput::local()
            .multiline(true)
            .with_text_role(TextRole::Body)
            .with_initial_value(text);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let measured = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );

        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(measured.height)));
        let theme = Theme::dark().with_text_scale(2.0);
        draw_with_theme(&mut input, &theme);
        let layout = input
            .single_layout
            .as_ref()
            .expect("shaped multiline layout");
        let expected_height = layout.height() + (ML_PAD_Y * 2.0) as f32;
        assert!(
            (measured.height - expected_height).abs() < 0.1,
            "measure {measured:?} must use drawn shaped height {expected_height}"
        );
        let last_row = layout.visual_line_index(input.text_to_layout_byte(text.len()));
        let caret = input.caret_rect(&theme, text.len());
        assert_eq!(
            (caret.y0 / caret.height()).floor() as usize,
            last_row,
            "caret row and shaped visual row index agree"
        );
    }

    // Case `multiline_trailing_lf_keeps_measure_draw_hit_and_caret_scroll_in_sync`.
    {
        let text = "let value = 1;\n";
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut input = TextInput::local()
                .multiline(true)
                .with_mono()
                .with_gutter(true)
                .with_current_line_highlight(true)
                .with_initial_value(text);
            input.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
            input.set_focus(true);
            input.set_cursor_pos(text.len());

            let mut renderer = TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let measured = Widget::measure(
                &mut input,
                taffy::Size {
                    width: Some(240.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(240.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            let viewport_height =
                f64::from(input.multiline_style(scale).line_height_px()) + ML_PAD_Y * 2.0;
            input.apply_layout_external(Rect::new(0.0, 0.0, 240.0, viewport_height));
            draw_with_theme(&mut input, &theme);

            let drawn_height = input.content_height() as f32;
            assert!(
                measured.height >= drawn_height && measured.height - drawn_height < 1.0,
                "pixel-ceiled measure {measured:?} agrees with attributed draw height {drawn_height} at {scale}x"
            );
            let layout = input
                .single_layout
                .as_ref()
                .expect("draw retained attributed multiline layout");
            let (_, eof_top, eof_height) =
                layout.caret_geometry(input.text_to_layout_byte(text.len()));
            assert_eq!(
                layout.visual_line_index(text.len()).saturating_add(1),
                layout.buffer.layout_runs().count(),
                "EOF stays on the final visual row even when large text wraps"
            );
            assert_eq!(layout.logical_line_origins().len(), 2);
            assert!(
                (eof_top + eof_height - layout.height()).abs() < 0.01,
                "EOF caret reaches the bottom of the final row"
            );

            let caret = input.caret_rect(&theme, text.len());
            assert_eq!(
                input.byte_at_point(Point::new(caret.x0 + 1.0, caret.center().y)),
                text.len(),
                "clicking the final empty row returns EOF"
            );
            let view_height = input.rect.height() - ML_PAD_Y * 2.0;
            let expected_scroll = (f64::from(layout.height()) - view_height).max(0.0);
            assert!(expected_scroll > 0.0);
            assert!(
                (input.scroll_y() - expected_scroll).abs() < 0.01,
                "caret-follow scroll remains at the EOF row"
            );
            assert!(input.gutter_width() > 0.0);
            assert!(
                input
                    .code_row_colors(&theme)
                    .iter()
                    .any(|(_, color)| *color != theme.text_primary),
                "syntax attribution survives terminal-row normalization"
            );
        }
    }

    // Case `multiline_measure_uses_build_layout_theme_scale_when_renderer_is_stale`.
    {
        let text = "The layout theme owns accessibility scale even when a retained renderer still carries its previous frame value.";
        let mut input = TextInput::local().multiline(true).with_initial_value(text);
        let large = Theme::dark().with_text_scale(2.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &large, &mut node_map);

        let mut renderer = TextRenderer::new();
        assert_eq!(renderer.text_scale(), 1.0, "stale renderer precondition");
        let measured_large = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(measured_large.height)));
        draw_with_theme(&mut input, &large);
        let drawn_large = input.content_height().ceil() as f32;
        assert!((measured_large.height - drawn_large).abs() < 0.1);

        let normal = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &normal, &mut node_map);
        let measured_normal = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(
            0.0,
            0.0,
            180.0,
            f64::from(measured_normal.height),
        ));
        draw_with_theme(&mut input, &normal);
        assert!((measured_normal.height - input.content_height().ceil() as f32).abs() < 0.1);
        assert!(
            measured_large.height > measured_normal.height * 1.8,
            "same instance remeasures from large text back to normal"
        );
    }

    // Case `measured_multiline_pointer_geometry_tracks_applied_origin_before_draw`.
    {
        let text = "first wrapped row continues for a while before the second hard line\nsecond";
        let mut input = TextInput::local().multiline(true).with_initial_value(text);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let measured = Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(180.0),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(180.0),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        input.apply_layout_external(Rect::new(
            100.0,
            50.0,
            280.0,
            50.0 + f64::from(measured.height),
        ));
        let second = text.find('\n').expect("hard break") + 1;
        let (x, top, height) = input
            .single_layout
            .as_ref()
            .expect("measurement retained shaped rows")
            .caret_geometry(input.text_to_layout_byte(second));
        let point = Point::new(
            input.rect.x0 + input.ml_left() + f64::from(x) + 1.0,
            input.rect.y0 + ML_PAD_Y + f64::from(top + height / 2.0),
        );
        assert_eq!(
            input.byte_at_point(point),
            second,
            "pointer uses the applied widget origin before the first draw"
        );
    }

    // Case `crdt_sync_undo_redo_invalidates_multiline_measurement`.
    {
        let short = "short";
        let long = "undo and redo replay real CRDT text through the same widget, including a long line that wraps at large text";
        let theme = Theme::dark().with_text_scale(2.0);
        let mut state = crate::State::new();
        let text_obj = state
            .doc
            .put_object(&state.root, "body", ObjType::Text)
            .expect("create document text");
        state
            .doc
            .splice_text(&text_obj, 0, 0, short)
            .expect("seed document text");
        state.commit_action(None, None);

        let mut input = TextInput::local().multiline(true);
        input.bind_text(text_obj.clone());
        Widget::sync(&mut input, &state.doc, &state.root);
        let mut renderer = TextRenderer::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let _ = Widget::build_layout(&mut input, &mut taffy, &theme, &mut node_map);
        let measure = |input: &mut TextInput, renderer: &mut TextRenderer| {
            Widget::measure(
                input,
                taffy::Size {
                    width: Some(180.0),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(180.0),
                    height: taffy::AvailableSpace::MaxContent,
                },
                renderer,
            )
            .height
        };
        let short_height = measure(&mut input, &mut renderer);
        input.apply_layout_external(Rect::new(0.0, 0.0, 180.0, f64::from(short_height)));
        assert!(!Widget::layout_needs_measure(&input));

        let old_len = state.doc.length(&text_obj);
        state
            .doc
            .splice_text(
                &text_obj,
                0,
                isize::try_from(old_len).expect("test text length fits isize"),
                long,
            )
            .expect("replace with wrapping text");
        state.commit_action(None, None);
        Widget::sync(&mut input, &state.doc, &state.root);
        assert_eq!(input.take_dirty(), None, "model sync is not a user edit");
        assert!(
            Widget::layout_needs_measure(&input),
            "CRDT text growth invalidates retained measurement"
        );
        let long_height = measure(&mut input, &mut renderer);
        assert!(long_height > short_height * 1.8, "long text wraps at 2x");

        state.undo(None, None).expect("undo long text");
        Widget::sync(&mut input, &state.doc, &state.root);
        assert!(Widget::layout_needs_measure(&input));
        let undo_height = measure(&mut input, &mut renderer);
        assert!((undo_height - short_height).abs() < 0.1);

        state.redo(None, None).expect("redo long text");
        Widget::sync(&mut input, &state.doc, &state.root);
        assert!(Widget::layout_needs_measure(&input));
        let redo_height = measure(&mut input, &mut renderer);
        assert!((redo_height - long_height).abs() < 0.1);
    }
}

#[test]
fn arrow_keys_move_and_extend_by_char_and_word() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `shift_arrow_extends_selection_by_char`.
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

    // Case `ctrl_arrow_moves_caret_by_word`.
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

    // Case `ctrl_arrow_collapses_a_selection_before_moving_by_word`.
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

    // Case `shift_ctrl_arrow_extends_selection_by_word`.
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

#[test]
fn home_end_and_select_all_shortcuts_move_extend_and_cut() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `home_end_move_and_extend`.
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

    // Case `ctrl_home_end_move_and_extend_across_multiline_document`.
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

    // Case `ctrl_a_selects_all_and_ctrl_x_cuts`.
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

    // Case `command_a_selects_all_on_platforms_using_super`.
    {
        let mut ti = TextInput::local().with_initial_value("hello 👋");
        ti.set_focus(true);
        let mut doc = AutoCommit::new();

        drive(&mut ti, &mut doc, &char_key("a"), command_mods());
        assert_eq!(ti.selection_range(), Some((0, ti.value().len())));
    }
}

#[test]
fn pointer_clicks_and_drags_select_word_range_and_all() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `double_click_selects_word_via_handle_event`.
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
        // Within the multi-click interval → double click.
        drive(&mut ti, &mut doc, &mouse_press(), mods(false, false));

        assert_eq!(
            ti.selection_range(),
            Some((6, 11)),
            "double-click selected the word"
        );
        assert_eq!(ti.selected_str(), Some("world"));
    }

    // Case `drag_selects_range_via_handle_event`.
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

    // Case `triple_click_selects_all_via_handle_event`.
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

#[test]
fn primary_selection_publishes_pointer_selections_and_middle_click_pastes() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `mouse_selection_is_eligible_for_primary_publication`.
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

    // Case `double_and_triple_click_selections_are_eligible_for_primary_publication`.
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

    // Case `middle_click_pastes_primary_at_clicked_caret_and_applies_filter`.
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

    // Case `secret_selection_never_publishes_but_middle_paste_is_allowed`.
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
