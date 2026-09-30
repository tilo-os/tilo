use super::*;
use automerge::{ObjType, ROOT, transaction::Transactable};
use winit::keyboard::ModifiersState;

fn send(editor: &mut BlockEditor, doc: &mut AutoCommit, key: NamedKey, modifiers: ModifiersState) {
    send_key(editor, doc, &Key::Named(key), modifiers);
}

fn send_key(editor: &mut BlockEditor, doc: &mut AutoCommit, key: &Key, modifiers: ModifiersState) {
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = key.clone();
    event.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    event.location = winit::keyboard::KeyLocation::Standard;
    event.state = ElementState::Pressed;
    let mut handled = false;
    editor.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        },
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::from(modifiers),
            state: doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    assert!(handled, "{key:?} must remain owned by the code editor");
    editor.sync(doc, &ROOT);
}

fn code(text: &str) -> Block {
    Block::code_block(Some("python".into()), text)
}

#[test]
fn code_select_all_copy_and_paste_respect_the_code_run() {
    fn select_all_first_selects_code_then_the_whole_document() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("Before"),
            code("one"),
            code("two"),
            Block::paragraph("After"),
        ]);
        editor.focus_block(1, CursorPlacement::Start);
        let mut doc = AutoCommit::new();
        send_key(
            &mut editor,
            &mut doc,
            &Key::Character("a".into()),
            ModifiersState::CONTROL,
        );
        assert_eq!(editor.selected_text().as_deref(), Some("one\ntwo"));
        send_key(
            &mut editor,
            &mut doc,
            &Key::Character("a".into()),
            ModifiersState::CONTROL,
        );
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("Before\none\ntwo\nAfter")
        );
    }

    fn repeated_select_all_stays_on_whole_note_and_copy_code_keeps_caret() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("Before"), code("one"), code("two")]);
        editor.focus_block(1, CursorPlacement::Byte(1));
        let mut doc = AutoCommit::new();
        send_key(
            &mut editor,
            &mut doc,
            &Key::Character("c".into()),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        assert_eq!(crate::clipboard_paste().as_deref(), Some("one\ntwo"));
        assert_eq!(editor.focused, Some(1));
        assert_eq!(editor.blocks[1].line.cursor_pos(), 1);
        for press in 0..4 {
            send_key(
                &mut editor,
                &mut doc,
                &Key::Character("a".into()),
                ModifiersState::CONTROL,
            );
            assert_eq!(
                editor.selected_text().as_deref(),
                Some(if press == 0 {
                    "one\ntwo"
                } else {
                    "Before\none\ntwo"
                })
            );
        }
    }

    fn multiline_code_paste_keeps_indentation_blank_lines_and_suffix() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("beforeafter")]);
        editor.focus_block(0, CursorPlacement::Byte(6));
        let mut doc = AutoCommit::new();
        clipboard_copy("first\n    second\n\nlast");
        send_key(
            &mut editor,
            &mut doc,
            &Key::Character("v".into()),
            ModifiersState::CONTROL,
        );
        assert_eq!(
            editor.blocks.iter().map(Block::text).collect::<Vec<_>>(),
            vec!["beforefirst", "    second", "", "lastafter"]
        );
        assert!(
            editor
                .blocks
                .iter()
                .all(|block| block.kind == code("").kind)
        );
        assert_eq!(editor.focused, Some(3));
        assert_eq!(editor.blocks[3].line.cursor_pos(), 4);
    }

    select_all_first_selects_code_then_the_whole_document();
    repeated_select_all_stays_on_whole_note_and_copy_code_keeps_caret();
    multiline_code_paste_keeps_indentation_blank_lines_and_suffix();
}

