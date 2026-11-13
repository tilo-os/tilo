//! A discussion under a task and under a note: a comment made from what it is about, the thread
//! that draws under it, what the comment changes for whoever made or read what it is about
//! (their row, the window's count, the line in the thread), an agent's comment, what a page that
//! draws the thread leaves out of `std.linked`, and a scenario that comments as the people and
//! times it names.

use automerge::AutoCommit;
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::store::{LinkHow, Store as _};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{ActionRef, InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::{Replica, field_id, target};

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
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

/// The dividers saying where the thread's unread comments start: the conversation's one divider
/// in the accent, told apart from a day's by the tone it is drawn in.
fn unread_dividers(tree: &UiNode) -> Vec<&UiNode> {
    of_kind(tree, "ui.SectionDivider")
        .into_iter()
        .filter(|node| match node.props.get("tone") {
            Some(Value::Enum(found)) => found.variant == "accent",
            Some(Value::Text(case)) => case == "accent",
            _ => false,
        })
        .collect()
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

/// The Tasks window these tests read their marks off, on the Inbox the tasks they make wait
/// in: the window opens on Today, where an undated task is not.
fn tasks_window(ada: &Replica) -> ViewInstance {
    let mut view = ada.view(TASKS_APP);
    crate::tasks::go_to(&mut view, "Inbox");
    view
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
fn task_titled(r: &Replica, title: &str) -> ResourceRef {
    let root = Value::resource(&r.host.root_of(TASKS_APP).unwrap());
    target(&call(
        r,
        "ada",
        TASKS_APP,
        "new_task",
        None,
        crate::tasks::create(&root, title),
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

/// Ada's task, with Bo's comment about it naming her: what a thread under it, and the marks it
/// leaves, start from.
fn ada_and_bo_after_a_comment() -> (Replica, Replica, ResourceRef, ResourceRef, String) {
    let mut ada = workspace();
    let task = task_titled(&ada, "Pay rent");
    ada.commit();
    let mut bo = ada.fork("bo", &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)]);
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
    (ada, bo, task, said, asked_text)
}

#[test]
fn a_comment_is_made_from_the_task_it_is_about_and_changes_it() {
    let (ada, _bo, task, said, _asked) = ada_and_bo_after_a_comment();

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
    let mut view = tasks_window(&ada);
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
}

#[test]
fn a_thread_draws_what_was_said_and_is_read_with_what_it_is_about() {
    let (mut ada, mut bo, task, said, asked_text) = ada_and_bo_after_a_comment();

    // The task's page draws the comment under it, with the line saying she has not read it.
    let page = ada.host.open_resource(&task, None).unwrap();
    let dividers = unread_dividers(page.tree()).len();
    assert_eq!(dividers, 1, "the thread says what she has not read");
    let drawn = comments_drawn(page.tree());
    assert_eq!(
        drawn
            .iter()
            .map(RichValue::written_text)
            .collect::<Vec<_>>(),
        std::slice::from_ref(&asked_text),
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
    let mut view = tasks_window(&ada);
    assert!(
        mark_of(&mut view, &task.id_string()).is_none(),
        "{:?}",
        rows(&mut view)
    );
    assert!(window_mark(&mut view).is_none(), "{:?}", rows(&mut view));
    let page = ada.host.open_resource(&task, None).unwrap();
    assert_eq!(
        unread_dividers(page.tree()).len(),
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
        unread_dividers(page.tree()).len(),
        1,
        "Bo's thread says he has not read her answer"
    );
}

#[test]
fn an_agent_comments_through_the_same_action() {
    let mut ada = workspace();
    let task = task_titled(&ada, "Book the van");
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
    let mut view = tasks_window(&ada);
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
        unread_dividers(view.tree()).len(),
        1,
        "and says she has not read it"
    );
}

#[test]
fn a_comment_is_about_its_task_so_the_page_does_not_list_it_as_made_from_it() {
    let mut ada = workspace();
    let task = task_titled(&ada, "Pay rent");
    ada.commit();
    let said = comment(&ada, "ada", &task, "Ask Bo about the receipt.");
    ada.commit();

    // What the store knows is a link either way; what the page says is what it draws: a page
    // that draws the thread under a resource leaves what is said about it out of `std.linked`,
    // so it is drawn once.
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

/// A step that wrote no field of its own says what its action is called: the app's words for the
/// action, never the comment its declaration explains to whoever writes the app.
#[test]
fn a_history_entry_says_what_was_done_in_the_apps_words_not_in_its_declaration() {
    let (ada, _bo, task, _said, _asked) = ada_and_bo_after_a_comment();
    let page = ada.host.open_resource(&task, None).unwrap();
    let section = of_kind(page.tree(), "ui.Section")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("tilolang-history")))
        .cloned()
        .expect("the task's page shows its history");
    let words: Vec<String> = of_kind(&section, "ui.Text")
        .into_iter()
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        words.iter().any(|word| word == "commented"),
        "the entry says what Bo did, in the words the app declares: {words:?}"
    );
    assert!(
        words.iter().any(|word| word == "bo"),
        "the entry names who did it: {words:?}"
    );
    assert!(
        !words
            .iter()
            .any(|word| word.contains("Says something about the resource")
                || word.contains("made_from")),
        "an entry never reads what the action documents: {words:?}"
    );
}

/// Ada's workspace with Tasks' discussion scenario run in it: the task it answers with, and what
/// the two people it names said about it.
fn a_task_discussed_by_a_scenario() -> (Replica, ResourceRef) {
    let (mut ada, loaded) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[(DISCUSSION, DISCUSSION_IDS), (TASKS, TASKS_IDS)],
    );
    ada.host.root_of(DISCUSSION_APP).unwrap();
    ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let scenario = loaded
        .last()
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a discussion on a task")
        .expect("Tasks stages a discussion on a task");
    let task = target(&match ada
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("the scenario answers the task: {other:?}"),
    });
    (ada, task)
}

