//! The library's promises: interruption keeps position and velocity,
//! frame rate does not change the curve, a settled motion costs nothing,
//! reduced motion and shortcuts land, and each named motion's shape holds.

use super::*;
use crate::time::{Duration, Instant};
use crate::tokens::{EffectiveMotion, Motion, MotionSource};
use vello::kurbo::{Point, Rect};

fn on() -> EffectiveMotion {
    EffectiveMotion::new(Motion::default(), false)
}

fn reduced() -> EffectiveMotion {
    EffectiveMotion::new(Motion::default(), true)
}

fn ms(start: Instant, ms: u64) -> Instant {
    start + Duration::from_millis(ms)
}

fn base_timing() -> Timing {
    Appear::SPEC.timing(Direction::Enter, on(), MotionSource::Pointer)
}

#[test]
fn a_reversal_continues_from_the_drawn_position_and_velocity() {
    let start = crate::time::now();
    let mut v = MotionValue::new(0.0_f64);
    v.set(100.0, base_timing(), start);
    let flip = ms(start, 60);
    let before = v.get(flip);
    let speed_before = v.velocity(flip);
    assert!(before > 5.0 && before < 95.0, "mid-flight: {before}");
    assert!(speed_before > 0.0);
    v.set(0.0, base_timing(), flip);
    assert!(
        (v.get(flip) - before).abs() < 1e-9,
        "no jump at the reversal"
    );
    assert!(
        (v.velocity(flip) - speed_before).abs() < 1e-6,
        "the velocity carries across the reversal"
    );
    // A frame later it is still moving the old way, braking: no snap back.
    let next = start + Duration::from_micros(60_000 + 8_333);
    assert!(v.get(next) > before);
    assert!(v.velocity(next) < speed_before);
}

#[test]
fn repeated_requests_collapse_into_one_motion() {
    let start = crate::time::now();
    let mut v = MotionValue::new(0.0_f64);
    v.set(1.0, base_timing(), start);
    let settles = v.settles_at();
    for step in 1..10 {
        v.set(1.0, base_timing(), ms(start, step * 5));
    }
    assert_eq!(
        v.settles_at(),
        settles,
        "the same target never restarts the curve"
    );
}

#[test]
fn the_curve_is_the_same_at_any_frame_rate() {
    let start = crate::time::now();
    let mut a = MotionValue::new(Rect::new(0.0, 0.0, 10.0, 10.0));
    a.set(Rect::new(300.0, 40.0, 700.0, 500.0), base_timing(), start);
    // Sampled at 30, 60 and 144 Hz, or once after a stall: what a frame at a
    // given instant shows does not depend on the frames before it.
    let probe = ms(start, 100);
    let direct = a.get(probe);
    for hz in [30_u32, 60, 144] {
        let b = a;
        let frame = Duration::from_secs(1) / hz;
        let mut t = start;
        while t < probe {
            let _ = b.get(t);
            t += frame;
        }
        assert_eq!(b.get(probe), direct, "{hz} Hz");
    }
}

#[test]
fn a_settled_motion_is_exactly_at_rest_and_asks_for_nothing() {
    let start = crate::time::now();
    let mut v = MotionValue::new(Point::new(0.0, 0.0));
    v.set(Point::new(400.0, -120.0), base_timing(), start);
    let settles = v.settles_at().expect("moving");
    assert!(v.is_moving(start));
    assert!(
        settles <= ms(start, 400),
        "a base motion is done well within slow"
    );
    assert!(!v.is_moving(settles));
    assert_eq!(v.get(settles), Point::new(400.0, -120.0), "lands exactly");
    assert_eq!(v.velocity(settles), Point::new(0.0, 0.0));
    let mut rest = MotionValue::new(3.0_f64);
    rest.set(3.0, base_timing(), start);
    assert_eq!(rest.settles_at(), None, "nothing to do costs nothing");
}

#[test]
fn reduced_motion_lands_movement_and_keeps_fades() {
    let now = crate::time::now();
    for spec in [
        PanelSlide::SPEC,
        Disclose::SPEC,
        WindowMotion::MOVE,
        Snap::SPEC,
    ] {
        assert!(
            spec.timing(Direction::Enter, reduced(), MotionSource::Pointer)
                .is_instant(),
            "{spec:?}"
        );
    }
    let mut p = Presence::closed();
    p.show(true, Appear::SPEC, reduced(), MotionSource::Pointer, now);
    let fast = Motion::default().durations.fast();
    let settles = p.settles_at().expect("a popover still fades in");
    assert!(settles <= now + fast + Duration::from_millis(1));
    let mid = p.progress(ms(now, 40));
    let pose = Appear::pose(mid, Point::new(10.0, 0.0), reduced());
    assert_eq!(pose.transform, vello::kurbo::Affine::IDENTITY, "no scale");
    assert!(pose.opacity > 0.0 && pose.opacity < 1.0, "but a fade");
    let drill = Drill::pose(0.4, 360.0, reduced());
    assert_eq!(drill.transform, vello::kurbo::Affine::IDENTITY);
}

#[test]
fn a_shortcut_lands_and_context_moves_at_most_fast() {
    for source in [MotionSource::Keyboard, MotionSource::Action] {
        for spec in [Appear::SPEC, Drill::SPEC, PanelSlide::SPEC, Disclose::SPEC] {
            assert!(spec.timing(Direction::Enter, on(), source).is_instant());
        }
        let Timing::Spring(spring) = Snap::SPEC.timing(Direction::Enter, on(), source) else {
            panic!("a scroll into view is context and still moves");
        };
        let fast = SpringSpec::settling_within(Motion::default().durations.fast());
        assert!(spring.response <= fast.response + 1e-9);
    }
}

#[test]
fn exits_are_quicker_than_entries_and_long_travel_takes_longer() {
    let spring = |t: Timing| match t {
        Timing::Spring(s) => s.response,
        Timing::Instant => 0.0,
    };
    let enter = spring(Appear::SPEC.timing(Direction::Enter, on(), MotionSource::Pointer));
    let exit = spring(Appear::SPEC.timing(Direction::Exit, on(), MotionSource::Pointer));
    assert!(exit < enter);
    let short = spring(Disclose::timing(0.0, 120.0, on(), MotionSource::Pointer));
    let tall = spring(Disclose::timing(0.0, 960.0, on(), MotionSource::Pointer));
    let slow = SpringSpec::settling_within(Motion::default().durations.slow()).response;
    assert!(tall > short && tall <= slow + 1e-9, "{short} {tall} {slow}");
}