#[test]
fn code_enter_and_backspace_exit_or_stay_inside_the_run() {
    fn command_enter_exits_entire_run_and_reuses_adjacent_prose() {
        for command in [ModifiersState::CONTROL, ModifiersState::SUPER] {
            let mut editor = BlockEditor::new();
            editor.set_blocks(vec![
                Block::paragraph("Before"),
                code("one"),
                code("two"),
                Block::paragraph("After"),
            ]);
            let ids: Vec<_> = editor.blocks.iter().map(|block| block.id.clone()).collect();
            editor.focus_block(1, CursorPlacement::Start);
            let mut doc = AutoCommit::new();
            send(&mut editor, &mut doc, NamedKey::Enter, command);
            assert_eq!(editor.focused, Some(3));
            editor.focus_block(2, CursorPlacement::End);
            send(
                &mut editor,
                &mut doc,
                NamedKey::Enter,
                command | ModifiersState::SHIFT,
            );
            assert_eq!(editor.focused, Some(0));
            assert_eq!(
                editor
                    .blocks
                    .iter()
                    .map(|block| block.id.clone())
                    .collect::<Vec<_>>(),
                ids
            );
        }
    }

    fn command_enter_creates_paragraph_at_document_boundaries() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("one"), code("two")]);
        editor.focus_block(0, CursorPlacement::Start);
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::CONTROL,
        );
        assert_eq!(editor.focused, Some(2));
        assert_eq!(editor.blocks[2].kind, BlockKind::Paragraph);
        editor.focus_block(1, CursorPlacement::End);
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        assert_eq!(editor.focused, Some(0));
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[1].text(), "one");
        assert_eq!(editor.blocks[2].text(), "two");
    }

    fn enter_autoindents_and_empty_trailing_line_exits_but_shift_enter_keeps_blank() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("    print('hello')")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks[1].text(), "    ");
        assert_eq!(editor.blocks[1].line.cursor_pos(), 4);
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::SHIFT,
        );
        assert_eq!(editor.blocks[1].kind, code("").kind);
        assert_eq!(editor.blocks[2].text(), "    ");
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks[2].kind, BlockKind::Paragraph);
        assert_eq!(editor.blocks[2].text(), "");
    }

    fn backspace_at_code_boundary_cannot_merge_code_into_prose() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("Before"), code("print(1)")]);
        editor.focus_block(1, CursorPlacement::Start);
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Backspace,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks.len(), 2);
        assert_eq!(editor.blocks[1].text(), "print(1)");
        editor.set_blocks(vec![code("")]);
        editor.focus_block(0, CursorPlacement::Start);
        send(
            &mut editor,
            &mut doc,
            NamedKey::Backspace,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
    }

    command_enter_exits_entire_run_and_reuses_adjacent_prose();
    command_enter_creates_paragraph_at_document_boundaries();
    enter_autoindents_and_empty_trailing_line_exits_but_shift_enter_keeps_blank();
    backspace_at_code_boundary_cannot_merge_code_into_prose();
}