/// What was said about `task`, each as the change that made it.
fn comments_about(ada: &Replica, task: &ResourceRef) -> Vec<tilolang_runtime::store::Version> {
    ada.host
        .store()
        .linked(task)
        .unwrap()
        .into_iter()
        // The task is made from the tasks root too; what the scenario comments are items.
        .filter(|link| link.how == LinkHow::CreatedFrom && link.of.item.is_some())
        .map(|link| {
            ada.host
                .store()
                .version_end(&link.of, false)
                .unwrap()
                .unwrap()
        })
        .collect()
}

#[test]
fn a_scenario_comments_as_the_people_and_times_it_names() {
    let (ada, task) = a_task_discussed_by_a_scenario();
    let made = comments_about(&ada, &task);
    assert_eq!(
        made.iter().map(|v| v.who.as_str()).collect::<Vec<_>>(),
        ["scenario:Ada", "scenario:Bo", "scenario:Ada"],
        "each comment is the person the scenario names"
    );
    assert_eq!(
        made.iter().map(|v| v.at.as_str()).collect::<Vec<_>>(),
        [
            "2026-06-24T09:12:00Z",
            "2026-06-24T15:40:00Z",
            "2026-06-25T08:03:00Z"
        ],
        "each comment is when the scenario names"
    );
    assert!(
        made.iter().all(|v| v
            .from
            .as_ref()
            .is_some_and(|from| from.id_string() == task.id_string())),
        "each comment is about the task: {made:#?}"
    );
}

