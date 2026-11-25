//! Every capability of the Rust Tasks app, through the tilolang Tasks' own actions and views: a
//! fast test per capability, so a port that loses one fails here rather than in a person's use.

use std::collections::BTreeMap;
use std::rc::Rc;

use tilolang_runtime::blob::FileValue;
use tilolang_runtime::value::EnumValue;
use tilolang_runtime::{Actor, InvokeOutcome, Value};
use tilolang_tilo::host::RootView;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::Replica;
use crate::tasks::{APP, call, create, items, key_of, read, target, workspace};

/// The value of the declared enum `ty_name`'s case `name`, as the app's own ids name it.
fn case(ada: &Replica, ty_name: &str, name: &str) -> Value {
    let meta = &ada.host.package(APP).unwrap().meta;
    let ty = meta
        .enums
        .iter()
        .find(|ty| ty.name == ty_name)
        .unwrap_or_else(|| panic!("the {ty_name} enum"));
    let found = ty
        .variants
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no {ty_name} case {name}"));
    Value::Enum(EnumValue {
        ty: ty.id.clone(),
        variant: found.id.clone(),
        fields: BTreeMap::new(),
    })
}

/// The name of the case an enum value is.
fn case_name(ada: &Replica, value: &Value) -> String {
    let meta = &ada.host.package(APP).unwrap().meta;
    let Value::Enum(value) = value else {
        panic!("an enum value: {value:?}")
    };
    let ty = meta.enums.iter().find(|ty| ty.id == value.ty).unwrap();
    ty.variants
        .iter()
        .find(|variant| variant.id == value.variant)
        .unwrap()
        .name
        .clone()
}

/// The package a declared type named `ty_name` is in: the app's own, or the standard library's.
fn package_of_type(ada: &Replica, ty_name: &str) -> tilolang_runtime::meta::Meta {
    let meta = ada.host.package(APP).unwrap().meta;
    if meta.types.iter().any(|ty| ty.name == ty_name) {
        return meta;
    }
    ada.host.package("tilo.std").unwrap().meta
}

fn none() -> Value {
    Value::Opt(None)
}

fn some(value: Value) -> Value {
    Value::Opt(Some(Box::new(value)))
}

fn int(value: i64) -> Value {
    Value::Int(value)
}

fn task(ada: &Replica, root: &Value, title: &str) -> Value {
    call(ada, "new_task", create(root, title))
}

fn plan(ada: &Replica, root: &Value, name: &str) -> Value {
    let made = call(ada, "new_project", vec![root.clone()]);
    call(ada, "rename", vec![made.clone(), Value::text(name)]);
    made
}

fn listed(r: &Replica, at: &Value, field: &str) -> Vec<Value> {
    let Value::Ref(at) = at else {
        panic!("a resource")
    };
    match r.host.runtime().read(&at.target, field).unwrap() {
        Value::List(items) => items,
        other => panic!("{field} reads a list: {other:?}"),
    }
}

/// The field `name` of the record value `value`.
fn field(ada: &Replica, value: &Value, ty_name: &str, name: &str) -> Option<Value> {
    let meta = package_of_type(ada, ty_name);
    let ty = meta.types.iter().find(|ty| ty.name == ty_name).unwrap();
    let field = ty.fields.iter().find(|field| field.name == name).unwrap();
    match value {
        Value::Rec(init) => init.fields.get(&field.id).cloned(),
        other => panic!("a record: {other:?}"),
    }
}

/// The case `task`'s status is, by name.
fn status(ada: &Replica, task: &Value) -> String {
    case_name(ada, &read(ada, task, "status"))
}

#[test]
fn a_task_has_a_number_a_status_and_moves_between_columns() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "First");
    let two = task(&ada, &root, "Second");
    assert_eq!(read(&ada, &one, "number"), int(1));
    assert_eq!(read(&ada, &two, "number"), int(2));
    assert_eq!(status(&ada, &one), "backlog");

    call(
        &ada,
        "set_status",
        vec![one.clone(), case(&ada, "Status", "in_progress"), int(0)],
    );
    ada.commit();
    assert_eq!(status(&ada, &one), "in_progress");
    assert_eq!(
        status(&ada, &two),
        "backlog",
        "the other task stays where it stood"
    );
}

