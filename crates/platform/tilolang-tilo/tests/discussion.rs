//! A discussion under a task and under a note: a comment made from what it is about, the thread
//! that draws under it, what the comment changes for whoever made or read what it is about
//! (their row, the window's count, the line in the thread), an agent's comment, and what
//! `linked` leaves out because the comment is about the task rather than made from it.

use automerge::AutoCommit;
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::store::{LinkHow, Store as _};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{ActionRef, InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const DISCUSSION_APP: &str = "so.tilo.lang.discussion";
const NOTES_APP: &str = "so.tilo.lang.notes";
const TASKS_APP: &str = "so.tilo.lang.tasks";

fn action(r: &Replica, app: &str, name: &str) -> String {
    r.host
        .package(app)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .unwrap_or_else(|| panic!("no action {name}"))
        .id
        .clone()
}

/// What `person` does when they run `action` of `app` with `args`, made from `from` when it is
/// made from a resource.
fn call(
    r: &Replica,
    person: &str,
    app: &str,
    action_name: &str,
    from: Option<&ResourceRef>,
    args: Vec<Value>,
) -> Value {
    let aref = ActionRef::new(action(r, app, action_name), args).made_from(from);
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(person), aref)
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action_name}: {other:?}"),
    }
}

fn target(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// What `person` says about `about`: the discussion app's own comment action, made from what it
/// is about, as the thread's field runs it.
fn comment(r: &Replica, person: &str, about: &ResourceRef, said: &str) -> ResourceRef {
    let all = Value::resource(&r.host.root_of(DISCUSSION_APP).unwrap());
    target(&call(
        r,
        person,
        DISCUSSION_APP,
        "comment",
        Some(about),
        vec![all, Value::Rich(RichValue::from_text(said))],
    ))
}

/// Every node of the tree, in the order it stands.
fn nodes<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        nodes(child, out);
    }
}

fn of_kind<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    nodes(node, &mut out);
    out.into_iter().filter(|node| node.kind == kind).collect()
}

/// The resource the `prop` of `node` names.
fn ref_of(node: &UiNode, prop: &str) -> Option<ResourceRef> {
    match node.props.get(prop) {
        Some(Value::Ref(r)) => Some((*r.target).clone()),
        _ => None,
    }
}

/// The rich text the comments of `tree` draw, in the order the thread reads them: a comment is
/// shown as the value it holds, not bound to a field like the page's own body.
fn comments_drawn(tree: &UiNode) -> Vec<RichValue> {
    of_kind(tree, "ui.BlockEditor")
        .into_iter()
        .filter_map(|editor| match editor.props.get("value") {
            Some(Value::Rich(rich)) => Some(rich.clone()),
            _ => None,
        })
        .collect()
}

/// The change mark the host puts on the window's header: how many changes of what it shows the
/// person has not seen, and who made the newest.
fn window_mark(view: &mut ViewInstance) -> Option<UiNode> {
    view.poll_patches().unwrap();
    of_kind(view.tree(), "ui.ChangeMark")
        .into_iter()
        .find(|mark| mark.props.get("window") == Some(&Value::Bool(true)))
        .map(|mark| (*mark).clone())
}

/// The rows the window's list draws, each as the resource it presents, and the change mark the
/// host placed on it when it carries one.
fn rows(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    view.poll_patches().unwrap();
    let found = of_kind(view.tree(), "ui.VirtualList");
    let Some(list) = found.first() else {
        return Vec::new();
    };
    let list = list.key.clone();
    let len = view.list_len(&list).unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..len {
        let Ok(Some(row)) = view.row(&list, i) else {
            continue;
        };
        for item in of_kind(row, "ui.ListItem") {
            let Some(Value::Ref(r)) = item.props.get("item") else {
                continue;
            };
            let mark = of_kind(item, "ui.ChangeMark")
                .first()
                .map(|mark| (*mark).clone());
            out.push((r.target.id_string(), mark));
        }
    }
    out
}

