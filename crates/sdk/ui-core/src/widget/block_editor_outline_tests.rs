use super::*;

#[test]
fn prose_nests_below_prose_without_changing_identity_or_inline_marks() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Parent"),
        Block::paragraph("é😀 child"),
    ]);
    editor.blocks[1]
        .line
        .set_marks(vec![MarkSpan::new(0..6, Mark::Bold)]);
    let id = editor.blocks[1].id.clone();
    editor.focus_block(1, CursorPlacement::Byte(6));
    assert!(editor.try_indent(1, 1));
    assert_eq!(editor.blocks[1].indent(), 1);
    assert_eq!(editor.blocks[1].id, id);
    assert_eq!(editor.blocks[1].text(), "é😀 child");
    assert_eq!(editor.blocks[1].line.cursor_pos(), 6);
    assert_eq!(
        editor.blocks[1].line.marks(),
        &[MarkSpan::new(0..6, Mark::Bold)]
    );
    assert!(editor.try_indent(1, -1));
    assert_eq!(editor.blocks[1].indent(), 0);
}

#[test]
fn joining_blocks_keeps_marks_on_both_sides_of_the_unicode_boundary() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![Block::paragraph("é"), Block::paragraph("😀 child")]);
    editor.blocks[0]
        .line
        .set_marks(vec![MarkSpan::new(0..2, Mark::Italic)]);
    editor.blocks[1]
        .line
        .set_marks(vec![MarkSpan::new(0..4, Mark::Bold)]);
    editor.merge_with_previous(1);
    assert_eq!(editor.blocks[0].text(), "é😀 child");
    assert_eq!(editor.blocks[0].line.cursor_pos(), 2);
    assert_eq!(
        editor.blocks[0].line.marks(),
        &[
            MarkSpan::new(0..2, Mark::Italic),
            MarkSpan::new(2..6, Mark::Bold),
        ]
    );
}

#[test]
fn selecting_a_parent_and_one_child_still_indents_all_its_descendants() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Before"),
        Block::bullet(0, "Parent"),
        Block::bullet(1, "Selected child"),
        Block::checklist(1, true, "Unselected child"),
        Block::paragraph("After"),
    ]);
    editor.focus_block(1, CursorPlacement::Start);
    editor.select_document_range(
        DocumentPosition { block: 1, byte: 0 },
        DocumentPosition { block: 2, byte: 2 },
    );
    assert!(editor.try_indent_selection(1));
    assert_eq!(
        editor.blocks.iter().map(Block::indent).collect::<Vec<_>>(),
        [0, 1, 2, 2, 0]
    );
}

#[test]
fn moving_outline_blocks_carries_subtrees_and_code_runs() {
    fn moving_list_parent_moves_its_entire_subtree_past_the_next_subtree() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::numbered(0, "A"),
            Block::bullet(1, "a"),
            Block::checklist(2, true, "aa"),
            Block::numbered(0, "B"),
            Block::bullet(1, "b"),
        ]);
        let ids: Vec<_> = editor.blocks.iter().map(|block| block.id.clone()).collect();
        editor.focus_block(0, CursorPlacement::End);
        assert_eq!(editor.neighboring_group(0, false), Some(3));
        assert!(editor.move_block_to(0, 3));
        assert_eq!(
            editor.blocks.iter().map(Block::text).collect::<Vec<_>>(),
            ["B", "b", "A", "a", "aa"]
        );
        assert_eq!(editor.focused_index(), Some(2));
        assert_eq!(editor.neighboring_group(2, true), Some(0));
        assert!(editor.move_block_to(2, 0));
        assert_eq!(
            editor
                .blocks
                .iter()
                .map(|block| block.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert!(
            !editor.move_block_to(0, 2),
            "cannot drop a parent inside itself"
        );
    }

    fn moving_from_middle_of_code_keeps_rows_and_caret_together() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("Before"),
            Block::code_block(Some("python".into()), "first()"),
            Block::code_block(Some("python".into()), "second()"),
            Block::paragraph("After"),
        ]);
        editor.focus_block(2, CursorPlacement::Byte(3));
        let id = editor.blocks[2].id.clone();
        assert_eq!(editor.neighboring_group(2, false), Some(3));
        assert!(editor.move_block_to(2, 3));
        assert_eq!(
            editor.blocks.iter().map(Block::text).collect::<Vec<_>>(),
            ["Before", "After", "first()", "second()"]
        );
        assert_eq!(editor.focused_index(), Some(3));
        assert_eq!(editor.blocks[3].id, id);
        assert_eq!(editor.blocks[3].line.cursor_pos(), 3);
        assert_eq!(editor.neighboring_group(3, true), Some(1));
        assert!(editor.move_block_to(3, 1));
        assert_eq!(editor.blocks[2].id, id);
    }

    fn moving_child_at_outline_boundary_does_not_reparent_it() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::bullet(0, "Parent"),
            Block::bullet(1, "Child"),
            Block::bullet(0, "Sibling"),
        ]);
        assert_eq!(editor.neighboring_group(1, true), None);
        assert_eq!(editor.neighboring_group(1, false), None);
    }

    moving_list_parent_moves_its_entire_subtree_past_the_next_subtree();
    moving_from_middle_of_code_keeps_rows_and_caret_together();
    moving_child_at_outline_boundary_does_not_reparent_it();
}

