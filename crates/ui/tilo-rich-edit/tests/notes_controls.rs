//! The note's image controls dispatched through the real host: pointer activation enters and
//! leaves source editing without changing the note.

use std::cell::Cell;

use tilo_rich_edit::{Block, BlockEditor};
use tilo_ui::State;
use tilo_ui_base::a11y::AccessNode;
use tilo_ui::app::{App, AppSurface};
use vello::Scene;
use vello::kurbo::Rect;

struct Window {
    redraw: Cell<bool>,
}

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(800, 700)
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

fn control_bounds(node: &AccessNode, target: &str) -> Option<Rect> {
    if node.action_target.as_deref() == Some(target) || node.semantic_key.as_deref() == Some(target)
    {
        return node.bounds.map(tilo_ui_base::a11y::AccessBounds::to_rect);
    }
    node.children
        .iter()
        .find_map(|child| control_bounds(child, target))
}

/// One frame of the window: the document synchronized, laid out and drawn.
fn frame(app: &mut App) {
    let window = Window {
        redraw: Cell::new(false),
    };
    app.surface_frame(&window, &mut Scene::new());
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
    let heads = app.state_for_test().doc.get_heads();
    let had_undo = app.state_for_test().peek_last_undo().is_some();
    let edit_rect = control_bounds(&app.root_widget_mut().accessibility().unwrap(), &edit)
        .expect("image edit button is exposed with real geometry");
    assert!(edit_rect.width() > 0.0 && edit_rect.height() > 0.0);
    // This production helper sends CursorMoved and actual press/release
    // events through the root, then drains the host's control invocations.
    // It does not call the button's semantic Activate implementation.
    assert!(app.semantic_click_for_test(edit_rect.center().x, edit_rect.center().y));
    frame(&mut app);
    let done_rect = control_bounds(&app.root_widget_mut().accessibility().unwrap(), &done)
        .expect("pointer activation must enter source editing");
    assert!(
        app.root_widget_mut().ime_cursor_rect().is_some(),
        "Edit image must focus the actual source input"
    );
    assert_eq!(
        app.state_for_test().doc.get_heads(),
        heads,
        "opening image controls is view state only"
    );
    assert!(app.semantic_click_for_test(done_rect.center().x, done_rect.center().y));
    frame(&mut app);
    let accessibility = app.root_widget_mut().accessibility().unwrap();
    assert!(control_bounds(&accessibility, &edit).is_some());
    assert!(
        control_bounds(&accessibility, &done).is_none(),
        "Done must return to the clean preview"
    );
    assert!(
        app.root_widget_mut().ime_cursor_rect().is_none(),
        "hidden image source must not keep an IME caret"
    );
    assert_eq!(
        app.state_for_test().doc.get_heads(),
        heads,
        "closing image controls must not change the note"
    );
    assert_eq!(app.state_for_test().peek_last_undo().is_some(), had_undo);
}