fn mark_of(view: &mut ViewInstance, resource: &str) -> Option<UiNode> {
    rows(view)
        .into_iter()
        .find(|(at, _)| at == resource)
        .unwrap_or_else(|| panic!("no row for {resource}"))
        .1
}

/// Ada's replication of a workspace that has the discussion app, notes and tasks.
fn workspace() -> Replica {
    let (replica, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (DISCUSSION, DISCUSSION_IDS),
            (NOTES, NOTES_IDS),
            (TASKS, TASKS_IDS),
        ],
    );
    replica
}

/// The task `title` of Ada's workspace.
fn page(r: &Replica, app: &str, action_name: &str, title: &str) -> ResourceRef {
    let root = Value::resource(&r.host.root_of(app).unwrap());
    target(&call(
        r,
        "ada",
        app,
        action_name,
        None,
        vec![root, Value::text(title)],
    ))
}

/// The note `title` of Ada's workspace: notes start untitled, so it is named after it is made.
fn note_named(r: &Replica, title: &str) -> ResourceRef {
    let root = Value::resource(&r.host.root_of(NOTES_APP).unwrap());
    let note = target(&call(r, "ada", NOTES_APP, "new_note", None, vec![root]));
    call(
        r,
        "ada",
        NOTES_APP,
        "retitle",
        None,
        vec![Value::resource(&note), Value::text(title)],
    );
    note
}

#[test]
fn a_comment_is_made_from_the_task_it_is_about_and_changes_it() {
    let mut ada = workspace();
    let task = page(&ada, TASKS_APP, "new_task", "Pay rent");
    ada.commit();
    let mut bo = ada.fork("bo", &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)]);

    // Bo asks Ada something about the task, naming her in it.
    let mut asked = RichValue::from_text("Is this the one that pays the whole quarter? ");
    asked.push_reference("Ada", &Reference::Person("ada".to_owned()));
    let asked_text = asked.written_text();
    let all = Value::resource(&bo.host.root_of(DISCUSSION_APP).unwrap());
    let said = target(&call(
        &bo,
        "bo",
        DISCUSSION_APP,
        "comment",
        Some(&task),
        vec![all, Value::Rich(asked)],
    ));
    bo.commit();
    ada.sync_with(&mut bo);

    // What it was made from is a fact of the change that made it.
    assert_eq!(
        ada.host
            .store()
            .version_end(&said, false)
            .unwrap()
            .unwrap()
            .from
            .map(|r| r.id_string()),
        Some(task.id_string()),
        "the comment is made from the task"
    );
    assert_eq!(
        ada.host
            .store()
            .linked(&task)
            .unwrap()
            .into_iter()
            .map(|link| (link.of.id_string(), link.how))
            .collect::<Vec<_>>(),
        [(said.id_string(), LinkHow::CreatedFrom)],
        "the task names the comment as made from it"
    );

    // A comment under the task is a change to the task: Ada's row for it is marked, and the
    // window that lists it counts one change she has not seen.
    let mut view = ada.view(TASKS_APP);
    let mark = mark_of(&mut view, &task.id_string()).expect("Bo's comment marks Ada's row");
    assert_eq!(
        mark.props.get("by"),
        Some(&Value::Principal(Principal::new("bo")))
    );
    assert_eq!(mark.props.get("kind"), Some(&Value::text("changed")));
    let counted = window_mark(&mut view).expect("the window counts what she has not seen");
    assert_eq!(
        counted.props.get("count"),
        Some(&Value::Int(1)),
        "{counted:#?}"
    );

    // The task's page draws the comment under it, with the line saying she has not read it.
    let page = ada.host.open_resource(&task, None).unwrap();
    let dividers = of_kind(page.tree(), "ui.UnreadDivider").len();
    assert_eq!(dividers, 1, "the thread says what she has not read");
    let drawn = comments_drawn(page.tree());
    assert_eq!(
        drawn
            .iter()
            .map(RichValue::written_text)
            .collect::<Vec<_>>(),
        [asked_text.clone()],
        "the thread draws what was said, @Ada as a reference in it, not prose"
    );
    assert!(
        drawn[0].mentions("ada"),
        "@Ada refers to her: {:#?}",
        drawn[0]
    );

    // Read (as opening it in a window of its own marks it), neither the row, nor the place, nor
    // the thread says anything: reading a task reads what was said about it.
    assert!(ada.host.runtime().mark_seen("ada", &task).unwrap());
    ada.commit();
    let mut view = ada.view(TASKS_APP);
    assert!(
        mark_of(&mut view, &task.id_string()).is_none(),
        "{:?}",
        rows(&mut view)
    );
    assert!(window_mark(&mut view).is_none(), "{:?}", rows(&mut view));
    let page = ada.host.open_resource(&task, None).unwrap();
    assert_eq!(
        of_kind(page.tree(), "ui.UnreadDivider").len(),
        0,
        "nothing stands since she read the task"
    );
    assert!(
        ada.host.runtime().unseen("ada", &said).unwrap().is_empty(),
        "the comment is read with the task it is about"
    );

    // She answers him in the same thread: her comment is made from the same task, stands after
    // his, and is what Bo has not read.
    let reply = comment(&ada, "ada", &task, "Yes, the whole quarter.");
    ada.commit();
    assert_eq!(
        ada.host
            .store()
            .version_end(&reply, false)
            .unwrap()
            .unwrap()
            .from
            .map(|r| r.id_string()),
        Some(task.id_string()),
        "a reply is made from the task the thread is about"
    );
    bo.sync_with(&mut ada);
    bo.commit();
    let page = bo.host.open_resource(&task, None).unwrap();
    assert_eq!(
        comments_drawn(page.tree())
            .iter()
            .map(RichValue::written_text)
            .collect::<Vec<_>>(),
        [asked_text, "Yes, the whole quarter.".to_owned()],
        "the thread reads oldest first, his comment then her answer: {} {reply:?}",
        said.id_string()
    );
    assert_eq!(
        of_kind(page.tree(), "ui.UnreadDivider").len(),
        1,
        "Bo's thread says he has not read her answer"
    );
}

