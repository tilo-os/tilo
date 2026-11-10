//! A notebook as a tree, through the real packages and the record: a note moves under the one
//! it lands on and among its own, a branch copies whole, a branch is deleted as one change a
//! person takes back at once, and a note's own follow their titles when the person asks it.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, ResourceRef, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;
use crate::legibility::call;

const NOTES_APP: &str = "so.tilo.lang.notes";
const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);

/// Ada's workspace with the notes app, and its notebook.
fn workspace() -> (Replica, ResourceRef) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, NOTES]);
    ada.commit();
    let notebook = ada.host.root_of(NOTES_APP).expect("the notebook");
    (ada, notebook)
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target.clone()),
        other => panic!("a resource: {other:?}"),
    }
}

/// The notes the notebook holds, in the order the record keeps them.
fn notes(ada: &Replica, notebook: &ResourceRef) -> Vec<ResourceRef> {
    match ada
        .host
        .runtime()
        .read(notebook, "notes")
        .expect("the notebook holds its notes")
    {
        Value::List(items) => items.into_iter().map(|item| target(&item)).collect(),
        other => panic!("the notes are a list: {other:?}"),
    }
}

/// A note called `title`, at the top of the tree until something moves it.
fn note(ada: &mut Replica, notebook: &ResourceRef, title: &str) -> ResourceRef {
    let made = target(&call(
        ada,
        "ada",
        NOTES_APP,
        "new_note",
        vec![Value::resource(notebook)],
    ));
    call(
        ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&made), Value::text(title)],
    );
    ada.commit();
    made
}

/// Put `moved` at the end of `under`'s own (the top level for none), or where `before` stands:
/// what the tree reports of a drop, and of `Tab` and the moves the keyboard makes.
fn move_to(
    ada: &mut Replica,
    notebook: &ResourceRef,
    moved: &ResourceRef,
    under: Option<&ResourceRef>,
    before: Option<&ResourceRef>,
) {
    // A note it may stand under, or none: the action takes it as an `Option<Note>`.
    let option_of = |of: Option<&ResourceRef>| match of {
        Some(of) => Value::Opt(Some(Box::new(Value::resource(of)))),
        None => Value::none(),
    };
    call(
        ada,
        "ada",
        NOTES_APP,
        "move_to",
        vec![
            Value::resource(notebook),
            Value::resource(moved),
            option_of(under),
            option_of(before),
        ],
    );
    ada.commit();
}

fn title_of(ada: &Replica, note: &ResourceRef) -> String {
    match ada.host.runtime().read(note, "title").unwrap() {
        Value::Text(title) => title,
        other => panic!("a title reads {other:?}"),
    }
}

fn titles(ada: &Replica, notebook: &ResourceRef) -> Vec<String> {
    notes(ada, notebook)
        .into_iter()
        .map(|note| title_of(ada, &note))
        .collect()
}

fn parent_of(ada: &Replica, note: &ResourceRef) -> Option<ResourceRef> {
    match ada.host.runtime().read(note, "parent").unwrap() {
        Value::Opt(Some(held)) => Some(target(&held)),
        Value::Opt(None) => None,
        other => panic!("a parent reads {other:?}"),
    }
}

#[test]
fn a_note_dropped_on_a_row_stands_inside_it() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let meetings = note(&mut ada, &notebook, "Meetings");
    let monday = note(&mut ada, &notebook, "Monday");
    move_to(&mut ada, &notebook, &monday, Some(&meetings), None);
    assert_eq!(parent_of(&ada, &monday), Some(meetings.clone()));
    assert_eq!(
        titles(&ada, &notebook),
        ["Q3 plan", "Meetings", "Monday"],
        "it stands at the end of the notes of the one it landed on"
    );

    // One moved out from under its note, after it, stands among the top-level notes again.
    move_to(&mut ada, &notebook, &monday, None, None);
    assert_eq!(parent_of(&ada, &monday), None);
    assert_eq!(titles(&ada, &notebook), ["Q3 plan", "Meetings", "Monday"]);

    // Dropped before one, it takes that one's place among its own.
    move_to(&mut ada, &notebook, &monday, None, Some(&plan));
    assert_eq!(titles(&ada, &notebook), ["Monday", "Q3 plan", "Meetings"]);
}

