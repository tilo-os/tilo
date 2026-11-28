//! A panel kept at an edge of what holds it, moved by its header: a drag let go elsewhere says the
//! edge nearest the pointer, and a press that stays inside the slop, or a drag called off with
//! Escape, says nothing.

use tilo_ui_app::edge_slots::DockEdge;
use tilo_ui_app::headless::Driver;
use tilo_ui_app::widget::Text;
use tilo_ui_app::widget::{DeclaredPanel, FloatingPanelProps};
use tilo_ui_base::component::{Described as _, Event, Intent, Value};
use tilo_ui_base::synthetic_input;
use tilo_ui_base::widget::Widget as _;

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
    Driver::new(tilo_ui_app::State::new(), Box::new(panel), (1280, 800))
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
    let band = tilo_ui_app::widget::arrange::Stack::stack()
        .fill_width()
        .fill_height()
        .push(Box::new(Text::new("The work")))
        .push(Box::new(panel));
    let mut host = Driver::new(tilo_ui_app::State::new(), Box::new(band), (1280, 800));
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

/// A press off a panel that reports one is a dismissal; a press on the panel, even on its own
/// empty ground, is none.
#[test]
fn a_press_off_a_panel_with_a_dismiss_reports_it_and_one_on_it_does_not() {
    use tilo_ui_app::component_testing::{Node, fired};
    use tilo_ui_app::headless::harness;
    use tilo_ui_base::component::Component;
    let queue = tilo_ui_base::Intents::default();
    let mut node = Node::new("FloatingPanel", "panel", &queue)
        .prop("width", tilo_ui_base::component::Value::Float(200.0))
        .on("dismiss");
    let props = FloatingPanelProps::from_source(&mut node).expect("the panel reads");
    let mut host = harness(DeclaredPanel::from_props("panel", props));
    host.relayout();
    let rect = host.root::<DeclaredPanel>().get_rect();

    host.hover_at(rect.x1 + 60.0, rect.y1 + 60.0);
    host.click_at(rect.x1 + 60.0, rect.y1 + 60.0);
    assert_eq!(queue.take(), [fired("panel", "dismiss", None)]);

    let (x, y) = (
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    host.hover_at(x, y);
    host.click_at(x, y);
    assert!(
        queue.take().is_empty(),
        "a press on the panel dismisses nothing"
    );
}

/// A declared popover open in a window that starts at a single pixel and then takes its real
/// size, as a session's surface does: once the window settles, what assistive technology reads
/// of its content stands inside the panel it paints, and a press there takes the control.
#[test]
fn a_declared_popover_is_read_and_pressed_where_it_paints() {
    use std::{cell::Cell, rc::Rc};
    use tilo_ui_app::widget::arrange::{Align, Distribute, Row, Stack};
    use tilo_ui_app::widget::{DeclaredPopover, PopoverProps, PopoverSide};
    use vello::kurbo::Rect;
    let _settled = tilo_ui_base::motion::testing::settled();
    let pressed = Rc::new(Cell::new(false));
    let shut_down = {
        let pressed = Rc::clone(&pressed);
        tilo_ui_app::widget::Button::new("shut-down", "Shut down")
            .on_click(move || pressed.set(true))
    };
    let content = Stack::stack()
        .push(Box::new(tilo_ui_app::widget::Text::new("Power")))
        .push(Box::new(tilo_ui_app::widget::Button::new("lock", "Lock")))
        .push(Box::new(shut_down));
    let popover = DeclaredPopover::new(PopoverProps {
        label: Some("Power menu".into()),
        open: true,
        side: Some(PopoverSide::Below),
        content: vec![Box::new(content)],
        ..PopoverProps::default()
    });
    let row = Row::row()
        .fill_width()
        .align(Align::Start)
        .distribute(Distribute::End)
        .push(Box::new(popover));
    let root = Stack::stack()
        .fill_width()
        .push(Box::new(tilo_ui_base::widget::Spacer::fixed(40.0)))
        .push(Box::new(row));
    let mut driver = Driver::for_root(
        tilo_ui_app::State::new(),
        root,
        (1, 1),
        tilo_ui_base::theme::Theme::dark(),
    );
    driver.poll_frame().expect("the first frame lays out");
    driver
        .resize((800, 600))
        .expect("the window takes its size");
    let settled = (0..20).any(|_| !driver.poll_frame().expect("a frame lays out"));
    assert!(settled, "the window comes to rest at its size");

    let panel = {
        let mut root = driver.root_mut::<Stack>();
        let row = &mut *root.children_mut().remove(1);
        let declared = row.children_mut().remove(0);
        declared
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<DeclaredPopover>())
            .expect("the declared popover")
            .popover()
            .rect()
    };
    let id = driver
        .a11y_node_named("Shut down")
        .expect("the panel's control is read");
    let bounds = driver
        .a11y_node(id)
        .and_then(|node| node.bounds())
        .expect("its bounds");
    let bounds = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
    assert!(
        panel.contains_rect(bounds),
        "the control {bounds:?} is read inside the panel painted at {panel:?}"
    );

    let centre = bounds.center();
    driver.hover_at(centre.x, centre.y);
    driver.click_at(centre.x, centre.y);
    assert!(
        pressed.get(),
        "a press where the control is read at takes it"
    );
}

