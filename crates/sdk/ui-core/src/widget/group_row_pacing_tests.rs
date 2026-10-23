//! How a strip moves frame by frame under a touchpad pan, a throw and a spun
//! wheel, measured through the production frame path ([`FrameHarness`]) on a
//! virtual clock: the display's frames and the input device's reports each
//! keep their own rate, as on real hardware.

use super::tests::make_group;
use super::*;
use crate::headless::FrameHarness;
use crate::time::testing::with_monotonic_time;
use std::cell::RefCell;
use std::rc::Rc;

/// What the input device did since the last frame, delivered on the next.
enum Report {
    Begin,
    Travel(f64, Instant),
    End(Instant),
    Reveal(u64, Instant),
}

/// A row fed reports between frames, as the shell feeds a touchpad's.
struct FedRow {
    row: GroupRow,
    reports: Rc<RefCell<Vec<Report>>>,
}

impl crate::widget::Decorator for FedRow {
    type Inner = GroupRow;
    fn inner(&self) -> &GroupRow {
        &self.row
    }
    fn inner_mut(&mut self) -> &mut GroupRow {
        &mut self.row
    }
    fn needs_redraw(&self) -> bool {
        !self.reports.borrow().is_empty() || self.row.needs_redraw()
    }
    fn update(&mut self, state: &crate::widget::AppModel, path: &ModelPath) -> bool {
        for report in self.reports.borrow_mut().drain(..) {
            match report {
                Report::Begin => self.row.begin_strip_pan(),
                Report::Travel(travel, at) => {
                    let _ = self.row.update_strip_pan(travel, at);
                }
                Report::End(at) => {
                    let _ = self.row.end_strip_pan(at);
                }
                Report::Reveal(group, at) => {
                    let _ = self.row.scroll_group_into_view_animated(
                        group,
                        at,
                        crate::tokens::MotionSource::Pointer,
                    );
                }
            }
        }
        self.row.update(state, path)
    }
}

struct Strip {
    harness: FrameHarness,
    reports: Rc<RefCell<Vec<Report>>>,
    start: Instant,
    /// Frames of motion that measured anything.
    measured: u32,
    frames: u32,
}

impl Strip {
    fn new(windows: u64) -> Self {
        let reports = Rc::new(RefCell::new(Vec::new()));
        let groups = (1..=windows)
            .map(|id| make_group(id * 100, &[id]))
            .collect();
        let start = crate::time::now();
        let mut harness = with_monotonic_time(start, || {
            FrameHarness::for_root(
                crate::State::new(),
                FedRow {
                    row: GroupRow::new(groups),
                    reports: Rc::clone(&reports),
                },
                (1200, 800),
                Theme::dark(),
            )
        });
        // Let the windows finish opening (their widths grow in), so every
        // frame measured afterwards is the strip's own motion.
        for frame in 0..90_u32 {
            let at = start + Duration::from_millis(u64::from(frame) * 11);
            with_monotonic_time(at, || harness.poll_frame()).unwrap();
        }
        Self {
            harness,
            reports,
            start,
            measured: 0,
            frames: 0,
        }
    }

    fn at(&self, ms: f64) -> Instant {
        self.start + Duration::from_secs_f64(1.0 + ms / 1000.0)
    }

    fn report(&self, report: Report) {
        self.reports.borrow_mut().push(report);
    }

    fn row(&self) -> &GroupRow {
        &self.harness.root::<FedRow>().row
    }

    /// Run the frame due at `ms`; the scroll it lays the windows out at.
    /// A frame of a strip in motion moves windows already laid out: it never
    /// rebuilds the strip, and only a window first coming into view may
    /// measure (its own content, once).
    fn frame(&mut self, ms: f64) -> f64 {
        let at = self.at(ms);
        let presented = with_monotonic_time(at, || self.harness.poll_frame()).unwrap();
        if presented {
            let stats = self.harness.last_relayout();
            assert!(
                !stats.rebuilt,
                "the frame at {ms} ms of a moving strip rebuilt it: {stats:?}"
            );
            self.frames += 1;
            self.measured += u32::from(stats.measured);
        }
        self.row().drawn_scroll_x()
    }

    /// Whether at most one frame in ten of the motion so far measured
    /// anything: windows entering view, never the motion itself.
    fn measured_rarely(&self) -> bool {
        self.measured * 10 <= self.frames
    }

