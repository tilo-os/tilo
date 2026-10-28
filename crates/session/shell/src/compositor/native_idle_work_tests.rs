//! Count actual widget work through the native pump and scheduling gates.
//!
//! An app may retain its poll deadline until draw consumes it. Re-arming that
//! already answered deadline used to run the compositor flat out, even though
//! the pump found no changed pixels. Virtual instants select scheduler events;
//! no elapsed time, thread scheduling or CPU measurement decides these tests.

use super::*;
use std::cell::Cell;
use std::rc::Rc;
use ui::widget::Widget;
use vello::{Scene, kurbo::Affine};

#[derive(Clone, Default)]
struct Work {
    updates: Rc<Cell<usize>>,
    layouts: Rc<Cell<usize>>,
    draws: Rc<Cell<usize>>,
    changed: Rc<Cell<bool>>,
}

impl Work {
    fn counts(&self) -> (usize, usize, usize) {
        (self.updates.get(), self.layouts.get(), self.draws.get())
    }
}

/// A deliberately uncooperative app: only drawing clears its deadline.
/// The real host must be able to poll this app without drawing it.
struct RetainedDeadline {
    deadline: Option<Instant>,
    work: Work,
}

impl Widget for RetainedDeadline {
    fn name(&self) -> &'static str {
        "retained-deadline-app"
    }

    fn update(&mut self, _: &dyn std::any::Any, _: &dyn std::any::Any) -> bool {
        self.work.updates.set(
            self.work
                .updates
                .get()
                .checked_add(1)
                .expect("update count overflow"),
        );
        self.work.changed.replace(false)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _: &ui::theme::Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {
        self.work.layouts.set(
            self.work
                .layouts
                .get()
                .checked_add(1)
                .expect("layout count overflow"),
        );
    }

    fn draw(
        &mut self,
        _: &mut Scene,
        _: Affine,
        _: &mut ui::text_render::TextRenderer,
        _: &ui::theme::Theme,
        _: &ui::presence::PresenceCtx,
    ) {
        self.work.draws.set(
            self.work
                .draws
                .get()
                .checked_add(1)
                .expect("draw count overflow"),
        );
        self.deadline = None;
    }
}

struct Desktop {
    frame: FrameBuilder,
    doc: AutoCommit,
    now: Instant,
    apps: Vec<Work>,
}

impl Desktop {
    fn new(deadline: Instant, apps: &[Work]) -> Self {
        let mut content = ui::widget::Column::new();
        for work in apps {
            content = content.push(Box::new(RetainedDeadline {
                deadline: Some(deadline),
                work: work.clone(),
            }));
        }
        let mut frame = FrameBuilder::with_fixture_content(Box::new(content));
        // The desk's widgets appear with the first update, a change of the
        // shell's own; these tests count what an app's deadline costs.
        frame.host_mut().force_desk_settled(Instant::now());
        // Once the desktop is first painted, a control hint's moment comes
        // seconds later, and the scheduler rightly wakes for it; these tests
        // count what an app's deadline costs.
        frame.host_mut().rest_control_hints();
        Self {
            frame,
            doc: AutoCommit::new(),
            now: deadline,
            apps: apps.to_vec(),
        }
    }

    /// Run the real app pump, redraw decision and park selector. There are
    /// no other services, animations, pending clients or presentation pressure
    /// in this fixture. The initial frame's layout is already built.
    fn tick(&mut self) -> Option<Duration> {
        let changed = self.frame.pump_apps(&mut self.doc, &automerge::ROOT);
        let app_tick = NativeAppTick::after_pump(changed, self.now);
        let reasons = NativeRedrawReasons {
            apps: app_tick.redraw,
            ..Default::default()
        };
        if reasons.content_changed(false) {
            self.frame.relayout();
            let _ = self.frame.paint();
        }
        native_loop_park_named(
            self.now,
            true,
            false,
            false,
            self.now
                .checked_add(Duration::from_millis(16))
                .expect("cadence instant overflow"),
            [("widget", app_tick.park_deadline(&self.frame, true))],
        )
        .0
    }

    /// Execute at most 128 scheduled events, not 128 milliseconds. The cap
    /// makes an injected zero-timeout spin fail by work count instead of hang.
    fn run_until_parked(&mut self) {
        for _ in 0..128 {
            let Some(wait) = self.tick() else { return };
            self.now = self
                .now
                .checked_add(wait)
                .expect("scheduled instant overflow");
        }
        panic!(
            "native scheduler exhausted 128 events without parking; app (updates, layouts, draws)={:?}; deadline={:?}",
            self.apps.iter().map(Work::counts).collect::<Vec<_>>(),
            self.frame.next_wake_deadline()
        );
    }
}

