use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tilo_actions::DoorError;

use super::*;
use crate::{Service, Services};

fn question(handle: &str) -> Question {
    Question {
        handle: handle.into(),
        kind: "portal.file-chooser".into(),
        app_id: Some("org.example.Editor".into()),
        label: "org.example.Editor".into(),
        details: json!({ "multiple": true }),
    }
}

fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn services(questions: &Questions) -> Services {
    let mut services = Services::default();
    services.add(Box::new(questions.clone()));
    services
}

fn invoke(
    services: &mut Services,
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<Result<Started, DoorError>> {
    services.invoke(resource, action, arguments)
}

#[tokio::test]
async fn an_answer_through_the_published_action_reaches_the_asker() {
    let questions = Questions::new();
    let asked = questions.ask(question("portal:1"));

    let published = questions.resources();
    let root = published.first().expect("the questions resource");
    assert_eq!(root.id.as_str(), QUESTIONS_RESOURCE);
    let child = root.children.first().expect("one question");
    assert_eq!(child.id.as_str(), question_resource("portal:1"));
    assert_eq!(child.kind, QUESTION_KIND);
    assert_eq!(
        child
            .presentation
            .as_ref()
            .and_then(|p| p.detail.as_deref()),
        Some("portal.file-chooser")
    );
    let answer = child
        .actions
        .iter()
        .find(|action| action.id == "answer")
        .expect("answer action");
    assert!(answer.contract.is_attended(), "only the person answers");

    let done = invoke(
        &mut services(&questions),
        &question_resource("portal:1"),
        "answer",
        &arguments(json!({ "uris": ["file:///home/me/a.txt"] })),
    )
    .expect("the questions publish it");
    assert!(done.is_ok(), "{done:?}");
    let answer = asked.await.expect("answered");
    assert_eq!(answer.uris, ["file:///home/me/a.txt"]);
    assert!(questions.pending().is_empty());
}

#[test]
fn a_questions_reading_is_what_the_sheet_shows() {
    let questions = Questions::new();
    let _asked = questions.ask(question("portal:4"));
    let read = questions
        .read(&question_resource("portal:4"))
        .expect("published");
    assert_eq!(read["kind"], "portal.file-chooser");
    assert_eq!(read["details"]["multiple"], true);
    assert!(questions.read(&question_resource("gone")).is_none());
}

#[tokio::test]
async fn declining_resolves_to_nothing() {
    let questions = Questions::new();
    let asked = questions.ask(question("portal:2"));
    let done = invoke(
        &mut services(&questions),
        &question_resource("portal:2"),
        "cancel",
        &serde_json::Map::new(),
    );
    assert!(matches!(done, Some(Ok(_))), "{done:?}");
    assert!(asked.await.is_none());
}

#[test]
fn arguments_outside_the_declared_ones_are_refused() {
    let questions = Questions::new();
    let _asked = questions.ask(question("portal:3"));
    let refused = invoke(
        &mut services(&questions),
        &question_resource("portal:3"),
        "answer",
        &arguments(json!({ "password": "hunter2" })),
    );
    assert!(matches!(refused, Some(Err(_))), "{refused:?}");
    assert_eq!(questions.pending().len(), 1, "still asked");
}

#[test]
fn an_address_nobody_publishes_is_none() {
    let questions = Questions::new();
    assert!(
        invoke(
            &mut services(&questions),
            &question_resource("gone"),
            "answer",
            &serde_json::Map::new(),
        )
        .is_none()
    );
}

#[test]
fn dropping_the_asker_withdraws_its_question_and_tells_the_session() {
    let questions = Questions::new();
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&changes);
    questions.on_change(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let asked = questions.ask(question("polkit:7"));
    assert_eq!(questions.pending().len(), 1);
    drop(asked);
    assert!(questions.pending().is_empty());
    assert_eq!(changes.load(Ordering::SeqCst), 2, "asked, then withdrawn");
}

#[tokio::test]
async fn asking_again_replaces_and_the_old_asker_only_withdraws_its_own() {
    let questions = Questions::new();
    let first = questions.ask(question("polkit:7"));
    let mut second = question("polkit:7");
    second.label = "retry".into();
    let second = questions.ask(second);
    assert!(first.await.is_none(), "the replaced asking ends");
    let pending = questions.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.first().map(|q| q.label.as_str()), Some("retry"));
    assert!(questions.answer("polkit:7", Answer::default()));
    assert!(second.await.is_some());
}

#[test]
fn an_answer_for_a_question_no_longer_asked_is_unknown() {
    let mut questions = Questions::new();
    let refused = questions.invoke(
        &question_resource("portal:9"),
        "cancel",
        &serde_json::Map::new(),
    );
    assert!(matches!(refused, Err(DoorError::Unknown(_))), "{refused:?}");
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
    let questions = Questions::new();
    let asked = questions.ask(question("q"));
    assert!(questions.update("q", serde_json::json!({ "choices": ["b"] })));
    assert_eq!(
        questions.question("q").map(|question| question.details),
        Some(serde_json::json!({ "choices": ["b"] }))
    );
    assert!(questions.answer("q", Answer::default()));
    assert!(asked.await.is_some());
    assert!(!questions.update("q", serde_json::Value::Null), "answered");
}
