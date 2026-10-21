use super::*;
use crate::tokens::Motion;

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn reduced() -> EffectiveMotion {
    EffectiveMotion::new(Motion::default(), true)
}

fn fill(line: &LoadingLine, now: Instant) -> Option<f64> {
    match line.visual(now)? {
        LoadingVisual::Bar { fill, .. } => Some(fill),
        _ => None,
    }
}

fn opacity(line: &LoadingLine, now: Instant) -> Option<f32> {
    Some(match line.visual(now)? {
        LoadingVisual::Bar { opacity, .. }
        | LoadingVisual::Sweep { opacity, .. }
        | LoadingVisual::Pulse { opacity } => opacity,
    })
}

#[test]
fn what_an_app_reports_is_bounded() {
    assert_eq!(WindowLoading::progress(f32::NAN), WindowLoading::Busy);
    assert_eq!(
        WindowLoading::progress(1.5),
        WindowLoading::Progress { fraction: 1.0 }
    );
    assert_eq!(
        WindowLoading::Progress { fraction: -2.0 }.bounded(),
        WindowLoading::Progress { fraction: 0.0 }
    );
    assert!(!WindowLoading::Idle.is_loading());
    assert!(WindowLoading::Busy.is_loading());
}

#[test]
fn one_call_says_what_the_window_is_doing_and_an_equal_report_is_free() {
    done();
    let before = revision();
    busy();
    busy();
    assert_eq!(current(), WindowLoading::Busy);
    assert_eq!(revision(), before.wrapping_add(1), "said once");
    progress(0.5);
    assert_eq!(current(), WindowLoading::Progress { fraction: 0.5 });
    done();
    assert_eq!(current(), WindowLoading::Idle);
    assert_eq!(revision(), before.wrapping_add(3));
}

// USER REQUIREMENT (2026-10-21): "determinate progress never goes backwards"
#[test]
fn determinate_progress_never_goes_backwards() {
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::progress(0.2), t0);
    // Reports as an engine sends them, one of them lower than the last.
    let reports = [(300, 0.6), (420, 0.3), (500, 0.35), (900, 0.8), (2000, 0.5)];
    let mut next = 0;
    let mut last = 0.0_f64;
    for ms in (150..6000).step_by(4) {
        let now = at(t0, ms);
        while let Some(&(when, fraction)) = reports.get(next) {
            if when > ms {
                break;
            }
            line.set(WindowLoading::progress(fraction), now);
            next += 1;
        }
        let shown = fill(&line, now).expect("the line shows past its delay");
        assert!(
            shown >= last - 1e-9,
            "went back from {last} to {shown} at {ms} ms"
        );
        assert!(shown < 1.0, "never full before the load is done");
        last = shown;
    }
    assert!(last >= 0.8, "it reached the highest report: {last}");
}

// USER REQUIREMENT (2026-10-21): "Real progress drives it; there's no fake
// creep beyond the reported value, except a slow approach toward the next
// reported step."
#[test]
fn between_reports_the_bar_creeps_slowly_and_never_reaches_the_rest() {
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::progress(0.5), t0);
    let arrived = fill(&line, at(t0, 1000)).unwrap();
    let much_later = fill(&line, at(t0, 60_000)).unwrap();
    assert!(arrived >= 0.5 - 1e-3);
    assert!(much_later > arrived, "it does not stall");
    assert!(much_later < 0.75, "it approaches the next step, never more");
    assert!(
        fill(&line, at(t0, 2000)).unwrap() - arrived < 0.1,
        "and slowly"
    );
}

// USER REQUIREMENT (2026-10-21): "loads finishing within 150 ms never show it"
#[test]
fn loads_finishing_within_150_ms_never_show_it() {
    for first in [WindowLoading::Busy, WindowLoading::progress(0.4)] {
        for finished in [0, 60, 149] {
            let t0 = crate::time::now();
            let mut line = LoadingLine::default();
            line.set(first, t0);
            assert_eq!(
                line.next_wake(t0),
                Some(at(t0, 150)),
                "it waits out its delay asleep"
            );
            assert!(!line.needs_frame(t0), "and asks for no frames meanwhile");
            line.set(WindowLoading::Idle, at(t0, finished));
            for ms in (0..2000).step_by(5) {
                let now = at(t0, ms);
                assert!(line.visual(now).is_none(), "{first:?} shown at {ms} ms");
                assert!(!line.needs_frame(now));
                assert!(line.next_wake(now).is_none());
            }
        }
    }
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::Busy, t0);
    assert!(line.visual(at(t0, 149)).is_none());
    assert!(line.visual(at(t0, 151)).is_some(), "a longer load shows");
}

// USER REQUIREMENT (2026-10-21): "Finish: the bar completes to 100% quickly,
// then fades out over about 200 ms."
#[test]
fn finishing_completes_the_bar_then_fades_it_out() {
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::progress(0.5), t0);
    let before = fill(&line, at(t0, 1000)).unwrap();
    assert!(before < 0.7);
    assert!((opacity(&line, at(t0, 1000)).unwrap() - 1.0).abs() < 1e-3);
    line.set(WindowLoading::Idle, at(t0, 1000));
    assert!(line.needs_frame(at(t0, 1000)));
    // Quickly complete: at most `fast` and a frame.
    assert!(
        (fill(&line, at(t0, 1150)).unwrap() - 1.0).abs() < 1e-2,
        "completes quickly"
    );
    let mut last = 1.0_f32;
    let mut faded_part_way = false;
    let mut gone_at = None;
    for ms in (1000..2000).step_by(4) {
        match opacity(&line, at(t0, ms)) {
            Some(alpha) => {
                assert!(gone_at.is_none(), "it does not come back");
                assert!(alpha <= last + 1e-6, "it only fades");
                faded_part_way |= alpha > 0.05 && alpha < 0.95;
                last = alpha;
            }
            None => {
                gone_at.get_or_insert(ms);
            }
        }
    }
    assert!(faded_part_way, "it fades rather than vanishing");
    let gone_at = gone_at.expect("it is gone");
    assert!(
        (1250..=1500).contains(&gone_at),
        "completes and fades in about a third of a second: {gone_at} ms"
    );
    assert!(!line.needs_frame(at(t0, 1600)), "and then costs nothing");
    assert!(line.next_wake(at(t0, 1600)).is_none());
}