#[test]
fn an_agent_comments_through_the_same_action() {
    let mut ada = workspace();
    let task = page(&ada, TASKS_APP, "new_task", "Book the van");
    ada.commit();

    // The agent alone in the discussion app: its comment is its own, made from the task the way
    // a person's is.
    let said = comment(
        &ada,
        "agent:scout",
        &task,
        "The van place is held until Friday noon.",
    );
    ada.commit();
    let said_value = ada.host.runtime().read(&said, "text").unwrap();
    let Value::Rich(said_value) = &said_value else {
        panic!("a comment's text is rich text: {said_value:?}");
    };
    assert_eq!(
        said_value.written_text(),
        "The van place is held until Friday noon."
    );
    let made = ada.host.store().version_end(&said, false).unwrap().unwrap();
    assert_eq!(made.who, "agent:scout");
    assert_eq!(made.from.map(|r| r.id_string()), Some(task.id_string()));

    // Ada's row is marked for it, and the thread draws the agent's comment as anyone's.
    let mut view = ada.view(TASKS_APP);
    assert!(mark_of(&mut view, &task.id_string()).is_some());
    let page = ada.host.open_resource(&task, None).unwrap();
    assert!(
        of_kind(page.tree(), "ui.Avatar")
            .iter()
            .any(|avatar| avatar.props.get("who")
                == Some(&Value::Principal(Principal::new("agent:scout")))),
        "the thread shows who said it"
    );
}

#[test]
fn a_comment_under_a_note_draws_in_the_notes_view() {
    let mut ada = workspace();
    let plan = note_named(&ada, "Q3 plan");
    ada.commit();
    let mut bo = ada.fork("bo", &[(DISCUSSION, DISCUSSION_IDS), (NOTES, NOTES_IDS)]);

    let said = comment(
        &bo,
        "bo",
        &plan,
        "Add the offline editor to the first paragraph.",
    );
    bo.commit();
    ada.sync_with(&mut bo);

    let view = ada.host.open_resource(&plan, None).unwrap();
    assert!(
        comments_drawn(view.tree())
            .iter()
            .any(|rich| rich.written_text() == "Add the offline editor to the first paragraph."),
        "the note's view draws what was said about it: {said:?}"
    );
    assert_eq!(
        of_kind(view.tree(), "ui.UnreadDivider").len(),
        1,
        "and says she has not read it"
    );
}