#[test]
fn the_page_under_a_task_draws_its_thread_with_names_and_times() {
    let (ada, task) = a_task_discussed_by_a_scenario();
    let page = ada.host.open_resource(&task, None).unwrap();
    assert_eq!(
        comments_drawn(page.tree())
            .iter()
            .map(RichValue::written_text)
            .collect::<Vec<_>>(),
        [
            "The supplier quotes six weeks. Do we order before the summer?",
            "Yes. The room stays empty either way.",
            "Ordered, delivery the first week of August."
        ],
        "the task's thread draws them oldest first"
    );
    let who: Vec<String> = of_kind(page.tree(), "ui.Avatar")
        .into_iter()
        .filter_map(|node| match node.props.get("who") {
            Some(Value::Principal(who)) => Some(who.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        who,
        ["scenario:Ada", "scenario:Bo", "scenario:Ada"],
        "the thread names who said each thing"
    );
    let when: Vec<String> = of_kind(page.tree(), "ui.Time")
        .into_iter()
        .filter_map(|node| match node.props.get("at") {
            Some(Value::Text(at)) => Some(at.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        when,
        [
            "2026-06-24T09:12:00Z",
            "2026-06-24T15:40:00Z",
            "2026-06-25T08:03:00Z"
        ],
        "each comment says when it was said"
    );
}

#[test]
fn only_its_author_edits_or_deletes_a_comment() {
    let mut ada = workspace();
    let task = task_titled(&ada, "Pay rent");
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
    let mut view = tasks_window(&ada);
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

/// The key of the node bound to `field` in a window.
fn bound_key(node: &UiNode, field: &str) -> Option<String> {
    if node
        .binding
        .as_ref()
        .and_then(|binding| binding.field())
        .is_some_and(|(_, bound)| bound == field)
    {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| bound_key(child, field))
}

/// The people the window's `@` drawer offers first, as the view gave them.
fn drawer(view: &ViewInstance, prop: &str) -> Vec<String> {
    let input = of_kind(view.tree(), "ui.RichInput")
        .into_iter()
        .next()
        .expect("the thread has its composer");
    match input.props.get(prop) {
        Some(Value::List(items)) => items
            .iter()
            .map(|item| match item {
                Value::Principal(who) => who.0.clone(),
                other => panic!("{prop} holds {other:?}"),
            })
            .collect(),
        other => panic!("{prop} is {other:?}"),
    }
}

#[test]
fn the_thread_offers_its_own_people_and_follows_nothing_of_what_it_is_about() {
    let (mut ada, task) = a_task_discussed_by_a_scenario();
    let page = ada.host.open_resource(&task, None).unwrap();
    let thread = ada
        .host
        .package(DISCUSSION_APP)
        .unwrap()
        .view_id("thread")
        .unwrap();
    let mut view = ada
        .host
        .runtime()
        .open_view(
            &Principal::new("ada"),
            &thread,
            vec![Value::resource(&task)],
        )
        .unwrap();
    assert_eq!(
        drawer(&view, "speakers"),
        ["scenario:Ada", "scenario:Bo"],
        "who spoke in the thread, newest first, each once; the members follow under them"
    );

    // Ada retitles the task in its own page, as a person typing in the field writes it, while
    // the thread stands open. The thread reads what was said about it, never what it holds, so
    // it is not built again.
    let title = field_id(&ada.host.package(TASKS_APP).unwrap().meta, "Task", "title");
    let input = bound_key(page.tree(), &title).expect("the task's title is bound");
    ada.commit();
    view.poll_patches().unwrap();
    let drawn = view.stats();
    page.edit(&input, &Value::text("Pay the whole quarter"))
        .unwrap();
    ada.commit();
    assert_eq!(
        ada.host.label_at(&task),
        "Pay the whole quarter",
        "the title is the task's now"
    );
    let patches = view.poll_patches().unwrap();
    assert!(
        patches.is_empty(),
        "nothing the thread shows changed: {patches:#?}"
    );
    assert_eq!(
        view.stats().renders,
        drawn.renders,
        "a field of what it is about does not run the thread again"
    );
    assert_eq!(
        drawer(&view, "speakers"),
        ["scenario:Ada", "scenario:Bo"],
        "and the drawer still offers the thread's own people"
    );
}