#[test]
fn the_workspace_board_shows_a_column_for_every_status() {
    let (ada, root) = workspace();
    let _ = task(&ada, &root, "Backlogged");
    let working = task(&ada, &root, "Working");
    call(
        &ada,
        "set_status",
        vec![working, case(&ada, "Status", "in_progress"), int(0)],
    );
    ada.commit();
    let view = ada
        .host
        .open_layer(
            APP,
            &RootView {
                root: None,
                view: Some("board".to_owned()),
            },
        )
        .unwrap();
    let meta = &ada.host.package(APP).unwrap().meta;
    let cases = meta
        .enums
        .iter()
        .find(|ty| ty.name == "Status")
        .unwrap()
        .variants
        .len();
    assert_eq!(
        view.tree().of_kind("ui.BoardList").len(),
        cases,
        "one column for every status"
    );
    assert_eq!(
        view.tree().of_kind("ui.BoardCard").len(),
        2,
        "both tasks stand on the board"
    );
}

#[test]
fn a_project_is_a_project_or_an_area_and_names_its_people() {
    let (ada, root) = workspace();
    let made = plan(&ada, &root, "Move flat");
    call(
        &ada,
        "set_kind",
        vec![made.clone(), case(&ada, "Kind", "area")],
    );
    call(
        &ada,
        "involve",
        vec![made.clone(), Value::text("Mia"), Value::text("mover")],
    );
    ada.commit();
    assert_eq!(case_name(&ada, &read(&ada, &made, "kind")), "area");
    let people = listed(&ada, &made, "people");
    assert_eq!(people.len(), 1);
    assert_eq!(read(&ada, &people[0], "name"), Value::text("Mia"));
    assert_eq!(read(&ada, &people[0], "role"), Value::text("mover"));
}

#[test]
fn an_estimate_is_set_and_cleared() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Sized");
    call(&ada, "estimate", vec![one.clone(), some(int(5))]);
    assert_eq!(read(&ada, &one, "estimate"), some(int(5)));
    call(&ada, "estimate", vec![one.clone(), none()]);
    assert_eq!(read(&ada, &one, "estimate"), none());
}

#[test]
fn a_file_is_attached_and_detached() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "With a file");
    let file = Value::File(Rc::new(FileValue::new(
        "plan.pdf",
        "application/pdf",
        42,
        "content-1",
    )));
    call(&ada, "attach", vec![one.clone(), file]);
    let held = listed(&ada, &one, "attachments");
    assert_eq!(held.len(), 1, "the file is attached");
    match &held[0] {
        Value::File(file) => assert_eq!(file.name, "plan.pdf"),
        other => panic!("an attachment is a file: {other:?}"),
    }
    call(&ada, "detach", vec![one.clone(), held[0].clone()]);
    assert!(
        listed(&ada, &one, "attachments").is_empty(),
        "and taken off again"
    );
}

#[test]
fn giving_a_task_to_an_agent_keeps_who_gave_it_and_when() {
    use tilolang_runtime::members::Member;
    let (ada, root) = workspace();
    ada.host.runtime().set_members(
        vec![
            Member::person("ada", tilo_actions::policy::Role::Owner),
            Member::agent("helper", "ada"),
        ],
        None,
    );
    let one = task(&ada, &root, "Give me a hand");
    let helper = Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
        "helper", "ada",
    )));
    call(&ada, "assign", vec![one.clone(), some(helper)]);
    ada.commit();
    match read(&ada, &one, "assigned_by") {
        Value::Opt(Some(held)) => match *held {
            Value::Actor(_) => {}
            other => panic!("assigned_by is an actor: {other:?}"),
        },
        other => panic!("assigned_by is set: {other:?}"),
    }
    match read(&ada, &one, "assigned_at") {
        Value::Opt(Some(held)) => match *held {
            Value::Text(_) => {}
            other => panic!("assigned_at is a time: {other:?}"),
        },
        other => panic!("assigned_at is set: {other:?}"),
    }
}

