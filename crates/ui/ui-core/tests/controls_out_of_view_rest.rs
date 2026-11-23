//! A control scrolled out of its view is left out of the frame, so nothing of it is on screen to
//! go stale: once the frame is done it rests like every drawn control, and an idle window asks
//! for no more frames however much of its page lies below the fold.

use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::widget::{
    Checkbox, Column, Container, Scrollable, Segment, Slider, Toggle, ToggleGroup,
};

#[test]
fn controls_scrolled_out_of_view_ask_for_no_more_frames() {
    let page = Column::new()
        .with_no_padding()
        .push(Box::new(
            Container::new().with_no_padding().with_size(360.0, 1500.0),
        ))
        .push(Box::new(Toggle::given("switch", true)))
        .push(Box::new(ToggleGroup::new(
            vec![Segment::new("Light"), Segment::new("Dark")],
            1,
        )))
        .push(Box::new(Checkbox::given(true)))
        .push(Box::new(Slider::new("level", "level")));
    let view = Scrollable::new_vertical().with_child(Box::new(page));
    let mut window = FrameHarness::new(State::new(), Box::new(view), (400, 300));
    let mut frames = 0;
    while window.poll_frame().expect("the page lays out") {
        frames += 1;
        let schedule = window.frame_schedule();
        assert!(
            frames < 10,
            "the idle page still asks for frames after {frames}: {schedule:?}"
        );
    }
    let schedule = window.frame_schedule();
    assert!(
        !schedule.needs_redraw && !schedule.needs_animation_frame,
        "an idle page with its controls out of view asks for a frame: {schedule:?}"
    );
}
