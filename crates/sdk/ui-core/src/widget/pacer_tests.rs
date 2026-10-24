use std::cell::Cell;
use std::rc::Rc;

use super::Pacer;
use crate::LocalState;
use crate::time::{Duration, Instant};
use crate::widget::{Container, Widget};

const BEAT: Duration = Duration::from_millis(850);

fn pacer(running: &LocalState<bool>, steps: &Rc<Cell<usize>>, last: usize) -> Pacer {
    let steps = Rc::clone(steps);
    Pacer::new(Box::new(Container::new()), running, BEAT, move || {
        steps.set(steps.get() + 1);
        steps.get() < last
    })
}

#[test]
fn a_running_pacer_steps_once_a_beat_and_sleeps_between() {
    let running = LocalState::new(true);
    let steps = Rc::new(Cell::new(0));
    let mut pacer = pacer(&running, &steps, 10);
    let start = Instant::now();
    let first = pacer
        .next_wake_deadline()
        .expect("a running pacer asks to wake");
    assert!(first >= start + BEAT, "it wakes a beat later, not at once");
    pacer.serve_deadline(first - Duration::from_millis(1));
    assert_eq!(steps.get(), 0, "nothing steps before the beat");
    pacer.serve_deadline(first);
    assert_eq!(steps.get(), 1);
    let second = pacer.next_wake_deadline().unwrap();
    assert_eq!(second, first + BEAT, "beats stay on one grid");
    // A window busy for three beats takes one step, not a burst.
    pacer.serve_deadline(second + BEAT * 3);
    assert_eq!(steps.get(), 2);
}

#[test]
fn a_stopped_pacer_asks_for_nothing_and_the_last_step_stops_it() {
    let running = LocalState::new(false);
    let steps = Rc::new(Cell::new(0));
    let mut pacer = pacer(&running, &steps, 2);
    assert_eq!(pacer.next_wake_deadline(), None);
    pacer.serve_deadline(Instant::now() + BEAT * 10);
    assert_eq!(steps.get(), 0, "a stopped pacer never steps");

    running.set(true);
    let mut due = pacer.next_wake_deadline().unwrap();
    pacer.serve_deadline(due);
    due = pacer.next_wake_deadline().unwrap();
    pacer.serve_deadline(due);
    assert_eq!(steps.get(), 2);
    assert!(!running.get(), "the step that said stop stopped it");
    assert_eq!(pacer.next_wake_deadline(), None);
}