#[test]
fn indenting_outline_parents_preserves_descendants_and_numbering() {
    fn mixed_descendant_lists_preserve_ancestor_numbering() {
        let blocks = vec![
            Block::numbered(0, "First"),
            Block::bullet(1, "A detail"),
            Block::numbered(2, "Nested step"),
            Block::checklist(3, true, "Verified"),
            Block::numbered(2, "Next nested step"),
            Block::bullet(1, "Another detail"),
            Block::numbered(2, "Restart below another parent"),
            Block::numbered(0, "Second"),
            Block::checklist(0, false, "Separate top-level list"),
            Block::numbered(0, "New numbered list"),
        ];
        assert_eq!(numbered_ordinals(&blocks), [1, 0, 1, 0, 2, 0, 1, 2, 0, 1]);
    }

    fn indenting_a_parent_preserves_descendants_identity_text_and_completion() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::bullet(0, "Previous"),
            Block::numbered(0, "Parent"),
            Block::checklist(1, true, "Child"),
            Block::bullet(2, "Grandchild"),
            Block::numbered(0, "Following sibling"),
        ]);
        let ids: Vec<_> = editor.blocks.iter().map(|b| b.id.clone()).collect();
        editor.focus_block(1, CursorPlacement::End);
        assert!(editor.try_indent(1, 1));
        assert_eq!(
            editor
                .blocks
                .iter()
                .map(|b| b.kind.indent())
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 0]
        );
        assert_eq!(
            editor
                .blocks
                .iter()
                .map(|b| b.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            editor.blocks[2].kind,
            BlockKind::Checklist {
                indent: 2,
                done: true
            }
        );
        assert_eq!(editor.blocks[3].text(), "Grandchild");
        assert_eq!(editor.focused_index(), Some(1));
        assert_eq!(editor.blocks[1].line.cursor_pos(), "Parent".len());
        assert!(editor.try_indent(1, -1));
        assert_eq!(
            editor
                .blocks
                .iter()
                .map(|b| b.kind.indent())
                .collect::<Vec<_>>(),
            [0, 0, 1, 2, 0]
        );
    }

    fn indent_limit_never_flattens_a_descendant() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::bullet(0, "Parent"),
            Block::numbered(MAX_LIST_INDENT, "Deep child"),
        ]);
        assert!(!editor.try_indent(0, 1));
        assert_eq!(editor.blocks[0].kind.indent(), 0);
        assert_eq!(editor.blocks[1].kind.indent(), MAX_LIST_INDENT);
    }

    fn a_paragraph_ends_the_list_subtree() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::bullet(0, "Parent"),
            Block::bullet(1, "Child"),
            Block::paragraph("Separate prose"),
            Block::bullet(1, "Another list"),
        ]);
        assert!(editor.try_indent(0, 1));
        assert_eq!(editor.blocks[1].kind.indent(), 2);
        assert_eq!(editor.blocks[3].kind.indent(), 1);
    }

    mixed_descendant_lists_preserve_ancestor_numbering();
    indenting_a_parent_preserves_descendants_identity_text_and_completion();
    indent_limit_never_flattens_a_descendant();
    a_paragraph_ends_the_list_subtree();
}