#[test]
fn a_batch_of_tasks_is_one_change_and_a_blank_one_makes_none() {
    let (ada, root) = workspace();
    let made = call(
        &ada,
        "new_tasks",
        vec![
            Value::List(vec![Value::text("One"), Value::text("Two")]),
            none(),
        ],
    );
    assert_eq!(items(&ada).len(), 2);
    assert_eq!(ada.commit(), 1, "adding a batch is one change, so one undo");
    let Value::List(made) = made else {
        panic!("the made tasks")
    };
    assert_eq!(made.len(), 2);

    let id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|a| a.name == "new_tasks")
        .unwrap()
        .id
        .clone();
    let refused = ada.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (
            id.as_str(),
            vec![
                root,
                Value::List(vec![Value::text("Three"), Value::text("  ")]),
                none(),
            ],
        ),
    );
    let refused = match refused {
        Ok(outcome) => !matches!(outcome, InvokeOutcome::Done(_)),
        Err(_) => true,
    };
    assert!(refused, "a blank title refuses the batch whole");
    assert_eq!(items(&ada).len(), 2, "and adds nothing");
}

#[test]
fn moving_a_group_of_tasks_is_one_change() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "One");
    let two = task(&ada, &root, "Two");
    call(
        &ada,
        "set_status_many",
        vec![
            Value::List(vec![one.clone(), two.clone()]),
            case(&ada, "Status", "in_review"),
            int(0),
        ],
    );
    assert_eq!(ada.commit(), 1, "one change for the group");
    assert_eq!(status(&ada, &one), "in_review");
    assert_eq!(status(&ada, &two), "in_review");
}

#[test]
fn a_task_waits_and_goes_on_with_its_work_kept() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Waits for a reply");
    call(
        &ada,
        "wait",
        vec![
            one.clone(),
            Value::text("the venue's reply"),
            some(Value::text("2026-12-01T10:00:00Z")),
            case(&ada, "WaitKind", "reply"),
            Value::text(""),
        ],
    );
    assert_eq!(
        read(&ada, &one, "waiting_for"),
        Value::text("the venue's reply")
    );
    match read(&ada, &one, "work") {
        Value::Opt(Some(held)) => match *held {
            Value::Rec(_) => {}
            other => panic!("the work is a record: {other:?}"),
        },
        other => panic!("the work is kept: {other:?}"),
    }
    call(&ada, "resume", vec![one.clone(), Value::text("it came")]);
    ada.commit();
    assert_eq!(read(&ada, &one, "waiting_for"), Value::text(""));
    assert_eq!(read(&ada, &one, "waiting_until"), none());
}

/// Whether invoking `action` with `args` is refused: the action says why, or the platform does.
fn refused(ada: &Replica, action: &str, args: Vec<Value>) -> bool {
    let id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match ada.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (id.as_str(), args),
    ) {
        Ok(outcome) => !matches!(outcome, InvokeOutcome::Done(_)),
        Err(_) => true,
    }
}

/// A session begins only where the task may still be worked: its money and its working time are
/// left, and its sessions were not lost too often in a row.
#[test]
fn a_session_begins_only_where_the_task_may_still_be_worked() {
    for (money, time) in [(true, false), (false, true)] {
        let (ada, root) = workspace();
        let one = task(&ada, &root, "Worked");
        call(
            &ada,
            "set_budget",
            vec![
                one.clone(),
                if money { some(int(10)) } else { none() },
                if time { some(int(10)) } else { none() },
                case(&ada, "BudgetBy", "person"),
            ],
        );
        call(
            &ada,
            "session_ended",
            vec![
                one.clone(),
                if money { int(10) } else { int(0) },
                if time { int(10) } else { int(0) },
            ],
        );
        assert!(
            refused(&ada, "session_began", vec![one.clone()]),
            "a spent budget begins no session"
        );
    }

    // A session lost twice in a row: the next one does not begin until its person says go on.
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Lost");
    for _ in 0..2 {
        call(&ada, "session_began", vec![one.clone()]);
        call(
            &ada,
            "stop",
            vec![
                one.clone(),
                case(&ada, "StoppedWhy", "lost_worker"),
                Value::text(""),
            ],
        );
    }
    assert!(
        refused(&ada, "session_began", vec![one.clone()]),
        "two sessions lost in a row begin no third"
    );
    call(
        &ada,
        "resume",
        vec![one.clone(), Value::text("its person said go on")],
    );
    call(&ada, "session_began", vec![one.clone()]);
    assert!(
        !refused(&ada, "session_began", vec![one.clone()]),
        "told to go on, its sessions begin again"
    );
}

