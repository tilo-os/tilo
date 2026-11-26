use super::*;
use crate::widget::{Banner, Button, Column, Container};

/// The widget whose test id is `id`, where it stands.
fn rect_of(app: &App, id: &str) -> vello::kurbo::Rect {
    let mut pending: Vec<&dyn Widget> = vec![app.root_widget.as_ref()];
    while let Some(widget) = pending.pop() {
        if widget.test_id() == Some(id) {
            return widget.get_rect();
        }
        pending.extend(widget.children());
    }
    panic!("{id} is in the tree");
}

/// Assistive technology reads every widget's bounds, and a frame that placed only what the views
/// moved has it place them all again, outside any frame. That placement stands a widget's own
/// parts on the grid the frame laid out on: a banner's action stays where the frame drew it, on
/// whole device pixels at 1.5, rather than moving to the nearest whole logical pixel.
#[test]
fn placing_every_widget_again_keeps_the_frames_device_pixels() {
    let on_grid = |edge: f64| ((edge * 1.5) - (edge * 1.5).round()).abs() < 1e-6;
    for above in [10.0, 10.5, 11.0] {
        let root: Box<dyn Widget> = Box::new(
            Column::new()
                .push(Box::new(
                    Container::new().with_no_padding().with_size(200.0, above),
                ))
                .push(Box::new(
                    Banner::warning("Sync paused")
                        .with_message("Files sync again when you are online.")
                        .with_action(Button::new("sync.retry", "Retry")),
                )),
        );
        let mut app = App::new_with_controlled_inputs(State::new(), root);
        app.init_layout();
        app.layout.layout_dirty = true;
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 390, 844, 1.5, false, None).unwrap();
        let framed = rect_of(&app, "sync.retry");
        assert!(
            !framed.is_zero_area(),
            "the frame places the action: {framed:?}"
        );
        app.layout.placed_partly = true;
        app.place_all();
        let placed = rect_of(&app, "sync.retry");
        assert_eq!(
            placed, framed,
            "placed again outside the frame, the action stands where the frame put it ({above} above)"
        );
        for edge in [placed.x0, placed.y0, placed.x1, placed.y1] {
            assert!(
                on_grid(edge),
                "an edge of the action at {edge} ({above} above) is off the display's pixels: {placed:?}"
            );
        }
    }
}
