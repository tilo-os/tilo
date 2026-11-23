//! The level a typed Markdown marker gives its block: a marker starts the block it makes at a level the
//! person names, not at the depth of the line it happened to be typed on.

use super::*;

#[test]
fn a_typed_marker_starts_at_its_own_level_not_the_one_it_is_typed_on() {
    // The line under a list item continues it: its depth says where its text
    // sits. A marker typed there starts a block of that list, at the item's
    // own level, never one deeper for every line typed.
    fn typed_bullet_on_a_continuation_line_stays_in_the_list_it_continues() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::bullet(0, "milk"), Block::paragraph("")]);
        editor.blocks_mut()[1].set_indent(1);
        editor.focus_block(1, CursorPlacement::Start);
        type_keys(&mut editor, "- eggs");
        assert_eq!(
            editor
                .blocks()
                .iter()
                .map(|block| (block.kind.clone(), block.text().to_owned()))
                .collect::<Vec<_>>(),
            [
                (BlockKind::Bullet { indent: 0 }, "milk".to_owned()),
                (BlockKind::Bullet { indent: 0 }, "eggs".to_owned()),
            ],
            "a sibling of the item above, beside it on the page"
        );
    }

    fn markers_typed_on_a_continuation_line_carry_no_list_level() {
        let continuation_at = |depth: u8| {
            let mut editor = BlockEditor::new();
            editor.set_blocks(vec![Block::bullet(0, "")]);
            editor.blocks_mut().push(Block::paragraph(""));
            editor.blocks_mut()[1].set_indent(depth);
            editor.focus_block(1, CursorPlacement::Start);
            editor
        };
        let mut heading = continuation_at(1);
        type_keys(&mut heading, "# Errands");
        assert_eq!(
            (
                heading.blocks()[1].kind.clone(),
                heading.blocks()[1].nesting
            ),
            (BlockKind::Heading(1), 0)
        );
        let mut quote = continuation_at(1);
        type_keys(&mut quote, "> Tuesday morning");
        assert_eq!(
            (quote.blocks()[1].kind.clone(), quote.blocks()[1].nesting),
            (BlockKind::Quote, 0)
        );
        let mut code = continuation_at(1);
        type_keys(&mut code, "```rust");
        assert_eq!(
            (code.blocks()[1].kind.clone(), code.blocks()[1].nesting),
            (
                BlockKind::CodeBlock {
                    lang: Some("rust".to_owned())
                },
                0
            )
        );
        let mut task = continuation_at(1);
        type_keys(&mut task, "[ ] coffee");
        assert_eq!(
            task.blocks()[1].kind,
            BlockKind::Checklist {
                indent: 0,
                done: false
            }
        );
    }

    fn a_marker_continues_the_item_it_was_typed_inside_of() {
        // Inside an item at depth 1, a continuation line is depth 2 and the
        // marker typed on it continues that inner list at depth 1.
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::bullet(0, "outer"),
            Block::bullet(1, "inner"),
            Block::paragraph(""),
        ]);
        editor.blocks_mut()[2].set_indent(2);
        editor.focus_block(2, CursorPlacement::Start);
        type_keys(&mut editor, "- next");
        assert_eq!(
            editor.blocks()[2].kind,
            BlockKind::Bullet { indent: 1 },
            "the level of the item it was written in"
        );
    }

    fn a_task_marker_on_a_bullet_keeps_that_bullet_s_level() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::bullet(2, "")]);
        editor.focus_block(0, CursorPlacement::Start);
        type_keys(&mut editor, "[ ] ");
        assert_eq!(
            editor.blocks()[0].kind,
            BlockKind::Checklist {
                indent: 2,
                done: false
            }
        );
    }

    typed_bullet_on_a_continuation_line_stays_in_the_list_it_continues();
    markers_typed_on_a_continuation_line_carry_no_list_level();
    a_marker_continues_the_item_it_was_typed_inside_of();
    a_task_marker_on_a_bullet_keeps_that_bullet_s_level();
}
