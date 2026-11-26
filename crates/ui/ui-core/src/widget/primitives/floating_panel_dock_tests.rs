//! A panel kept at an edge of what holds it, moved by its header: a drag let go elsewhere says the
//! edge nearest the pointer, and a press that stays inside the slop, or a drag called off with
//! Escape, says nothing.

use super::surfaces_component::{DeclaredPanel, FloatingPanelProps};
use crate::edge_slots::DockEdge;
use crate::headless::Driver;
use crate::widget::Text;
use tilo_ui_base::component::{Described as _, Event, Intent, Value};
use tilo_ui_base::synthetic_input;

fn harness(queue: &tilo_ui_base::Intents<tilo_ui_base::component::Fired>) -> Driver {
    let panel = DeclaredPanel::from_props(
        "people",
        FloatingPanelProps {
            width: Some(300.0),
            label: Some("People".into()),
            header: Some(Box::new(Text::new("People"))),
            body: vec![Box::new(Text::new("Mia Clayton"))],
            edge: Some(DockEdge::Right),
            docked: Event::View(Intent::new(queue, "people", "docked")),
            ..FloatingPanelProps::default()
        },
    );
    Driver::new(crate::State::new(), Box::new(panel), (1280, 800))
}

fn docked(queue: &tilo_ui_base::Intents<tilo_ui_base::component::Fired>) -> Vec<String> {
    queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "docked")
        .map(|fired| match fired.value {
            Some(Value::Text(edge)) => edge.clone(),
            other => panic!("an edge: {other:?}"),
        })
        .collect()
}

#[test]
fn a_panel_kept_at_an_edge_is_dragged_to_another_and_a_called_off_drag_moves_nothing() {
    let queue = tilo_ui_base::Intents::default();
    let mut host = harness(&queue);
    host.relayout();
    let rect = host.root_widget().get_rect();
    assert!(
        rect.x1 > 1200.0 && rect.x0 > 800.0,
        "it waits at the right edge: {rect:?}"
    );
    // Its header, at the panel's top.
    let grip = (rect.x0 + 40.0, rect.y0 + 16.0);

    // A press that stays inside the slop is not a drag.
    host.dispatch_to_root(&synthetic_input::cursor_moved(grip.0, grip.1));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    host.dispatch_to_root(&synthetic_input::cursor_moved(grip.0 + 2.0, grip.1));
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert!(docked(&queue).is_empty(), "a press is not a move");

    // Taken by its header to the bottom-left corner and let go there: that is its edge now.
    host.dispatch_to_root(&synthetic_input::cursor_moved(grip.0, grip.1));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        host.dispatch_to_root(&synthetic_input::cursor_moved(
            grip.0 + (60.0 - grip.0) * t,
            grip.1 + (760.0 - grip.1) * t,
        ));
    }
    // Held, it stands under the hand, and what it holds is read there, with no layout solved
    // again for it.
    let held = host
        .a11y_node_named("Mia Clayton")
        .and_then(|id| host.a11y_node(id))
        .and_then(|node| node.bounds())
        .expect("the panel says who is here");
    assert!(
        held.x0 < 400.0,
        "what it holds goes with the hand: {held:?}, from {rect:?}"
    );
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert_eq!(docked(&queue), ["bottom_left"], "let go at the corner");

    // A drag called off with Escape says nothing, nor does the button let go after it.
    host.dispatch_to_root(&synthetic_input::cursor_moved(grip.0, grip.1));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    host.dispatch_to_root(&synthetic_input::cursor_moved(400.0, 400.0));
    host.dispatch_to_root(&synthetic_input::named_key(
        winit::keyboard::NamedKey::Escape,
    ));
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert!(docked(&queue).is_empty(), "the drag was called off");
}

/// Kept at an edge inside a stack that holds other things, a panel is placed in the whole stack:
/// the room it stands in is the stack's, not the part of it the panel would take in the flow.
#[test]
fn a_panel_kept_at_an_edge_of_a_stack_stands_at_that_edge_of_the_whole_stack() {
    let queue = tilo_ui_base::Intents::default();
    let panel = DeclaredPanel::from_props(
        "people",
        FloatingPanelProps {
            width: Some(300.0),
            label: Some("People".into()),
            header: Some(Box::new(Text::new("People"))),
            body: vec![Box::new(Text::new("Mia Clayton"))],
            edge: Some(DockEdge::BottomRight),
            docked: Event::View(Intent::new(&queue, "people", "docked")),
            ..FloatingPanelProps::default()
        },
    );
    let band = crate::widget::arrange::Stack::stack()
        .fill_width()
        .fill_height()
        .push(Box::new(Text::new("The work")))
        .push(Box::new(panel));
    let mut host = Driver::new(crate::State::new(), Box::new(band), (1280, 800));
    host.relayout();
    let mia = host
        .a11y_node_named("Mia Clayton")
        .and_then(|id| host.a11y_node(id))
        .and_then(|node| node.bounds())
        .expect("the panel says who is here");
    assert!(
        mia.x0 > 640.0 && mia.y0 > 600.0 && mia.y1 < 800.0,
        "it stands at the stack's bottom-right corner: {mia:?}"
    );
}