/// Work on a task's pieces ends its wait once every piece is through: closed, waiting for a
/// person to look, or stopped.
#[test]
fn work_on_the_pieces_resumes_the_task_once_they_are_all_through() {
    let (ada, root) = workspace();
    let parent = task(&ada, &root, "Parent");
    let one = call(
        &ada,
        "add_subtask",
        vec![parent.clone(), Value::text("One")],
    );
    let two = call(
        &ada,
        "add_subtask",
        vec![parent.clone(), Value::text("Two")],
    );
    call(
        &ada,
        "wait",
        vec![
            parent.clone(),
            Value::text("the pieces"),
            none(),
            case(&ada, "WaitKind", "pieces"),
            Value::text(""),
        ],
    );
    ada.commit();
    assert_eq!(
        read(&ada, &parent, "waiting_for"),
        Value::text("the pieces"),
        "the task waits for its pieces"
    );
    call(&ada, "check", vec![one]);
    ada.commit();
    assert_eq!(
        read(&ada, &parent, "waiting_for"),
        Value::text("the pieces"),
        "one piece through is not all of them"
    );
    call(&ada, "check", vec![two]);
    ada.commit();
    assert_eq!(
        read(&ada, &parent, "waiting_for"),
        Value::text(""),
        "the last piece through ends the wait"
    );
    let work = format!("{:?}", read(&ada, &parent, "work"));
    assert!(
        work.contains("the pieces are through"),
        "it says what made it go on: {work}"
    );
}

/// A task that lets one piece be in work at once takes no second piece given to an agent while
/// the first is worked.
#[test]
fn a_piece_beyond_what_its_task_allows_is_not_given_to_an_agent() {
    use tilolang_runtime::members::Member;
    let (ada, root) = workspace();
    ada.host.runtime().set_members(
        vec![
            Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::agent("helper", "ada"),
        ],
        None,
    );
    let parent = task(&ada, &root, "Parent");
    let one = call(
        &ada,
        "add_subtask",
        vec![parent.clone(), Value::text("One")],
    );
    let two = call(
        &ada,
        "add_subtask",
        vec![parent.clone(), Value::text("Two")],
    );
    call(
        &ada,
        "set_pieces_at_once",
        vec![parent.clone(), some(int(1))],
    );
    call(&ada, "session_began", vec![one.clone()]);
    ada.commit();
    let helper = MemberId::agent("helper", "ada");
    let give = |task: &Value| {
        vec![
            task.clone(),
            Value::Opt(Some(Box::new(Value::Actor(Actor::from(helper.clone()))))),
        ]
    };
    assert!(
        refused(&ada, "assign", give(&two)),
        "the second piece waits while the first is in work"
    );
    call(&ada, "check", vec![one]);
    ada.commit();
    call(&ada, "assign", give(&two));
    ada.commit();
    assert_eq!(
        read(&ada, &two, "assignee"),
        some(Value::Actor(Actor::from(helper))),
        "with the first piece through, the second is given"
    );
}

#[test]
fn a_tasks_work_records_its_budget_its_sessions_and_what_stopped_it() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Worked on");
    call(
        &ada,
        "set_budget",
        vec![
            one.clone(),
            some(int(2_000_000)),
            some(int(3_600_000)),
            case(&ada, "BudgetBy", "person"),
        ],
    );
    call(&ada, "session_began", vec![one.clone()]);
    call(
        &ada,
        "session_ended",
        vec![one.clone(), int(500_000), int(60_000)],
    );
    call(
        &ada,
        "stop",
        vec![
            one.clone(),
            case(&ada, "StoppedWhy", "budget"),
            Value::text("two sessions"),
        ],
    );
    ada.commit();
    match read(&ada, &one, "work") {
        Value::Opt(Some(held)) => match *held {
            Value::Rec(_) => {
                assert_eq!(
                    field(&ada, &held, "Work", "sessions"),
                    Some(int(1)),
                    "one session counted"
                );
                assert_eq!(
                    field(&ada, &held, "Work", "spent_micros"),
                    Some(int(500_000))
                );
                assert_eq!(field(&ada, &held, "Work", "worked_ms"), Some(int(60_000)));
                assert_eq!(field(&ada, &held, "Work", "lost"), Some(int(1)));
                match field(&ada, &held, "Work", "stopped") {
                    Some(Value::Opt(Some(_))) => {}
                    other => panic!("what stopped it is kept: {other:?}"),
                }
            }
            other => panic!("the work is a record: {other:?}"),
        },
        other => panic!("the work is kept: {other:?}"),
    }
}

