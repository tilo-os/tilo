use super::*;
use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::{Chip, Column, Tooltip};
use vello::kurbo::Rect;

/// One frame of `widget` at `at`: sync, layout, draw.
fn frame_at(frame: &mut ShotFrame, widget: &mut dyn Widget, at: crate::time::Instant) -> Scene {
    with_monotonic_time(at, || {
        frame.sync(widget);
        frame.layout(widget).expect("layout solves");
        frame.draw(widget)
    })
}

#[test]
fn a_tooltip_fading_in_inside_a_flow_paints_new_pixels_each_frame() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 200), Theme::dark());
    let tip = with_monotonic_time(t0, || {
        let mut tip = Tooltip::new("Saved")
            .with_delay(Duration::ZERO)
            .with_anchor(Rect::new(16.0, 16.0, 96.0, 48.0));
        tip.on_hover_enter(Duration::ZERO);
        tip
    });
    let mut column = Column::new().push(Box::new(tip));

    let first = frame_at(&mut frame, &mut column, t0);
    let moving = with_monotonic_time(t0, || column.needs_animation_frame());
    assert!(moving, "the fade started when the frame drew it");

    let later = t0 + Duration::from_millis(90);
    let second = frame_at(&mut frame, &mut column, later);

    assert!(
        !first.encoding_eq(&second),
        "the flow's scene is only as fresh as the child that moves: a fade \
         must repaint through the cache"
    );
}

#[test]
fn a_leaf_changed_by_a_setter_repaints() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let mut chip = Chip::new("alpha").selectable().selected(false);

    let first = frame_at(&mut frame, &mut chip, t0);
    crate::widget::Widget::mark_drawn(&mut chip);
    assert!(
        !with_monotonic_time(t0, || chip.needs_redraw()),
        "the frame that drew it rested it"
    );

    chip.set_selected(true);
    assert!(
        with_monotonic_time(t0, || chip.needs_redraw()),
        "a setter that changes the paint asks for a frame"
    );

    let second = frame_at(&mut frame, &mut chip, t0 + Duration::from_millis(16));
    crate::widget::Widget::mark_drawn(&mut chip);
    assert!(
        !first.encoding_eq(&second),
        "the selection paints new pixels"
    );
    assert!(
        !with_monotonic_time(t0 + Duration::from_millis(16), || chip.needs_redraw()),
        "and it rests after that frame"
    );
}
