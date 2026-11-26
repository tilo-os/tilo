//! An agent suggests an action on a task: the call asks Ada even where the agent could run it,
//! and waits as its asked record until she answers it on the system's confirm. The task shows
//! what waits on it and who suggested it; her yes runs it once, her no changes nothing.

use crate::fixtures::Workspace;
use tilolang_runtime::{
    Actor, AgentBlock, InvocationEntry, InvokeOutcome, ResourceRef, Store as _, Value,
};
use tilolang_tilo::publish::Caller;

use crate::fixtures::{Replica, TASKS_SET, target};

const APP: &str = "so.tilo.tasks";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    r.call(APP, action, args)
}

/// Ada's replica with a task and a project she filed it under, and the task.
fn workspace() -> (Replica, Value) {
    let (ada, _) = Replica::new("ada", Workspace::new(), TASKS_SET);
    ada.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    let task = call(&ada, "new_task", crate::tasks::create(&root, "van"));
    let project = call(&ada, "new_project", vec![root]);
    call(
        &ada,
        "rename",
        vec![project.clone(), Value::text("Move flat")],
    );
    call(&ada, "file", vec![task.clone(), project]);
    ada.commit();
    (ada, task)
}

fn agent_actor(who: &str) -> Actor {
    tilolang_tilo::runtime::ids::MemberId::agent(who, "ada").into()
}

fn agent(who: &str) -> Caller {
    Caller {
        actor: agent_actor(who),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
        handed: false,
        app: None,
    }
}

fn read(r: &Replica, item: &Value, field: &str) -> Value {
    r.host.runtime().read(&target(item), field).unwrap()
}

/// The agent's block suggesting that the task be scheduled for the Saturday after next.
fn schedule_source(task: &Value) -> String {
    format!(
        "suggest tasks.schedule(@tasks.Task:{}, due: .some(2026-10-31))",
        target(task).id_string()
    )
}

/// Run `source` as the agent `who` in the workspace: the invocation record it left.
fn suggest(ada: &Replica, who: &str, source: &str) -> (ResourceRef, InvocationEntry) {
    ada.host.admit_agent(&agent(who));
    match ada
        .host
        .runtime()
        .run_agent_block(&agent_actor(who), source)
        .unwrap()
    {
        AgentBlock::Invocation { record, resource } => (resource, *record),
        other => panic!("not an invocation: {other:?}"),
    }
}

/// Ada approves the call `record` names on the system's confirm: the dispatch hands it back for
/// the agent that made it, and its answer comes back to the call.
fn approve(ada: &Replica, record: &InvocationEntry, task: &Value) {
    let ran = ada
        .host
        .runtime()
        .invoke_approved_through(
            &record.author,
            None,
            (record.action.as_str(), record.arguments().unwrap()),
            None,
        )
        .unwrap();
    assert!(matches!(ran, InvokeOutcome::Done(_)), "{ran:?}");
    answered(ada, record, task, "done");
}

fn answered(ada: &Replica, record: &InvocationEntry, task: &Value, state: &str) {
    ada.host
        .runtime()
        .call_answered(&record.run, &[target(task)], state)
        .unwrap();
    ada.commit();
}