#[test]
fn an_imported_issue_keeps_its_origin_and_imports_once() {
    let (ada, root) = workspace();
    let origin = call(
        &ada,
        "add_origin",
        vec![
            root.clone(),
            Value::text("Mia"),
            Value::text("GitHub"),
            Value::text(""),
            Value::text("https://example.test/issues/7"),
            Value::text("7"),
            none(),
            none(),
        ],
    );
    let made = call(
        &ada,
        "import_issue",
        vec![
            origin.clone(),
            Value::text("Fix the printer"),
            Value::text("7"),
            case(&ada, "Status", "todo"),
        ],
    );
    assert_eq!(read(&ada, &made, "title"), Value::text("Fix the printer"));
    match read(&ada, &made, "origin") {
        Value::Opt(Some(held)) => {
            // The origin is a resource of the workspace: the task refers to it.
            let Value::Ref(held) = *held else {
                panic!("the origin the task names: {held:?}");
            };
            assert_eq!(
                read(&ada, &Value::Ref(held), "reference"),
                some(Value::text("7"))
            );
        }
        other => panic!("its origin is kept: {other:?}"),
    }
    assert_eq!(
        read(&ada, &made, "url"),
        some(Value::text("https://example.test/issues/7"))
    );
    let again = call(
        &ada,
        "import_issue",
        vec![
            origin,
            Value::text("Fix the printer"),
            Value::text("7"),
            case(&ada, "Status", "todo"),
        ],
    );
    assert_eq!(
        listed(&ada, &root, "items").len(),
        1,
        "importing it again makes none"
    );
    assert_eq!(
        target_id(&again),
        target_id(&made),
        "and answers the task it made"
    );
}

fn target_id(value: &Value) -> Option<String> {
    match value {
        Value::Ref(r) => Some(r.target.id_string()),
        _ => None,
    }
}

#[test]
fn files_import_as_tasks_and_again_only_once() {
    let (ada, _root) = workspace();
    let file = Value::File(Rc::new(FileValue::new(
        "letter.eml",
        "message/rfc822",
        120,
        "content-2",
    )));
    let made = call(
        &ada,
        "import_files",
        vec![Value::List(vec![file.clone()]), none()],
    );
    let Value::List(made) = made else {
        panic!("the made tasks")
    };
    assert_eq!(made.len(), 1);
    assert_eq!(read(&ada, &made[0], "title"), Value::text("letter.eml"));
    let again = call(&ada, "import_files", vec![Value::List(vec![file]), none()]);
    let Value::List(again) = again else {
        panic!("the made tasks")
    };
    assert!(again.is_empty(), "the same file imports once");
    assert_eq!(items(&ada).len(), 1);
}

#[test]
fn a_projects_page_shows_its_people() {
    let (ada, root) = workspace();
    let made = plan(&ada, &root, "Launch");
    call(
        &ada,
        "involve",
        vec![made.clone(), Value::text("Bo"), Value::text("")],
    );
    ada.commit();
    let view_id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "project_page")
        .unwrap()
        .id
        .clone();
    let view = ada
        .host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view_id,
            vec![made],
        )
        .unwrap();
    let shown = format!("{:?}", view.tree());
    assert!(
        shown.contains("Bo"),
        "the project's people are on its page: {shown}"
    );
}

#[test]
fn a_task_page_shows_its_number_and_its_status() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Shown");
    call(
        &ada,
        "set_status",
        vec![one.clone(), case(&ada, "Status", "in_review"), int(0)],
    );
    ada.commit();
    let view_id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "task_page")
        .unwrap()
        .id
        .clone();
    let view = ada
        .host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view_id,
            vec![one],
        )
        .unwrap();
    let shown = format!("{:?}", view.tree());
    assert!(shown.contains("Shown"), "the page shows its title: {shown}");
    assert!(shown.contains("#1"), "and its number: {shown}");
}

