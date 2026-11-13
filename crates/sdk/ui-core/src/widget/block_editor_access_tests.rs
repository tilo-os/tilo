//! What a block's row says and does for a reader: its name, its place, the action that opens it and the
//! box that ticks a checklist block.

use super::*;

#[test]
fn block_rows_are_named_items_a_reader_opens_and_a_checklist_it_ticks() {
    let mut editor = BlockEditor::new();
    editor.set_blocks(vec![
        Block::paragraph("Send the launch notes"),
        Block::checklist(0, false, "Book the room"),
    ]);
    let first_id = editor.blocks()[0].id.clone();
    let second_id = editor.blocks()[1].id.clone();
    let node = Widget::accessibility(&editor).expect("document semantics");
    let row = |id: &str| {
        node.children
            .iter()
            .find(|child| child.semantic_key.as_deref() == Some(&*format!("notes.block:{id}")))
            .expect("the block's row")
    };
    let first = row(&first_id);
    assert_eq!(first.role, crate::a11y::Role::ListItem);
    assert_eq!(first.name.as_deref(), Some("Send the launch notes"));
    assert_eq!(first.pos_in_set, Some(1));
    assert_eq!(
        first.action_target.as_deref(),
        Some(&*format!("notes.block:{first_id}"))
    );

    let mut doc = AutoCommit::new();
    assert!(Widget::perform_semantic_action(
        &mut editor,
        &format!("notes.block:{first_id}"),
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(
        editor.focused,
        Some(0),
        "opening a block's row puts the caret in it"
    );

    let second = row(&second_id);
    assert_eq!(second.role, crate::a11y::Role::ListItem);
    let box_ = second
        .children
        .iter()
        .find(|child| child.role == crate::a11y::Role::Checkbox)
        .expect("the checklist's box is a control of its own");
    assert_eq!(box_.state.checked, Some(false));
    assert_eq!(box_.name.as_deref(), Some("Book the room"));
    assert!(Widget::perform_semantic_action(
        &mut editor,
        &format!("notes.block-check:{second_id}"),
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(
        editor.take_pending_semantic_ops(),
        vec![BlockEditOp::SetChecklist {
            block_id: second_id,
            done: true,
        }],
        "ticking the box is the same edit a press on it makes"
    );
}