/// A note cannot be moved inside itself, nor inside anything of its own: the move is refused,
/// so two people moving notes at once never leave one in two places.
#[test]
fn a_note_never_goes_inside_itself() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let meetings = note(&mut ada, &notebook, "Meetings");
    move_to(&mut ada, &notebook, &meetings, Some(&plan), None);
    assert_eq!(parent_of(&ada, &meetings), Some(plan.clone()));
    let id = ada
        .host
        .package(NOTES_APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "move_to")
        .unwrap()
        .id
        .clone();
    let refused = ada.host.runtime().invoke_action(
        &tilolang_runtime::Principal::new("ada"),
        (
            id.as_str(),
            vec![
                Value::resource(&notebook),
                Value::resource(&plan),
                Value::resource(&meetings),
                Value::none(),
            ],
        ),
    );
    let moved = matches!(&refused, Ok(tilolang_runtime::InvokeOutcome::Done(_)));
    assert!(
        !moved,
        "a note moved inside its own branch is refused: {refused:?}"
    );
    assert_eq!(parent_of(&ada, &plan), None, "it stayed where it stood");
    assert_eq!(titles(&ada, &notebook), ["Q3 plan", "Meetings"]);
}

#[test]
fn a_branch_copies_with_every_note_under_it() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let meetings = note(&mut ada, &notebook, "Meetings");
    let monday = note(&mut ada, &notebook, "Monday");
    move_to(&mut ada, &notebook, &meetings, Some(&plan), None);
    move_to(&mut ada, &notebook, &monday, Some(&meetings), None);
    let copy = target(&call(
        &ada,
        "ada",
        NOTES_APP,
        "duplicate",
        vec![Value::resource(&notebook), Value::resource(&plan)],
    ));
    ada.commit();
    assert_eq!(
        titles(&ada, &notebook),
        [
            "Q3 plan", "Meetings", "Monday", "Q3 plan", "Meetings", "Monday"
        ],
        "the copy stands after the branch it copies, with its own in order"
    );
    assert_eq!(parent_of(&ada, &copy), None);
    let copied: Vec<ResourceRef> = notes(&ada, &notebook)
        .into_iter()
        .filter(|note| parent_of(&ada, note) == Some(copy.clone()))
        .collect();
    assert_eq!(copied.len(), 1, "the copy holds a copy of what it held");
    let under: Vec<ResourceRef> = notes(&ada, &notebook)
        .into_iter()
        .filter(|note| parent_of(&ada, note) == Some(copied[0].clone()))
        .collect();
    assert_eq!(under.len(), 1, "and that copy holds its own");
    assert_eq!(title_of(&ada, &under[0]), "Monday");
}

/// A note deleted takes every note under it, as one action of the app's: what it held goes with
/// it, and the notes beside it stay.
#[test]
fn a_branch_goes_with_every_note_under_it() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let meetings = note(&mut ada, &notebook, "Meetings");
    let monday = note(&mut ada, &notebook, "Monday");
    note(&mut ada, &notebook, "Groceries");
    move_to(&mut ada, &notebook, &meetings, Some(&plan), None);
    move_to(&mut ada, &notebook, &monday, Some(&meetings), None);
    call(
        &ada,
        "ada",
        NOTES_APP,
        "delete",
        vec![Value::resource(&notebook), Value::resource(&plan)],
    );
    ada.commit();
    assert_eq!(
        titles(&ada, &notebook),
        ["Groceries"],
        "the note and every note under it went, and nothing beside it"
    );
    let _ = (monday, meetings);
}

#[test]
fn a_notes_own_follow_their_titles_when_a_person_sorts_them() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    for title in ["Wednesday", "Monday", "Tuesday"] {
        let child = note(&mut ada, &notebook, title);
        move_to(&mut ada, &notebook, &child, Some(&plan), None);
    }
    assert_eq!(
        titles(&ada, &notebook),
        ["Q3 plan", "Wednesday", "Monday", "Tuesday"],
        "they stand in the order they were made"
    );
    call(
        &ada,
        "ada",
        NOTES_APP,
        "sort_children",
        vec![Value::resource(&notebook), Value::resource(&plan)],
    );
    ada.commit();
    assert_eq!(
        titles(&ada, &notebook),
        ["Q3 plan", "Monday", "Tuesday", "Wednesday"],
        "the notes of the branch follow their titles, first to last"
    );
    assert_eq!(
        parent_of(&ada, &notes(&ada, &notebook)[1]),
        Some(plan),
        "and they stay under the note they are of"
    );
}