/// Each count runs on [`REPETITIONS`] fresh fixture desktops, every one with
/// its own hash seeds and widget ids. Building a desktop takes seconds in a
/// debug build, so the repetitions are split into [`SPLITS`] tests that run
/// side by side instead of one that holds a test slot for minutes.
const REPETITIONS: usize = 64;
const SPLITS: usize = 16;

/// The repetitions of split `part` of [`SPLITS`].
fn repetitions(part: usize) -> std::ops::Range<usize> {
    let each = REPETITIONS.checked_div(SPLITS).expect("SPLITS is not zero");
    let at = |part: usize| part.checked_mul(each).expect("repetition overflow");
    at(part)..at(plus(part, 1))
}

/// `count + more`, failing on overflow instead of wrapping.
fn plus(count: usize, more: usize) -> usize {
    count.checked_add(more).expect("count overflow")
}

/// One test per split of each count, numbered from 1.
macro_rules! split_tests {
    ($count:ident: $($name:ident = $part:expr),+ $(,)?) => {
        $(#[test]
        fn $name() {
            $count(repetitions($part));
        })+
    };
}

#[test]
fn the_splits_cover_every_repetition_once() {
    let covered: Vec<usize> = (0..SPLITS).flat_map(repetitions).collect();
    assert_eq!(covered, (0..REPETITIONS).collect::<Vec<_>>());
}

split_tests!(future_deadline_wakes_once:
    a_future_app_deadline_wakes_once_then_parks_without_drawing_1 = 0,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_2 = 1,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_3 = 2,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_4 = 3,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_5 = 4,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_6 = 5,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_7 = 6,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_8 = 7,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_9 = 8,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_10 = 9,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_11 = 10,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_12 = 11,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_13 = 12,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_14 = 13,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_15 = 14,
    a_future_app_deadline_wakes_once_then_parks_without_drawing_16 = 15,
);

split_tests!(answered_deadline_does_one_pump:
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_1 = 0,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_2 = 1,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_3 = 2,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_4 = 3,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_5 = 4,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_6 = 5,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_7 = 6,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_8 = 7,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_9 = 8,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_10 = 9,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_11 = 10,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_12 = 11,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_13 = 12,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_14 = 13,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_15 = 14,
    an_answered_app_deadline_does_one_pump_and_no_layout_or_draw_16 = 15,
);

fn future_deadline_wakes_once(repetitions: std::ops::Range<usize>) {
    let due = Instant::now()
        .checked_add(Duration::from_secs(1))
        .expect("instant overflow");
    for repetition in repetitions {
        let apps = [Work::default(), Work::default()];
        let mut desktop = Desktop::new(due, &apps);
        let built = apps.each_ref().map(Work::counts);
        desktop.now = due.checked_sub(Duration::from_secs(1)).unwrap();
        desktop.run_until_parked();
        assert_eq!(desktop.now, due, "the scheduler delivered the future poll");
        for (app, built) in apps.iter().zip(built) {
            assert_eq!(app.counts(), (plus(built.0, 2), built.1, built.2));
        }
        drop(desktop);
        println!(
            "IDLE-FUTURE-WORK repetition={repetition} apps=2 per_app_pumps=2 layouts=0 draws=0 scheduled_wakes=1"
        );
    }
}

fn answered_deadline_does_one_pump(repetitions: std::ops::Range<usize>) {
    let now = Instant::now();
    for repetition in repetitions {
        let apps = [Work::default(), Work::default()];
        let work = &apps[0];
        let mut desktop = Desktop::new(now, &apps);
        let built = apps.each_ref().map(Work::counts);
        desktop.run_until_parked();
        for (app, built) in apps.iter().zip(built) {
            assert_eq!(
                app.counts(),
                (plus(built.0, 1), built.1, built.2),
                "repetition {repetition}: each unchanged app must be polled once, not spin or paint"
            );
        }
        assert_eq!(
            desktop.frame.next_wake_deadline(),
            Some(now),
            "the app really retained its answered deadline; only the native scheduler parks it"
        );

        // A later real app change still reaches layout/draw; parking the
        // deadline must not freeze the app or discard its pending work.
        work.changed.set(true);
        desktop.now = desktop
            .now
            .checked_add(Duration::from_secs(1))
            .expect("instant overflow");
        desktop.run_until_parked();
        for (app, built) in apps.iter().zip(built) {
            assert_eq!(
                app.counts(),
                (plus(built.0, 2), plus(built.1, 1), plus(built.2, 1))
            );
        }
        assert_eq!(
            desktop.frame.next_wake_deadline(),
            None,
            "the real paint pass reached the app and consumed its deadline"
        );
        drop(desktop);
        println!(
            "IDLE-WORK repetition={repetition} apps=2 per_app_idle_pumps=1 idle_layouts=0 idle_draws=0 changed_pumps=1 changed_layouts=1 changed_draws=1"
        );
    }
}
