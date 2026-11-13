//! Pointing at what a report is about: the window lights what is under the
//! pointer, the click picks it, and the box opens about it — the app's own
//! menu carrying the box, with no menu around it.

use super::*;
use vello::kurbo::{Point, Rect};

use crate::floating_surface::{FloatingSurfaceHostMessage, Recipient, RecipientKind};
use crate::shared_selection::take_open_send_box;
use crate::widget::{Button, Column, ContextMenuArea, MenuEntry, Spacer};

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

/// A window whose content is a button the person can point at.
fn pointed_at_window() -> App {
    let content = Column::new()
        .push(Box::new(Spacer::fixed(80.0)))
        .push(Box::new(
            Button::new("send", "Send message").on_click(|| {}),
        ));
    let area = ContextMenuArea::new(Box::new(content), |_| {
        vec![MenuEntry::new("Report a problem", || {})]
    });
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(area));
    app.init_layout();
    app
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

fn area(app: &App) -> &ContextMenuArea {
    let root = app
        .root_widget
        .as_any()
        .and_then(|any| any.downcast_ref::<ContextMenuArea>());
    assert!(root.is_some(), "the menu area is the root");
    root.unwrap_or_else(|| unreachable!())
}

/// Where the button stands in the window.
fn button_centre(app: &App) -> Point {
    app.root_widget.as_ref().children()[1].get_rect().center()
}

/// A window asked to point at what a report is about lights what is under the
/// pointer, and a click opens its box about it — no menu around it, to the row
/// the shell named.
#[cfg(feature = "native")]
#[test]
fn pointing_picks_the_thing_under_the_pointer_for_the_box() {
    let _ = take_open_send_box();
    let mut app = pointed_at_window();
    frames(&mut app);
    let report = Recipient::found(RecipientKind::Feedback, "", "Report a problem")
        .with_detail("This computer is not set up to send reports about Tilo.");
    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: vec![report.clone()],
            pointing: true,
        }])
    );
    frames(&mut app);
    assert!(
        take_open_send_box().is_none(),
        "pointing opens no box of its own: the click does"
    );

    let centre = button_centre(&app);
    send(
        &mut app,
        &crate::synthetic_input::cursor_moved(centre.x, centre.y),
    );
    frames(&mut app);
    assert_eq!(
        app.resolve_cursor(centre),
        Some(winit::window::CursorIcon::Crosshair),
        "the window says it is waiting for the person to point"
    );

    send(
        &mut app,
        &crate::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Left,
        ),
    );
    frames(&mut app);
    let opened = area(&app).menu().send_to();
    assert!(opened.is_open(), "the click asked the window for its box");
    assert_eq!(
        opened
            .subjects()
            .first()
            .map(crate::widget::send_to::subject_label),
        Some("Send message".to_owned()),
        "the thing the person pointed at is what the box is about"
    );
    assert_eq!(
        opened.recipients(),
        [report],
        "and it goes where the shell said"
    );
    assert!(
        app.resolve_cursor(centre) != Some(winit::window::CursorIcon::Crosshair),
        "the window is done pointing"
    );
    let window = Rect::new(0.0, 0.0, 400.0, 400.0);
    assert!(
        window.contains_rect(opened.rect()),
        "the box stands inside the window it was asked in: {:?} in {window:?}",
        opened.rect()
    );
}

/// Escape leaves pointing without a box, and the window is its own again.
#[cfg(feature = "native")]
#[test]
fn escape_leaves_pointing_without_a_box() {
    let _ = take_open_send_box();
    let mut app = pointed_at_window();
    frames(&mut app);
    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: vec![],
            pointing: true,
        }])
    );
    frames(&mut app);
    send(
        &mut app,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Escape),
    );
    frames(&mut app);
    assert!(
        take_open_send_box().is_none(),
        "leaving pointing opens nothing"
    );
    assert_ne!(
        app.resolve_cursor(Point::new(10.0, 10.0)),
        Some(winit::window::CursorIcon::Crosshair),
        "pointing is over"
    );
}

/// The box the shell asks for stands up in a window whose own menu is not
/// open: every app carries one, not only the few that wrap their content in
/// an area of their own.
#[cfg(feature = "native")]
#[test]
fn the_box_the_shell_asks_for_stands_in_any_window_with_a_menu() {
    let _ = take_open_send_box();
    let mut app = pointed_at_window();
    frames(&mut app);
    assert!(
        app.apply_floating_host_messages(vec![FloatingSurfaceHostMessage::ShareSelection {
            to: vec![],
            pointing: false,
        }])
    );
    frames(&mut app);
    assert!(
        area(&app).menu().send_to().is_open(),
        "the window's own menu carries the box the shell asked for"
    );
    assert!(
        take_open_send_box().is_none(),
        "it was handed on, not left for the next frame"
    );
}
