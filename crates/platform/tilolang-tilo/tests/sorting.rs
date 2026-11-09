//! A window's sort: the fields its app names, chosen from the bar's menu, and the list the store
//! answers in the order chosen.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const NOTES: &str = "so.tilo.lang.notes";

/// The notes app and the package it uses, as they ship.
fn packages() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            include_str!("../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../apps/tl/discussion/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../apps/tl/notes/app.ids"),
        ),
    ]
}

fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    walk(node, kind, &mut out);
    out
}

fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        walk(child, kind, out);
    }
}

/// The notebook's rows as it shows them: what each row says its note is called.
fn titles(view: &mut ViewInstance) -> Vec<String> {
    view.set_lazy_rows(false).unwrap();
    view.poll_patches().unwrap();
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("the notebook's list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .map(|row| {
            let lines = all(&row, "ui.ListRow");
            let line = lines
                .first()
                .unwrap_or_else(|| panic!("a note's row is a line: {row:?}"));
            match line.props.get("title") {
                Some(Value::Text(title)) => title.clone(),
                other => panic!("a row's title: {other:?}"),
            }
        })
        .collect()
}

/// The window's sort menu: the bar's one control whose entries are orders.
fn sort_menu(view: &ViewInstance) -> Vec<UiNode> {
    all(view.tree(), "ui.MenuButton")
        .into_iter()
        .filter(|button| {
            matches!(
                button.props.get("label"),
                Some(Value::Text(label)) if label == "Sort" || label == "tilolang-sort"
            )
        })
        .flat_map(|button| all(button, "ui.MenuItem"))
        .map(UiNode::clone)
        .collect()
}

/// The entries of the window's sort menu, and whether each is the one chosen.
fn orders(view: &ViewInstance) -> Vec<(String, bool)> {
    sort_menu(view)
        .iter()
        .map(|item| {
            let label = match item.props.get("label") {
                Some(Value::Text(label)) => label.clone(),
                other => panic!("a menu entry's label: {other:?}"),
            };
            (label, item.props.get("checked") == Some(&Value::Int(1)))
        })
        .collect()
}

/// Choosing the order by title in the notebook's bar lists the notes by title: the menu names
/// the fields the app declared as orders, and the order the store answers in is the one chosen.
#[test]
fn the_notebook_lists_its_notes_by_the_order_chosen() {
    let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages());
    replica.host.root_of(NOTES).unwrap();
    replica.commit();
    let scenario = loaded
        .last()
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a week of notes")
        .expect("Notes stages a week of notes");
    replica
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap();
    replica.commit();

    let mut notebook = replica.view(NOTES);
    notebook.set_lazy_rows(false).unwrap();
    notebook.poll_patches().unwrap();
    // The list's own order first, called by the host's word for it, then the one field the app
    // names: `sorts: [Note.title]` in its window.
    let menu = orders(&notebook);
    assert_eq!(
        menu.iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>()[1..],
        ["Title"],
        "the app declares the fields people may sort by; the list's own order is the first entry"
    );
    assert!(menu[0].1, "the list's own order is the one chosen");
    assert!(!menu[1].1, "no field is chosen to begin with");

    let as_kept = titles(&mut notebook);
    assert_eq!(
        &as_kept[..3],
        ["Q3 plan", "Standup, Monday", "Groceries"],
        "the list as the store keeps it: the notes in the order they were written"
    );

    let title = sort_menu(&notebook)
        .into_iter()
        .find(|item| item.props.get("label") == Some(&Value::text("Title")))
        .expect("the menu offers the order by title")
        .key;
    notebook.fire(&title, "choose").unwrap();
    notebook.poll_patches().unwrap();

    let by_title = titles(&mut notebook);
    let mut expected = as_kept.clone();
    expected.sort();
    assert_eq!(by_title, expected, "the notes are listed by title");
    assert_eq!(by_title[0], "Book list", "A to Z, from the title on");
    assert_eq!(
        orders(&notebook),
        vec![
            (orders(&notebook)[0].0.clone(), false),
            ("Title".to_owned(), true),
        ],
        "the menu says which order the list is in"
    );
    // Choosing the list's own order again puts the notes back as the store keeps them.
    let kept = sort_menu(&notebook)
        .first()
        .expect("the menu's first entry")
        .key
        .clone();
    notebook.fire(&kept, "choose").unwrap();
    notebook.poll_patches().unwrap();
    assert_eq!(titles(&mut notebook), as_kept);
}
