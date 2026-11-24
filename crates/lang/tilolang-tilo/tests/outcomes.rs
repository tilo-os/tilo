//! Outcomes across replicas, on the real Todo app: checking an item records what the person saw
//! as the argument (`done: true`), so two people checking it at once both leave it checked, an
//! agent's explicit `done: false` wins, and history has the value.

use crate::fixtures::Workspace;
use serde_json::{Value as Json, json};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Meta, SourceWords, Value};
use tilolang_tilo::DynAction;
use tilolang_tilo::publish::{Caller, PlanRequest};

use crate::fixtures::{Replica, TASKS_SET, field_id};

const APP: &str = "so.tilo.tasks";

/// Ada's replica holding a list with one item, Grace's fork of it, the package, and the item.
fn workspace() -> (Replica, Replica, Meta, ResourceRef) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), TASKS_SET);
    let meta = packages.last().unwrap().meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    carried(
        &ada,
        &meta,
        &root,
        "Task",
        "new_task",
        json!({"title": "Take the bins out"}),
        None,
    );
    let items = field_id(&meta, "Tasks", "items");
    let first = match ada.read(&root, &items) {
        Value::List(items) => match items.first() {
            Some(Value::Ref(r)) => (*r.target).clone(),
            other => panic!("an item: {other:?}"),
        },
        other => panic!("no items: {other:?}"),
    };
    let grace = ada.fork("grace", TASKS_SET);
    (ada, grace, meta, first)
}

/// The kind the type `ty` is published as.
fn kind_of(ty: &str) -> String {
    format!("tl.{APP}.{ty}")
}

/// What the API plans for the action `name` on `on`, without carrying it out.
fn plan(
    replica: &Replica,
    meta: &Meta,
    on: &ResourceRef,
    ty: &str,
    name: &str,
    args: Json,
    caller: Option<&Caller>,
) -> DynAction {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("the action {name}"));
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = kind_of(ty);
    let id = on.id_string();
    replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &action.id,
            resource: Some(&id),
            args: &args,
            caller,
            from: None,
        })
        .unwrap()
}

/// Carry out the action `name` on `on` as the API does, and answer its record.
fn carried(
    replica: &Replica,
    meta: &Meta,
    on: &ResourceRef,
    ty: &str,
    name: &str,
    args: Json,
    caller: Option<&Caller>,
) -> DynAction {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("the action {name}"));
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = kind_of(ty);
    let id = on.id_string();
    let request = PlanRequest {
        app: APP,
        kind: &kind,
        action_id: &action.id,
        resource: Some(&id),
        args: &args,
        caller,
        from: None,
    };
    replica.carry_out(&request).unwrap()
}

fn done(replica: &Replica, item: &ResourceRef) -> Value {
    replica.host.runtime().read(item, "done").unwrap()
}

#[test]
fn two_replicas_checking_one_item_at_once_both_leave_it_checked() {
    let (ada, grace, meta, item) = workspace();
    let by_ada = plan(&ada, &meta, &item, "Task", "check", json!({}), None);
    let by_grace = plan(&grace, &meta, &item, "Task", "check", json!({}), None);
    assert_eq!(
        by_ada.args.get("done"),
        Some(&json!(true)),
        "{:?}",
        by_ada.args
    );
    assert_eq!(
        by_grace.args.get("done"),
        Some(&json!(true)),
        "{:?}",
        by_grace.args
    );
    carried(&ada, &meta, &item, "Task", "check", json!({}), None);
    carried(&grace, &meta, &item, "Task", "check", json!({}), None);
    ada.sync_with(&grace);
    assert_eq!(done(&ada, &item), Value::Bool(true));
    assert_eq!(done(&grace, &item), Value::Bool(true));
}

#[test]
fn an_agent_passing_done_explicitly_wins() {
    let (ada, _, meta, item) = workspace();
    let agent = Caller {
        actor: tilolang_tilo::runtime::ids::MemberId::agent("task-1").into(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
        app: None,
    };
    let unchecked = plan(
        &ada,
        &meta,
        &item,
        "Task",
        "check",
        json!({"done": false}),
        Some(&agent),
    );
    assert_eq!(unchecked.args.get("done"), Some(&json!(false)));
    assert_eq!(
        unchecked.authorship.invoker,
        Some(tilolang_tilo::runtime::Actor::from(
            tilolang_tilo::runtime::ids::MemberId::agent("task-1")
        ))
    );
    carried(
        &ada,
        &meta,
        &item,
        "Task",
        "check",
        json!({"done": false}),
        Some(&agent),
    );
    assert_eq!(
        done(&ada, &item),
        Value::Bool(false),
        "the default would have checked it"
    );
    carried(
        &ada,
        &meta,
        &item,
        "Task",
        "check",
        json!({"done": true}),
        Some(&agent),
    );
    carried(
        &ada,
        &meta,
        &item,
        "Task",
        "check",
        json!({"done": true}),
        Some(&agent),
    );
    assert_eq!(
        done(&ada, &item),
        Value::Bool(true),
        "checked twice is checked"
    );
}

#[test]
fn the_published_action_offers_done_as_optional_and_describes_its_default() {
    let (ada, _, meta, _) = workspace();
    let check = meta.actions.iter().find(|a| a.name == "check").unwrap();
    let done = check.params.iter().find(|p| p.name == "done").unwrap();
    assert_eq!(
        done.default.as_ref().map(|d| d.text.as_str()),
        Some("not closed(t.status)")
    );
    let planner: std::sync::Arc<dyn tilolang_tilo::publish::Planner> =
        std::sync::Arc::new(ada.host.handle());
    let ty = tilolang_tilo::publish::action_type(
        &meta,
        &format!("tl.{APP}.Task"),
        check,
        &planner,
        &SourceWords,
    );
    let spec = ty.bind(tilo_actions::Target::new(String::new()));
    let param = spec
        .params()
        .iter()
        .find(|p| p.name == "done")
        .expect("published");
    assert!(!param.required, "an invocation may leave it out");
    assert!(
        param.description.contains("not closed(t.status)"),
        "{}",
        param.description
    );
    assert!(
        matches!(spec.input(), tilo_actions::ActionInput::Immediate(None)),
        "a person choosing it is asked nothing"
    );
}
