use super::*;
use crate::shot_frame::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use tilo_ui_base::widget::Spacer;
use tilo_ui_base::{arriving::GRACE, arriving::MIN_SHOWN, arriving::frame_budget};

const SHAPE: SkeletonShape = SkeletonShape::Text { lines: 2 };

fn ms(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

fn frame_at(frame: &mut ShotFrame, view: &mut dyn Widget, at: Instant) {
    with_monotonic_time(at, || {
        frame.sync(view);
        frame.layout(view).expect("layout solves");
        let _ = frame.draw(view);
    });
}

fn content_height(frame: &mut ShotFrame) -> f32 {
    let theme = frame.theme.clone();
    tilo_ui_base::num::f32_of_f64(SkeletonFace::new(SHAPE).height(&mut frame.text, &theme, 400.0))
}

fn shown(view: &dyn Widget) -> String {
    view.children()[0].children()[0].name().to_owned()
}

fn shown_height(view: &dyn Widget) -> f64 {
    view.children()[0].children()[0].get_rect().height()
}

fn reserved_height(view: &dyn Widget) -> f64 {
    let mut reserved = None;
    view.for_each_layout_child(&mut |child| {
        if child.name() == "Reserve" {
            reserved.get_or_insert(child.get_rect().height());
        }
    });
    reserved.expect("the view reserves its box")
}

fn walk(widget: &dyn Widget, found: &mut Vec<(String, Option<String>)>) {
    found.push((
        widget.name().to_owned(),
        widget.accessibility().and_then(|node| node.name),
    ));
    for child in widget.children() {
        walk(child, found);
    }
}

#[test]
fn remote_data_shows_ground_then_a_skeleton_that_stands_its_minimum_then_cross_fades_in_without_moving()
 {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let height = content_height(&mut frame);
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::Remote);
        let view = arriving
            .view(|height| Box::new(Spacer::fixed(*height)))
            .shaped(SHAPE);
        (arriving, view)
    });

    frame_at(&mut frame, &mut view, t0);
    assert_eq!(
        view.holds_frame_until(t0),
        None,
        "a remote read holds no frame"
    );
    assert_eq!(view.step(t0), Step::Ground);
    assert_eq!(shown(&view), "Reserve");
    let reserved = reserved_height(&view);
    assert!(
        (reserved - f64::from(height)).abs() < 0.5,
        "{reserved} reserved for {height}"
    );
    assert!(view.accessibility().is_some_and(|node| node.state.busy));

    let skeleton = t0 + GRACE;
    assert_eq!(view.step(skeleton - Duration::from_millis(1)), Step::Ground);
    frame_at(&mut frame, &mut view, ms(t0, 160));
    assert_eq!(view.step(ms(t0, 160)), Step::Skeleton);
    assert_eq!(view.step(skeleton), Step::Skeleton);

    // The answer lands, but the skeleton never shows for less than its minimum.
    arriving.writer().ready(height);
    let reveal = skeleton + MIN_SHOWN;
    for at in [ms(t0, 200), ms(t0, 300), reveal - Duration::from_millis(1)] {
        frame_at(&mut frame, &mut view, at);
        assert_eq!(
            view.showing(),
            Showing::Placeholder,
            "the skeleton never flashes: at {:?}",
            at - t0
        );
    }
    with_monotonic_time(ms(t0, 300), || {
        assert_eq!(
            view.next_wake_deadline(),
            Some(reveal),
            "a deadline, not a loop"
        );
    });

    frame_at(&mut frame, &mut view, reveal);
    assert_eq!(view.showing(), Showing::Answer);
    assert_eq!(shown(&view), "Spacer");
    with_monotonic_time(reveal, || {
        assert!(view.needs_animation_frame(), "it fades in");
        assert!(view.fade.outgoing(reveal).is_some(), "over the skeleton");
    });
    assert!(
        (shown_height(&view) - reserved).abs() < 0.5,
        "nothing moved"
    );
    let settled = view.fade.settles_at().expect("fading");
    frame_at(&mut frame, &mut view, settled);
    with_monotonic_time(settled, || assert!(!view.needs_animation_frame()));
    assert!((shown_height(&view) - reserved).abs() < 0.5);
    assert!(
        (reserved_height(&view) - reserved).abs() < 0.5,
        "the box stands where it stood"
    );
}