#[test]
fn a_busy_load_sweeps_a_soft_highlight_left_to_right_easing_in_and_out() {
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::Busy, t0);
    let shown = at(t0, 150);
    assert!(line.needs_frame(at(t0, 400)), "a sweep moves every frame");
    let sweep = |ms: u64| match line.visual(shown + Duration::from_millis(ms)) {
        Some(LoadingVisual::Sweep { sweep, .. }) => sweep,
        other => panic!("{other:?}"),
    };
    let mut last = -1.0;
    for ms in (0..1490).step_by(10) {
        let now = sweep(ms);
        assert!(now > last, "left to right");
        last = now;
    }
    let early = sweep(75);
    let middle = sweep(750) - sweep(675);
    assert!(early < middle, "it eases in: {early} then {middle}");
}

// USER REQUIREMENT (2026-10-21): "reduced motion has no sweep"
#[test]
fn reduced_motion_has_no_sweep() {
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.motion = reduced();
    line.set(WindowLoading::Busy, t0);
    let mut levels = Vec::new();
    for ms in (400..2400).step_by(100) {
        match line.visual(at(t0, ms)) {
            Some(LoadingVisual::Pulse { opacity }) => levels.push(opacity),
            other => panic!("busy under reduced motion pulses, got {other:?}"),
        }
        assert!(
            !line.needs_frame(at(t0, ms)),
            "no frames at the display's rate"
        );
    }
    let quiet = levels.iter().copied().fold(f32::MAX, f32::min);
    let full = levels.iter().copied().fold(0.0, f32::max);
    assert!(full - quiet > 0.2, "the track pulses: {levels:?}");
    assert!(line.next_wake(at(t0, 400)).is_some(), "at a gentle rate");

    // Determinate progress still grows, without creeping.
    line.set(WindowLoading::progress(0.6), at(t0, 2400));
    let start = fill(&line, at(t0, 2400)).unwrap();
    let grown = fill(&line, at(t0, 2700)).unwrap();
    assert!(grown > start, "progress grows");
    assert!((grown - 0.6).abs() < 1e-2);
    assert!((fill(&line, at(t0, 20_000)).unwrap() - 0.6).abs() < 1e-2);
}

#[test]
fn hidden_it_draws_nothing_and_shown_it_draws_inside_its_band() {
    let theme = Theme::dark();
    let content = Rect::new(100.0, 80.0, 900.0, 700.0);
    let line_band = band(content, content.y0, &theme);
    assert!((line_band.y0 - content.y0).abs() < 1e-9, "at the top edge");
    assert!((line_band.height() - 2.0).abs() < 1e-9, "2 px at 1x");
    assert!(
        (line_band.width() - content.width()).abs() < 1e-9,
        "edge to edge"
    );
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    let mut scene = Scene::new();
    let drawn = crate::time::testing::with_monotonic_time(t0, || {
        line.draw(&mut scene, Affine::IDENTITY, &theme, line_band)
    });
    assert!(drawn.is_empty() && scene.encoding().draw_tags.is_empty());
    line.set(WindowLoading::Busy, t0);
    let drawn = crate::time::testing::with_monotonic_time(at(t0, 100), || {
        line.draw(&mut scene, Affine::IDENTITY, &theme, line_band)
    });
    assert!(drawn.is_empty(), "nothing during its delay");
    let drawn = crate::time::testing::with_monotonic_time(at(t0, 600), || {
        line.draw(&mut scene, Affine::scale(2.0), &theme, line_band)
    });
    assert_eq!(drawn.len(), 1);
    let device = Affine::scale(2.0).transform_rect_bbox(line_band);
    assert!(
        drawn[0].contains_rect(device),
        "{drawn:?} covers {device:?}"
    );
    assert!(
        drawn[0].height() <= device.height() + 2.0,
        "and no more than its band"
    );
    assert!(!scene.encoding().draw_tags.is_empty());
}

#[test]
fn a_creeping_bar_wakes_once_per_device_pixel_not_every_frame() {
    let theme = Theme::dark();
    let line_band = band(Rect::new(0.0, 0.0, 1000.0, 600.0), 0.0, &theme);
    let t0 = crate::time::now();
    let mut line = LoadingLine::default();
    line.set(WindowLoading::progress(0.5), t0);
    let now = at(t0, 1000);
    let mut scene = Scene::new();
    crate::time::testing::with_monotonic_time(now, || {
        line.draw(&mut scene, Affine::IDENTITY, &theme, line_band);
    });
    assert!(!line.needs_frame(now), "a settled bar draws no frames");
    let wake = line.next_wake(now).expect("it creeps on");
    assert!(wake > now);
    let step = fill(&line, wake).unwrap() - fill(&line, now).unwrap();
    assert!(
        (step - 1.0 / 1000.0).abs() < 2e-4,
        "one pixel of a 1000 px line: {step}"
    );
}
