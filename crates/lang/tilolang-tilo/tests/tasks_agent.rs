//! Giving a task to an agent asks it: one ask made from the task, by whoever gave it, saying
//! the task's title when the agent may read the task. Giving it to a person asks nothing.

use tilolang_runtime::Value;
use tilolang_tilo::runtime::Actor;
use tilolang_tilo::runtime::ids::MemberId;

use crate::mention_asks::asks;
use crate::tasks::{call, create, target, workspace};

fn give(task: &Value, to: &MemberId) -> Vec<Value> {
    vec![
        task.clone(),
        Value::Opt(Some(Box::new(Value::Actor(Actor::from(to.clone()))))),
    ]
}

#[test]
fn giving_a_task_to_an_agent_asks_it_from_the_task() {
    let (ada, root) = workspace();
    let helper = MemberId::agent("helper");
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_action_policy::Role::Owner),
            tilolang_runtime::members::Member::person("bo", tilo_action_policy::Role::Editor),
            tilolang_runtime::members::Member::agent("helper"),
        ],
        None,
    );
    let task = call(&ada, "new_task", create(&root, "Plan the launch"));
    while ada.commit() > 0 {}

    call(&ada, "assign", give(&task, &MemberId::person("bo")));
    while ada.commit() > 0 {}
    assert!(asks(&ada).is_empty(), "a person is asked nothing");

    call(&ada, "assign", give(&task, &helper));
    while ada.commit() > 0 {}
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "one ask: {asked:?}");
    let ask = &asked[0];
    assert_eq!(ask.agent, Actor::from(helper));
    assert_eq!(ask.said.as_deref(), Some("Plan the launch"));
    assert_eq!(ask.from, target(&task), "the ask is made from the task");
    assert_eq!(
        ask.who,
        Some(Actor::from(MemberId::person("ada"))),
        "the one who gave the task asked"
    );
}