#[test]
fn a_failure_shows_the_error_with_a_retry_that_asks_again_or_alone_when_there_is_none() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> =
            Arriving::pending(Source::Remote).on_retry(|arriving: &Arriving<f32>| {
                let _ = arriving.begin(Source::Remote);
            });
        let view = arriving.view(|height| Box::new(Spacer::fixed(*height)));
        (arriving, view)
    });
    frame_at(&mut frame, &mut view, t0);
    arriving.writer().fail(String::from("Laptop is asleep"));
    frame_at(&mut frame, &mut view, ms(t0, 40));
    assert_eq!(view.showing(), Showing::Failure);
    assert_eq!(shown(&view), "Banner");
    let mut found = Vec::new();
    walk(&view, &mut found);
    assert!(
        found.iter().any(|(name, _)| *name == "Button"),
        "the failure offers a retry: {found:?}"
    );

    with_monotonic_time(ms(t0, 50), || arriving.retry());
    frame_at(&mut frame, &mut view, ms(t0, 60));
    assert_eq!(view.showing(), Showing::Placeholder, "asked again");
    assert_eq!(
        view.step(ms(t0, 60)),
        Step::Ground,
        "a fresh read's grace period"
    );

    // A read that offers no way to retry shows its failure alone.
    let (lonely, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::Remote);
        let view = arriving.view(|height| Box::new(Spacer::fixed(*height)));
        (arriving, view)
    });
    lonely.writer().fail(String::from("gone"));
    frame_at(&mut frame, &mut view, ms(t0, 10));
    assert_eq!(view.showing(), Showing::Failure);
    let mut found = Vec::new();
    walk(&view, &mut found);
    assert!(
        !found.iter().any(|(name, _)| *name == "Button"),
        "{found:?}"
    );
}