#[test]
fn routine_motion_never_overshoots() {
    let start = crate::time::now();
    for spec in [
        Appear::SPEC,
        Drill::SPEC,
        PanelSlide::SPEC,
        Disclose::SPEC,
        WindowMotion::MOVE,
    ] {
        let mut v = MotionValue::new(0.0_f64);
        v.set(
            1.0,
            spec.timing(Direction::Enter, on(), MotionSource::Pointer),
            start,
        );
        for step in 0..600 {
            let x = v.get(ms(start, step));
            assert!(x <= 1.0 + 1e-9, "{spec:?} overshot to {x}");
        }
    }
}

#[test]
fn presence_intent_changes_at_once_while_the_drawing_follows() {
    let now = crate::time::now();
    let mut p = Presence::closed();
    p.show(true, Appear::SPEC, on(), MotionSource::Pointer, now);
    assert!(p.is_open(), "input acts on the open state immediately");
    assert!(p.is_visible(now));
    let opened = ms(now, 80);
    let drawn = p.progress(opened);
    p.show(false, Appear::SPEC, on(), MotionSource::Pointer, opened);
    assert!(!p.is_open());
    assert!(
        p.is_visible(opened),
        "a closing surface still draws its exit"
    );
    assert!((p.progress(opened) - drawn).abs() < 1e-9);
    let gone = p.settles_at().expect("closing");
    assert!(!p.is_visible(gone));
    assert!(!p.is_moving(gone));
}

#[test]
fn appear_scales_about_its_origin_and_leads_with_opacity() {
    let origin = Point::new(300.0, 0.0);
    let pose = Appear::pose(0.5, origin, on());
    assert_eq!(pose.to_drawn(origin), origin, "the origin stays put");
    assert!(
        (pose.opacity - 0.833).abs() < 0.01,
        "opacity leads: {}",
        pose.opacity
    );
    let corner = Point::new(0.0, 400.0);
    let drawn = pose.to_drawn(corner);
    assert!(
        drawn.x > corner.x && drawn.y < corner.y,
        "the rest closes in on the origin"
    );
    assert!(
        (pose.to_local(drawn) - corner).hypot() < 1e-9,
        "hit testing inverts it"
    );
    assert!(Appear::pose(1.0, origin, on()).is_rest());
    assert!(!Appear::pose(0.0, origin, on()).is_visible());
    assert_eq!(
        Appear::origin(
            Rect::new(100.0, 40.0, 500.0, 600.0),
            Point::new(480.0, 12.0)
        ),
        Point::new(480.0, 40.0)
    );
}

#[test]
fn drill_pages_share_one_axis_and_back_is_the_same_curve_reversed() {
    let width = 360.0;
    // At depth 0.5 between root and level 1, both pages are mostly faded
    // and offset in opposite directions.
    let root = Drill::pose(0.0 - 0.5, width, on());
    let page = Drill::pose(1.0 - 0.5, width, on());
    assert!(root.opacity < 0.3 && page.opacity < 0.3);
    assert!(root.to_drawn(Point::ZERO).x < 0.0 && page.to_drawn(Point::ZERO).x > 0.0);
    // The outgoing page is gone by 60 % of the way.
    assert!(!Drill::pose(-0.65, width, on()).is_visible());
    assert!(Drill::pose(0.0, width, on()).is_rest());
    // A narrow panel shifts less.
    assert!(Drill::pose(1.0, 100.0, on()).to_drawn(Point::ZERO).x <= 10.0 + 1e-9);

    let now = crate::time::now();
    let mut depth = DrillDepth::at(0);
    depth.go(1, on(), MotionSource::Pointer, now);
    let mid = ms(now, 50);
    let drawn = depth.get(mid);
    depth.go(0, on(), MotionSource::Pointer, mid);
    assert!(
        (depth.get(mid) - drawn).abs() < 1e-9,
        "back mid-way continues"
    );
    depth.go(2, on(), MotionSource::Keyboard, mid);
    assert_eq!(depth.get(mid), 2.0, "a shortcut lands");
}

#[test]
fn cross_fade_hands_over_without_a_pop() {
    let now = crate::time::now();
    let mut fade = CrossFade::new("clock");
    fade.show("timer", on(), MotionSource::Pointer, now);
    assert_eq!(*fade.shown(), "timer");
    let (old, o) = fade.outgoing(now).expect("the clock is still drawn");
    assert_eq!(*old, "clock");
    assert!((o - 1.0).abs() < 1e-6);
    let mid = ms(now, 40);
    let drawn_timer = fade.incoming(mid);
    fade.show("call", on(), MotionSource::Pointer, mid);
    let (old, o) = fade.outgoing(mid).expect("the timer fades out");
    assert_eq!(*old, "timer");
    assert!((o - drawn_timer).abs() < 1e-6, "from where it was drawn");
    let done = fade.settles_at().expect("moving");
    assert!(fade.outgoing(done).is_none());
    assert_eq!(fade.incoming(done), 1.0);
    let mut keys = CrossFade::new(1);
    keys.show(2, on(), MotionSource::Keyboard, now);
    assert!(keys.outgoing(now).is_none(), "Ctrl+Tab swaps at once");
}

#[test]
fn arriving_data_fades_in_at_fast_even_under_reduced_motion() {
    let now = crate::time::now();
    for motion in [on(), reduced()] {
        let mut fade = CrossFade::new("skeleton");
        fade.arrive("album", motion, now);
        assert_eq!(*fade.shown(), "album");
        assert!(fade.outgoing(now).is_some(), "the skeleton is still drawn");
        let done = fade.settles_at().expect("it fades");
        assert!(done <= now + Motion::default().durations.fast() + Duration::from_millis(1));
        assert_eq!(fade.incoming(done), 1.0);
    }
}

