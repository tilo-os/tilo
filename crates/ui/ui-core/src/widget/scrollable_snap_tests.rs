//! A scroll view settling on the detents its content offers: a board's lists,
//! one at a time, on a pane too narrow to show two side by side.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::Scrollable;
use crate::app::driver::Framed;
use crate::testing::dispatch;
use crate::time::testing::with_monotonic_time;
use crate::time::{Duration, Instant};
use crate::{widget::Board, widget::Container, widget::Text};
use automerge::AutoCommit;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{tokens::EffectiveMotion, tokens::Motion, tokens::Rhythm};
use tilo_ui_base::{widget::Element, widget::Widget};
use winit::event::{MouseScrollDelta, TouchPhase};
use winit::keyboard::NamedKey;

fn lists(count: usize) -> Board {
    let mut board = Board::new();
    for index in 0..count {
        let key = format!("list-{index}");
        let header: Element = Box::new(Text::new(&key));
        let header = Some(header);
        let card: Element = Box::new(Container::new().with_size(160.0, 40.0));
        board = board.list(
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

fn list_pitch() -> f32 {
    let theme = Theme::dark();
    Board::lists_width(&theme, 390.0, 1) + theme.tokens.spacing.rhythm.get(Rhythm::Sibling)
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

tilo_test_env::scenario! {
    /// List snapping of a board that scrolls sideways.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_board_snaps_to_its_lists;

    phase a_compact_board_offers_a_detent_per_list;
    // The detents a phone-width board offers the view showing it: one per list,
    // a list and its gap apart, the first at the start of the content.
    {
        let view = board_on_a_pane(lists(3), 390);
        let detents = &view.snap_x;
        assert_eq!(detents.len(), 3, "one per list: {detents:?}");
        assert!(
            detents[0].abs() < 0.5,
            "the first list is the start of the content: {detents:?}"
        );
        let pitch = list_pitch();
        assert!(
            (detents[1] - detents[0] - pitch).abs() < 0.5,
            "the second list stands a list and a gap on: {detents:?}"
        );
        assert!(
            detents[2] > detents[1] && detents[2] <= detents[1] + pitch + 0.5,
            "the last list's detent comes after the second: {detents:?}"
        );
    }

    phase a_fling_settles_on_the_next_list;
    // A fling that is still moving settles a list on, not halfway between two:
    // the view carries the gesture's speed into the settle.
    {
        let mut view = board_on_a_pane(lists(3), 390);
        let at = crate::time::now();
        let released = drag(&mut view, at, 90.0, 4);
        let_go(&mut view, released);
        let rest = run_out(&mut view, released);
        let next = view.snap_x[1];
        assert!(
            (f64::from(rest) - f64::from(next)).abs() < 0.5,
            "the fling settled on the next list's edge {next}: {rest}"
        );
    }

    phase a_drag_that_comes_to_rest_returns_to_its_list;
    // A drag that comes to rest before it is let go returns to the list it was
    // thrown from: it never left that list far enough to cross into the next.
    {
        let mut view = board_on_a_pane(lists(3), 390);
        let at = crate::time::now();
        let list = view.snap_x[1];
        with_monotonic_time(at, || {
            view.move_by(list, 0.0);
        });
        let settled = at + Duration::from_millis(500);
        let released = drag(&mut view, settled, 90.0, 4);
        let_go(&mut view, released + Duration::from_millis(300));
        let rest = run_out(&mut view, released + Duration::from_millis(300));
        assert!(
            (f64::from(rest) - f64::from(list)).abs() < 0.5,
            "the drag came back to the list it was in {list}: {rest}"
        );
    }

    phase a_release_still_moving_settles_ahead_never_back;
    // The same drag let go while still moving settles on the list ahead: the
    // detents behind the way the hand went are never where it comes to rest.
    {
        let mut view = board_on_a_pane(lists(3), 390);
        let at = crate::time::now();
        let list = view.snap_x[1];
        with_monotonic_time(at, || {
            view.move_by(list, 0.0);
        });
        let released = drag(&mut view, at + Duration::from_millis(500), 90.0, 4);
        let_go(&mut view, released);
        let rest = run_out(&mut view, released);
        let ahead = *view.snap_x.last().expect("the last list");
        assert!(
            (f64::from(rest) - f64::from(ahead)).abs() < 0.5,
            "the release settled ahead on {ahead}, not back: {rest}"
        );
        assert!(
            rest > list,
            "the content went on, never back: {rest} from {list}"
        );
    }

    phase a_key_walks_the_lists_one_at_a_time;
    // A key walks the lists one at a time: it moves the view a line, and the
    // view lands on the next list rather than a line further on.
    {
        let mut view = board_on_a_pane(lists(3), 390);
        let at = crate::time::now();
        view.set_focus(true);
        let next = view.snap_x[1];
        with_monotonic_time(at, || {
            assert!(
                dispatch(
                    &mut *view,
                    &tilo_ui_base::synthetic_input::named_key(NamedKey::ArrowRight)
                ),
                "the view takes the key"
            );
        });
        let rest = run_out(&mut view, at);
        assert!(
            (f64::from(rest) - f64::from(next)).abs() < 0.5,
            "the key landed on the next list's edge {next}: {rest}"
        );
    }

    phase under_reduced_motion_a_release_lands_on_the_list_at_once;
    // Under reduced motion a release lands on the list at once: no animation
    // runs, and the view asks for no frame to run one.
    {
        let mut view = board_on_a_pane(lists(3), 390);
        view.motion = EffectiveMotion::new(Motion::default(), true);
        let at = crate::time::now();
        let released = drag(&mut view, at, 90.0, 4);
        let_go(&mut view, released);
        let next = view.snap_x[1];
        assert!(
            (view.offset_x() - next).abs() < 0.5,
            "the release landed on the next list's edge {next}: {}",
            view.offset_x()
        );
        assert!(
            !view.needs_animation_frame(),
            "reduced motion animates nothing"
        );
    }

    phase a_board_wide_enough_for_its_lists_scrolls_freely;
    // A board wide enough to show its lists side by side offers no detents: the content stays
    // where the hand left it, and a fling coasts on from there, as a free scroll does.
    {
        let mut view = board_on_a_pane(lists(6), 1280);
        assert!(
            view.snap_x.is_empty(),
            "a pane showing its lists side by side offers none: {:?}",
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
        let pitch = f64::from(list_pitch());
        assert!(
            [1.0, 2.0, 3.0]
                .iter()
                .all(|lists| (f64::from(rest) - pitch * lists).abs() > 2.0),
            "no list edge pulled it: {rest}"
        );
        let released = drag(&mut view, paused + Duration::from_millis(500), 90.0, 4);
        let_go(&mut view, released);
        let flung = run_out(&mut view, released);
        assert!(
            flung > 180.0,
            "the fling coasts on past the hand: {flung} from 90"
        );
    }
}
