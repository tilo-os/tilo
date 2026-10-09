use super::*;
use crate::testing::Dispatch;
use automerge::ROOT;
use winit::keyboard::ModifiersState;

fn send(editor: &mut BlockEditor, doc: &mut AutoCommit, key: NamedKey, modifiers: ModifiersState) {
    send_key(editor, doc, &Key::Named(key), modifiers);
}

fn send_key(editor: &mut BlockEditor, doc: &mut AutoCommit, key: &Key, modifiers: ModifiersState) {
    assert!(
        dispatch_key(editor, doc, key, modifiers),
        "{key:?} must remain owned by the editor"
    );
}

fn dispatch_key(
    editor: &mut BlockEditor,
    doc: &mut AutoCommit,
    key: &Key,
    modifiers: ModifiersState,
) -> bool {
    let event = crate::synthetic_input::key_event(key.clone(), None, ElementState::Pressed);
    let handled = Dispatch::new()
        .modifiers(modifiers)
        .doc(doc)
        .send(
            editor,
            &WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event,
                is_synthetic: false,
            },
        )
        .handled;
    editor.sync(doc, &ROOT);
    handled
}

fn code(text: &str) -> Block {
    Block::code_block(Some("python".into()), text)
}

#[test]
fn selected_outline_roots_indent_once_and_exclude_a_column_zero_endpoint() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Parent"),
        Block::bullet(0, "First"),
        Block::checklist(1, true, "Descendant"),
        Block::paragraph("Second"),
        Block::paragraph("Outside selection"),
    ]);
    editor.focus_block(1, CursorPlacement::Start);
    editor.select_document_range(
        DocumentPosition { block: 1, byte: 0 },
        DocumentPosition { block: 4, byte: 0 },
    );
    let selected = editor.document_selection();
    let text = editor.selected_text();
    let mut doc = AutoCommit::new();
    send(
        &mut editor,
        &mut doc,
        NamedKey::Tab,
        ModifiersState::empty(),
    );
    assert_eq!(
        editor.blocks.iter().map(Block::indent).collect::<Vec<_>>(),
        [0, 1, 2, 1, 0]
    );
    assert_eq!(editor.document_selection(), selected);
    assert_eq!(editor.selected_text(), text);
    send(&mut editor, &mut doc, NamedKey::Tab, ModifiersState::SHIFT);
    assert_eq!(
        editor.blocks.iter().map(Block::indent).collect::<Vec<_>>(),
        [0, 0, 1, 0, 0]
    );
}

#[test]
fn escape_releases_tab_to_host_focus_navigation_until_editing_resumes() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("Parent"), Block::paragraph("Child")]);
    editor.focus_block(1, CursorPlacement::End);
    let mut doc = AutoCommit::new();
    assert!(Widget::wants_tab_input(&editor));
    send(
        &mut editor,
        &mut doc,
        NamedKey::Escape,
        ModifiersState::empty(),
    );
    assert!(
        !Widget::wants_tab_input(&editor),
        "Escape then Tab must leave the editor"
    );
    editor.set_focus(false);
    editor.set_focus(true);
    assert!(Widget::wants_tab_input(&editor));
    send(
        &mut editor,
        &mut doc,
        NamedKey::Tab,
        ModifiersState::empty(),
    );
    assert_eq!(editor.blocks[1].indent(), 1);
}

#[test]
fn escape_clears_retained_batch_and_releases_shift_tab() {
    let mut editor = BlockEditor::new();
    editor.present_blocks(&[
        BlockContent::paragraph("a", "A"),
        BlockContent::paragraph("b", ""),
    ]);
    editor.retain_block_selection(vec!["a".into(), "b".into()], false);
    let mut doc = AutoCommit::new();
    send(
        &mut editor,
        &mut doc,
        NamedKey::Escape,
        ModifiersState::empty(),
    );
    assert!(editor.retained_block_selection.is_none());
    assert!(editor.document_selection().is_none());
    assert!(!Widget::wants_tab_input(&editor));
    dispatch_key(
        &mut editor,
        &mut doc,
        &Key::Named(NamedKey::Shift),
        ModifiersState::SHIFT,
    );
    assert!(!dispatch_key(
        &mut editor,
        &mut doc,
        &Key::Named(NamedKey::Tab),
        ModifiersState::SHIFT
    ));
}