#[test]
fn feedback_comes_at_once_and_goes_over_fast() {
    let now = crate::time::now();
    let mut f = Feedback::default();
    f.set(true, on(), now);
    assert_eq!(f.level(now), 1.0);
    assert!(!f.is_moving(now));
    f.set(false, on(), now);
    assert!(f.level(ms(now, 30)) > 0.0);
    let gone = f.settles_at().expect("fading");
    assert!(gone <= now + Motion::default().durations.fast() + Duration::from_millis(1));
}

#[test]
fn attention_swings_and_rests_or_flashes_under_reduced_motion() {
    let now = crate::time::now();
    let mut a = Attention::default();
    a.refuse(on(), now);
    let peak = (0..120)
        .map(|t| a.offset(ms(now, t)).x)
        .fold(0.0_f64, f64::max);
    assert!((peak - 8.0).abs() < 0.5, "shakes 8 px: {peak}");
    let rest = a.settles_at().expect("moving");
    assert!(rest <= ms(now, 900));
    assert_eq!(a.offset(rest).x, 0.0);

    let mut b = Attention::default();
    b.refuse(reduced(), now);
    assert_eq!(b.offset(ms(now, 20)).x, 0.0, "no movement");
    assert!(b.tint(ms(now, 20)) > 0.5, "a flash instead");

    let mut c = Attention::default();
    c.pulse(on(), now);
    let top = (0..200)
        .map(|t| c.scale(ms(now, t)))
        .fold(1.0_f64, f64::max);
    assert!((top - 1.06).abs() < 0.005, "pulses 6 %: {top}");
}

#[test]
fn extents_follow_without_growing_on_first_paint() {
    let now = crate::time::now();
    let mut e = Extent::default();
    e.follow(300.0, on(), MotionSource::Pointer, now);
    assert_eq!(e.get(now), 300.0, "first paint is at rest");
    e.follow(180.0, on(), MotionSource::Pointer, now);
    assert!(e.is_moving(now));
    assert_eq!(e.get(e.settles_at().expect("moving")), 180.0);
}

#[test]
fn zoom_fits_one_rect_into_another() {
    let rest = Rect::new(0.0, 0.0, 800.0, 600.0);
    let card = Rect::new(100.0, 50.0, 300.0, 200.0);
    let fit = Zoom::fit(rest, card);
    assert_eq!(fit * Point::new(0.0, 0.0), Point::new(100.0, 50.0));
    assert_eq!(fit * Point::new(800.0, 600.0), Point::new(300.0, 200.0));
    assert_eq!(Zoom::contents(0.0), (1.0, 0.0));
    assert_eq!(Zoom::contents(1.0), (0.0, 1.0));
}

#[test]
fn the_overview_zoom_stays_spatial_context_when_a_key_opens_it() {
    let fast = Motion::default().durations.fast();
    let slow = Motion::default().durations.slow();
    let enter = |motion, source| Zoom::OVERVIEW.settle_time(Direction::Enter, motion, source);
    // Super+Tab: the windows still fly to their places, at `fast` at most.
    let keyed = enter(on(), MotionSource::Keyboard);
    assert!(keyed > Duration::ZERO, "a key still shows where they went");
    assert!(keyed <= fast + Duration::from_millis(1), "{keyed:?}");
    // A click on "Overview" is a whole-view change.
    assert!(enter(on(), MotionSource::Pointer) <= slow + Duration::from_millis(1));
    assert!(enter(on(), MotionSource::Pointer) > keyed);
    // Reduced motion: a key lands, a click fades at `fast`.
    assert_eq!(enter(reduced(), MotionSource::Keyboard), Duration::ZERO);
    assert!(enter(reduced(), MotionSource::Pointer) <= fast + Duration::from_millis(1));
    // The shared element's own rule is unchanged: a shortcut lands.
    assert_eq!(
        Zoom::SPEC.settle_time(Direction::Enter, on(), MotionSource::Keyboard),
        Duration::ZERO
    );
    assert_eq!(Zoom::radius_at(14.0, 8.0, 0.0), 14.0);
    assert_eq!(Zoom::radius_at(14.0, 8.0, 1.0), 8.0);
}

#[test]
fn panel_content_reflows_once_at_the_wide_end() {
    assert_eq!(PanelSlide::content_width(700.0, 940.0, true), 940.0);
    assert_eq!(PanelSlide::content_width(700.0, 940.0, false), 700.0);
    assert_eq!(PanelSlide::shown(0.5, 240.0), 120.0);
}

#[test]
fn snap_projects_the_fling_to_the_nearest_detent() {
    let rest = Snap::rest_for(100.0, 1000.0, on());
    assert!(rest > 100.0);
    assert_eq!(Snap::nearest([0.0, 200.0, 400.0], rest), Some(200.0));
}

#[test]
fn ambient_chrome_leaves_at_once_and_arrives_gently_unless_asked() {
    let now = crate::time::now();
    let mut unasked = Presence::closed();
    unasked.show(true, Ambient::spec(false), on(), MotionSource::Pointer, now);
    let mut asked = Presence::closed();
    asked.show(true, Ambient::spec(true), on(), MotionSource::Pointer, now);
    let probe = ms(now, 60);
    assert!(
        asked.progress(probe) > unasked.progress(probe),
        "asked is quicker"
    );
    assert!(unasked.settles_at() > asked.settles_at());
    asked.show(
        false,
        Ambient::spec(true),
        on(),
        MotionSource::Pointer,
        probe,
    );
    assert!(!asked.is_visible(probe), "leaving is instant");
}

/// Every routine standard motion, for the rules they all keep.
fn routine_specs() -> [MotionSpec; 13] {
    [
        ControlChange::MOVE,
        ControlChange::SHOW,
        Appear::SPEC,
        Drill::SPEC,
        Drill::WORKSPACE,
        Disclose::SPEC,
        PanelSlide::SPEC,
        CROSS_FADE,
        ListChange::MOVE,
        ListChange::FADE,
        Zoom::SPEC,
        Zoom::OVERVIEW,
        WindowMotion::MOVE,
    ]
}

