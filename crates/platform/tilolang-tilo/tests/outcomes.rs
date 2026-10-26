//! Outcomes across replicas (convergence design 1.1), on the real Todo app: checking an item
//! records what the person saw as the argument (`done: true`), so two people checking it at once
//! both leave it checked, an agent's explicit `done: false` wins, and History has the value.

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilo_actions::{Did, Scalar};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{Meta, Value};
use tilolang_tilo::DynAction;
use tilolang_tilo::publish::{Caller, PlanRequest};
use tilolang_tilo::record::DynValue;

use crate::fixtures::{Replica, field_id};

const TODO: &str = include_str!("../../../../apps/tl/todo/app.tl");
const TODO_IDS: &str = include_str!("../../../../apps/tl/todo/app.ids");
const APP: &str = "so.tilo.lang.todo";

/// Ada's replica holding a list with one item, Grace's fork of it, the package, and the item's
/// resource id.
fn room() -> (Replica, Replica, Meta, String) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, TODO_IDS)]);
    let meta = packages[0].meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let add = plan(
        &ada,
        &meta,
        ("TodoList", &root.doc),
        "add",
        json!({"title": "Take the bins out"}),
        None,
    );
    ada.apply(&add);
    let items = field_id(&meta, "TodoList", "items");
    let record = ada.host.store().record(&root.doc).unwrap();
    let Some(DynValue::Collection(list)) = record.get(&items) else {
        panic!("no items");
    };
    let first = list
        .iter()
        .next()
        .map(|(id, _)| id.as_str().to_owned())
        .expect("one item");
    let item = format!("{}/{items}:{first}", root.doc);
    let grace = ada.fork("grace", &[(TODO, TODO_IDS)]);
    (ada, grace, meta, item)
}

/// What the door builds for the action `name` invoked on `on` (a type and a resource id).
fn plan(
    replica: &Replica,
    meta: &Meta,
    on: (&str, &str),
    name: &str,
    args: Json,
    caller: Option<&Caller>,
) -> DynAction {
    let action = meta.actions.iter().find(|a| a.name == name).unwrap();
    let args = args.as_object().cloned().unwrap_or_default();
    let kind = format!("tl.{APP}.{}", on.0);
    replica
        .host
        .plan(&PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &action.id,
            resource: Some(on.1),
            args: &args,
            caller,
        })
        .unwrap()
}

fn done(replica: &Replica, meta: &Meta, item: &str) -> Value {
    let ty = meta.types.iter().find(|t| t.name == "Todo").unwrap();
    let at = ResourceRef::parse_id(item, ty.id.clone()).unwrap();
    replica.host.runtime().read(&at, "done").unwrap()
}

#[test]
fn two_replicas_checking_one_item_at_once_both_leave_it_checked() {
    let (mut ada, mut grace, meta, item) = room();
    let by_ada = plan(&ada, &meta, ("Todo", &item), "check", json!({}), None);
    let by_grace = plan(&grace, &meta, ("Todo", &item), "check", json!({}), None);
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
    ada.apply(&by_ada);
    grace.apply(&by_grace);
    ada.sync_with(&mut grace);
    assert_eq!(done(&ada, &meta, &item), Value::Bool(true));
    assert_eq!(done(&grace, &meta, &item), Value::Bool(true));
    assert_eq!(
        ada.host
            .store()
            .record(item.split('/').next().unwrap())
            .unwrap(),
        grace
            .host
            .store()
            .record(item.split('/').next().unwrap())
            .unwrap()
    );
}

#[test]
fn an_agent_passing_done_explicitly_wins() {
    let (mut ada, _, meta, item) = room();
    let agent = Caller {
        actor: "task-1".to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    };
    let unchecked = plan(
        &ada,
        &meta,
        ("Todo", &item),
        "check",
        json!({"done": false}),
        Some(&agent),
    );
    assert_eq!(unchecked.args.get("done"), Some(&json!(false)));
    assert_eq!(unchecked.authorship.invoker, "task-1");
    ada.apply(&unchecked);
    assert_eq!(
        done(&ada, &meta, &item),
        Value::Bool(false),
        "the default would have checked it"
    );
    let checked = plan(
        &ada,
        &meta,
        ("Todo", &item),
        "check",
        json!({"done": true}),
        Some(&agent),
    );
    ada.apply(&checked);
    let again = plan(
        &ada,
        &meta,
        ("Todo", &item),
        "check",
        json!({"done": true}),
        Some(&agent),
    );
    ada.apply(&again);
    assert_eq!(
        done(&ada, &meta, &item),
        Value::Bool(true),
        "checked twice is checked"
    );
}

#[test]
fn the_recorded_invocation_and_history_carry_the_argument() {
    let (mut ada, _, meta, item) = room();
    let check = plan(&ada, &meta, ("Todo", &item), "check", json!({}), None);
    ada.apply(&check);
    let uncheck = plan(&ada, &meta, ("Todo", &item), "check", json!({}), None);
    assert_eq!(
        uncheck.args.get("done"),
        Some(&json!(false)),
        "computed on the state then"
    );
    ada.apply(&uncheck);
    let id = &meta.actions.iter().find(|a| a.name == "check").unwrap().id;
    let versions = tilo_resource::history::versions(&mut ada.doc);
    let recorded: Vec<Option<Scalar>> = versions
        .iter()
        .filter_map(|version| match version.context.as_ref()?.what.as_ref()? {
            Did::Verb(verb) if &verb.action.action == id => Some(verb.args.get("done").cloned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded,
        [
            Scalar::from_json(&json!(true)),
            Scalar::from_json(&json!(false))
        ],
        "{versions:#?}"
    );
}

#[test]
fn the_published_action_offers_done_as_optional_and_describes_its_default() {
    let (ada, _, meta, _) = room();
    let check = meta.actions.iter().find(|a| a.name == "check").unwrap();
    let done = check.params.iter().find(|p| p.name == "done").unwrap();
    assert_eq!(
        done.default.as_ref().map(|d| d.text.as_str()),
        Some("not item.done")
    );
    let planner: std::sync::Arc<dyn tilolang_tilo::publish::Planner> =
        std::sync::Arc::new(ada.host.handle());
    let ty = tilolang_tilo::publish::action_type(&meta, &format!("tl.{APP}.Todo"), check, &planner);
    let spec = ty.bind(tilo_actions::Target::new(String::new()));
    let param = spec
        .params()
        .iter()
        .find(|p| p.name == "done")
        .expect("published");
    assert!(!param.required, "an invocation may leave it out");
    assert!(
        param.description.contains("not item.done"),
        "{}",
        param.description
    );
    assert!(
        matches!(spec.input(), tilo_actions::ActionInput::Immediate(None)),
        "a person choosing it is asked nothing"
    );
}