tilo_test_env::scenario! {
    /// A view waiting on data: budget, ground, skeleton, cross-fade, refresh and failure.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_arriving_view_steps_from_ground_to_its_answer_or_its_failure;

    phase local_data_inside_the_budget_paints_on_frame_one_without_a_placeholder;
    {
        let t0 = crate::time::now();
        let mut frame = ShotFrame::new((400, 300), Theme::dark());
        let height = content_height(&mut frame);
        let (arriving, mut view) = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::pending(Source::Local);
            let view = arriving
                .view(|height| Box::new(Spacer::fixed(*height)))
                .shaped(SHAPE);
            (arriving, view)
        });
        with_monotonic_time(t0, || frame.sync(&mut view));
        assert_eq!(
            view.holds_frame_until(t0),
            Some(t0 + frame_budget()),
            "the frame that would show the view waits for the local read"
        );

        arriving.writer().ready(height);
        let landed = ms(t0, 8);
        assert_eq!(view.holds_frame_until(landed), None, "the frame goes ahead");
        frame_at(&mut frame, &mut view, landed);
        assert_eq!(view.showing(), Showing::Answer);
        assert_eq!(shown(&view), "Spacer");
        with_monotonic_time(landed, || {
            assert!(!view.needs_animation_frame(), "it lands without motion");
            assert!(view.fade.outgoing(landed).is_none());
            assert_eq!(view.next_wake_deadline(), None);
        });
    }

    phase a_local_read_past_its_budget_releases_the_frame_and_wakes_it;
    {
        let t0 = crate::time::now();
        let mut frame = ShotFrame::new((400, 300), Theme::dark());
        let mut view = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::pending(Source::Local);
            arriving.view(|height| Box::new(Spacer::fixed(*height)))
        });
        with_monotonic_time(t0, || frame.sync(&mut view));
        let budget = t0 + frame_budget();
        with_monotonic_time(t0, || {
            assert_eq!(
                view.next_wake_deadline(),
                Some(budget),
                "the window wakes to draw the frame it held"
            );
        });
        assert_eq!(view.holds_frame_until(budget), None);
    }

    phase a_pending_view_draws_no_frames_between_its_timed_steps;
    {
        let t0 = crate::time::now();
        let mut frame = ShotFrame::new((400, 300), Theme::dark());
        let mut view = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::pending(Source::Remote);
            arriving.view(|height| Box::new(Spacer::fixed(*height)))
        });
        let steps = [
            (ms(t0, 50), Step::Ground, Some(t0 + GRACE)),
            (ms(t0, 1_000), Step::Skeleton, None),
            (ms(t0, 60_000), Step::Skeleton, None),
        ];
        for (at, step, next) in steps {
            frame_at(&mut frame, &mut view, at);
            assert_eq!(view.step(at), step);
            with_monotonic_time(at, || {
                assert!(
                    !view.needs_animation_frame(),
                    "idle while pending at {step:?}"
                );
                assert!(!view.needs_redraw(), "nothing to repaint at {step:?}");
                assert_eq!(view.next_wake_deadline(), next);
            });
        }
        let skeleton_due = t0 + GRACE;
        let mut view = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::pending(Source::Remote);
            arriving.view(|height| Box::new(Spacer::fixed(*height)))
        });
        frame_at(&mut frame, &mut view, ms(t0, 50));
        with_monotonic_time(skeleton_due, || {
            assert!(view.needs_animation_frame(), "the due step is drawn");
            assert!(view.serve_deadline(skeleton_due));
        });
        frame_at(&mut frame, &mut view, skeleton_due);
        with_monotonic_time(skeleton_due, || assert!(!view.needs_animation_frame()));
        with_monotonic_time(ms(t0, 60_000), || {
            assert_eq!(
                view.accessibility().and_then(|node| node.name).as_deref(),
                Some("Loading"),
                "it says it is loading, never where from"
            );
        });
    }

    phase a_refreshed_answer_replaces_the_shown_one_without_a_placeholder;
    {
        let t0 = crate::time::now();
        let mut frame = ShotFrame::new((400, 300), Theme::dark());
        let (arriving, mut view) = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::ready(10.0);
            let view = arriving.view(|height| Box::new(Spacer::fixed(*height)));
            (arriving, view)
        });
        frame_at(&mut frame, &mut view, t0);
        assert_eq!(
            view.showing(),
            Showing::Answer,
            "an answer already here shows at once"
        );
        let completion = with_monotonic_time(ms(t0, 10), || arriving.begin(Source::Remote));
        frame_at(&mut frame, &mut view, ms(t0, 500));
        assert_eq!(view.showing(), Showing::Answer, "the shown answer stays");
        assert!(completion.finish(Ok(20.0)));
        frame_at(&mut frame, &mut view, ms(t0, 600));
        assert_eq!(view.showing(), Showing::Answer);
        assert!(
            (shown_height(&view) - 20.0).abs() < 0.5,
            "the new answer shows"
        );
    }

    phase a_placeholder_keeps_its_timing_however_often_its_view_is_built;
    {
        let t0 = crate::time::now();
        let mut frame = ShotFrame::new((400, 300), Theme::dark());
        let skeleton = t0 + GRACE;
        let mut view = with_monotonic_time(ms(t0, 120), || {
            ArrivingView::placeholder_since(Source::Remote, t0, SHAPE)
        });
        frame_at(&mut frame, view.as_mut(), ms(t0, 120));
        with_monotonic_time(ms(t0, 120), || {
            assert_eq!(view.next_wake_deadline(), Some(skeleton));
            assert!(!view.needs_animation_frame());
            assert!(
                !view.has_pending_visual_work(),
                "a capture does not wait on an answer that comes with a new view"
            );
        });
        frame_at(&mut frame, view.as_mut(), skeleton);
        let height = reserved_height(view.as_ref());
        assert!(
            (height - f64::from(content_height(&mut frame))).abs() < 0.5,
            "it reserves its shape's geometry"
        );
        with_monotonic_time(ms(t0, 2_000), || {
            assert_eq!(
                view.accessibility().and_then(|node| node.name).as_deref(),
                Some("Loading")
            );
        });
    }

    phase a_failure_moved_away_leaves_the_child_lists_agreeing;
    // A failure that moved to the box showing it takes the view out of the children this one
    // lists, walks and hands out to change at the same time: an action a route addresses by index
    // reaches the view the walk found, or nothing, never the view that moved away.
    {
        let t0 = crate::time::now();
        let mut view = with_monotonic_time(t0, || {
            let arriving: Arriving<f32> = Arriving::pending(Source::Local);
            arriving
                .view(|height| Box::new(Spacer::fixed(*height)))
                .shaped(SHAPE)
        });
        view.respoken = true;
        view.reveal.set(Reveal {
            showing: Showing::Failure,
            revision: 0,
            speaks: false,
        });
        assert!(view.failure_moved_away());
        let listed = view.children().len();
        let mut walked = 0;
        view.for_each_child(&mut |_| walked += 1);
        let handed = view.children_mut().len();
        assert_eq!(listed, handed, "the lists hold the same children");
        assert_eq!(walked, listed, "the walk visits the children listed");
        assert_eq!(
            listed, 0,
            "the view that moved its failure away is no child here"
        );
    }
}
