//! A scroll view settling on the detents its content offers: a board's lanes,
//! one at a time, on a pane too narrow to show two side by side.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::Scrollable;
use crate::headless::Framed;
use crate::testing::dispatch;
use crate::theme::Theme;
use crate::time::testing::with_monotonic_time;
use crate::time::{Duration, Instant};
use crate::tokens::{EffectiveMotion, Motion, Rhythm};
use crate::widget::{Board, Container, Element, Text, Widget};
use automerge::AutoCommit;
use winit::event::{MouseScrollDelta, TouchPhase};
use winit::keyboard::NamedKey;

fn lanes(count: usize) -> Board {
    let mut board = Board::new();
    for index in 0..count {
        let key = format!("lane-{index}");
        let header: Element = Box::new(Text::new(&key));
        let header = Some(header);
        let card: Element = Box::new(Container::new().with_size(160.0, 40.0));
        board = board.lane(
            key.clone(),
            header,
            vec![(format!("{key}-card"), card)],
            None,
        );
    }
    board
}

/// `board` in the sideways scroll view a page gives it.
fn board_in_a_scroller(board: Board) -> Scrollable {
    Scrollable::new_horizontal()
        .with_content_min_viewport_width()
        .with_child(Box::new(board))
}

/// `board` laid out in a `pane` wide view, as a page shows it.
fn board_on_a_pane(board: Board, pane: u32) -> Framed<Scrollable> {
    Framed::new(
        crate::State::new(),
        board_in_a_scroller(board),
        (pane, 844),
        Theme::dark(),
    )
}

fn lane_pitch() -> f32 {
    let theme = Theme::dark();
    Board::lane_width(&theme, 390.0) + theme.tokens.spacing.rhythm.get(Rhythm::Sibling)
}

/// Move the content by `dx` over `steps` samples `16 ms` apart, as a finger or
/// a touchpad reports it, and answer when the last one landed.
fn drag(view: &mut Framed<Scrollable>, from: Instant, dx: f32, steps: u32) -> Instant {
    let mut at = from;
    for _ in 0..steps {
        with_monotonic_time(at, || {
            view.pan(dx / steps as f32, 0.0);
        });
        at += Duration::from_millis(16);
    }
    at
}

/// Let go at `at` without another sample, as the gesture's `Ended` does.
fn let_go(view: &mut Framed<Scrollable>, at: Instant) {
    let lift = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 0.0));
    with_monotonic_time(at, || {
        view.on_wheel(&lift, TouchPhase::Ended, false, 1.0);
    });
}

/// Run the frames the view asks for at `60 Hz` from `from`, and answer where
/// the content came to rest.
fn run_out(view: &mut Framed<Scrollable>, from: Instant) -> f32 {
    let doc = AutoCommit::new();
    let mut frame = from;
    for _ in 0..300 {
        if !view.needs_animation_frame() {
            break;
        }
        frame += Duration::from_nanos(16_666_667);
        with_monotonic_time(frame, || {
            view.update(&doc, &automerge::ROOT);
        });
        view.sync(&doc, &automerge::ROOT);
        view.mark_drawn();
    }
    view.offset_x()
}

/// The detents a phone-width board offers the view showing it: one per lane,
/// a lane and its gap apart, the first at the start of the content.
#[test]
fn a_compact_board_offers_a_detent_per_lane() {
    let view = board_on_a_pane(lanes(3), 390);
    let detents = &view.snap_x;
    assert_eq!(detents.len(), 3, "one per lane: {detents:?}");
    assert!(
        detents[0].abs() < 0.5,
        "the first lane is the start of the content: {detents:?}"
    );
    let pitch = lane_pitch();
    assert!(
        (detents[1] - detents[0] - pitch).abs() < 0.5,
        "the second lane stands a lane and a gap on: {detents:?}"
    );
    assert!(
        detents[2] > detents[1] && detents[2] <= detents[1] + pitch + 0.5,
        "the last lane's detent comes after the second: {detents:?}"
    );
}

/// A fling that is still moving settles a lane on, not halfway between two:
/// the view carries the gesture's speed into the settle.
#[test]
fn a_fling_settles_on_the_next_lane() {
    let mut view = board_on_a_pane(lanes(3), 390);
    let at = crate::time::now();
    let released = drag(&mut view, at, 90.0, 4);
    let_go(&mut view, released);
    let rest = run_out(&mut view, released);
    let next = view.snap_x[1];
    assert!(
        (f64::from(rest) - f64::from(next)).abs() < 0.5,
        "the fling settled on the next lane's edge {next}: {rest}"
    );
}

