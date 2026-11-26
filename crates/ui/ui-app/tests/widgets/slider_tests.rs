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
use tilo_ui::widget::internals::slider::test_support::*;
use tilo_ui::widget::internals::slider::*;
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
/// A view that builds the slider anew on every value it takes, as a volume, brightness or
/// seek slider in most apps is: a drag goes on through every rebuild, by pointer and by
/// finger.
#[test]
fn a_drag_goes_on_through_a_view_that_rebuilds_on_every_value() {
    use std::cell::Cell;
    use std::rc::Rc;
    use tilo_ui::widget::Slider;
    use tilo_ui_base::widget::Widget;
    use winit::event::TouchPhase::{Ended, Moved, Started};

    fn built(level: &Rc<Cell<f64>>, rebuilt: &Rc<Cell<bool>>) -> Slider {
        let (level, rebuilt) = (Rc::clone(level), Rc::clone(rebuilt));
        Slider::from_state(&tilo_ui_base::LocalState::new(level.get())).on_change(move |value| {
            level.set(value);
            rebuilt.set(true);
        })
    }
    fn view(d: &mut tilo_ui::app::driver::Driver, level: &Rc<Cell<f64>>, rebuilt: &Rc<Cell<bool>>) {
        if rebuilt.replace(false) {
            d.root_mut::<Slider>().replace(built(level, rebuilt));
            d.relayout();
        }
    }
    fn welded(d: &tilo_ui::app::driver::Driver, x: f64, level: f64) {
        let s = d.root::<Slider>();
        assert!(s.is_held(), "at {x}: still held");
        let expected = s.value_from_cursor_x(x);
        assert!(
            (s.value() - expected).abs() < 1e-9,
            "at {x}: {} for {expected}",
            s.value()
        );
        assert!((level - expected).abs() < 1e-9, "the model follows");
        let now = tilo_ui_base::motion::frame_now();
        assert!(
            (s.test_thumb().get(now) - s.test_fraction()).abs() < 1e-9,
            "at {x}: the thumb is under the hand"
        );
        assert!(
            !s.test_thumb().is_moving(now),
            "nothing runs under the hand"
        );
    }

    let theme = tilo_ui_base::theme::Theme::dark();
    for finger in [false, true] {
        let level = Rc::new(Cell::new(0.2));
        let rebuilt = Rc::new(Cell::new(false));
        let mut d = tilo_ui::app::driver::Driver::for_root(
            tilo_ui::State::new(),
            built(&level, &rebuilt),
            (400, 80),
            theme.clone(),
        );
        d.relayout();
        let track = d.root::<Slider>().track_rect();
        let y = track.center().y;
        let thumb = track.x0 + TRACK_PADDING + 0.2 * (track.width() - TRACK_PADDING * 2.0);
        if finger {
            d.touch(4, Started, thumb, y);
        } else {
            d.send(&tilo_ui_base::synthetic_input::cursor_moved(thumb, y));
            d.send(&tilo_ui_base::synthetic_input::mouse_press());
        }
        view(&mut d, &level, &rebuilt);
        let mut x = thumb;
        for _ in 0..8 {
            x += 25.0;
            if finger {
                d.touch(4, Moved, x, y + 3.0);
            } else {
                d.send(&tilo_ui_base::synthetic_input::cursor_moved(x, y + 3.0));
            }
            assert!(rebuilt.get(), "every value rebuilds the view");
            view(&mut d, &level, &rebuilt);
            welded(&d, x, level.get());
        }
        let left_at = level.get();
        if finger {
            d.touch(4, Ended, x, y + 3.0);
        } else {
            d.send(&tilo_ui_base::synthetic_input::mouse_release());
        }
        view(&mut d, &level, &rebuilt);
        let s = d.root::<Slider>();
        let now = tilo_ui_base::motion::frame_now();
        assert!(!s.is_held() && !Widget::wants_pointer_capture(s), "let go");
        assert_eq!(s.value(), left_at, "the release keeps the value");
        assert!(
            (s.test_thumb().get(now) - s.test_fraction()).abs() < 1e-9
                && !s.needs_animation_frame(),
            "the release leaves the thumb where it is"
        );
    }
}