/// A page of thirty lines, as a panel's body.
fn a_page_of_lines() -> tilo_ui_base::widget::Element {
    use tilo_ui_app::widget::Text;
    use tilo_ui_app::widget::arrange::Stack;
    let mut page = Stack::stack().fill_width();
    for index in 0..30 {
        page = page.push(Box::new(Text::new(&format!("line {index}"))));
    }
    Box::new(page)
}

/// A panel that fills its width and the height it is offered, holding `body`.
fn a_filling_panel(body: tilo_ui_base::widget::Element) -> DeclaredPanel {
    DeclaredPanel::from_props(
        "notices",
        FloatingPanelProps {
            fill: true,
            fill_height: true,
            body: vec![body],
            ..FloatingPanelProps::default()
        },
    )
}

/// The panel in a filling row in a filling stack, as a surface's body is laid: a view call's
/// `fill` gives the panel the room.
fn a_panel_in_a_filling_row(panel: DeclaredPanel) -> tilo_ui_app::widget::arrange::Stack {
    use tilo_ui_app::widget::Fitted;
    use tilo_ui_app::widget::arrange::{Fit, Part, Row, Stack};
    let row = Row::row()
        .fill_width()
        .fill_height()
        .push_part(Part::new(Box::new(Fitted::new(
            Box::new(panel),
            Fit::fill(1.0),
        ))));
    Stack::stack()
        .fill_width()
        .fill_height()
        .push_part(Part::new(Box::new(Fitted::new(
            Box::new(row),
            Fit::fill(1.0),
        ))))
}

/// Every widget's name and rect in `driver`, in tree order.
fn named_rects(driver: &Driver) -> Vec<(String, [f64; 4])> {
    driver
        .inspection()
        .root
        .nodes()
        .into_iter()
        .map(|node| (node.widget.clone(), node.rect))
        .collect()
}

/// A panel a room is measured for takes the room's height, and its body scrolls inside it: a
/// page taller than the room shows under the panel's header and scrolls, never an empty column
/// with its content cut off at the foot.
#[test]
fn a_filling_panel_caps_its_body_in_the_room_it_is_given() {
    use tilo_ui_app::widget::Container;
    let stack = a_panel_in_a_filling_row(a_filling_panel(a_page_of_lines()));
    let place = Container::new()
        .with_no_padding()
        .with_id("place")
        .with_height(360.0)
        .with_child(Box::new(stack));
    let mut driver = Driver::new(tilo_ui_app::State::new(), Box::new(place), (304, 400));
    driver.relayout();
    let nodes = named_rects(&driver);
    let panel = nodes
        .iter()
        .find(|(widget, _)| widget == "FloatingPanel")
        .map(|(_, rect)| *rect)
        .expect("the panel is laid out");
    assert!(
        (panel[3] - 360.0).abs() < 2.0,
        "the panel takes the room's height: {panel:?}"
    );
    let scroll = nodes
        .iter()
        .find(|(widget, _)| widget == "Scrollable")
        .map(|(_, rect)| *rect)
        .expect("the panel's body scrolls");
    assert!(
        scroll[3] > 300.0 && scroll[3] <= panel[3] + 0.5 && scroll[2] > 100.0,
        "the body is the room less the header, with the page inside it: {scroll:?} in {panel:?}"
    );
}

/// A panel that fills, standing in a band's scroll (whose content is as tall as what it holds),
/// still shows what it holds through a scrolling body: the panel stands at its content's height
/// there, and the page inside it scrolls in the panel's own scroller, never an empty column.
#[test]
fn a_filling_panel_in_a_scroll_still_scrolls_its_body() {
    use tilo_ui_app::widget::Scrollable;
    let stack = a_panel_in_a_filling_row(a_filling_panel(a_page_of_lines()));
    let scroll = Scrollable::new_vertical()
        .fill_parent_height()
        .with_child(Box::new(stack));
    let mut driver = Driver::new(tilo_ui_app::State::new(), Box::new(scroll), (304, 360));
    driver.relayout();
    let nodes = named_rects(&driver);
    assert_eq!(
        nodes.iter().filter(|(w, _)| w == "FloatingPanel").count(),
        1,
        "the panel stands"
    );
    assert!(
        nodes.iter().filter(|(w, _)| w == "Scrollable").count() >= 2,
        "the panel's body is a scroller of its own, beside the band's"
    );
    assert!(
        nodes
            .iter()
            .filter(|(w, r)| w == "Text" && r[3] > 0.0)
            .count()
            >= 30,
        "the page shows under the body's scroller"
    );
}