/// A project's title stands on a line that wraps: beside it the window's views and the "…" it
/// keeps its commands behind share that line, and wrap onto the next one of a narrow page rather
/// than standing over each other.
#[test]
fn the_line_beside_a_project_pages_title_wraps() {
    let (ada, root) = workspace();
    let made = plan(&ada, &root, "Launch");
    ada.commit();
    let Value::Ref(at) = &made else {
        panic!("a resource: {made:?}")
    };
    let mut view = ada.host.open_resource(&at.target, None).unwrap();
    view.poll_patches().unwrap();
    // The window switches the project's views from the bar: find the page among them, which is
    // the one whose title input holds what the work goes towards.
    let tabs = key_of(&view, "ui.ViewTabs", &|_| true);
    let pages = view.tree().of_kind("ui.Segment").len();
    let mut found = None;
    for index in 0..pages {
        view.fire_with(
            &tabs,
            "change",
            Some(&Value::Int(i64::try_from(index).unwrap_or(i64::MAX))),
        )
        .unwrap();
        view.poll_patches().unwrap();
        let page = view.tree().of_kind("ui.TextInput").iter().any(|input| {
            input.props.get("placeholder") == Some(&Value::text("What the work goes towards"))
        });
        if page {
            found = Some(index);
            break;
        }
    }
    assert!(
        found.is_some(),
        "the project's page among the window's {pages} views"
    );
    // A project's page is one of the views that share its title's line, and its own actions
    // stand behind the line's "…".
    let line = view
        .tree()
        .of_kind("row")
        .into_iter()
        .find(|row| row.key.ends_with("~line"))
        .unwrap_or_else(|| panic!("the title's line: {:#?}", view.tree()));
    assert_eq!(
        line.props.get("wrap"),
        Some(&Value::Bool(true)),
        "the title's line wraps: {line:#?}"
    );
    for kind in ["ui.ViewTabs", "ui.MenuButton"] {
        assert!(
            line.children.iter().any(|child| child.kind == kind),
            "the {kind} shares the title's line: {line:#?}"
        );
    }
}

/// The row that holds `key`, as the nearest one over it: what stands beside the node a page
/// shows reads off it.
fn row_holding<'a>(
    node: &'a tilolang_runtime::UiNode,
    key: &str,
) -> Option<&'a tilolang_runtime::UiNode> {
    if node.kind == "row" && node.children.iter().any(|child| child.key == key) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| row_holding(child, key))
}

/// The number a task has stands with its facts, beside the pickers, not alone on a line under
/// its title where a narrow page wraps it there.
#[test]
fn a_task_pages_number_stands_with_its_facts() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Shown");
    ada.commit();
    let view = crate::tasks::task_page(&ada, &one);
    let number = view
        .tree()
        .of_kind("ui.Text")
        .into_iter()
        .find(|node| {
            node.props.get("value") == Some(&Value::text("#1"))
                || node.props.get("label") == Some(&Value::text("#1"))
        })
        .expect("the page shows its number");
    let facts = row_holding(view.tree(), &number.key).expect("the number stands in a row");
    assert!(
        !facts.of_kind("ui.MenuButton").is_empty(),
        "the number stands with the task's facts: {facts:#?}"
    );
}

/// A task's page offers one person to pick: its assignee. The hand that gave a task to an agent
/// is the app's bookkeeping, so it takes no second "No one" picker beside the first.
#[test]
fn a_task_page_offers_one_person_to_pick() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Shown");
    ada.commit();
    let view = crate::tasks::task_page(&ada, &one);
    let pickers = view.tree().of_kind("ui.PersonField");
    assert_eq!(pickers.len(), 1, "one person to pick: {:#?}", view.tree());
}

/// The page of a task keeps every command it offers behind the "…" beside its title: the host
/// reads what the task's type declares, so the page names them without the view drawing them.
#[test]
fn the_task_page_offers_the_tasks_commands_beside_its_title() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Shown");
    ada.commit();
    let view_id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "task_page")
        .unwrap()
        .id
        .clone();
    let view = ada
        .host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view_id,
            vec![one],
        )
        .unwrap();
    let more = view
        .tree()
        .of_kind("ui.MenuButton")
        .into_iter()
        .find(|node| node.key.ends_with("~more"))
        .unwrap_or_else(|| {
            panic!(
                "the page keeps its commands behind a \"…\": {:#?}",
                view.tree()
            )
        });
    let commands: Vec<String> = more
        .of_kind("ui.MenuItem")
        .into_iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    for wanted in ["Complete", "Today", "Tomorrow", "No date", "High", "Delete"] {
        assert!(
            commands.iter().any(|command| command == wanted),
            "the page offers {wanted}: {commands:?}"
        );
    }
    assert!(
        commands.iter().any(|command| command.starts_with("Assign")),
        "the page offers the command that asks who does it: {commands:?}"
    );
}

