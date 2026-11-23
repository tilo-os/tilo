//! A notebook as a tree, through the real packages and the document: a note moves under the one
//! it lands on and among its own, a branch copies whole, a branch is deleted as one change a
//! person takes back at once, and a note's own follow their titles when the person asks it.

use crate::fixtures::Workspace;
use tilolang_runtime::{Actor, ResourceRef, Value, ViewInstance};

use crate::fixtures::{Replica, target};
use crate::legibility::call;

const NOTES_APP: &str = "so.tilo.notes";
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
    let (ada, _) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    ada.commit();
    let notebook = ada.host.root_of(NOTES_APP).expect("the notebook");
    (ada, notebook)
}

/// The notes the notebook holds, in the order the document keeps them.
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
fn note(ada: &Replica, notebook: &ResourceRef, title: &str) -> ResourceRef {
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
    ada: &Replica,
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

tilo_test_env::scenario! {
    fn the_tree_of_notes_moves_and_copies_its_branches;

    phase a_note_dropped_on_a_row_stands_inside_it;
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let meetings = note(&ada, &notebook, "Meetings");
        let monday = note(&ada, &notebook, "Monday");
        move_to(&ada, &notebook, &monday, Some(&meetings), None);
        assert_eq!(parent_of(&ada, &monday).as_ref(), Some(&meetings));
        assert_eq!(
            titles(&ada, &notebook),
            ["Q3 plan", "Meetings", "Monday"],
            "it stands at the end of the notes of the one it landed on"
        );

        // One moved out from under its note, after it, stands among the top-level notes again.
        move_to(&ada, &notebook, &monday, None, None);
        assert_eq!(parent_of(&ada, &monday), None);
        assert_eq!(titles(&ada, &notebook), ["Q3 plan", "Meetings", "Monday"]);

        // Dropped before one, it takes that one's place among its own.
        move_to(&ada, &notebook, &monday, None, Some(&plan));
        assert_eq!(titles(&ada, &notebook), ["Monday", "Q3 plan", "Meetings"]);
    }

    phase a_note_never_goes_inside_itself;
    // A note cannot be moved inside itself, nor inside anything of its own: the move is refused,
    // so two people moving notes at once never leave one in two places.
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let meetings = note(&ada, &notebook, "Meetings");
        move_to(&ada, &notebook, &meetings, Some(&plan), None);
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
            &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
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

    phase a_branch_copies_with_every_note_under_it;
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let meetings = note(&ada, &notebook, "Meetings");
        let monday = note(&ada, &notebook, "Monday");
        move_to(&ada, &notebook, &meetings, Some(&plan), None);
        move_to(&ada, &notebook, &monday, Some(&meetings), None);
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

    phase a_branch_goes_with_every_note_under_it;
    // A note deleted takes every note under it, as one action of the app's: what it held goes with
    // it, and the notes beside it stay.
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let meetings = note(&ada, &notebook, "Meetings");
        let monday = note(&ada, &notebook, "Monday");
        note(&ada, &notebook, "Groceries");
        move_to(&ada, &notebook, &meetings, Some(&plan), None);
        move_to(&ada, &notebook, &monday, Some(&meetings), None);
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
}

