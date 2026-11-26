//! A window showing a live stream shows the frame that arrived last, at the size it arrived, once
//! the next frame lays it out.
use std::sync::Arc;

use tilo_ui::State;
use tilo_ui::headless::Driver;
use tilo_ui::live_pictures::{address, forget, publish};
use tilo_ui::pictures::Pictures;
use tilo_ui::widget::{Image, PictureSource, Row};

#[test]
fn a_window_shows_the_newest_frame_at_the_size_it_arrived() {
    let slot = "stream-draws-newest";
    publish(slot, 1, 4, 2, Arc::new(vec![200_u8; 4 * 2 * 4]));
    let image = Image::from_picture(Pictures::window(), PictureSource { src: address(slot) });
    let mut harness = Driver::new(
        State::new(),
        Box::new(Row::new().push(Box::new(image))),
        (40, 40),
    );
    let first = harness.root_widget().children()[0].get_rect();
    assert_eq!(
        (first.width(), first.height()),
        (4.0, 2.0),
        "the window's picture is the frame it was sent, at the size it arrived"
    );
    publish(slot, 2, 2, 4, Arc::new(vec![210_u8; 2 * 4 * 4]));
    harness.recompose().expect("the next frame lays out");
    let next = harness.root_widget().children()[0].get_rect();
    assert_eq!(
        (next.width(), next.height()),
        (2.0, 4.0),
        "a frame that arrived after the last draw is what the window shows"
    );
    forget(slot);
}