/// A drag that comes to rest before it is let go returns to the lane it was
/// thrown from: it never left that lane far enough to cross into the next.
#[test]
fn a_drag_that_comes_to_rest_returns_to_its_lane() {
    let mut view = board_on_a_pane(lanes(3), 390);
    let at = crate::time::now();
    let lane = view.snap_x[1];
    with_monotonic_time(at, || {
        view.move_by(lane, 0.0);
    });
    let settled = at + Duration::from_millis(500);
    let released = drag(&mut view, settled, 90.0, 4);
    let_go(&mut view, released + Duration::from_millis(300));
    let rest = run_out(&mut view, released + Duration::from_millis(300));
    assert!(
        (f64::from(rest) - f64::from(lane)).abs() < 0.5,
        "the drag came back to the lane it was in {lane}: {rest}"
    );
}

/// The same drag let go while still moving settles on the lane ahead: the
/// detents behind the way the hand went are never where it comes to rest.
#[test]
fn a_release_still_moving_settles_ahead_never_back() {
    let mut view = board_on_a_pane(lanes(3), 390);
    let at = crate::time::now();
    let lane = view.snap_x[1];
    with_monotonic_time(at, || {
        view.move_by(lane, 0.0);
    });
    let released = drag(&mut view, at + Duration::from_millis(500), 90.0, 4);
    let_go(&mut view, released);
    let rest = run_out(&mut view, released);
    let ahead = *view.snap_x.last().expect("the last lane");
    assert!(
        (f64::from(rest) - f64::from(ahead)).abs() < 0.5,
        "the release settled ahead on {ahead}, not back: {rest}"
    );
    assert!(
        rest > lane,
        "the content went on, never back: {rest} from {lane}"
    );
}

/// A key walks the lanes one at a time: it moves the view a line, and the
/// view lands on the next lane rather than a line further on.
#[test]
fn a_key_walks_the_lanes_one_at_a_time() {
    let mut view = board_on_a_pane(lanes(3), 390);
    let at = crate::time::now();
    view.set_focus(true);
    let next = view.snap_x[1];
    with_monotonic_time(at, || {
        assert!(
            dispatch(
                &mut *view,
                &crate::synthetic_input::named_key(NamedKey::ArrowRight)
            ),
            "the view takes the key"
        );
    });
    let rest = run_out(&mut view, at);
    assert!(
        (f64::from(rest) - f64::from(next)).abs() < 0.5,
        "the key landed on the next lane's edge {next}: {rest}"
    );
}

/// Under reduced motion a release lands on the lane at once: no animation
/// runs, and the view asks for no frame to run one.
#[test]
fn under_reduced_motion_a_release_lands_on_the_lane_at_once() {
    let mut view = board_on_a_pane(lanes(3), 390);
    view.motion = EffectiveMotion::new(Motion::default(), true);
    let at = crate::time::now();
    let released = drag(&mut view, at, 90.0, 4);
    let_go(&mut view, released);
    let next = view.snap_x[1];
    assert!(
        (view.offset_x() - next).abs() < 0.5,
        "the release landed on the next lane's edge {next}: {}",
        view.offset_x()
    );
    assert!(
        !view.needs_animation_frame(),
        "reduced motion animates nothing"
    );
}

/// A board wide enough to show its lanes side by side offers no detents: the content stays
/// where the hand left it, and a fling coasts on from there, as a free scroll does.
#[test]
fn a_board_wide_enough_for_its_lanes_scrolls_freely() {
    let mut view = board_on_a_pane(lanes(6), 1280);
    assert!(
        view.snap_x.is_empty(),
        "a pane showing its lanes side by side offers none: {:?}",
        view.snap_x
    );
    assert!(!view.snaps(), "and so it does not snap");
    let at = crate::time::now();
    let released = drag(&mut view, at, 90.0, 4);
    let paused = released + Duration::from_millis(300);
    let_go(&mut view, paused);
    let rest = run_out(&mut view, paused);
    assert!(
        (rest - 90.0).abs() < 0.5,
        "the content stays where the hand left it: {rest}"
    );
    let pitch = f64::from(lane_pitch());
    assert!(
        [1.0, 2.0, 3.0]
            .iter()
            .all(|lanes| (f64::from(rest) - pitch * lanes).abs() > 2.0),
        "no lane edge pulled it: {rest}"
    );
    let released = drag(&mut view, paused + Duration::from_millis(500), 90.0, 4);
    let_go(&mut view, released);
    let flung = run_out(&mut view, released);
    assert!(
        flung > 180.0,
        "the fling coasts on past the hand: {flung} from 90"
    );
}
