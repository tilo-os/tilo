use super::*;
use crate::a11y::AccessNode;
use crate::widget::{Block, BlockEditor};

fn control_bounds(node: &AccessNode, target: &str) -> Option<KurboRect> {
    if node.action_target.as_deref() == Some(target) || node.semantic_key.as_deref() == Some(target)
    {
        return node.bounds.map(crate::a11y::AccessBounds::to_rect);
    }
    node.children
        .iter()
        .find_map(|child| control_bounds(child, target))
}

fn frame(app: &mut App) {
    app.root_widget.sync(&app.state.doc, &app.state.root);
    app.init_layout();
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        800,
        700,
        1.0,
        true,
        None,
    )
    .unwrap();
    app.root_widget.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut app.text_renderer,
        &app.theme,
        &PresenceCtx::default(),
    );
}

#[test]
fn image_edit_and_done_buttons_dispatch_through_the_real_host_without_authoring_changes() {
    let block = Block::paragraph("![Scan](scan.png)");
    let edit = format!("notes.image-edit:{}", block.id);
    let done = format!("notes.image-done:{}", block.id);
    let mut editor = BlockEditor::new().with_wrap_lines(true);
    editor.set_blocks(vec![block]);
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(editor));
    frame(&mut app);
    let heads = app.state.doc.get_heads();
    let had_undo = app.state.peek_last_undo().is_some();
    let edit_rect = control_bounds(&app.root_widget.accessibility().unwrap(), &edit)
        .expect("image edit button is exposed with real geometry");
    assert!(edit_rect.width() > 0.0 && edit_rect.height() > 0.0);
    // This production helper sends CursorMoved and actual press/release
    // events through the root, then drains the host's control invocations.
    // It does not call the button's semantic Activate implementation.
    assert!(app.dispatch_semantic_click(edit_rect.center().x, edit_rect.center().y));
    frame(&mut app);
    let done_rect = control_bounds(&app.root_widget.accessibility().unwrap(), &done)
        .expect("pointer activation must enter source editing");
    assert!(
        app.root_widget.ime_cursor_rect().is_some(),
        "Edit image must focus the actual source input"
    );
    assert_eq!(
        app.state.doc.get_heads(),
        heads,
        "opening image controls is view state only"
    );
    assert!(app.dispatch_semantic_click(done_rect.center().x, done_rect.center().y));
    frame(&mut app);
    let accessibility = app.root_widget.accessibility().unwrap();
    assert!(control_bounds(&accessibility, &edit).is_some());
    assert!(
        control_bounds(&accessibility, &done).is_none(),
        "Done must return to the clean preview"
    );
    assert!(
        app.root_widget.ime_cursor_rect().is_none(),
        "hidden image source must not keep an IME caret"
    );
    assert_eq!(
        app.state.doc.get_heads(),
        heads,
        "closing image controls must not change the note"
    );
    assert_eq!(app.state.peek_last_undo().is_some(), had_undo);
}
