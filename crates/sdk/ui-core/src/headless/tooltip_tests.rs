//! A host that runs a widget tree of its own — the desktop — sends its input through
//! [`FrameHarness::send`], and what its controls name of themselves is revealed there on the same dwell
//! a window of its own uses.

use super::*;

struct Pane {
    rect: Rect,
}

impl Widget for Pane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(
            "Groceries and the whole week's list",
        ))
    }

    fn name(&self) -> &'static str {
        "Pane"
    }
}

#[test]
fn a_widget_trees_input_reveals_the_tip_under_a_resting_pointer() {
    let mut frame = FrameHarness::new(
        State::new(),
        Box::new(Pane { rect: Rect::ZERO }),
        (400, 300),
    );
    frame.relayout();
    let resting = Point::new(120.0, 80.0);
    let settled = crate::time::Instant::now();
    crate::time::testing::with_monotonic_time(settled, || {
        frame.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(resting.x, resting.y),
        });
    });
    assert!(
        frame.tooltips.visible().is_none(),
        "the pointer has just arrived: nothing is named yet"
    );
    for millis in [700, 1100] {
        crate::time::testing::with_monotonic_time(
            settled + crate::time::Duration::from_millis(millis),
            || {
                frame.poll_frame().expect("a frame after the dwell");
            },
        );
    }
    assert_eq!(
        frame.tooltips.drawn().map(|(text, _)| text),
        Some("Groceries and the whole week's list"),
        "what the pointer rests on names itself once the dwell has run"
    );
}
