//! The schedule's rules, through the coach's own interface.

use super::coach::{CUE_FRESH, IGNORED_AFTER, MIN_GAP, RETURN_AFTER};
use super::*;
use crate::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sig {
    Opened,
    Moved,
    BarHovered,
    TwoWindows,
}

static SPECS: &[HintSpec<Sig>] = &[
    HintSpec {
        id: "open",
        when: &[Cue::After(Duration::from_secs(17))],
        only_while: &[],
        learned_by: &[Sig::Opened],
        gesture: Gesture::OpenFromSearch,
        words: "open",
    },
    HintSpec {
        id: "move",
        when: &[Cue::Saw(Sig::BarHovered), Cue::Saw(Sig::Opened)],
        only_while: &[],
        learned_by: &[Sig::Moved],
        gesture: Gesture::MoveWindow,
        words: "move",
    },
    HintSpec {
        id: "resize",
        when: &[Cue::Holds(Sig::TwoWindows)],
        only_while: &[Sig::TwoWindows],
        learned_by: &[],
        gesture: Gesture::ResizeEdge,
        words: "resize",
    },
];

const TIMING: Timing = Timing {
    leaving: Duration::from_millis(210),
    done: Duration::from_millis(1800),
};

struct Run {
    coach: Coach<Sig>,
    start: Instant,
    facts: Vec<Sig>,
    quiet: Option<Quiet>,
}

impl Run {
    fn new() -> Self {
        Self::with(Learned::default())
    }

    fn with(learned: Learned) -> Self {
        let mut run = Self {
            coach: Coach::new(SPECS, learned),
            start: crate::time::now(),
            facts: Vec::new(),
            quiet: None,
        };
        run.tick(0);
        run
    }

    fn at(&self, secs: u64) -> Instant {
        self.start + Duration::from_secs(secs)
    }

    fn tick(&mut self, secs: u64) {
        let facts = self.facts.clone();
        let now = self.at(secs);
        self.coach
            .tick(now, &|s| facts.contains(&s), self.quiet, &|_| true, TIMING);
    }

    fn see(&mut self, secs: u64, signal: Sig) {
        self.coach.observe(signal, self.at(secs));
    }

    fn showing(&self) -> Option<&'static str> {
        let current = self.coach.current()?;
        matches!(current.phase, Phase::Shown(_)).then(|| SPECS[current.index].id)
    }
}

#[test]
fn a_hint_shows_at_its_moment() {
    let mut run = Run::new();
    run.tick(10);
    assert_eq!(run.showing(), None, "too early");
    run.tick(17);
    assert_eq!(run.showing(), Some("open"));
    assert_eq!(run.coach.learned().showings("open"), 1);
}

#[test]
fn an_expert_who_already_did_it_never_sees_it() {
    let mut run = Run::new();
    run.see(5, Sig::Opened);
    run.see(6, Sig::Moved);
    for secs in (7..300).step_by(3) {
        run.tick(secs);
        assert_ne!(run.showing(), Some("open"));
        assert_ne!(run.showing(), Some("move"));
    }
    assert!(run.coach.learned().knows("open") && run.coach.learned().knows("move"));
    assert_eq!(run.coach.learned().showings("open"), 0);
}

#[test]
fn doing_it_while_it_shows_acknowledges_and_learns_it() {
    let mut run = Run::new();
    run.tick(20);
    assert_eq!(run.showing(), Some("open"));
    run.see(21, Sig::Opened);
    assert!(matches!(
        run.coach.current().map(|c| c.phase),
        Some(Phase::Done(_))
    ));
    assert!(run.coach.learned().knows("open"));
    let changed = run.coach.take_changed().expect("the record changed");
    assert!(changed.knows("open"));
    run.tick(23);
    assert_eq!(run.coach.current(), None, "the acknowledgement ends");
}

#[test]
fn at_most_one_hint_begins_a_minute() {
    let mut run = Run::new();
    run.tick(17);
    assert_eq!(run.showing(), Some("open"));
    run.see(18, Sig::Opened);
    // The opened window is the move hint's moment, but the minute holds it.
    run.tick(21);
    run.tick(30);
    assert_eq!(run.showing(), None);
    // Its moment went stale before the minute was up.
    let since = 17 + MIN_GAP.as_secs();
    run.tick(since);
    assert_eq!(run.showing(), None);
    run.see(since + 5, Sig::BarHovered);
    run.tick(since + 5);
    assert_eq!(run.showing(), Some("move"));
    assert!(CUE_FRESH < MIN_GAP);
}