#[test]
fn code_tab_indentation_preserves_identity_and_emits_canonical_undoable_edits() {
    fn tab_indents_selected_code_rows_preserving_identity_and_selection() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("α()"), code("  beta()"), code("gamma()")]);
        let ids: Vec<_> = editor.blocks.iter().map(|block| block.id.clone()).collect();
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition { block: 2, byte: 0 },
        );
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks[0].text(), "    α()");
        assert_eq!(editor.blocks[1].text(), "      beta()");
        assert_eq!(editor.blocks[2].text(), "gamma()");
        assert_eq!(
            editor.document_selection.unwrap().cursor,
            DocumentPosition { block: 2, byte: 0 }
        );
        send(&mut editor, &mut doc, NamedKey::Tab, ModifiersState::SHIFT);
        assert_eq!(editor.blocks[0].text(), "α()");
        assert_eq!(editor.blocks[1].text(), "  beta()");
        assert_eq!(
            editor
                .blocks
                .iter()
                .map(|block| block.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }

    fn tab_at_unicode_caret_inserts_spaces_and_outdent_keeps_body() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("αβ")]);
        editor.focus_block(0, CursorPlacement::Byte(2));
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        assert_eq!(editor.blocks[0].text(), "α    β");
        assert_eq!(editor.blocks[0].line.cursor_pos(), 6);
        send(&mut editor, &mut doc, NamedKey::Tab, ModifiersState::SHIFT);
        assert_eq!(editor.blocks[0].text(), "α    β");
    }

    fn bound_code_indent_and_exit_have_undo_redo_checkpoints() {
        let mut state = crate::state::State::new();
        let list = state.doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
        let mut editor = BlockEditor::new();
        editor.bind_blocks(&mut state.doc, list);
        editor.try_convert_to(0, code("").kind);
        editor.blocks[0].line.set_text("value");
        editor.code_keyboard_edited();
        editor.sync(&mut state.doc, &ROOT);
        editor.focus_block(0, CursorPlacement::Start);
        let id = editor.blocks[0].id.clone();
        let bound = editor.blocks[0].line.bound_obj();
        state.commit_action(None, None);
        crate::undo::on_commit(&mut editor);
        send(
            &mut editor,
            &mut state.doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        assert!(crate::undo::needs_commit(&editor));
        state.commit_action(None, None);
        crate::undo::on_commit(&mut editor);
        send(
            &mut editor,
            &mut state.doc,
            NamedKey::Enter,
            ModifiersState::CONTROL,
        );
        state.commit_action(None, None);
        crate::undo::on_commit(&mut editor);
        assert_eq!(editor.blocks.len(), 2);
        assert!(state.undo(None, None).is_some());
        editor.sync(&mut state.doc, &ROOT);
        assert_eq!(editor.blocks.len(), 1);
        assert_eq!(editor.blocks[0].text(), "    value");
        assert!(state.undo(None, None).is_some());
        editor.sync(&mut state.doc, &ROOT);
        assert_eq!(editor.blocks[0].text(), "value");
        assert_eq!(editor.blocks[0].id, id);
        assert_eq!(editor.blocks[0].line.bound_obj(), bound);
        assert!(state.redo(None, None).is_some());
        editor.sync(&mut state.doc, &ROOT);
        assert_eq!(editor.blocks[0].text(), "    value");
    }

    fn code_indentation_emits_canonical_text_edits_for_stable_blocks() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("first"), code("second")]);
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition { block: 1, byte: 6 },
        );
        editor.set_action_interception(true);
        editor.begin_action_intercept();
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        let ops = editor.take_intercepted_ops();
        assert_eq!(
            ops.len(),
            2,
            "one canonical splice per existing source line: {ops:?}"
        );
        assert!(
            ops.iter()
                .all(|op| matches!(op, BlockEditOp::TextSplice { .. }))
        );
    }

    fn long_code_line_scrolls_to_keep_the_keyboard_caret_visible() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code(&"long_identifier + ".repeat(30))]);
        editor.focus_block(0, CursorPlacement::End);
        let theme = Theme::dark();
        let mut tree = TaffyTree::new();
        let node = editor.build_layout(&mut tree, &theme, &mut HashMap::new());
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(260.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .unwrap();
        editor.apply_layout(&tree, node, Point::ZERO);
        editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
        );
        let line = &editor.blocks[0].line;
        assert!(line.inner().scroll_x() > 0.0);
        let caret = line.inner().caret_rect(&theme, line.cursor_pos());
        assert!(
            caret.x0 >= line.get_rect().x0 && caret.x1 <= line.get_rect().x1,
            "the whole caret must remain inside the painted clip, not just its left edge"
        );
    }

    tab_indents_selected_code_rows_preserving_identity_and_selection();
    tab_at_unicode_caret_inserts_spaces_and_outdent_keeps_body();
    bound_code_indent_and_exit_have_undo_redo_checkpoints();
    code_indentation_emits_canonical_text_edits_for_stable_blocks();
    long_code_line_scrolls_to_keep_the_keyboard_caret_visible();
}
