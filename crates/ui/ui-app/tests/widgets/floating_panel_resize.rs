//! A panel the person resizes by its trailing edge: the edge takes hold, the panel follows the
//! hand, and the width it is let go at is what the view hears.

use tilo_ui::headless::Driver;
use tilo_ui::widget::internals::primitives::surfaces_component::{
    DeclaredPanel, FloatingPanelProps,
};
use tilo_ui_base::synthetic_input;
use tilo_ui_base::{component::Described, component::Event, component::Intent, component::Value};
fn harness(queue: &tilo_ui_base::Intents<tilo_ui_base::component::Fired>) -> Driver {
    let panel = DeclaredPanel::from_props(
        "card",
        FloatingPanelProps {
            width: Some(300.0),
            label: Some("Calendar".into()),
            resized: Event::View(Intent::new(queue, "card", "resized")),
            ..FloatingPanelProps::default()
        },
    );
    Driver::new(tilo_ui::State::new(), Box::new(panel), (800, 600))
}

fn resized(queue: &tilo_ui_base::Intents<tilo_ui_base::component::Fired>) -> Vec<f64> {
    queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "resized")
        .map(|fired| match fired.value {
            Some(Value::Float(width)) => width,
            other => panic!("a width: {other:?}"),
        })
        .collect()
}

#[test]
fn the_trailing_edge_resizes_the_panel_and_says_its_width() {
    let queue = tilo_ui_base::Intents::default();
    let mut host = harness(&queue);
    host.relayout();
    let rect = host.root_widget().get_rect();
    assert!(
        (rect.width() - 300.0).abs() < 0.5,
        "it opens at its width: {rect:?}"
    );
    let middle = rect.center().y;

    // The edge says it can be drawn, and the middle of the panel does not.
    assert_eq!(
        host.root_widget().cursor_shape((rect.x1, middle).into()),
        Some(winit::window::CursorIcon::EwResize),
        "its trailing edge offers a resize"
    );
    assert_eq!(
        host.root_widget().cursor_shape(rect.center()),
        None,
        "inside it is not an edge"
    );

    // A press inside the panel takes no edge, and a drag from there resizes nothing.
    host.dispatch_to_root(&synthetic_input::cursor_moved(rect.center().x, middle));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    host.dispatch_to_root(&synthetic_input::cursor_moved(
        rect.center().x + 80.0,
        middle,
    ));
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert!(resized(&queue).is_empty(), "the body is not the edge");

    // The edge drawn out by a hundred pixels: the panel follows, and the view hears the width.
    host.dispatch_to_root(&synthetic_input::cursor_moved(rect.x1, middle));
    assert!(
        host.dispatch_to_root(&synthetic_input::mouse_press()),
        "the edge takes hold"
    );
    host.dispatch_to_root(&synthetic_input::cursor_moved(rect.x1 + 100.0, middle));
    host.relayout();
    let following = host.root_widget().get_rect();
    assert!(
        (following.width() - 400.0).abs() < 0.5,
        "the panel follows the hand: {following:?}"
    );
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert_eq!(resized(&queue), [400.0], "it is let go at its new width");

    // Drawn far in, it stops at the narrowest a card reads at.
    let rect = host.root_widget().get_rect();
    host.dispatch_to_root(&synthetic_input::cursor_moved(rect.x1, middle));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    host.dispatch_to_root(&synthetic_input::cursor_moved(rect.x1 - 390.0, middle));
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert_eq!(resized(&queue), [200.0], "no narrower than a line of words");
}