#[test]
fn a_notes_own_follow_their_titles_when_a_person_sorts_them() {
    let (ada, notebook) = workspace();
    let plan = note(&ada, &notebook, "Q3 plan");
    for title in ["Wednesday", "Monday", "Tuesday"] {
        let child = note(&ada, &notebook, title);
        move_to(&ada, &notebook, &child, Some(&plan), None);
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

tilo_test_env::scenario! {
    fn a_tree_row_shows_its_note_and_who_is_at_it;

    phase a_pressed_row_shows_the_note_beside_the_tree;
    // The tree's page is the notebook's own: a row pressed opens the note beside the tree and
    // nothing of the window's, and the tree highlights the note its page shows.
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let groceries = note(&ada, &notebook, "Groceries");
        let mut window = ada.view(NOTES_APP);
        window.poll_patches().unwrap();
        let tree = window.tree().first_of_kind("ui.TreeView").expect("the notebook's tree");
        let list = tree.key.clone();
        // No row opens anything of the window's own: the page beside the tree is the view's.
        let len = window.list_len(&list).unwrap_or(0);
        assert!(len >= 2, "the tree shows the notebook's notes");
        for index in 0..len {
            let row = window.row(&list, index).unwrap().cloned().expect("a row");
            let node = row.first_of_kind("ui.TreeViewNode").expect("the row is a tree node");
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
            let tree = window.tree().first_of_kind("ui.TreeView").expect("the notebook's tree");
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

    phase a_tree_row_says_who_else_is_at_the_note;
    // Who else is at a note shows on its tree row, as it does on a list's: the notebook's tree reads
    // the lenses on each note and the row says them.
    {
        let (ada, notebook) = workspace();
        let plan = note(&ada, &notebook, "Q3 plan");
        let mut window = ada.view(NOTES_APP);
        window.poll_patches().unwrap();
        // Who the tree's row for the note says is at it, as the tree builds its rows.
        let presence_of = |window: &mut ViewInstance, at: &ResourceRef| {
            window.poll_patches().unwrap();
            let tree = window.tree().first_of_kind("ui.TreeView").expect("the notebook's tree");
            let key = tree.key.clone();
            let len = window.list_len(&key).unwrap_or(0);
            for index in 0..len {
                let Some(row) = window.row(&key, index).unwrap().cloned() else {
                    continue;
                };
                if let Some(node) = row.first_of_kind("ui.TreeViewNode")
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
            .set_lenses(&plan.id_string(), &[(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")), false, false)]);
        let shown = presence_of(&mut window, &plan).expect("the row says who is at the note");
        match shown {
            Value::Actor(who) => assert_eq!(who.id(), "ben"),
            other => panic!("the row says who, as a person: {other:?}"),
        }
    }
}

/// Who reacted with each emoji, as the note's page shows it in its header's strip: the emoji and
/// who reacted, sorted so the order two replicas group them in does not matter.
fn reacted_shown(view: &mut ViewInstance) -> Vec<(String, Vec<String>)> {
    view.poll_patches().unwrap();
    let strip = view
        .tree()
        .first_of_kind("ui.Reactions")
        .expect("the note's page shows its reactions");
    assert!(
        strip.events.contains_key("toggle"),
        "a pill is a press: {strip:?}"
    );
    let Some(Value::Map(entries)) = strip.props.get("reactions") else {
        panic!("reactions as a map: {:?}", strip.props);
    };
    let mut shown: Vec<(String, Vec<String>)> = entries
        .iter()
        .map(|(emoji, who)| {
            let Value::List(who) = who else {
                panic!("who reacted, as a set: {who:?}");
            };
            let who = who
                .iter()
                .map(|p| match p {
                    Value::Actor(p) => p.id().to_owned(),
                    other => panic!("an actor: {other:?}"),
                })
                .collect();
            (emoji.as_text().unwrap_or_default().to_owned(), who)
        })
        .collect();
    for (_, who) in &mut shown {
        who.sort();
    }
    shown.sort();
    shown
}

/// Who reacted with each emoji, as the note's `reactions` field holds it.
fn reacted_field(ada: &Replica, note: &ResourceRef) -> Vec<(String, Vec<String>)> {
    let Value::List(entries) = ada.host.runtime().read(note, "reactions").unwrap() else {
        panic!("the reactions are a map");
    };
    let mut held: Vec<(String, Vec<String>)> = entries
        .into_iter()
        .map(|entry| {
            let Value::Tuple(pair) = entry else {
                panic!("a reaction is its emoji and who: {entry:?}");
            };
            let [emoji, who] = pair.as_slice() else {
                panic!("a reaction is its emoji and who: {pair:?}");
            };
            let (Value::Text(emoji), Value::List(who)) = (emoji, who) else {
                panic!("a reaction is its emoji and who: {pair:?}");
            };
            let mut who: Vec<String> = who
                .iter()
                .map(|p| match p {
                    Value::Actor(p) => p.id().to_owned(),
                    other => panic!("an actor: {other:?}"),
                })
                .collect();
            who.sort();
            (emoji.clone(), who)
        })
        .collect();
    held.sort();
    held
}

#[test]
fn two_people_react_to_a_note_and_its_page_shows_who_reacted() {
    let (ada, notebook) = workspace();
    let plan = note(&ada, &notebook, "Q3 plan");
    let ben = ada.fork("ben", &[DISCUSSION, NOTES]);
    let mut ada_page = ada.host.open_resource(&plan, None).unwrap();
    let mut ben_page = ben.host.open_resource(&plan, None).unwrap();
    assert!(
        reacted_shown(&mut ada_page).is_empty(),
        "no reactions yet, and the strip's add control is offered"
    );

    // Both react with the same emoji before either hears of the other's, and Ben with another.
    call(
        &ada,
        "ada",
        NOTES_APP,
        "react",
        vec![Value::resource(&plan), Value::text("👍")],
    );
    ada.commit();
    call(
        &ben,
        "ben",
        NOTES_APP,
        "react",
        vec![Value::resource(&plan), Value::text("👍")],
    );
    call(
        &ben,
        "ben",
        NOTES_APP,
        "react",
        vec![Value::resource(&plan), Value::text("🎉")],
    );
    ben.commit();
    ada.sync_with(&ben);
    let both = vec![
        ("🎉".to_owned(), vec!["ben".to_owned()]),
        ("👍".to_owned(), vec!["ada".to_owned(), "ben".to_owned()]),
    ];
    for view in [&mut ada_page, &mut ben_page] {
        assert_eq!(reacted_shown(view), both, "neither reaction is lost");
    }
    assert_eq!(reacted_field(&ada, &plan), both);

    // Ada's second press takes hers back; Ben's stays, and both replicas show it so.
    call(
        &ada,
        "ada",
        NOTES_APP,
        "react",
        vec![Value::resource(&plan), Value::text("👍")],
    );
    ada.commit();
    ada.sync_with(&ben);
    let bens = vec![
        ("🎉".to_owned(), vec!["ben".to_owned()]),
        ("👍".to_owned(), vec!["ben".to_owned()]),
    ];
    for view in [&mut ada_page, &mut ben_page] {
        assert_eq!(reacted_shown(view), bens, "a second press takes yours back");
    }
    assert_eq!(reacted_field(&ada, &plan), bens);
}

/// A note's body edit is history's like any other field's: the entry says what happened to the
/// body, under it stands the words the edit added, and the bound edit's action id never shows.
#[test]
fn a_note_body_edit_says_what_changed_in_the_body() {
    let (ada, notebook) = workspace();
    let note = note(&ada, &notebook, "Trip to Lisbon");
    let meta = ada.host.package(NOTES_APP).expect("the notes package").meta;
    let body = crate::fixtures::field_id(&meta, "Note", "body");
    ada.edit(
        &note,
        &body,
        &Value::Rich(tilolang_runtime::rich::RichValue::from_text(
            "Pack the map.",
        )),
    );
    ada.commit();
    let versions = ada.versions(&note);
    let last = versions
        .last()
        .expect("the body edit is a version of the note");
    assert_eq!(last.action, "edit-text");
    assert_eq!(
        last.fields,
        vec![body],
        "the version names the field the edit wrote: {last:#?}"
    );

    let viewer = ada.host.viewer().clone();
    let mut history = ada
        .host
        .runtime()
        .open_written_view(
            &viewer,
            tilolang_tilo::instance::WINDOW_HISTORY,
            &viewer,
            vec![Value::resource(&note)],
        )
        .unwrap();
    history.poll_patches().unwrap();
    // Ada's changes a moment apart are one step: it opens to each of them.
    let show = crate::fixtures::nodes(history.tree())
        .into_iter()
        .filter(|node| node.kind == "ui.Button")
        .find(|node| {
            matches!(
                node.props.get("label"),
                Some(Value::Text(label))
                    if label == "Show changes" || label == "tilolang-history-show"
            )
        })
        .map(|node| node.key.clone());
    if let Some(key) = show {
        history.press(&key).unwrap();
        history.poll_patches().unwrap();
    }
    let words = crate::tasks::texts(history.tree());
    let said = words.join(" ");
    assert!(
        !said.contains("edit-text") && !said.contains("Edit-text"),
        "a history entry never reads the bound edit's action id: {words:?}"
    );
    assert!(
        words
            .iter()
            .any(|word| word == "changed body" || word == "tilolang-did-changed"),
        "the entry says what happened to the body: {words:?}"
    );
    assert!(
        said.contains("Pack the map."),
        "the words the edit wrote stand under it: {words:?}"
    );
}
