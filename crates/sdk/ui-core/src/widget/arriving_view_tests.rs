use super::*;
use crate::arriving::{GRACE, MIN_SHOWN, NAME_SOURCE_AFTER, frame_budget};
use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::Spacer;

const SHAPE: SkeletonShape = SkeletonShape::Text { lines: 2 };

fn ms(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

/// One frame of `view` at `at`: sync, layout, draw.
fn frame_at(frame: &mut ShotFrame, view: &mut dyn Widget, at: Instant) {
    with_monotonic_time(at, || {
        frame.sync(view);
        frame.layout(view).expect("layout solves");
        let _ = frame.draw(view);
    });
}

/// The height of the content the skeleton stands in for.
fn content_height(frame: &mut ShotFrame) -> f32 {
    let theme = frame.theme.clone();
    crate::num::f32_of_f64(SkeletonFace::new(SHAPE).height(&mut frame.text, &theme, 400.0))
}

/// What the view's inner subtree is now: the reserved space, the content
/// (a `Spacer` here) or the failure (a `Banner`).
fn shown(view: &dyn Widget) -> String {
    view.children()[0].children()[0].name().to_owned()
}

/// The height the view's inner subtree takes.
fn shown_height(view: &dyn Widget) -> f64 {
    view.children()[0].children()[0].get_rect().height()
}

/// The height of the box the view reserves: what every step stands in.
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
fn local_data_inside_the_budget_paints_on_frame_one_without_a_placeholder() {
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

    // The read answers inside the budget, before the held frame is drawn.
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

#[test]
fn a_local_read_past_its_budget_releases_the_frame_and_wakes_it() {
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

#[test]
fn remote_data_shows_ground_then_a_skeleton_then_cross_fades_in_without_moving() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let height = content_height(&mut frame);
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::peer("Laptop"));
        let view = arriving
            .view(|height| Box::new(Spacer::fixed(*height)))
            .shaped(SHAPE);
        (arriving, view)
    });

    // The ground, at the content's geometry from the first frame.
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

    // The skeleton after the grace period.
    let skeleton = t0 + GRACE;
    assert_eq!(view.step(skeleton - Duration::from_millis(1)), Step::Ground);
    frame_at(&mut frame, &mut view, skeleton);
    assert_eq!(view.step(skeleton), Step::Skeleton);

    // The answer lands while the skeleton has not stood its minimum.
    arriving.writer().ready(height);
    frame_at(&mut frame, &mut view, ms(t0, 200));
    assert_eq!(
        view.showing(),
        Showing::Placeholder,
        "the skeleton never flashes"
    );

    // It cross-fades in at the reserved geometry.
    let reveal = skeleton + MIN_SHOWN;
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
fn a_skeleton_never_shows_for_less_than_its_minimum() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::host("eu1.tilo.coffee"));
        let view = arriving.view(|height| Box::new(Spacer::fixed(*height)));
        (arriving, view)
    });
    frame_at(&mut frame, &mut view, t0);
    frame_at(&mut frame, &mut view, ms(t0, 160));
    assert_eq!(view.step(ms(t0, 160)), Step::Skeleton);
    arriving.writer().ready(10.0);
    let reveal = t0 + GRACE + MIN_SHOWN;
    for at in [ms(t0, 170), ms(t0, 300), reveal - Duration::from_millis(1)] {
        frame_at(&mut frame, &mut view, at);
        assert_eq!(view.showing(), Showing::Placeholder, "at {:?}", at - t0);
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
}

#[test]
fn a_pending_view_draws_no_frames_between_its_timed_steps() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let mut view = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::peer("Laptop"));
        arriving.view(|height| Box::new(Spacer::fixed(*height)))
    });
    let steps = [
        (ms(t0, 50), Step::Ground, Some(t0 + GRACE)),
        (ms(t0, 1_000), Step::Skeleton, Some(t0 + NAME_SOURCE_AFTER)),
        (ms(t0, 2_500), Step::Named, None),
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
    // A step falling due asks for exactly the frame that shows it.
    let named_due = t0 + NAME_SOURCE_AFTER;
    let mut view = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::peer("Laptop"));
        arriving.view(|height| Box::new(Spacer::fixed(*height)))
    });
    frame_at(&mut frame, &mut view, ms(t0, 1_000));
    with_monotonic_time(named_due, || {
        assert!(view.needs_animation_frame(), "the due step is drawn");
        assert!(view.serve_deadline(named_due));
    });
    frame_at(&mut frame, &mut view, named_due);
    with_monotonic_time(named_due, || assert!(!view.needs_animation_frame()));
    with_monotonic_time(named_due, || {
        assert_eq!(
            view.accessibility().and_then(|node| node.name).as_deref(),
            Some("From Laptop"),
            "it says where the data comes from"
        );
    });
}

#[test]
fn a_failure_shows_the_error_with_a_retry_that_asks_again() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> =
            Arriving::pending(Source::peer("Laptop")).on_retry(|arriving: &Arriving<f32>| {
                let _ = arriving.begin(Source::peer("Laptop"));
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
}

#[test]
fn a_read_without_a_way_to_retry_shows_its_failure_alone() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<f32> = Arriving::pending(Source::peer("Laptop"));
        let view = arriving.view(|height| Box::new(Spacer::fixed(*height)));
        (arriving, view)
    });
    arriving.writer().fail(String::from("gone"));
    frame_at(&mut frame, &mut view, ms(t0, 10));
    assert_eq!(view.showing(), Showing::Failure);
    let mut found = Vec::new();
    walk(&view, &mut found);
    assert!(
        !found.iter().any(|(name, _)| *name == "Button"),
        "{found:?}"
    );
}

#[test]
fn a_refreshed_answer_replaces_the_shown_one_without_a_placeholder() {
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
    let completion = with_monotonic_time(ms(t0, 10), || arriving.begin(Source::peer("Laptop")));
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

#[test]
fn an_arrival_paints_its_placeholder_steps_inside_its_rect() {
    let t0 = crate::time::now();
    let mut clock = ArrivalClock::with_budget(t0, Source::host("eu1.tilo.coffee"), frame_budget());
    assert_eq!(ArrivalFace::new(SHAPE, &clock, t0).step(), Step::Ground);
    let named = ArrivalFace::new(SHAPE, &clock, t0 + NAME_SOURCE_AFTER);
    assert_eq!(named.caption().as_deref(), Some("From eu1.tilo.coffee"));
    clock.arrive(ms(t0, 3_000));
    assert_eq!(
        ArrivalFace::new(SHAPE, &clock, ms(t0, 3_000)).step(),
        Step::Shown
    );
}

#[test]
fn a_placeholder_keeps_its_timing_however_often_its_view_is_built() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((400, 300), Theme::dark());
    let skeleton = t0 + GRACE;
    // The view around it is built again after its read began: the
    // placeholder still steps on from when the read began.
    let mut view = with_monotonic_time(ms(t0, 120), || {
        ArrivingView::placeholder_since(Source::host("eu1.tilo.coffee"), t0, SHAPE)
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
    with_monotonic_time(t0 + NAME_SOURCE_AFTER, || {
        assert_eq!(
            view.accessibility().and_then(|node| node.name).as_deref(),
            Some("From eu1.tilo.coffee")
        );
    });
}