    fn animating(&self, ms: f64) -> bool {
        with_monotonic_time(self.at(ms), || {
            crate::widget::Widget::needs_animation_frame(self.row())
        })
    }
}

/// Per-frame scroll steps of a steady swipe at `speed` px/s, its touchpad
/// reporting at `report_hz` and the display refreshing at `frame_hz`, over
/// `ms` of swiping; the steps of the first 30 ms (before the path has
/// history) are left out. Also returns the strip with the fingers still down
/// at `ms`.
fn swipe(report_hz: f64, frame_hz: f64, speed: f64, ms: f64) -> (Vec<f64>, Strip) {
    let mut strip = Strip::new(10);
    let report_dt = 1000.0 / report_hz;
    let frame_dt = 1000.0 / frame_hz;
    let mut reports = 0_u32;
    let mut previous = strip.row().scroll_x();
    let mut steps = Vec::new();
    strip.report(Report::Begin);
    for frame in 0_u32.. {
        let frame_ms = frame_dt * (f64::from(frame) + 0.5);
        if frame_ms > ms {
            break;
        }
        // Every report the touchpad made before this frame.
        loop {
            let report_ms = report_dt * f64::from(reports);
            if report_ms > frame_ms {
                break;
            }
            let travel = speed * (report_ms + report_dt) / 1000.0;
            strip.report(Report::Travel(travel, strip.at(report_ms)));
            reports += 1;
        }
        let scroll = strip.frame(frame_ms);
        if frame_ms > 30.0 {
            steps.push(scroll - previous);
        }
        previous = scroll;
    }
    (steps, strip)
}

fn spread(steps: &[f64]) -> (f64, f64, f64) {
    let min = steps.iter().copied().fold(f64::INFINITY, f64::min);
    let max = steps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let frames = f64::from(u32::try_from(steps.len()).expect("a handful of frames"));
    let mean = steps.iter().sum::<f64>() / frames;
    (min, mean, max)
}

/// A touchpad reports at its own rate and the display refreshes at another;
/// showing the newest report moved the strip one report on some frames and
/// two on others (at 125 Hz into 90 Hz: 9.6 px, then 19.2 px). Every frame
/// now moves the distance the fingers covered in one frame.
#[test]
fn a_steady_swipe_moves_the_strip_the_same_distance_every_frame() {
    let speed = 1200.0;
    for (report_hz, frame_hz) in [(125.0, 90.0), (125.0, 120.0), (100.0, 120.0), (140.0, 60.0)] {
        let (steps, strip) = swipe(report_hz, frame_hz, speed, 400.0);
        let expected = speed / frame_hz;
        let (min, mean, max) = spread(&steps);
        eprintln!(
            "swipe {report_hz} Hz reports / {frame_hz} Hz frames: steps min {min:.2} mean \
             {mean:.2} max {max:.2} (fingers {expected:.2} px per frame)"
        );
        assert!(steps.len() > 15, "{report_hz}/{frame_hz}: {steps:?}");
        assert!(
            strip.measured_rarely(),
            "{report_hz}/{frame_hz}: {} of {} frames measured",
            strip.measured,
            strip.frames
        );
        for step in &steps {
            assert!(
                (step - expected).abs() <= expected * 0.02,
                "{report_hz} Hz reports / {frame_hz} Hz frames: every frame moves \
                 {expected:.2} px, the fingers' own pace: {steps:?}"
            );
        }
    }
}

