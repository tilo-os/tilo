//! Tab nests prose under the block above it, Shift+Tab moves it back out,
//! a selection across blocks moves together, and Escape hands Tab back to
//! the window's focus order.

use super::*;
use automerge::{ObjType, ROOT, transaction::Transactable};
use winit::keyboard::ModifiersState;

/// Press `key` and report whether the editor kept it.
fn press(
    editor: &mut BlockEditor,
    doc: &mut AutoCommit,
    key: NamedKey,
    mods: ModifiersState,
) -> bool {
    let mut event: KeyEvent = unsafe { std::mem::zeroed() };
    event.logical_key = Key::Named(key);
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
            modifiers: &Modifiers::from(mods),
            state: doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    editor.sync(doc, &ROOT);
    handled
}

fn depths(editor: &BlockEditor) -> Vec<u8> {
    editor.blocks().iter().map(Block::indent).collect()
}

fn prose(lines: &[&str]) -> BlockEditor {
    let mut editor = BlockEditor::new();
    editor.set_blocks(lines.iter().map(|line| Block::paragraph(line)).collect());
    editor
}

#[test]
fn tab_nests_prose_under_the_block_above() {
    fn a_paragraph_nests_one_level_under_the_one_above() {
        let mut editor = prose(&["one", "two"]);
        editor.focus_block(1, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty()
        ));
        assert_eq!(depths(&editor), [0, 1]);
        // Never deeper than one level under the block above.
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty()
        ));
        assert_eq!(depths(&editor), [0, 1]);
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::SHIFT
        ));
        assert_eq!(depths(&editor), [0, 0]);
        assert_eq!(editor.focused_index(), Some(1));
        assert_eq!(editor.blocks()[1].line.cursor_pos(), "two".len());
    }

    fn the_first_block_and_one_after_a_divider_have_nothing_to_nest_under() {
        let mut editor = prose(&["one"]);
        assert!(!editor.try_indent(0, 1));
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![
            Block::paragraph("above"),
            Block::divider(),
            Block::paragraph("below"),
        ]);
        assert!(!editor.try_indent(2, 1));
        assert_eq!(depths(&editor), [0, 0, 0]);
    }

    fn a_nested_paragraph_carries_its_own_children() {
        let mut editor = prose(&["one", "two", "three"]);
        assert!(editor.try_indent(1, 1));
        assert!(editor.try_indent(2, 1));
        assert!(editor.try_indent(2, 1));
        assert_eq!(depths(&editor), [0, 1, 2]);
        assert!(editor.try_indent(1, -1));
        assert_eq!(depths(&editor), [0, 0, 1]);
    }

    fn list_items_still_nest_and_keep_their_kind() {
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::bullet(0, "a"), Block::bullet(0, "b")]);
        editor.focus_block(1, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty()
        ));
        assert_eq!(editor.blocks()[1].kind, BlockKind::Bullet { indent: 1 });
    }

    a_paragraph_nests_one_level_under_the_one_above();
    the_first_block_and_one_after_a_divider_have_nothing_to_nest_under();
    a_nested_paragraph_carries_its_own_children();
    list_items_still_nest_and_keep_their_kind();
}

#[test]
fn tab_moves_every_selected_block() {
    fn a_selection_across_blocks_nests_each_of_them() {
        let mut editor = prose(&["one", "two", "three", "four"]);
        let ids: Vec<_> = editor
            .blocks()
            .iter()
            .map(|block| block.id.clone())
            .collect();
        editor.select_document_range(
            DocumentPosition { block: 1, byte: 1 },
            DocumentPosition { block: 2, byte: 2 },
        );
        let mut doc = AutoCommit::new();
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty()
        ));
        assert_eq!(depths(&editor), [0, 1, 1, 0]);
        assert!(
            editor.document_selection().is_some(),
            "the selection stays for the next Tab"
        );
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::SHIFT
        ));
        assert_eq!(depths(&editor), [0, 0, 0, 0]);
        assert_eq!(
            editor
                .blocks()
                .iter()
                .map(|block| block.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }

    fn a_selection_from_the_first_block_has_nothing_to_nest_under() {
        let mut editor = prose(&["one", "two"]);
        editor.select_document_range(
            DocumentPosition { block: 0, byte: 0 },
            DocumentPosition { block: 1, byte: 3 },
        );
        let mut doc = AutoCommit::new();
        assert!(press(
            &mut editor,
            &mut doc,
            NamedKey::Tab,
            ModifiersState::empty()
        ));
        assert_eq!(depths(&editor), [0, 0]);
    }

    a_selection_across_blocks_nests_each_of_them();
    a_selection_from_the_first_block_has_nothing_to_nest_under();
}

#[test]
fn escape_hands_tab_back_to_the_focus_order() {
    let mut editor = prose(&["one", "two"]);
    editor.focus_block(1, CursorPlacement::End);
    let mut doc = AutoCommit::new();
    assert!(
        editor.wants_tab_input(),
        "Tab nests while the caret is in a block"
    );
    let _ = press(
        &mut editor,
        &mut doc,
        NamedKey::Escape,
        ModifiersState::empty(),
    );
    assert!(
        !editor.wants_tab_input(),
        "after Escape the next Tab leaves the editor"
    );
    assert!(!press(
        &mut editor,
        &mut doc,
        NamedKey::Tab,
        ModifiersState::empty()
    ));
    assert_eq!(depths(&editor), [0, 0]);
    let _ = press(
        &mut editor,
        &mut doc,
        NamedKey::ArrowLeft,
        ModifiersState::empty(),
    );
    assert!(editor.wants_tab_input(), "editing again takes Tab back");
    assert!(press(
        &mut editor,
        &mut doc,
        NamedKey::Tab,
        ModifiersState::empty()
    ));
    assert_eq!(depths(&editor), [0, 1]);
}

#[test]
fn nesting_is_stored_with_the_block_and_undoes_as_one_step() {
    let mut state = crate::state::State::new();
    let list = state.doc.put_object(ROOT, "blocks", ObjType::List).unwrap();
    let mut editor = prose(&["one", "two"]);
    editor.bind_blocks(&mut state.doc, list.clone());
    state.commit_action(None, None);
    crate::undo::on_commit(&mut editor);
    editor.focus_block(1, CursorPlacement::End);
    let ids: Vec<_> = editor
        .blocks()
        .iter()
        .map(|block| block.id.clone())
        .collect();

    assert!(press(
        &mut editor,
        &mut state.doc,
        NamedKey::Tab,
        ModifiersState::empty()
    ));
    assert!(crate::undo::needs_commit(&editor));
    state.commit_action(None, None);
    crate::undo::on_commit(&mut editor);

    // A second editor bound to the same document reads the nesting back.
    let mut peer = BlockEditor::new();
    peer.bind_blocks(&mut state.doc, list);
    assert_eq!(depths(&peer), [0, 1]);

    assert!(state.undo(None, None).is_some());
    editor.sync(&mut state.doc, &ROOT);
    assert_eq!(depths(&editor), [0, 0]);
    assert!(state.redo(None, None).is_some());
    editor.sync(&mut state.doc, &ROOT);
    assert_eq!(depths(&editor), [0, 1]);
    assert_eq!(
        editor
            .blocks()
            .iter()
            .map(|block| block.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
}
