use std::collections::HashMap;

use taffy::TaffyTree;
use vello::kurbo::Point;
use winit::event::TouchPhase;

use super::*;
use crate::component::{Event, Fired, Intent};
use crate::time::testing::with_monotonic_time;
use crate::time::{Duration, Instant, now};

const WIDTH: f32 = 400.0;

fn touch(phase: TouchPhase, at: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(at.x, at.y),
        force: None,
        id: 7,
    })
}

/// Where the actions the tests' swipes run are reported.
struct Runs(crate::Intents<Fired>);

impl Runs {
    fn new() -> Self {
        Self(crate::Intents::default())
    }

    /// How many ran since asked last.
    fn get(&self) -> usize {
        self.0.take().len()
    }
}

/// An action reported to `runs`.
fn counted(label: &str, runs: &Runs) -> SwipeActionProps {
    SwipeActionProps {
        label: label.to_owned(),
        icon: None,
        destructive: false,
        swipe: Event::View(Intent::new(&runs.0, label, "swipe")),
    }
}

/// A row laid out, with the tree its events are dispatched over.
struct Laid {
    row: SwipeRow,
    taffy: TaffyTree<()>,
    node: taffy::NodeId,
}

impl Laid {
    fn send(&mut self, event: &WindowEvent) -> bool {
        crate::testing::Dispatch::new()
            .layout(&self.taffy, self.node)
            .send(&mut self.row, event)
            .handled
    }

    fn offset(&self) -> f64 {
        self.row.offset()
    }
}

fn row(leading: Option<SwipeActionProps>, trailing: Option<SwipeActionProps>) -> Laid {
    let mut row = SwipeRow::new(
        Box::new(
            crate::widget::arrange::Stack::stack()
                .fill_width()
                .push(Box::new(crate::widget::Spacer::fixed(48.0))),
        ),
        leading,
        trailing,
    );
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    let node = Widget::build_layout(&mut row, &mut taffy, &Theme::dark(), &mut nodes);
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(WIDTH),
                height: taffy::AvailableSpace::Definite(48.0),
            },
        )
        .expect("the row lays out");
    Widget::apply_layout(&mut row, &taffy, node, Point::ZERO);
    Laid { row, taffy, node }
}

/// Drag the row from its middle by `dx` and let go, at `at`.
fn swipe(row: &mut Laid, dx: f64, at: Instant) {
    let start = Point::new(f64::from(WIDTH) / 2.0, 24.0);
    with_monotonic_time(at, || {
        row.send(&touch(TouchPhase::Started, start));
    });
    let steps = 6;
    for step in 1..=steps {
        let t = at + Duration::from_millis(16 * step);
        let x = start.x + dx * f64::from(u32::try_from(step).unwrap_or(0)) / f64::from(6);
        with_monotonic_time(t, || {
            row.send(&touch(TouchPhase::Moved, Point::new(x, start.y)));
        });
    }
    with_monotonic_time(at + Duration::from_millis(16 * (steps + 30)), || {
        row.send(&touch(TouchPhase::Ended, Point::new(start.x + dx, start.y)));
    });
}

#[test]
fn a_swipe_past_the_threshold_runs_its_action_and_springs_back() {
    let runs = Runs::new();
    let mut row = row(None, Some(counted("Reply", &runs)));
    let at = now();
    swipe(&mut row, -f64::from(WIDTH) * 0.6, at);
    assert_eq!(
        runs.get(),
        1,
        "past the threshold, letting go runs the action"
    );
    let later = at + Duration::from_secs(3);
    with_monotonic_time(later, || {
        assert!(
            row.offset().abs() < 0.5,
            "the row settled back, not left open"
        );
        assert!(
            !Widget::needs_animation_frame(&row.row),
            "and asks for no more frames"
        );
    });
}

#[test]
fn a_short_swipe_springs_back_without_running() {
    let runs = Runs::new();
    let mut row = row(None, Some(counted("Reply", &runs)));
    let at = now();
    swipe(&mut row, -f64::from(WIDTH) * 0.15, at);
    assert_eq!(runs.get(), 0, "short of the threshold, nothing runs");
    let just_after = at + Duration::from_millis(16 * 37);
    with_monotonic_time(just_after, || {
        assert!(row.offset() < 0.0, "the row is on its way back");
        assert!(
            Widget::needs_animation_frame(&row.row),
            "it springs back over frames"
        );
    });
}

#[test]
fn the_row_follows_the_finger_each_way_it_has_an_action_for() {
    let runs = Runs::new();
    let mut row = row(Some(counted("React", &runs)), None);
    let start = Point::new(200.0, 24.0);
    let at = now();
    with_monotonic_time(at, || {
        row.send(&touch(TouchPhase::Started, start));
        row.send(&touch(TouchPhase::Moved, Point::new(260.0, 24.0)));
        assert!(
            (row.offset() - 60.0).abs() < 0.5,
            "it is where the finger is"
        );
        row.send(&touch(TouchPhase::Moved, Point::new(120.0, 24.0)));
        assert!(
            row.offset().abs() < 0.5,
            "no trailing action: it does not move that way"
        );
    });
}

#[test]
fn a_vertical_drag_is_the_lists_scroll_and_moves_nothing() {
    let runs = Runs::new();
    let mut row = row(None, Some(counted("Reply", &runs)));
    let at = now();
    with_monotonic_time(at, || {
        row.send(&touch(TouchPhase::Started, Point::new(200.0, 10.0)));
        let handled = row.send(&touch(TouchPhase::Moved, Point::new(195.0, 60.0)));
        assert!(!handled, "the scroll is not taken");
        row.send(&touch(TouchPhase::Moved, Point::new(100.0, 60.0)));
        assert!(row.offset().abs() < 0.5);
        row.send(&touch(TouchPhase::Ended, Point::new(100.0, 60.0)));
    });
    assert_eq!(runs.get(), 0);
}

#[test]
fn a_touch_during_the_settle_catches_the_row_where_it_is_drawn() {
    let runs = Runs::new();
    let mut row = row(None, Some(counted("Reply", &runs)));
    let at = now();
    swipe(&mut row, -f64::from(WIDTH) * 0.2, at);
    let caught = at + Duration::from_millis(16 * 38);
    let held = with_monotonic_time(caught, || {
        let drawn = row.offset();
        assert!(drawn < -1.0, "still on its way back");
        row.send(&touch(TouchPhase::Started, Point::new(200.0, 24.0)));
        drawn
    });
    let later = caught + Duration::from_secs(2);
    with_monotonic_time(later, || {
        assert!(
            (row.offset() - held).abs() < 0.5,
            "held under the finger, it stays where it was caught"
        );
        row.send(&touch(TouchPhase::Moved, Point::new(140.0, 24.0)));
        assert!(
            (row.offset() - (held - 60.0)).abs() < 0.5,
            "and follows the finger from there"
        );
    });
}

#[test]
fn an_armed_or_destructive_label_reads_at_body_text_contrast_in_both_shades() {
    for theme in [Theme::light(), Theme::dark()] {
        for (destructive, armed) in [(false, true), (true, false), (true, true)] {
            let (fill, ink) = super::uncovered_colors(&theme, destructive, armed);
            let ratio = crate::a11y::contrast_ratio(ink, fill);
            assert!(
                ratio >= crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST,
                "destructive {destructive}, armed {armed}: {ratio:.2}"
            );
        }
    }
}