#[test]
fn only_one_hint_at_a_time() {
    let mut run = Run::new();
    run.facts.push(Sig::TwoWindows);
    run.tick(1);
    assert_eq!(run.showing(), Some("resize"));
    run.tick(8);
    assert_eq!(run.showing(), Some("resize"));
    // Ignored, it went; the open hint's moment came meanwhile, and waits
    // out the minute.
    run.tick(17);
    run.tick(18);
    assert_eq!(run.showing(), None, "the open hint waits");
    run.tick(61);
    assert_eq!(run.showing(), Some("open"));
}

#[test]
fn never_while_typing_in_a_call_or_following() {
    for quiet in [Quiet::Typing, Quiet::Call, Quiet::Following, Quiet::Busy] {
        let mut run = Run::new();
        run.quiet = Some(quiet);
        run.tick(20);
        assert_eq!(run.showing(), None, "{quiet:?}");
        run.quiet = None;
        run.tick(21);
        assert_eq!(run.showing(), Some("open"), "after {quiet:?}");
    }
}

#[test]
fn a_call_takes_a_showing_hint_away_but_typing_does_not() {
    let mut run = Run::new();
    run.tick(17);
    run.quiet = Some(Quiet::Typing);
    run.tick(18);
    assert_eq!(run.showing(), Some("open"));
    run.quiet = Some(Quiet::Call);
    run.tick(19);
    assert!(matches!(
        run.coach.current().map(|c| c.phase),
        Some(Phase::Leaving(_))
    ));
}

#[test]
fn an_ignored_hint_goes_and_comes_back_once_much_later() {
    let mut run = Run::new();
    run.tick(17);
    let ignored = 17 + IGNORED_AFTER.as_secs();
    run.tick(ignored);
    assert!(matches!(
        run.coach.current().map(|c| c.phase),
        Some(Phase::Leaving(_))
    ));
    run.tick(ignored + 1);
    assert_eq!(run.coach.current(), None);
    run.tick(ignored + 120);
    assert_eq!(run.showing(), None, "not again soon");
    let back = ignored + RETURN_AFTER.as_secs();
    run.tick(back);
    assert_eq!(run.showing(), Some("open"), "once, much later");
    run.tick(back + IGNORED_AFTER.as_secs());
    run.tick(back + IGNORED_AFTER.as_secs() + 1);
    run.tick(back + 3 * RETURN_AFTER.as_secs());
    assert_eq!(run.showing(), None, "never a third time");
    assert_eq!(run.coach.learned().showings("open"), 2);
}

#[test]
fn turned_off_nothing_shows_and_a_showing_hint_goes() {
    let mut run = Run::new();
    run.tick(17);
    run.coach.set_enabled(false, run.at(18));
    assert!(matches!(
        run.coach.current().map(|c| c.phase),
        Some(Phase::Leaving(_))
    ));
    run.facts.push(Sig::TwoWindows);
    run.tick(200);
    run.tick(400);
    assert_eq!(run.showing(), None);
}

#[test]
fn learned_on_another_device_is_learned_here() {
    let mut run = Run::new();
    run.tick(17);
    let mut elsewhere = Learned::default();
    elsewhere.learn("open");
    elsewhere.learn("move");
    run.coach.merge(&elsewhere, run.at(18));
    assert!(matches!(
        run.coach.current().map(|c| c.phase),
        Some(Phase::Leaving(_))
    ));
    run.see(100, Sig::BarHovered);
    run.tick(100);
    assert_eq!(run.showing(), None);
}

#[test]
fn a_record_from_before_starts_where_it_left_off() {
    let mut learned = Learned::default();
    learned.learn("open");
    learned.shown.insert("move".into(), 2);
    let mut run = Run::with(learned);
    run.see(1, Sig::BarHovered);
    run.tick(20);
    assert_eq!(run.showing(), None);
}

#[test]
fn the_record_reads_what_it_wrote() {
    let mut learned = Learned::default();
    learned.learn("move");
    learned.note_shown("open");
    learned.enabled = false;
    let json = serde_json::to_string(&learned).unwrap();
    assert_eq!(serde_json::from_str::<Learned>(&json).unwrap(), learned);
    let empty: Learned = serde_json::from_str("{}").unwrap();
    assert!(empty.enabled, "hints are on until turned off");
}

#[test]
fn the_performed_log_hears_each_kind_once_per_read() {
    let _ = performed::take();
    performed::note(performed::Performed::PannedView);
    performed::note(performed::Performed::PannedView);
    performed::note(performed::Performed::MovedWindow);
    assert_eq!(
        performed::take(),
        vec![
            performed::Performed::PannedView,
            performed::Performed::MovedWindow
        ]
    );
    assert!(performed::take().is_empty());
}
