use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;

fn question(handle: &str) -> Question {
    Question {
        handle: handle.into(),
        kind: "portal.file-chooser".into(),
        app_id: Some("org.example.Editor".into()),
        label: "org.example.Editor".into(),
        details: json!({ "multiple": true }),
    }
}

/// A question waits until it is answered with what the person chose, and a declined one resolves
/// to nothing.
#[tokio::test]
async fn a_question_waits_for_its_answer_or_its_decline() {
    let (asks, person) = channel();
    let asked = asks.ask(question("portal:1"));
    assert_eq!(
        person.question("portal:1").map(|q| q.details),
        Some(json!({ "multiple": true }))
    );
    assert!(person.answer(
        "portal:1",
        Answer {
            uris: vec!["file:///home/me/a.txt".into()],
            ..Answer::default()
        },
    ));
    let answer = asked.await.expect("answered");
    assert_eq!(answer.uris, ["file:///home/me/a.txt"]);
    assert!(person.pending().is_empty());

    let asked = asks.ask(question("portal:2"));
    assert!(person.decline("portal:2"));
    assert!(asked.await.is_none());
}

#[test]
fn dropping_the_asker_withdraws_its_question_and_tells_the_session() {
    let (asks, person) = channel();
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&changes);
    person.on_change(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let asked = asks.ask(question("polkit:7"));
    assert_eq!(person.pending().len(), 1);
    drop(asked);
    assert!(person.pending().is_empty());
    assert_eq!(changes.load(Ordering::SeqCst), 2, "asked, then withdrawn");
}

#[tokio::test]
async fn asking_again_replaces_and_the_old_asker_only_withdraws_its_own() {
    let (asks, person) = channel();
    let first = asks.ask(question("polkit:7"));
    let mut second = question("polkit:7");
    second.label = "retry".into();
    let second = asks.ask(second);
    assert_eq!(person.pending().len(), 1);
    assert!(first.await.is_none(), "the replaced asking ends");
    let pending = person.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.first().map(|q| q.label.as_str()), Some("retry"));
    assert!(person.answer("polkit:7", Answer::default()));
    assert!(second.await.is_some());
}

#[test]
fn an_answer_for_a_question_no_longer_asked_reaches_nobody() {
    let (asks, person) = channel();
    assert!(!person.answer("portal:9", Answer::default()));
    assert!(!person.decline("portal:9"));
    drop(asks.ask(question("portal:9")));
    assert!(
        !person.answer("portal:9", Answer::default()),
        "its asker went"
    );
}

/// A receiver that is gone answers every question no.
#[tokio::test]
async fn nobody_receiving_declines() {
    let (asks, person) = channel();
    drop(person);
    assert!(asks.ask(question("polkit:1")).await.is_none());
}

#[test]
fn typed_values_never_show_in_debug_output() {
    let answer = Answer {
        values: vec![zeroize::Zeroizing::new("hunter2".into())],
        ..Answer::default()
    };
    assert!(!format!("{answer:?}").contains("hunter2"));
}

#[tokio::test]
async fn an_update_changes_what_shows_and_keeps_the_asker_waiting() {
    let (asks, person) = channel();
    let asked = asks.ask(question("q"));
    asks.update("q", serde_json::json!({ "choices": ["b"] }));
    assert_eq!(
        person.question("q").map(|question| question.details),
        Some(serde_json::json!({ "choices": ["b"] }))
    );
    assert!(person.answer("q", Answer::default()));
    assert!(asked.await.is_some());
}

#[test]
fn a_program_withdraws_what_it_asked_under_a_handle() {
    let (asks, person) = channel();
    let _asked = asks.ask(question("nm:1"));
    asks.withdraw("nm:1");
    assert!(person.pending().is_empty());
}