#[test]
fn the_standard_motions_are_one_library_every_surface_composes_from() {
    let now = crate::time::now();
    // One engine: every standard motion moves a value that a reversal
    // continues from where it is drawn, and that comes to rest exactly.
    for spec in routine_specs() {
        let mut p = Presence::closed();
        p.show(true, spec, on(), MotionSource::Pointer, now);
        let mid = ms(now, 40);
        let drawn = p.progress(mid);
        p.show(false, spec, on(), MotionSource::Pointer, mid);
        assert!((p.progress(mid) - drawn).abs() < 1e-9, "{spec:?} jumped");
        let rest = p.settles_at().expect("moving");
        assert_eq!(p.progress(rest), 0.0, "{spec:?} lands exactly");
        assert!(!p.is_moving(rest), "{spec:?} then costs nothing");
    }
}

#[test]
fn standard_motion_never_makes_tilo_annoying() {
    let now = crate::time::now();
    let slow = Motion::default().durations.slow();
    for spec in routine_specs() {
        // Never bouncy: routine motion does not overshoot.
        let mut v = MotionValue::new(0.0_f64);
        v.set(
            1.0,
            spec.timing(Direction::Enter, on(), MotionSource::Pointer),
            now,
        );
        assert!(
            (0..800).all(|t| v.get(ms(now, t)) <= 1.0 + 1e-9),
            "{spec:?} bounces"
        );
        // Short: nothing routine takes longer than `slow`.
        let enter = spec.settle_time(Direction::Enter, on(), MotionSource::Pointer);
        assert!(
            enter <= slow + Duration::from_millis(1),
            "{spec:?} takes {enter:?}"
        );
        // A key is never slower than a click.
        let keyed = spec.settle_time(Direction::Enter, on(), MotionSource::Keyboard);
        assert!(
            keyed <= enter,
            "{spec:?}: keyboard {keyed:?} vs pointer {enter:?}"
        );
        // Leaving is never slower than arriving.
        let exit = spec.settle_time(Direction::Exit, on(), MotionSource::Pointer);
        assert!(exit <= enter, "{spec:?}: exit {exit:?} vs enter {enter:?}");
        // Reduced motion is no slower either.
        let reduced = spec.settle_time(Direction::Enter, self::reduced(), MotionSource::Pointer);
        assert!(reduced <= enter, "{spec:?}: reduced {reduced:?}");
    }
    // Repeated toggles collapse into one motion: no queue, and the state
    // asked for last is the one input acts on.
    let mut p = Presence::closed();
    for i in 0..10_u64 {
        p.show(
            i % 2 == 0,
            Appear::SPEC,
            on(),
            MotionSource::Pointer,
            ms(now, i * 5),
        );
    }
    assert!(!p.is_open());
    let settled = p.settles_at().expect("still moving");
    assert!(
        settled <= ms(now, 45) + slow,
        "ten toggles are not ten animations"
    );
}

#[test]
fn a_transient_arrives_holds_and_is_gone_exactly_when_it_expires() {
    let shown = crate::time::now();
    let span = (shown, ms(shown, 1500));
    let readout = Transient::READOUT;
    assert_eq!(Transient::opacity(readout, span, shown, on()), 0.0);
    assert!(Transient::is_moving(readout, span, shown, on()));
    let holding = ms(shown, 700);
    assert_eq!(Transient::opacity(readout, span, holding, on()), 1.0);
    assert!(
        !Transient::is_moving(readout, span, holding, on()),
        "the hold costs nothing"
    );
    let fading = ms(shown, 1450);
    let o = Transient::opacity(readout, span, fading, on());
    assert!(o > 0.0 && o < 1.0, "{o}");
    assert_eq!(Transient::opacity(readout, span, span.1, on()), 0.0);
    // A confirmation is there at once.
    assert_eq!(
        Transient::opacity(Transient::CONFIRMATION, span, shown, on()),
        1.0
    );
    // Reduced motion: whole until it goes.
    assert_eq!(Transient::opacity(readout, span, shown, reduced()), 1.0);
    assert_eq!(Transient::opacity(readout, span, fading, reduced()), 1.0);
    assert!(!Transient::is_moving(readout, span, fading, reduced()));
}

#[test]
fn a_token_motion_run_once_takes_exactly_its_token() {
    let d = Motion::default().durations;
    let spec = MotionSpec {
        enter: Pace::Fast,
        exit: Pace::Base,
        reduced: Reduced::Lands,
        shortcut: Shortcut::Lands,
    };
    assert_eq!(
        spec.settle_time(Direction::Enter, on(), MotionSource::Pointer),
        d.fast()
    );
    assert_eq!(
        spec.settle_time(Direction::Exit, on(), MotionSource::Pointer),
        d.base()
    );
    let before = spec.progress_after(
        Direction::Enter,
        on(),
        MotionSource::Pointer,
        d.fast() - Duration::from_millis(1),
    );
    assert!(before > 0.99 && before < 1.0, "{before}");
    assert_eq!(
        spec.progress_after(Direction::Enter, on(), MotionSource::Pointer, d.fast()),
        1.0
    );
}

#[test]
fn a_deferred_motion_resumes_where_it_stood() {
    let now = crate::time::now();
    let mut v = MotionValue::new(0.0_f64);
    v.set(1.0, base_timing(), now);
    let at = ms(now, 40);
    let drawn = v.get(at);
    v.defer(Duration::from_millis(100));
    assert!(
        (v.get(ms(now, 140)) - drawn).abs() < 1e-9,
        "the wait was not progress"
    );
}

// ---------------------------------------------------------------------------
// Appearing, the motion clock and the input a change follows
// ---------------------------------------------------------------------------

#[test]
fn an_appearing_surface_follows_the_input_that_asked() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut clicked = Appearing::closed();
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        clicked.follow(true, on());
        assert!(clicked.is_open(), "the intent changes at once");
        assert!(clicked.is_moving(), "a click brings it in");
        assert!(clicked.progress() < 0.01);
    });
    let entered = Appear::SPEC.settle_time(Direction::Enter, on(), MotionSource::Pointer);
    // Settle times are whole milliseconds; a millisecond past is at rest.
    with_monotonic_time(start + entered + Duration::from_millis(1), || {
        assert!(!clicked.is_moving(), "it arrives after `base`");
        assert!(
            clicked
                .pose(Rect::new(0.0, 0.0, 100.0, 100.0), Point::ZERO)
                .is_rest()
        );
    });

    let mut typed = Appearing::closed();
    with_monotonic_time(start, || {
        note_source(MotionSource::Keyboard);
        typed.follow(true, on());
        assert!(!typed.is_moving(), "a key lands it at once");
        assert!(typed.progress() >= 1.0);
        typed.follow(false, on());
        assert!(!typed.is_drawn(), "and closes it at once");
    });
}

