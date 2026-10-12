//! Natural-language tasks through `ModelPlanner`, with a deterministic
//! stand-in for the model that still goes through `apps/agent`'s discovery
//! tools: it must reveal an operation before its plan may name it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use common::KeywordModel;
use serde_json::json;
use tilo_agent::surface::Catalogue;
use tilo_agent::tools::Answer;
use tilo_agent_participant::{
    ModelPlanner, PlanContext, PlanModel, Planner, Replica, Seed, StepAction,
};
use tilo_browser_peer::DocumentRoute;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const SCRATCHPAD: &str = "so.tilo.scratchpad";
const NOTES: &str = "so.tilo.notes";

/// Answers with whatever it was built with, unchecked.
struct Canned(Answer);

impl PlanModel for Canned {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Ok(self.0.clone())
    }
}

fn context_with(app_id: &str) -> (PlanContext, DocumentRoute, Replica) {
    let catalogue = common::catalogue();
    let app = Replica::open(&catalogue, app_id, ROOM, "agent", true, Seed::New).unwrap();
    let route = DocumentRoute {
        app_id: app_id.into(),
        cluster_id: ROOM.into(),
        record: app.record().to_owned(),
    };
    let context = PlanContext {
        open: vec![(route.clone(), app.survey())],
        known: Vec::new(),
        apps: vec![SCRATCHPAD.into(), NOTES.into()],
    };
    (context, route, app)
}

fn context_with_scratchpad() -> (PlanContext, DocumentRoute, Replica) {
    context_with(SCRATCHPAD)
}

#[test]
fn a_natural_language_task_becomes_a_published_action_with_typed_arguments() {
    let (context, route, mut app) = context_with(NOTES);
    let mut planner = ModelPlanner::new(Box::new(KeywordModel));
    let steps = planner
        .plan(
            r#"Please add a new note titled "Hello from the model""#,
            &context,
        )
        .unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].record.as_deref(), Some(route.record.as_str()));
    let StepAction::Invoke { address, arguments } = &steps[0].action else {
        panic!("{:?}", steps[0]);
    };
    assert_eq!(address.action, "new-note");
    // The door accepts what the planner produced.
    app.invoke(address, arguments, false).unwrap();
    let body = app
        .read(|doc| serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap())
        .unwrap();
    assert!(body.contains("hello from the model"), "{body}");
}

#[test]
fn explicit_steps_still_bypass_the_model() {
    let (context, _, _app) = context_with_scratchpad();
    let mut planner = ModelPlanner::new(Box::new(Canned(Answer {
        actions: vec!["never#asked".into()],
        arguments: vec![serde_json::Map::new()],
        why: String::new(),
        then: None,
        reply: None,
    })));
    let steps = planner
        .plan(
            &json!({"app": SCRATCHPAD, "resource": "scratchpad.note", "action": "field-edit"})
                .to_string(),
            &context,
        )
        .unwrap();
    assert!(matches!(
        &steps[0].action,
        StepAction::Invoke { address, .. } if address.action == "field-edit"
    ));
}

#[test]
fn an_answer_outside_the_offers_is_refused_and_an_open_continues() {
    let (context, _, _app) = context_with_scratchpad();
    let mut invented = ModelPlanner::new(Box::new(Canned(Answer {
        actions: vec!["scratchpad.note#format-disk".into()],
        arguments: vec![serde_json::Map::new()],
        why: String::new(),
        then: None,
        reply: None,
    })));
    let refused = invented.plan("tidy up", &context).unwrap_err();
    assert!(refused.contains("not offered"), "{refused}");

    let mut opening = ModelPlanner::new(Box::new(Canned(Answer {
        actions: vec!["app.so.tilo.notes#open".into()],
        arguments: vec![serde_json::Map::new()],
        why: String::new(),
        then: Some("add a page called Plans".into()),
        reply: None,
    })));
    let steps = opening.plan("open notes and add a page", &context).unwrap();
    assert_eq!(steps[0].app, "so.tilo.notes");
    assert_eq!(steps[0].action, StepAction::Open);
    assert_eq!(
        steps[1].action,
        StepAction::Continue("add a page called Plans".into())
    );
}
