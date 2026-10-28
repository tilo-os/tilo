//! Keys after a mouse selection act on what is shown selected: a drag inside
//! one block is that block's own selection, and a selection across blocks
//! follows the keys that move its caret.
use super::*;

/// A mouse drag that stays inside one block selects in that block, so the
/// keys after it act on that selection as they do on a keyboard one: End
/// goes to the line end, Shift+End and Ctrl+Right extend or move from it, and
/// what is typed or copied is what is shown selected.
#[test]
fn keys_after_a_drag_inside_one_block_act_on_the_dragged_selection() {
    use crate::synthetic_input::{cursor_moved, key_press, mouse_press, mouse_release};
    use winit::keyboard::ModifiersState;
    const TEXT: &str = "alpha beta gamma delta";
    let dragged = || {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph(TEXT), Block::paragraph("next")]);
        let mut renderer = TextRenderer::new();
        layout_image_editor(&mut editor, 1000.0, &mut renderer);
        let rect = editor.blocks[0].line.get_rect();
        let y = rect.center().y;
        let x_of = |editor: &BlockEditor, byte: usize| {
            let mut x = rect.x0;
            while x < rect.x1
                && editor.blocks[0]
                    .line
                    .inner()
                    .byte_at_point(Point::new(x, y))
                    < byte
            {
                x += 0.5;
            }
            x
        };
        let (from, to) = (x_of(&editor, 6), x_of(&editor, 16));
        dispatch(&mut editor, &cursor_moved(from, y));
        dispatch(&mut editor, &mouse_press());
        for step in 1..=8 {
            dispatch(
                &mut editor,
                &cursor_moved(from + (to - from) * f64::from(step) / 8.0, y),
            );
        }
        dispatch(&mut editor, &mouse_release());
        assert_eq!(editor.blocks[0].line.selection_range(), Some((6, 16)));
        assert_eq!(
            editor.document_selection(),
            None,
            "one block owns its own selection"
        );
        editor
    };
    let key = |editor: &mut BlockEditor, key: NamedKey, modifiers: ModifiersState| {
        Dispatch::new()
            .modifiers(modifiers)
            .send(editor, &key_press(Key::Named(key), None));
    };

    let mut editor = dragged();
    key(&mut editor, NamedKey::End, ModifiersState::empty());
    type_keys(&mut editor, "Z");
    assert_eq!(
        editor.blocks[0].text(),
        format!("{TEXT}Z"),
        "End goes to the line end"
    );

    let mut editor = dragged();
    key(&mut editor, NamedKey::Home, ModifiersState::empty());
    type_keys(&mut editor, "Z");
    assert_eq!(
        editor.blocks[0].text(),
        format!("Z{TEXT}"),
        "Home goes to the line start"
    );

    let mut editor = dragged();
    key(&mut editor, NamedKey::End, ModifiersState::SHIFT);
    assert_eq!(
        editor.blocks[0].line.selection_range(),
        Some((6, TEXT.len())),
        "Shift+End extends"
    );
    type_keys(&mut editor, "Z");
    assert_eq!(editor.blocks[0].text(), "alpha Z");

    let mut editor = dragged();
    key(&mut editor, NamedKey::ArrowRight, ModifiersState::CONTROL);
    type_keys(&mut editor, "Z");
    assert_eq!(
        editor.blocks[0].text(),
        "alpha beta gammaZ delta",
        "Ctrl+Right leaves the selection at its end, as in a one-line field"
    );
}

/// A selection across blocks follows the keys that move its caret inside
/// the block it ends in, instead of keeping the range it had.
#[test]
fn a_selection_across_blocks_follows_the_keys_that_move_its_caret() {
    use crate::synthetic_input::key_press;
    use winit::keyboard::ModifiersState;
    let key = |editor: &mut BlockEditor, key: NamedKey, modifiers: ModifiersState| {
        Dispatch::new()
            .modifiers(modifiers)
            .send(editor, &key_press(Key::Named(key), None));
    };
    let across = || {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("first line"),
            Block::paragraph("second line"),
        ]);
        editor.focus_block(0, CursorPlacement::Start);
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 6 },
            DocumentPosition { block: 1, byte: 3 },
        );
        assert_eq!(editor.selected_text().as_deref(), Some("line\nsec"));
        editor
    };

    let mut editor = across();
    key(&mut editor, NamedKey::End, ModifiersState::SHIFT);
    assert_eq!(editor.selected_text().as_deref(), Some("line\nsecond line"));
    type_keys(&mut editor, "Z");
    assert_eq!(
        editor.blocks().iter().map(Block::text).collect::<Vec<_>>(),
        ["first Z"],
        "typing replaces the selection as extended"
    );

    let mut editor = across();
    key(&mut editor, NamedKey::ArrowLeft, ModifiersState::SHIFT);
    assert_eq!(editor.selected_text().as_deref(), Some("line\nse"));

    let mut editor = across();
    key(&mut editor, NamedKey::ArrowRight, ModifiersState::CONTROL);
    assert_eq!(editor.document_selection(), None, "a plain move ends it");
    type_keys(&mut editor, "Z");
    assert_eq!(
        editor.blocks().iter().map(Block::text).collect::<Vec<_>>(),
        ["first line", "secZond line"],
        "typing lands at the caret, not over the old range"
    );
}