#[test]
fn a_cursor_merely_moving_asks_for_nothing() {
    note_source(MotionSource::Keyboard);
    note_input(&crate::synthetic_input::cursor_moved(10.0, 10.0));
    assert_eq!(input_source(), MotionSource::Keyboard);
}

#[test]
fn an_appearing_surface_scales_about_the_point_nearest_its_control() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let surface = Rect::new(100.0, 60.0, 300.0, 260.0);
    // A button above the surface's leading half.
    let button = Point::new(140.0, 40.0);
    let mut menu = Appearing::closed();
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        menu.follow(true, on());
    });
    with_monotonic_time(ms(start, 20), || {
        let pose = menu.pose(surface, button);
        let origin = Point::new(140.0, 60.0);
        assert!(
            (pose.to_drawn(origin) - origin).hypot() < 1e-9,
            "the edge under the button stays put"
        );
        let far = Point::new(300.0, 260.0);
        assert!(
            (pose.to_drawn(far) - far).hypot() > 0.5,
            "the far corner grows out of it"
        );
        assert!(pose.opacity > 0.0 && pose.opacity < 1.0);
    });
}

#[test]
fn a_leaving_surface_is_drawn_until_it_has_gone_and_exits_are_shorter() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut popover = Appearing::opened();
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        popover.follow(false, on());
        assert!(!popover.is_open());
        assert!(popover.is_drawn(), "it is still drawn as it leaves");
    });
    let exit = Appear::SPEC.settle_time(Direction::Exit, on(), MotionSource::Pointer);
    let enter = Appear::SPEC.settle_time(Direction::Enter, on(), MotionSource::Pointer);
    assert!(exit < enter, "exits are shorter: {exit:?} vs {enter:?}");
    with_monotonic_time(start + exit + Duration::from_millis(1), || {
        assert!(!popover.is_drawn(), "gone once the exit settles");
    });
}

#[test]
fn reduced_motion_appears_by_fading_in_place() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut sheet = Appearing::closed();
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        sheet.follow(true, reduced());
    });
    with_monotonic_time(ms(start, 40), || {
        let pose = sheet.pose(Rect::new(0.0, 0.0, 200.0, 100.0), Point::new(100.0, 50.0));
        assert_eq!(
            pose.transform,
            vello::kurbo::Affine::IDENTITY,
            "nothing scales"
        );
        assert!(pose.opacity > 0.0 && pose.opacity < 1.0, "it fades");
    });
    let fast = Appear::SPEC.settle_time(Direction::Enter, reduced(), MotionSource::Pointer);
    with_monotonic_time(start + fast + Duration::from_millis(1), || {
        assert!(!sheet.is_moving(), "at `fast`");
    });
}

#[test]
fn a_started_motion_asks_for_frames_until_it_settles_and_no_longer() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut menu = Appearing::closed();
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        menu.follow(true, on());
    });
    assert!(frames_wanted(start), "the arrival asks for frames");
    let until = frames_until().expect("a motion is moving");
    assert!(frames_wanted(until - Duration::from_millis(1)));
    frame_drawn(until);
    assert!(!frames_wanted(until), "a settled motion asks for nothing");
    assert!(frames_until().is_none(), "and holds nothing");
}

/// The last frame of a motion is the one showing it at rest. A frame drawn
/// a hair before the motion ended leaves that hair on screen, so the clock
/// keeps asking until a frame was drawn at or after the end, and then stops.
#[test]
fn the_frame_at_rest_is_owed_until_one_is_drawn_after_the_motion_ends() {
    let start = crate::time::now();
    let mut v = MotionValue::new(0.0_f64);
    v.set(100.0, base_timing(), start);
    let until = v.settles_at().expect("it moves");
    let just_before = until - Duration::from_millis(1);
    frame_drawn(just_before);
    let after = until + Duration::from_millis(5);
    assert!(
        frames_wanted(after),
        "the last frame drawn showed it short of rest: one more is owed"
    );
    frame_drawn(after);
    assert!(
        !frames_wanted(after + Duration::from_millis(1)),
        "drawn at rest, nothing more is asked"
    );
}

/// Every motion asks for its frames, whoever started it and whoever draws
/// it: a host only has to read the clock to paint every motion it shows.
#[test]
fn any_motion_value_started_asks_the_clock_for_its_frames() {
    let start = crate::time::now();
    let mut v = MotionValue::new(Point::ZERO);
    assert!(!frames_wanted(start), "nothing moving, nothing asked");
    v.set(Point::new(40.0, 0.0), base_timing(), start);
    assert_eq!(frames_until(), v.settles_at(), "asks until it settles");
    assert!(frames_wanted(start));
    let mut idle = MotionValue::new(1.0_f64);
    idle.set(1.0, base_timing(), start);
    assert_eq!(
        frames_until(),
        v.settles_at(),
        "a value already where it is going asks nothing more"
    );
}

/// A motion jumped to its end mid-way (a shortcut landing what it changed,
/// an owner letting it go) takes back its wish: the clock asks for no frame
/// it would never draw, while another motion still moving keeps its own.
#[test]
fn a_motion_landed_mid_way_asks_for_no_more_frames() {
    let start = crate::time::now();
    let mut landed = MotionValue::new(0.0_f64);
    let mut moving = MotionValue::new(0.0_f64);
    landed.set(100.0, base_timing(), start);
    moving.set(10.0, base_timing(), start);
    assert!(frames_wanted(start));
    landed.snap(100.0);
    assert_eq!(
        frames_until(),
        moving.settles_at(),
        "only the motion still moving asks"
    );
    moving.snap(10.0);
    assert!(
        !frames_wanted(start),
        "both landed: no frame, not even one at rest, is owed by the clock"
    );
    // A retarget replaces the motion's wish rather than adding one.
    let mut retargeted = MotionValue::new(0.0_f64);
    retargeted.set(100.0, base_timing(), start);
    retargeted.set(1.0, base_timing(), start);
    assert_eq!(frames_until(), retargeted.settles_at());
}

