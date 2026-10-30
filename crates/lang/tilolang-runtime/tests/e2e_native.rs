//! The native vocabulary run: `std.list`'s items and rows, its sort as view state and its
//! places as the item type's filters with a count each, a row view, and the choice and facts a
//! document shows.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.notes "Notes" (icon: .bookmark)

type Note {
    title: Text (title)
    pinned: Bool = false
}

type Notebook {
    notes: Collection<Note>
}

enum NoteFilter {
    /// All notes
    all
    pinned
}

enum NoteOrder {
    created
    title
}

root Notebook

default view notebook for Notebook

row view note_row for Note

primary action add(nb: Notebook, title: Text, pinned: Bool) -> Note (icon: .plus) {
    return nb.notes.insert(Note(title: title, pinned: pinned))
}

view note_row(n: Note) {
    row(n.title, mark: if n.pinned { .pin })
}

view notebook(nb: Notebook) {
    window order: NoteOrder = .created
    std.list(nb, nb.notes, sort: order, empty: "No notes yet") { n ->
        row(n.title, checkbox: n.pinned)
    }
}

view rows(nb: Notebook) {
    list(nb.notes)
}

view detail(n: Note) {
    lens shown: NoteFilter = .pinned
    std.document(n) {
        choice(bind: shown)
        facts {
            "Title": n.title
            "Pinned": n.pinned
        }
    }
}
"#;

    fn setup() -> (Runtime, App, ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let nb = rt.create("Notebook").unwrap();
        let add = app.action("add");
        for (title, pinned) in [("Q3 plan", true), ("Groceries", false), ("Ideas", false)] {
            rt.invoke_action(
                &me(),
                (
                    add.as_str(),
                    vec![
                        Value::resource(&nb),
                        Value::text(title),
                        Value::Bool(pinned),
                    ],
                ),
            )
            .unwrap();
        }
        (rt, app, nb)
    }

    fn list(tree: &UiNode) -> &UiNode {
        of_kind(tree, "list")
            .into_iter()
            .chain(of_kind(tree, "ui.VirtualList"))
            .next()
            .unwrap_or_else(|| panic!("no list in\n{}", dump(tree)))
    }

    fn titles(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .into_iter()
            .map(|r| text_prop(r, "title"))
            .collect()
    }

    /// The node of `kind` whose `prop` is `value`.
    fn node_with<'a>(tree: &'a UiNode, kind: &str, prop: &str, value: &str) -> &'a UiNode {
        of_kind(tree, kind)
            .into_iter()
            .find(|node| node.props.get(prop) == Some(&Value::text(value)))
            .unwrap_or_else(|| panic!("no {kind} with {prop} {value} in\n{}", dump(tree)))
    }

    #[test]
    fn a_list_holds_one_item_per_resource_keyed_by_it_with_its_row() {
        let (rt, app, nb) = setup();
        let mut view = rt
            .open_view(&me(), &app.view("notebook"), vec![Value::resource(&nb)])
            .unwrap();
        let tree = view.tree();
        assert_eq!(
            tree.kind,
            "ui.NavShell",
            "the window is a shell: {}",
            dump(tree)
        );
        let l = list(tree);
        let key = l.key.clone();
        assert_eq!(view.list_len(&key), Some(3), "{}", dump(tree));
        assert!(
            of_kind(tree, "ui.ListItem").is_empty(),
            "no row renders before the host shows it: {}",
            dump(tree)
        );
        let mut items = Vec::new();
        for i in 0..3 {
            let row_key = view.row_key(&key, i).expect("a row");
            let row = view.row(&key, i).unwrap().expect("shown").clone();
            assert_eq!(row.key, row_key);
            let [item] = row.children.as_slice() else {
                panic!("a shown row holds its item: {}", dump(&row));
            };
            assert_eq!(item.kind, "ui.ListItem");
            let Value::Ref(r) = &item.props["item"] else {
                panic!("an item carries its resource: {}", dump(item));
            };
            assert!(
                row_key.ends_with(&format!("/{}", r.target.id_string())),
                "a row is keyed by its resource: {row_key}"
            );
            assert_eq!(of_kind(item, "ui.ListRow").len(), 1, "{}", dump(item));
            items.push(item.clone());
        }
        assert_eq!(titles(view.tree()), ["Q3 plan", "Groceries", "Ideas"]);
        let row = of_kind(&items[0], "ui.ListRow")[0];
        assert_eq!(view.bound_value(&row.key), Some(Value::Bool(true)));

        // A notebook with nothing in it says so. Its primary action takes a title the empty
        // state has none of: it offers it as its form, which asks for the title.
        let empty = rt.create("Notebook").unwrap();
        let view = rt
            .open_view(&me(), &app.view("notebook"), vec![Value::resource(&empty)])
            .unwrap();
        let state = of_kind(view.tree(), "ui.EmptyState")[0];
        assert_eq!(text_prop(state, "title"), "No notes yet");
        assert_eq!(text_prop(state, "action"), "Add…", "{}", dump(state));
    }

    #[test]
    fn the_sort_is_view_state_and_the_places_the_item_types_filters_with_counts() {
        let (rt, app, nb) = setup();
        let mut view = rt
            .open_view(&me(), &app.view("notebook"), vec![Value::resource(&nb)])
            .unwrap();
        let counts = |tree: &UiNode| -> Vec<Value> {
            ["all", "is:pinned"]
                .iter()
                .map(|place| node_with(tree, "ui.NavItem", "key", place).props["count"].clone())
                .collect()
        };
        let tree = view.tree();
        assert_eq!(text_prop(of_kind(tree, "ui.NavList")[0], "selected"), "all");
        assert_eq!(counts(tree), [Value::Int(3), Value::Int(1)]);
        let checked = |tree: &UiNode, label: &str| {
            node_with(tree, "ui.MenuItem", "label", label)
                .props
                .get("checked")
                .cloned()
        };
        assert_eq!(checked(tree, "Created"), Some(Value::Int(1)));
        assert_ne!(checked(tree, "Title"), Some(Value::Int(1)));
        // Rows are read here as a host that draws every row does.
        view.set_lazy_rows(false).unwrap();
        let pinned = node_with(view.tree(), "ui.NavItem", "key", "is:pinned")
            .key
            .clone();
        view.fire(&pinned, "select").unwrap();
        view.poll_patches().unwrap();
        let tree = view.tree();
        assert_eq!(
            text_prop(of_kind(tree, "ui.NavList")[0], "selected"),
            "is:pinned"
        );
        assert_eq!(titles(tree), ["Q3 plan"]);
        assert_eq!(
            counts(tree),
            [Value::Int(3), Value::Int(1)],
            "the counts do not depend on the filter shown"
        );
        let title = node_with(tree, "ui.MenuItem", "label", "Title").key.clone();
        view.fire(&title, "choose").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(checked(view.tree(), "Title"), Some(Value::Int(1)));
    }

    #[test]
    fn a_list_without_an_item_block_shows_the_types_row_view() {
        let (rt, app, nb) = setup();
        let mut view = rt
            .open_view(&me(), &app.view("rows"), vec![Value::resource(&nb)])
            .unwrap();
        view.set_lazy_rows(false).unwrap();
        let l = list(view.tree());
        assert_eq!(l.children.len(), 3);
        let rows = of_kind(&l.children[0], "ui.ListRow");
        assert_eq!(rows.len(), 1, "{}", dump(l));
        assert_eq!(text_prop(rows[0], "title"), "Q3 plan");
        assert_eq!(text_prop(rows[0], "mark"), "pin");
        assert!(
            !of_kind(&l.children[1], "ui.ListRow")[0]
                .props
                .contains_key("mark")
        );
    }

    #[test]
    fn a_choice_picks_view_state_and_facts_show_their_values() {
        let (rt, app, nb) = setup();
        let Value::List(notes) = rt.read(&nb, "notes").unwrap() else {
            panic!("notes");
        };
        let Value::Ref(note) = &notes[0] else {
            panic!("a note");
        };
        let mut view = rt
            .open_view(
                &me(),
                &app.view("detail"),
                vec![Value::resource(&note.target)],
            )
            .unwrap();
        let choice = of_kind(view.tree(), "choice")[0].clone();
        assert_eq!(text_prop(&choice, "selected"), "pinned");
        assert_eq!(text_prop(&choice, "enum"), app.meta().enums[0].id.clone());
        view.fire(&choice.key, "pick.all").unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "choice")[0], "selected"),
            "all"
        );
        let facts = of_kind(view.tree(), "ui.Property");
        // The view's own facts, and nothing of the document's history.
        let pairs: Vec<(String, Value)> = facts
            .iter()
            .map(|f| (text_prop(f, "label"), f.props["value"].clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("Title".to_string(), Value::text("Q3 plan")),
                ("Pinned".to_string(), Value::Int(1)),
            ]
        );
    }
}