#[test]
fn a_comment_is_about_its_task_so_the_page_does_not_list_it_as_made_from_it() {
    let mut ada = workspace();
    let task = page(&ada, TASKS_APP, "new_task", "Pay rent");
    ada.commit();
    let said = comment(&ada, "ada", &task, "Ask Bo about the receipt.");
    ada.commit();

    // What the store knows is a link either way; what the page shows is the difference the
    // comment's type declares: a comment is about the task, never work made from it.
    assert_eq!(
        ada.host
            .store()
            .linked(&task)
            .unwrap()
            .into_iter()
            .map(|link| link.of.id_string())
            .collect::<Vec<_>>(),
        [said.id_string()]
    );
    let page = ada.host.open_resource(&task, None).unwrap();
    let listed: Vec<String> = of_kind(page.tree(), "ui.ListRow")
        .into_iter()
        .filter_map(|row| ref_of(row, "item"))
        .map(|at| at.id_string())
        .collect();
    assert!(
        !listed.contains(&said.id_string()),
        "“Made from this” leaves out what is about the task: {listed:?}"
    );
    // The comment is drawn by the thread instead, as what was said about it.
    assert!(
        comments_drawn(page.tree())
            .iter()
            .any(|rich| rich.written_text() == "Ask Bo about the receipt."),
        "the thread draws it: {said:?}"
    );
}

#[test]
fn only_its_author_edits_or_deletes_a_comment() {
    let mut ada = workspace();
    let task = page(&ada, TASKS_APP, "new_task", "Pay rent");
    ada.commit();
    let mut bo = ada.fork("bo", &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)]);

    let said = comment(&bo, "bo", &task, "Is this the one that pays the quarter?");
    bo.commit();
    ada.sync_with(&mut bo);

    // Ada may not change what Bo said, and Bo may: what he says is his, as any resource is.
    let refused = ada.host.runtime().invoke_action(
        &Principal::new("ada"),
        ActionRef::new(
            action(&ada, DISCUSSION_APP, "edit"),
            vec![
                Value::resource(&said),
                Value::Rich(RichValue::from_text("Something else")),
            ],
        ),
    );
    assert!(
        refused.is_err(),
        "a comment is its author's to change: {refused:?}"
    );
    call(
        &bo,
        "bo",
        DISCUSSION_APP,
        "edit",
        None,
        vec![
            Value::resource(&said),
            Value::Rich(RichValue::from_text(
                "Is this the one that pays all four quarters?",
            )),
        ],
    );
    bo.commit();
    ada.sync_with(&mut bo);
    let versions = ada.host.store().versions(&said).unwrap();
    assert_eq!(versions.len(), 2, "the edit is a version of the comment");
    assert_eq!(versions[1].who, "bo");

    // The row of what it is about is marked for the edit too: what changed is the task's
    // discussion, and the place counts it as the change it is.
    let mut view = ada.view(TASKS_APP);
    let mark = mark_of(&mut view, &task.id_string()).expect("the edit marks the task's row");
    assert_eq!(
        mark.props.get("by"),
        Some(&Value::Principal(Principal::new("bo")))
    );

    // Its author removes it, and it leaves the thread with the task.
    let all = Value::resource(&bo.host.root_of(DISCUSSION_APP).unwrap());
    call(
        &bo,
        "bo",
        DISCUSSION_APP,
        "delete",
        None,
        vec![all, Value::resource(&said)],
    );
    bo.commit();
    ada.sync_with(&mut bo);
    let mut view = ada.host.open_resource(&task, None).unwrap();
    view.poll_patches().unwrap();
    assert!(
        !comments_drawn(view.tree())
            .iter()
            .any(|rich| rich.written_text() == "Is this the one that pays all four quarters?"),
        "a removed comment draws nowhere: {said:?}"
    );
}