/// A motion that asks the clock for frames is named while it moves: the
/// place that started it, so a frame nobody can see in the widget tree
/// (a menu an owner draws in its own pass) is still attributed.
#[test]
fn a_motion_asking_for_frames_is_named_while_it_moves() {
    use crate::time::testing::with_monotonic_time;
    let start = crate::time::now();
    let mut menu = Appearing::closed();
    let line = line!() + 3;
    with_monotonic_time(start, || {
        note_source(MotionSource::Pointer);
        menu.follow(true, on());
    });
    let owners = frames_wanted_by(start);
    assert!(
        owners
            .iter()
            .any(|owner| owner.ends_with(&format!("tests.rs:{line}"))),
        "the menu's opening names where it started: {owners:?}"
    );
    let until = frames_until().expect("a motion is moving");
    frame_drawn(until);
    assert!(
        frames_wanted_by(until).is_empty(),
        "a settled motion drawn at rest is named by nobody"
    );
    assert!(!frames_wanted(until));
}

#[test]
fn the_settled_clock_reads_every_motion_at_its_end() {
    let start = crate::time::now();
    let mut v = MotionValue::new(0.0_f64);
    v.set(100.0, base_timing(), start);
    assert!(v.is_moving(ms(start, 10)));
    {
        let _settled = testing::settled();
        assert!(testing::is_settled());
        assert!(
            !v.is_moving(ms(start, 10)),
            "moving motions read as arrived"
        );
        assert!((v.get(ms(start, 10)) - 100.0).abs() < f64::EPSILON);
        assert!(!frames_wanted(start), "and ask for no frames");
    }
    assert!(
        !testing::is_settled(),
        "dropping the guard restores the clock"
    );
    assert!(v.is_moving(ms(start, 10)));
}

#[test]
fn a_speaking_ring_comes_with_the_voice_and_eases_out_in_the_pause() {
    let now = crate::time::now();
    let mut ring = Speaking::default();
    ring.set(true, on(), now);
    assert_eq!(ring.level(now), 1.0, "there the moment they are heard");
    ring.set(false, on(), now);
    let rest = ring.settles_at().expect("eases out");
    assert!(
        ring.level(ms(now, 20)) > 0.0,
        "a pause fades, it does not cut"
    );
    assert_eq!(ring.level(rest), 0.0);
    assert!(!ring.is_moving(rest), "a quiet face costs nothing");
    assert!(
        rest <= now + Motion::default().durations.fast() + Duration::from_millis(10),
        "it eases out within fast"
    );

    let mut ring = Speaking::default();
    ring.set(true, reduced(), now);
    ring.set(false, reduced(), now);
    assert_eq!(ring.level(now), 0.0, "reduced motion: gone at once");
    assert!(!ring.is_moving(now));
}

/// Every layout pass of one frame reads a motion at the same instant, so a
/// widget sized by a motion (a disclosure opening) measures the same in each
/// pass and the frame's composition settles. Read at the clock, a slow pass
/// saw the motion further on each time and resizing during it failed with
/// `CompositionDidNotSettle`.
#[test]
fn a_frame_reads_its_motions_at_one_instant_across_its_layout_passes() {
    let start = crate::time::now();
    let mut seen = Vec::new();
    let mut passes = 0;
    let settled = crate::widget::layout::settle_widget_composition(true, |_| {
        passes += 1;
        let at = frame_now();
        seen.push(at);
        let grown = at.saturating_duration_since(start).as_secs_f32();
        std::thread::sleep(std::time::Duration::from_millis(2));
        Ok((
            Some(vec![(
                0,
                taffy::Point { x: 0.0, y: 0.0 },
                taffy::Size {
                    width: 1.0,
                    height: grown,
                },
            )]),
            passes == 1,
        ))
    });
    assert!(settled.is_ok(), "{settled:?}");
    assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    assert!(
        frame_now() >= seen[0],
        "outside the frame the clock runs on"
    );
}

#[test]
fn a_controls_thumb_retargets_mid_flight_from_where_it_is_drawn() {
    let now = crate::time::now();
    let mut thumb = Thumb::at(0.0);
    thumb.go(1.0, on(), MotionSource::Pointer, now);
    let mid = ms(now, 40);
    let drawn = thumb.get(mid);
    assert!(drawn > 0.0 && drawn < 1.0, "on its way: {drawn}");
    // A second click mid-flight reverses from the drawn place, no jump.
    thumb.go(0.0, on(), MotionSource::Pointer, mid);
    assert!((thumb.get(mid) - drawn).abs() < 1e-9, "the reversal jumped");
    assert_eq!(thumb.target(), 0.0);
    let rest = thumb.settles_at().expect("moving");
    assert_eq!(thumb.get(rest), 0.0, "it lands exactly");
    assert!(!thumb.is_moving(rest));
}

#[test]
fn a_key_or_an_action_lands_a_control_at_once() {
    let now = crate::time::now();
    for source in [MotionSource::Keyboard, MotionSource::Action] {
        let mut thumb = Thumb::at(0.0);
        thumb.go(1.0, on(), source, now);
        assert_eq!(thumb.get(now), 1.0, "{source:?}");
        assert!(!thumb.is_moving(now), "{source:?}");
        let mut mark = Mark::at(false);
        mark.set(true, on(), source, now);
        assert_eq!(mark.level(now), 1.0, "{source:?}");
        assert!(!mark.is_moving(now), "{source:?}");
    }
}

#[test]
fn reduced_motion_lands_a_controls_position_and_fades_what_it_shows() {
    let now = crate::time::now();
    let mut thumb = Thumb::at(0.0);
    thumb.go(1.0, reduced(), MotionSource::Pointer, now);
    assert_eq!(thumb.get(now), 1.0, "the knob lands");
    assert!(!thumb.is_moving(now));
    let mut mark = Mark::at(false);
    mark.set(true, reduced(), MotionSource::Pointer, now);
    let fast = Motion::default().durations.fast();
    let level = mark.level(ms(now, 40));
    assert!(level > 0.0 && level < 1.0, "the tint fades: {level}");
    let rest = mark.settles_at().expect("fading");
    assert!(rest <= now + fast + Duration::from_millis(1));
}

