use serde_json::json;
use tilo_automation::{Binding, Step};
use ui::prelude::Json;
use ui::tilo_actions::{ActionParam, ParamType};

use super::*;
use crate::model::{Draft, Program};

fn step(value: serde_json::Value) -> Step {
    serde_json::from_value(value).unwrap()
}

fn draft(steps: Vec<Step>) -> Draft {
    Draft {
        name: "Digest".into(),
        shortcut_id: "digest".into(),
        program: Json(Program {
            inputs: vec![
                ActionParam::required("who", ParamType::Text, "Who"),
                ActionParam::required("count", ParamType::Integer, "How many"),
            ],
            steps,
            output: None,
            ..Program::default()
        }),
        replaces_saved: false,
    }
}

fn list() -> Step {
    step(
        json!({"id": "list", "step": "invoke", "app_id": "so.tilo.notes",
                "resource": {"fixed": "notes.folder:inbox"}, "action": "list"}),
    )
}

fn create(args: &serde_json::Value) -> Step {
    step(
        json!({"id": "create", "step": "invoke", "app_id": "so.tilo.notes",
                "resource": {"fixed": "notes.folder:inbox"}, "action": "create",
                "args": args}),
    )
}

#[test]
fn a_parameter_is_offered_only_type_compatible_values_in_scope() {
    let catalog = crate::samples::catalog();
    let draft = draft(vec![list(), create(&json!({}))]);
    let title = ActionParam::required("title", ParamType::Text, "Title");
    let offered: Vec<Binding> = choices(&draft, &catalog, "1", &title)
        .into_iter()
        .map(|choice| choice.binding)
        .collect();
    assert_eq!(
        offered,
        vec![Binding::Input("who".into())],
        "the integer input and the list's array are not text"
    );
    let pinned = ActionParam::required("pinned", ParamType::Boolean, "Pin");
    assert!(choices(&draft, &catalog, "1", &pinned).is_empty());
    let items = ActionParam::required("items", ParamType::Array, "Items");
    let arrays: Vec<Binding> = choices(&draft, &catalog, "1", &items)
        .into_iter()
        .map(|choice| choice.binding)
        .collect();
    assert_eq!(arrays, vec![Binding::step("list", "/titles")]);
    // The first step cannot use its own result.
    assert!(
        choices(&draft, &catalog, "0", &items).is_empty(),
        "nothing earlier than the first step"
    );
}

#[test]
fn problems_name_the_offending_step() {
    let catalog = crate::samples::catalog();
    let broken = draft(vec![
        list(),
        create(&json!({"title": {"step_output": {"step": "list", "pointer": "/titles"}}})),
    ]);
    let found = problems(&broken, &catalog, &[]);
    let at_create = found.get("1").expect("the create step is wrong");
    assert!(
        at_create.iter().any(|reason| reason.contains("title")),
        "{found:?}"
    );
    assert!(
        at_create.iter().any(|reason| reason.contains("pinned")),
        "the required `pinned` is unbound: {found:?}"
    );
    assert!(!found.contains_key("0"), "the list step is fine: {found:?}");

    let fixed = draft(vec![
        list(),
        create(&json!({
            "title": {"input": "who"},
            "pinned": {"literal": false}
        })),
    ]);
    assert!(problems(&fixed, &catalog, &[]).is_empty());
}

#[test]
fn the_proposed_grant_is_the_least_each_step_needs() {
    let catalog = crate::samples::catalog();
    let steps = vec![
        list(),
        create(&json!({})),
        step(json!({"id": "ai", "step": "ai", "transform": "summarise",
                    "input": {"literal": "x"}})),
        list(),
    ];
    let permissions = grant::least_privilege(&steps, &catalog);
    let named: Vec<(String, String, collab_types::Verb)> = permissions
        .iter()
        .map(|permission| {
            (
                permission.app_id.clone(),
                permission.action.clone(),
                permission.verb,
            )
        })
        .collect();
    assert_eq!(
        named,
        vec![
            (
                "so.tilo.notes".into(),
                "list".into(),
                collab_types::Verb::Read
            ),
            (
                "so.tilo.notes".into(),
                "create".into(),
                collab_types::Verb::Write
            ),
            (
                tilo_automation::AGENT_APP_ID.into(),
                "summarise".into(),
                collab_types::Verb::Read
            ),
        ]
    );
    // The draft checks against exactly that grant.
    let draft = draft(vec![
        list(),
        create(&json!({"title": {"input": "who"}, "pinned": {"literal": true}})),
    ]);
    assert!(problems(&draft, &catalog, &[]).is_empty());
    assert_eq!(candidate(&draft, &catalog).grant.len(), 2);
}

#[test]
fn typed_text_becomes_a_typed_literal() {
    let param = |ty| ActionParam::required("p", ty, "p");
    assert_eq!(literal("42", &param(ParamType::Integer)), json!(42));
    assert_eq!(literal("4.5", &param(ParamType::Number)), json!(4.5));
    assert_eq!(literal("yes", &param(ParamType::Boolean)), json!(true));
    assert_eq!(literal("[1, 2]", &param(ParamType::Array)), json!([1, 2]));
    assert_eq!(literal(" hi ", &param(ParamType::Text)), json!(" hi "));
    // Not a number stays text, so the checker names the problem.
    assert_eq!(literal("many", &param(ParamType::Integer)), json!("many"));
}

#[test]
fn the_picker_groups_the_catalogue_by_app_then_kind() {
    let groups = picker(&crate::samples::catalog());
    let apps: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
    // Ordered by application id.
    assert_eq!(apps, ["Agent", "Mail", "Notes", "Shortcuts"]);
    let notes = groups
        .iter()
        .find(|group| group.app_id == "so.tilo.notes")
        .unwrap();
    assert_eq!(notes.kinds.len(), 1);
    let create = notes.kinds[0]
        .offers
        .iter()
        .find(|offer| offer.action == "create")
        .unwrap();
    assert_eq!(create.params, ["title: text", "pinned: boolean"]);
    assert_eq!(app_label("so.tilo.speed-test"), "Speed Test");
}