/// Lifting fingers that are still moving throws the strip: it carries on at
/// their speed and slows to a stop over the gesture's coast time, rather
/// than stopping dead under the fingers, which made crossing a long strip a
/// matter of many short swipes.
#[test]
fn lifted_fingers_throw_the_strip_and_it_coasts_to_rest() {
    let speed = 1500.0;
    let frame_dt = 1000.0 / 90.0;
    let (steps, mut strip) = swipe(125.0, 90.0, speed, 300.0);
    let last_step = *steps.last().unwrap();
    let released = 300.0;
    strip.report(Report::End(strip.at(released)));
    let mut previous = strip.row().scroll_x();
    let at_release = previous;
    let mut glide = Vec::new();
    for frame in 0..180_u32 {
        let scroll = strip.frame(released + frame_dt * (f64::from(frame) + 0.5));
        glide.push(scroll - previous);
        previous = scroll;
    }
    let travelled = previous - at_release;
    let coast = speed * f64::from(crate::tokens::Gesture::default().fling_projection_ms) / 1000.0;
    eprintln!(
        "throw at {speed} px/s: last pan step {last_step:.2}, first glide steps {:?}, coasted \
         {travelled:.1} px (velocity x coast time {coast:.1})",
        &glide[..6]
    );
    assert!(
        (travelled - coast).abs() <= coast * 0.1,
        "a throw coasts velocity x coast time: {travelled} vs {coast}"
    );
    assert!(
        glide[1] > last_step * 0.6 && glide[1] <= last_step * 1.05,
        "the throw starts at the fingers' speed: {last_step} then {glide:?}"
    );
    assert!(
        glide
            .windows(2)
            .skip(1)
            .all(|w| w[1] <= w[0] + MOVE_SETTLED_PX),
        "and only slows: {glide:?}"
    );
    assert!(
        !strip.animating(released + 2000.0),
        "a coasted strip comes to rest and stops asking for frames"
    );
    assert!(
        strip.measured_rarely(),
        "{} of {} frames measured",
        strip.measured,
        strip.frames
    );

    // Fingers that stopped before lifting throw nothing.
    let (_, mut strip) = swipe(125.0, 90.0, speed, 300.0);
    let rested = strip.frame(400.0);
    strip.report(Report::End(strip.at(420.0)));
    assert_eq!(strip.frame(430.0), rested);
    assert_eq!(strip.frame(600.0), rested, "a release at rest stays put");
}

/// The frames of a spun wheel's motion while notches still arrive: from the
/// fourth frame of motion (past the start from rest) to the last notch.
/// `steps` are `(frame time, step)` in ms and px.
fn while_spinning(steps: &[(f64, f64)], last_notch: f64) -> Vec<f64> {
    steps
        .iter()
        .skip_while(|(_, step)| step.abs() < 0.5)
        .skip(3)
        .take_while(|(ms, _)| *ms <= last_notch)
        .map(|&(_, step)| step)
        .collect()
}

/// A spun wheel (Alt+Shift+wheel) moves focus a window per notch, and each
/// notch retargets the reveal. Restarting an eased curve from rest at every
/// notch made the strip surge and stall (steps of 75, 15, then 139 px); the
/// glide carries its velocity through a retarget.
#[test]
fn a_spun_wheel_moves_the_strip_without_stalls_or_surges() {
    let mut strip = Strip::new(12);
    let ids = strip.row().group_ids();
    let frame_dt = 1000.0 / 90.0;
    let notch_dt = 70.0;
    let mut notches = 0_u32;
    let mut previous = strip.row().scroll_x();
    let mut steps = Vec::new();
    let mut ms = 0.0;
    for frame in 0..108_u32 {
        ms = frame_dt * f64::from(frame);
        // Every notch the wheel clicked before this frame.
        loop {
            let notch_ms = notch_dt * f64::from(notches);
            if notches >= 8 || notch_ms > ms {
                break;
            }
            let index = usize::try_from(notches).expect("eight notches") + 3;
            let target = ids[index.min(ids.len() - 1)];
            strip.report(Report::Reveal(target, strip.at(notch_ms)));
            notches += 1;
        }
        let scroll = strip.frame(ms);
        steps.push((ms, scroll - previous));
        previous = scroll;
    }
    let spinning = while_spinning(&steps, notch_dt * 7.0);
    let (min, mean, max) = spread(&spinning);
    let surge = spinning
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(0.0_f64, f64::max);
    eprintln!(
        "wheel spun 8 notches every {notch_dt} ms at 90 Hz: steps while spinning min {min:.1} \
         mean {mean:.1} max {max:.1}, largest step increase {surge:.1}; steps {:?}",
        steps.iter().map(|(_, s)| s.round()).collect::<Vec<_>>()
    );
    assert!(spinning.len() >= 10, "the spin moves the strip: {steps:?}");
    assert!(
        min >= max / 3.0,
        "no frame of a spin stalls next to a surging one: {spinning:?}"
    );
    assert!(
        surge <= mean * 0.6,
        "no notch makes the strip lurch forward: {spinning:?}"
    );
    assert!(!strip.animating(ms), "the reveal comes to rest");
    assert!(
        strip.measured_rarely(),
        "{} of {} frames measured",
        strip.measured,
        strip.frames
    );
}
