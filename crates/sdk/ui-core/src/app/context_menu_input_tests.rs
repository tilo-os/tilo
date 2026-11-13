//! A menu row a person presses with the pointer runs when the button comes up, in a window of its
//! own as in the frame harness: the press stays the pressed widget's until it is released.

use super::*;
use std::cell::Cell;
use std::rc::Rc;

use vello::kurbo::Point;

use crate::widget::{ContextMenuArea, MenuEntry, Spacer};

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

fn area(chosen: &Rc<Cell<usize>>) -> ContextMenuArea {
    let chosen = Rc::clone(chosen);
    ContextMenuArea::new(Box::new(Spacer::fixed(300.0)), move |_| {
        (1..=4)
            .map(|index| {
                let chosen = Rc::clone(&chosen);
                MenuEntry::new(format!("Action {index}"), move || chosen.set(index))
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

fn menu_row(app: &App, index: usize) -> Point {
    let root = app
        .root_widget
        .as_any()
        .and_then(|any| any.downcast_ref::<ContextMenuArea>());
    root.expect("the menu area is the root")
        .menu()
        .item_rect(index)
        .expect("the row is laid out")
        .center()
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
    let chosen = Rc::new(Cell::new(0));
    let mut app = App::new_with_controlled_inputs(crate::State::new(), Box::new(area(&chosen)));
    frames(&mut app);
    right_click(&mut app, Point::new(100.0, 100.0));
    let at = menu_row(&app, 1);
    press(&mut app, at);
    assert_eq!(chosen.get(), 0, "the press alone runs nothing");
    frames(&mut app);
    release(&mut app);
    assert_eq!(chosen.get(), 2, "the release on the row runs it");
}