/// The tree's page is the notebook's own: a row pressed opens the note beside the tree and
/// nothing of the window's, and the tree highlights the note its page shows.
#[test]
fn a_pressed_row_shows_the_note_beside_the_tree() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let groceries = note(&mut ada, &notebook, "Groceries");
    let mut window = ada.view(NOTES_APP);
    window.poll_patches().unwrap();
    let tree = find(window.tree(), "ui.TreeView").expect("the notebook's tree");
    let list = tree.key.clone();
    // No row opens anything of the window's own: the page beside the tree is the view's.
    let len = window.list_len(&list).unwrap_or(0);
    assert!(len >= 2, "the tree shows the notebook's notes");
    for index in 0..len {
        let row = window.row(&list, index).unwrap().cloned().expect("a row");
        let node = find(&row, "ui.TreeViewNode").expect("the row is a tree node");
        assert!(
            matches!(
                node.props.get("navigates"),
                Some(Value::Bool(false) | Value::Int(0))
            ),
            "a row opens the note where the tree already shows it: {:?}",
            node.props.get("navigates")
        );
    }
    // The first note is open until a person chooses another, and the tree says so.
    let chosen = |window: &mut ViewInstance| {
        window.poll_patches().unwrap();
        let tree = find(window.tree(), "ui.TreeView").expect("the notebook's tree");
        match tree.props.get("selection") {
            Some(Value::List(items)) => items.iter().map(target).collect::<Vec<_>>(),
            other => panic!("the tree says which note is chosen: {other:?}"),
        }
    };
    assert_eq!(
        chosen(&mut window)
            .iter()
            .map(ResourceRef::id_string)
            .collect::<Vec<_>>(),
        [plan.id_string()],
        "the note the window opened with stands chosen"
    );
    // A press on another row moves the tree's choice with the page beside it.
    window
        .fire_with(&list, "select", Some(&Value::resource(&groceries)))
        .unwrap();
    assert_eq!(
        chosen(&mut window)
            .iter()
            .map(ResourceRef::id_string)
            .collect::<Vec<_>>(),
        [groceries.id_string()],
        "the row the person pressed is the one the page shows"
    );
    // A link or a breadcrumb that shows a note in this window moves the tree's choice to it,
    // rather than pressing a second page over the tree.
    window.show(&groceries).unwrap();
    assert_eq!(
        chosen(&mut window)
            .iter()
            .map(ResourceRef::id_string)
            .collect::<Vec<_>>(),
        [groceries.id_string()],
        "the note the window shows is the one the tree highlights"
    );
}

/// The first node of `kind` in `tree`.
fn find<'a>(tree: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if tree.kind == kind {
        return Some(tree);
    }
    tree.children.iter().find_map(|child| find(child, kind))
}

/// Who else is at a note shows on its tree row, as it does on a list's: the notebook's tree reads
/// the lenses on each note and the row says them.
#[test]
fn a_tree_row_says_who_else_is_at_the_note() {
    let (mut ada, notebook) = workspace();
    let plan = note(&mut ada, &notebook, "Q3 plan");
    let mut window = ada.view(NOTES_APP);
    window.poll_patches().unwrap();
    // Who the tree's row for the note says is at it, as the tree builds its rows.
    let presence_of = |window: &mut ViewInstance, at: &ResourceRef| {
        window.poll_patches().unwrap();
        let tree = find(window.tree(), "ui.TreeView").expect("the notebook's tree");
        let key = tree.key.clone();
        let len = window.list_len(&key).unwrap_or(0);
        for index in 0..len {
            let Some(row) = window.row(&key, index).unwrap().cloned() else {
                continue;
            };
            if let Some(node) = find(&row, "ui.TreeViewNode")
                && matches!(node.props.get("item"), Some(Value::Ref(r)) if r.target.id_string() == at.id_string())
            {
                return node.props.get("presence").cloned();
            }
        }
        None
    };
    assert_eq!(
        presence_of(&mut window, &plan),
        None,
        "nobody else is at it"
    );
    // Ben has the note open: the tree says so.
    ada.host
        .runtime()
        .set_lenses(&plan.id_string(), &[(Principal::new("ben"), false, false)]);
    let shown = presence_of(&mut window, &plan).expect("the row says who is at the note");
    match shown {
        Value::Principal(who) => assert_eq!(who.0, "ben"),
        other => panic!("the row says who, as a person: {other:?}"),
    }
}
