//! An agent proposes an action on a task instead of applying it: the proposal waits on the task
//! for whoever sees it, changes nothing, and once a person accepts it the change is theirs and
//! says who proposed it.

use automerge::AutoCommit;
use tilolang_runtime::store::Version;
use tilolang_runtime::{
    AgentBlock, InvocationRecord, InvokeOutcome, Principal, Resolution, ResourceRef, Store as _,
    Value,
};
use tilolang_tilo::publish::Caller;

use crate::fixtures::Replica;

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const APP: &str = "so.tilo.lang.tasks";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// Ada's replica with a task and a project she filed it under, and the task.
fn workspace() -> (Replica, Value) {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)],
    );
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

fn agent(who: &str) -> Caller {
    Caller {
        actor: who.to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    }
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

fn read(r: &Replica, item: &Value, field: &str) -> Value {
    r.host.runtime().read(&target(item), field).unwrap()
}

/// The agent's block proposing that the task be scheduled for the Saturday after next.
fn schedule_source(task: &Value) -> String {
    format!(
        "suggest tasks.schedule(@tasks.Task:{}, due: .some(2026-10-31))",
        target(task).id_string()
    )
}

/// Run `source` as the agent `who` in the workspace, answering the recorded proposal.
fn suggest(ada: &Replica, who: &str, source: &str) -> (ResourceRef, InvocationRecord) {
    ada.host.admit_agent(&agent(who));
    match ada
        .host
        .runtime()
        .run_agent_block(&Principal::new(who), source)
        .unwrap()
    {
        AgentBlock::Invocation { record, resource } => (resource, *record),
        other => panic!("not a proposal: {other:?}"),
    }
}

/// Who proposed the versions of the task.
fn proposed(ada: &Replica, task: &Value) -> Vec<String> {
    ada.host
        .store()
        .versions(&target(task))
        .unwrap()
        .into_iter()
        .filter_map(|v: Version| v.suggested_by)
        .collect()
}

#[test]
fn a_proposed_change_waits_until_a_person_accepts_it() {
    let (mut ada, task) = workspace();
    let (inv, record) = suggest(&ada, "agent:scout", &schedule_source(&task));
    assert_eq!(record.state, "suggested");
    assert_eq!(
        record.subject.as_ref().map(ResourceRef::id_string),
        Some(target(&task).id_string())
    );
    ada.commit();
    // Nothing has changed: the task still has no day.
    assert_eq!(read(&ada, &task, "due"), Value::none());
    assert!(proposed(&ada, &task).is_empty());
    // Accepting it applies the change as Ada's, saying who proposed it.
    assert_eq!(
        ada.host
            .runtime()
            .resolve_invocation(&inv, &Principal::new("ada"), Resolution::Approve)
            .unwrap(),
        "done"
    );
    ada.commit();
    assert_eq!(
        read(&ada, &task, "due"),
        Value::some(Value::text("2026-10-31"))
    );
    assert_eq!(proposed(&ada, &task), ["agent:scout".to_owned()]);
    let due = ada
        .host
        .store()
        .versions(&target(&task))
        .unwrap()
        .into_iter()
        .rfind(|v| v.suggested_by.is_some())
        .unwrap();
    assert_eq!(due.who, "ada");
}

#[test]
fn a_dismissed_proposal_changes_nothing_and_asks_once() {
    let (mut ada, task) = workspace();
    let (inv, _) = suggest(&ada, "agent:scout", &schedule_source(&task));
    ada.commit();
    let before = ada.host.store().versions(&target(&task)).unwrap().len();
    assert_eq!(
        ada.host
            .runtime()
            .resolve_invocation(&inv, &Principal::new("ada"), Resolution::Decline)
            .unwrap(),
        "declined"
    );
    ada.commit();
    assert_eq!(read(&ada, &task, "due"), Value::none());
    // The refusal is a record of its own, not a change of the task.
    assert_eq!(
        ada.host.store().versions(&target(&task)).unwrap().len(),
        before
    );
    assert_eq!(
        ada.host.runtime().read_invocation(&inv).unwrap().state,
        "declined"
    );
    assert!(proposed(&ada, &task).is_empty());
    // Decided already: a second decision is refused.
    assert!(
        ada.host
            .runtime()
            .resolve_invocation(&inv, &Principal::new("ada"), Resolution::Approve)
            .is_err()
    );
}

#[test]
fn two_agents_propose_on_one_task_and_each_card_waits_for_its_own_decision() {
    let (mut ada, task) = workspace();
    let priority = read(&ada, &task, "priority");
    let due = suggest(&ada, "agent:scout", &schedule_source(&task));
    let assign = suggest(
        &ada,
        "agent:helper",
        &format!(
            "suggest tasks.set_priority_high(@tasks.Task:{})",
            target(&task).id_string()
        ),
    );
    ada.commit();
    // Both wait on the task, each naming who proposed it.
    let waiting = ada.host.runtime().proposals_of(&target(&task));
    let mut who: Vec<String> = waiting
        .iter()
        .map(|(_, rec)| rec.author.0.clone())
        .collect();
    who.sort();
    assert_eq!(who, ["agent:helper".to_owned(), "agent:scout".to_owned()]);
    // Accepting one applies only it; the other still waits.
    assert_eq!(
        ada.host
            .runtime()
            .resolve_invocation(&due.0, &Principal::new("ada"), Resolution::Approve)
            .unwrap(),
        "done"
    );
    ada.commit();
    assert_eq!(
        read(&ada, &task, "due"),
        Value::some(Value::text("2026-10-31"))
    );
    assert_eq!(
        ada.host.runtime().proposals_of(&target(&task)).len(),
        1,
        "the other proposal still waits"
    );
    // A proposal of nobody changes nothing when dismissed.
    assert_eq!(
        ada.host
            .runtime()
            .resolve_invocation(&assign.0, &Principal::new("ada"), Resolution::Decline)
            .unwrap(),
        "declined"
    );
    ada.commit();
    assert_eq!(
        read(&ada, &task, "priority"),
        priority,
        "the dismissed proposal changed nothing"
    );
    assert!(ada.host.runtime().proposals_of(&target(&task)).is_empty());
    assert_eq!(proposed(&ada, &task), ["agent:scout".to_owned()]);
}

#[test]
fn the_task_shows_what_waits_on_it_and_who_proposed_it() {
    let (mut ada, task) = workspace();
    // The task page and the row draw nothing while nothing waits.
    let page = ada.host.open_resource(&target(&task), None).unwrap();
    assert!(cards(page.tree()).is_empty(), "{:?}", texts(page.tree()));
    assert!(rows_with_note(card_view(&ada, &task).tree()).is_empty());
    let (inv, _) = suggest(&ada, "agent:scout", &schedule_source(&task));
    ada.commit();
    let page = ada.host.open_resource(&target(&task), None).unwrap();
    let shown = cards(page.tree());
    assert_eq!(shown.len(), 1, "{:?}", texts(page.tree()));
    let card = shown[0];
    assert_eq!(card.props.get("proposed"), Some(&Value::Bool(true)));
    assert_eq!(
        card.props.get("state"),
        Some(&Value::text("suggested")),
        "the card asks"
    );
    // The card says which record deciding it resolves, and that Ada may decide: what the
    // window's accept and dismiss buttons answer with.
    assert_eq!(card.props.get("invocation"), Some(&Value::resource(&inv)));
    assert_eq!(card.props.get("can_resolve"), Some(&Value::Bool(true)));
    // The card says what the change would be, and who proposed it: the words a change to the
    // field reads with, the value as its type shows it, and the proposer the host draws the
    // face and the name from.
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
        Some(&Value::Principal(Principal::new("agent:scout"))),
        "who proposes it"
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
    // Decided, the card and the row's line go away.
    ada.host
        .runtime()
        .resolve_invocation(&inv, &Principal::new("ada"), Resolution::Approve)
        .unwrap();
    ada.commit();
    let page = ada.host.open_resource(&target(&task), None).unwrap();
    assert!(cards(page.tree()).is_empty());
    assert!(rows_with_note(card_view(&ada, &task).tree()).is_empty());
    assert_eq!(
        read(&ada, &task, "due"),
        Value::some(Value::text("2026-10-31"))
    );
}

/// The task's card view, which draws its row.
fn card_view(ada: &Replica, task: &Value) -> tilolang_runtime::ViewInstance {
    ada.host
        .runtime()
        .open_view(
            &Principal::new("ada"),
            &ada.host
                .package(APP)
                .unwrap()
                .meta
                .views
                .iter()
                .find(|v| v.name == "task_card")
                .unwrap()
                .id,
            vec![task.clone(), Value::List(Vec::new())],
        )
        .unwrap()
}

/// Every node of a tree of `kind`, depth first.
fn of_kind<'a>(
    node: &'a tilolang_runtime::UiNode,
    kind: &str,
) -> Vec<&'a tilolang_runtime::UiNode> {
    let mut out = Vec::new();
    walk(node, kind, &mut out);
    out
}

fn walk<'a>(
    node: &'a tilolang_runtime::UiNode,
    kind: &str,
    out: &mut Vec<&'a tilolang_runtime::UiNode>,
) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        walk(child, kind, out);
    }
}

fn cards(tree: &tilolang_runtime::UiNode) -> Vec<&tilolang_runtime::UiNode> {
    of_kind(tree, "invocation")
        .into_iter()
        .filter(|node| node.props.get("proposed") == Some(&Value::Bool(true)))
        .collect()
}

/// The lines the rows carry under their text: a row says nothing of its own when the line is
/// empty.
fn rows_with_note(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    of_kind(tree, "ui.ListRow")
        .into_iter()
        .filter_map(|row| match row.props.get("note") {
            Some(Value::Text(note)) if !note.is_empty() => Some(note.clone()),
            _ => None,
        })
        .collect()
}

fn texts(tree: &tilolang_runtime::UiNode) -> Vec<String> {
    of_kind(tree, "ui.Text")
        .into_iter()
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}