#[test]
fn labels_are_given_to_one_task_and_to_a_group() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "One");
    let two = task(&ada, &root, "Two");
    call(&ada, "label", vec![one.clone(), Value::text("bug")]);
    call(
        &ada,
        "label_many",
        vec![
            Value::List(vec![one.clone(), two.clone()]),
            Value::text("office"),
        ],
    );
    ada.commit();
    let labels = |at: &Value| match read(&ada, at, "labels") {
        Value::List(names) => names
            .into_iter()
            .map(|name| match name {
                Value::Text(name) => name,
                other => panic!("a label: {other:?}"),
            })
            .collect::<Vec<_>>(),
        other => panic!("labels read a list: {other:?}"),
    };
    assert_eq!(labels(&one), ["bug", "office"]);
    assert_eq!(labels(&two), ["office"], "the group gets its label too");
    call(&ada, "unlabel", vec![one.clone(), Value::text("bug")]);
    assert_eq!(labels(&one), ["office"]);
}

#[test]
fn a_subtask_opens_a_closed_parent_and_moves_out_again() {
    let (ada, root) = workspace();
    let parent = task(&ada, &root, "Parent");
    call(
        &ada,
        "set_status",
        vec![parent.clone(), case(&ada, "Status", "done"), int(0)],
    );
    let child = call(
        &ada,
        "add_subtask",
        vec![parent.clone(), Value::text("Child")],
    );
    ada.commit();
    assert_eq!(
        status(&ada, &parent),
        "todo",
        "a closed task with work under it opens"
    );
    assert_eq!(
        status(&ada, &child),
        "todo",
        "and the step added under it is work to do"
    );
    call(&ada, "outdent", vec![child.clone()]);
    assert_eq!(
        read(&ada, &child, "parent"),
        none(),
        "it stands at the top again"
    );
}

#[test]
fn a_days_presets_and_a_reminder_move_with_the_task() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Due soon");
    call(&ada, "due_tomorrow", vec![one.clone()]);
    assert_eq!(read(&ada, &one, "due"), some(Value::text("2026-10-30")));
    call(&ada, "due_clear", vec![one.clone()]);
    assert_eq!(read(&ada, &one, "due"), none());
    call(
        &ada,
        "remind_at",
        vec![one.clone(), Value::text("2026-10-29T12:00:00Z")],
    );
    assert_eq!(
        read(&ada, &one, "remind"),
        some(Value::text("2026-10-29T12:00:00Z"))
    );
    call(&ada, "remind_clear", vec![one.clone()]);
    assert_eq!(read(&ada, &one, "remind"), none());
}

#[test]
fn priorities_are_set_and_cleared() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Ranked");
    call(&ada, "set_priority_high", vec![one.clone()]);
    assert_eq!(case_name(&ada, &read(&ada, &one, "priority")), "high");
    call(&ada, "set_priority_none", vec![one.clone()]);
    assert_eq!(case_name(&ada, &read(&ada, &one, "priority")), "none");
}

#[test]
fn duplicating_copies_the_subtree_and_deleting_takes_it() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "One");
    let _ = call(
        &ada,
        "add_subtask",
        vec![one.clone(), Value::text("Under it")],
    );
    ada.commit();
    let copy = call(&ada, "duplicate", vec![one.clone()]);
    assert_eq!(items(&ada).len(), 4, "the task and both tasks under it");
    assert_ne!(copy, one);
    call(&ada, "remove", vec![copy]);
    assert_eq!(items(&ada).len(), 2, "its subtree goes with it");
}

