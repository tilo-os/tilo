use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::internals::toggle::test_support::*;
use tilo_ui::widget::internals::toggle::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
#[test]
fn a_vertical_swipe_on_a_knob_scrolls_and_a_horizontal_one_drags_it() {
    use tilo_ui::widget::{Column, Scrollable};
    use winit::event::TouchPhase::{Ended, Moved, Started};
    let theme = Theme::dark();
    let states: Vec<_> = (0..30)
        .map(|_| tilo_ui_base::LocalState::new(false))
        .collect();
    let mut column = Column::new();
    for state in &states {
        column = column.push(Box::new(Toggle::from_state(state)));
    }
    let mut frame = tilo_ui::app::driver::Driver::for_root(
        tilo_ui::State::new(),
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(column)),
        (360, 300),
        theme.clone(),
    );
    let offset = |frame: &tilo_ui::app::driver::Driver| frame.root::<Scrollable>().offset_y();
    let knob = |frame: &tilo_ui::app::driver::Driver, index: usize| {
        let column = frame.root::<Scrollable>().children()[0];
        let track = track_rect(column.children()[index].get_rect(), &theme);
        Point::new(track.x0 + track.height() / 2.0, track.center().y)
    };
    let at = knob(&frame, 1);
    assert_eq!(
        Widget::touch_claim(frame.root::<Scrollable>().children()[0].children()[1], at),
        tilo_ui_base::interaction::TouchClaim::DragAcross
    );

    // A finger reports every 8 ms (120 Hz) however long the test takes
    // to handle each report: a swipe's throw is read from those times,
    // and a loaded host must not turn it into a slow drag.
    let finger = |frame: &mut tilo_ui::app::driver::Driver,
                  start: tilo_ui::time::Instant,
                  reports: &[(u64, winit::event::TouchPhase, f64, f64)]| {
        let mut at = start;
        for &(id, phase, x, y) in reports {
            tilo_ui::time::testing::with_monotonic_time(at, || frame.touch(id, phase, x, y));
            at += tilo_ui::time::Duration::from_millis(8);
        }
        at
    };

    let mut swipe = vec![(1, Started, at.x, at.y)];
    for step in 1..=6 {
        swipe.push((1, Moved, at.x + 2.0, at.y - 10.0 * f64::from(step)));
    }
    swipe.push((1, Ended, at.x + 2.0, at.y - 60.0));
    let _ = finger(&mut frame, tilo_ui::time::now(), &swipe);
    assert!(offset(&frame) > 0.0, "the page scrolled");
    assert!(!states[1].get(), "the switch is untouched");
    assert!(states.iter().all(|state| !state.get()));

    assert!(frame.settle_for_capture(tilo_ui::time::Duration::from_secs(5)));
    let scrolled = offset(&frame);
    let at = knob(&frame, 6);
    assert!(at.y > 0.0 && at.y < 300.0, "on screen: {at:?}");
    let mut drag = vec![(2, Started, at.x, at.y)];
    for step in 1..=5 {
        drag.push((2, Moved, at.x + 5.0 * f64::from(step), at.y + 1.0));
    }
    drag.push((2, Ended, at.x + 25.0, at.y + 1.0));
    let next = finger(&mut frame, tilo_ui::time::now(), &drag);
    assert!(states[6].get(), "dragged on");
    assert_eq!(states.iter().filter(|state| state.get()).count(), 1);
    assert_eq!(offset(&frame), scrolled, "a drag across does not scroll");

    let at = Point::new(at.x + 20.0, at.y);
    let _ = finger(
        &mut frame,
        next,
        &[(3, Started, at.x, at.y), (3, Ended, at.x, at.y)],
    );
    assert!(!states[6].get(), "a tap flips");
    assert_eq!(offset(&frame), scrolled);
}