tilo_test_env::scenario! {
    fn a_suggestion_waits_for_its_person;

    phase a_suggestion_asks_its_person_and_runs_once_on_their_yes;
    {
        let (ada, task) = workspace();
        let (_, record) = suggest(&ada, "scout", &schedule_source(&task));
        // The call went to the dispatch asking Ada, which keeps it: the record names it.
        assert_eq!(record.state, "asked");
        assert_eq!(record.run, "call-1");
        assert_eq!(
            record.target.as_ref().map(ResourceRef::id_string),
            Some(target(&task).id_string())
        );
        ada.commit();
        // Nothing has changed: the task still has no day.
        assert_eq!(read(&ada, &task, "due"), Value::none());
        approve(&ada, &record, &task);
        assert_eq!(
            read(&ada, &task, "due"),
            Value::some(Value::text("2026-10-31"))
        );
        assert!(ada.host.runtime().proposals_of(&target(&task)).is_empty());
    }

    phase a_declined_suggestion_changes_nothing;
    {
        let (ada, task) = workspace();
        let (inv, record) = suggest(&ada, "scout", &schedule_source(&task));
        ada.commit();
        let before = ada.host.store().versions(&target(&task)).unwrap().len();
        answered(&ada, &record, &task, "declined");
        assert_eq!(read(&ada, &task, "due"), Value::none());
        // The refusal is the record's, not a change of the task.
        assert_eq!(
            ada.host.store().versions(&target(&task)).unwrap().len(),
            before
        );
        assert_eq!(
            ada.host.runtime().read_invocation(&inv).unwrap().state,
            "declined"
        );
        assert!(ada.host.runtime().proposals_of(&target(&task)).is_empty());
    }

    phase two_agents_suggest_on_one_task_and_each_waits_for_its_own_answer;
    {
        let (ada, task) = workspace();
        let priority = read(&ada, &task, "priority");
        let (_, due) = suggest(&ada, "scout", &schedule_source(&task));
        let (_, high) = suggest(
            &ada,
            "helper",
            &format!(
                "suggest tasks.set_priority_high(@tasks.Task:{})",
                target(&task).id_string()
            ),
        );
        ada.commit();
        assert_ne!(due.run, high.run, "each is a call of its own");
        // Both wait on the task, each naming who suggested it.
        let mut who: Vec<String> = ada
            .host
            .runtime()
            .proposals_of(&target(&task))
            .iter()
            .map(|(_, rec)| rec.author.id().to_owned())
            .collect();
        who.sort();
        assert_eq!(who, ["helper".to_owned(), "scout".to_owned()]);
        // Approving one runs only it; the other still waits.
        approve(&ada, &due, &task);
        assert_eq!(
            read(&ada, &task, "due"),
            Value::some(Value::text("2026-10-31"))
        );
        assert_eq!(ada.host.runtime().proposals_of(&target(&task)).len(), 1);
        answered(&ada, &high, &task, "declined");
        assert_eq!(
            read(&ada, &task, "priority"),
            priority,
            "the declined suggestion changed nothing"
        );
        assert!(ada.host.runtime().proposals_of(&target(&task)).is_empty());
    }

    phase the_task_shows_what_waits_on_it_and_who_suggested_it;
    {
        let (ada, task) = workspace();
        // The task page and the row draw nothing while nothing waits.
        let page = ada.host.open_resource(&target(&task), None).unwrap();
        assert!(cards(page.tree()).is_empty(), "{:?}", texts(page.tree()));
        assert!(rows_with_note(card_view(&ada, &task).tree()).is_empty());
        let (inv, record) = suggest(&ada, "scout", &schedule_source(&task));
        ada.commit();
        let page = ada.host.open_resource(&target(&task), None).unwrap();
        let shown = cards(page.tree());
        assert_eq!(shown.len(), 1, "{:?}", texts(page.tree()));
        let card = shown[0];
        assert_eq!(card.props.get("state"), Some(&Value::text("asked")));
        assert_eq!(card.props.get("invocation"), Some(&Value::resource(&inv)));
        // The card offers no answer of its own: Ada answers on the system's confirm.
        assert_eq!(card.props.get("can_resolve"), None);
        // The card says what the change would be, and who suggested it: the words a change to
        // the field reads with, the value as its type shows it, and the agent the host draws
        // the face and the name from.
        let said = texts(page.tree());
        assert!(
            said.iter().any(|text| text == "set the due date to"),
            "what it would do: {said:?}"
        );
        assert!(
            said.iter().any(|text| text == "2026-10-31"),
            "the day it would set: {said:?}"
        );
        assert_eq!(
            card.props.get("author"),
            Some(&Value::Actor(agent_actor("scout"))),
            "who suggests it"
        );
        // The row of the task says it quietly, on a line of its own, in the same sentence.
        let rows = rows_with_note(card_view(&ada, &task).tree());
        assert_eq!(rows.len(), 1, "{:?}", texts(card_view(&ada, &task).tree()));
        assert!(
            rows[0].contains("suggests: set the due date to"),
            "{}",
            rows[0]
        );
        assert!(!rows[0].contains("schedule"), "{}", rows[0]);
        // Answered, the card and the row's line go away.
        approve(&ada, &record, &task);
        let page = ada.host.open_resource(&target(&task), None).unwrap();
        assert!(cards(page.tree()).is_empty());
        assert!(rows_with_note(card_view(&ada, &task).tree()).is_empty());
    }

    phase the_scenario_of_an_agents_suggestion_stages_one_waiting;
    {
        // A suggestion inside an action asks once the action has committed, as the agent the
        // scenario names.
        let (ada, loaded) = Replica::new("ada", Workspace::new(), TASKS_SET);
        ada.host.root_of(APP).unwrap();
        ada.commit();
        let scenario = loaded
            .last()
            .unwrap()
            .meta
            .scenarios
            .iter()
            .find(|scenario| scenario.name == "an agent's suggestion")
            .expect("Tasks stages an agent's suggestion")
            .action
            .clone();
        let Ok(InvokeOutcome::Done(task)) = ada.host.runtime().invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.as_str(), vec![]),
        ) else {
            panic!("the scenario stages a task");
        };
        ada.commit();
        let waiting = ada.host.runtime().proposals_of(&target(&task));
        assert_eq!(waiting.len(), 1, "{waiting:?}");
        assert_eq!(waiting[0].1.author.id(), "scenario:Scout");
        assert_eq!(waiting[0].1.run, "call-1", "it asked through the dispatch");
        assert_eq!(read(&ada, &task, "due"), Value::none(), "nothing changed yet");
    }
}

/// The project the task stands under, as its card's view takes it: the row a board draws leaves
/// out the project the board is of.
fn project_of(ada: &Replica, task: &Value) -> Value {
    match read(ada, task, "project") {
        Value::Opt(Some(held)) => *held,
        other => panic!("a filed task stands under a project: {other:?}"),
    }
}

/// The task's card view, which draws its row.
fn card_view(ada: &Replica, task: &Value) -> tilolang_runtime::ViewInstance {
    ada.host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &ada.host
                .package(APP)
                .unwrap()
                .meta
                .views
                .iter()
                .find(|v| v.name == "task_card")
                .unwrap()
                .id,
            vec![task.clone(), project_of(ada, task)],
        )
        .unwrap()
}

fn cards(tree: &tilolang_runtime::UiNode) -> Vec<&tilolang_runtime::UiNode> {
    tree.of_kind("invocation")
        .into_iter()
        .filter(|node| node.props.get("proposed") == Some(&Value::Bool(true)))
        .collect()
}

/// The lines the rows carry under their text: a row says nothing of its own when the line is
/// empty.
fn rows_with_note(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    tree.of_kind("ui.ListRow")
        .into_iter()
        .filter_map(|row| match row.props.get("note") {
            Some(Value::Text(note)) if !note.is_empty() => Some(note.clone()),
            _ => None,
        })
        .collect()
}

fn texts(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    tree.of_kind("ui.Text")
        .into_iter()
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}