#[test]
fn a_tasks_note_is_written_in_notes_made_from_the_task() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Write me down");
    let _note = call(&ada, "write_note", vec![one]);
    ada.commit();
    let notebook = ada.host.root_of("so.tilo.notes").unwrap();
    let held = ada
        .host
        .runtime()
        .read(&notebook, "notes")
        .expect("the notebook reads");
    let Value::List(notes) = held else {
        panic!("the notebook lists its notes")
    };
    assert_eq!(notes.len(), 1, "one note holds the task's words");
    let _ = notes;
}

#[test]
fn the_home_window_offers_every_place_and_each_label_in_use() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Labelled");
    call(&ada, "label", vec![one, Value::text("bug")]);
    ada.commit();
    let view = ada.view(APP);
    let items: Vec<String> = view
        .tree()
        .of_kind("ui.NavItem")
        .iter()
        .filter_map(|node| match node.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    for place in [
        "Tasks",
        "Inbox",
        "Today",
        "Upcoming",
        "Todos",
        "Nobody yet",
        "Agent work",
        "Waiting",
        "Done",
    ] {
        assert!(
            items.iter().any(|label| label == place),
            "{place} is a place: {items:?}"
        );
    }
    assert!(
        items.iter().any(|label| label == "bug"),
        "a label in use is a place: {items:?}"
    );
}

#[test]
fn a_waits_place_pieces_and_the_task_page_are_kept() {
    let (ada, root) = workspace();
    let one = task(&ada, &root, "Pieces");
    call(
        &ada,
        "set_place",
        vec![
            one.clone(),
            Value::text("files/the-work"),
            Value::text("notes/the-work"),
        ],
    );
    call(&ada, "set_pieces_at_once", vec![one.clone(), some(int(3))]);
    call(
        &ada,
        "wait",
        vec![
            one.clone(),
            Value::text("the pieces"),
            none(),
            case(&ada, "WaitKind", "pieces"),
            Value::text("automation-1"),
        ],
    );
    ada.commit();
    match read(&ada, &one, "work") {
        Value::Opt(Some(held)) => {
            assert_eq!(
                field(&ada, &held, "Work", "folder"),
                Some(Value::text("files/the-work"))
            );
            assert_eq!(
                field(&ada, &held, "Work", "pieces_at_once"),
                Some(some(int(3)))
            );
        }
        other => panic!("the work is kept: {other:?}"),
    }
    let view_id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "task_page")
        .unwrap()
        .id
        .clone();
    let view = ada
        .host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view_id,
            vec![one],
        )
        .unwrap();
    let shown = format!("{:?}", view.tree());
    assert!(
        shown.contains("files/the-work"),
        "the page says where its work is: {shown}"
    );
    assert!(
        shown.contains("Discussion"),
        "the page carries the task's thread: {shown}"
    );
}

#[test]
fn a_subtask_typed_in_one_burst_is_one_task_with_the_whole_words() {
    let (ada, root) = workspace();
    let parent = task(&ada, &root, "Parent");
    ada.commit();
    let view_id = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "task_page")
        .unwrap()
        .id
        .clone();
    let mut view = ada
        .host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view_id,
            vec![parent.clone()],
        )
        .unwrap();
    view.poll_patches().unwrap();
    let entry = crate::tasks::key_of(&view, "ui.TextInput", &|node| {
        node.props.get("placeholder") == Some(&Value::text("Add a subtask"))
    });
    // A burst arrives as one change: the field makes the step with the whole of it.
    view.fire_with(&entry, "begin", Some(&Value::text("Buy milk")))
        .unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    let under = |ada: &Replica| -> Vec<String> {
        items(ada)
            .into_iter()
            .filter(|t| target(&read(ada, t, "parent")) == target(&parent))
            .map(|t| match read(ada, &t, "title") {
                Value::Text(title) => title,
                other => panic!("a title: {other:?}"),
            })
            .collect()
    };
    assert_eq!(
        under(&ada),
        ["Buy milk"],
        "one step, holding the whole burst, not its first letter"
    );
    // The field edits the record from then on: emptied, and left, the step goes.
    view.edit(&entry, &Value::text("")).unwrap();
    ada.commit();
    view.poll_patches().unwrap();
    view.fire_with(&entry, "blur", None).unwrap();
    ada.commit();
    assert!(
        under(&ada).is_empty(),
        "a step left empty is taken away: {:?}",
        under(&ada)
    );
}
