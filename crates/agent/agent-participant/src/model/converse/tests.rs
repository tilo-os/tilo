//! The `plan` tool of a coordinated task, and the role its calls' usage
//! lines carry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use crate::planner::{PlanRole, Role};

/// Only a coordinated task's `plan` takes items as objects (their apps,
/// what they follow and wait for) and `adjust`, and says how in its
/// description; every other task's tools are byte-identical to before.
#[test]
fn only_a_coordinated_task_s_plan_takes_items_and_notes() {
    let uncoordinated = planning_tools(crate::model::MOST_STEPS, false, false, false);
    let mut coordinated = uncoordinated.clone();
    coordinated_tools(&mut coordinated);
    let plan = |tools: &[ToolDefinition]| {
        tools
            .iter()
            .find(|tool| tool.name == tilo_agent::tools::PLAN)
            .cloned()
            .unwrap()
    };
    let (before, after) = (plan(&uncoordinated), plan(&coordinated));
    assert!(before.input_schema["properties"].get("adjust").is_none());
    assert_eq!(
        before.input_schema["properties"]["obligations"]["items"]["type"],
        "string"
    );
    assert!(!before.description.contains(DECOMPOSE_NOTE));
    let items = &after.input_schema["properties"]["obligations"]["items"];
    assert_eq!(items["type"], "object");
    assert_eq!(items["required"], serde_json::json!(["text"]));
    assert_eq!(
        items["properties"]["wait_for"], before.input_schema["properties"]["wait_for"],
        "an item waits as a plan does"
    );
    assert_eq!(
        after.input_schema["properties"]["adjust"]["items"]["required"],
        serde_json::json!(["item", "note"])
    );
    assert!(after.description.ends_with(DECOMPOSE_NOTE));
    assert_eq!(
        after.input_schema["required"],
        before.input_schema["required"]
    );
}

/// A coordinator's plan of no steps that lists items is an answer without
/// a `reply`; an item's agent's still needs one, and the items, their plans
/// and the notes are read.
#[test]
fn a_coordinator_lists_items_without_a_reply() {
    let revealed = tilo_agent::tools::Revealed::new();
    let input = serde_json::json!({"steps": [], "obligations": [
            {"text": "post Alpha", "apps": ["chat", 3], "after": [], "wait_for": null},
            {"text": "wait for the code", "wait_for": {"app": "messages", "contains": "code",
                "seconds": 999}},
            "log it",
            {"apps": ["sheets"]}],
        "adjust": [{"item": 2, "note": " say Bravo "}, {"item": 0, "note": "x"}, {"item": 1}]});
    let coordinator = PlanContext {
        may_plan_nothing: true,
        role: Role::Coordinator {
            lists: true,
            note: String::new(),
        },
        ..PlanContext::default()
    };
    let answer = read_answer(&input, &revealed, 4, &coordinator).expect("listed");
    assert_eq!(
        answer.obligations,
        ["post Alpha", "wait for the code", "log it"]
    );
    assert_eq!(answer.items.len(), 3);
    assert_eq!(answer.items[0].apps, ["chat"]);
    let wait = answer.items[1].wait.clone().expect("a wait");
    assert_eq!(
        (wait.app.as_deref(), wait.seconds),
        (Some("messages"), tilo_agent::tools::MOST_WAIT_SECONDS)
    );
    assert_eq!(answer.items[2], tilo_agent::tools::ItemPlan::default());
    assert_eq!(answer.adjust, [(2, "say Bravo".to_owned())]);

    let item = PlanContext {
        may_plan_nothing: true,
        role: Role::Item {
            note: String::new(),
        },
        ..PlanContext::default()
    };
    assert!(read_answer(&input, &revealed, 4, &item).is_err());
    let strings = serde_json::json!({"steps": [], "reply": "Done.", "obligations": ["a", "b"]});
    let answer = read_answer(&strings, &revealed, 4, &item).expect("a plan");
    assert!(answer.items.is_empty(), "no item says how it is run");
}

/// A call made while a coordinated task's plan is made logs its role,
/// item and task id; an uncoordinated one logs neither, as before.
#[test]
fn a_usage_line_carries_the_role_it_was_made_for() {
    let answer: Result<ModelResponse, ModelError> = Err(ModelError::new(ErrorKind::Transport));
    let line = || {
        let mut line = usage_line(
            "m",
            Some("agent"),
            (Duration::from_millis(5), (0, 0)),
            &answer,
            Attempt::default(),
        );
        with_role(&mut line, plan_role().as_ref());
        line
    };
    assert!(line().get("role").is_none());
    let role = PlanRole {
        role: "item",
        item: Some(2),
        task: "t1/item-2".into(),
    };
    let logged = as_role(Some(role), line);
    assert_eq!(logged["role"], "item");
    assert_eq!(logged["item"], 2);
    assert_eq!(logged["task"], "t1/item-2");
    assert!(plan_role().is_none(), "the role ends with the plan");
}
