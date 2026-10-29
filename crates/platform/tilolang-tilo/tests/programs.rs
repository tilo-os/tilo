//! An agent's program on the door (`run-program`): its `let`s kept between programs, one step
//! that commits whole or not at all, and a preview before it changes many resources.

use automerge::AutoCommit;
use serde_json::Value as Json;
use tilo_action_policy::{RoomPolicy, Standing};
use tilolang_tilo::HostEffect;
use tilolang_tilo::publish::Caller;

use crate::fixtures::Replica;

const TODO: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TODO_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const APP: &str = "so.tilo.lang.todo";

fn agent() -> Caller {
    Caller {
        actor: "task-1".to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    }
}

fn ada() -> Replica {
    let standing = Standing::person(RoomPolicy::new("room", "ada"), "ada");
    let (mut ada, _) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(TODO, TODO_IDS)],
        Some(standing),
    );
    ada.host.root_of(APP).unwrap();
    ada.commit();
    ada
}

/// Run `source` as the agent's program; its answer, after what it committed went through.
fn run(replica: &mut Replica, source: &str, confirm: bool) -> Json {
    let output = replica
        .host
        .answer(
            HostEffect::RunProgram {
                source: source.to_owned(),
                session: None,
                confirm: Some(confirm),
                apps: None,
                reads: None,
            },
            Some(&agent()),
        )
        .unwrap();
    replica.commit();
    output.value.unwrap()
}

fn items(replica: &mut Replica) -> usize {
    let answer = run(replica, "todo.tasks.items.len()", false);
    answer["output"][0].as_str().unwrap().parse().unwrap()
}

#[test]
fn a_programs_lets_are_kept_for_the_next_program() {
    let mut ada = ada();
    let first = run(&mut ada, "let list = todo.tasks\nlist.items.len()", false);
    assert_eq!(first["state"], "done", "{first:#}");
    assert_eq!(first["output"], serde_json::json!(["0"]), "{first:#}");
    assert_eq!(first["lets"][0]["name"], "list", "{first:#}");
    let second = run(
        &mut ada,
        "todo.add(list, \"Take the bins out\")\nlist.items.len()",
        false,
    );
    assert_eq!(second["state"], "done", "{second:#}");
    assert_eq!(second["changed"], 1, "{second:#}");
    assert_eq!(items(&mut ada), 1);
}

#[test]
fn a_program_that_changes_many_resources_is_previewed_and_applied_on_confirm() {
    let mut ada = ada();
    let bulk = "for t in [\"a\", \"b\", \"c\", \"d\", \"e\"] {\n    todo.add(todo.tasks, t)\n}";
    let previewed = run(&mut ada, bulk, false);
    assert_eq!(previewed["state"], "preview", "{previewed:#}");
    let steps = previewed["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 5, "{previewed:#}");
    assert!(
        steps.iter().all(|step| step["action"] == "add"),
        "{previewed:#}"
    );
    assert_eq!(items(&mut ada), 0, "a preview changes nothing");
    let applied = run(&mut ada, bulk, true);
    assert_eq!(applied["state"], "done", "{applied:#}");
    assert_eq!(applied["changed"], 5, "{applied:#}");
    assert_eq!(items(&mut ada), 5);
}

#[test]
fn a_program_that_fails_changes_nothing() {
    let mut ada = ada();
    let failing = "todo.add(todo.tasks, \"milk\")\nprint(1 / (todo.tasks.items.len() - 1))";
    let answer = run(&mut ada, failing, false);
    assert_ne!(answer["state"], "done", "{answer:#}");
    assert_eq!(answer["changed"], 0, "{answer:#}");
    assert_eq!(
        items(&mut ada),
        0,
        "what it added before it failed is not kept"
    );
}