#[test]
fn local_caret_and_typing_gestures_clear_a_refused_batch() {
    for key in [
        Key::Named(NamedKey::ArrowLeft),
        Key::Character("x".into()),
        Key::Named(NamedKey::Space),
    ] {
        let rows = [
            BlockContent::paragraph("a", "A"),
            BlockContent::paragraph("b", "B"),
            BlockContent::paragraph("c", "C"),
        ];
        let mut editor = BlockEditor::new();
        editor.present_blocks(&rows);
        editor.retain_block_selection(vec!["a".into(), "b".into()], false);
        editor.present_blocks(&[
            rows[0].clone(),
            BlockContent::paragraph("peer", "Peer"),
            rows[1].clone(),
            rows[2].clone(),
        ]);
        assert_eq!(editor.selected_block_range(), None);
        assert!(
            editor
                .blocks()
                .iter()
                .all(|block| block.line.selection_range().is_none())
        );
        let mut doc = AutoCommit::new();
        dispatch_key(&mut editor, &mut doc, &key, ModifiersState::empty());
        assert!(editor.retained_block_selection.is_none());
        assert_eq!(editor.selected_block_move(false), Some((2..3, 4)));
        assert_eq!(editor.blocks()[1].text(), "Peer");
    }
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

    fn code_indent_and_exit_are_undo_checkpoints_on_the_same_block() {
        let mut doc = AutoCommit::new();
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("value")]);
        editor.focus_block(0, CursorPlacement::Start);
        let id = editor.blocks[0].id.clone();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        assert_eq!(
            editor.take_edit_ops(),
            vec![BlockEditOp::TextSplice {
                block_id: id.clone(),
                start: 0,
                end: 0,
                text: "    ".into(),
            }]
        );
        send(
            &mut editor,
            &mut doc,
            NamedKey::Enter,
            ModifiersState::CONTROL,
        );
        assert_eq!(editor.blocks.len(), 2);
        assert_eq!(editor.blocks[0].id, id);
        assert_eq!(editor.blocks[0].text(), "    value");
        let ops = editor.take_edit_ops();
        assert!(
            matches!(
                ops.as_slice(),
                [BlockEditOp::Insert { after: Some(after), .. }] if *after == id
            ),
            "exiting code appends one block after the code: {ops:?}"
        );
    }

    fn code_indentation_emits_canonical_text_edits_for_stable_blocks() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![code("first"), code("second")]);
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition { block: 1, byte: 6 },
        );
        editor.begin_edit();
        let mut doc = AutoCommit::new();
        send(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty(),
        );
        let ops = editor.take_edit_ops();
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
    code_indent_and_exit_are_undo_checkpoints_on_the_same_block();
    code_indentation_emits_canonical_text_edits_for_stable_blocks();
    long_code_line_scrolls_to_keep_the_keyboard_caret_visible();
}

#[test]
fn pressing_shift_after_escape_keeps_backward_focus_navigation_available() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::bullet(1, "Child")]);
    editor.focus_block(0, CursorPlacement::End);
    let mut doc = AutoCommit::new();
    send(
        &mut editor,
        &mut doc,
        NamedKey::Escape,
        ModifiersState::empty(),
    );
    dispatch_key(
        &mut editor,
        &mut doc,
        &Key::Named(NamedKey::Shift),
        ModifiersState::SHIFT,
    );
    assert!(
        !Widget::wants_tab_input(&editor),
        "Shift must not cancel Escape then Shift+Tab"
    );
    assert_eq!(editor.blocks[0].indent(), 1);
}
