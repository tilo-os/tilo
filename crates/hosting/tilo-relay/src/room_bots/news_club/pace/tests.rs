#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

/// Always the low end, always happens, always the first choice.
struct Eager;

impl Dice for Eager {
    fn between(&mut self, low: i64, _high: i64) -> i64 {
        low
    }
    fn chance(&mut self, _chance: f64) -> bool {
        true
    }
    fn pick(&mut self, _len: usize) -> usize {
        0
    }
}

/// Never happens.
struct Shy;

impl Dice for Shy {
    fn between(&mut self, _low: i64, high: i64) -> i64 {
        high
    }
    fn chance(&mut self, _chance: f64) -> bool {
        false
    }
    fn pick(&mut self, len: usize) -> usize {
        len - 1
    }
}

fn story(id: u64) -> Story {
    Story {
        id,
        title: format!("Story {id}"),
        url: None,
        text: None,
        score: 100,
    }
}

fn club_line(id: &str, persona: &str) -> Seen {
    Seen {
        id: id.into(),
        persona: Some(persona.into()),
        text: "a thought".into(),
    }
}

fn person_line(id: &str, text: &str) -> Seen {
    Seen {
        id: id.into(),
        persona: None,
        text: text.into(),
    }
}

/// A club with one thread `root` opened at 0 by Ada, nothing planned.
fn opened_club() -> (Club, Pace) {
    let pace = Pace::default();
    let mut club = Club::default();
    club.opened(("root".into(), story(1), "Ada".into()), 0, &pace, &mut Shy);
    (club, pace)
}

#[test]
fn a_new_club_opens_a_story_at_once_then_waits_a_while_for_the_next() {
    let pace = Pace::default();
    let mut club = Club::default();
    let Next::Open { persona } = club.next(0, &pace, &mut Eager) else {
        panic!("a new club opens a story");
    };
    club.opened(
        ("root".into(), story(7), persona.clone()),
        0,
        &pace,
        &mut Eager,
    );
    assert!(club.took(7));
    assert_eq!(club.next_story, pace.story_gap.0);
    let first = &club.planned[0];
    assert_eq!(
        first.due, pace.first_reply.0,
        "the first answer comes minutes later"
    );
    assert_ne!(first.persona, persona, "nobody answers their own opening");
    assert_eq!(club.next(first.due - 1, &pace, &mut Eager), Next::Wait);
}

#[test]
fn an_opening_is_sometimes_left_unanswered() {
    let (club, _) = opened_club();
    assert!(club.planned.is_empty());
}

#[test]
fn a_person_is_answered_sooner_than_the_club_answers_itself() {
    let (mut club, pace) = opened_club();
    club.take_in(
        "root",
        &[person_line("p1", "is this new?")],
        100,
        &pace,
        &mut Eager,
    );
    let plan = club.planned.pop().unwrap();
    assert_eq!(plan.due, 100 + pace.person_reply.0);
    assert_eq!(plan.answering.as_deref(), Some("p1"));

    club.take_in("root", &[club_line("c1", "Bruno")], 100, &pace, &mut Eager);
    let plan = club.planned.pop().unwrap();
    assert_eq!(plan.due, 100 + pace.club_reply.0);
    assert!(pace.person_reply.1 < pace.club_reply.1);
}

#[test]
fn a_line_is_taken_in_once() {
    let (mut club, pace) = opened_club();
    let line = person_line("p1", "hm");
    club.take_in("root", std::slice::from_ref(&line), 10, &pace, &mut Eager);
    club.planned.clear();
    club.take_in("root", &[line], 20, &pace, &mut Eager);
    assert!(
        club.planned.is_empty(),
        "the same line is not answered twice"
    );
}

#[test]
fn a_persona_a_person_names_answers_them() {
    let (mut club, pace) = opened_club();
    club.take_in(
        "root",
        &[person_line("p1", "Hana, what is the threat model?")],
        10,
        &pace,
        &mut Eager,
    );
    assert_eq!(club.planned[0].persona, "Hana");
}

#[test]
fn a_persona_does_not_answer_itself_or_the_voice_before() {
    let (mut club, pace) = opened_club();
    club.take_in("root", &[club_line("c1", "Bruno")], 10, &pace, &mut Shy);
    assert!(club.planned.is_empty(), "the club may stay quiet");
    club.take_in("root", &[club_line("c2", "Chen")], 10, &pace, &mut Eager);
    let plan = &club.planned[0];
    assert_ne!(plan.persona, "Chen");
    assert_ne!(plan.persona, "Bruno");
}

#[test]
fn one_answer_is_planned_per_thread_and_a_person_takes_its_place() {
    let (mut club, pace) = opened_club();
    club.take_in("root", &[club_line("c1", "Bruno")], 10, &pace, &mut Eager);
    club.take_in("root", &[club_line("c2", "Chen")], 10, &pace, &mut Eager);
    assert_eq!(club.planned.len(), 1);
    assert_eq!(club.planned[0].answering.as_deref(), Some("c1"));
    club.take_in("root", &[person_line("p1", "why?")], 20, &pace, &mut Eager);
    assert_eq!(club.planned.len(), 1);
    assert_eq!(club.planned[0].answering.as_deref(), Some("p1"));
}

#[test]
fn a_full_thread_stops_answering() {
    let (mut club, pace) = opened_club();
    for _ in 1..pace.thread_posts {
        club.replied("root", "Bruno", 50);
    }
    club.take_in(
        "root",
        &[person_line("p1", "anyone?")],
        60,
        &pace,
        &mut Eager,
    );
    assert!(club.planned.is_empty());
}

#[test]
fn the_hourly_cap_holds_back_even_a_due_reply() {
    let (mut club, pace) = opened_club();
    club.take_in("root", &[person_line("p1", "hi")], 0, &pace, &mut Eager);
    for _ in 0..pace.hourly_posts {
        club.posted.push_back(100);
    }
    assert_eq!(club.next(1000, &pace, &mut Eager), Next::Wait);
    assert!(matches!(club.next(3700, &pace, &mut Eager), Next::Reply(_)));
}

#[test]
fn an_old_thread_is_retired_with_its_plans() {
    let (mut club, pace) = opened_club();
    club.take_in("root", &[person_line("p1", "hi")], 0, &pace, &mut Eager);
    club.retire(pace.thread_life, &pace);
    assert!(club.threads.is_empty());
    assert!(club.planned.is_empty());
}

#[test]
fn no_new_story_while_the_live_threads_are_full() {
    let pace = Pace::default();
    let mut club = Club::default();
    for id in 0..pace.live_threads {
        club.opened(
            (
                format!("r{id}"),
                story(u64::try_from(id).unwrap()),
                "Ada".into(),
            ),
            0,
            &pace,
            &mut Shy,
        );
    }
    club.next_story = 0;
    assert_eq!(club.next(10, &pace, &mut Shy), Next::Wait);
}