#[test]
fn a_settled_control_wants_no_frames() {
    testing_clean_frames();
    let now = crate::time::now();
    let mut thumb = Thumb::at(1.0);
    let mut mark = Mark::at(true);
    // Asking for where it already is starts nothing.
    thumb.go(1.0, on(), MotionSource::Pointer, now);
    mark.set(true, on(), MotionSource::Pointer, now);
    assert!(thumb.settles_at().is_none() && mark.settles_at().is_none());
    assert!(!frames_wanted(now), "a settled control asks for nothing");
    // A click asks for frames until it settles, and not one after.
    thumb.go(0.0, on(), MotionSource::Pointer, now);
    mark.set(false, on(), MotionSource::Pointer, now);
    let rest = thumb
        .settles_at()
        .into_iter()
        .chain(mark.settles_at())
        .max()
        .expect("moving");
    assert!(frames_wanted(ms(now, 10)));
    assert!(frames_wanted(rest), "the frame showing it at rest is owed");
    frame_drawn(rest);
    assert!(!frames_wanted(rest), "and once drawn, not one after");
    assert!(!thumb.is_moving(rest) && !mark.is_moving(rest));
}

/// The frame wish of an earlier test on this thread is cleared.
fn testing_clean_frames() {
    super::clock::forget_frames_for_tests();
}

#[test]
fn a_held_thumb_is_where_the_hand_is_and_settles_with_its_velocity() {
    let now = crate::time::now();
    let mut thumb = Thumb::at(0.0);
    // A click sends it on its way; the hand takes it mid-flight.
    thumb.go(1.0, on(), MotionSource::Pointer, now);
    for (i, hand) in [0.3, 0.42, 0.55, 0.61].into_iter().enumerate() {
        thumb.track(hand);
        let at = ms(now, 20 + 8 * u64::try_from(i).unwrap());
        assert_eq!(thumb.get(at), hand, "1:1 with the hand");
        assert!(!thumb.is_moving(at), "nothing runs while it is held");
    }
    // Let go moving right: it carries on toward the side it was thrown.
    let released = ms(now, 60);
    thumb.release(1.0, 4.0, on(), released);
    let just_after = thumb.get(released + Duration::from_millis(8));
    assert!(
        just_after > 0.61,
        "the release velocity carries: {just_after}"
    );
    let rest = thumb.settles_at().expect("settling");
    assert_eq!(thumb.get(rest), 1.0);
    // Never drawn past the end of its travel, even thrown hard.
    let mut thrown = Thumb::at(0.9);
    thrown.release(1.0, 40.0, on(), now);
    assert!((0..200).all(|t| thrown.get(ms(now, t)) <= 1.0));
    // Reduced motion: the release lands.
    let mut calm = Thumb::at(0.4);
    calm.release(1.0, 4.0, reduced(), now);
    assert_eq!(calm.get(now), 1.0);
}

#[test]
fn a_controls_mark_goes_quicker_than_it_comes() {
    let now = crate::time::now();
    let mut mark = Mark::at(false);
    mark.set(true, on(), MotionSource::Pointer, now);
    let came = mark.settles_at().expect("coming") - now;
    let mut leaving = Mark::at(true);
    leaving.set(false, on(), MotionSource::Pointer, now);
    let went = leaving.settles_at().expect("going") - now;
    assert!(went < came, "out {went:?} vs in {came:?}");
    // Unchecked mid-way, it goes from what is drawn.
    let mid = ms(now, 40);
    let drawn = mark.level(mid);
    mark.set(false, on(), MotionSource::Pointer, mid);
    assert!((mark.level(mid) - drawn).abs() < 1e-6);
    assert!(!mark.is_on());
}

// ---------------------------------------------------------------------------
// Scroll to what was asked for
// ---------------------------------------------------------------------------

fn down(y: f64) -> vello::kurbo::Vec2 {
    vello::kurbo::Vec2::new(0.0, y)
}

/// A view 600 px tall.
const VIEW: vello::kurbo::Vec2 = vello::kurbo::Vec2::new(800.0, 600.0);

#[test]
fn a_short_jump_glides_at_fast_and_then_asks_for_no_frames() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    let jump = to.go_from(
        down(0.0),
        down(900.0),
        VIEW,
        on(),
        MotionSource::Pointer,
        start,
    );
    assert_eq!(jump, ScrollJump::Glide);
    assert_eq!(
        to.offset(start),
        Some(down(0.0)),
        "it starts where it is drawn"
    );
    let mid = to.offset(ms(start, 40)).expect("gliding");
    assert!(mid.y > 100.0 && mid.y < 900.0, "on its way: {mid:?}");
    assert_eq!(to.opacity(ms(start, 40)), 1.0, "a glide does not fade");
    let fast = ScrollTo::SPEC.settle_time(Direction::Enter, on(), MotionSource::Pointer);
    assert_eq!(fast, Motion::default().durations.fast());
    // `fast` brings a spring within a thousandth of the jump; the glide
    // goes on to within a quarter pixel, a whole device pixel at twice the
    // density, which a long jump reaches a little later.
    let rest = to.settles_at().expect("moving");
    assert!(rest >= start + fast.mul_f64(0.9), "not sooner than fast");
    assert!(
        rest <= start + fast.mul_f64(1.5),
        "about fast: {:?}",
        rest - start
    );
    assert!(!to.is_moving(rest), "arrived");
    assert_eq!(to.arrived(rest), Some(down(900.0)));
    assert_eq!(
        to.offset(rest),
        None,
        "at rest the scroller's offset is the target"
    );
    assert!(!to.is_moving(ms(start, 1000)));
    assert_eq!(to.settles_at(), None, "a settled jump holds no deadline");
}

#[test]
fn a_glide_steps_in_whole_device_pixels_and_lands_exactly() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    to.go_from(
        down(3.0),
        down(410.5),
        VIEW,
        on(),
        MotionSource::Pointer,
        start,
    );
    for frame in 1..20 {
        let at = start + Duration::from_nanos(frame * 8_333_333);
        let drawn = to.offset_in_device_pixels(at, 2.0).expect("gliding");
        assert_eq!((drawn.y * 2.0).fract(), 0.0, "frame {frame}: {drawn:?}");
    }
    let rest = to.settles_at().expect("moving");
    assert_eq!(to.offset_in_device_pixels(rest, 2.0), Some(down(410.5)));
}

