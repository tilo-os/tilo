//! Marks in place (task-legibility.md §3): with no code of the app's, the host marks the rows,
//! the bound fields and the window of what an agent's run changed and the person has not seen;
//! each mark shows the run that made it; a view may place the mark itself; and every mark goes
//! once the run is seen.

use automerge::AutoCommit;
use tilolang_runtime::{UiNode, Value, ViewEvent, ViewInstance};

use crate::fixtures::Replica;
use crate::legibility::{AGENT, NOTES_APP, RUN, Tidied, a_run_in_notes_and_tasks, call};

fn find_all<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        find_all(child, kind, out);
    }
}

fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    find_all(node, kind, &mut out);
    out
}

/// Every row of the view's list, as the list builds them.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("a list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The resource each row presents, and the change mark the host placed on it, if any.
fn row_marks(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    let mut out = Vec::new();
    for row in rows(view) {
        for item in all(&row, "ui.ListItem") {
            let of = match item.props.get("item") {
                Some(Value::Ref(r)) => r.target.id_string(),
                other => panic!("a row presents its item: {other:?}"),
            };
            let mark = item
                .children
                .iter()
                .find(|c| c.kind == "slot" && c.props.get("name") == Some(&Value::text("change")))
                .and_then(|slot| slot.children.first().cloned());
            out.push((of, mark));
        }
    }
    out
}

fn principal(node: &UiNode, prop: &str) -> Option<String> {
    match node.props.get(prop) {
        Some(Value::Principal(who)) => Some(who.0.clone()),
        _ => None,
    }
}

fn marked<'m>(marks: &'m [(String, Option<UiNode>)], id: &str) -> Option<&'m UiNode> {
    marks
        .iter()
        .find(|(of, _)| of == id)
        .and_then(|(_, mark)| mark.as_ref())
}

#[test]
fn the_host_marks_rows_and_the_window_and_a_mark_shows_its_run() {
    let Tidied {
        ada,
        made,
        groceries,
        ..
    } = a_run_in_notes_and_tasks();

    // The notebook: the rows the run made or changed carry its mark, with who and which run;
    // no app code asked for it.
    let mut notebook = ada.view(NOTES_APP);
    let marks = row_marks(&mut notebook);
    for (at, kind) in [(&made, "created"), (&groceries, "changed")] {
        let mark = marked(&marks, &at.id_string())
            .unwrap_or_else(|| panic!("{at:?} is marked: {marks:?}"));
        assert_eq!(mark.kind, "ui.ChangeMark");
        assert_eq!(principal(mark, "by").as_deref(), Some(AGENT));
        assert_eq!(
            mark.props.get("run"),
            Some(&Value::text("Tidy my notes and tasks"))
        );
        assert_eq!(mark.props.get("kind"), Some(&Value::text(kind)));
    }
    // The window's header counts what changed in it, and by whom.
    let window = all(notebook.tree(), "ui.ChangeMark")
        .into_iter()
        .find(|m| m.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked");
    assert_eq!(principal(&window, "by").as_deref(), Some(AGENT));
    assert!(
        matches!(window.props.get("count"), Some(Value::Int(n)) if *n >= 2),
        "{window:?}"
    );

    // Activating a mark shows the run that made it.
    let key = marked(&marks, &groceries.id_string()).unwrap().key.clone();
    let found = notebook.find(&key).cloned();
    assert!(found.is_some(), "the mark is found by its key");
    assert!(
        found.unwrap().events.contains_key("activate"),
        "it activates"
    );
    notebook.fire(&key, "activate").unwrap();
    assert!(
        notebook.poll_events().iter().any(|event| matches!(
            event,
            ViewEvent::Review { run, target } if run == RUN && *target == groceries
        )),
        "the mark shows its run"
    );

    // Mark all seen on the run: every mark goes, live.
    assert!(ada.host.runtime().mark_run_seen("ada", RUN));
    let marks = row_marks(&mut notebook);
    assert!(marks.iter().all(|(_, mark)| mark.is_none()), "{marks:?}");
    assert!(
        all(notebook.tree(), "ui.ChangeMark").is_empty(),
        "the window's mark is gone"
    );
}

#[test]
fn a_field_the_run_changed_takes_its_colour_and_says_what_it_held() {
    let Tidied { ada, groceries, .. } = a_run_in_notes_and_tasks();
    let mut editor = ada.host.open_resource(&groceries, None).unwrap();
    let changed: Vec<UiNode> = all(editor.tree(), "ui.TextInput")
        .into_iter()
        .filter(|input| principal(input, "changed_by").is_some())
        .cloned()
        .collect();
    // The title it renamed, not the body it left alone.
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(principal(&changed[0], "changed_by").as_deref(), Some(AGENT));
    assert_eq!(
        changed[0].props.get("previous"),
        Some(&Value::text("Groceries"))
    );
    // The window of the note counts its one change.
    assert!(
        all(editor.tree(), "ui.ChangeMark")
            .iter()
            .any(|m| m.props.get("count") == Some(&Value::Int(1))),
        "the note's window is marked"
    );
    // Once seen, the field is the field again.
    assert!(ada.host.runtime().mark_seen("ada", &groceries).unwrap());
    editor.poll_patches().unwrap();
    assert!(
        all(editor.tree(), "ui.TextInput")
            .iter()
            .all(|input| principal(input, "changed_by").is_none())
    );
}

const PLACED: &str = r#"app so.tilo.lang.placed "Placed"

type Book {
    title: Text (title)
}

type Shelf {
    books: Collection<Book>
}

root Shelf

default view shelf for Shelf

action shelve(s: Shelf, title: Text) -> Book {
    s.books.insert(Book(title: title))
}

/// Each row places the mark itself, before the title.
view shelf(s: Shelf) {
    ui.VirtualList {
        for b in s.books {
            ui.ListItem(item: b) {
                row {
                    ui.ChangeMark(b)
                    text(b.title)
                }
            }
        }
    }
}
"#;

#[test]
fn a_view_that_places_the_mark_itself_gets_it_filled_and_the_host_places_none() {
    let app = "so.tilo.lang.placed";
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(PLACED, "")]);
    let shelf = Value::resource(&ada.host.root_of(app).unwrap());
    ada.commit();
    call(&ada, AGENT, app, "shelve", vec![shelf, Value::text("Dune")]);
    ada.commit_in_run(AGENT, RUN, "Stock the shelf");
    let mut view = ada.view(app);
    let rows = rows(&mut view);
    let placed: Vec<&UiNode> = rows
        .iter()
        .flat_map(|row| all(row, "ui.ChangeMark"))
        .collect();
    assert_eq!(placed.len(), 1, "one mark, the view's: {rows:?}");
    assert_eq!(principal(placed[0], "by").as_deref(), Some(AGENT));
    assert_eq!(
        placed[0].props.get("run"),
        Some(&Value::text("Stock the shelf"))
    );
}
