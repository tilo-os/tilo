//! A menu row a person presses with the pointer runs when the button comes up, in a window of its
//! own as in the frame harness: the press stays the pressed widget's until it is released.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

use vello::kurbo::Point;

use crate::widget::{ContextMenuArea, MenuEntry};

struct Window;
impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(400, 400)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

const LABELS: [&str; 4] = ["Action 1", "Action 2", "Action 3", "Action 4"];

fn area(chosen: &Rc<RefCell<Vec<String>>>) -> ContextMenuArea {
    let content = crate::testing::MenuRows::new(&LABELS, Rc::clone(chosen));
    ContextMenuArea::new(Box::new(content), |_| {
        LABELS
            .into_iter()
            .map(|label| {
                MenuEntry::bound(
                    label,
                    crate::testing::bound_press(
                        crate::testing::MENU_ROW_RESOURCE,
                        &crate::testing::MenuRows::action_of(label),
                    ),
                )
            })
            .collect()
    })
}

fn frames(app: &mut App) {
    for _ in 0..4 {
        let mut scene = crate::vello::Scene::new();
        if !app.surface_frame(&Window, &mut scene) || !app.unsettled() {
            break;
        }
    }
}

fn send(app: &mut App, event: &winit::event::WindowEvent) {
    app.surface_input(&Window, event.clone(), false);
}

fn right_click(app: &mut App, at: Point) {
    send(app, &crate::synthetic_input::cursor_moved(at.x, at.y));
    send(
        app,
        &crate::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Right,
        ),
    );
    send(
        app,
        &crate::synthetic_input::mouse_input(
            winit::event::ElementState::Released,
            winit::event::MouseButton::Right,
        ),
    );
    frames(app);
}

fn menu(app: &App) -> &crate::widget::ContextMenu {
    let root = app.root_widget.as_any();
    let area = root.and_then(|any| any.downcast_ref::<ContextMenuArea>());
    area.expect("the menu area is the root").menu()
}

fn menu_row(app: &App, index: usize) -> Point {
    let row = menu(app).item_rect(index);
    row.expect("the row is laid out").center()
}

fn press(app: &mut App, at: Point) {
    send(app, &crate::synthetic_input::cursor_moved(at.x, at.y));
    send(app, &crate::synthetic_input::mouse_press());
}

fn release(app: &mut App) {
    send(app, &crate::synthetic_input::mouse_release());
}

#[test]
fn a_menu_row_pressed_in_a_window_runs_when_the_button_comes_up() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(area(&chosen)));
    frames(&mut app);
    right_click(&mut app, Point::new(100.0, 100.0));
    let at = menu_row(&app, 1);
    press(&mut app, at);
    assert!(chosen.borrow().is_empty(), "the press alone runs nothing");
    frames(&mut app);
    release(&mut app);
    assert_eq!(
        *chosen.borrow(),
        vec!["Action 2".to_owned()],
        "the release on the row runs the action it names"
    );
}

#[test]
fn a_right_click_on_a_row_sends_about_that_row_and_the_typing_is_the_message() {
    crate::shared_selection::set_send_host_available(true);
    let row = |id: &str, title| crate::widget::ListRow::new(id, title).item(format!("file-{id}"));
    let rows = crate::widget::Column::new()
        .push(Box::new(row("a", "Agenda")))
        .push(Box::new(row("b", "Budget")));
    let area = ContextMenuArea::new(Box::new(rows), |_| {
        vec![MenuEntry::bound(
            "Open",
            crate::testing::bound_press("test:menu", "open"),
        )]
    });
    let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(area));
    frames(&mut app);
    app.refresh_semantic_tree();
    let budget = app
        .semantic_tree()
        .find_by_name("Budget")
        .and_then(|node| node.bounds);
    right_click(&mut app, budget.expect("the row").center());
    let send_row = menu_row(
        &app,
        *menu(&app).shown_rows().last().expect("Send to… ends it"),
    );
    press(&mut app, send_row);
    release(&mut app);
    let about = crate::shared_subject::SharedSelection::of_text("Budget")
        .at(tilo_actions::SelectionPlace::item("file-b"));
    let subjects: Vec<_> = menu(&app)
        .send_to()
        .subjects()
        .iter()
        .map(|s| s.anchor.clone())
        .collect();
    assert_eq!(
        subjects,
        [about.anchor],
        "the box is about the row right-clicked, not the window"
    );
    // The row names whom the message goes to, so the keyboard is in the
    // message: what the person types is what they wrote.
    assert_eq!(
        menu(&app).send_to().focus(),
        crate::widget::SendToFocus::Message,
        "the box opens with the keyboard in the message"
    );
    send(&mut app, &crate::synthetic_input::char_key("h"));
    assert_eq!(
        menu(&app).send_to().text(),
        "h",
        "a typed key is the message, never a search for a recipient"
    );
    crate::shared_selection::set_send_host_available(false);
}