#[test]
fn a_second_jump_retargets_from_the_drawn_offset_and_speed() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    to.go_from(
        down(0.0),
        down(600.0),
        VIEW,
        on(),
        MotionSource::Keyboard,
        start,
    );
    let next = ms(start, 30);
    let drawn = to.offset(next).expect("gliding");
    // Enter pressed again in the find field: the next hit, further down.
    let jump = to.go_from(
        down(0.0),
        down(1200.0),
        VIEW,
        on(),
        MotionSource::Keyboard,
        next,
    );
    assert_eq!(jump, ScrollJump::Glide, "measured from where it is drawn");
    let after = to.offset(next).expect("gliding");
    assert!((after - drawn).hypot() < 1e-6, "no jump at the retarget");
    assert!(
        to.offset(ms(start, 40)).expect("gliding").y > drawn.y,
        "it keeps going"
    );
    assert_eq!(to.target(), Some(down(1200.0)));
}

#[test]
fn the_hand_takes_over_from_where_the_content_is_drawn() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    to.go_from(
        down(0.0),
        down(800.0),
        VIEW,
        on(),
        MotionSource::Pointer,
        start,
    );
    let wheel = ms(start, 35);
    let drawn = to.offset(wheel).expect("gliding");
    assert_eq!(to.take_over(wheel), Some(drawn));
    assert!(!to.is_moving(wheel), "the wheel stops it at once");
    assert_eq!(
        to.offset(ms(start, 60)),
        None,
        "and it never moves the content again"
    );
    assert_eq!(to.settles_at(), None);
    assert_eq!(to.take_over(wheel), None, "nothing to take over at rest");
}

#[test]
fn a_long_jump_fades_in_at_the_target_instead_of_scrolling_through() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    let far = down(VIEW.y * ScrollTo::GLIDE_VIEWPORTS + 1.0);
    let jump = to.go_from(down(0.0), far, VIEW, on(), MotionSource::Pointer, start);
    assert_eq!(jump, ScrollJump::FadeIn);
    assert_eq!(
        to.offset(start),
        None,
        "the content is at the target at once"
    );
    assert!(to.opacity(start) < 0.05, "and fades in there");
    assert!(to.is_moving(ms(start, 40)));
    let rest = to.settles_at().expect("fading");
    assert_eq!(to.opacity(rest), 1.0);
    assert!(!to.is_moving(rest));
    // Exactly three viewports still glides.
    let mut near = ScrollTo::new();
    let three = down(VIEW.y * ScrollTo::GLIDE_VIEWPORTS);
    assert_eq!(
        near.go_from(down(0.0), three, VIEW, on(), MotionSource::Pointer, start),
        ScrollJump::Glide
    );
}

#[test]
fn reduced_motion_lands_and_a_long_jump_still_fades() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    let jump = to.go_from(
        down(0.0),
        down(500.0),
        VIEW,
        reduced(),
        MotionSource::Pointer,
        start,
    );
    assert_eq!(jump, ScrollJump::Land);
    assert_eq!(to.offset(start), None);
    assert!(!to.is_moving(start));
    let jump = to.go_from(
        down(500.0),
        down(9000.0),
        VIEW,
        reduced(),
        MotionSource::Pointer,
        start,
    );
    assert_eq!(jump, ScrollJump::FadeIn, "what is shown changed: it fades");
    assert!(to.opacity(start) < 0.05);
}

#[test]
fn focus_walking_by_key_and_a_reveal_nobody_asked_for_land() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    // Tab moving focus off screen lands it.
    to.land(down(300.0));
    assert!(!to.is_moving(start));
    assert_eq!(to.offset(start), None);
    // An agent's action lands.
    crate::time::testing::with_monotonic_time(start, || {
        note_source(MotionSource::Action);
        assert_eq!(
            to.go(down(0.0), down(300.0), VIEW, on(), start),
            ScrollJump::Land
        );
    });
    // A reveal long after the last input (a sync) lands.
    crate::time::testing::with_monotonic_time(start, || note_source(MotionSource::Pointer));
    let later = ms(start, 2_000);
    crate::time::testing::with_monotonic_time(later, || {
        assert_eq!(
            to.go(down(0.0), down(300.0), VIEW, on(), later),
            ScrollJump::Land
        );
    });
    // A click just now glides.
    crate::time::testing::with_monotonic_time(later, || {
        note_source(MotionSource::Pointer);
        assert_eq!(
            to.go(down(0.0), down(300.0), VIEW, on(), later),
            ScrollJump::Glide
        );
    });
}

#[test]
fn the_settled_clock_shows_a_scroll_jump_arrived() {
    let start = crate::time::now();
    let mut to = ScrollTo::new();
    to.go_from(
        down(0.0),
        down(400.0),
        VIEW,
        on(),
        MotionSource::Pointer,
        start,
    );
    let _settled = testing::settled();
    assert!(!to.is_moving(start));
    assert_eq!(to.offset(start), Some(down(400.0)));
}

#[test]
fn a_workspace_switch_by_pointer_is_whole_view_motion_and_a_shortcut_lands() {
    let response = |t: Timing| match t {
        Timing::Spring(s) => s.response,
        Timing::Instant => 0.0,
    };
    let slow = SpringSpec::settling_within(Motion::default().durations.slow()).response;
    let pointer = response(Drill::WORKSPACE.timing(Direction::Enter, on(), MotionSource::Pointer));
    assert!(
        (pointer - slow).abs() < 1e-9,
        "toward a later workspace: slow"
    );
    let back = response(Drill::WORKSPACE.timing(Direction::Exit, on(), MotionSource::Pointer));
    assert!(back > 0.0 && back < pointer, "back is quicker");
    for source in [MotionSource::Keyboard, MotionSource::Action] {
        assert!(
            Drill::WORKSPACE
                .timing(Direction::Enter, on(), source)
                .is_instant()
        );
    }
    let fast = SpringSpec::settling_within(Motion::default().durations.fast()).response;
    let reduced_fade =
        response(Drill::WORKSPACE.timing(Direction::Enter, reduced(), MotionSource::Pointer));
    assert!(
        (reduced_fade - fast).abs() < 1e-9,
        "reduced: a fast cross-fade"
    );
}
